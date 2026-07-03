//! Shared DMG/CGB Programmable Sound Generator (PSG) channels.
//!
//! The Game Boy, Game Boy Color and Game Boy Advance all embed the *same* four
//! PSG channels (two squares, one wave, one noise) plus the 8-step 512 Hz frame
//! sequencer. Only the register byte layout and the driving clock differ between
//! the GBC and GBA MMUs. This module is the single source of truth for the
//! channel state machines so both `gbc::apu` and `gba::apu` reuse identical,
//! tested logic instead of maintaining two copies.

/// Channel 1 (square with frequency sweep).
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
        // Advance the duty pointer by every whole period the elapsed cycles span, not just
        // one: coarse tick chunks (the GBA HALT path feeds up to ~308 GBC cycles at once) and
        // very high notes (p as low as 4) would otherwise lose steps and alias down in pitch.
        let p = ((2048 - self.period) * 4) as u32; // >= 4, never 0 (period masked to 11 bits)
        let acc = self.period_timer as u32;
        if cycles < acc {
            self.period_timer = (acc - cycles) as u16;
            return;
        }
        let overflow = cycles - acc;
        let steps = 1 + overflow / p;
        self.duty_pointer = ((self.duty_pointer as u32 + steps) % 8) as u8;
        self.period_timer = (p - (overflow % p)) as u16; // in 1..=p
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

/// Channel 2 (square, no sweep).
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
        // Advance the duty pointer by every whole period the elapsed cycles span, not just
        // one: coarse tick chunks (the GBA HALT path feeds up to ~308 GBC cycles at once) and
        // very high notes (p as low as 4) would otherwise lose steps and alias down in pitch.
        let p = ((2048 - self.period) * 4) as u32; // >= 4, never 0 (period masked to 11 bits)
        let acc = self.period_timer as u32;
        if cycles < acc {
            self.period_timer = (acc - cycles) as u16;
            return;
        }
        let overflow = cycles - acc;
        let steps = 1 + overflow / p;
        self.duty_pointer = ((self.duty_pointer as u32 + steps) % 8) as u8;
        self.period_timer = (p - (overflow % p)) as u16; // in 1..=p
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

/// Channel 3 (wave RAM playback).
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
        // Advance the sample pointer by every whole period spanned (see Square1::tick_period).
        let p = ((2048 - self.period) * 2) as u32; // >= 2, never 0
        let acc = self.period_timer as u32;
        if cycles < acc {
            self.period_timer = (acc - cycles) as u16;
            return;
        }
        let overflow = cycles - acc;
        let steps = 1 + overflow / p;
        self.sample_pointer = ((self.sample_pointer as u32 + steps) % 32) as u8;
        self.period_timer = (p - (overflow % p)) as u16;
    }

    pub fn get_amplitude(&self) -> f64 {
        if !self.enabled || !self.dac_enabled || self.volume_shift == 0 {
            return 0.0;
        }
        let byte_idx = (self.sample_pointer / 2) as usize;
        let byte = self.wave_ram[byte_idx];
        let sample = (if self.sample_pointer % 2 == 0 {
            byte >> 4
        } else {
            byte & 0x0F
        }) as i32;

        // Center the 4-bit code about its midpoint (7.5) BEFORE attenuating, so a reduced
        // volume scales a bipolar wave instead of pinning it to the negative rail. On hardware
        // the DAC centers the code and the NRx2 volume shift attenuates *after* the DAC — the
        // old `(sample >> shift)` shifted toward 0 first, dragging 50%/25% output negative.
        // (2*sample - 15) is the integer-exact form of (sample - 7.5)*2, range [-15, 15].
        let centered = (2 * sample - 15) as f64 / 15.0; // [-1, 1]
        let vol = match self.volume_shift {
            1 => 1.0,  // 100%
            2 => 0.5,  // 50%
            3 => 0.25, // 25%
            _ => 0.0,  // muted (already guarded above)
        };
        centered * vol
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

/// Channel 4 (LFSR noise).
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
        let period = self.get_period(); // >= 8, never 0
        if cycles < self.period_timer {
            self.period_timer -= cycles;
            return;
        }
        let overflow = cycles - self.period_timer;
        // Clock the LFSR once per whole period spanned. The LFSR is sequential (each fold
        // depends on the previous), so this is a bounded loop rather than O(1). Ceiling:
        // cycles stays <= ~308 (the GBA /4 of a 1232-cycle HALT chunk) and p_min = 8, so
        // real `steps` <= ~39; the .min(512) is a pure anti-spin guard for a pathological
        // (cycles, period) pair and never bites in practice.
        let steps = (1 + overflow / period).min(512);
        for _ in 0..steps {
            let xor = (self.lfsr & 1) ^ ((self.lfsr >> 1) & 1);
            self.lfsr = (self.lfsr >> 1) | (xor << 14);
            if self.width_7bit {
                self.lfsr = (self.lfsr & !(1 << 6)) | (xor << 6);
            }
        }
        self.period_timer = period - (overflow % period);
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

/// One step of the 8-step 512 Hz frame sequencer, shared by both APUs.
/// Steps 0/2/4/6 clock length, 2/6 clock the ch1 sweep, 7 clocks envelopes.
pub fn frame_sequencer_step(
    step: u8,
    ch1: &mut Square1Channel,
    ch2: &mut Square2Channel,
    ch3: &mut WaveChannel,
    ch4: &mut NoiseChannel,
) {
    if step == 0 || step == 2 || step == 4 || step == 6 {
        ch1.tick_length();
        ch2.tick_length();
        ch3.tick_length();
        ch4.tick_length();
    }
    if step == 2 || step == 6 {
        ch1.tick_sweep();
    }
    if step == 7 {
        ch1.tick_envelope();
        ch2.tick_envelope();
        ch4.tick_envelope();
    }
}

/// Decode a write to a DMG/CGB-numbered sound register (`offset` in the 0x10..=0x3F
/// NRxx space, wave RAM at 0x30..=0x3F) into channel state. Both APUs funnel their
/// platform-specific register maps through this one decoder. NR50/NR51/NR52 are
/// handled by each APU directly (they read those at mix time), so they are absent here.
pub fn write_channel_register(
    ch1: &mut Square1Channel,
    ch2: &mut Square2Channel,
    ch3: &mut WaveChannel,
    ch4: &mut NoiseChannel,
    offset: u8,
    val: u8,
) {
    match offset {
        // Ch1 Sweep
        0x10 => {
            ch1.sweep_period = (val >> 4) & 0x07;
            ch1.sweep_direction = (val & 0x08) != 0;
            ch1.sweep_shift = val & 0x07;
        }
        // Ch1 Length / Duty
        0x11 => {
            ch1.duty = val >> 6;
            ch1.length_counter = 64 - (val & 0x3F) as u16;
        }
        // Ch1 Env
        0x12 => {
            ch1.env_initial_volume = val >> 4;
            ch1.env_direction = (val & 0x08) != 0;
            ch1.env_period = val & 0x07;
        }
        // Ch1 Freq Low
        0x13 => {
            ch1.period = (ch1.period & 0x0700) | (val as u16);
        }
        // Ch1 Freq High / Trigger
        0x14 => {
            ch1.period = (ch1.period & 0x00FF) | (((val & 0x07) as u16) << 8);
            ch1.length_enabled = (val & 0x40) != 0;
            if (val & 0x80) != 0 {
                ch1.trigger();
            }
        }

        // Ch2 Length / Duty
        0x16 => {
            ch2.duty = val >> 6;
            ch2.length_counter = 64 - (val & 0x3F) as u16;
        }
        // Ch2 Env
        0x17 => {
            ch2.env_initial_volume = val >> 4;
            ch2.env_direction = (val & 0x08) != 0;
            ch2.env_period = val & 0x07;
        }
        // Ch2 Freq Low
        0x18 => {
            ch2.period = (ch2.period & 0x0700) | (val as u16);
        }
        // Ch2 Freq High / Trigger
        0x19 => {
            ch2.period = (ch2.period & 0x00FF) | (((val & 0x07) as u16) << 8);
            ch2.length_enabled = (val & 0x40) != 0;
            if (val & 0x80) != 0 {
                ch2.trigger();
            }
        }

        // Ch3 DAC Enable
        0x1A => {
            ch3.dac_enabled = (val & 0x80) != 0;
            if !ch3.dac_enabled {
                ch3.enabled = false;
            }
        }
        // Ch3 Length
        0x1B => {
            ch3.length_counter = 256 - val as u16;
        }
        // Ch3 Volume Shift
        0x1C => {
            ch3.volume_shift = (val >> 5) & 0x03;
        }
        // Ch3 Freq Low
        0x1D => {
            ch3.period = (ch3.period & 0x0700) | (val as u16);
        }
        // Ch3 Freq High / Trigger
        0x1E => {
            ch3.period = (ch3.period & 0x00FF) | (((val & 0x07) as u16) << 8);
            ch3.length_enabled = (val & 0x40) != 0;
            if (val & 0x80) != 0 {
                ch3.trigger();
            }
        }

        // Ch4 Length
        0x20 => {
            ch4.length_counter = 64 - (val & 0x3F) as u16;
        }
        // Ch4 Env
        0x21 => {
            ch4.env_initial_volume = val >> 4;
            ch4.env_direction = (val & 0x08) != 0;
            ch4.env_period = val & 0x07;
        }
        // Ch4 Polynomial Counter
        0x22 => {
            ch4.shift_clock = val >> 4;
            ch4.width_7bit = (val & 0x08) != 0;
            ch4.divisor = val & 0x07;
        }
        // Ch4 Trigger
        0x23 => {
            ch4.length_enabled = (val & 0x40) != 0;
            if (val & 0x80) != 0 {
                ch4.trigger();
            }
        }

        // Wave RAM writes
        0x30..=0x3F => {
            ch3.wave_ram[(offset - 0x30) as usize] = val;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggering_square1_produces_signal() {
        let mut ch1 = Square1Channel::default();
        let mut ch2 = Square2Channel::default();
        let mut ch3 = WaveChannel::default();
        let mut ch4 = NoiseChannel::default();
        // Duty 50%, mid volume, freq, then trigger.
        write_channel_register(&mut ch1, &mut ch2, &mut ch3, &mut ch4, 0x11, 0x80); // duty 2
        write_channel_register(&mut ch1, &mut ch2, &mut ch3, &mut ch4, 0x12, 0xF0); // vol 15
        write_channel_register(&mut ch1, &mut ch2, &mut ch3, &mut ch4, 0x13, 0x00);
        write_channel_register(&mut ch1, &mut ch2, &mut ch3, &mut ch4, 0x14, 0x87); // trigger, freq hi
        assert!(ch1.enabled, "trigger must enable the channel");
        // Advance a couple of duty steps and confirm the amplitude is non-zero at some point.
        let mut saw_signal = false;
        for _ in 0..16 {
            ch1.tick_period(4096);
            if ch1.get_amplitude().abs() > 0.0 {
                saw_signal = true;
            }
        }
        assert!(saw_signal, "a triggered square must output a non-zero sample");
    }

    #[test]
    fn length_counter_disables_channel() {
        let mut ch1 = Square1Channel::default();
        let mut ch2 = Square2Channel::default();
        let mut ch3 = WaveChannel::default();
        let mut ch4 = NoiseChannel::default();
        // Length load = 63 -> counter 1; enable length; trigger.
        write_channel_register(&mut ch1, &mut ch2, &mut ch3, &mut ch4, 0x11, 0x3F);
        write_channel_register(&mut ch1, &mut ch2, &mut ch3, &mut ch4, 0x12, 0xF0);
        write_channel_register(&mut ch1, &mut ch2, &mut ch3, &mut ch4, 0x14, 0xC7); // trigger + length enable
        assert!(ch1.enabled);
        // One length clock (frame-seq step 0) should expire the 1-tick counter.
        frame_sequencer_step(0, &mut ch1, &mut ch2, &mut ch3, &mut ch4);
        assert!(!ch1.enabled, "length expiry must disable the channel");
    }

    #[test]
    fn wave_full_volume_endpoints() {
        // At 100% volume (shift 1) the min/max 4-bit codes must map to the DAC rails.
        let mut ch3 = WaveChannel {
            enabled: true,
            dac_enabled: true,
            volume_shift: 1,
            ..Default::default()
        };
        ch3.wave_ram = [0xFF; 16]; // every nibble = 0xF
        ch3.sample_pointer = 0;
        assert!((ch3.get_amplitude() - 1.0).abs() < 1e-9, "code 0xF at 100% must be +1.0");
        ch3.wave_ram = [0x00; 16]; // every nibble = 0x0
        assert!((ch3.get_amplitude() + 1.0).abs() < 1e-9, "code 0x0 at 100% must be -1.0");
    }

    #[test]
    fn wave_low_volume_is_bipolar() {
        // Regression: reduced volume must scale a *centered* wave, not pin it negative.
        // 0x0F nibbles alternate codes 0 and 15 -> centered +-1 -> at 50% volume +-0.5.
        let mut ch3 = WaveChannel {
            enabled: true,
            dac_enabled: true,
            volume_shift: 2, // 50%
            ..Default::default()
        };
        ch3.wave_ram = [0x0F; 16];
        let mut saw_pos = false;
        let mut saw_neg = false;
        let mut sum = 0.0;
        for p in 0..32u8 {
            ch3.sample_pointer = p;
            let a = ch3.get_amplitude();
            if a > 0.0 {
                saw_pos = true;
            }
            if a < 0.0 {
                saw_neg = true;
            }
            sum += a;
        }
        assert!(saw_pos && saw_neg, "50% wave must swing both signs, not sit on one rail");
        assert!((sum / 32.0).abs() < 1e-9, "a symmetric wave must have ~zero DC even at 50%");
    }

    #[test]
    fn tick_period_advances_multiple_steps() {
        // A single coarse tick spanning many periods must advance the duty pointer by the
        // whole count, not once. period=2047 -> p=4; 40 cycles from a full timer crosses
        // 1 + (40-4)/4 = 10 duty steps.
        let mut ch1 = Square1Channel {
            enabled: true,
            period: 2047,
            period_timer: 4, // = (2048-2047)*4
            duty_pointer: 0,
            ..Default::default()
        };
        ch1.tick_period(40);
        assert_eq!(ch1.duty_pointer, 2, "10 steps mod 8 = 2");
        assert!(
            ch1.period_timer >= 1 && ch1.period_timer <= 4,
            "reloaded timer must land in 1..=p"
        );
    }

    #[test]
    fn noise_lfsr_clocks_n_times() {
        // tick_period(k*p) must clock the LFSR exactly k times, matching k single-period ticks.
        let base = NoiseChannel {
            enabled: true,
            divisor: 1,      // -> get_period() = 16
            shift_clock: 0,
            lfsr: 0x7FFF,
            period_timer: 16,
            ..Default::default()
        };
        let p = base.get_period();
        assert_eq!(p, 16);

        let mut lumped = base.clone();
        lumped.tick_period(5 * p); // one coarse tick

        let mut stepped = base.clone();
        for _ in 0..5 {
            stepped.tick_period(p); // five fine ticks
        }

        assert_eq!(lumped.lfsr, stepped.lfsr, "coarse tick must equal N fine ticks");
        assert_ne!(lumped.lfsr, 0x7FFF, "the LFSR must actually have advanced");
    }
}
