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
fn allocate_aligned<T: Copy + Default>(len: usize, alignment: usize) -> (Vec<T>, usize) {
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
fn bgr555(r: u8, g: u8, b: u8) -> u16 {
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
        // ponytail: un RESET con ROM cargado reinicia la consola dentro del juego,
        // no al placeholder azul. Splash solo aplica cuando no hay ROM.
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
            // In-game RESET reboots the cartridge (PC = entry, pipeline primed),
            // not the zeroed BIOS.
            self.gba_cpu.boot(&mut self.gba_mmu);
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
        self.front_video_buffer.fill(0);
        self.raw_audio_buffer.fill(0);
    }

    fn reset_on_rom_load(&mut self) {
        self.ticks = 0;
        // ponytail: un ROM cargado arranca directo en Gameplay (corre el core real);
        // Splash es solo el placeholder cuando no hay ROM. reset_on_rom_load() solo
        // se llama desde load_rom/load_rom_path, ambas tras rom_loaded = true.
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
        // Boot the ARM core into the cartridge entry point (sets PC + primes the
        // pipeline). Must come after the buffers/PPU reset above, and is the
        // single place both load paths converge on, so PC is never left at 0.
        self.gba_cpu.boot(&mut self.gba_mmu);
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
                    // Present inside the render gate: the swap invariant requires a
                    // fully redrawn back buffer (skipped ticks would flicker 2 frames).
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
            for i in 0..num_samples * 2 {
                self.raw_audio_buffer[self.audio_offset + i] = 0;
            }
            return;
        }

        // ponytail: the legacy per-frame "player_x/y" mock movement that lived here was dead
        // for real ROMs (nothing renders it) and has been removed. player_x/y now carry only
        // ROM-load defaults + savestate values, which is all the FFI getters/tests rely on.
        if self.console_type == crate::ffi::ConsoleType::Gbc && self.rom_loaded {
            self.gbc_mmu.buttons = self.buttons;
            self.gbc_mmu.apu.resampler.sample_count = 0;

            let mut cycles_run = 0;
            let mut instructions_run = 0;

            // Buffers are borrowed per call (not hoisted): present_frame() swaps the
            // back/front buffers and their offsets, so any pre-borrowed slice would go stale.
            let audio_off = self.audio_offset;
            let video_len = 160 * 144;

            let double_speed = self.gbc_cpu.double_speed;

            while cycles_run < cycle_budget {
                // See the GBA branch: livelock guard scaled by cycle_budget, not a fixed cap.
                // GB steps cost >=4 cycles so the old 150_000 never bound at normal speed, but
                // double-speed @ 4x needed ~140_448 instr — a 6.4% near-miss now removed.
                if instructions_run as u32 >= cycle_budget {
                    break;
                }

                let elapsed = self.gbc_cpu.step(&mut self.gbc_mmu);
                cycles_run += elapsed;
                instructions_run += 1;

                // Only rasterize the frame that will actually be presented. During
                // fast-forward (speed>1) cycle_budget spans several video frames; the
                // intermediate ones run timing-only (IRQ/DMA/scanline still advance, pixel
                // composition is skipped). speed==1 => cycle_budget==base_cycles => always
                // true, so 1x rendering is unchanged.
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

                // Present only on the VBlank edge: swap the completed back frame to the
                // front so the frontend never sees a half-drawn frame (no tearing).
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

            // Buffers are borrowed per flush (not hoisted): present_frame() swaps the
            // back/front buffers and their offsets, so any pre-borrowed slice would go stale.
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

            // Batching scheduler: instead of ticking timers/PPU/APU/DMA after every
            // instruction (~150k calls/frame, ~76% of runtime), accumulate the CPU's
            // cycle debt in mmu.pending_cycles and flush tick_system_components() only
            // when the next observable event is due (PPU boundary, timer overflow, APU
            // frame-seq step, audio-sample cap) or the CPU wrote an IO register. Every
            // IF raise still lands on its exact cycle: the flush batch is clamped
            // internally to those same boundaries, and a flush always runs after the
            // instruction that crossed the event — the same instruction boundary at
            // which the old per-instruction tick raised it.
            //
            // Known skew (accepted): the io_dirty flush runs after the writing
            // instruction, so the flushed batch renders its pre-write cycles with the
            // post-write register values — a register change lands retroactively by up
            // to one batch (~381 cycles, ~23 us). Per-instruction ticking had the same
            // skew at ~1 instruction; no game-visible effect has been traced to it.
            let mut until_event = self.gba_mmu.cycles_to_next_event(&self.gba_ppu, self.speed);
            while cycles_run < cycle_budget {
                // Livelock guard, not a speed limiter: every real step consumes >=1 cycle,
                // so instructions_run can never legitimately exceed cycle_budget. Bounding it
                // by cycle_budget makes the guard scale with speed (the old fixed 200_000 cap
                // throttled GBA fast-forward to ~1.4x, since THUMB code averages ~2 cyc/instr)
                // while still stopping a pathological zero-cycle loop; cycle_budget is already
                // clamped to 5_000_000, so per-tick work stays bounded even at extreme --speed.
                if instructions_run as u32 >= cycle_budget {
                    break;
                }

                if self.gba_cpu.halted {
                    // Jump straight to the next event while halted: IF bits only change
                    // at event boundaries, so this wakes the CPU with the same
                    // granularity as ticking cycle-by-cycle. until_event is never
                    // coarser than one scanline segment (<= 1232 cycles), which keeps
                    // the per-scanline wake timing that fades and transitions need.
                    let chunk = (cycle_budget - cycles_run).min(until_event).max(1);
                    // See the non-halted step below: only the final video frame of the
                    // budget is rasterized; fast-forward frames advance timing-only.
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
                    // step() returns ~1 cycle while still halted and clears `halted` (honoring
                    // IntrWait flags) the moment an enabled interrupt is pending.
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
                    // Only rasterize the final video frame of this tick; intermediate
                    // fast-forward frames advance timing-only (see GBC path for rationale).
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
                    // Clear AFTER the flush: IO writes made during the flush itself
                    // (PPU DISPSTAT updates, DMA transfers into IO) need no re-flush.
                    self.gba_mmu.io_dirty = false;
                    until_event = self.gba_mmu.cycles_to_next_event(&self.gba_ppu, self.speed);
                    // Present only on the VBlank edge (see GBC path) to avoid tearing.
                    if self.gba_ppu.frame_completed {
                        self.gba_ppu.frame_completed = false;
                        if render_pixels {
                            self.present_frame();
                        }
                    }
                }
            }

            // Drain any leftover cycle debt before this tick returns: the frontend
            // reads the audio buffer (resampler sample_count) and video state per
            // tick, so pending cycles must never carry across tick() calls.
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
        } else {
            // Fallback mock logic for GBA mode or GBC mode without ROM loaded
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
                // Present inside the render gate (see splash path): swap needs a
                // fully redrawn back buffer.
                self.present_frame();
            }

            // Audio generation: simple beep while A is held (mock placeholder, no-ROM only)
            let is_jumping = self.buttons.a;
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

    /// Active framebuffer length in u16 elements (BGR555 pixels) for the current console.
    fn active_video_len(&self) -> usize {
        match self.console_type {
            crate::ffi::ConsoleType::Gba => 240 * 160,
            _ => 160 * 144,
        }
    }

    /// Publishes the completed back frame by swapping back/front buffers and their
    /// alignment offsets (O(1); replaces the previous full-frame memcpy).
    /// Invariant: only call on a tick that fully re-rendered the back buffer —
    /// after the swap the new back buffer holds a two-presents-old image.
    fn present_frame(&mut self) {
        std::mem::swap(&mut self.raw_video_buffer, &mut self.front_video_buffer);
        std::mem::swap(&mut self.video_offset, &mut self.front_offset);
    }

    /// Returns the last presented frame as BGR555 (XBGR1555) pixels.
    /// The slice is invalidated by the next `tick()` (buffers may swap);
    /// callers must not hold it across ticks.
    pub fn get_video_buffer(&self) -> &[u16] {
        let len = self.active_video_len();
        &self.front_video_buffer[self.front_offset..self.front_offset + len]
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
                    self.gba_ppu = crate::gba::ppu::GbaPpu::new();
                    // Boot into the cartridge happens in reset_on_rom_load() below.
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

#[cfg(test)]
mod speed_scaling_tests {
    use super::*;
    use std::path::Path;

    /// Regression for the GBA fast-forward bug: the per-tick instruction guard in
    /// `tick()` must scale with `speed`. The old fixed `200_000` cap throttled GBA
    /// throughput to ~1.4x regardless of the requested multiplier, so "modify frame
    /// speed" did nothing for GBA while GB worked. This drives the real ARM core and
    /// checks that 4x actually advances ~4x the CPU cycles of 1x.
    ///
    /// Skips cleanly when the (untracked, copyrighted) test ROM is absent, so it never
    /// breaks a checkout that lacks `roms/`.
    #[test]
    fn gba_speed_scales_cpu_throughput() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let rom = repo
            .join("roms")
            .join("Pokemon - Emerald Version (USA, Europe).gba");
        if !rom.exists() {
            eprintln!(
                "SKIP gba_speed_scales_cpu_throughput: ROM not found at {}",
                rom.display()
            );
            return;
        }

        let mut emu = Emulator::new();
        let msg = emu.load_rom_path(rom.to_str().unwrap(), repo.to_str().unwrap());
        assert!(!msg.starts_with("LOAD_ROM_ERROR"), "load failed: {msg}");
        assert!(
            emu.get_console_type() == crate::ffi::ConsoleType::Gba,
            "expected GBA console type"
        );
        emu.play();

        // Run past boot into the ROM's steady CPU-bound loop before measuring.
        emu.set_speed(1.0);
        for _ in 0..60 {
            emu.tick();
        }

        let window = |emu: &mut Emulator, speed: f32| -> u64 {
            emu.set_speed(speed);
            let start = emu.get_cpu_cycles();
            for _ in 0..25 {
                emu.tick();
            }
            emu.get_cpu_cycles() - start
        };

        // Interleave 1x and 4x windows so both sample the same game phases; scene
        // changes then cancel out of the ratio instead of biasing it.
        let mut cycles_1x: u64 = 0;
        let mut cycles_4x: u64 = 0;
        for _ in 0..8 {
            cycles_1x += window(&mut emu, 1.0);
            cycles_4x += window(&mut emu, 4.0);
        }

        let ratio = cycles_4x as f64 / cycles_1x as f64;
        eprintln!(
            "GBA cpu-cycle throughput: 1x={cycles_1x}  4x={cycles_4x}  ratio={ratio:.2} (ideal 4.0; pre-fix ~1.4)"
        );

        assert!(
            ratio >= 2.5,
            "GBA speed did not scale: 4x/1x throughput ratio {ratio:.2} < 2.5 \
             (instruction cap still throttling fast-forward)"
        );
    }

    /// Manual perf probe (run with `cargo test -- --ignored --nocapture`): reports the
    /// core's sustainable WALL-CLOCK throughput, which the cycle-ratio test above cannot.
    /// GBA realtime = 16.78 Mcyc/s; true 4x fast-forward needs ~67 Mcyc/s. On this host the
    /// core tops out around 40-45 Mcyc/s (~2.4-2.7x), so requesting 4x is capped by raw core
    /// speed, not by the (correct) pacing logic — reaching a real 4x needs core optimization.
    /// `#[ignore]` so it doesn't add noise/time to the normal suite.
    #[test]
    #[ignore = "manual perf probe; run explicitly with --ignored --nocapture"]
    fn gba_wall_clock_throughput_probe() {
        use std::time::Instant;
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let rom = repo.join("roms").join("Pokemon - Emerald Version (USA, Europe).gba");
        if !rom.exists() { eprintln!("SKIP probe: ROM absent"); return; }

        let mut emu = Emulator::new();
        emu.load_rom_path(rom.to_str().unwrap(), repo.to_str().unwrap());
        emu.play();
        emu.set_speed(1.0);
        for _ in 0..120 { emu.tick(); }

        // Unthrottled: no frontend limiter here, so ticks run as fast as the core allows.
        let start_cy = emu.get_cpu_cycles();
        let t = Instant::now();
        let mut ticks = 0u64;
        while t.elapsed().as_millis() < 1000 { emu.tick(); ticks += 1; }
        let secs = t.elapsed().as_secs_f64();
        let cyc = emu.get_cpu_cycles() - start_cy;
        let mcyc_s = cyc as f64 / secs / 1.0e6;
        eprintln!(
            "PROBE: {ticks} ticks in {secs:.3}s | {mcyc_s:.1} Mcyc/s | realtime=16.78 | \
             max_speed≈{:.2}x | equiv_fps≈{:.1}",
            mcyc_s / 16.78, ticks as f64 / secs
        );
    }
}
