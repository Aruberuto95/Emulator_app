mod emulator;
mod gba;
mod gbc;
mod n64;
mod psg;
pub mod resampler;
mod rom;
pub mod savestate;

use std::pin::Pin;

#[cxx::bridge(namespace = "ffi")]
pub mod ffi {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ConsoleType {
        Gbc,
        Gba,
        Nintendo64,
    }

    #[derive(Clone, Copy)]
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
        stick_x: i32,
        stick_y: i32,
        z: bool,
        c_up: bool,
        c_down: bool,
        c_left: bool,
        c_right: bool,
    }

    extern "Rust" {
        type Emulator;

        fn create_emulator() -> Box<Emulator>;
        fn play(emu: Pin<&mut Emulator>);
        fn pause(emu: Pin<&mut Emulator>);
        fn reset(emu: Pin<&mut Emulator>);
        fn tick(emu: Pin<&mut Emulator>);
        fn flush_battery(emu: Pin<&mut Emulator>);
        fn inject_input(emu: Pin<&mut Emulator>, buttons: ButtonState);
        fn get_video_buffer(emu: &Emulator) -> &[u16];
        fn get_audio_buffer(emu: &Emulator) -> &[i16];

        // Custom Getters
        fn get_ticks(emu: &Emulator) -> u32;
        fn is_playing(emu: &Emulator) -> bool;
        fn get_state_string(emu: &Emulator) -> String;
        fn get_player_x(emu: &Emulator) -> u16;
        fn get_player_y(emu: &Emulator) -> u16;
        fn get_button_state(emu: &Emulator) -> ButtonState;

        // Milestone 2 expansions
        fn get_console_type(emu: &Emulator) -> ConsoleType;
        fn get_width(emu: &Emulator) -> u32;
        fn get_height(emu: &Emulator) -> u32;
        fn get_speed(emu: &Emulator) -> f32;
        fn get_frame_skip(emu: &Emulator) -> u32;
        fn get_cpu_cycles(emu: &Emulator) -> u64;
        fn get_rendered_frames(emu: &Emulator) -> u32;

        // Milestone 2 mutators
        fn set_speed(emu: Pin<&mut Emulator>, speed: f32);
        fn set_frame_skip(emu: Pin<&mut Emulator>, frame_skip: u32);
        fn load_rom(emu: Pin<&mut Emulator>, rom_data: &[u8]) -> bool;
        fn load_rom_path(emu: Pin<&mut Emulator>, rom_path: &str, base_dir: &str) -> String;
        fn save_state(emu: &Emulator, slot: &str, base_dir: &str) -> String;
        fn load_state(emu: Pin<&mut Emulator>, slot: &str, base_dir: &str) -> String;
        fn scan_roms(dir_path: &str, base_dir: &str) -> String;
        
        // Milestone 4 expansions
        fn get_expansion_pak(emu: &Emulator) -> bool;
        fn set_expansion_pak(emu: Pin<&mut Emulator>, enabled: bool);
    }
}

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

fn flush_battery(emu: Pin<&mut Emulator>) {
    emu.get_mut().flush_battery();
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

fn get_player_x(emu: &Emulator) -> u16 {
    emu.get_player_x()
}

fn get_player_y(emu: &Emulator) -> u16 {
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

fn set_frame_skip(emu: Pin<&mut Emulator>, frame_skip: u32) {
    emu.get_mut().set_frame_skip(frame_skip);
}

fn load_rom(emu: Pin<&mut Emulator>, rom_data: &[u8]) -> bool {
    emu.get_mut().load_rom(rom_data)
}

fn load_rom_path(emu: Pin<&mut Emulator>, rom_path: &str, base_dir: &str) -> String {
    emu.get_mut().load_rom_path(rom_path, base_dir)
}

fn save_state(emu: &Emulator, slot: &str, base_dir: &str) -> String {
    emu.save_state(slot, base_dir)
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

fn get_expansion_pak(emu: &Emulator) -> bool {
    emu.get_expansion_pak()
}

fn set_expansion_pak(emu: Pin<&mut Emulator>, enabled: bool) {
    emu.get_mut().set_expansion_pak(enabled);
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
        mmu.io[0x06] = 120;
        mmu.io[0x07] = 0;

        mmu.write_byte(0x04000006, 99);
        assert_eq!(mmu.io[0x06], 120);

        mmu.write_halfword(0x04000006, 0x1234);
        assert_eq!(mmu.io[0x06], 120);
        assert_eq!(mmu.io[0x07], 0);

        mmu.write_word(0x04000004, 0x99999999);
        assert_eq!(mmu.io[0x06], 120);
        assert_eq!(mmu.io[0x07], 0);
    }

    #[test]
    fn test_emulator_speed_validation() {
        let mut emu = Emulator::new();

        emu.set_speed(1.5);
        assert_eq!(emu.get_speed(), 1.5);

        emu.set_speed(0.0);
        assert_eq!(emu.get_speed(), 1.5);

        emu.set_speed(-0.5);
        assert_eq!(emu.get_speed(), 1.5);

        emu.set_speed(std::f32::NAN);
        assert_eq!(emu.get_speed(), 1.5);

        emu.set_speed(std::f32::INFINITY);
        assert_eq!(emu.get_speed(), 1.5);
    }
}
