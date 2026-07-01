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
            // APU is disabled. Zero all channels.
            self.ch1.enabled = false;
            self.ch2.enabled = false;
            self.ch3.enabled = false;
            self.ch4.enabled = false;
            return;
        }

        // Advance channels period timers
        self.ch1.tick_period(cycles);
        self.ch2.tick_period(cycles);
        self.ch3.tick_period(cycles);
        self.ch4.tick_period(cycles);

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

        let right_master_vol = (nr50 & 0x07) as f64 / 7.0;
        let left_master_vol = ((nr50 >> 4) & 0x07) as f64 / 7.0;

        // Resample accumulating
        // cycles_per_sample for 44.1 kHz from 4.194304 MHz (or double in double speed)
        let base_rate = if double_speed { 8388608.0 } else { 4194304.0 };
        let cycles_per_sample = (base_rate * speed as f64) / 44100.0;

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
