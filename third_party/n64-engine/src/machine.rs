// Embedded N64 machine adapted from Gopher64 v1.1.3 (see UPSTREAM.md).
// Owns CPU/RSP/peripherals and an independent C++ paraLLEl-RDP instance.
// No upstream application, network client or filesystem persistence is linked.
mod battery;
mod bridge;
mod device;
mod ram;
mod savestates;
mod snapshot;
mod storage;
mod ui;

pub struct Engine {
    device: Box<device::Device>,
    frame: bridge::ffi::VideoFrame,
    accessories: [Accessory; 4],
}
/// Keep Rust guest faults inside Rust; unwinding through the C++ ABI is invalid.
fn guest_boundary<T>(operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)).map_err(|payload| {
        let detail = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("unknown guest fault");
        format!("N64 emulation stopped: {detail}")
    })?
}
impl Engine {
    pub fn new(rom: Vec<u8>, output_hz: u32) -> Result<Self, String> {
        guest_boundary(|| Self::initialize(rom, output_hz))
    }
    fn initialize(rom: Vec<u8>, output_hz: u32) -> Result<Self, String> {
        if rom.len() < 4096
            || rom.len() > 64 * 1024 * 1024
            || rom.len() % 4 != 0
            || rom.get(..4) != Some(&[0x80, 0x37, 0x12, 0x40])
        {
            return Err("Invalid normalized N64 ROM".into());
        }
        if !std::is_x86_feature_detected!("sse4.1") || !std::is_x86_feature_detected!("ssse3") {
            return Err("N64 currently requires an x86-64 CPU with SSE4.1 and SSSE3".into());
        }
        let mut d = Box::new(device::Device::new());
        d.ui.output_hz = output_hz.clamp(8000, 384000);
        device::cart::rom::init(&mut d, rom);
        d.ui.storage.save_type = storage::get_save_type(&d.cart.rom, &d.ui.game_id);
        device::rdram::init(&mut d);
        // SAFETY: aligned RDRAM allocation never moves/resizes while this renderer
        // lives. Ui is the first Device field and destroys it before RDRAM drops.
        d.ui.renderer =
            unsafe { bridge::ffi::create_renderer(&mut d.rdram.mem) }.map_err(|e| e.to_string())?;
        device::mi::init(&mut d);
        device::pif::init(&mut d);
        device::memory::init(&mut d);
        device::cache::init(&mut d);
        device::rsp_interface::init(&mut d);
        device::rdp::init(&mut d);
        device::vi::init(&mut d);
        device::cpu::init(&mut d);
        Ok(Self {
            device: d,
            accessories: [Accessory::Auto; 4],
            frame: bridge::ffi::VideoFrame {
                width: 320,
                height: 240,
                pixels: vec![0xff000000; 320 * 240],
            },
        })
    }
    pub fn tick(&mut self, render: bool) -> Result<(), String> {
        guest_boundary(|| self.tick_inner(render))
    }
    fn tick_inner(&mut self, render: bool) -> Result<(), String> {
        let d = &mut self.device;
        d.ui.samples.clear();
        let count = d.cpu.cop0.regs[device::cop0::COP0_COUNT_REG as usize];
        // Boot may not have programmed VI yet. A bounded CPU slice prevents a
        // malformed guest from blocking the frontend forever before first VI.
        let budget = if d.vi.delay == 0 {
            1_562_500
        } else {
            d.vi.delay.saturating_mul(2).clamp(1, 10_000_000)
        };
        device::cpu::run_until(d, count.saturating_add(budget), d.vi.vi_counter + 1);
        if let Some(error) = d.ui.error.take() {
            return Err(error);
        }
        let frame =
            d.ui.renderer
                .pin_mut()
                .scanout(render)
                .map_err(|e| e.to_string())?;
        if frame.width != 0 && frame.height != 0 {
            self.frame = frame;
        }
        Ok(())
    }
    pub fn pixels(&self) -> &[u32] {
        &self.frame.pixels
    }
    pub fn rom_len(&self) -> u64 {
        self.device.cart.rom.len() as u64
    }
    pub fn reset(&mut self) -> Result<(), String> {
        let mut next = Self::new(self.device.cart.rom.clone(), self.device.ui.output_hz)?;
        next.accessories = self.accessories;
        next.device.ui.controllers = self.device.ui.controllers;
        device::pif::connect_pif_channels(&mut next.device);
        next.apply_accessories();
        std::mem::swap(
            &mut next.device.ui.storage.saves,
            &mut self.device.ui.storage.saves,
        );
        *self = next;
        Ok(())
    }
    pub fn dimensions(&self) -> (u32, u32) {
        (self.frame.width, self.frame.height)
    }
    pub fn audio(&self) -> &[i16] {
        &self.device.ui.samples
    }
    pub fn set_audio_rate(&mut self, hz: u32) {
        self.device.ui.output_hz = hz.clamp(8000, 384000);
    }
    pub fn refresh_hz(&self) -> f64 {
        if (1.0 / 120.0..=1.0 / 30.0).contains(&self.device.vi.frame_time) {
            1.0 / self.device.vi.frame_time
        } else if self.device.cart.pal {
            50.0
        } else {
            60.0
        }
    }
    pub fn cycles(&self) -> u64 {
        self.device.cpu.cop0.regs[device::cop0::COP0_COUNT_REG as usize]
    }
    pub fn vi_count(&self) -> u64 {
        self.device.vi.vi_counter
    }
    pub fn pc(&self) -> u64 {
        self.device.cpu.pc
    }
    pub fn set_input(&mut self, channel: usize, buttons: u16, x: i8, y: i8, connected: bool) {
        if channel >= 4 {
            return;
        }
        let changed = self.device.ui.controllers[channel] != connected;
        self.device.ui.controllers[channel] = connected;
        self.device.ui.input[channel] =
            if connected {
                u32::from_ne_bytes([(buttons >> 8) as u8, (buttons & 0xff) as u8,
                    x.clamp(-80, 80) as u8, y.clamp(-80, 80) as u8])
            } else { 0 };
        if changed {
            device::pif::connect_pif_channels(&mut self.device);
            self.apply_accessories();
        }
    }

    pub fn set_accessory(&mut self, channel: usize, accessory: Accessory) {
        if let Some(current) = self.accessories.get_mut(channel) {
            *current = accessory;
            self.apply_accessories();
        }
    }

    pub fn rumble(&self, channel: usize) -> bool {
        channel < 4 && self.device.ui.controllers[channel]
            && self.device.ui.rumble[channel].get() != 0
    }

    fn apply_accessories(&mut self) {
        use device::controller::{PakHandler, PakType};
        for (index, preference) in self.accessories.iter().enumerate() {
            let kind = if !self.device.ui.controllers[index] { PakType::None } else {
                match preference {
                    Accessory::Auto => device::pif::get_default_handler(&self.device).pak_type,
                    Accessory::None => PakType::None,
                    Accessory::ControllerPak => PakType::MemPak,
                    Accessory::RumblePak => PakType::RumblePak,
                }
            };
            let channel = &mut self.device.pif.channels[index];
            if channel.pak_handler.map(|p| p.pak_type) != (kind != PakType::None).then_some(kind) {
                channel.pak_handler = PakHandler::for_type(kind);
                channel.change_pak = PakType::None;
                self.device.ui.rumble[index].set(0);
            }
        }
    }
}

#[cfg(test)]
mod controller_tests {
    use super::*;
    use device::controller::PakType;

    #[test]
    #[ignore = "Requires N64_TEST_ROM and Vulkan; only reads the local ROM"]
    fn controller_preferences_survive_restore_and_reset() {
        let path = std::env::var("N64_TEST_ROM").expect("N64_TEST_ROM");
        let mut engine = Engine::new(std::fs::read(path).unwrap(), 48000).unwrap();
        let snapshot = engine.save_state().unwrap();
        for port in 0..4 {
            engine.set_input(port, 0, 0, 0, true);
            engine.set_accessory(port, Accessory::RumblePak);
        }
        engine.load_state(&snapshot).unwrap();
        // The snapshot restores guest connections; the chosen host accessory
        // must still be applied when the frontend reconnects physical devices.
        for port in 0..4 {
            engine.set_input(port, 0, 0, 0, true);
            assert!(engine.device.pif.channels[port].pak_handler.unwrap().pak_type == PakType::RumblePak);
        }
        let battery = engine.battery_data().unwrap();
        engine.reset().unwrap();
        assert_eq!(engine.battery_data().unwrap(), battery);
        for port in 0..4 {
            assert!(engine.device.ui.controllers[port]);
            assert!(engine.device.pif.channels[port].pak_handler.unwrap().pak_type == PakType::RumblePak);
            assert!(!engine.rumble(port));
        }
    }

    #[test]
    fn late_connections_accessory_changes_and_disconnects_are_consistent() {
        // Controller routing does not require a GPU; use the real PIF and host
        // adapter with an empty scanout to keep this regression portable in CI.
        let mut engine = Engine {
            device: Box::new(device::Device::new()),
            frame: bridge::ffi::VideoFrame { width: 0, height: 0, pixels: vec![] },
            accessories: [Accessory::Auto; 4],
        };
        device::pif::init(&mut engine.device);
        for port in 0..4 {
            engine.set_input(port, 0x8000, 127, -128, true);
            assert!(engine.device.pif.channels[port].pak_handler.unwrap().pak_type == PakType::MemPak);
            assert_eq!(engine.device.ui.input[port].to_ne_bytes(), [0x80, 0, 80, (-80i8) as u8]);
            engine.set_accessory(port, Accessory::RumblePak);
            assert!(engine.device.pif.channels[port].pak_handler.unwrap().pak_type == PakType::RumblePak);
            engine.device.ui.rumble[port].set(1);
            assert!(engine.rumble(port));
            engine.set_accessory(port, Accessory::None);
            assert!(engine.device.pif.channels[port].pak_handler.is_none());
            assert!(!engine.rumble(port));
            engine.set_accessory(port, Accessory::ControllerPak);
            engine.set_input(port, 0xffff, 80, 80, false);
            assert!(engine.device.pif.channels[port].pak_handler.is_none());
            assert_eq!(engine.device.ui.input[port], 0);
            engine.set_input(port, 0, 0, 0, true);
            assert!(engine.device.pif.channels[port].pak_handler.unwrap().pak_type == PakType::MemPak);
        }
        engine.device.ui.game_id = "NCT".into();
        engine.set_accessory(0, Accessory::Auto);
        assert!(engine.device.pif.channels[0].pak_handler.unwrap().pak_type == PakType::RumblePak);
        engine.set_input(4, 0xffff, 80, 80, true);
        engine.set_accessory(4, Accessory::None);
        assert!(!engine.rumble(4));
    }
}
