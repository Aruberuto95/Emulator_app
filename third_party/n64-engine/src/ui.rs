//! Host adapter. Emulated peripherals never open files, windows or audio devices.
//! The application owns timing and persistence; this boundary carries typed data.
pub(crate) use crate::storage;
use crate::{bridge::ffi, device};

#[derive(Default)]
pub struct Storage {
    pub saves: storage::Saves,
    pub save_type: Vec<storage::SaveTypes>,
}
pub struct Ui {
    // Drops before the Device's RDRAM. Never serialize GPU handles.
    pub renderer: cxx::UniquePtr<ffi::Renderer>,
    pub game_id: String,
    pub game_hash: String,
    pub storage: Storage,
    pub controllers: [bool; 4],
    pub input: [u32; 4],
    pub rumble: [std::cell::Cell<u8>; 4],
    pub samples: Vec<i16>,
    pub output_hz: u32,
    pub input_hz: u64,
    pub phase: f64,
    pub previous: [i16; 2],
    pub error: Option<String>,
}
impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}
impl Ui {
    pub fn new() -> Self {
        Self {
            renderer: cxx::UniquePtr::null(),
            game_id: String::new(),
            game_hash: String::new(),
            storage: Storage::default(),
            controllers: [true, false, false, false],
            input: [0; 4],
            rumble: std::array::from_fn(|_| std::cell::Cell::new(0)),
            samples: Vec::with_capacity(4096),
            output_hz: 44100,
            input_hz: 33600,
            phase: 0.0,
            previous: [0; 2],
            error: None,
        }
    }
}
pub mod input {
    pub struct Input {
        pub data: u32,
        pub pak_change_pressed: bool,
    }
    pub fn get(ui: &super::Ui, channel: usize) -> Input {
        Input {
            data: ui.input[channel],
            pak_change_pressed: false,
        }
    }
    pub fn set_rumble(ui: &super::Ui, channel: usize, strength: u8) {
        ui.rumble[channel].set(strength);
    }
}
pub mod video {
    use super::*;
    pub fn onscreen_message(_ui: &Ui, message: &str) {
        eprintln!("N64: {message}");
    }
    pub fn set_register(ui: &mut Ui, index: u32, value: u32) {
        if !ui.renderer.is_null() {
            ui.renderer.pin_mut().set_register(index, value);
        }
    }
    pub fn process_rdp_list(d: &mut device::Device) -> u64 {
        match d
            .ui
            .renderer
            .pin_mut()
            .process(&d.rsp.mem[..4096], &mut d.rdp.regs_dpc)
        {
            Ok(timer) => timer,
            Err(error) => {
                d.ui.error = Some(error.to_string());
                d.cpu.running = false;
                0
            }
        }
    }
}
pub mod audio {
    use super::*;
    pub fn close_game_audio(_ui: &mut Ui) {} // No host device belongs to the emulated DAC.
    pub fn init_game_audio(ui: &mut Ui, hz: u64) {
        ui.input_hz = hz.max(1);
    }
    pub fn play_audio(d: &mut device::Device, address: usize, length: u64) {
        if length > 0x40000 || length % 4 != 0 {
            d.ui.error = Some("Invalid N64 audio DMA length".into());
            d.cpu.running = false;
            return;
        }
        let ratio = d.ui.input_hz as f64 / d.ui.output_hz as f64;
        for offset in (0..length as usize).step_by(4) {
            let a = address + offset;
            // RDRAM is word-swapped on this supported little-endian host.
            let word = d.rdram.mem.get(a..a + 4).unwrap_or(&[0; 4]);
            let current = [
                i16::from_le_bytes([word[2], word[3]]),
                i16::from_le_bytes([word[0], word[1]]),
            ];
            while d.ui.phase < 1.0 {
                for channel in 0..2 {
                    let previous = f64::from(d.ui.previous[channel]);
                    let sample = previous + (f64::from(current[channel]) - previous) * d.ui.phase;
                    d.ui.samples
                        .push(sample.round().clamp(-32768.0, 32767.0) as i16);
                }
                d.ui.phase += ratio;
                if d.ui.samples.len() > 384_000 * 2 {
                    d.ui.error = Some("N64 audio output limit exceeded".into());
                    d.cpu.running = false;
                    return;
                }
            }
            d.ui.phase -= 1.0;
            d.ui.previous = current;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stereo_resampling_keeps_phase_across_dma_blocks() {
        let mut d = Box::new(crate::device::Device::new());
        d.rdram.mem = crate::ram::AlignedRam::new(0x400000);
        d.ui.input_hz = 48000;
        d.ui.output_hz = 96000;
        for (i, (left, right)) in [(1000i16, -1000i16), (-1000, 1000)].into_iter().enumerate() {
            let word = ((left as u16 as u32) << 16) | (right as u16 as u32);
            d.rdram.mem[i * 4..i * 4 + 4].copy_from_slice(&word.to_ne_bytes());
        }
        super::audio::play_audio(&mut d, 0, 4);
        super::audio::play_audio(&mut d, 4, 4);
        assert_eq!(d.ui.samples, [0, 0, 500, -500, 1000, -1000, 0, 0]);
        assert_eq!(d.ui.phase, 0.0);
        super::audio::play_audio(&mut d, 0, 0x40004);
        assert!(d.ui.error.is_some(), "oversized DMA must be rejected");
    }
}
