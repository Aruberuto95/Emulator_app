use crate::ffi::ButtonState;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum EmulatorState {
    Splash,
    Gameplay,
}

/// The main Emulator engine state.
///
/// Holds the pre-allocated video and audio buffers to guarantee zero heap
/// allocations during active execution steps.
pub struct Emulator {
    video_buffer: Vec<u8>,
    audio_buffer: Vec<i16>,
    is_playing: bool,
    buttons: ButtonState,
    ticks: u32,
    state: EmulatorState,
    player_x: u8,
    player_y: u8,
}

impl Emulator {
    /// Creates and initializes the Emulator state with pre-allocated buffers.
    pub fn new() -> Self {
        // Video: 256x240 Resolution with RGB format (3 bytes/pixel) -> 184,320 bytes
        let video_buffer = vec![0u8; 256 * 240 * 3];
        // Audio: Stereo 44.1kHz buffer for a single frame (~735 samples * 2 channels = 1470 samples)
        let audio_buffer = vec![0i16; 1470];

        Self {
            video_buffer,
            audio_buffer,
            is_playing: false,
            buttons: ButtonState {
                up: false,
                down: false,
                left: false,
                right: false,
                a: false,
                b: false,
                start: false,
                select: false,
            },
            ticks: 0,
            state: EmulatorState::Splash,
            player_x: 128,
            player_y: 120,
        }
    }

    /// Resumes emulator execution.
    pub fn play(&mut self) {
        self.is_playing = true;
    }

    /// Pauses emulator execution.
    pub fn pause(&mut self) {
        self.is_playing = false;
    }

    /// Resets emulator state and zeroes the buffers, but preserves playback state.
    pub fn reset(&mut self) {
        self.state = EmulatorState::Splash;
        self.ticks = 0;
        self.player_x = 128;
        self.player_y = 120;
        self.buttons = ButtonState {
            up: false,
            down: false,
            left: false,
            right: false,
            a: false,
            b: false,
            start: false,
            select: false,
        };
        self.video_buffer.fill(0);
        self.audio_buffer.fill(0);
    }

    /// Executes a single emulation frame/tick.
    ///
    /// Ensures no heap allocations occur in active tick cycles to preserve low latency.
    pub fn tick(&mut self) {
        if !self.is_playing {
            self.audio_buffer.fill(0);
            return;
        }

        self.ticks += 1;

        // Handle splash state transitions and rendering
        if self.state == EmulatorState::Splash {
            if self.buttons.start {
                self.state = EmulatorState::Gameplay;
                self.player_x = 128;
                self.player_y = 120;

                // Render first gameplay video frame immediately
                let bg_r = ((self.ticks * 2) % 256) as u8;
                let bg_g = ((self.ticks * 3) % 256) as u8;
                let bg_b = ((self.ticks * 5) % 256) as u8;
                for i in (0..self.video_buffer.len()).step_by(3) {
                    self.video_buffer[i] = bg_r;
                    self.video_buffer[i + 1] = bg_g;
                    self.video_buffer[i + 2] = bg_b;
                }
                let offset = ((self.player_y as usize * 256) + self.player_x as usize) * 3;
                if offset + 2 < self.video_buffer.len() {
                    self.video_buffer[offset] = 255;
                    self.video_buffer[offset + 1] = 0;
                    self.video_buffer[offset + 2] = 0;
                }
            } else {
                // Render splash screen
                let animated_blue = (255 - (self.ticks % 256)) as u8;
                for i in (0..self.video_buffer.len()).step_by(3) {
                    self.video_buffer[i] = 0;
                    self.video_buffer[i + 1] = 0;
                    self.video_buffer[i + 2] = 255;
                }
                if self.video_buffer.len() >= 3 {
                    self.video_buffer[2] = animated_blue;
                }
            }
            self.audio_buffer.fill(0);
            return;
        }

        // Gameplay state logic
        let mut move_x = 0;
        let mut move_y = 0;

        // Resolve LEFT + RIGHT SOCD neutralization
        if self.buttons.left && self.buttons.right {
            move_x = 0;
        } else if self.buttons.left {
            move_x = -1;
        } else if self.buttons.right {
            move_x = 1;
        }

        // Resolve UP + DOWN SOCD neutralization
        if self.buttons.up && self.buttons.down {
            move_y = 0;
        } else if self.buttons.up {
            move_y = -1;
        } else if self.buttons.down {
            move_y = 1;
        }

        // Action button A (Jump) forces y-axis move upwards (decreases y) by 1 pixel/tick
        let is_jumping = self.buttons.a;
        if is_jumping {
            move_y = -1;
        }

        self.player_x = (self.player_x as i32 + move_x).clamp(0, 255) as u8;
        self.player_y = (self.player_y as i32 + move_y).clamp(0, 239) as u8;

        // Render gameplay background
        let bg_r = ((self.ticks * 2) % 256) as u8;
        let bg_g = ((self.ticks * 3) % 256) as u8;
        let bg_b = ((self.ticks * 5) % 256) as u8;
        for i in (0..self.video_buffer.len()).step_by(3) {
            self.video_buffer[i] = bg_r;
            self.video_buffer[i + 1] = bg_g;
            self.video_buffer[i + 2] = bg_b;
        }

        // Draw red player pixel
        let offset = ((self.player_y as usize * 256) + self.player_x as usize) * 3;
        if offset + 2 < self.video_buffer.len() {
            self.video_buffer[offset] = 255;
            self.video_buffer[offset + 1] = 0;
            self.video_buffer[offset + 2] = 0;
        }

        // Audio generation: 440Hz sine wave if jumping
        if is_jumping {
            let frequency = 440.0;
            let amplitude = 10000.0;
            let sample_rate = 44100.0;
            for i in 0..735 {
                let t = (self.ticks as f64 * 735.0 + i as f64) / sample_rate;
                let val = (amplitude * (2.0 * std::f64::consts::PI * frequency * t).sin()) as i32;
                let clamped = val.clamp(-32768, 32767) as i16;
                self.audio_buffer[i * 2] = clamped;
                self.audio_buffer[i * 2 + 1] = clamped;
            }
        } else {
            self.audio_buffer.fill(0);
        }
    }

    /// Injects controller button inputs.
    pub fn inject_input(&mut self, buttons: ButtonState) {
        self.buttons = buttons;
    }

    /// Returns a zero-copy slice to the video frame buffer.
    pub fn get_video_buffer(&self) -> &[u8] {
        &self.video_buffer
    }

    /// Returns a zero-copy slice to the audio sample buffer.
    pub fn get_audio_buffer(&self) -> &[i16] {
        &self.audio_buffer
    }

    /// Returns the current number of ticks executed.
    pub fn get_ticks(&self) -> u32 {
        self.ticks
    }

    /// Returns whether the emulator is currently running (playing).
    pub fn is_playing(&self) -> bool {
        self.is_playing
    }

    /// Returns the string representation of the current state.
    pub fn get_state_string(&self) -> String {
        match self.state {
            EmulatorState::Splash => "splash".to_string(),
            EmulatorState::Gameplay => "gameplay".to_string(),
        }
    }

    /// Returns player X coordinate.
    pub fn get_player_x(&self) -> u8 {
        self.player_x
    }

    /// Returns player Y coordinate.
    pub fn get_player_y(&self) -> u8 {
        self.player_y
    }

    /// Returns current button state register.
    pub fn get_button_state(&self) -> ButtonState {
        self.buttons
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emulator_initialization() {
        let emu = Emulator::new();
        assert!(!emu.is_playing);
        assert_eq!(emu.get_video_buffer().len(), 256 * 240 * 3);
        assert_eq!(emu.get_audio_buffer().len(), 1470);
        assert_eq!(emu.get_ticks(), 0);
        assert_eq!(emu.get_state_string(), "splash");
    }

    #[test]
    fn test_emulator_play_pause() {
        let mut emu = Emulator::new();
        assert!(!emu.is_playing);
        emu.play();
        assert!(emu.is_playing);
        emu.pause();
        assert!(!emu.is_playing);
    }

    #[test]
    fn test_emulator_reset() {
        let mut emu = Emulator::new();
        emu.play();
        emu.tick();
        assert_eq!(emu.get_ticks(), 1);
        emu.reset();
        assert_eq!(emu.get_ticks(), 0);
        assert_eq!(emu.get_state_string(), "splash");
        assert_eq!(emu.get_video_buffer()[2], 0);
    }

    #[test]
    fn test_emulator_tick_and_input() {
        let mut emu = Emulator::new();
        emu.play();
        
        // Tick with 'A' button not pressed -> remains in splash
        emu.tick();
        assert_eq!(emu.get_video_buffer()[2], 254);
        assert_eq!(emu.get_state_string(), "splash");

        // Inject 'Start' button to transition to gameplay
        emu.inject_input(ButtonState {
            up: false,
            down: false,
            left: false,
            right: false,
            a: false,
            b: false,
            start: true,
            select: false,
        });
        emu.tick();
        assert_eq!(emu.get_state_string(), "gameplay");
    }
}

