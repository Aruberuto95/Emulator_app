use crate::ffi::ButtonState;
use std::path::Path;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum EmulatorState {
    Splash,
    Gameplay,
}

/// Allocates a zeroed `Vec<T>` with `len` elements plus padding so that
/// `&vec[offset..]` starts on an `alignment`-byte boundary.
/// Returns `(vec, offset)` where `offset` is in ELEMENTS, not bytes.
pub(crate) fn allocate_aligned<T: Copy + Default>(len: usize, alignment: usize) -> (Vec<T>, usize) {
    let elem_size = std::mem::size_of::<T>();
    debug_assert!(alignment % elem_size == 0);
    let vec = vec![T::default(); len + alignment / elem_size];
    let ptr = vec.as_ptr() as usize;
    let aligned_ptr = (ptr + alignment - 1) & !(alignment - 1);
    let offset = (aligned_ptr - ptr) / elem_size;
    (vec, offset)
}

/// Packs 8-bit RGB into BGR555, the native framebuffer format (splash/mock paths only;
/// the PPUs produce BGR555 directly from palette RAM).
pub(crate) fn bgr555(r: u8, g: u8, b: u8) -> u16 {
    (((b as u16) >> 3) << 10) | (((g as u16) >> 3) << 5) | ((r as u16) >> 3)
}

pub struct Emulator {
    /// Back buffer the PPU draws into. Pixel format is BGR555 (XBGR1555):
    /// R bits 0-4, G bits 5-9, B bits 10-14, bit 15 always 0.
    pub(crate) raw_video_buffer: Vec<u16>,
    /// Front buffer returned by get_video_buffer(). The PPU draws into raw_video_buffer
    /// (back); on the VBlank edge the two buffers (and their offsets) are swapped, so
    /// the frontend never observes a half-rendered frame (no tearing).
    pub(crate) front_video_buffer: Vec<u16>,
    pub(crate) raw_audio_buffer: Vec<i16>,
    pub(crate) video_offset: usize,
    pub(crate) front_offset: usize,
    pub(crate) audio_offset: usize,
    pub(crate) is_playing: bool,
    pub(crate) buttons: ButtonState,
    pub(crate) ticks: u32,
    pub(crate) state: EmulatorState,
    pub(crate) player_x: u32,
    pub(crate) player_y: u32,
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
    pub n64_mmu: crate::n64::mmu::N64Mmu,
    pub n64_cpu: crate::n64::cpu::Cpu,
    pub(crate) expansion_pak: bool,
    pub(crate) extra_fields: Vec<(String, String)>,
}

impl Emulator {
    pub fn new() -> Self {
        let (raw_video_buffer, video_offset) = allocate_aligned::<u16>(240 * 160, 16);
        let (front_video_buffer, front_offset) = allocate_aligned::<u16>(240 * 160, 16);
        let (raw_audio_buffer, audio_offset) = allocate_aligned::<i16>(1470 * 4, 16);

        Self {
            raw_video_buffer,
            front_video_buffer,
            raw_audio_buffer,
            video_offset,
            front_offset,
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
                stick_x: 0,
                stick_y: 0,
                z: false,
                c_up: false,
                c_down: false,
                c_left: false,
                c_right: false,
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
            n64_mmu: crate::n64::mmu::N64Mmu::new(vec![]),
            n64_cpu: crate::n64::cpu::Cpu::new(),
            expansion_pak: false,
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
        self.state = if self.rom_loaded {
            EmulatorState::Gameplay
        } else {
            EmulatorState::Splash
        };
        self.ticks = 0;
        if self.console_type == crate::ffi::ConsoleType::Gba {
            self.player_x = 120;
            self.player_y = 80;
            self.gba_ppu = crate::gba::ppu::GbaPpu::new();
            self.gba_mmu.apu = crate::gba::apu::GbaApu::new();
            self.gba_cpu.boot(&mut self.gba_mmu);
        } else if self.console_type == crate::ffi::ConsoleType::Nintendo64 {
            self.player_x = 160;
            self.player_y = 120;
            self.n64_cpu.reset();
            self.n64_cpu.hle_boot(&mut self.n64_mmu);
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
            stick_x: 0,
            stick_y: 0,
            z: false,
            c_up: false,
            c_down: false,
            c_left: false,
            c_right: false,
        };
        self.cpu_cycles = 0;
        self.rendered_frames = 0;
        self.raw_video_buffer.fill(0);
        self.front_video_buffer.fill(0);
        self.raw_audio_buffer.fill(0);
    }

    fn reset_on_rom_load(&mut self) {
        self.ticks = 0;
        self.state = EmulatorState::Gameplay;
        self.cpu_cycles = 0;
        self.rendered_frames = 0;
        self.raw_video_buffer.fill(0);
        self.front_video_buffer.fill(0);
        self.raw_audio_buffer.fill(0);
        self.gbc_cpu.reset();
        self.gbc_ppu.reset();
        self.gbc_mmu.apu.reset();
        self.gba_ppu = crate::gba::ppu::GbaPpu::new();
        self.gba_mmu.apu = crate::gba::apu::GbaApu::new();
        self.gba_cpu.boot(&mut self.gba_mmu);
        if self.console_type == crate::ffi::ConsoleType::Nintendo64 {
            self.player_x = 160;
            self.player_y = 120;
            self.n64_cpu.reset();
            self.n64_cpu.hle_boot(&mut self.n64_mmu);
        }
    }

    pub fn flush_battery(&mut self) {
        if !self.rom_loaded || self.rom_path.as_os_str().is_empty() {
            return;
        }
        if self.console_type == crate::ffi::ConsoleType::Gba {
            let _ = self.gba_mmu.flash.load_flash_from_disk(&self.rom_path, &self.base_dir);
        } else if self.console_type == crate::ffi::ConsoleType::Gbc {
            if self.gbc_mmu.mbc.ram_dirty {
                self.gbc_mmu.mbc.ram_dirty = false;
                let sav_path = self.rom_path.with_extension("sav");
                if let Ok(safe_sav_path) = crate::rom::validate_path_safety(&sav_path, &self.base_dir) {
                    let mut data = self.gbc_mmu.mbc.ram.clone();
                    if self.gbc_mmu.mbc.has_rtc {
                        let rtc = &self.gbc_mmu.mbc.rtc;
                        data.extend_from_slice(&(rtc.seconds as u32).to_le_bytes());
                        data.extend_from_slice(&(rtc.minutes as u32).to_le_bytes());
                        data.extend_from_slice(&(rtc.hours as u32).to_le_bytes());
                        data.extend_from_slice(&(rtc.days as u32).to_le_bytes());
                        let flags = if rtc.halt { 1 } else { 0 } | if rtc.day_overflow { 2 } else { 0 };
                        data.extend_from_slice(&flags.to_be_bytes());
                        if let Ok(duration) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                            data.extend_from_slice(&duration.as_secs().to_le_bytes());
                        } else {
                            data.extend_from_slice(&0u64.to_le_bytes());
                        }
                    }
                    let _ = std::fs::write(&safe_sav_path, &data);
                }
            }
        }
    }

    pub fn tick(&mut self) {
        let placeholder_samples = std::cmp::min((735.0 * self.speed) as usize, 2940);
        if !self.is_playing {
            for i in 0..placeholder_samples * 2 {
                self.raw_audio_buffer[self.audio_offset + i] = 0;
            }
            return;
        }

        self.ticks += 1;

        if self.ticks % 600 == 0 {
            self.flush_battery();
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
            crate::ffi::ConsoleType::Nintendo64 => 280896,
        };
        let raw_budget = (base_cycles as f32 * self.speed) as u32;
        let cycle_budget = std::cmp::min(raw_budget, 5_000_000);

        if self.state == EmulatorState::Splash {
            self.cpu_cycles = self.cpu_cycles.wrapping_add(cycle_budget as u64);

            if self.buttons.start {
                self.state = EmulatorState::Gameplay;
                if self.console_type == crate::ffi::ConsoleType::Gba {
                    self.player_x = 120;
                    self.player_y = 80;
                } else if self.console_type == crate::ffi::ConsoleType::Nintendo64 {
                    self.player_x = 160;
                    self.player_y = 120;
                } else {
                    self.player_x = 80;
                    self.player_y = 72;
                }

                if is_render_tick {
                    let bg = bgr555(
                        ((self.ticks * 2) % 256) as u8,
                        ((self.ticks * 3) % 256) as u8,
                        ((self.ticks * 5) % 256) as u8,
                    );
                    let active_len = self.width as usize * self.height as usize;
                    let vo = self.video_offset;
                    self.raw_video_buffer[vo..vo + active_len].fill(bg);
                    let offset = (self.player_y as usize * self.width as usize)
                        + self.player_x as usize;
                    if offset < active_len {
                        self.raw_video_buffer[vo + offset] = bgr555(255, 0, 0);
                    }
                    self.present_frame();
                }
            } else {
                if is_render_tick {
                    let animated_blue = (255 - (self.ticks % 256)) as u8;
                    let active_len = self.width as usize * self.height as usize;
                    let vo = self.video_offset;
                    self.raw_video_buffer[vo..vo + active_len].fill(bgr555(0, 0, 255));
                    if active_len > 0 {
                        self.raw_video_buffer[vo] = bgr555(0, 0, animated_blue);
                    }
                    self.present_frame();
                }
            }
            for i in 0..placeholder_samples * 2 {
                self.raw_audio_buffer[self.audio_offset + i] = 0;
            }
            return;
        }

        if self.console_type == crate::ffi::ConsoleType::Gbc && self.rom_loaded {
            self.gbc_mmu.buttons = self.buttons;
            self.gbc_mmu.apu.resampler.sample_count = 0;

            let mut cycles_run = 0;
            let mut instructions_run = 0;
            let audio_off = self.audio_offset;
            let video_len = 160 * 144;
            let double_speed = self.gbc_cpu.double_speed;

            while cycles_run < cycle_budget {
                if instructions_run as u32 >= cycle_budget {
                    break;
                }

                let elapsed = self.gbc_cpu.step(&mut self.gbc_mmu);
                cycles_run += elapsed;
                instructions_run += 1;

                let render_pixels =
                    is_render_tick && (cycles_run + base_cycles as u32 >= cycle_budget);

                let vo = self.video_offset;
                self.gbc_ppu.tick(
                    elapsed,
                    &mut self.gbc_mmu,
                    &mut self.raw_video_buffer[vo..vo + video_len],
                    render_pixels,
                    double_speed,
                );

                if self.gbc_ppu.frame_completed {
                    self.gbc_ppu.frame_completed = false;
                    if render_pixels {
                        self.present_frame();
                    }
                }

                self.gbc_mmu.apu.tick(
                    elapsed,
                    &mut self.gbc_mmu.io,
                    &mut self.raw_audio_buffer,
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
            let audio_off = self.audio_offset;
            let video_len = 240 * 160;

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

            let mut until_event = self.gba_mmu.cycles_to_next_event(&self.gba_ppu, self.speed);
            while cycles_run < cycle_budget {
                if instructions_run as u32 >= cycle_budget {
                    break;
                }

                if self.gba_cpu.halted {
                    let chunk = (cycle_budget - cycles_run).min(until_event).max(1);
                    let render_pixels =
                        is_render_tick && (cycles_run + base_cycles as u32 >= cycle_budget);
                    let batch = self.gba_mmu.pending_cycles + chunk;
                    self.gba_mmu.pending_cycles = 0;
                    let vo = self.video_offset;
                    self.gba_mmu.tick_system_components(
                        batch,
                        &mut self.raw_video_buffer[vo..vo + video_len],
                        &mut self.raw_audio_buffer,
                        audio_off,
                        self.speed,
                        &mut self.gba_ppu,
                        render_pixels,
                    );
                    self.gba_mmu.io_dirty = false;
                    until_event = self.gba_mmu.cycles_to_next_event(&self.gba_ppu, self.speed);
                    if self.gba_ppu.frame_completed {
                        self.gba_ppu.frame_completed = false;
                        if render_pixels {
                            self.present_frame();
                        }
                    }
                    cycles_run += chunk;
                    let halt_step = self.gba_cpu.step(&mut self.gba_mmu);
                    cycles_run += halt_step;
                    self.gba_mmu.pending_cycles += halt_step;
                    continue;
                }

                let elapsed = self.gba_cpu.step(&mut self.gba_mmu);
                cycles_run += elapsed;
                instructions_run += 1;
                self.gba_mmu.pending_cycles += elapsed;

                if self.gba_mmu.pending_cycles >= until_event || self.gba_mmu.io_dirty {
                    let render_pixels =
                        is_render_tick && (cycles_run + base_cycles as u32 >= cycle_budget);
                    let pending = self.gba_mmu.pending_cycles;
                    self.gba_mmu.pending_cycles = 0;
                    let vo = self.video_offset;
                    self.gba_mmu.tick_system_components(
                        pending,
                        &mut self.raw_video_buffer[vo..vo + video_len],
                        &mut self.raw_audio_buffer,
                        audio_off,
                        self.speed,
                        &mut self.gba_ppu,
                        render_pixels,
                    );
                    self.gba_mmu.io_dirty = false;
                    until_event = self.gba_mmu.cycles_to_next_event(&self.gba_ppu, self.speed);
                    if self.gba_ppu.frame_completed {
                        self.gba_ppu.frame_completed = false;
                        if render_pixels {
                            self.present_frame();
                        }
                    }
                }
            }

            if self.gba_mmu.pending_cycles > 0 {
                let pending = self.gba_mmu.pending_cycles;
                self.gba_mmu.pending_cycles = 0;
                let vo = self.video_offset;
                self.gba_mmu.tick_system_components(
                    pending,
                    &mut self.raw_video_buffer[vo..vo + video_len],
                    &mut self.raw_audio_buffer,
                    audio_off,
                    self.speed,
                    &mut self.gba_ppu,
                    is_render_tick,
                );
                self.gba_mmu.io_dirty = false;
                if self.gba_ppu.frame_completed {
                    self.gba_ppu.frame_completed = false;
                    if is_render_tick {
                        self.present_frame();
                    }
                }
            }

            self.cpu_cycles = self.cpu_cycles.wrapping_add(cycles_run as u64);
        } else if self.console_type == crate::ffi::ConsoleType::Nintendo64 && self.rom_loaded {
            self.n64_mmu.buttons = self.buttons;
            self.n64_mmu.ai_resampler.sample_count = 0;

            let mut cycles_run = 0;
            let audio_off = self.audio_offset;
            while cycles_run < cycle_budget {
                let elapsed = self.n64_cpu.step(&mut self.n64_mmu);
                self.n64_mmu.tick_vi(elapsed);
                self.n64_mmu.tick_ai(elapsed, self.speed, &mut self.raw_audio_buffer, audio_off);
                cycles_run += elapsed;
            }
            self.cpu_cycles = self.cpu_cycles.wrapping_add(cycles_run as u64);

            let mut move_x = 0;
            let mut move_y = 0;
            if self.buttons.left && self.buttons.right {
                // Neutralize
            } else if self.buttons.left {
                move_x = -1;
            } else if self.buttons.right {
                move_x = 1;
            }

            if self.buttons.up && self.buttons.down {
                // Neutralize
            } else if self.buttons.up {
                move_y = -1;
            } else if self.buttons.down {
                move_y = 1;
            }

            if self.buttons.a {
                move_y = -1;
            }

            if self.buttons.stick_x.abs() > 10 {
                move_x += if self.buttons.stick_x > 0 { 1 } else { -1 };
            }
            if self.buttons.stick_y.abs() > 10 {
                move_y += if self.buttons.stick_y > 0 { 1 } else { -1 };
            }

            let max_w = self.width as i32;
            let max_h = self.height as i32;
            self.player_x = (std::cmp::max(0, std::cmp::min(max_w - 1, self.player_x as i32 + move_x))) as u32;
            self.player_y = (std::cmp::max(0, std::cmp::min(max_h - 1, self.player_y as i32 + move_y))) as u32;

            if is_render_tick {
                let vo = self.video_offset;
                let active_len = (self.width * self.height) as usize;

                self.n64_mmu.update_video_buffer(
                    &mut self.raw_video_buffer[vo..vo + active_len],
                    self.width,
                    self.height,
                    self.player_x,
                    self.player_y,
                );

                self.present_frame();
            }

            // Fallback audio if no samples are generated
            if self.n64_mmu.ai_resampler.sample_count == 0 {
                for i in 0..placeholder_samples * 2 {
                    self.raw_audio_buffer[self.audio_offset + i] = 0;
                }
            }
        } else {
            self.cpu_cycles = self.cpu_cycles.wrapping_add(cycle_budget as u64);

            if is_render_tick {
                let bg = bgr555(
                    ((self.ticks * 2) % 256) as u8,
                    ((self.ticks * 3) % 256) as u8,
                    ((self.ticks * 5) % 256) as u8,
                );
                let active_len = self.width as usize * self.height as usize;
                let vo = self.video_offset;
                self.raw_video_buffer[vo..vo + active_len].fill(bg);

                let offset =
                    (self.player_y as usize * self.width as usize) + self.player_x as usize;
                if offset < active_len {
                    self.raw_video_buffer[vo + offset] = bgr555(255, 0, 0);
                }
                self.present_frame();
            }

            let is_jumping = self.buttons.a;
            if is_jumping {
                let frequency = 440.0;
                let amplitude = 10000.0;
                let sample_rate = 44100.0;
                for i in 0..placeholder_samples {
                    let t = (self.ticks as f64 * 735.0 + i as f64) / sample_rate;
                    let val =
                        (amplitude * (2.0 * std::f64::consts::PI * frequency * t).sin()) as i32;
                    let clamped = val.clamp(-32768, 32767) as i16;
                    self.raw_audio_buffer[self.audio_offset + i * 2] = clamped;
                    self.raw_audio_buffer[self.audio_offset + i * 2 + 1] = clamped;
                }
            } else {
                for i in 0..placeholder_samples * 2 {
                    self.raw_audio_buffer[self.audio_offset + i] = 0;
                }
            }
        }
    }

    fn active_video_len(&self) -> usize {
        (self.width * self.height) as usize
    }

    fn present_frame(&mut self) {
        std::mem::swap(&mut self.raw_video_buffer, &mut self.front_video_buffer);
        std::mem::swap(&mut self.video_offset, &mut self.front_offset);
    }

    pub fn get_video_buffer(&self) -> &[u16] {
        let len = self.active_video_len();
        &self.front_video_buffer[self.front_offset..self.front_offset + len]
    }

    pub fn get_audio_buffer(&self) -> &[i16] {
        if !self.is_playing {
            return &[];
        }
        let sample_count = if self.rom_loaded && self.state == EmulatorState::Gameplay && self.is_playing {
            if self.console_type == crate::ffi::ConsoleType::Gba {
                self.gba_mmu.apu.resampler.sample_count
            } else if self.console_type == crate::ffi::ConsoleType::Nintendo64 {
                if self.n64_mmu.ai_resampler.sample_count > 0 {
                    self.n64_mmu.ai_resampler.sample_count
                } else {
                    std::cmp::min((735.0 * self.speed) as usize, 2940)
                }
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

    pub fn get_player_x(&self) -> u16 {
        self.player_x as u16
    }

    pub fn get_player_y(&self) -> u16 {
        self.player_y as u16
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
        let is_n64 = crate::rom::detect_n64_endianness(rom_data).is_some();
        let max_size = if is_n64 { 64 * 1024 * 1024 } else { 32 * 1024 * 1024 };
        if rom_data.len() > max_size {
            return false;
        }

        let mut data_vec = Vec::new();
        let final_data = if let Some(endianness) = crate::rom::detect_n64_endianness(rom_data) {
            data_vec = rom_data.to_vec();
            crate::rom::byteswap_n64_rom(&mut data_vec, endianness);
            &data_vec[..]
        } else {
            rom_data
        };

        match crate::rom::validate_and_parse_header(final_data) {
            Ok(console) => {
                self.console_type = console;
                if console == crate::ffi::ConsoleType::Gba {
                    self.width = 240;
                    self.height = 160;
                    self.player_x = 120;
                    self.player_y = 80;
                    self.gba_mmu = crate::gba::mmu::GbaMmu::new(final_data.to_vec());
                    self.gba_cpu.reset();
                    self.gba_ppu = crate::gba::ppu::GbaPpu::new();
                    self.rom_loaded = true;
                } else if console == crate::ffi::ConsoleType::Nintendo64 {
                    self.width = 320;
                    self.height = 240;
                    self.player_x = 160;
                    self.player_y = 120;

                    let (raw_video, video_offset) = allocate_aligned::<u16>(320 * 240, 16);
                    let (front_video, front_offset) = allocate_aligned::<u16>(320 * 240, 16);
                    self.raw_video_buffer = raw_video;
                    self.video_offset = video_offset;
                    self.front_video_buffer = front_video;
                    self.front_offset = front_offset;

                    self.n64_mmu = crate::n64::mmu::N64Mmu::new(final_data.to_vec());
                    self.rom_loaded = true;
                } else {
                    self.width = 160;
                    self.height = 144;
                    self.player_x = 80;
                    self.player_y = 72;
                    self.gbc_mmu = crate::gbc::mmu::Mmu::new(final_data.to_vec(), None);
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

        let is_n64 = if let Some(ext) = safe_path.extension().and_then(|e| e.to_str()) {
            let lower_ext = ext.to_lowercase();
            lower_ext == "z64" || lower_ext == "v64" || lower_ext == "n64"
        } else {
            false
        };
        let max_size = if is_n64 { 64 * 1024 * 1024 } else { 32 * 1024 * 1024 };

        if meta.len() > max_size {
            return "LOAD_ROM_ERROR File size exceeds limit".to_string();
        }

        let mut data = match std::fs::read(&safe_path) {
            Ok(d) => d,
            Err(e) => return format!("LOAD_ROM_ERROR {}", e),
        };

        if let Some(endianness) = crate::rom::detect_n64_endianness(&data) {
            crate::rom::byteswap_n64_rom(&mut data, endianness);
        }

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
                    self.gba_ppu = crate::gba::ppu::GbaPpu::new();
                    let _ = self.gba_mmu.flash.load_flash_from_disk(&safe_path, base);
                    self.rom_loaded = true;
                } else if console == crate::ffi::ConsoleType::Nintendo64 {
                    self.width = 320;
                    self.height = 240;
                    self.player_x = 160;
                    self.player_y = 120;

                    let (raw_video, video_offset) = allocate_aligned::<u16>(320 * 240, 16);
                    let (front_video, front_offset) = allocate_aligned::<u16>(320 * 240, 16);
                    self.raw_video_buffer = raw_video;
                    self.video_offset = video_offset;
                    self.front_video_buffer = front_video;
                    self.front_offset = front_offset;

                    self.n64_mmu = crate::n64::mmu::N64Mmu::new(data);
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
                    self.gbc_mmu = crate::gbc::mmu::Mmu::new(data, initial_ram);
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
                } else if e == "Invalid Nintendo logo" {
                    "LOAD_ROM_ERROR Invalid Nintendo logo".to_string()
                } else {
                    "LOAD_ROM_ERROR Invalid ROM".to_string()
                }
            }
        }
    }

    pub fn get_expansion_pak(&self) -> bool {
        self.expansion_pak
    }

    pub fn set_expansion_pak(&mut self, enabled: bool) {
        if self.console_type == crate::ffi::ConsoleType::Nintendo64 {
            self.expansion_pak = enabled;
            if enabled {
                self.width = 640;
                self.height = 480;
                self.player_x = 320;
                self.player_y = 240;
            } else {
                self.width = 320;
                self.height = 240;
                self.player_x = 160;
                self.player_y = 120;
            }
            // Re-allocate video buffers
            let (raw_video, video_offset) = allocate_aligned::<u16>((self.width * self.height) as usize, 16);
            let (front_video, front_offset) = allocate_aligned::<u16>((self.width * self.height) as usize, 16);
            self.raw_video_buffer = raw_video;
            self.video_offset = video_offset;
            self.front_video_buffer = front_video;
            self.front_offset = front_offset;
        }
    }
}
