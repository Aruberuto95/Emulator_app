//! Game Boy Color APU. The four channel state machines and the frame sequencer
//! live in [`crate::psg`] (shared with the GBA APU); this module owns the GBC
//! register map, NR50/NR51/NR52 mixing and the resampler wiring.

use crate::psg::{
    frame_sequencer_step, write_channel_register, NoiseChannel, Square1Channel, Square2Channel,
    WaveChannel,
};

/// Game Boy Color Audio Processing Unit (APU) with Resampler.
#[derive(Clone)]
pub struct Apu {
    pub ch1: Square1Channel,
    pub ch2: Square2Channel,
    pub ch3: WaveChannel,
    pub ch4: NoiseChannel,

    // Frame Sequencer
    pub frame_seq_timer: u32,
    pub frame_seq_step: u8,

    /// Odd cycle left over when halving the CPU clock for the channel period
    /// timers in CGB double-speed mode; see [`Apu::tick`]. Carried so no cycle
    /// is lost across calls, which would flatten the pitch by a hair over time.
    ///
    /// ponytail: not carried in the JSON savestate, whose field list is fixed
    /// and versionless. Ceiling: a restore can be one 4 MHz cycle out of phase,
    /// which is 1/4194304 of a second and cannot be heard. Upgrade path: add it
    /// when that format next gains a version field.
    psg_cycle_carry: u32,

    // Resampler
    pub resampler: crate::resampler::BoxResampler,
}

impl Apu {
    pub fn new() -> Self {
        Self {
            ch1: Square1Channel::default(),
            ch2: Square2Channel::default(),
            ch3: WaveChannel::default(),
            ch4: NoiseChannel::default(),
            frame_seq_timer: 0,
            frame_seq_step: 0,
            psg_cycle_carry: 0,
            resampler: crate::resampler::BoxResampler::new(),
        }
    }

    pub fn reset(&mut self) {
        self.ch1 = Square1Channel::default();
        self.ch2 = Square2Channel::default();
        self.ch3 = WaveChannel::default();
        self.ch4 = NoiseChannel::default();
        self.frame_seq_timer = 0;
        self.frame_seq_step = 0;
        self.psg_cycle_carry = 0;
        self.resampler.reset();
    }

    /// Update channels and resample output into the stereo 44.1 kHz buffer.
    pub fn tick(
        &mut self,
        cycles: u32,
        io: &mut [u8],
        audio_buffer: &mut [i16],
        audio_offset: usize,
        double_speed: bool,
        speed: f32,
    ) {
        // NR52 (Sound Enable) check
        let nr52 = io[0x26];
        if (nr52 & 0x80) == 0 {
            // APU is disabled. Zero all channels, but still emit silence SAMPLES so
            // the 44.1 kHz stream stays continuous — an early return starves the
            // frontend queue for these cycles and the refill edge is an audible click.
            self.ch1.enabled = false;
            self.ch2.enabled = false;
            self.ch3.enabled = false;
            self.ch4.enabled = false;
            let base_rate = if double_speed { 8388608.0 } else { 4194304.0 };
            let cycles_per_sample = (base_rate * speed as f64) / self.resampler.output_hz();
            self.resampler
                .tick(cycles, 0.0, 0.0, cycles_per_sample, audio_buffer, audio_offset);
            return;
        }

        // Advance the channel period timers.
        //
        // These clock from the 4.194304 MHz base even in CGB double-speed mode:
        // doubling KEY1 doubles the CPU and the frame sequencer's DIV tap, not
        // the sound generator, so a note keeps its pitch when a game switches
        // speed. `cycles` arrives in CPU cycles, which the caller has already
        // doubled (the frame budget goes 70224 -> 140448), so it must be halved
        // back here. Both other consumers in this function already compensate
        // -- `cycles_per_sample` uses the 8.4 MHz base and `frame_seq_rate` uses
        // 16384 -- and this was the one that did not, which ran all four
        // channels at twice their written frequency: exactly one octave sharp
        // for as long as the game stayed in double speed (Pokemon Crystal spends
        // most of its runtime there).
        //
        // The odd cycle is carried rather than dropped, so halving cannot
        // accumulate a systematic pitch error.
        let psg_cycles = if double_speed {
            let total = self.psg_cycle_carry + cycles;
            self.psg_cycle_carry = total & 1;
            total >> 1
        } else {
            self.psg_cycle_carry = 0;
            cycles
        };
        self.ch1.tick_period(psg_cycles);
        self.ch2.tick_period(psg_cycles);
        self.ch3.tick_period(psg_cycles);
        self.ch4.tick_period(psg_cycles);

        // Frame Sequencer (512 Hz timer)
        // 512 Hz is once every 8192 cycles (or 16384 in double speed)
        let frame_seq_rate = if double_speed { 16384 } else { 8192 };
        self.frame_seq_timer += cycles;
        if self.frame_seq_timer >= frame_seq_rate {
            self.frame_seq_timer -= frame_seq_rate;
            let step = self.frame_seq_step;
            self.frame_seq_step = (step + 1) % 8;
            frame_sequencer_step(step, &mut self.ch1, &mut self.ch2, &mut self.ch3, &mut self.ch4);
        }

        // Map audio settings
        let nr50 = io[0x24];
        let nr51 = io[0x25];

        // NR50 master volume is (n+1)/8, not n/7. GBATEK and the Pan Docs both
        // describe the field as "volume 0 is NOT silent" -- level 0 is the
        // quietest of eight steps, at 1/8 of full scale. Dividing by 7 made
        // level 0 exact digital silence, so a driver fading a track out by
        // ramping NR50 to 0 while the channels keep playing cut the output dead
        // one step early instead of leaving the quietest step audible.
        let right_master_vol = ((nr50 & 0x07) as f64 + 1.0) / 8.0;
        let left_master_vol = (((nr50 >> 4) & 0x07) as f64 + 1.0) / 8.0;

        // Resample accumulating
        // cycles_per_sample for 44.1 kHz from 4.194304 MHz (or double in double speed)
        let base_rate = if double_speed { 8388608.0 } else { 4194304.0 };
        let cycles_per_sample = (base_rate * speed as f64) / self.resampler.output_hz();

        // ponytail: this mix is sampled AFTER `tick_period` advanced the channels,
        // so a duty/wave edge inside the chunk is rendered as if it had happened at
        // the chunk start. Ceiling: edges quantize to the caller's chunk, which is
        // 4..24 cycles out of `Cpu::step` (HALT included — it returns 4), i.e. <=25%
        // of one output sample at 4.19 MHz / 44.1 kHz, against the ~100% that made
        // the same defect audible on the GBA's ~381-cycle chunks. Measured GBC pitch
        // is within +/-5 cents, so this stays unfixed for want of evidence.
        // Upgrade path: hoist `gba::apu::psg_cycles_to_next_edge` into `psg.rs` (it
        // already works on these four shared channel types) and drive both consoles
        // from one edge-bounded sub-step loop, the way `nds::mmu::tick_apu` now does.
        let ch1_amp = self.ch1.get_amplitude();
        let ch2_amp = self.ch2.get_amplitude();
        let ch3_amp = self.ch3.get_amplitude();
        let ch4_amp = self.ch4.get_amplitude();

        // Left mixing
        let mut left_ch = 0.0;
        if (nr51 & 0x10) != 0 {
            left_ch += ch1_amp;
        }
        if (nr51 & 0x20) != 0 {
            left_ch += ch2_amp;
        }
        if (nr51 & 0x40) != 0 {
            left_ch += ch3_amp;
        }
        if (nr51 & 0x80) != 0 {
            left_ch += ch4_amp;
        }
        left_ch = (left_ch / 4.0) * left_master_vol;

        // Right mixing
        let mut right_ch = 0.0;
        if (nr51 & 0x01) != 0 {
            right_ch += ch1_amp;
        }
        if (nr51 & 0x02) != 0 {
            right_ch += ch2_amp;
        }
        if (nr51 & 0x04) != 0 {
            right_ch += ch3_amp;
        }
        if (nr51 & 0x08) != 0 {
            right_ch += ch4_amp;
        }
        right_ch = (right_ch / 4.0) * right_master_vol;

        self.resampler.tick(
            cycles,
            left_ch,
            right_ch,
            cycles_per_sample,
            audio_buffer,
            audio_offset,
        );

        // Update sound status bits in NR52
        let mut updated_nr52 = nr52 & 0x80;
        if self.ch1.enabled {
            updated_nr52 |= 0x01;
        }
        if self.ch2.enabled {
            updated_nr52 |= 0x02;
        }
        if self.ch3.enabled {
            updated_nr52 |= 0x04;
        }
        if self.ch4.enabled {
            updated_nr52 |= 0x08;
        }
        io[0x26] = updated_nr52;
    }

    /// Intercept I/O register writes to update APU state variables. GBC NRxx registers
    /// are contiguous from 0xFF10 (offset 0x10); the shared decoder handles them.
    pub fn write_register(&mut self, offset: u8, val: u8, _io: &mut [u8]) {
        write_channel_register(
            &mut self.ch1,
            &mut self.ch2,
            &mut self.ch3,
            &mut self.ch4,
            offset,
            val,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_apu_still_emits_silence_samples() {
        // NR52 bit7 clear must not stall the 44.1 kHz stream: the frontend queues
        // whatever tick produced, so a sample gap here becomes an audible click.
        let mut apu = Apu::new();
        let mut io = vec![0u8; 0x100];
        io[0x26] = 0x00; // master disable
        let mut buf = vec![0i16; 4096];
        apu.tick(70224, &mut io, &mut buf, 0, false, 1.0); // one full frame of cycles
        let n = apu.resampler.sample_count;
        assert!(
            (730..=740).contains(&n),
            "expected ~735 silence samples from a disabled APU, got {n}"
        );
        assert!(buf[..n * 2].iter().all(|&s| s == 0));
    }

    #[test]
    fn dc_blocker_removes_offset_on_asymmetric_duty() {
        // A 12.5% duty square is maximally asymmetric: its raw average is -0.75*v, which
        // becomes ~ -5600 at the output before filtering. The resampler's DC blocker must
        // null that offset (a steady periodic tone has zero DC), otherwise it thumps.
        let mut apu = Apu::new();
        let mut io = vec![0u8; 0x100];
        io[0x26] = 0x80; // NR52 master enable
        io[0x24] = 0x77; // NR50 full L/R master
        io[0x25] = 0x11; // NR51 ch1 left + right
        apu.write_register(0x11, 0x00, &mut io); // duty 0 (12.5%), length load
        apu.write_register(0x12, 0xF0, &mut io); // volume 15, no envelope
        apu.write_register(0x13, 0x00, &mut io); // freq low
        apu.write_register(0x14, 0x87, &mut io); // trigger, freq high, length off

        let mut buf = vec![0i16; 12000];
        // sample_count is not reset inside tick(); let it accumulate to ~4000 samples.
        while apu.resampler.sample_count < 4000 {
            apu.tick(64, &mut io, &mut buf, 0, false, 1.0);
        }
        let n = apu.resampler.sample_count;
        // Average the left channel over the second half, past the HPF settling transient.
        let start = n / 2;
        let sum: i64 = (start..n).map(|i| buf[i * 2] as i64).sum();
        let mean = sum as f64 / (n - start) as f64;
        assert!(
            mean.abs() < 800.0,
            "DC blocker must null the duty offset; mean={mean} (unfiltered ~ -5600)"
        );
    }
}
