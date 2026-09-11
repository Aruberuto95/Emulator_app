mod cpu_bus;
// Public for the same reason as `nds` below: `core/tests/*` drive the emulator
// from outside the crate, and a private module silently drops those targets from
// `cargo test`.
pub mod emulator;
mod gba;
mod gbc;
mod n64;
// The ARM9 block recompiler. Private: nothing outside the core drives it, and
// `jit::exec_mem` is the crate's only `unsafe` — keeping it unexported means a
// consumer cannot obtain an executable page through this crate's API.
mod jit;
// Public so `core/tests/*` (out-of-crate integration tests) can drive the NDS
// MMU/CPU/HLE directly. Without this the whole `cargo test` invocation fails to
// compile, which silently reduced the suite to `cargo test --lib`.
pub mod nds;
mod psg;
pub mod resampler;
mod rom;
pub mod savestate;
pub mod snapshot;

use std::pin::Pin;

#[cxx::bridge(namespace = "ffi")]
pub mod ffi {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ConsoleType {
        Gbc,
        Gba,
        Nds,
        N64,
    }

    #[derive(Clone, Copy, Default)]
    struct ButtonState {
        up: bool,
        down: bool,
        left: bool,
        right: bool,
        a: bool,
        b: bool,
        start: bool,
        select: bool,
        l: bool,
        r: bool,
        x: bool,
        y: bool,
        nds_touch_x: u16,
        nds_touch_y: u16,
        nds_touch_pressed: bool,
    }

    #[derive(Clone, Copy, Default)]
    struct N64Input {
        buttons: u16,
        stick_x: i8,
        stick_y: i8,
        connected: bool,
    }

    /// N64 wire bits shared by physical input and the legacy/scripted adapter.
    #[repr(u16)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum N64Button {
        A = 0x8000, B = 0x4000, Z = 0x2000, Start = 0x1000,
        DpadUp = 0x0800, DpadDown = 0x0400, DpadLeft = 0x0200, DpadRight = 0x0100,
        L = 0x0020, R = 0x0010, CUp = 0x0008, CDown = 0x0004, CLeft = 0x0002, CRight = 0x0001,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum N64Accessory { Auto, None, ControllerPak, RumblePak }

    struct RomEntry { path: String, console_type: ConsoleType }

    extern "Rust" {
        type Emulator;

        fn create_emulator() -> Box<Emulator>;
        fn play(emu: Pin<&mut Emulator>);
        fn pause(emu: Pin<&mut Emulator>);
        fn reset(emu: Pin<&mut Emulator>);
        fn tick(emu: Pin<&mut Emulator>);
        fn tick_with_video(emu: Pin<&mut Emulator>, render_video: bool);
        fn flush_battery(emu: Pin<&mut Emulator>) -> String;
        fn battery_error(emu: &Emulator) -> &str;
        fn state_path(emu: &Emulator, slot: &str, base_dir: &str) -> String;
        fn inject_input(emu: Pin<&mut Emulator>, buttons: ButtonState);
        fn get_video_buffer(emu: &Emulator) -> &[u16];
        fn get_video_rgba(emu: &Emulator) -> &[u32];
        fn console_refresh_hz(emu: &Emulator) -> f64;
        fn runtime_error(emu: &Emulator) -> &str;
        fn inject_n64_input(emu: Pin<&mut Emulator>, channel: u8, input: N64Input);
        fn n64_input_from_buttons(buttons: ButtonState) -> N64Input;
        fn set_n64_accessory(emu: Pin<&mut Emulator>, channel: u8, accessory: N64Accessory) -> bool;
        fn n64_rumble(emu: &Emulator, channel: u8) -> bool;
        fn rom_console(path: &str) -> Result<ConsoleType>;
        fn get_audio_buffer(emu: &Emulator) -> &[i16];

        // Custom Getters
        fn get_ticks(emu: &Emulator) -> u32;
        fn is_playing(emu: &Emulator) -> bool;
        fn get_state_string(emu: &Emulator) -> String;
        fn get_player_x(emu: &Emulator) -> u8;
        fn get_player_y(emu: &Emulator) -> u8;
        fn get_button_state(emu: &Emulator) -> ButtonState;

        // Milestone 2 expansions
        fn get_console_type(emu: &Emulator) -> ConsoleType;
        fn get_width(emu: &Emulator) -> u32;
        fn get_height(emu: &Emulator) -> u32;
        fn get_speed(emu: &Emulator) -> f32;
        fn get_frame_skip(emu: &Emulator) -> u32;
        fn get_cpu_cycles(emu: &Emulator) -> u64;
        fn get_rendered_frames(emu: &Emulator) -> u32;

        fn set_speed(emu: Pin<&mut Emulator>, speed: f32);
        fn min_speed() -> f32;
        fn max_speed() -> f32;
        fn console_max_speed(emu: &Emulator) -> f32;
        fn set_audio_sample_rate(emu: Pin<&mut Emulator>, hz: u32);
        fn set_frame_skip(emu: Pin<&mut Emulator>, frame_skip: u32);
        fn load_rom(emu: Pin<&mut Emulator>, rom_data: &[u8]) -> bool;
        fn load_rom_path(emu: Pin<&mut Emulator>, rom_path: &str, base_dir: &str) -> String;
        fn save_state(emu: Pin<&mut Emulator>, slot: &str, base_dir: &str) -> String;
        fn load_state(emu: Pin<&mut Emulator>, slot: &str, base_dir: &str) -> String;
        fn scan_roms(dir_path: &str, base_dir: &str) -> String;
        fn scan_rom_entries(dir_path: &str, base_dir: &str) -> Result<Vec<RomEntry>>;
        fn is_rom_file(path: &str) -> bool;
    }
}

fn get_video_rgba(emu: &Emulator) -> &[u32] { emu.n64.as_ref().map_or(&[], |engine| engine.pixels()) }
fn console_refresh_hz(emu: &Emulator) -> f64 {
    if let Some(engine) = &emu.n64 { engine.refresh_hz() }
    else if emu.console_type == ffi::ConsoleType::Nds { 59.8261 } else { 59.7275 }
}
fn runtime_error(emu: &Emulator) -> &str { &emu.runtime_error }
fn inject_n64_input(emu: Pin<&mut Emulator>, channel:u8, input:ffi::N64Input) {
    if let Some(engine) = &mut emu.get_mut().n64 { engine.set_input(channel as usize,input.buttons,input.stick_x,input.stick_y,input.connected); }
}
fn n64_input_from_buttons(buttons:ffi::ButtonState) -> ffi::N64Input { crate::n64::input_from_buttons(buttons) }
fn console_max_speed(emu: &Emulator) -> f32 { emu.max_speed() }
fn set_n64_accessory(emu: Pin<&mut Emulator>, channel: u8, accessory: ffi::N64Accessory) -> bool {
    let accessory = match accessory {
        ffi::N64Accessory::Auto => n64_engine::Accessory::Auto,
        ffi::N64Accessory::None => n64_engine::Accessory::None,
        ffi::N64Accessory::ControllerPak => n64_engine::Accessory::ControllerPak,
        ffi::N64Accessory::RumblePak => n64_engine::Accessory::RumblePak,
        _ => return false,
    };
    if channel >= 4 { return false; }
    if let Some(engine) = &mut emu.get_mut().n64 {
        engine.set_accessory(usize::from(channel), accessory);
        true
    } else { false }
}
fn n64_rumble(emu: &Emulator, channel: u8) -> bool {
    emu.n64.as_ref().is_some_and(|engine| engine.rumble(usize::from(channel)))
}
fn rom_console(path:&str) -> Result<ffi::ConsoleType,String> {
    crate::rom::extension_console(std::path::Path::new(path)).ok_or_else(||"Unsupported ROM extension".into())
}
fn scan_rom_entries(dir_path:&str, base_dir:&str) -> Result<Vec<ffi::RomEntry>,String> {
    rom::scan_entries(std::path::Path::new(dir_path),std::path::Path::new(base_dir)).map_err(str::to_string)
}
fn is_rom_file(path:&str) -> bool { rom::extension_console(std::path::Path::new(path)).is_some() }

// Re-export the Emulator struct so the bridge can find it in the current scope
pub use emulator::Emulator;

fn create_emulator() -> Box<Emulator> {
    Box::new(Emulator::new())
}

fn play(emu: Pin<&mut Emulator>) {
    emu.get_mut().play();
}

fn pause(emu: Pin<&mut Emulator>) {
    emu.get_mut().pause();
}

fn reset(emu: Pin<&mut Emulator>) {
    emu.get_mut().reset();
}

fn tick(emu: Pin<&mut Emulator>) {
    emu.get_mut().tick();
}

fn tick_with_video(emu: Pin<&mut Emulator>, render_video: bool) {
    emu.get_mut().tick_with_video(render_video);
}

fn flush_battery(emu: Pin<&mut Emulator>) -> String {
    emu.get_mut().flush_battery().err().unwrap_or_default()
}

fn battery_error(emu: &Emulator) -> &str {
    &emu.battery_error
}

fn state_path(emu: &Emulator, slot: &str, base_dir: &str) -> String {
    emu.state_path(slot, base_dir).to_string_lossy().into_owned()
}

fn inject_input(emu: Pin<&mut Emulator>, buttons: ffi::ButtonState) {
    emu.get_mut().inject_input(buttons);
}

fn get_video_buffer(emu: &Emulator) -> &[u16] {
    emu.get_video_buffer()
}

fn get_audio_buffer(emu: &Emulator) -> &[i16] {
    emu.get_audio_buffer()
}

fn get_ticks(emu: &Emulator) -> u32 {
    emu.get_ticks()
}

fn is_playing(emu: &Emulator) -> bool {
    emu.is_playing()
}

fn get_state_string(emu: &Emulator) -> String {
    emu.get_state_string()
}

fn get_player_x(emu: &Emulator) -> u8 {
    emu.get_player_x()
}

fn get_player_y(emu: &Emulator) -> u8 {
    emu.get_player_y()
}

fn get_button_state(emu: &Emulator) -> ffi::ButtonState {
    emu.get_button_state()
}

fn get_console_type(emu: &Emulator) -> ffi::ConsoleType {
    emu.get_console_type()
}

fn get_width(emu: &Emulator) -> u32 {
    emu.get_width()
}

fn get_height(emu: &Emulator) -> u32 {
    emu.get_height()
}

fn get_speed(emu: &Emulator) -> f32 {
    emu.get_speed()
}

fn get_frame_skip(emu: &Emulator) -> u32 {
    emu.get_frame_skip()
}

fn get_cpu_cycles(emu: &Emulator) -> u64 {
    emu.get_cpu_cycles()
}

fn get_rendered_frames(emu: &Emulator) -> u32 {
    emu.get_rendered_frames()
}

fn set_speed(emu: Pin<&mut Emulator>, speed: f32) {
    emu.get_mut().set_speed(speed);
}

/// Global lower bound for speed parsing before a cartridge is loaded.
///
/// Callers also check `console_max_speed` after loading because N64 has a
/// stricter ceiling. Sharing both limits avoids reporting success for a value
/// that `set_speed` silently rejects.
fn min_speed() -> f32 {
    Emulator::MIN_SPEED
}

/// Global upper bound for parsing; `console_max_speed` is the active ceiling.
fn max_speed() -> f32 {
    Emulator::MAX_SPEED
}

/// Retarget the core's audio output to the host device's real sample rate.
/// Call once after the device is opened; leaving the core at its default
/// while the device runs at another rate hands SDL a hidden resampler.
fn set_audio_sample_rate(emu: Pin<&mut Emulator>, hz: u32) {
    emu.get_mut().set_audio_sample_rate(hz);
}

fn set_frame_skip(emu: Pin<&mut Emulator>, frame_skip: u32) {
    emu.get_mut().set_frame_skip(frame_skip);
}

fn load_rom(emu: Pin<&mut Emulator>, rom_data: &[u8]) -> bool {
    emu.get_mut().load_rom(rom_data)
}

fn load_rom_path(emu: Pin<&mut Emulator>, rom_path: &str, base_dir: &str) -> String {
    emu.get_mut().load_rom_path(rom_path, base_dir)
}

/// Takes `&mut` because a binary snapshot walks the machine with the same
/// visitor in both directions (see `crate::snapshot`); saving mutates nothing.
fn save_state(emu: Pin<&mut Emulator>, slot: &str, base_dir: &str) -> String {
    emu.get_mut().save_state(slot, base_dir)
}

fn load_state(emu: Pin<&mut Emulator>, slot: &str, base_dir: &str) -> String {
    emu.get_mut().load_state(slot, base_dir)
}

fn scan_roms(dir_path: &str, base_dir: &str) -> String {
    let path = std::path::Path::new(dir_path);
    let base = std::path::Path::new(base_dir);
    match rom::scan_roms_in_directory(path, base) {
        Ok(json_list) => format!("SCAN_ROMS_OK {}", json_list),
        Err(e) => format!("SCAN_ROMS_ERROR {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gba::mmu::GbaMmu;

    #[test]
    fn test_gba_mmu_fifo_writes() {
        let mut mmu = GbaMmu::new(vec![]);
        // Write byte to FIFO A (offset 0xA0 in region 0x04)
        mmu.write_byte(0x040000A0, 42);
        assert_eq!(mmu.apu.fifo_a.pop(), Some(42));

        // Write byte to FIFO B (offset 0xA4 in region 0x04)
        mmu.write_byte(0x040000A4, 84);
        assert_eq!(mmu.apu.fifo_b.pop(), Some(84));
    }

    #[test]
    fn test_gba_mmu_ppu_unaligned_read_no_panic() {
        let mut mmu = GbaMmu::new(vec![]);
        mmu.palette_ram[1022] = 0xAA;
        mmu.palette_ram[1023] = 0x55;
        let val = mmu.read_palette_halfword(1023);
        assert_eq!(val, 0x55AA);

        mmu.oam[1022] = 0xBB;
        mmu.oam[1023] = 0x66;
        let val = mmu.read_oam_halfword(1023);
        assert_eq!(val, 0x66BB);

        let vram_len = mmu.vram.len();
        mmu.vram[vram_len - 2] = 0xCC;
        mmu.vram[vram_len - 1] = 0x77;
        let val = mmu.read_vram_halfword(98303);
        assert_eq!(val, 0x77CC);
    }

    #[test]
    fn test_gba_mmu_vcount_readonly() {
        let mut mmu = GbaMmu::new(vec![]);
        // Set VCOUNT directly to a value
        mmu.io[0x06] = 120;
        mmu.io[0x07] = 0;

        // Try writing to VCOUNT via write_byte
        mmu.write_byte(0x04000006, 99);
        assert_eq!(mmu.io[0x06], 120);

        // Try writing to VCOUNT via write_halfword
        mmu.write_halfword(0x04000006, 0x1234);
        assert_eq!(mmu.io[0x06], 120);
        assert_eq!(mmu.io[0x07], 0);

        // Try writing to VCOUNT via write_word
        mmu.write_word(0x04000004, 0x99999999);
        assert_eq!(mmu.io[0x06], 120);
        assert_eq!(mmu.io[0x07], 0);
    }

    #[test]
    fn test_emulator_speed_validation() {
        let mut emu = Emulator::new();

        // Try setting a valid speed
        emu.set_speed(1.5);
        assert_eq!(emu.get_speed(), 1.5);

        // Try setting invalid speeds
        emu.set_speed(0.0);
        assert_eq!(emu.get_speed(), 1.5);

        emu.set_speed(-0.5);
        assert_eq!(emu.get_speed(), 1.5);

        emu.set_speed(std::f32::NAN);
        assert_eq!(emu.get_speed(), 1.5);

        emu.set_speed(std::f32::INFINITY);
        assert_eq!(emu.get_speed(), 1.5);

        // Out of range in either direction. A near-zero speed is the dangerous
        // one: it drives `cycles_per_sample` toward zero, and the resampler
        // emits one sample per `cycles_per_sample` cycles, so a single slice
        // would produce millions of samples. 3.6e-6 is the smallest value that
        // still leaves the GBA a non-zero cycle budget, i.e. the worst case.
        emu.set_speed(0.000_003_6);
        assert_eq!(emu.get_speed(), 1.5);

        emu.set_speed(1000.0);
        assert_eq!(emu.get_speed(), 1.5);

        // The bounds themselves are valid, and span the frontend's 0.5x..4x.
        emu.set_speed(Emulator::MIN_SPEED);
        assert_eq!(emu.get_speed(), Emulator::MIN_SPEED);
        emu.set_speed(Emulator::MAX_SPEED);
        assert_eq!(emu.get_speed(), Emulator::MAX_SPEED);
    }
}
