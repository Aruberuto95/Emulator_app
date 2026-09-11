//! Keep the application's other consoles available on non-x86 hosts. N64 fails
//! at construction, before a cartridge replaces the currently running machine.
const UNAVAILABLE: &str = "N64 currently requires an x86-64 CPU with SSSE3/SSE4.1 and Vulkan";
pub struct Engine {
    _private: (),
}
impl Engine {
    pub fn rom_len(&self) -> u64 {
        0
    }
    pub fn new(_: Vec<u8>, _: u32) -> Result<Self, String> {
        Err(UNAVAILABLE.into())
    }
    pub fn tick(&mut self, _: bool) -> Result<(), String> {
        Err(UNAVAILABLE.into())
    }
    pub fn reset(&mut self) -> Result<(), String> {
        Err(UNAVAILABLE.into())
    }
    pub fn save_state(&mut self) -> Result<Vec<u8>, String> {
        Err(UNAVAILABLE.into())
    }
    pub fn load_state(&mut self, _: &[u8]) -> Result<(), String> {
        Err(UNAVAILABLE.into())
    }
    pub fn battery_data(&self) -> Result<Vec<u8>, String> {
        Err(UNAVAILABLE.into())
    }
    pub fn load_battery(&mut self, _: &[u8]) -> Result<(), String> {
        Err(UNAVAILABLE.into())
    }
    pub fn battery_dirty(&self) -> bool {
        false
    }
    pub fn battery_clean(&mut self) {}
    pub fn pixels(&self) -> &[u32] {
        &[]
    }
    pub fn audio(&self) -> &[i16] {
        &[]
    }
    pub fn dimensions(&self) -> (u32, u32) {
        (320, 240)
    }
    pub fn refresh_hz(&self) -> f64 {
        60.0
    }
    pub fn cycles(&self) -> u64 {
        0
    }
    pub fn vi_count(&self) -> u64 {
        0
    }
    pub fn pc(&self) -> u64 {
        0
    }
    pub fn set_audio_rate(&mut self, _: u32) {}
    pub fn set_input(&mut self, _: usize, _: u16, _: i8, _: i8, _: bool) {}
    pub fn set_accessory(&mut self, _: usize, _: crate::Accessory) {}
    pub fn rumble(&self, _: usize) -> bool { false }
}
