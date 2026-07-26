// core/src/nds/apu.rs
//! NDS APU: the 16 sound channels at 0x040004xx (ARM7 side), mixed to stereo
//! and resampled to 44.1 kHz through the same `BoxResampler` output stage the
//! GBA/GBC paths use (DC-block + low-pass + box integration).
//!
//! Implemented formats: PCM8, PCM16, IMA-ADPCM (SoulSilver's BGM/SFX streams),
//! and PSG (format 3): rectangular waves with the 8-step duty sequencer on
//! channels 8-13 and the 15-bit LFSR noise on channels 14-15, both clocked by
//! the same SOUNDxTMR period as PCM (GBATEK "DS Sound").
//!
//! Memory-touching logic (sample fetches, key-on header reads, per-tick
//! mixing) lives in `NdsMmu::tick_apu` — channels read source data through the
//! ARM7 bus view (main RAM / shared WRAM). This module owns the pure state:
//! register file, ADPCM decode, and the stereo mix of already-decoded samples.

use crate::resampler::BoxResampler;

/// Bus-cycle rate the NDS run loop is budgeted in: the real hardware clock.
///
/// One emulated frame is the 560190 cycles `emulator.rs` budgets (355 dots x
/// 263 lines x 6), and the DS clock is 33.513982 MHz — which is exactly why the
/// console refreshes at 59.8261 Hz and not 60.
///
/// This divides the resampler's output rate into `cycles_per_sample`, and the
/// frontend paces emulation on how fast the audio queue drains, so the constant
/// sets the emulated frame rate directly: the rounded `560190 * 60` it replaces
/// ran the whole machine 0.29% fast — pitch, tempo and video alike.
pub const NDS_CYCLES_PER_SEC: f64 = 33_513_982.0;

/// Standard IMA-ADPCM step table (GBATEK "DS Sound" / IMA spec, 89 entries).
const ADPCM_STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41,
    45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190,
    209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724,
    796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132,
    7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500,
    20350, 22385, 24623, 27086, 29794, 32767,
];

/// Step-index delta per nibble magnitude (low 3 bits of the ADPCM nibble).
const ADPCM_INDEX_DELTA: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// One hardware sound channel: raw registers as last written plus the runtime
/// playback cursor/decoder state. `Copy` so `NdsMmu` can advance a scratch
/// copy while reading sample bytes through `&self` bus methods.
#[derive(Clone, Copy, Default)]
pub struct NdsChannel {
    /// SOUNDxCNT as written. Bit 31 here is the last *written* start bit;
    /// the live busy state readback comes from `active`.
    pub cnt: u32,
    /// SOUNDxSAD source address (27-bit, main RAM / WRAM).
    pub sad: u32,
    /// SOUNDxTMR timer reload: sample period = 2*(0x10000-TMR) bus cycles.
    pub tmr: u16,
    /// SOUNDxPNT loop start, in 32-bit words from SAD (ADPCM: includes header).
    pub pnt: u16,
    /// SOUNDxLEN length past the loop point, in 32-bit words (22-bit).
    pub len: u32,

    /// True while the channel plays; cleared on one-shot end or start-bit clear.
    pub active: bool,
    /// Byte offset from SAD of the next source unit to consume.
    pub cursor: u32,
    /// Bus cycles accumulated toward the next source sample.
    pub timer_acc: u32,
    /// Current decoded output sample (held between source samples).
    pub sample: i16,
    /// True when the next ADPCM nibble is the high one of the byte at cursor.
    pub adpcm_high: bool,
    pub adpcm_val: i32,
    pub adpcm_idx: i32,
    /// Decoder snapshot taken when playback first reaches the loop point;
    /// hardware resumes from here on loop wrap (it does NOT re-read the header).
    pub adpcm_loop_val: i32,
    pub adpcm_loop_idx: i32,

    /// PSG rectangular wave: position 0-7 within the 8-step duty sequence.
    pub psg_phase: u8,
    /// PSG noise: 15-bit LFSR, seeded 0x7FFF at key-on.
    pub noise_lfsr: u16,
}

impl NdsChannel {
    #[inline]
    pub fn format(&self) -> u32 {
        (self.cnt >> 29) & 3
    }

    #[inline]
    pub fn repeat_mode(&self) -> u32 {
        (self.cnt >> 27) & 3
    }

    /// Total playable bytes: (PNT + LEN) words.
    #[inline]
    pub fn total_bytes(&self) -> u32 {
        (self.pnt as u32 + self.len) * 4
    }

    /// Loop-restart byte offset. For ADPCM the first word is the header, so a
    /// (malformed) PNT=0 is clamped past it instead of re-decoding the header
    /// as sample data.
    #[inline]
    pub fn loop_start_bytes(&self) -> u32 {
        let p = self.pnt as u32 * 4;
        if self.format() == 2 { p.max(4) } else { p }
    }

    /// Sample period in bus cycles (>= 2; TMR=0xFFFF is the fastest rate).
    #[inline]
    pub fn period_cycles(&self) -> u32 {
        2 * (0x1_0000 - self.tmr as u32)
    }

    /// Decode one IMA-ADPCM nibble into `sample` (GBATEK clamping: +/-0x7FFF,
    /// index saturated to 0..88).
    pub fn adpcm_decode(&mut self, nib: u8) {
        // Clamped at the read, not only after the update below: `adpcm_idx` is a
        // `pub i32` that `snapshot` restores verbatim, so an out-of-range value —
        // or a negative one, which `as usize` turns enormous — would index this
        // 89-entry table out of bounds. That is a panic, and a Rust panic aborts
        // the process across the cxx FFI boundary. Every runtime write already
        // stays in 0..=88 (key-on takes `.min(88)`, the update below clamps), so
        // this only ever normalizes restored state.
        self.adpcm_idx = self.adpcm_idx.clamp(0, 88);
        let step = ADPCM_STEPS[self.adpcm_idx as usize];
        let mut diff = step >> 3;
        if nib & 1 != 0 {
            diff += step >> 2;
        }
        if nib & 2 != 0 {
            diff += step >> 1;
        }
        if nib & 4 != 0 {
            diff += step;
        }
        if nib & 8 != 0 {
            self.adpcm_val = (self.adpcm_val - diff).max(-0x7FFF);
        } else {
            self.adpcm_val = (self.adpcm_val + diff).min(0x7FFF);
        }
        self.adpcm_idx =
            (self.adpcm_idx + ADPCM_INDEX_DELTA[(nib & 7) as usize]).clamp(0, 88);
        self.sample = self.adpcm_val as i16;
    }

    /// Advance the PSG generator by one sample clock (the same SOUNDxTMR
    /// period as PCM; a full square cycle spans 8 clocks). `index` is the
    /// hardware channel number: 8-13 = rectangular wave (duty in CNT bits
    /// 24-26), 14-15 = LFSR noise. Format 3 on channels 0-7 is invalid and
    /// stays silent per GBATEK.
    pub fn psg_step(&mut self, index: usize) {
        if index >= 14 {
            // GBATEK noise: carry out of bit 0 taps back into bits 14+13
            // (XOR 0x6000) and drives the output level low; no carry = high.
            if self.noise_lfsr & 1 != 0 {
                self.noise_lfsr = (self.noise_lfsr >> 1) ^ 0x6000;
                self.sample = -0x7FFF;
            } else {
                self.noise_lfsr >>= 1;
                self.sample = 0x7FFF;
            }
        } else if index >= 8 {
            // Duty D: (7-D) LOW eighths then (D+1) HIGH eighths per cycle;
            // duty 7 is constant LOW (hardware-verified table, GBATEK).
            let duty = ((self.cnt >> 24) & 7) as u8;
            let phase = self.psg_phase;
            self.psg_phase = (phase + 1) & 7;
            self.sample = if duty == 7 || phase < 7 - duty {
                -0x7FFF
            } else {
                0x7FFF
            };
        }
    }
}

pub struct NdsApu {
    pub channels: [NdsChannel; 16],
    /// SOUNDCNT (0x04000500): bits 0-6 master volume, bit 15 master enable.
    /// Output-routing bits 8-13 are ignored — everything mixes. ponytail:
    /// routing matters only for sound capture, which does not exist here.
    pub soundcnt: u16,
    /// SOUNDBIAS (0x04000504): stored for readback; the normalized float mix
    /// has no PWM bias to apply.
    pub soundbias: u16,
    /// Sound capture units (SNDCAP0/1): CNT at 0x508/0x509, DAD at
    /// 0x510/0x518, LEN (words) at 0x514/0x51C. State is stored and readable;
    /// U27 evidence stage — nothing records into the DAD rings yet. The SDK
    /// reverb arms these and loop-plays the rings on channels 1/3, so an
    /// armed capture with matching ch1/ch3 SAD names the echo defect.
    pub cap_cnt: [u8; 2],
    pub cap_dad: [u32; 2],
    pub cap_len: [u16; 2],
    /// Evidence log of every capture-register byte write (offset, value),
    /// bounded; the probe dumps it to correlate arming with key-ons.
    pub cap_write_log: Vec<(u32, u8)>,
    /// W1 evidence. `dbg_peak` is the largest absolute mix output seen and
    /// `dbg_clip` counts samples that hit the limiter: a peak far below 1.0
    /// during confirmed music means the fixed /4 headroom in `mix` is costing
    /// real level. `dbg_soundcnt_seen` ORs every SOUNDCNT value ever written —
    /// bits 8-13 (output source select, and the channel 1/3 mixer bypass) are
    /// parsed nowhere, so a nonzero value there means the game routes audio in
    /// a way the mixer currently ignores.
    /// Key-ons per channel (start-bit 0->1 edges). See the census note at the
    /// write site in `NdsMmu::apu_write_byte`.
    pub key_ons: [u32; 16],
    pub dbg_peak: f64,
    pub dbg_clip: u64,
    pub dbg_samples: u64,
    pub dbg_soundcnt_seen: u16,
    pub resampler: BoxResampler,
}

impl NdsApu {
    pub fn new() -> Self {
        Self {
            channels: [NdsChannel::default(); 16],
            soundcnt: 0,
            soundbias: 0,
            cap_cnt: [0; 2],
            cap_dad: [0; 2],
            cap_len: [0; 2],
            cap_write_log: Vec::new(),
            key_ons: [0; 16],
            dbg_peak: 0.0,
            dbg_clip: 0,
            dbg_samples: 0,
            dbg_soundcnt_seen: 0,
            resampler: BoxResampler::new(),
        }
    }

    /// Byte read anywhere in 0x04000400..0x04000520 (offset within ARM7 IO).
    /// SOUNDxCNT reads back as written except bit 31 = live busy; SAD/TMR/PNT/
    /// LEN are write-only on hardware and read 0.
    pub fn read_reg(&self, offset: u32) -> u8 {
        match offset {
            0x500 => self.soundcnt as u8,
            0x501 => (self.soundcnt >> 8) as u8,
            0x504 => self.soundbias as u8,
            0x505 => (self.soundbias >> 8) as u8,
            // SNDCAPxCNT read back as written (bit 7 = running, as armed);
            // DAD is R/W per GBATEK, LEN is write-only (reads 0).
            0x508 | 0x509 => self.cap_cnt[(offset - 0x508) as usize],
            0x510..=0x513 => (self.cap_dad[0] >> ((offset - 0x510) * 8)) as u8,
            0x518..=0x51B => (self.cap_dad[1] >> ((offset - 0x518) * 8)) as u8,
            o if (0x400..0x500).contains(&o) => {
                let ch = &self.channels[((o - 0x400) / 16) as usize];
                match o & 0xF {
                    0 => ch.cnt as u8,
                    1 => (ch.cnt >> 8) as u8,
                    2 => (ch.cnt >> 16) as u8,
                    3 => ((ch.cnt >> 24) as u8 & 0x7F) | ((ch.active as u8) << 7),
                    _ => 0,
                }
            }
            _ => 0,
        }
    }

    /// Stereo mix of the current channel samples, normalized to [-1, 1].
    /// Per-channel volume (0-127), volume divider (/1 /2 /4 /16) and pan
    /// (0=left .. 127=right) per GBATEK; master volume applies last.
    ///
    /// There is no headroom divider: hardware's own clip point is the full
    /// scale of the mixer accumulator, so dividing again just throws away
    /// level. Measured on SoulSilver before this was removed, the mixer peaked
    /// at 0.0846 of full scale and clipped 0 times in 68 million samples —
    /// roughly 21 dB of unused range, of which the fixed /4 was 12 dB.
    ///
    /// ponytail: SOUNDCNT bits 8-13 (per-output source select, and the channel
    /// 1/3 mixer bypass used by the capture-based reverb path) are still
    /// ignored. Ceiling: a game routing those channels away from the mixer
    /// hears them anyway. Measured on SoulSilver the whole run only ever wrote
    /// 0x8000 — master enable — so honouring them now would be unevidenced.
    /// Upgrade path: split the per-channel accumulation and select per output.
    pub fn mix(&self) -> (f64, f64) {
        if (self.soundcnt & 0x8000) == 0 {
            return (0.0, 0.0);
        }
        let mut l = 0.0f64;
        let mut r = 0.0f64;
        for (i, ch) in self.channels.iter().enumerate() {
            // Format 3 below channel 8 is invalid (no PSG hardware there).
            if !ch.active || (ch.format() == 3 && i < 8) {
                continue;
            }
            let vol = (ch.cnt & 0x7F) as f64 / 128.0;
            let div = [1.0, 2.0, 4.0, 16.0][((ch.cnt >> 8) & 3) as usize];
            // GBATEK: both sides divide by 128, not by the 127 maximum — pan
            // 127 leaves 1/128 in the left channel, so a channel can never be
            // panned to absolute silence. Dividing by 127 also put centre pan
            // (64) at 0.504/0.496 instead of exactly half. Sub-0.1 dB either
            // way and not audible; it is here because /128 is the documented
            // law and, being a power of two, is exact and division-free.
            let pan = ((ch.cnt >> 16) & 0x7F) as f64;
            let s = ch.sample as f64 / 32768.0 * vol / div;
            l += s * (128.0 - pan) / 128.0;
            r += s * pan / 128.0;
        }
        let master = (self.soundcnt & 0x7F) as f64 / 128.0;
        (
            (l * master).clamp(-1.0, 1.0),
            (r * master).clamp(-1.0, 1.0),
        )
    }
}

impl crate::snapshot::Snap for NdsChannel {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.cnt.snap(v);
        self.sad.snap(v);
        self.tmr.snap(v);
        self.pnt.snap(v);
        self.len.snap(v);
        self.active.snap(v);
        self.cursor.snap(v);
        self.timer_acc.snap(v);
        self.sample.snap(v);
        self.adpcm_high.snap(v);
        self.adpcm_val.snap(v);
        self.adpcm_idx.snap(v);
        self.adpcm_loop_val.snap(v);
        self.adpcm_loop_idx.snap(v);
        self.psg_phase.snap(v);
        self.noise_lfsr.snap(v);
    }
}

impl crate::snapshot::Snap for NdsApu {
    /// The `dbg_*` counters and `cap_write_log` are diagnostics, not hardware
    /// state, so they are not carried: a restored state keeps the counters of
    /// the session doing the restoring.
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.channels.snap(v);
        self.soundcnt.snap(v);
        self.soundbias.snap(v);
        self.cap_cnt.snap(v);
        self.cap_dad.snap(v);
        self.cap_len.snap(v);
        self.resampler.snap(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-computed IMA vector from a zeroed decoder (val=0, idx=0, step=7):
    /// nibble 0x7 -> diff = 0+1+3+7 = 11, val=11, idx 0+8=8 (step=16);
    /// nibble 0x8 -> diff = 16>>3 = 2, sign -> val=9, idx 8-1=7.
    #[test]
    fn adpcm_decode_reference_vector() {
        let mut ch = NdsChannel::default();
        ch.adpcm_decode(0x7);
        assert_eq!(ch.sample, 11);
        assert_eq!(ch.adpcm_idx, 8);
        ch.adpcm_decode(0x8);
        assert_eq!(ch.sample, 9);
        assert_eq!(ch.adpcm_idx, 7);
    }

    /// Clamps: value saturates at +/-0x7FFF, index at 0..88.
    #[test]
    fn adpcm_decode_saturates() {
        let mut ch = NdsChannel {
            adpcm_val: 0x7FF0,
            adpcm_idx: 88, // step 32767
            ..Default::default()
        };
        ch.adpcm_decode(0x7); // huge positive diff
        assert_eq!(ch.adpcm_val, 0x7FFF);
        assert_eq!(ch.adpcm_idx, 88);
        ch.adpcm_val = -0x7FF0;
        ch.adpcm_decode(0xF); // huge negative diff, index delta +8 stays 88
        assert_eq!(ch.adpcm_val, -0x7FFF);
        for _ in 0..30 {
            ch.adpcm_decode(0x8); // index delta -1 each, floors at 0
        }
        assert_eq!(ch.adpcm_idx, 58);
    }

    /// Master enable gates the mix; pan splits a full-right channel.
    ///
    /// The pan law is `(128-pan)/128` and `pan/128`, not `/127`: GBATEK
    /// documents pan 64 as "Half", which is exactly half only when the divisor
    /// is the field's power-of-two width. The consequence at the extreme —
    /// 1/128 of a full-right channel still reaching the left output — is a
    /// property of that law, not a leak.
    #[test]
    fn mix_respects_enable_volume_and_pan() {
        let mut apu = NdsApu::new();
        apu.channels[0].active = true;
        apu.channels[0].cnt = 0x007F_007F; // vol 127, div /1, pan 127 = right
        apu.channels[0].sample = 16384;
        assert_eq!(apu.mix(), (0.0, 0.0), "master disable must silence");
        apu.soundcnt = 0x807F;
        let (l, r) = apu.mix();
        assert!(r > 0.05, "right channel must carry signal, got {r}");
        assert!(
            (r / l - 127.0).abs() < 1e-6,
            "pan 127 must split 127:1, got {r}:{l}"
        );

        // Pan 64 is documented as "Half" and must therefore be exactly centred.
        let mut centred = NdsApu::new();
        centred.soundcnt = 0x807F;
        centred.channels[0].active = true;
        centred.channels[0].cnt = 0x0040_007F; // vol 127, div /1, pan 64
        centred.channels[0].sample = 16384;
        let (cl, cr) = centred.mix();
        assert!((cl - cr).abs() < 1e-12, "pan 64 must be exactly half: {cl} vs {cr}");

        // Format 3 on channels 0-7 is invalid PSG — must stay silent there...
        apu.channels[0].cnt |= 3 << 29;
        assert_eq!(apu.mix(), (0.0, 0.0));
        // ...but a real PSG channel (8+) contributes like any other format.
        apu.channels[8] = apu.channels[0];
        apu.channels[0] = NdsChannel::default();
        let (_, r) = apu.mix();
        assert!(r > 0.05, "PSG channel 8 must reach the mix, got {r}");
    }

    /// Duty D = (7-D) LOW then (D+1) HIGH eighths; duty 7 is constant LOW.
    #[test]
    fn psg_duty_high_fractions() {
        let count_high = |duty: u32| {
            let mut ch = NdsChannel { cnt: duty << 24, ..Default::default() };
            (0..8).filter(|_| { ch.psg_step(8); ch.sample > 0 }).count()
        };
        assert_eq!(count_high(0), 1, "duty 0 = high 1/8");
        assert_eq!(count_high(3), 4, "duty 3 = high 4/8 (square)");
        assert_eq!(count_high(6), 7, "duty 6 = high 7/8");
        assert_eq!(count_high(7), 0, "duty 7 = constant low");
        // Phase sequence for duty 0: exactly the last eighth is high.
        let mut ch = NdsChannel::default();
        for k in 0..16 {
            ch.psg_step(8);
            assert_eq!(ch.sample > 0, k % 8 == 7, "step {k}");
        }
        // Channels 0-7 must NOT generate (invalid PSG): sample stays put.
        let mut ch = NdsChannel { sample: 1234, ..Default::default() };
        ch.psg_step(0);
        assert_eq!(ch.sample, 1234);
    }

    /// First 29 outputs of the seeded LFSR, hand-computed: X=0x7FFF halves
    /// through 14 taps (all carry -> LOW), reaches 0x4000, then 14 carry-free
    /// shifts (HIGH) down to 1, then taps again (LOW) landing on X=0x6000.
    #[test]
    fn psg_noise_lfsr_reference_vector() {
        let mut ch = NdsChannel { noise_lfsr: 0x7FFF, ..Default::default() };
        for k in 0..29 {
            ch.psg_step(14);
            let expect_low = k < 14 || k == 28;
            assert_eq!(ch.sample < 0, expect_low, "step {k} lfsr={:#06x}", ch.noise_lfsr);
        }
        assert_eq!(ch.noise_lfsr, 0x6000);
    }

    /// Register file: CNT reads back with live busy in bit 31; SAD/TMR/PNT/LEN
    /// are write-only (read 0).
    #[test]
    fn read_reg_busy_and_write_only_semantics() {
        let mut apu = NdsApu::new();
        apu.channels[2].cnt = 0xAB00_1234; // written start bit set
        apu.channels[2].active = false;
        let base = 0x400 + 2 * 16;
        assert_eq!(apu.read_reg(base + 3), 0x2B, "busy bit follows active, not the written bit");
        apu.channels[2].active = true;
        assert_eq!(apu.read_reg(base + 3), 0xAB);
        assert_eq!(apu.read_reg(base + 4), 0, "SAD is write-only");
        assert_eq!(apu.read_reg(base + 8), 0, "TMR is write-only");
        assert_eq!(apu.read_reg(base + 0xC), 0, "LEN is write-only");
    }
}
