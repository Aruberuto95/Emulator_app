/// GBC APU Channel 1 (Square with Sweep)
#[derive(Clone, Default)]
pub struct Square1Channel {
    pub enabled: bool,
    pub duty: u8,
    pub duty_pointer: u8,
    pub length_enabled: bool,
    pub length_counter: u16,
    pub period: u16,
    pub period_timer: u16,
    pub volume: u8,
    pub env_enabled: bool,
    pub env_period: u8,
    pub env_timer: u8,
    pub env_direction: bool, // true = add, false = sub
    pub env_initial_volume: u8,
    pub sweep_enabled: bool,
    pub sweep_period: u8,
    pub sweep_timer: u8,
    pub sweep_shift: u8,
    pub sweep_direction: bool, // true = sub, false = add
    pub shadow_frequency: u16,
}

impl Square1Channel {
    pub fn trigger(&mut self) {
        self.enabled = true;
        if self.length_counter == 0 {
            self.length_counter = 64;
        }
        self.period_timer = (2048 - self.period) * 4;
        self.env_timer = self.env_period;
        self.volume = self.env_initial_volume;
        self.env_enabled = self.env_period > 0;
        self.shadow_frequency = self.period;
        self.sweep_timer = self.sweep_period;
        self.sweep_enabled = self.sweep_period > 0 || self.sweep_shift > 0;
    }

    pub fn tick_period(&mut self, cycles: u32) {
        if !self.enabled {
            return;
        }
        if self.period_timer > cycles as u16 {
            self.period_timer -= cycles as u16;
        } else {
            let overflow = cycles as u16 - self.period_timer;
            let p = (2048 - self.period) * 4;
            self.period_timer = p - (overflow % p);
            self.duty_pointer = (self.duty_pointer + 1) % 8;
        }
    }

    pub fn get_amplitude(&self) -> f64 {
        if !self.enabled || self.volume == 0 {
            return 0.0;
        }
        let duty_table = match self.duty {
            0 => [0, 0, 0, 0, 0, 0, 0, 1], // 12.5%
            1 => [1, 0, 0, 0, 0, 0, 0, 1], // 25%
            2 => [1, 0, 0, 0, 0, 1, 1, 1], // 50%
            3 => [0, 1, 1, 1, 1, 1, 1, 0], // 75%
            _ => [0; 8],
        };
        if duty_table[self.duty_pointer as usize] == 1 {
            (self.volume as f64) / 15.0
        } else {
            -(self.volume as f64) / 15.0
        }
    }

    pub fn tick_length(&mut self) {
        if self.length_enabled && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
            }
        }
    }

    pub fn tick_envelope(&mut self) {
        if !self.env_enabled || self.env_period == 0 {
            return;
        }
        if self.env_timer > 0 {
            self.env_timer -= 1;
            if self.env_timer == 0 {
                self.env_timer = self.env_period;
                if self.env_direction {
                    if self.volume < 15 {
                        self.volume += 1;
                    } else {
                        self.env_enabled = false;
                    }
                } else {
                    if self.volume > 0 {
                        self.volume -= 1;
                    } else {
                        self.env_enabled = false;
                    }
                }
            }
        }
    }

    pub fn tick_sweep(&mut self) {
        if !self.sweep_enabled || self.sweep_period == 0 {
            return;
        }
        if self.sweep_timer > 0 {
            self.sweep_timer -= 1;
            if self.sweep_timer == 0 {
                self.sweep_timer = self.sweep_period;
                let mut new_freq = self.shadow_frequency;
                let delta = new_freq >> self.sweep_shift;
                if self.sweep_direction {
                    new_freq = new_freq.wrapping_sub(delta);
                } else {
                    new_freq = new_freq.wrapping_add(delta);
                }

                if new_freq > 2047 {
                    self.enabled = false;
                    self.sweep_enabled = false;
                } else if self.sweep_shift > 0 {
                    self.shadow_frequency = new_freq;
                    self.period = new_freq;
                }
            }
        }
    }
}

/// GBC APU Channel 2 (Square)
#[derive(Clone, Default)]
pub struct Square2Channel {
    pub enabled: bool,
    pub duty: u8,
    pub duty_pointer: u8,
    pub length_enabled: bool,
    pub length_counter: u16,
    pub period: u16,
    pub period_timer: u16,
    pub volume: u8,
    pub env_enabled: bool,
    pub env_period: u8,
    pub env_timer: u8,
    pub env_direction: bool,
    pub env_initial_volume: u8,
}

impl Square2Channel {
    pub fn trigger(&mut self) {
        self.enabled = true;
        if self.length_counter == 0 {
            self.length_counter = 64;
        }
        self.period_timer = (2048 - self.period) * 4;
        self.env_timer = self.env_period;
        self.volume = self.env_initial_volume;
        self.env_enabled = self.env_period > 0;
    }

    pub fn tick_period(&mut self, cycles: u32) {
        if !self.enabled {
            return;
        }
        if self.period_timer > cycles as u16 {
            self.period_timer -= cycles as u16;
        } else {
            let overflow = cycles as u16 - self.period_timer;
            let p = (2048 - self.period) * 4;
            self.period_timer = p - (overflow % p);
            self.duty_pointer = (self.duty_pointer + 1) % 8;
        }
    }

    pub fn get_amplitude(&self) -> f64 {
        if !self.enabled || self.volume == 0 {
            return 0.0;
        }
        let duty_table = match self.duty {
            0 => [0, 0, 0, 0, 0, 0, 0, 1],
            1 => [1, 0, 0, 0, 0, 0, 0, 1],
            2 => [1, 0, 0, 0, 0, 1, 1, 1],
            3 => [0, 1, 1, 1, 1, 1, 1, 0],
            _ => [0; 8],
        };
        if duty_table[self.duty_pointer as usize] == 1 {
            (self.volume as f64) / 15.0
        } else {
            -(self.volume as f64) / 15.0
        }
    }

    pub fn tick_length(&mut self) {
        if self.length_enabled && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
            }
        }
    }

    pub fn tick_envelope(&mut self) {
        if !self.env_enabled || self.env_period == 0 {
            return;
        }
        if self.env_timer > 0 {
            self.env_timer -= 1;
            if self.env_timer == 0 {
                self.env_timer = self.env_period;
                if self.env_direction {
                    if self.volume < 15 {
                        self.volume += 1;
                    } else {
                        self.env_enabled = false;
                    }
                } else {
                    if self.volume > 0 {
                        self.volume -= 1;
                    } else {
                        self.env_enabled = false;
                    }
                }
            }
        }
    }
}

/// GBC APU Channel 3 (Wave RAM)
#[derive(Clone)]
pub struct WaveChannel {
    pub enabled: bool,
    pub dac_enabled: bool,
    pub length_enabled: bool,
    pub length_counter: u16,
    pub period: u16,
    pub period_timer: u16,
    pub volume_shift: u8,
    pub wave_ram: [u8; 16],
    pub sample_pointer: u8,
}

impl Default for WaveChannel {
    fn default() -> Self {
        Self {
            enabled: false,
            dac_enabled: false,
            length_enabled: false,
            length_counter: 0,
            period: 0,
            period_timer: 0,
            volume_shift: 0,
            wave_ram: [0u8; 16],
            sample_pointer: 0,
        }
    }
}

impl WaveChannel {
    pub fn trigger(&mut self) {
        self.enabled = true;
        if self.length_counter == 0 {
            self.length_counter = 256;
        }
        self.period_timer = (2048 - self.period) * 2;
        self.sample_pointer = 0;
    }

    pub fn tick_period(&mut self, cycles: u32) {
        if !self.enabled || !self.dac_enabled {
            return;
        }
        if self.period_timer > cycles as u16 {
            self.period_timer -= cycles as u16;
        } else {
            let overflow = cycles as u16 - self.period_timer;
            let p = (2048 - self.period) * 2;
            self.period_timer = p - (overflow % p);
            self.sample_pointer = (self.sample_pointer + 1) % 32;
        }
    }

    pub fn get_amplitude(&self) -> f64 {
        if !self.enabled || !self.dac_enabled || self.volume_shift == 0 {
            return 0.0;
        }
        let byte_idx = (self.sample_pointer / 2) as usize;
        let byte = self.wave_ram[byte_idx];
        let sample = if self.sample_pointer % 2 == 0 {
            byte >> 4
        } else {
            byte & 0x0F
        };

        // Apply shift: 1 = 100%, 2 = 50%, 3 = 25%, 0 = muted
        let shift = match self.volume_shift {
            1 => 0,
            2 => 1,
            3 => 2,
            _ => 4,
        };
        let shifted_sample = sample >> shift;
        (shifted_sample as f64 / 15.0) * 2.0 - 1.0
    }

    pub fn tick_length(&mut self) {
        if self.length_enabled && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
            }
        }
    }
}

/// GBC APU Channel 4 (Noise)
#[derive(Clone, Default)]
pub struct NoiseChannel {
    pub enabled: bool,
    pub length_enabled: bool,
    pub length_counter: u16,
    pub volume: u8,
    pub env_enabled: bool,
    pub env_period: u8,
    pub env_timer: u8,
    pub env_direction: bool,
    pub env_initial_volume: u8,
    pub lfsr: u16,
    pub divisor: u8,
    pub shift_clock: u8,
    pub width_7bit: bool,
    pub period_timer: u32,
}

impl NoiseChannel {
    pub fn trigger(&mut self) {
        self.enabled = true;
        if self.length_counter == 0 {
            self.length_counter = 64;
        }
        self.lfsr = 0x7FFF;
        self.env_timer = self.env_period;
        self.volume = self.env_initial_volume;
        self.env_enabled = self.env_period > 0;
        self.period_timer = self.get_period();
    }

    fn get_period(&self) -> u32 {
        let div = match self.divisor {
            0 => 8,
            d => (d as u32) * 16,
        };
        div << self.shift_clock
    }

    pub fn tick_period(&mut self, cycles: u32) {
        if !self.enabled {
            return;
        }
        let period = self.get_period();
        if self.period_timer > cycles {
            self.period_timer -= cycles;
        } else {
            let overflow = cycles - self.period_timer;
            self.period_timer = period - (overflow % period);

            let xor = (self.lfsr & 1) ^ ((self.lfsr >> 1) & 1);
            self.lfsr = (self.lfsr >> 1) | (xor << 14);
            if self.width_7bit {
                self.lfsr = (self.lfsr & !(1 << 6)) | (xor << 6);
            }
        }
    }

    pub fn get_amplitude(&self) -> f64 {
        if !self.enabled || self.volume == 0 {
            return 0.0;
        }
        // Output is the inverted lowest bit of LFSR
        let bit = (self.lfsr & 1) ^ 1;
        if bit == 1 {
            (self.volume as f64) / 15.0
        } else {
            -(self.volume as f64) / 15.0
        }
    }

    pub fn tick_length(&mut self) {
        if self.length_enabled && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
            }
        }
    }

    pub fn tick_envelope(&mut self) {
        if !self.env_enabled || self.env_period == 0 {
            return;
        }
        if self.env_timer > 0 {
            self.env_timer -= 1;
            if self.env_timer == 0 {
                self.env_timer = self.env_period;
                if self.env_direction {
                    if self.volume < 15 {
                        self.volume += 1;
                    } else {
                        self.env_enabled = false;
                    }
                } else {
                    if self.volume > 0 {
                        self.volume -= 1;
                    } else {
                        self.env_enabled = false;
                    }
                }
            }
        }
    }
}

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
            self.tick_frame_sequencer();
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

    fn tick_frame_sequencer(&mut self) {
        let step = self.frame_seq_step;
        self.frame_seq_step = (step + 1) % 8;

        // Step 0, 2, 4, 6: Clock Length counter
        if step == 0 || step == 2 || step == 4 || step == 6 {
            self.ch1.tick_length();
            self.ch2.tick_length();
            self.ch3.tick_length();
            self.ch4.tick_length();
        }

        // Step 2, 6: Clock Sweep
        if step == 2 || step == 6 {
            self.ch1.tick_sweep();
        }

        // Step 7: Clock Volume Envelope
        if step == 7 {
            self.ch1.tick_envelope();
            self.ch2.tick_envelope();
            self.ch4.tick_envelope();
        }
    }

    /// Intercept I/O register writes to update APU state variables.
    pub fn write_register(&mut self, offset: u8, val: u8, _io: &mut [u8]) {
        match offset {
            // Ch1 Sweep
            0x10 => {
                self.ch1.sweep_period = (val >> 4) & 0x07;
                self.ch1.sweep_direction = (val & 0x08) != 0;
                self.ch1.sweep_shift = val & 0x07;
            }
            // Ch1 Length / Duty
            0x11 => {
                self.ch1.duty = val >> 6;
                self.ch1.length_counter = 64 - (val & 0x3F) as u16;
            }
            // Ch1 Env
            0x12 => {
                self.ch1.env_initial_volume = val >> 4;
                self.ch1.env_direction = (val & 0x08) != 0;
                self.ch1.env_period = val & 0x07;
            }
            // Ch1 Freq Low
            0x13 => {
                self.ch1.period = (self.ch1.period & 0x0700) | (val as u16);
            }
            // Ch1 Freq High / Trigger
            0x14 => {
                self.ch1.period = (self.ch1.period & 0x00FF) | (((val & 0x07) as u16) << 8);
                self.ch1.length_enabled = (val & 0x40) != 0;
                if (val & 0x80) != 0 {
                    self.ch1.trigger();
                }
            }

            // Ch2 Length / Duty
            0x16 => {
                self.ch2.duty = val >> 6;
                self.ch2.length_counter = 64 - (val & 0x3F) as u16;
            }
            // Ch2 Env
            0x17 => {
                self.ch2.env_initial_volume = val >> 4;
                self.ch2.env_direction = (val & 0x08) != 0;
                self.ch2.env_period = val & 0x07;
            }
            // Ch2 Freq Low
            0x18 => {
                self.ch2.period = (self.ch2.period & 0x0700) | (val as u16);
            }
            // Ch2 Freq High / Trigger
            0x19 => {
                self.ch2.period = (self.ch2.period & 0x00FF) | (((val & 0x07) as u16) << 8);
                self.ch2.length_enabled = (val & 0x40) != 0;
                if (val & 0x80) != 0 {
                    self.ch2.trigger();
                }
            }

            // Ch3 DAC Enable
            0x1A => {
                self.ch3.dac_enabled = (val & 0x80) != 0;
                if !self.ch3.dac_enabled {
                    self.ch3.enabled = false;
                }
            }
            // Ch3 Length
            0x1B => {
                self.ch3.length_counter = 256 - val as u16;
            }
            // Ch3 Volume Shift
            0x1C => {
                self.ch3.volume_shift = (val >> 5) & 0x03;
            }
            // Ch3 Freq Low
            0x1D => {
                self.ch3.period = (self.ch3.period & 0x0700) | (val as u16);
            }
            // Ch3 Freq High / Trigger
            0x1E => {
                self.ch3.period = (self.ch3.period & 0x00FF) | (((val & 0x07) as u16) << 8);
                self.ch3.length_enabled = (val & 0x40) != 0;
                if (val & 0x80) != 0 {
                    self.ch3.trigger();
                }
            }

            // Ch4 Length
            0x20 => {
                self.ch4.length_counter = 64 - (val & 0x3F) as u16;
            }
            // Ch4 Env
            0x21 => {
                self.ch4.env_initial_volume = val >> 4;
                self.ch4.env_direction = (val & 0x08) != 0;
                self.ch4.env_period = val & 0x07;
            }
            // Ch4 Polynomial Counter
            0x22 => {
                self.ch4.shift_clock = val >> 4;
                self.ch4.width_7bit = (val & 0x08) != 0;
                self.ch4.divisor = val & 0x07;
            }
            // Ch4 Trigger
            0x23 => {
                self.ch4.length_enabled = (val & 0x40) != 0;
                if (val & 0x80) != 0 {
                    self.ch4.trigger();
                }
            }

            // Wave RAM writes
            0x30..=0x3F => {
                self.ch3.wave_ram[(offset - 0x30) as usize] = val;
            }
            _ => {}
        }
    }
}
