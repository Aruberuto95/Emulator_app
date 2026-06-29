mod emulator;

use std::pin::Pin;

#[cxx::bridge(namespace = "ffi")]
pub mod ffi {
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
    }

    extern "Rust" {
        type Emulator;

        fn create_emulator() -> Box<Emulator>;
        fn play(emu: Pin<&mut Emulator>);
        fn pause(emu: Pin<&mut Emulator>);
        fn reset(emu: Pin<&mut Emulator>);
        fn tick(emu: Pin<&mut Emulator>);
        fn inject_input(emu: Pin<&mut Emulator>, buttons: ButtonState);
        fn get_video_buffer(emu: &Emulator) -> &[u8];
        fn get_audio_buffer(emu: &Emulator) -> &[i16];

        // Custom Getters
        fn get_ticks(emu: &Emulator) -> u32;
        fn is_playing(emu: &Emulator) -> bool;
        fn get_state_string(emu: &Emulator) -> String;
        fn get_player_x(emu: &Emulator) -> u8;
        fn get_player_y(emu: &Emulator) -> u8;
        fn get_button_state(emu: &Emulator) -> ButtonState;
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

fn inject_input(emu: Pin<&mut Emulator>, buttons: ffi::ButtonState) {
    emu.get_mut().inject_input(buttons);
}

fn get_video_buffer(emu: &Emulator) -> &[u8] {
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
