use crate::psg::{
    frame_sequencer_step, write_channel_register, NoiseChannel, Square1Channel, Square2Channel,
    WaveChannel,
};

pub struct SoundFifo {
    pub buffer: [i8; 32],
    pub write_ptr: usize,
    pub read_ptr: usize,
    pub count: usize,
}

impl SoundFifo {
    pub fn new() -> Self {
        Self {
            buffer: [0; 32],
            write_ptr: 0,
            read_ptr: 0,
            count: 0,
        }
    }

    pub fn push(&mut self, val: i8) {
        if self.count < 32 {
            self.buffer[self.write_ptr] = val;
            self.write_ptr = (self.write_ptr + 1) % 32;
            self.count += 1;
        }
    }

    /// Pop the next sample, or None when empty. Hardware never snaps the DAC to zero
    /// on an underrun — it simply keeps holding the last latched sample until the
    /// driver refills the FIFO (GBATEK). Returning 0 here injected a full-swing step
    /// (audible click/crackle burst) every time the refill DMA ran late, e.g. across
    /// song transitions; callers must treat None as "keep the current latch".
    pub fn pop(&mut self) -> Option<i8> {
        if self.count > 0 {
            let val = self.buffer[self.read_ptr];
            self.read_ptr = (self.read_ptr + 1) % 32;
            self.count -= 1;
            Some(val)
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.write_ptr = 0;
        self.read_ptr = 0;
        self.count = 0;
        self.buffer.fill(0);
    }
}

// SOUNDCNT_H FIFO-reset bits live in the HIGH byte (written at I/O offset 0x83):
// bit 11 (= high-byte bit 3) resets FIFO A, bit 15 (= high-byte bit 7) resets FIFO B.
const SOUNDCNT_H_HI_FIFO_A_RESET: u8 = 0x08;
const SOUNDCNT_H_HI_FIFO_B_RESET: u8 = 0x80;

// SOUNDBIAS: bits 0-9 hold the DC bias level applied by the output DAC (BIOS default
// 0x200). Bits 14-15 (amplitude resolution / sampling cycle) are not modeled here.
const SOUNDBIAS_LEVEL_MASK: u16 = 0x03FF;
const SOUNDBIAS_DEFAULT: u16 = 0x0200;

// Relative output levels of the two mix sources, in the normalized [-1, 1] domain. These set
// how much headroom each source gets before the SOUNDBIAS DAC stage; they are calibration
// knobs, not exact hardware constants.
//   DS_GAIN: DirectSound A + B, both at 100% and panned to one side, then sum to +-1.0 (the
//            common MP2K case) instead of the old +-2.0 that brick-wall-clipped in apply_bias.
//   PSG_GAIN: the four legacy channels sit well below a full DirectSound stream on real
//            hardware; this keeps a simultaneous DS+PSG mix (MP2K drives both) inside +-1 so
//            apply_bias only clips genuine overload. Folds in the existing PSG `/4.0` average.
const DS_GAIN: f32 = 0.5;
const PSG_GAIN: f32 = 0.25;

pub struct GbaApu {
    pub fifo_a: SoundFifo,
    pub fifo_b: SoundFifo,
    pub dma_request_a: bool,
    pub dma_request_b: bool,

    // Direct Sound A & B current sample output
    pub current_sample_a: i8,
    pub current_sample_b: i8,

    // DirectSound linear-interpolation state. The DAC latch is a zero-order hold at the
    // FIFO timer rate (~13-18 kHz in MP2K games); rendering it raw leaves the first ZOH
    // image at only ~-10 dB inside the audible band (constant fizz, metallic tails on
    // percussion). Instead the mix ramps linearly from the PREVIOUS latch to the current
    // one across the measured overflow period (a causal, one-period-delayed lerp,
    // ~75 us — inaudible), pushing the first image below ~-35 dB. The period is measured
    // between consecutive overflows of the selected timer — not successful pops — so a
    // FIFO underrun degenerates to prev == current and flat-holds exactly like before.
    // Transient state, not serialized (same policy as the resampler; savestate load
    // re-syncs prev_sample_* to the loaded latch so no stale ramp is audible).
    pub prev_sample_a: i8,
    pub prev_sample_b: i8,
    pub cycles_since_overflow_a: u32,
    pub cycles_since_overflow_b: u32,
    // 0 = no overflow observed yet -> the interp degrades to plain ZOH of the latch.
    pub overflow_period_a: u32,
    pub overflow_period_b: u32,

    // Sound registers
    pub soundcnt_h: u16,
    pub soundcnt_x: u16,
    pub soundbias: u16,

    // PSG channels 1-4 (shared DMG/CGB silicon). MP2K drives these alongside the
    // DirectSound FIFOs; without them PSG-voiced tracks are silent and mixed tracks
    // sound thin. NR50 (master L/R volume) and NR51 (per-channel L/R pan).
    pub ch1: Square1Channel,
    pub ch2: Square2Channel,
    pub ch3: WaveChannel,
    pub ch4: NoiseChannel,
    pub nr50: u8,
    pub nr51: u8,
    pub frame_seq_timer: u32,
    pub frame_seq_step: u8,
    // GBA clock is exactly 4x the GBC clock the PSG channel constants assume; this
    // accumulates the 4:1 remainder so PSG timing stays exact (see `tick`).
    pub psg_cycle_acc: u32,

    // Downsampling accumulator
    pub cycle_accumulator: f64,

    // Resampler
    pub resampler: crate::resampler::BoxResampler,
}

impl GbaApu {
    pub fn new() -> Self {
        Self {
            fifo_a: SoundFifo::new(),
            fifo_b: SoundFifo::new(),
            dma_request_a: false,
            dma_request_b: false,
            current_sample_a: 0,
            current_sample_b: 0,
            prev_sample_a: 0,
            prev_sample_b: 0,
            cycles_since_overflow_a: 0,
            cycles_since_overflow_b: 0,
            overflow_period_a: 0,
            overflow_period_b: 0,
            soundcnt_h: 0,
            soundcnt_x: 0x80, // enabled
            soundbias: SOUNDBIAS_DEFAULT,
            ch1: Square1Channel::default(),
            ch2: Square2Channel::default(),
            ch3: WaveChannel::default(),
            ch4: NoiseChannel::default(),
            nr50: 0,
            nr51: 0,
            frame_seq_timer: 0,
            frame_seq_step: 0,
            psg_cycle_acc: 0,
            cycle_accumulator: 0.0,
            resampler: crate::resampler::BoxResampler::new(),
        }
    }

    pub fn write_register(&mut self, offset: u32, value: u8) {
        match offset {
            0x82 => {
                // Low byte: DMG/DirectSound volume + DirectSound-A enable bits. No
                // FIFO-reset bits live here (those are in the high byte, offset 0x83).
                self.soundcnt_h = (self.soundcnt_h & 0xFF00) | (value as u16);
            }
            0x83 => {
                // High byte: DirectSound-B enable + timer-select + the two FIFO-reset
                // bits. Both reset bits may be set in a single write, so test them
                // independently. Resetting a FIFO clears only its buffer/pointers; the
                // last latched output sample keeps playing until the next timer pop.
                let old_soundcnt_h = self.soundcnt_h;
                self.soundcnt_h = (self.soundcnt_h & 0x00FF) | ((value as u16) << 8);

                if (old_soundcnt_h & 0x0400) != (self.soundcnt_h & 0x0400) {
                    self.reset_ds_a_interpolation();
                }
                if (old_soundcnt_h & 0x4000) != (self.soundcnt_h & 0x4000) {
                    self.reset_ds_b_interpolation();
                }

                if (value & SOUNDCNT_H_HI_FIFO_A_RESET) != 0 {
                    self.fifo_a.clear();
                    self.reset_ds_a_interpolation();
                }
                if (value & SOUNDCNT_H_HI_FIFO_B_RESET) != 0 {
                    self.fifo_b.clear();
                    self.reset_ds_b_interpolation();
                }
            }
            0x84 => {
                self.soundcnt_x = (self.soundcnt_x & 0xFF00) | (value as u16);
            }
            0x85 => {
                self.soundcnt_x = (self.soundcnt_x & 0x00FF) | ((value as u16) << 8);
            }
            0x88 => {
                self.soundbias = (self.soundbias & 0xFF00) | (value as u16);
            }
            0x89 => {
                self.soundbias = (self.soundbias & 0x00FF) | ((value as u16) << 8);
            }
            _ => {}
        }
    }

    /// Route a PSG register byte write. GBA lays the NRxx registers out with gaps
    /// (SOUND1CNT_L/H/X at 0x60/0x62/0x64, etc.) unlike the GBC's contiguous block,
    /// so translate the GBA I/O offset to the GBC NRxx offset and reuse the shared
    /// decoder. Wave RAM (0x90-0x9F) maps to 0x30-0x3F. NR50/NR51 (0x80/0x81) are
    /// latched locally for mixing; unmapped gap offsets are ignored.
    pub fn write_psg_register(&mut self, offset: u32, value: u8) {
        // ponytail: single-bank 32-sample wave only (matches the reused GBC WaveChannel).
        // GBA's 64-sample dual-bank mode (SOUND3CNT_L bits 5-6) is unused by MP2K music;
        // add a second bank + bank-select if a game ever needs it.
        let gbc_offset: Option<u8> = match offset {
            0x60 => Some(0x10),
            0x62 => Some(0x11),
            0x63 => Some(0x12),
            0x64 => Some(0x13),
            0x65 => Some(0x14),
            0x68 => Some(0x16),
            0x69 => Some(0x17),
            0x6C => Some(0x18),
            0x6D => Some(0x19),
            0x70 => Some(0x1A),
            0x72 => Some(0x1B),
            0x73 => Some(0x1C),
            0x74 => Some(0x1D),
            0x75 => Some(0x1E),
            0x78 => Some(0x20),
            0x79 => Some(0x21),
            0x7C => Some(0x22),
            0x7D => Some(0x23),
            0x90..=0x9F => Some(0x30 + (offset - 0x90) as u8),
            0x80 => {
                self.nr50 = value;
                None
            }
            0x81 => {
                self.nr51 = value;
                None
            }
            _ => None,
        };
        if let Some(gbc) = gbc_offset {
            write_channel_register(
                &mut self.ch1,
                &mut self.ch2,
                &mut self.ch3,
                &mut self.ch4,
                gbc,
                value,
            );
        }
    }

    pub fn write_fifo_a(&mut self, value: u32) {
        // Push 4 bytes of 32-bit word into FIFO A
        self.fifo_a.push((value & 0xFF) as i8);
        self.fifo_a.push(((value >> 8) & 0xFF) as i8);
        self.fifo_a.push(((value >> 16) & 0xFF) as i8);
        self.fifo_a.push(((value >> 24) & 0xFF) as i8);
    }

    pub fn write_fifo_b(&mut self, value: u32) {
        // Push 4 bytes of 32-bit word into FIFO B
        self.fifo_b.push((value & 0xFF) as i8);
        self.fifo_b.push(((value >> 8) & 0xFF) as i8);
        self.fifo_b.push(((value >> 16) & 0xFF) as i8);
        self.fifo_b.push(((value >> 24) & 0xFF) as i8);
    }

    pub fn on_timer_overflow(&mut self, timer_index: usize) {
        // Direct Sound A Timer Select: Bit 10 of SOUNDCNT_H (0=Timer 0, 1=Timer 1)
        let timer_a = if (self.soundcnt_h & 0x0400) != 0 {
            1
        } else {
            0
        };
        if timer_index == timer_a {
            // Interp bookkeeping: measure the overflow-to-overflow period and roll the
            // latch into prev BEFORE the pop, so the mix ramps prev -> current across
            // the next period. On underrun prev == current -> flat hold, unchanged.
            self.overflow_period_a = self.cycles_since_overflow_a.max(1);
            self.cycles_since_overflow_a = 0;
            self.prev_sample_a = self.current_sample_a;
            // Empty FIFO: hold the latch (see SoundFifo::pop) — the DMA request below
            // still fires so the stream recovers as soon as the driver catches up.
            if let Some(v) = self.fifo_a.pop() {
                self.current_sample_a = v;
            }
            // Trigger DMA request if FIFO has 16 or fewer bytes (4 words or fewer)
            if self.fifo_a.count <= 16 {
                self.dma_request_a = true;
            }
        }

        // Direct Sound B Timer Select: Bit 14 of SOUNDCNT_H (0=Timer 0, 1=Timer 1)
        let timer_b = if (self.soundcnt_h & 0x4000) != 0 {
            1
        } else {
            0
        };
        if timer_index == timer_b {
            self.overflow_period_b = self.cycles_since_overflow_b.max(1);
            self.cycles_since_overflow_b = 0;
            self.prev_sample_b = self.current_sample_b;
            if let Some(v) = self.fifo_b.pop() {
                self.current_sample_b = v;
            }
            if self.fifo_b.count <= 16 {
                self.dma_request_b = true;
            }
        }
    }

    pub fn reset_ds_a_interpolation(&mut self) {
        self.cycles_since_overflow_a = 0;
        self.overflow_period_a = 0;
    }

    pub fn reset_ds_b_interpolation(&mut self) {
        self.cycles_since_overflow_b = 0;
        self.overflow_period_b = 0;
    }

    pub fn handle_timer_change(&mut self, timer_idx: usize) {
        let timer_a = if (self.soundcnt_h & 0x0400) != 0 { 1 } else { 0 };
        if timer_idx == timer_a {
            self.reset_ds_a_interpolation();
        }
        let timer_b = if (self.soundcnt_h & 0x4000) != 0 { 1 } else { 0 };
        if timer_idx == timer_b {
            self.reset_ds_b_interpolation();
        }
    }

    /// GBA cycles until the next 512 Hz frame-sequencer step — the only point where
    /// CPU-visible PSG state (length expiry / NR52 channel-on flags, sweep, envelope)
    /// changes between timer overflows. Used by the batching scheduler as an event
    /// boundary. frame_seq_timer < 8192 and psg_cycle_acc < 4 always hold, so the
    /// result is >= 1.
    pub fn cycles_to_next_frame_seq(&self) -> u32 {
        (8192 - self.frame_seq_timer) * 4 - self.psg_cycle_acc
    }

    /// GBC cycles until any *audible* PSG state can next change between frame-sequencer
    /// steps: the earliest enabled channel's period-timer expiry (duty/wave/LFSR edge),
    /// bounded by the next 512 Hz frame-sequencer step (envelope/length/sweep). None when
    /// every channel is idle — then the mix is constant for arbitrarily long chunks.
    /// period_timer can legitimately be 0 right after a raw register write (an edge is
    /// due immediately); clamp each distance to >= 1 so callers always make progress.
    fn psg_cycles_to_next_edge(&self) -> Option<u32> {
        let candidates = [
            (self.ch1.enabled, self.ch1.period_timer as u32),
            (self.ch2.enabled, self.ch2.period_timer as u32),
            (
                self.ch3.enabled && self.ch3.dac_enabled,
                self.ch3.period_timer as u32,
            ),
            (self.ch4.enabled, self.ch4.period_timer),
        ];
        let mut next: Option<u32> = None;
        for (on, t) in candidates {
            if on {
                let t = t.max(1);
                next = Some(next.map_or(t, |n| n.min(t)));
            }
        }
        // frame_seq_timer < 8192 always holds (see tick), so the bound is >= 1.
        next.map(|n| n.min(8192 - self.frame_seq_timer))
    }

    pub fn tick(&mut self, cycles: u32, audio_buffer: &mut [i16], audio_offset: usize, speed: f32) {
        let cycles_per_sample = (16777216.0 * speed as f64) / 44100.0;
        if (self.soundcnt_x & 0x80) == 0 {
            // APU disabled: emit silence SAMPLES (not zero samples) so the 44.1 kHz
            // stream stays continuous — an early return starves the frontend queue
            // for these cycles and the refill edge is an audible click.
            // The interp counters still advance: timer overflows keep firing while the
            // master enable is off, so the measured period must stay truthful.
            // saturating_add: with no DS timer running the counter would otherwise
            // overflow u32 after ~256 s.
            self.cycles_since_overflow_a = self.cycles_since_overflow_a.saturating_add(cycles);
            self.cycles_since_overflow_b = self.cycles_since_overflow_b.saturating_add(cycles);
            self.resampler
                .tick(cycles, 0.0, 0.0, cycles_per_sample, audio_buffer, audio_offset);
            return;
        }

        // Sub-step the chunk at PSG edges. The batching scheduler hands the APU up to
        // ~cycles_per_sample (~381) cycles at once; rendering such a chunk with a single
        // post-advance mix quantizes every duty/wave/LFSR edge to the output-sample grid
        // (audible rasp/sheen). Instead: render the mix of the state at the chunk START
        // for exactly the cycles until the next edge, then advance the channels across
        // it — the box resampler integrates each edge on its exact cycle. Idle PSG (the
        // common MP2K case) takes one iteration for the whole chunk. Iterations are
        // bounded: every sub-chunk is >= 1 cycle and the shortest PSG period is 4 GBC
        // = 16 GBA cycles, so a 400-cycle chunk costs at most ~25 mixes, only while an
        // extreme-pitch note is actually playing.
        let mut rem = cycles;
        while rem > 0 {
            let sub = match self.psg_cycles_to_next_edge() {
                // The edge fires when (psg_cycle_acc + sub) spans edge_gbc whole GBC
                // cycles: smallest such sub is edge_gbc*4 - psg_cycle_acc (acc < 4).
                Some(edge_gbc) => rem
                    .min((edge_gbc * 4).saturating_sub(self.psg_cycle_acc))
                    .max(1),
                None => rem,
            };

            // Evaluate the DS ramp at the midpoint of this sub-chunk: the box resampler
            // integrates a constant over the chunk, and midpoint (trapezoid) evaluation
            // is EXACT integration for a linear segment. Sub-chunks never span a FIFO
            // pop (the scheduler clamps steps to timer overflows and ticks the APU
            // before the timers), so the segment really is linear across `sub`.
            let (left, right) = self.current_mix(sub / 2);
            self.resampler.tick(
                sub,
                left as f64,
                right as f64,
                cycles_per_sample,
                audio_buffer,
                audio_offset,
            );
            self.cycles_since_overflow_a = self.cycles_since_overflow_a.saturating_add(sub);
            self.cycles_since_overflow_b = self.cycles_since_overflow_b.saturating_add(sub);

            // Advance the PSG channels in the GBC clock domain. The reused channel
            // constants assume the 4.19 MHz GBC clock; the GBA runs at exactly 4x, so
            // accumulate GBA cycles and feed the integer /4 quotient, carrying the
            // remainder for exact timing.
            self.psg_cycle_acc += sub;
            let gbc_cycles = self.psg_cycle_acc / 4;
            self.psg_cycle_acc %= 4;
            if gbc_cycles > 0 {
                self.ch1.tick_period(gbc_cycles);
                self.ch2.tick_period(gbc_cycles);
                self.ch3.tick_period(gbc_cycles);
                self.ch4.tick_period(gbc_cycles);

                // Frame sequencer at 512 Hz (every 8192 GBC cycles).
                self.frame_seq_timer += gbc_cycles;
                if self.frame_seq_timer >= 8192 {
                    self.frame_seq_timer -= 8192;
                    let step = self.frame_seq_step;
                    self.frame_seq_step = (step + 1) % 8;
                    frame_sequencer_step(
                        step,
                        &mut self.ch1,
                        &mut self.ch2,
                        &mut self.ch3,
                        &mut self.ch4,
                    );
                }
            }

            rem -= sub;
        }
    }

    /// Post-SOUNDBIAS stereo mix (normalized [-1, 1]) of the DirectSound streams and the
    /// PSG channels, from CURRENT state — callers must mix BEFORE advancing channel
    /// timers so a chunk renders the state at its start. The PSG part is instantaneous;
    /// the DirectSound part is the linear prev->current latch ramp evaluated `ds_mid`
    /// cycles past the sub-chunk start (callers pass the sub-chunk midpoint, see `tick`).
    fn current_mix(&self, ds_mid: u32) -> (f32, f32) {
        // DirectSound latch value on the prev -> current linear ramp. period == 0 (no
        // overflow observed yet) degrades to plain ZOH of the current latch; the frac
        // clamp bounds a stale period (timer-select/reload change) to one mis-sloped
        // but discontinuity-free ramp. Output stays inside [min(prev,cur), max(prev,cur)],
        // so the interp can never introduce new clipping downstream.
        let ds_interp = |prev: i8, cur: i8, age: u32, period: u32| -> f32 {
            let frac = if period == 0 {
                1.0
            } else {
                (age.saturating_add(ds_mid) as f32 / period as f32).min(1.0)
            };
            let p = (prev as f32) / 128.0;
            p + ((cur as f32) / 128.0 - p) * frac
        };

        // Direct Sound A
        let dsa_l = (self.soundcnt_h & 0x0200) != 0;
        let dsa_r = (self.soundcnt_h & 0x0100) != 0;
        let dsa_vol_100 = (self.soundcnt_h & 0x0004) != 0;
        let sample_a = ds_interp(
            self.prev_sample_a,
            self.current_sample_a,
            self.cycles_since_overflow_a,
            self.overflow_period_a,
        );
        let scaled_a = sample_a * (if dsa_vol_100 { 1.0 } else { 0.5 }) * DS_GAIN;

        // Direct Sound B
        let dsb_l = (self.soundcnt_h & 0x2000) != 0;
        let dsb_r = (self.soundcnt_h & 0x1000) != 0;
        let dsb_vol_100 = (self.soundcnt_h & 0x0008) != 0;
        let sample_b = ds_interp(
            self.prev_sample_b,
            self.current_sample_b,
            self.cycles_since_overflow_b,
            self.overflow_period_b,
        );
        let scaled_b = sample_b * (if dsb_vol_100 { 1.0 } else { 0.5 }) * DS_GAIN;

        // Direct Sound mixing
        let mut left_ds = 0.0;
        let mut right_ds = 0.0;

        if dsa_l {
            left_ds += scaled_a;
        }
        if dsa_r {
            right_ds += scaled_a;
        }
        if dsb_l {
            left_ds += scaled_b;
        }
        if dsb_r {
            right_ds += scaled_b;
        }

        // PSG (channels 1-4): mix per side with NR51 pan and NR50 master volume (same as
        // the GBC path), then scale by the SOUNDCNT_H PSG output ratio and add to the mix.
        //
        // Idle fast-path: every get_amplitude() returns exactly +0.0 when its channel is
        // disabled (Square1/2 & Noise: `!enabled || volume==0`; Wave: `!enabled ||
        // !dac_enabled || volume_shift==0`). If all four channels are disabled, psg_l and
        // psg_r are exactly +0.0, so the two adds below reduce to `left_ds += 0.0f32` /
        // `right_ds += 0.0f32`. left_ds/right_ds are always non-negative-zero finite here
        // (they start at +0.0 and only accumulate `+= scaled_a/scaled_b`, which are +0.0
        // when the sample is 0, never a subtraction), so `x + 0.0 == x` bit-for-bit and
        // skipping the whole block is behavior-preserving. tick_period() and the frame
        // sequencer above are unaffected. The PSG channels are idle the vast majority of
        // ticks in DirectSound-driven (MP2K) games, so this elides four get_amplitude()
        // calls plus the NR50/NR51/ratio float math on nearly every ~2-cycle tick.
        if self.ch1.enabled || self.ch2.enabled || self.ch3.enabled || self.ch4.enabled {
            let ch1_amp = self.ch1.get_amplitude();
            let ch2_amp = self.ch2.get_amplitude();
            let ch3_amp = self.ch3.get_amplitude();
            let ch4_amp = self.ch4.get_amplitude();
            let left_master = ((self.nr50 >> 4) & 0x07) as f64 / 7.0;
            let right_master = (self.nr50 & 0x07) as f64 / 7.0;
            let mut psg_l = 0.0;
            let mut psg_r = 0.0;
            if (self.nr51 & 0x10) != 0 {
                psg_l += ch1_amp;
            }
            if (self.nr51 & 0x20) != 0 {
                psg_l += ch2_amp;
            }
            if (self.nr51 & 0x40) != 0 {
                psg_l += ch3_amp;
            }
            if (self.nr51 & 0x80) != 0 {
                psg_l += ch4_amp;
            }
            if (self.nr51 & 0x01) != 0 {
                psg_r += ch1_amp;
            }
            if (self.nr51 & 0x02) != 0 {
                psg_r += ch2_amp;
            }
            if (self.nr51 & 0x04) != 0 {
                psg_r += ch3_amp;
            }
            if (self.nr51 & 0x08) != 0 {
                psg_r += ch4_amp;
            }
            // SOUNDCNT_H bits 0-1: PSG->DirectSound output ratio (0=25%, 1=50%, 2/3=100%).
            let psg_ratio = match self.soundcnt_h & 0x03 {
                0 => 0.25,
                1 => 0.5,
                _ => 1.0,
            };
            left_ds += ((psg_l / 4.0) * left_master * psg_ratio) as f32 * PSG_GAIN;
            right_ds += ((psg_r / 4.0) * right_master * psg_ratio) as f32 * PSG_GAIN;
        }

        // SOUNDBIAS output stage: the GBA DAC rides the mix on a DC bias and clamps to a
        // 10-bit window [0, 0x3FF]. With the BIOS-default bias (0x200) an in-range signal
        // passes through unchanged; a game that shifts the bias gets the hardware's
        // asymmetric clipping. Modeled in the normalized domain (full-scale swing = the
        // default 0x200 = 512). Saturating `clamp` keeps a hostile ROM-supplied bias from
        // overflowing or distorting the output away — it can only re-shape the clip point.
        let bias = (self.soundbias & SOUNDBIAS_LEVEL_MASK) as f32;
        let apply_bias = |s: f32| -> f32 { ((s * 512.0 + bias).clamp(0.0, 1023.0) - bias) / 512.0 };
        (apply_bias(left_ds), apply_bias(right_ds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_apu_still_emits_silence_samples() {
        // SOUNDCNT_X bit7 clear must not stall the 44.1 kHz stream (see GBC twin test).
        let mut apu = GbaApu::new();
        apu.soundcnt_x = 0; // master disable
        let mut buf = vec![0i16; 4096];
        apu.tick(280_896, &mut buf, 0, 1.0); // one full frame of cycles
        let n = apu.resampler.sample_count;
        assert!(
            (730..=740).contains(&n),
            "expected ~738 silence samples from a disabled APU, got {n}"
        );
        assert!(buf[..n * 2].iter().all(|&s| s == 0));
    }

    #[test]
    fn fifo_empty_pop_holds_last_sample() {
        let mut apu = GbaApu::new();
        apu.fifo_a.push(42);
        apu.on_timer_overflow(0); // soundcnt_h = 0: DS A sourced from timer 0
        assert_eq!(apu.current_sample_a, 42);
        // FIFO now empty: an underrun must hold the DAC latch, never snap to 0
        // (the snap was an audible click whenever the refill DMA ran late).
        apu.on_timer_overflow(0);
        assert_eq!(apu.current_sample_a, 42, "empty-FIFO pop must hold the latch");
        assert!(apu.dma_request_a, "an underrun still requests a refill");
    }

    #[test]
    fn ds_interp_ramps_linearly_between_pops() {
        // The DS output must ramp linearly from prev to current across the overflow
        // period, evaluated at each sub-chunk MIDPOINT (trapezoid = exact integration
        // of a linear segment). prev=-100 -> cur=100 over period 256, rendered as
        // 4x64-cycle chunks: midpoints at 32/96/160/224 cycles -> lerp values
        // -75/-25/+25/+75 -> per-chunk left_sum deltas of value/128 * DS_GAIN * 64.
        let mut apu = GbaApu::new();
        apu.prev_sample_a = -100;
        apu.current_sample_a = 100;
        apu.overflow_period_a = 256;
        apu.cycles_since_overflow_a = 0;
        // dsa_left(0x0200) | dsa_right(0x0100) | dsa_vol100(0x0004)
        apu.soundcnt_h = 0x0200 | 0x0100 | 0x0004;

        let mut buf = vec![0i16; 8];
        let expected_cumulative = [-18.75, -25.0, -18.75, 0.0];
        for (i, &expected) in expected_cumulative.iter().enumerate() {
            // 4*64 = 256 total cycles < cycles_per_sample (~380): no sample is
            // emitted, so the raw integration stays inspectable in left_sum.
            apu.tick(64, &mut buf, 0, 1.0);
            let got = apu.resampler.left_sum;
            assert!(
                (got - expected).abs() < 1e-6,
                "chunk {i}: cumulative left_sum {got}, expected {expected}"
            );
        }
    }

    #[test]
    fn ds_interp_period_measured_between_overflows() {
        // The interp period is measured overflow-to-overflow of the selected timer and
        // the latch pair rolls prev <- current on every overflow.
        let mut apu = GbaApu::new();
        apu.fifo_a.push(10);
        apu.fifo_a.push(20);
        apu.on_timer_overflow(0); // soundcnt_h = 0: DS A sourced from timer 0
        assert_eq!(apu.current_sample_a, 10);

        let mut buf = vec![0i16; 8];
        apu.tick(200, &mut buf, 0, 1.0); // < cycles_per_sample: no sample emitted

        apu.on_timer_overflow(0);
        assert_eq!(apu.overflow_period_a, 200, "period = cycles between overflows");
        assert_eq!(apu.prev_sample_a, 10, "prev must hold the pre-pop latch");
        assert_eq!(apu.current_sample_a, 20);
        assert_eq!(apu.cycles_since_overflow_a, 0, "counter restarts at the overflow");
    }

    #[test]
    fn ds_interp_underrun_renders_flat_hold() {
        // An underrun overflow sets prev == current, so the rendered ramp is flat at
        // the held latch — the audible behavior the ZOH hold guaranteed before interp.
        let mut apu = GbaApu::new();
        apu.current_sample_a = 100;
        apu.on_timer_overflow(0); // FIFO empty: prev <- 100, latch holds 100
        assert_eq!(apu.prev_sample_a, 100);
        assert_eq!(apu.current_sample_a, 100);
        apu.soundcnt_h = 0x0200 | 0x0100 | 0x0004;

        let mut buf = vec![0i16; 8];
        // Per chunk: (100/128) * DS_GAIN(0.5) * 64 cycles = 25.0, exactly flat.
        apu.tick(64, &mut buf, 0, 1.0);
        assert!((apu.resampler.left_sum - 25.0).abs() < 1e-6);
        apu.tick(64, &mut buf, 0, 1.0);
        assert!((apu.resampler.left_sum - 50.0).abs() < 1e-6);
    }

    #[test]
    fn psg_edge_integrated_exactly() {
        // A duty edge inside a coarse tick chunk must be integrated on its exact
        // cycle by the sub-step loop, not quantized to the chunk/sample boundary.
        let mut apu = GbaApu::new();
        apu.ch2.enabled = true;
        apu.ch2.volume = 15;
        apu.ch2.duty = 2; // 50%: table [1,0,0,0,0,1,1,1]
        apu.ch2.duty_pointer = 4; // idx4 = 0 (amp -1), next idx5 = 1 (amp +1)
        apu.ch2.period = 2047; // p = (2048-2047)*4 = 4 GBC cycles
        apu.ch2.period_timer = 1; // edge after 1 GBC = 4 GBA cycles
        apu.write_psg_register(0x81, 0x22); // NR51: ch2 left+right
        apu.write_psg_register(0x80, 0x77); // NR50: full L/R master
        apu.soundcnt_h = 0x0002; // PSG ratio 100%

        let mut buf = vec![0i16; 8];
        // 16 cycles < cycles_per_sample (~380): no sample is emitted, so the raw
        // integration stays inspectable in left_sum.
        apu.tick(16, &mut buf, 0, 1.0);

        // Per-cycle PSG contribution = (amp/4) * master(1.0) * ratio(1.0) * PSG_GAIN
        // = amp * 0.0625. Exactly 4 GBA cycles at -1, then the edge flips to +1
        // for the remaining 12.
        let expected = (-0.0625 * 4.0) + (0.0625 * 12.0);
        let got = apu.resampler.left_sum;
        assert!(
            (got - expected).abs() < 1e-9,
            "duty edge must land on its exact cycle: got {got}, expected {expected}"
        );
    }

    #[test]
    fn psg_wave_ram_write_lands_in_channel3() {
        let mut apu = GbaApu::new();
        apu.write_psg_register(0x90, 0xAB);
        apu.write_psg_register(0x9F, 0xCD);
        assert_eq!(apu.ch3.wave_ram[0], 0xAB);
        assert_eq!(apu.ch3.wave_ram[15], 0xCD);
    }

    #[test]
    fn psg_square1_trigger_and_mix() {
        let mut apu = GbaApu::new();
        // SOUND1CNT_H: duty 50% (bit 7..6 = 10) in low byte (NR11), envelope vol 15 in high (NR12).
        apu.write_psg_register(0x62, 0x80); // NR11 duty=2
        apu.write_psg_register(0x63, 0xF0); // NR12 initial volume 15, no envelope decay
        apu.write_psg_register(0x64, 0x00); // NR13 freq low
        apu.write_psg_register(0x65, 0x87); // NR14 trigger + freq high
        assert!(apu.ch1.enabled, "trigger must enable channel 1");

        // Enable PSG output: NR51 pan ch1 to both sides, NR50 full master volume, PSG ratio 100%.
        apu.write_psg_register(0x81, 0x11); // NR51: ch1 left+right
        apu.write_psg_register(0x80, 0x77); // NR50: full L/R master
        apu.soundcnt_h |= 0x02; // PSG ratio = 100%

        // Run ~1 frame of GBA cycles; expect at least one non-zero sample in the buffer.
        let mut buf = vec![0i16; 4096];
        for _ in 0..2000 {
            apu.tick(160, &mut buf, 0, 1.0);
        }
        assert!(
            buf.iter().any(|&s| s != 0),
            "a triggered, panned, unmuted PSG square must produce audible output"
        );
    }

    #[test]
    fn psg_silent_when_master_disabled() {
        let mut apu = GbaApu::new();
        apu.write_psg_register(0x62, 0x80);
        apu.write_psg_register(0x63, 0xF0);
        apu.write_psg_register(0x65, 0x87);
        apu.write_psg_register(0x81, 0x11);
        apu.write_psg_register(0x80, 0x77);
        apu.soundcnt_x = 0; // master enable (bit 7) cleared
        let mut buf = vec![0i16; 4096];
        for _ in 0..2000 {
            apu.tick(160, &mut buf, 0, 1.0);
        }
        assert!(buf.iter().all(|&s| s == 0), "master-disabled APU must be silent");
    }

    #[test]
    fn ds_mix_stays_linear_when_summed() {
        // DirectSound A + B, both at half-scale (64) and both routed to the left, must sum in
        // the linear region: 0.25 + 0.25 = 0.5 -> ~14600 out. The old full-scale (+-1 each)
        // normalization summed to 1.0 and apply_bias clamped it to ~30000, flattening the
        // waveform. The output filters shape early samples (the LPF's first-sample gain is
        // only ~0.48), so read sample 7: the biquad step response has settled and only the
        // DC blocker's slow droop remains (0.996^7 ~= 0.972 -> ~14580 expected).
        let mut apu = GbaApu::new();
        apu.current_sample_a = 64;
        apu.current_sample_b = 64;
        // dsa_left(0x0200) | dsb_left(0x2000) | dsa_vol100(0x0004) | dsb_vol100(0x0008)
        apu.soundcnt_h = 0x0200 | 0x2000 | 0x0004 | 0x0008;

        let mut buf = vec![0i16; 64];
        // 3200 GBA cycles / ~380 cycles-per-sample emits 8 samples.
        apu.tick(3200, &mut buf, 0, 1.0);
        assert!(apu.resampler.sample_count >= 8, "eight samples must be emitted");

        let left = buf[14];
        assert!(
            (13500..=16500).contains(&left),
            "summed DS must stay linear (~14600), got {left} (old clamped path gives ~29000)"
        );
        assert_eq!(buf[15], 0, "nothing was routed right");
    }

    #[test]
    fn test_directsound_interpolation_resets() {
        let mut apu = GbaApu::new();

        // 1. Check basic interpolation resets
        apu.cycles_since_overflow_a = 100;
        apu.overflow_period_a = 200;
        apu.reset_ds_a_interpolation();
        assert_eq!(apu.cycles_since_overflow_a, 0);
        assert_eq!(apu.overflow_period_a, 0);

        apu.cycles_since_overflow_b = 150;
        apu.overflow_period_b = 300;
        apu.reset_ds_b_interpolation();
        assert_eq!(apu.cycles_since_overflow_b, 0);
        assert_eq!(apu.overflow_period_b, 0);

        // 2. Check handle_timer_change
        // By default, soundcnt_h is 0, meaning:
        // DS A uses Timer 0 (bit 10 is 0)
        // DS B uses Timer 0 (bit 14 is 0)
        apu.cycles_since_overflow_a = 100;
        apu.overflow_period_a = 200;
        apu.cycles_since_overflow_b = 150;
        apu.overflow_period_b = 300;
        apu.handle_timer_change(1); // neither uses Timer 1
        assert_eq!(apu.overflow_period_a, 200);
        assert_eq!(apu.overflow_period_b, 300);

        apu.handle_timer_change(0); // both use Timer 0
        assert_eq!(apu.overflow_period_a, 0);
        assert_eq!(apu.overflow_period_b, 0);

        // Set soundcnt_h: DS A uses Timer 1 (bit 10), DS B uses Timer 0
        apu.soundcnt_h = 0x0400; 
        apu.cycles_since_overflow_a = 100;
        apu.overflow_period_a = 200;
        apu.cycles_since_overflow_b = 150;
        apu.overflow_period_b = 300;
        apu.handle_timer_change(0); // DS B uses Timer 0, DS A uses Timer 1
        assert_eq!(apu.overflow_period_a, 200);
        assert_eq!(apu.overflow_period_b, 0);

        apu.handle_timer_change(1); // DS A uses Timer 1
        assert_eq!(apu.overflow_period_a, 0);

        // Set soundcnt_h: DS A uses Timer 0, DS B uses Timer 1 (bit 14)
        apu.soundcnt_h = 0x4000;
        apu.cycles_since_overflow_a = 100;
        apu.overflow_period_a = 200;
        apu.cycles_since_overflow_b = 150;
        apu.overflow_period_b = 300;
        apu.handle_timer_change(1); // DS B uses Timer 1
        assert_eq!(apu.overflow_period_b, 0);
        assert_eq!(apu.overflow_period_a, 200);

        // 3. Check write_register at offset 0x83
        // Setup initial soundcnt_h with DS A uses Timer 0, DS B uses Timer 0 (all 0)
        apu.soundcnt_h = 0;
        apu.cycles_since_overflow_a = 100;
        apu.overflow_period_a = 200;
        apu.cycles_since_overflow_b = 150;
        apu.overflow_period_b = 300;

        // Write new value to high byte of soundcnt_h:
        // Set bit 10 of soundcnt_h (high byte bit 2, i.e., value |= 0x04).
        // This changes DS A timer select bit.
        apu.write_register(0x83, 0x04);
        assert_eq!(apu.overflow_period_a, 0, "DS A timer select changed, expected reset");
        assert_eq!(apu.overflow_period_b, 300, "DS B timer select did not change");

        // Write new value to high byte:
        // Set bit 14 of soundcnt_h (high byte bit 6, i.e., value |= 0x40).
        // DS A select stays bit 10 set (value has 0x40 | 0x04 = 0x44).
        apu.cycles_since_overflow_a = 100;
        apu.overflow_period_a = 200;
        apu.cycles_since_overflow_b = 150;
        apu.overflow_period_b = 300;
        apu.write_register(0x83, 0x44);
        assert_eq!(apu.overflow_period_a, 200, "DS A timer select did not change");
        assert_eq!(apu.overflow_period_b, 0, "DS B timer select changed, expected reset");

        // Test FIFO A reset commanded (high-byte bit 3 = 0x08)
        apu.cycles_since_overflow_a = 100;
        apu.overflow_period_a = 200;
        apu.write_register(0x83, 0x44 | 0x08);
        assert_eq!(apu.overflow_period_a, 0, "FIFO A reset should reset interpolation state");

        // Test FIFO B reset commanded (high-byte bit 7 = 0x80)
        apu.cycles_since_overflow_b = 150;
        apu.overflow_period_b = 300;
        apu.write_register(0x83, 0x44 | 0x80);
        assert_eq!(apu.overflow_period_b, 0, "FIFO B reset should reset interpolation state");
    }
}
