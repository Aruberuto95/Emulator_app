use crate::ffi::ButtonState;
use std::path::Path;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum EmulatorState {
    Splash,
    Gameplay,
}

fn allocate_aligned_u8(size: usize, alignment: usize) -> (Vec<u8>, usize) {
    let vec = vec![0u8; size + alignment];
    let ptr = vec.as_ptr() as usize;
    let aligned_ptr = (ptr + alignment - 1) & !(alignment - 1);
    let offset = aligned_ptr - ptr;
    (vec, offset)
}

fn allocate_aligned_i16(size: usize, alignment: usize) -> (Vec<i16>, usize) {
    let element_alignment = alignment / 2;
    let vec = vec![0i16; size + element_alignment];
    let ptr = vec.as_ptr() as usize;
    let aligned_ptr = (ptr + alignment - 1) & !(alignment - 1);
    let offset = (aligned_ptr - ptr) / 2;
    (vec, offset)
}

pub struct Emulator {
    pub(crate) raw_video_buffer: Vec<u8>,
    pub(crate) raw_audio_buffer: Vec<i16>,
    pub(crate) video_offset: usize,
    pub(crate) audio_offset: usize,
    pub(crate) is_playing: bool,
    pub(crate) buttons: ButtonState,
    pub(crate) ticks: u32,
    pub(crate) state: EmulatorState,
    pub(crate) player_x: u8,
    pub(crate) player_y: u8,
    pub(crate) console_type: crate::ffi::ConsoleType,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) speed: f32,
    pub(crate) frame_skip: u32,
    pub(crate) cpu_cycles: u64,
    pub(crate) rendered_frames: u32,

    // Real GBC Emulator Components
    pub gbc_cpu: crate::gbc::cpu::Cpu,
    pub gbc_mmu: crate::gbc::mmu::Mmu,
    pub gbc_ppu: crate::gbc::ppu::Ppu,
    pub rom_path: std::path::PathBuf,
    pub base_dir: std::path::PathBuf,
    pub rom_loaded: bool,

    // Real GBA Emulator Components
    pub gba_cpu: crate::gba::cpu::GbaCpu,
    pub gba_mmu: crate::gba::mmu::GbaMmu,
    pub gba_ppu: crate::gba::ppu::GbaPpu,
    pub(crate) extra_fields: Vec<(String, String)>,
}

impl Emulator {
    pub fn new() -> Self {
        let (raw_video_buffer, video_offset) = allocate_aligned_u8(240 * 160 * 3, 16);
        let (raw_audio_buffer, audio_offset) = allocate_aligned_i16(1470 * 4, 16);

        Self {
            raw_video_buffer,
            raw_audio_buffer,
            video_offset,
            audio_offset,
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
                l: false,
                r: false,
            },
            ticks: 0,
            state: EmulatorState::Splash,
            player_x: 80,
            player_y: 72,
            console_type: crate::ffi::ConsoleType::Gbc,
            width: 160,
            height: 144,
            speed: 1.0,
            frame_skip: 0,
            cpu_cycles: 0,
            rendered_frames: 0,
            gbc_cpu: crate::gbc::cpu::Cpu::new(),
            gbc_mmu: crate::gbc::mmu::Mmu::new(vec![], None),
            gbc_ppu: crate::gbc::ppu::Ppu::new(),
            rom_path: std::path::PathBuf::new(),
            base_dir: std::path::PathBuf::new(),
            rom_loaded: false,
            gba_cpu: crate::gba::cpu::GbaCpu::new(),
            gba_mmu: crate::gba::mmu::GbaMmu::new(vec![]),
            gba_ppu: crate::gba::ppu::GbaPpu::new(),
            extra_fields: Vec::new(),
        }
    }

    pub fn play(&mut self) {
        self.is_playing = true;
    }

    pub fn pause(&mut self) {
        self.is_playing = false;
    }

    pub fn reset(&mut self) {
        self.state = EmulatorState::Splash;
        self.ticks = 0;
        if self.console_type == crate::ffi::ConsoleType::Gba {
            self.player_x = 120;
            self.player_y = 80;
            self.gba_cpu.reset();
            self.gba_ppu = crate::gba::ppu::GbaPpu::new();
            self.gba_mmu.apu = crate::gba::apu::GbaApu::new();
        } else {
            self.player_x = 80;
            self.player_y = 72;
            self.gbc_cpu.reset();
            self.gbc_ppu.reset();
            self.gbc_mmu.apu.reset();
        }
        self.buttons = ButtonState {
            up: false,
            down: false,
            left: false,
            right: false,
            a: false,
            b: false,
            start: false,
            select: false,
            l: false,
            r: false,
        };
        self.cpu_cycles = 0;
        self.rendered_frames = 0;
        self.raw_video_buffer.fill(0);
        self.raw_audio_buffer.fill(0);
    }

    fn reset_on_rom_load(&mut self) {
        self.ticks = 0;
        self.state = EmulatorState::Splash;
        self.cpu_cycles = 0;
        self.rendered_frames = 0;
        self.raw_video_buffer.fill(0);
        self.raw_audio_buffer.fill(0);
        self.gbc_cpu.reset();
        self.gbc_ppu.reset();
        self.gbc_mmu.apu.reset();
        self.gba_cpu.reset();
        self.gba_ppu = crate::gba::ppu::GbaPpu::new();
        self.gba_mmu.apu = crate::gba::apu::GbaApu::new();
    }

    pub fn tick(&mut self) {
        let num_samples = std::cmp::min((735.0 * self.speed) as usize, 2940);
        if !self.is_playing {
            for i in 0..num_samples * 2 {
                self.raw_audio_buffer[self.audio_offset + i] = 0;
            }
            return;
        }

        self.ticks += 1;

        // Periodic check for SRAM save
        if self.console_type == crate::ffi::ConsoleType::Gbc && self.rom_loaded {
            if self.ticks % 600 == 0 && self.gbc_mmu.mbc.is_dirty {
                if !self.rom_path.as_os_str().is_empty() {
                    let _ = self.gbc_mmu.mbc.save_sram(&self.rom_path, &self.base_dir);
                    self.gbc_mmu.mbc.is_dirty = false;
                }
            }
        }

        let is_render_tick = self.ticks % (self.frame_skip + 1) == 0;
        if is_render_tick {
            self.rendered_frames += 1;
        }

        let base_cycles = match self.console_type {
            crate::ffi::ConsoleType::Gba => 280896,
            crate::ffi::ConsoleType::Gbc => {
                if self.gbc_cpu.double_speed {
                    140448
                } else {
                    70224
                }
            }
            _ => 70224,
        };
        let raw_budget = (base_cycles as f32 * self.speed) as u32;
        let cycle_budget = std::cmp::min(raw_budget, 5_000_000);

        // Handle splash state transitions and rendering
        if self.state == EmulatorState::Splash {
            self.cpu_cycles = self.cpu_cycles.wrapping_add(cycle_budget as u64);

            if self.buttons.start {
                self.state = EmulatorState::Gameplay;
                if self.console_type == crate::ffi::ConsoleType::Gba {
                    self.player_x = 120;
                    self.player_y = 80;
                } else {
                    self.player_x = 80;
                    self.player_y = 72;
                }

                if is_render_tick {
                    let bg_r = ((self.ticks * 2) % 256) as u8;
                    let bg_g = ((self.ticks * 3) % 256) as u8;
                    let bg_b = ((self.ticks * 5) % 256) as u8;
                    let active_len = self.width as usize * self.height as usize * 3;
                    for i in (0..active_len).step_by(3) {
                        self.raw_video_buffer[self.video_offset + i] = bg_r;
                        self.raw_video_buffer[self.video_offset + i + 1] = bg_g;
                        self.raw_video_buffer[self.video_offset + i + 2] = bg_b;
                    }
                    let offset = ((self.player_y as usize * self.width as usize)
                        + self.player_x as usize)
                        * 3;
                    if offset + 2 < active_len {
                        self.raw_video_buffer[self.video_offset + offset] = 255;
                        self.raw_video_buffer[self.video_offset + offset + 1] = 0;
                        self.raw_video_buffer[self.video_offset + offset + 2] = 0;
                    }
                }
            } else {
                if is_render_tick {
                    let animated_blue = (255 - (self.ticks % 256)) as u8;
                    let active_len = self.width as usize * self.height as usize * 3;
                    for i in (0..active_len).step_by(3) {
                        self.raw_video_buffer[self.video_offset + i] = 0;
                        self.raw_video_buffer[self.video_offset + i + 1] = 0;
                        self.raw_video_buffer[self.video_offset + i + 2] = 255;
                    }
                    if active_len >= 3 {
                        self.raw_video_buffer[self.video_offset + 2] = animated_blue;
                    }
                }
            }
            for i in 0..num_samples * 2 {
                self.raw_audio_buffer[self.audio_offset + i] = 0;
            }
            return;
        }

        // Gameplay state logic
        let mut move_x = 0;
        let mut move_y = 0;

        if self.buttons.left && self.buttons.right {
            move_x = 0;
        } else if self.buttons.left {
            move_x = -1;
        } else if self.buttons.right {
            move_x = 1;
        }

        if self.buttons.up && self.buttons.down {
            move_y = 0;
        } else if self.buttons.up {
            move_y = -1;
        } else if self.buttons.down {
            move_y = 1;
        }

        let is_jumping = self.buttons.a;
        if is_jumping {
            move_y = -1;
        }

        self.player_x = (self.player_x as i32 + move_x).clamp(0, self.width as i32 - 1) as u8;
        self.player_y = (self.player_y as i32 + move_y).clamp(0, self.height as i32 - 1) as u8;

        if self.console_type == crate::ffi::ConsoleType::Gbc && self.rom_loaded {
            self.gbc_mmu.buttons = self.buttons;
            self.gbc_mmu.apu.resampler.sample_count = 0;

            let mut cycles_run = 0;
            let mut instructions_run = 0;

            let audio_buf = &mut self.raw_audio_buffer;
            let audio_off = self.audio_offset;
            let video_off = self.video_offset;

            let video_len = 160 * 144 * 3;
            let video_slice = &mut self.raw_video_buffer[video_off..video_off + video_len];

            let double_speed = self.gbc_cpu.double_speed;

            while cycles_run < cycle_budget {
                if instructions_run >= 150_000 {
                    break;
                }

                let elapsed = self.gbc_cpu.step(&mut self.gbc_mmu);
                cycles_run += elapsed;
                instructions_run += 1;

                if is_render_tick {
                    self.gbc_ppu
                        .tick(elapsed, &mut self.gbc_mmu, video_slice, double_speed);
                } else {
                    let mut dummy = [0u8; 160 * 144 * 3];
                    self.gbc_ppu
                        .tick(elapsed, &mut self.gbc_mmu, &mut dummy, double_speed);
                }

                self.gbc_mmu.apu.tick(
                    elapsed,
                    &mut self.gbc_mmu.io,
                    audio_buf,
                    audio_off,
                    double_speed,
                    self.speed,
                );
                self.gbc_mmu
                    .mbc
                    .rtc
                    .tick_cycles(elapsed as u64, double_speed);
            }

            self.cpu_cycles = self.cpu_cycles.wrapping_add(cycles_run as u64);
        } else if self.console_type == crate::ffi::ConsoleType::Gba && self.rom_loaded {
            self.gba_mmu.apu.resampler.sample_count = 0;
            let mut cycles_run = 0;
            let mut instructions_run = 0;

            let audio_buf = &mut self.raw_audio_buffer;
            let audio_off = self.audio_offset;
            let video_off = self.video_offset;

            let video_len = 240 * 160 * 3;
            let video_slice = &mut self.raw_video_buffer[video_off..video_off + video_len];

            let mut keyinput = 0x03FFu16;
            if self.buttons.a {
                keyinput &= !0x0001;
            }
            if self.buttons.b {
                keyinput &= !0x0002;
            }
            if self.buttons.select {
                keyinput &= !0x0004;
            }
            if self.buttons.start {
                keyinput &= !0x0008;
            }
            if self.buttons.right {
                keyinput &= !0x0010;
            }
            if self.buttons.left {
                keyinput &= !0x0020;
            }
            if self.buttons.up {
                keyinput &= !0x0040;
            }
            if self.buttons.down {
                keyinput &= !0x0080;
            }
            if self.buttons.r {
                keyinput &= !0x0100;
            }
            if self.buttons.l {
                keyinput &= !0x0200;
            }

            self.gba_mmu.write_halfword_safe(0x04000130, keyinput);

            while cycles_run < cycle_budget {
                if instructions_run >= 200_000 {
                    eprintln!("Instruction execution limit reached for this frame. Breaking loop.");
                    break;
                }

                if self.gba_cpu.halted {
                    let elapsed = cycle_budget - cycles_run;
                    self.gba_mmu.tick_system_components(
                        elapsed,
                        video_slice,
                        audio_buf,
                        audio_off,
                        self.speed,
                        &mut self.gba_ppu,
                        is_render_tick,
                    );
                    cycles_run += elapsed;
                    break;
                }

                let elapsed = self.gba_cpu.step(&mut self.gba_mmu);
                cycles_run += elapsed;
                instructions_run += 1;

                self.gba_mmu.tick_system_components(
                    elapsed,
                    video_slice,
                    audio_buf,
                    audio_off,
                    self.speed,
                    &mut self.gba_ppu,
                    is_render_tick,
                );
            }

            self.cpu_cycles = self.cpu_cycles.wrapping_add(cycles_run as u64);
        } else {
            // Fallback mock logic for GBA mode or GBC mode without ROM loaded
            self.cpu_cycles = self.cpu_cycles.wrapping_add(cycle_budget as u64);

            if is_render_tick {
                let bg_r = ((self.ticks * 2) % 256) as u8;
                let bg_g = ((self.ticks * 3) % 256) as u8;
                let bg_b = ((self.ticks * 5) % 256) as u8;
                let active_len = self.width as usize * self.height as usize * 3;
                for i in (0..active_len).step_by(3) {
                    self.raw_video_buffer[self.video_offset + i] = bg_r;
                    self.raw_video_buffer[self.video_offset + i + 1] = bg_g;
                    self.raw_video_buffer[self.video_offset + i + 2] = bg_b;
                }

                let offset =
                    ((self.player_y as usize * self.width as usize) + self.player_x as usize) * 3;
                if offset + 2 < active_len {
                    self.raw_video_buffer[self.video_offset + offset] = 255;
                    self.raw_video_buffer[self.video_offset + offset + 1] = 0;
                    self.raw_video_buffer[self.video_offset + offset + 2] = 0;
                }
            }

            // Audio generation
            if is_jumping {
                let frequency = 440.0;
                let amplitude = 10000.0;
                let sample_rate = 44100.0;
                for i in 0..num_samples {
                    let t = (self.ticks as f64 * 735.0 + i as f64) / sample_rate;
                    let val =
                        (amplitude * (2.0 * std::f64::consts::PI * frequency * t).sin()) as i32;
                    let clamped = val.clamp(-32768, 32767) as i16;
                    self.raw_audio_buffer[self.audio_offset + i * 2] = clamped;
                    self.raw_audio_buffer[self.audio_offset + i * 2 + 1] = clamped;
                }
            } else {
                for i in 0..num_samples * 2 {
                    self.raw_audio_buffer[self.audio_offset + i] = 0;
                }
            }
        }
    }

    pub fn inject_input(&mut self, buttons: ButtonState) {
        self.buttons = buttons;
    }

    pub fn get_video_buffer(&self) -> &[u8] {
        let len = match self.console_type {
            crate::ffi::ConsoleType::Gbc => 160 * 144 * 3,
            crate::ffi::ConsoleType::Gba => 240 * 160 * 3,
            _ => 160 * 144 * 3,
        };
        &self.raw_video_buffer[self.video_offset..self.video_offset + len]
    }

    pub fn get_audio_buffer(&self) -> &[i16] {
        let sample_count = if self.rom_loaded && self.state == EmulatorState::Gameplay && self.is_playing {
            if self.console_type == crate::ffi::ConsoleType::Gba {
                self.gba_mmu.apu.resampler.sample_count
            } else {
                self.gbc_mmu.apu.resampler.sample_count
            }
        } else {
            std::cmp::min((735.0 * self.speed) as usize, 2940)
        };
        let max_samples = (self.raw_audio_buffer.len() - self.audio_offset) / 2;
        let clamped = std::cmp::min(sample_count, max_samples);
        &self.raw_audio_buffer[self.audio_offset..self.audio_offset + clamped * 2]
    }

    pub fn get_ticks(&self) -> u32 {
        self.ticks
    }

    pub fn is_playing(&self) -> bool {
        self.is_playing
    }

    pub fn get_state_string(&self) -> String {
        match self.state {
            EmulatorState::Splash => "splash".to_string(),
            EmulatorState::Gameplay => "gameplay".to_string(),
        }
    }

    pub fn get_player_x(&self) -> u8 {
        self.player_x
    }

    pub fn get_player_y(&self) -> u8 {
        self.player_y
    }

    pub fn get_button_state(&self) -> ButtonState {
        self.buttons
    }

    pub fn get_console_type(&self) -> crate::ffi::ConsoleType {
        self.console_type
    }

    pub fn get_width(&self) -> u32 {
        self.width
    }

    pub fn get_height(&self) -> u32 {
        self.height
    }

    pub fn get_speed(&self) -> f32 {
        self.speed
    }

    pub fn get_frame_skip(&self) -> u32 {
        self.frame_skip
    }

    pub fn get_cpu_cycles(&self) -> u64 {
        self.cpu_cycles
    }

    pub fn get_rendered_frames(&self) -> u32 {
        self.rendered_frames
    }

    pub fn set_speed(&mut self, speed: f32) {
        if speed > 0.0 && speed.is_finite() {
            self.speed = speed;
        }
    }

    pub fn set_frame_skip(&mut self, frame_skip: u32) {
        self.frame_skip = frame_skip;
    }

    pub fn load_rom(&mut self, rom_data: &[u8]) -> bool {
        if rom_data.len() > 32 * 1024 * 1024 {
            return false;
        }
        match crate::rom::validate_and_parse_header(rom_data) {
            Ok(console) => {
                self.console_type = console;
                if console == crate::ffi::ConsoleType::Gba {
                    self.width = 240;
                    self.height = 160;
                    self.player_x = 120;
                    self.player_y = 80;
                    self.gba_mmu = crate::gba::mmu::GbaMmu::new(rom_data.to_vec());
                    self.gba_cpu.reset();
                    self.gba_ppu = crate::gba::ppu::GbaPpu::new();
                    self.rom_loaded = true;
                } else {
                    self.width = 160;
                    self.height = 144;
                    self.player_x = 80;
                    self.player_y = 72;
                    self.gbc_mmu = crate::gbc::mmu::Mmu::new(rom_data.to_vec(), None);
                    self.gbc_cpu.reset();
                    self.gbc_ppu.reset();
                    self.rom_loaded = true;
                }
                self.reset_on_rom_load();
                true
            }
            Err(_) => false,
        }
    }

    pub fn load_rom_path(&mut self, rom_path: &str, base_dir: &str) -> String {
        let path = Path::new(rom_path);
        let base = Path::new(base_dir);

        let safe_path = match crate::rom::validate_path_safety(path, base) {
            Ok(p) => p,
            Err(e) => return format!("LOAD_ROM_ERROR {}", e),
        };

        if !safe_path.exists() {
            return "LOAD_ROM_ERROR File not found".to_string();
        }

        if safe_path.is_dir() {
            return "LOAD_ROM_ERROR Not a file".to_string();
        }

        let meta = match std::fs::metadata(&safe_path) {
            Ok(m) => m,
            Err(e) => return format!("LOAD_ROM_ERROR {}", e),
        };

        if meta.len() == 0 {
            return "LOAD_ROM_ERROR Empty ROM file".to_string();
        }

        if meta.len() > 32 * 1024 * 1024 {
            return "LOAD_ROM_ERROR File size exceeds 32MB limit".to_string();
        }

        let data = match std::fs::read(&safe_path) {
            Ok(d) => d,
            Err(e) => return format!("LOAD_ROM_ERROR {}", e),
        };

        match crate::rom::validate_and_parse_header(&data) {
            Ok(console) => {
                self.console_type = console;
                self.rom_path = safe_path.clone();
                self.base_dir = base.to_path_buf();
                if console == crate::ffi::ConsoleType::Gba {
                    self.width = 240;
                    self.height = 160;
                    self.player_x = 120;
                    self.player_y = 80;
                    self.gba_mmu = crate::gba::mmu::GbaMmu::new(data.clone());
                    self.gba_cpu.reset();
                    self.gba_ppu = crate::gba::ppu::GbaPpu::new();
                    let _ = self.gba_mmu.flash.load_flash_from_disk(&safe_path, base);
                    self.rom_loaded = true;
                } else {
                    self.width = 160;
                    self.height = 144;
                    self.player_x = 80;
                    self.player_y = 72;
                    let mut initial_ram = None;
                    let mut dummy_rtc = crate::gbc::mbc3::RealTimeClock::new();

                    let sav_path = safe_path.with_extension("sav");
                    if let Ok(safe_sav_path) = crate::rom::validate_path_safety(&sav_path, base) {
                        if safe_sav_path.exists() {
                            if let Ok(save_data) = std::fs::read(&safe_sav_path) {
                                if save_data.len() >= 32 * 1024 {
                                    initial_ram = Some(save_data[..32 * 1024].to_vec());
                                    if save_data.len() >= 32 * 1024 + 28 {
                                        let footer = &save_data[32 * 1024..];
                                        let seconds =
                                            u32::from_le_bytes(footer[0..4].try_into().unwrap())
                                                as u8;
                                        let minutes =
                                            u32::from_le_bytes(footer[4..8].try_into().unwrap())
                                                as u8;
                                        let hours =
                                            u32::from_le_bytes(footer[8..12].try_into().unwrap())
                                                as u8;
                                        let days =
                                            u32::from_le_bytes(footer[12..16].try_into().unwrap())
                                                as u16;
                                        let flags =
                                            u32::from_le_bytes(footer[16..20].try_into().unwrap());
                                        let saved_timestamp =
                                            u64::from_le_bytes(footer[20..28].try_into().unwrap());

                                        dummy_rtc.seconds = seconds;
                                        dummy_rtc.minutes = minutes;
                                        dummy_rtc.hours = hours;
                                        dummy_rtc.days = days;
                                        dummy_rtc.halt = (flags & 0x01) != 0;
                                        dummy_rtc.day_overflow = (flags & 0x02) != 0;

                                        if !dummy_rtc.halt && saved_timestamp > 0 {
                                            if let Ok(duration) = std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                            {
                                                let current_timestamp = duration.as_secs();
                                                if current_timestamp > saved_timestamp {
                                                    let diff = current_timestamp - saved_timestamp;
                                                    for _ in 0..diff {
                                                        dummy_rtc.increment_second();
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    self.gbc_mmu = crate::gbc::mmu::Mmu::new(data.to_vec(), initial_ram);
                    self.gbc_mmu.mbc.rtc = dummy_rtc;
                    self.gbc_cpu.reset();
                    self.gbc_ppu.reset();
                    self.rom_loaded = true;
                }
                self.reset_on_rom_load();
                "LOAD_ROM_OK".to_string()
            }
            Err(e) => {
                if e == "GBC header checksum mismatch" {
                    "LOAD_ROM_ERROR GBC header checksum mismatch".to_string()
                } else if e == "GBA header checksum mismatch" {
                    "LOAD_ROM_ERROR GBA header checksum mismatch".to_string()
                } else {
                    format!("LOAD_ROM_ERROR {}", e)
                }
            }
        }
    }
}
