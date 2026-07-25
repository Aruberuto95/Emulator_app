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

    // Real NDS Emulator Components
    pub nds_arm9: crate::nds::cpu::Arm9Cpu,
    pub nds_arm7: crate::nds::cpu::Arm7Cpu,
    pub nds_mmu: crate::nds::mmu::NdsMmu,
    pub nds_ppu: crate::nds::ppu::NdsPpu,
}

impl Emulator {
    pub fn new() -> Self {
        let (raw_video_buffer, video_offset) = allocate_aligned::<u16>(256 * 384, 16);
        let (front_video_buffer, front_offset) = allocate_aligned::<u16>(256 * 384, 16);
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
                x: false,
                y: false,
                nds_touch_x: 0,
                nds_touch_y: 0,
                nds_touch_pressed: false,
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
            nds_arm9: crate::nds::cpu::Arm9Cpu::new(),
            nds_arm7: crate::nds::cpu::Arm7Cpu::new(),
            nds_mmu: crate::nds::mmu::NdsMmu::new(),
            nds_ppu: crate::nds::ppu::NdsPpu::new(),
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
        } else if self.console_type == crate::ffi::ConsoleType::Nds {
            self.player_x = 128;
            self.player_y = 192;
            self.nds_ppu.reset();
            if self.rom_loaded {
                let rom = self.nds_mmu.rom.clone();
                let _ = crate::nds::hle::boot_load_rom(&mut self.nds_mmu, &mut self.nds_arm9, &mut self.nds_arm7, &rom);
            }
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
            x: false,
            y: false,
            nds_touch_x: 0,
            nds_touch_y: 0,
            nds_touch_pressed: false,
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
        if self.console_type == crate::ffi::ConsoleType::Nds {
            self.nds_ppu.reset();
            let rom = self.nds_mmu.rom.clone();
            let _ = crate::nds::hle::boot_load_rom(&mut self.nds_mmu, &mut self.nds_arm9, &mut self.nds_arm7, &rom);
        } else {
            self.gba_cpu.boot(&mut self.gba_mmu);
        }
    }

    /// Persist battery-backed save RAM to disk immediately (GBC SRAM / GBA flash).
    /// Idempotent and dirty-gated: no-ops when no ROM is loaded, the path is
    /// empty, or nothing was written. Call before ROM teardown/switch and on app
    /// quit so in-game saves are never lost.
    pub fn flush_battery(&mut self) {
        if !self.rom_loaded || self.rom_path.as_os_str().is_empty() {
            return;
        }
        match self.console_type {
            crate::ffi::ConsoleType::Gbc => {
                if self.gbc_mmu.mbc.is_dirty
                    && self.gbc_mmu.mbc.save_sram(&self.rom_path, &self.base_dir).is_ok()
                {
                    self.gbc_mmu.mbc.is_dirty = false;
                }
            }
            crate::ffi::ConsoleType::Gba => {
                if self.gba_mmu.flash.is_dirty
                    && self
                        .gba_mmu
                        .flash
                        .save_flash_to_disk(&self.rom_path, &self.base_dir)
                        .is_ok()
                {
                    self.gba_mmu.flash.is_dirty = false;
                }
            }
            crate::ffi::ConsoleType::Nds => {
                if self.nds_mmu.backup.is_dirty() {
                    // save_to_disk clears the dirty flag itself on success, so a
                    // failed write stays pending and the next flush retries.
                    let _ = self
                        .nds_mmu
                        .backup
                        .save_to_disk(&self.rom_path, &self.base_dir);
                }
            }
            _ => {}
        }
    }

    pub fn tick(&mut self) {
        // Sample count for the placeholder audio fills below (paused silence, splash
        // silence, no-ROM mock beep). Real gameplay audio is produced by the APU
        // resampler and sized by `resampler.sample_count`, not by this.
        let placeholder_samples = std::cmp::min((735.0 * self.speed) as usize, 2940);
        if !self.is_playing {
            for i in 0..placeholder_samples * 2 {
                self.raw_audio_buffer[self.audio_offset + i] = 0;
            }
            return;
        }

        self.ticks += 1;

        // Periodic battery save (~every 600 ticks): dirty-gated, covers GBC SRAM,
        // GBA flash and the NDS cartridge backup chip. Bounds save-data loss to
        // ~10 frames on an unexpected exit.
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
            for i in 0..placeholder_samples * 2 {
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
        } else if self.console_type == crate::ffi::ConsoleType::Nds && self.rom_loaded {
            let base_cycles = 560190;
            let raw_budget = (base_cycles as f32 * self.speed) as u32;
            let cycle_budget = std::cmp::min(raw_budget, 5_000_000);

            self.poll_nds_touch_penirq();
            // Real APU output this tick: the resampler fills the buffer and
            // get_audio_buffer() reports its sample_count (same contract as GBA).
            self.nds_mmu.apu.resampler.sample_count = 0;
            let audio_off = self.audio_offset;
            let mut arm9_cycles_run = 0;

            while arm9_cycles_run < cycle_budget {
                // Fine ARM9/ARM7 interleave: the boot IPC handshake is a tight
                // IPCSYNC ping-pong where each core polls (with a short timeout)
                // for the other's reply; a coarse slice lets a poll time out
                // before the partner runs, stalling the handshake. 64 cycles keeps
                // them in lock-step through it.
                // ponytail: global fine interleave, not free at runtime — upgrade
                // to yield-on-IPCSYNC-write if this costs FPS in Release.
                let slice_9 = std::cmp::min(64, cycle_budget - arm9_cycles_run);
                let slice_7 = slice_9 / 2;

                let mut run_9 = 0;
                while run_9 < slice_9 {
                    let elapsed = self.nds_arm9.step(&mut self.nds_mmu);
                    run_9 += elapsed;
                }
                arm9_cycles_run += run_9;

                let mut run_7 = 0;
                while run_7 < slice_7 {
                    let elapsed = self.nds_arm7.step(&mut self.nds_mmu);
                    run_7 += elapsed;
                }

                // Timers on both cores clock at the 33.55 MHz bus, the same
                // unit run_9 is budgeted in (560190/frame ≈ 33.55 MHz / 60).
                self.nds_mmu.tick_nds_timers(run_9 as u32);
                self.nds_mmu
                    .tick_apu(run_9 as u32, &mut self.raw_audio_buffer, audio_off, self.speed);

                let vo = self.video_offset;
                let video_slice = &mut self.raw_video_buffer[vo..vo + 256 * 384];
                self.nds_ppu.tick(
                    run_9 as u32,
                    &mut self.nds_mmu,
                    video_slice,
                    is_render_tick,
                );

                if self.nds_ppu.frame_completed {
                    self.nds_ppu.frame_completed = false;
                    if is_render_tick {
                        self.present_frame();
                    }
                }
            }

            self.cpu_cycles = self.cpu_cycles.wrapping_add(arm9_cycles_run as u64);
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

    pub fn inject_input(&mut self, buttons: ButtonState) {
        self.buttons = buttons;
    }

    /// Active framebuffer length in u16 elements (BGR555 pixels) for the current console.
    fn active_video_len(&self) -> usize {
        match self.console_type {
            crate::ffi::ConsoleType::Nds => 256 * 384,
            crate::ffi::ConsoleType::Gba => 240 * 160,
            _ => 160 * 144,
        }
    }

    /// Publishes the completed back frame by swapping back/front buffers and their
    /// alignment offsets (O(1); replaces the previous full-frame memcpy).
    /// Invariant: only call on a tick that fully re-rendered the back buffer —
    /// after the swap the new back buffer holds a two-presents-old image.
    /// Sample the current input state into the NDS MMU: buttons for KEYINPUT/
    /// EXTKEYIN and the stylus for the SPI touchscreen controller. Pen-down
    /// reaches the game through EXTKEYIN bit 6 + TSC conversions, which the
    /// ARM7 driver POLLS every frame (measured: X/Y conversions run exactly
    /// while pressed) — the NDS has no pen IRQ line in IF; the former bit-22
    /// latch here was the "screens unfolding" (hinge) IRQ and risked a fake
    /// lid-open wake during sleep, so it was removed.
    fn poll_nds_touch_penirq(&mut self) {
        self.nds_mmu.buttons = self.buttons;
        self.nds_mmu.spi.tsc.touch_x = self.buttons.nds_touch_x;
        self.nds_mmu.spi.tsc.touch_y = self.buttons.nds_touch_y;
        self.nds_mmu.spi.tsc.touch_pressed = self.buttons.nds_touch_pressed;
    }

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
        // Paused: emit NOTHING rather than a stale block. tick() does not run while
        // paused, so a non-empty return would hand the frontend the same ~16.7 ms of
        // old samples every loop iteration — re-queued forever, it plays as a
        // perfectly periodic "robotic" loop (reachable via the --pause CLI path).
        if !self.is_playing {
            return &[];
        }
        let sample_count = if self.rom_loaded && self.state == EmulatorState::Gameplay && self.is_playing {
            match self.console_type {
                crate::ffi::ConsoleType::Gba => self.gba_mmu.apu.resampler.sample_count,
                crate::ffi::ConsoleType::Nds => self.nds_mmu.apu.resampler.sample_count,
                _ => self.gbc_mmu.apu.resampler.sample_count,
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
        match crate::rom::validate_and_parse_header(rom_data) {
            Ok(console) => {
                let max_size = if console == crate::ffi::ConsoleType::Nds { 128 * 1024 * 1024 } else { 32 * 1024 * 1024 };
                if rom_data.len() > max_size {
                    return false;
                }
                // Commit the outgoing cartridge's battery before its chip is
                // replaced below — the periodic flush runs only every 600 ticks,
                // so up to that much play is otherwise lost on a ROM switch.
                self.flush_battery();
                // A ROM loaded from memory has no file to persist beside. Clear
                // the persistence identity so a later flush cannot write this
                // cartridge's save over the PREVIOUS ROM's .sav.
                self.rom_path = std::path::PathBuf::new();
                self.base_dir = std::path::PathBuf::new();
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
                } else if console == crate::ffi::ConsoleType::Nds {
                    self.width = 256;
                    self.height = 384;
                    self.player_x = 128;
                    self.player_y = 192;
                    self.nds_mmu.rom = rom_data.to_vec();
                    // The cartridge still carries a save chip even when the ROM
                    // came from memory: leaving it absent makes AUXSPI read
                    // 0xFF, whose status byte has WIP set, so the game's
                    // wait-busy loop spins until it times out and never issues
                    // the READ that boot needs. It simply has nowhere to
                    // persist to — `rom_path` is cleared above, and
                    // `flush_battery` early-returns on an empty path, so this
                    // cartridge's contents can never reach another ROM's .sav.
                    self.nds_mmu.backup = crate::nds::backup::NdsBackup::new(
                        rom_data
                            .get(0x0C..0x10)
                            .map(crate::nds::backup::backup_kind_for_gamecode)
                            .unwrap_or_default(),
                    );
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

        let ext = safe_path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        let max_size = if ext == "nds" { 128 * 1024 * 1024 } else { 32 * 1024 * 1024 };
        if meta.len() > max_size {
            if ext == "nds" {
                return "LOAD_ROM_ERROR File size exceeds 128MB limit".to_string();
            } else {
                return "LOAD_ROM_ERROR File size exceeds 32MB limit".to_string();
            }
        }

        let data = match std::fs::read(&safe_path) {
            Ok(d) => d,
            Err(e) => return format!("LOAD_ROM_ERROR {}", e),
        };

        match crate::rom::validate_and_parse_header(&data) {
            Ok(console) => {
                // Commit the outgoing cartridge's battery while `rom_path` and
                // `console_type` still identify it; both are overwritten next.
                self.flush_battery();
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
                } else if console == crate::ffi::ConsoleType::Nds {
                    self.width = 256;
                    self.height = 384;
                    self.player_x = 128;
                    self.player_y = 192;
                    self.nds_mmu.rom = data.clone();
                    // Cartridge backup chip. NDS headers carry no save-type
                    // field (0x14 is ROM capacity), so the device comes from the
                    // gamecode at 0x0C and NEVER from the ROM or the .sav — see
                    // `backup::backup_kind_for_gamecode`.
                    let kind = data
                        .get(0x0C..0x10)
                        .map(crate::nds::backup::backup_kind_for_gamecode)
                        .unwrap_or_default();
                    self.nds_mmu.backup = crate::nds::backup::NdsBackup::new(kind);
                    let _ = self.nds_mmu.backup.load_from_disk(&safe_path, base);
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

    /// W1 evidence probe: drives real Crystal and reports the chain its
    /// per-scanline background effects ride on.
    ///
    /// Decoded from the ROM: the only `ldh [rSTAT]` enable write is 0x08
    /// (mode-0 only), and the STAT vector at 0x0048 jumps to 0x0552, which
    /// reads `wLYOverrides[LY]` (0xD100, banked WRAM) and stores it to the IO
    /// register named by `hLCDCPointer` (0xFFC6). Four of the eleven sites that
    /// set that pointer live in bank 0x32, the battle-animation bank.
    ///
    /// Reading order: mode-0 count != 144 puts the defect in the PPU mode
    /// machine; count == 144 with `hLCDCPointer` always zero means the effect
    /// is never armed and the fault is upstream of the PPU.
    #[test]
    #[ignore = "manual LCD probe; run with --ignored --nocapture"]
    fn crystal_scanline_effect_probe() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let rom = repo
            .join("roms")
            .join("Pokemon - Crystal Version (UE) (V1.1) [C][!].gbc");
        if !rom.exists() {
            eprintln!("SKIP: Crystal ROM absent at {}", rom.display());
            return;
        }
        let mut emu = Emulator::new();
        let msg = emu.load_rom_path(rom.to_str().unwrap(), repo.to_str().unwrap());
        assert!(!msg.starts_with("LOAD_ROM_ERROR"), "load failed: {msg}");
        emu.play();

        let mut armed_frames = 0u32;
        let mut pointers: std::collections::BTreeMap<u8, u32> = Default::default();
        let mut table_varied = 0u32;
        let ticks = 900u32; // ~15 s: title, intro, into the save file
        for _ in 0..ticks {
            emu.tick();
            // hLCDCPointer: non-zero means an LY-override effect is armed.
            let p = emu.gbc_mmu.read_byte(0xFFC6);
            if p != 0 {
                armed_frames += 1;
                *pointers.entry(p).or_default() += 1;
                // wLYOverrides at 0xD100: a varying table means a live effect,
                // a constant one means it is armed but inert.
                let first = emu.gbc_mmu.read_byte(0xD100);
                if (1..144u16).any(|i| emu.gbc_mmu.read_byte(0xD100 + i) != first) {
                    table_varied += 1;
                }
            }
        }
        let p = &emu.gbc_ppu;
        eprintln!(
            "GBC over {ticks} frames: stat_mode0_irq={} ({:.1}/frame, expect 144) reentrant={}",
            p.dbg_stat_mode0_irq,
            p.dbg_stat_mode0_irq as f64 / ticks as f64,
            p.dbg_stat_reentrant
        );
        let ps: Vec<String> = pointers
            .iter()
            .map(|(reg, n)| format!("{:#04x}x{n}", 0xFF00u16 + *reg as u16))
            .collect();
        eprintln!(
            "  hLCDCPointer armed on {armed_frames}/{ticks} frames, table varied on {table_varied}; targets: {}",
            if ps.is_empty() { "none".into() } else { ps.join(" ") }
        );
    }

    /// W1 evidence probe: drives real Emerald and reports DMA triggers per
    /// start timing.
    ///
    /// The decisive number is HBlank-timed triggers per frame. Hardware does
    /// not start HBlank DMA during VBlank, so a correct core fires on 160
    /// scanlines; without a VCOUNT gate this core fires on all 228, which
    /// mis-phases Emerald's whole scanline-effect engine by ~68 lines.
    #[test]
    #[ignore = "manual DMA probe; run with --ignored --nocapture"]
    fn emerald_dma_timing_probe() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let rom = repo
            .join("roms")
            .join("Pokemon - Emerald Version (USA, Europe).gba");
        if !rom.exists() {
            eprintln!("SKIP: Emerald ROM absent at {}", rom.display());
            return;
        }
        let mut emu = Emulator::new();
        let msg = emu.load_rom_path(rom.to_str().unwrap(), repo.to_str().unwrap());
        assert!(!msg.starts_with("LOAD_ROM_ERROR"), "load failed: {msg}");
        assert_eq!(emu.get_console_type(), crate::ffi::ConsoleType::Gba);
        emu.play();

        let ticks = 600u64;
        for _ in 0..ticks {
            emu.tick();
        }
        let m = &emu.gba_mmu;
        let per_frame = |n: u64| n as f64 / ticks as f64;
        eprintln!(
            "GBA DMA over {ticks} frames: immediate={} ({:.1}/frame) vblank={} ({:.1}/frame) hblank={} ({:.1}/frame) fifo={} ({:.1}/frame)",
            m.dbg_dma_trigger[0], per_frame(m.dbg_dma_trigger[0]),
            m.dbg_dma_trigger[1], per_frame(m.dbg_dma_trigger[1]),
            m.dbg_dma_trigger[2], per_frame(m.dbg_dma_trigger[2]),
            m.dbg_dma_trigger[3], per_frame(m.dbg_dma_trigger[3]),
        );
        eprintln!(
            "  hblank DMA fired on VCOUNT {}..={} (hardware: 0..=159)",
            m.dbg_hblank_dma_vcount_min, m.dbg_hblank_dma_vcount_max
        );
    }

    /// TEMP evidence probe (remove after audio debug): loads real Crystal, ticks the
    /// intro, prints double_speed state + 0.25 s RMS/peak/zero-cross-pitch envelope so
    /// "secciones mudas" (silent windows) and octave-up pitch are directly observable.
    #[test]
    #[ignore = "manual audio probe; run with --ignored --nocapture"]
    fn crystal_intro_audio_probe() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let rom = repo
            .join("roms")
            .join("Pokemon - Crystal Version (UE) (V1.1) [C][!].gbc");
        if !rom.exists() {
            eprintln!("SKIP: Crystal ROM absent at {}", rom.display());
            return;
        }

        let mut emu = Emulator::new();
        let msg = emu.load_rom_path(rom.to_str().unwrap(), repo.to_str().unwrap());
        assert!(!msg.starts_with("LOAD_ROM_ERROR"), "load failed: {msg}");
        assert_eq!(emu.get_console_type(), crate::ffi::ConsoleType::Gbc);
        emu.play();

        let mut left: Vec<i16> = Vec::new();
        let mut ds_per_frame: Vec<bool> = Vec::new();
        let mut spt: Vec<usize> = Vec::new();
        let ticks = 1500; // ~25 s
        for _ in 0..ticks {
            emu.tick();
            let ds = emu.gbc_cpu.double_speed;
            let buf = emu.get_audio_buffer();
            let frames = buf.len() / 2;
            spt.push(frames);
            for f in 0..frames {
                left.push(buf[f * 2]);
                ds_per_frame.push(ds);
            }
        }

        let total = left.len();
        let mean_spt = spt.iter().sum::<usize>() as f64 / ticks as f64;
        eprintln!("total frames={total} mean samples/tick={mean_spt:.1} (expect ~738)");

        let mut prev = false;
        for (i, &ds) in ds_per_frame.iter().enumerate() {
            if ds != prev {
                eprintln!("  double_speed -> {ds} at t={:.2}s", i as f64 / 44100.0);
                prev = ds;
            }
        }

        let win = 11025usize; // 0.25 s
        eprintln!("  t(s)  ds   rms   peak   zcrHz");
        let mut i = 0;
        while i + win <= total {
            let slice = &left[i..i + win];
            let ds = ds_per_frame[i];
            let (mut sumsq, mut peak, mut zc) = (0f64, 0i32, 0u32);
            let mut prev_s = 0i16;
            for (k, &s) in slice.iter().enumerate() {
                sumsq += (s as f64) * (s as f64);
                let a = (s as i32).abs();
                if a > peak {
                    peak = a;
                }
                if k > 0 && ((prev_s >= 0) != (s >= 0)) {
                    zc += 1;
                }
                prev_s = s;
            }
            let rms = (sumsq / win as f64).sqrt();
            let zcr_hz = zc as f64 / 2.0 / 0.25;
            eprintln!(
                "  {:4.2}  {}  {:5.0}  {:5}  {:6.0}",
                i as f64 / 44100.0,
                if ds { "D" } else { "." },
                rms,
                peak,
                zcr_hz
            );
            i += win;
        }
    }

    /// End-to-end audio regression guard (ROM-gated, `#[ignore]`): the Emerald intro must
    /// play *continuous* music. A PPU bug once fired the VBlank IRQ on every VBlank scanline
    /// (~68x/frame) instead of once per frame, so the game's VBlank-driven MP2K sound update
    /// ran many times per frame — songs raced to their end and left a ~23 s mid-intro silence
    /// (see the vblank rising-edge gate in gba/ppu.rs). This asserts no long silence gap once
    /// the intro music has started. Set EMU_PCM_OUT to also dump raw s16le stereo for manual
    /// tempo/spectral analysis.
    #[test]
    #[ignore = "manual audio probe; needs Emerald ROM; run with --ignored --nocapture"]
    fn emerald_intro_no_long_silence() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let rom = repo.join("roms").join("Pokemon - Emerald Version (USA, Europe).gba");
        if !rom.exists() {
            eprintln!("SKIP: Emerald ROM absent at {}", rom.display());
            return;
        }
        let mut emu = Emulator::new();
        let msg = emu.load_rom_path(rom.to_str().unwrap(), repo.to_str().unwrap());
        assert!(!msg.starts_with("LOAD_ROM_ERROR"), "load failed: {msg}");
        emu.play();
        emu.set_speed(1.0);

        // Per-0.5s-window RMS of the left channel across ~47 s of intro.
        const WIN_TICKS: usize = 30; // ~0.5 s at 59.73 fps
        let mut pcm: Vec<i16> = Vec::new();
        let mut window_rms: Vec<f64> = Vec::new();
        let (mut sumsq, mut n) = (0.0f64, 0u64);
        for tk in 0..2820usize {
            emu.tick();
            let buf = emu.get_audio_buffer();
            pcm.extend_from_slice(buf);
            for s in buf.iter().step_by(2) {
                sumsq += (*s as f64) * (*s as f64);
                n += 1;
            }
            if (tk + 1) % WIN_TICKS == 0 {
                window_rms.push(if n > 0 { (sumsq / n as f64).sqrt() } else { 0.0 });
                sumsq = 0.0;
                n = 0;
            }
        }

        if let Ok(path) = std::env::var("EMU_PCM_OUT") {
            let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
            std::fs::write(&path, &bytes).expect("write pcm");
            eprintln!("wrote {} stereo samples to {path}", pcm.len() / 2);
        }

        // "Music" = RMS above a small floor (silence is a true 0 here; music is ~300-2400).
        const FLOOR: f64 = 50.0;
        let first_music = window_rms.iter().position(|&r| r > FLOOR);
        assert!(first_music.is_some(), "intro produced no audible music at all");
        let start = first_music.unwrap();

        // Longest run of consecutive silent windows AFTER music has begun.
        let (mut longest, mut cur) = (0usize, 0usize);
        for &r in &window_rms[start..] {
            if r <= FLOOR {
                cur += 1;
                longest = longest.max(cur);
            } else {
                cur = 0;
            }
        }
        let longest_s = longest as f64 * WIN_TICKS as f64 / 59.7275;
        // Pre-fix this was ~23 s; a correctly playing intro has no multi-second gap.
        assert!(
            longest_s < 2.0,
            "intro music stalls: longest mid-intro silence {longest_s:.1}s \
             (music starts at window {start}); VBlank IRQ likely over-firing"
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

    #[test]
    fn test_nds_rom_booting_on_load() {
        let mut rom_data = vec![0u8; 528];
        
        // 1. Title
        let title = b"TESTNDSROM\0\0";
        rom_data[0..12].copy_from_slice(title);

        // 2. Offsets and sizes
        // ARM9 ROM offset = 512 (0x200), RAM address = 0x02000000, size = 4
        rom_data[0x20..0x24].copy_from_slice(&512u32.to_le_bytes());
        rom_data[0x24..0x28].copy_from_slice(&0x02000000u32.to_le_bytes());
        rom_data[0x28..0x2C].copy_from_slice(&0x02000000u32.to_le_bytes());
        rom_data[0x2C..0x30].copy_from_slice(&4u32.to_le_bytes());

        // ARM7 ROM offset = 516 (0x204), RAM address = 0x03800000, size = 4
        rom_data[0x30..0x34].copy_from_slice(&516u32.to_le_bytes());
        rom_data[0x34..0x38].copy_from_slice(&0x03800000u32.to_le_bytes());
        rom_data[0x38..0x3C].copy_from_slice(&0x03800000u32.to_le_bytes());
        rom_data[0x3C..0x40].copy_from_slice(&4u32.to_le_bytes());

        // 3. GBA logo
        rom_data[0x0C0..0x0C0 + 156].copy_from_slice(&crate::rom::GBA_LOGO[..]);

        // 4. Calculate header CRC
        let crc = {
            let mut crc = 0xFFFFu16;
            for i in 0..0x15C {
                crc ^= (rom_data[i] as u16) << 8;
                for _ in 0..8 {
                    if (crc & 0x8000) != 0 {
                        crc = (crc << 1) ^ 0x1021;
                    } else {
                        crc <<= 1;
                    }
                }
            }
            crc
        };
        rom_data[0x15C..0x15E].copy_from_slice(&crc.to_le_bytes());

        // 5. Binary contents (ARM9 code and ARM7 code)
        // At 512: ARM9 code (4 bytes)
        // At 516: ARM7 code (4 bytes)
        rom_data[512..516].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        rom_data[516..520].copy_from_slice(&[0x55, 0x66, 0x77, 0x88]);

        // Now load the ROM in the Emulator
        let mut emu = Emulator::new();
        let loaded = emu.load_rom(&rom_data);
        assert!(loaded, "Failed to load mock NDS ROM");

        // Verify console type is NDS
        assert_eq!(emu.get_console_type(), crate::ffi::ConsoleType::Nds);

        // Verify PC entry + that the binary copy occurred. gpr[15] reads
        // entry + 8 because boot primes the pipeline (the value real ARM
        // hardware exposes via R15); the entry instruction sits at `entry`.
        assert_eq!(emu.nds_arm9.cpu.registers.gpr[15], 0x02000008);
        assert_eq!(emu.nds_mmu.read_word_arm9(0x02000000), 0x44332211);

        assert_eq!(emu.nds_arm7.cpu.registers.gpr[15], 0x03800008);
        assert_eq!(emu.nds_mmu.read_word_arm7(0x03800000), 0x88776655);
    }

    #[test]
    fn test_nds_touch_sync_reaches_tsc_and_extkeyin_without_irq() {
        // The per-tick input sync must hand the stylus to the SPI TSC and to
        // EXTKEYIN (bit 6 active-low pen-down), and must NOT latch any IRQ:
        // the ARM7 driver polls — IF bit 22 is the hinge (lid-open) line, and
        // a spurious latch there would fake a wake-from-sleep event.
        let mut emu = Emulator::new();
        emu.nds_mmu.arm7_if = 0;
        emu.buttons.nds_touch_x = 215;
        emu.buttons.nds_touch_y = 170;
        emu.buttons.nds_touch_pressed = true;
        emu.poll_nds_touch_penirq();
        assert_eq!(emu.nds_mmu.spi.tsc.touch_x, 215);
        assert_eq!(emu.nds_mmu.spi.tsc.touch_y, 170);
        assert!(emu.nds_mmu.spi.tsc.touch_pressed);
        assert_eq!(emu.nds_mmu.get_extkeyin() & 0x40, 0, "pen-down drives EXTKEYIN bit 6 low");
        assert_eq!(emu.nds_mmu.arm7_if, 0, "touch must not latch any ARM7 IRQ");

        emu.buttons.nds_touch_pressed = false;
        emu.poll_nds_touch_penirq();
        assert!(!emu.nds_mmu.spi.tsc.touch_pressed);
        assert_ne!(emu.nds_mmu.get_extkeyin() & 0x40, 0, "pen-up releases bit 6");
        assert_eq!(emu.nds_mmu.arm7_if, 0);
    }

    // Evidence probe (M2): boot the real ROM and observe how far the ARM9 gets
    // and what it writes to the 2D engine, so the PPU work is driven by facts.
    // Run explicitly (needs the ROM present):
    //   cargo test --lib -- --ignored --nocapture nds_soulsilver_ppu_probe
    #[test]
    #[ignore]
    fn nds_soulsilver_ppu_probe() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../roms/Pokemon - SoulSilver Version (USA).nds"
        );
        let rom = match std::fs::read(path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("SKIP probe (no ROM): {e}");
                return;
            }
        };
        let mut emu = Emulator::new();
        assert!(emu.load_rom(&rom), "load_rom failed");
        emu.is_playing = true;
        let rd32 = |io: &[u8], b: usize| u32::from_le_bytes([io[b], io[b + 1], io[b + 2], io[b + 3]]);
        eprintln!(
            "console={:?} rom={} bytes  entry a9={:#010x} a7={:#010x}",
            emu.get_console_type(),
            rom.len(),
            emu.nds_arm9.cpu.registers.gpr[15],
            emu.nds_arm7.cpu.registers.gpr[15]
        );
        let h = |b: usize| u32::from_le_bytes([rom[b], rom[b + 1], rom[b + 2], rom[b + 3]]);
        eprintln!(
            "header ARM9: rom_off={:#x} entry={:#010x} ram={:#010x} size={:#x}",
            h(0x20), h(0x24), h(0x28), h(0x2C)
        );
        eprintln!(
            "header ARM7: rom_off={:#x} entry={:#010x} ram={:#010x} size={:#x}",
            h(0x30), h(0x34), h(0x38), h(0x3C)
        );
        eprintln!(
            "autoload: arm9_info(0x70)={:#x} arm7_info(0x74)={:#x} arm9_autoload(0x74)={:#x} arm7_autoload(0x78)={:#x}",
            h(0x70), h(0x74), h(0x74), h(0x78)
        );
        eprintln!("0x037f8000 RIGHT AFTER BOOT (before any execution):");
        for a in (0x037f_8000u32..0x037f_8020).step_by(4) {
            eprintln!("  {a:#010x}: {:08x}", emu.nds_mmu.read_word_arm7(a));
        }
        // Autoload table candidates: hdr 0x70=0x2000aac (ARM9 ram+0xaac → ROM
        // 0x4000+0xaac) and hdr 0x74=0x2380158 (ARM7 ram+0x158 → ROM 0x2f7400+0x158).
        // Valid autoload = triples {dest_in_RAM, size, bss_size}, dest=0 terminates.
        let dump_tbl = |name: &str, rom_base: usize| {
            eprintln!("{name}:");
            for i in 0..12usize {
                eprintln!("  +{:#04x}: {:#010x}", i * 4, h(rom_base + i * 4));
            }
        };
        dump_tbl("ARM9 autoload cand @ROM 0x4aac (hdr0x70)", 0x4000 + 0xaac);
        dump_tbl("ARM7 autoload cand @ROM 0x2f7558 (hdr0x74)", 0x2f7400 + 0x158);
        for frame in 0..200 {
            emu.tick();
            let da = rd32(&emu.nds_mmu.arm9_io, 0x000);
            let pal_nz = emu.nds_mmu.palette_ram.iter().any(|&b| b != 0);
            let vram_nz = (0..9).any(|i| emu.nds_mmu.vram.banks[i].data.iter().any(|&b| b != 0));
            let progressed = da != 0 || pal_nz || vram_nz;
            if frame % 20 == 0 || progressed {
                eprintln!(
                    "f{frame}: a9={:#010x} a7={:#010x} DISPCNT_A={da:#010x} pal_nz={pal_nz} vram_nz={vram_nz} 3d={} a9if={:#x} a7if={:#x}",
                    emu.nds_arm9.cpu.registers.gpr[15],
                    emu.nds_arm7.cpu.registers.gpr[15],
                    emu.nds_mmu.has_3d_activity,
                    emu.nds_mmu.arm9_if,
                    emu.nds_mmu.arm7_if
                );
            }
            if progressed {
                eprintln!("*** graphics state changed at frame {frame} ***");
                break;
            }
        }
        let a9pc = emu.nds_arm9.cpu.registers.gpr[15] & !3;
        eprintln!("--- ARM9 stall loop around pc={a9pc:#010x} ---");
        for a in (a9pc.saturating_sub(0x2c)..a9pc + 0x2c).step_by(4) {
            eprintln!("  {a:#010x}: {:08x}", emu.nds_mmu.read_word_arm9(a));
        }
        // Disassemble the call site that jumped into the deliberate hang
        // (immediate caller from the stack walk = 0x020ddbb4, so bl@0x020ddbb0) and
        // its own caller, to see the condition/branch that led to the fatal halt.
        eprintln!("--- ARM9 hang call site @~0x020ddbb0 (immediate caller) ---");
        for a in (0x020d_db70u32..0x020d_dbc4).step_by(4) {
            eprintln!("  {a:#010x}: {:08x}", emu.nds_mmu.read_word_arm9(a));
        }
        eprintln!("--- ARM9 RTOS decision @0x020ddc00..0x020ddc48 (post IPC-send -> panic) ---");
        for a in (0x020d_dc00u32..0x020d_dc48).step_by(4) {
            eprintln!("  {a:#010x}: {:08x}", emu.nds_mmu.read_word_arm9(a));
        }
        // Full send+wait+status body: the error path decides the RTOS panic, and
        // the interesting checks (IPCFIFOCNT bit14, readiness bitmask, timeout)
        // sit PAST 0x020d6714 — dump wide enough to hand-disassemble all of it.
        eprintln!("--- ARM9 IPC cmd-14 fn @0x020d6680..0x020d6880 (bl 0x020d66c4 from RTOS 0x020ddc38; err -> panic) ---");
        for a in (0x020d_6680u32..0x020d_6880).step_by(4) {
            eprintln!("  {a:#010x}: {:08x}", emu.nds_mmu.read_word_arm9(a));
        }
        // Panic-chain call sites (from the tick-faithful RTOS-entry log):
        // 0x020d1ec4 BLs the panic entry 0x020dd920; 0x020d4308 BLs the terminate
        // family 0x020ddb3c..54; 0x020db330 is polled ==4 in the pre-hang loop.
        // Dump them all for hand-disassembly of the failing check.
        for (name, lo, hi) in [
            ("root-cause caller @0x020d1e40..0x020d1f60 (BL 0x020dd920 from 0x020d1ec0)", 0x020d_1e40u32, 0x020d_1f60u32),
            ("terminate caller @0x020d4280..0x020d4360 (BL 0x020ddb3c from 0x020d4304)", 0x020d_4280, 0x020d_4360),
            ("panic svc @0x020dd900..0x020dd960", 0x020d_d900, 0x020d_d960),
            ("terminate entries @0x020ddb30..0x020ddb70", 0x020d_db30, 0x020d_db70),
            ("polled-state fn @0x020db320..0x020db3a0 (loop while ==4)", 0x020d_b320, 0x020d_b3a0),
        ] {
            eprintln!("--- {name} ---");
            for a in (lo..hi).step_by(4) {
                eprintln!("  {a:#010x}: {:08x}", emu.nds_mmu.read_word_arm9(a));
            }
        }
        // The word 0x021e3820 recurs on the stack (a context/message pointer passed
        // through the panic subsystem). Dump it as words + ASCII: if it's an assert
        // message or a structured reason, it names what failed.
        eprintln!("--- [0x021e3820..0x021e38a0] (recurring panic context ptr) ---");
        for a in (0x021e_3820u32..0x021e_38a0).step_by(16) {
            let bytes: Vec<u8> = (0..16).map(|i| emu.nds_mmu.read_byte_arm9(a + i)).collect();
            let ascii: String = bytes.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
            let words: Vec<String> = (0..4).map(|i| format!("{:08x}", u32::from_le_bytes([bytes[i*4], bytes[i*4+1], bytes[i*4+2], bytes[i*4+3]]))).collect();
            eprintln!("  {a:#010x}: {} |{ascii}|", words.join(" "));
        }
        {
            let r = emu.nds_arm9.cpu.registers.gpr;
            let cpsr = emu.nds_arm9.cpu.registers.cpsr;
            eprintln!(
                "  ARM9 regs: r0={:#x} r1={:#010x} r2={:#010x} r3={:#010x} r4={:#010x} r5={:#010x} r6={:#010x} sp={:#010x} lr={:#010x} cpsr={cpsr:#010x} I={} T={}",
                r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[13], r[14],
                (cpsr >> 7) & 1, (cpsr >> 5) & 1
            );
            // Walk the ARM9 stack: the hang function's prologue pushed its return
            // address, so the code-looking words (0x02xxxxxx) up the stack are the
            // caller chain — this reveals HOW the ARM9 reached the deliberate hang
            // (an error/assert path vs a normal "boot done, wait" path).
            eprintln!("  ARM9 stack walk (sp..sp+0x180, tag return-addrs by region):");
            for off in (0..0x180u32).step_by(4) {
                let w = emu.nds_mmu.read_word_arm9(r[13].wrapping_add(off));
                // Tag panic-subsystem frames vs "normal" game code, so the entry into
                // the panic path (a non-0x020dd / non-0x020d3 return addr) stands out.
                let tag = if (0x020d_d000..0x020d_e000).contains(&w) {
                    " <-panic-subsys"
                } else if (0x020d_3000..0x020d_4000).contains(&w) {
                    " <-hang/critsec"
                } else if (0x0200_0000..0x0240_0000).contains(&w) {
                    " <-NORMAL-CODE?"
                } else {
                    ""
                };
                eprintln!("    [sp+{off:#04x}] = {w:#010x}{tag}");
            }
            // The stall loop polls a status word [r2] (bit31=busy, bit23=word-ready)
            // and streams from data port [r1]: the signature of the NDS cartridge
            // (Gamecard) ROMCTRL protocol. Show the register addresses to confirm.
            eprintln!(
                "  stream: r1(data port)={:#010x} r2(status)={:#010x} [r2]={:#010x}",
                r[1], r[2],
                if r[2] < 0x1000_0000 { emu.nds_mmu.read_word_arm9(r[2]) } else { 0 }
            );
            // Gamecard command buffer (0x040001A8..0x040001AF, 8 bytes, MSB-first) +
            // ROMCTRL block-size field (bits 26-24) — decode exactly what block-read
            // SoulSilver issues so the cart protocol can be implemented precisely.
            let romctrl = u32::from_le_bytes(emu.nds_mmu.arm9_io[0x1A4..0x1A8].try_into().unwrap());
            let cmd = &emu.nds_mmu.arm9_io[0x1A8..0x1B0];
            let bs_field = (romctrl >> 24) & 0x7;
            let block = match bs_field { 0 => 0, 7 => 4, n => 0x100u32 << n };
            eprintln!(
                "  GAMECARD: ROMCTRL={romctrl:#010x} block_size_field={bs_field}(={block}B) cmd=[{:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}] rom_len={}",
                cmd[0], cmd[1], cmd[2], cmd[3], cmd[4], cmd[5], cmd[6], cmd[7], emu.nds_mmu.rom.len()
            );
            // The stall polls bit31 of [r5]; show what it points at (the hardware
            // register that never clears) to name the missing peripheral.
            eprintln!(
                "  [r5]={:#010x} (r5 region {:#04x})  [r4]={:#010x}",
                emu.nds_mmu.read_word_arm9(r[5]),
                (r[5] >> 24) & 0xFF,
                emu.nds_mmu.read_word_arm9(r[4])
            );
        }
        eprintln!("--- ARM7 relocation target 0x037f8000 (both-cores run) ---");
        for a in (0x037f_8000u32..0x037f_8030).step_by(4) {
            eprintln!("  {a:#010x}: {:08x}", emu.nds_mmu.read_word_arm7(a));
        }
        let a7pc = emu.nds_arm7.cpu.registers.gpr[15] & !1;
        eprintln!("--- ARM7 loop around pc={a7pc:#010x} ---");
        for a in (a7pc.saturating_sub(0x30)..a7pc + 0x1c).step_by(4) {
            eprintln!("  {a:#010x}: {:08x}", emu.nds_mmu.read_word_arm7(a));
        }
        eprintln!(
            "  ipc a7->a9={:#x} a9->a7={:#x} arm7_ime={:#x} arm7_ie={:#x} arm7_if={:#x} arm9_ime={:#x} arm9_ie={:#x} arm9_if={:#x}",
            emu.nds_mmu.ipc.arm7_to_arm9_sync,
            emu.nds_mmu.ipc.arm9_to_arm7_sync,
            emu.nds_mmu.arm7_ime,
            emu.nds_mmu.arm7_ie,
            emu.nds_mmu.arm7_if,
            emu.nds_mmu.arm9_ime,
            emu.nds_mmu.arm9_ie,
            emu.nds_mmu.arm9_if
        );
        eprintln!(
            "  IPCFIFO at stall: CNT9={:#06x} CNT7={:#06x} rawctl9={:#06x} rawctl7={:#06x} len9to7={} len7to9={}",
            emu.nds_mmu.read_ipc_fifo_cnt_arm9(),
            emu.nds_mmu.read_ipc_fifo_cnt_arm7(),
            emu.nds_mmu.ipc.fifo_control_arm9,
            emu.nds_mmu.ipc.fifo_control_arm7,
            emu.nds_mmu.ipc.fifo_9to7.len(),
            emu.nds_mmu.ipc.fifo_7to9.len()
        );
        {
            let dispstat7 = ((emu.nds_mmu.arm7_io[5] as u16) << 8) | emu.nds_mmu.arm7_io[4] as u16;
            let dispstat9 = ((emu.nds_mmu.arm9_io[5] as u16) << 8) | emu.nds_mmu.arm9_io[4] as u16;
            eprintln!(
                "  ARM7 halted={} pc={:#010x}  DISPSTAT7={dispstat7:#06x}(vbl-irq-en bit3={}) DISPSTAT9={dispstat9:#06x}(bit3={})",
                emu.nds_arm7.cpu.halted,
                emu.nds_arm7.cpu.registers.gpr[15],
                (dispstat7 >> 3) & 1,
                (dispstat9 >> 3) & 1,
            );
        }
        // TICK-FAITHFUL trace: reproduce tick()'s exact ARM9(64)/ARM7(32)/PPU
        // cadence (the 2:1 no-PPU derail hunt below DIVERGES from real execution),
        // stepping the ARM9 one instruction at a time. Record every crossing INTO
        // the RTOS subsystem (0x020dd000..) with its return address, then trap the
        // first time the ARM9 reaches the deliberate hang (0x020d3f48) and dump the
        // recent callers — the last non-RTOS `lr` is the normal code that triggered
        // the fatal path.
        eprintln!("--- TICK-FAITHFUL RTOS/hang entry trace (+ IPC FIFO event log) ---");
        {
            // FIFO state fingerprint watched around EVERY instruction on BOTH
            // cores: (len 9->7, head, last, len 7->9, head, last, ctl9, ctl7,
            // ARM9-IF ipc bits 16-18, ARM7-IF ipc bits 16-18). Any change is
            // logged with the pc that caused it — this shows the whole cmd-14
            // exchange: who enables what, every push/pop with its value, every
            // error-latch (bit14 of ctl) and every IPC IRQ raise/ack.
            fn snap(
                m: &crate::nds::mmu::NdsMmu,
            ) -> (usize, u32, u32, usize, u32, u32, u16, u16, u32, u32) {
                (
                    m.ipc.fifo_9to7.len(),
                    m.ipc.fifo_9to7.first().copied().unwrap_or(0),
                    m.ipc.fifo_9to7.last().copied().unwrap_or(0),
                    m.ipc.fifo_7to9.len(),
                    m.ipc.fifo_7to9.first().copied().unwrap_or(0),
                    m.ipc.fifo_7to9.last().copied().unwrap_or(0),
                    m.ipc.fifo_control_arm9,
                    m.ipc.fifo_control_arm7,
                    (m.arm9_if >> 16) & 7,
                    (m.arm7_if >> 16) & 7,
                )
            }
            type Snap = (usize, u32, u32, usize, u32, u32, u16, u16, u32, u32);
            fn delta(b: &Snap, a: &Snap) -> String {
                let mut d = String::new();
                if b.0 != a.0 {
                    let (verb, val) = if a.0 > b.0 { ("push", a.2) } else { ("pop", b.1) };
                    d += &format!(" 9to7:{}->{}({verb}={val:#010x})", b.0, a.0);
                }
                if b.3 != a.3 {
                    let (verb, val) = if a.3 > b.3 { ("push", a.5) } else { ("pop", b.4) };
                    d += &format!(" 7to9:{}->{}({verb}={val:#010x})", b.3, a.3);
                }
                if b.6 != a.6 {
                    d += &format!(" ctl9:{:#06x}->{:#06x}", b.6, a.6);
                }
                if b.7 != a.7 {
                    d += &format!(" ctl7:{:#06x}->{:#06x}", b.7, a.7);
                }
                if b.8 != a.8 {
                    d += &format!(" if9ipc:{:#x}->{:#x}", b.8, a.8);
                }
                if b.9 != a.9 {
                    d += &format!(" if7ipc:{:#x}->{:#x}", b.9, a.9);
                }
                d
            }
            // Bounded two-window log: verbatim head (init/enable order) + tail
            // ring (the exchange right before the hang) — full trace would be GBs.
            struct EvLog {
                head: Vec<String>,
                tail: Vec<String>,
                n: usize,
            }
            impl EvLog {
                fn push(&mut self, s: String) {
                    self.n += 1;
                    if self.head.len() < 150 {
                        self.head.push(s);
                        return;
                    }
                    self.tail.push(s);
                    if self.tail.len() > 400 {
                        self.tail.remove(0);
                    }
                }
                fn dump(&self) {
                    eprintln!(
                        "  IPC FIFO events: {} total (showing first {} + last {}):",
                        self.n,
                        self.head.len(),
                        self.tail.len()
                    );
                    for s in &self.head {
                        eprintln!("    {s}");
                    }
                    let shown = self.head.len() + self.tail.len();
                    if self.n > shown {
                        eprintln!("    ... ({} events omitted) ...", self.n - shown);
                    }
                    for s in &self.tail {
                        eprintln!("    {s}");
                    }
                }
            }
            // Gamecard/DMA visibility: ROMCTRL word + the 4 ARM9 DMA CNT words.
            // Overlay/asset loads go through FS -> CARD -> (PIO or NDS-slot DMA
            // timing); if the game arms a non-immediate DMA we never run, its
            // read buffer stays zeroed and the ARM9 jumps into empty memory.
            fn snap_hw(m: &crate::nds::mmu::NdsMmu) -> (u32, u32, u32, u32, u32) {
                let r = |b: usize| {
                    u32::from_le_bytes([
                        m.arm9_io[b],
                        m.arm9_io[b + 1],
                        m.arm9_io[b + 2],
                        m.arm9_io[b + 3],
                    ])
                };
                (r(0x1A4), r(0xB8), r(0xC4), r(0xD0), r(0xDC))
            }
            let mut ev = EvLog { head: Vec::new(), tail: Vec::new(), n: 0 };
            let mut emu4 = Emulator::new();
            assert!(emu4.load_rom(&rom));
            let mut vo_buf = vec![0u16; 256 * 384];
            let mut entries: Vec<(u32, u32)> = Vec::new(); // (entry_pc, lr caller)
            let mut ring: Vec<u32> = Vec::new();
            // (resolved exec addr, instr, thumb, was_flush_step, sp) — resolved
            // means: after a jump (pc_modified) the true exec addr is the flush
            // target gpr15, not gpr15-4/-8; recording it kills the pipeline
            // artifacts that made return targets ambiguous in earlier traces.
            let mut ring2: Vec<(u32, u32, bool, bool, u32)> = Vec::new();
            let mut prev_in_rtos = false;
            let mut reached = false;
            let mut zero_trapped = false;
            // The fatal POP @0x02025ebc pops a corrupted return address from
            // [0x027e3744] (deterministic run). Watch that slot: whoever writes
            // the garbage 0x027e0080 there is the root cause.
            let mut watch_prev = 0u32;
            let mut watch_logs = 0u32;
            // The Timer0 alarm dispatcher BLXes through [0x02225024], which by
            // f207 holds ASCII file data. Catch every writer of that word.
            let mut watch2_prev = 0u32;
            let mut watch2_logs = 0u32;
            // Does SoulSilver's ARM9/ARM7 call BIOS SWIs we HLE as no-ops?
            let mut swi9: Vec<(u32, u32)> = Vec::new(); // (pc, comment)
            let mut swi7: Vec<(u32, u32)> = Vec::new();
            let mut swi9_n = 0u64;
            let mut swi7_n = 0u64;
            // Trap the panic subsystem entries with their ARGUMENTS: OS_Panic-style
            // fns carry an error code / message pointer — deref anything that looks
            // like a main-RAM pointer as ASCII to name the root cause directly.
            let mut panic_dumps = 0u32;
            let mut decomp_dumps = 0u32;
            let mut gcsrc_prev = 0u32;
            let mut watch3_prev = 0u32;
            let mut sm_logs = 0u32;
            let mut sm_last_frame = u32::MAX;
            let mut st4_dumps = 0u32;
            let mut watch5_prev = 0u8;
            let mut q_last_frame = u32::MAX;
            let mut watch5_logs = 0u32;
            let mut reply_dumps = 0u32;
            let mut replygen_dumps = 0u32;
            let mut jobcb_dumps = 0u32;
            let mut bootbuf_prev = 0u16;
            let mut bootbuf_logs = 0u32;
            let mut spin_lrs: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
            let mut strb_dumps = 0u32;
            // Steady-state profile: where do both cores spend time once boot
            // settles? Sampled 1-in-4 from frame 350; top sites printed at end.
            let mut prof9: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
            let mut prof7: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
            let mut prof_n = 0u64;
            let mut prof_n7 = 0u64;
            // Timer arm/disarm trace (who starts/stops each channel, when) and
            // the sound-ready flag writer hunt ([0x021d43cc], either core).
            let mut tctl9_prev = [0u16; 4];
            let mut tctl7_prev = [0u16; 4];
            let mut tctl_ring: Vec<String> = Vec::new();
            let mut watch4_prev = 0u32;
            // ARM7 IRQ health: log CPSR mode/I transitions (does it EVER take an
            // IRQ? does the handler return and restore I=0?).
            let mut a7_prev_cpsr = emu4.nds_arm7.cpu.registers.cpsr;
            let mut a7_cpsr_events = 0u32;
            // U21: the post-GX stall polls [0x021dd404] — log every writer.
            let mut watch6_prev = 0u32;
            let mut watch6_logs = 0u32;
            // U21 audio end-to-end: run the APU at the same cadence tick() does
            // and measure RMS/peak of the mixed 44.1 kHz output; BGM key-ons
            // show up as a nonzero peak with its first frame recorded.
            let mut abuf = vec![0i16; 16384];
            let mut rms_acc = 0f64;
            let mut rms_n = 0u64;
            let mut audio_peak = 0i32;
            let mut audio_first_frame = u32::MAX;
            // U25: PSG usage evidence — does the BGM ever key on format-3
            // (wave/noise) channels, and how many at once?
            let mut psg_first_frame = u32::MAX;
            let mut psg_max_live = 0usize;
            // U25 run 2b: touch-only input (no START/A) to prove the title
            // advances on a tap alone; frame hashes around the taps show the
            // scene changing. Toggled by env so both runs share one build.
            let touch_only = std::env::var("PROBE_TOUCH_ONLY").is_ok();
            // U34: PROBE_INGAME drives the A-mash tap schedule that reaches
            // the overworld (bedroom), running long enough and dumping full
            // layer evidence there — the user's chars/furniture bug lives
            // in-game, past the intro the shorter runs stop at.
            let ingame = std::env::var("PROBE_INGAME").is_ok();
            let frame_cap: u32 = if ingame { 17000 } else { 7800 };
            let mut band_dumps = 0u32;
            // U27: TP-struct candidate addresses found by the mid-hold RAM
            // scan; re-dumped after pen release.
            let mut tp_hit_addrs: Vec<u32> = Vec::new();
            let mut tp_snap_ring: Vec<u8> = Vec::new();
            let mut tp_snap_hot: Vec<u8> = Vec::new();
            let mut calib_pc_logs = 0u32;
            // GX trace: skip the boot's PUSH/IDENTITY/POP init flood; arm at
            // the title screen so the trace holds Lugia's real display list.
            emu4.nds_mmu.gx.trace_on = false;
            // U27l: trap every write into the TP calibrate-param struct from
            // boot onward — the values + frame identify the zero source.
            emu4.nds_mmu.calib_watch_on = true;
            // U27m: watch the boot window — settings-copy reads + AUXSPI
            // traffic around the f36-f38 zero-calibration writes.
            emu4.nds_mmu.tp_read_watch_on = true;
            // U27p: which firmware-flash pages does the ARM7 read at boot?
            emu4.nds_mmu.spi.firmware.log_on = true;
            'trace: for frame in 0..frame_cap {
                if frame == 1 {
                    // Did the boot HLE's settings copy survive to frame 1?
                    let cal: Vec<String> = (0..12)
                        .map(|k| format!("{:02x}", emu4.nds_mmu.main_ram[0x3FFC80 + 0x58 + k]))
                        .collect();
                    eprintln!("  RAMSETTINGS f1 cal@0x027FFCD8: {}", cal.join(" "));
                }
                if frame < 300 && !emu4.nds_mmu.spi.firmware.read_log.is_empty() {
                    let fr: Vec<String> = emu4
                        .nds_mmu
                        .spi
                        .firmware
                        .read_log
                        .drain(..)
                        .map(|(a, b)| format!("{a:#x}={b:02x}"))
                        .collect();
                    eprintln!("  FLASHREAD f{frame}: {}", fr.join(" "));
                }
                // U27s: the SDK's CRC16 routine (Thumb fn at 0x038008F4,
                // resolved via the validator's veneer) — dump for disasm.
                if frame == 42 {
                    let words: Vec<String> = (0..0x60)
                        .map(|k| {
                            let o = 0x8C0 + k * 4;
                            format!(
                                "{:08x}",
                                u32::from_le_bytes([
                                    emu4.nds_mmu.arm7_wram[o],
                                    emu4.nds_mmu.arm7_wram[o + 1],
                                    emu4.nds_mmu.arm7_wram[o + 2],
                                    emu4.nds_mmu.arm7_wram[o + 3],
                                ])
                            )
                        })
                        .collect();
                    eprintln!("  A7CRCFN@0x038008c0: {}", words.join(" "));
                }
                // U27o: per-frame timing of settings-copy reads (which frame
                // read the calibration fields, relative to the f36-38 zero
                // param writes).
                if frame < 100 && !emu4.nds_mmu.settings_read_log.is_empty() {
                    let sr: Vec<String> = emu4
                        .nds_mmu
                        .settings_read_log
                        .drain(..)
                        .map(|a| format!("{a:#x}"))
                        .collect();
                    eprintln!("  SETREAD f{frame}: {}", sr.join(" "));
                }
                // U28d: the TP init + defaults-decision code (writer pcs
                // 0x020da084-110, caller lr 0x020da01c) — the branch that
                // chose defaults over reading the settings copy lives here.
                if frame == 42 {
                    let words: Vec<String> = (0..0xA0)
                        .map(|k| {
                            format!(
                                "{:08x}",
                                emu4.nds_mmu.read_word_arm9(0x020d_9f00 + k * 4)
                            )
                        })
                        .collect();
                    eprintln!("  TPINITCODE@0x020d9f00: {}", words.join(" "));
                }
                // U27q: find the ARM7's settings-copy writer/validator — scan
                // ARM7 WRAM + shared WRAM for literals of the copy base and
                // dump code around each hit (the validator's real checks are
                // a few instructions above its 0x027FFC80 store loop).
                if frame == 42 {
                    for (name, buf, base) in [
                        ("arm7_wram", &emu4.nds_mmu.arm7_wram, 0x0380_0000u32),
                        ("shared_wram", &emu4.nds_mmu.shared_wram, 0x037F_8000u32),
                    ] {
                        for target in [0x027F_FC80u32, 0x027F_FCD8] {
                            let mut hits = Vec::new();
                            for i in (0..buf.len().saturating_sub(4)).step_by(4) {
                                if u32::from_le_bytes([
                                    buf[i],
                                    buf[i + 1],
                                    buf[i + 2],
                                    buf[i + 3],
                                ]) == target
                                    && hits.len() < 6
                                {
                                    hits.push(base + i as u32);
                                }
                            }
                            for &h in &hits {
                                // 1 KB before the pool: ARM functions keep
                                // their literals at the end, so the loader
                                // instruction and the validator body precede.
                                let start = ((h - base) as usize).saturating_sub(0x400);
                                let words: Vec<String> = (0..0x110)
                                    .map(|k| {
                                        let o = (start + k * 4).min(buf.len() - 4);
                                        format!(
                                            "{:08x}",
                                            u32::from_le_bytes([
                                                buf[o],
                                                buf[o + 1],
                                                buf[o + 2],
                                                buf[o + 3],
                                            ])
                                        )
                                    })
                                    .collect();
                                eprintln!(
                                    "  A7SETREF {name} lit@{h:#010x} (code from {:#010x}): {}",
                                    base + start as u32,
                                    words.join(" ")
                                );
                            }
                        }
                    }
                }
                // U27o: find TP_SetCalibrateParam's CALLER — scan for ARM BL
                // and Thumb BL/BLX instructions targeting 0x020daaa8, then
                // dump the caller bodies (the param computation + its data
                // source live there).
                if frame == 40 {
                    let target = 0x020d_aaa8u32;
                    let mut hits: Vec<u32> = Vec::new();
                    for a in (0x0200_0000u32..0x0228_0000).step_by(2) {
                        if hits.len() >= 8 {
                            break;
                        }
                        let o = (a & 0x00FF_FFFF) as usize;
                        let h1 = u16::from_le_bytes([
                            emu4.nds_mmu.main_ram[o],
                            emu4.nds_mmu.main_ram[o + 1],
                        ]);
                        // ARM BL (word-aligned): 0xEB imm24.
                        if a & 3 == 0 {
                            let w = u32::from_le_bytes([
                                emu4.nds_mmu.main_ram[o],
                                emu4.nds_mmu.main_ram[o + 1],
                                emu4.nds_mmu.main_ram[o + 2],
                                emu4.nds_mmu.main_ram[o + 3],
                            ]);
                            if w >> 24 == 0xEB {
                                let imm = ((w & 0x00FF_FFFF) as i32) << 8 >> 8;
                                if a.wrapping_add(8).wrapping_add((imm as u32) << 2) == target {
                                    hits.push(a);
                                    continue;
                                }
                            }
                        }
                        // Thumb BLX pair: F000-F7FF then E800-EFFF (word target).
                        if (0xF000..0xF800).contains(&h1) {
                            let h2 = u16::from_le_bytes([
                                emu4.nds_mmu.main_ram[o + 2],
                                emu4.nds_mmu.main_ram[o + 3],
                            ]);
                            if (0xE800..0xF000).contains(&h2) {
                                let off = ((((h1 & 0x7FF) as i32) << 21 >> 21) << 12)
                                    + (((h2 & 0x7FF) as i32) << 1);
                                let t = (a.wrapping_add(4).wrapping_add(off as u32)) & !3;
                                if t == target {
                                    hits.push(a);
                                }
                            }
                        }
                    }
                    for &h in &hits {
                        let words: Vec<String> = (0..24)
                            .map(|k| {
                                format!(
                                    "{:08x}",
                                    emu4.nds_mmu.read_word_arm9(h.wrapping_sub(0x40) + k * 4)
                                )
                            })
                            .collect();
                        eprintln!("  TPSETCALLER@{h:#010x}-0x40: {}", words.join(" "));
                    }
                    if hits.is_empty() {
                        eprintln!("  TPSETCALLER: no BL/BLX callers found in 0x02000000-0x02100000");
                    }
                }
                if frame == 60 {
                    emu4.nds_mmu.tp_read_watch_on = false;
                    let sr: Vec<String> = emu4
                        .nds_mmu
                        .settings_read_log
                        .iter()
                        .map(|a| format!("{a:#010x}"))
                        .collect();
                    eprintln!("  BOOTWATCH f60: settings-copy reads: {}", sr.join(" "));
                    let al: Vec<String> = emu4
                        .nds_mmu
                        .aux_log
                        .iter()
                        .map(|(d, v)| format!("{}{v:02x}", if *d == 0 { "w" } else { "r" }))
                        .collect();
                    eprintln!("  BOOTWATCH f60: auxspi: {}", al.join(" "));
                    emu4.nds_mmu.tp_slot_reads = [0; 6];
                }
                if !emu4.nds_mmu.calib_write_log.is_empty() {
                    let entries: Vec<String> = emu4
                        .nds_mmu
                        .calib_write_log
                        .drain(..)
                        .map(|(a, v)| format!("[{a:#010x}]={v:08x}"))
                        .collect();
                    eprintln!("  CALIBWRITE f{frame}: {}", entries.join(" "));
                }
                if frame == 2050 {
                    // U32: capture the flyover's display list — why are the
                    // villagers (3D geometry; OBJ is disabled here) missing?
                    emu4.nds_mmu.gx.trace_on = true;
                }
                if (2900..=3300).contains(&frame) {
                    // U31e: self-locating band detector — the Unown-swarm
                    // beat draws a striped garbage band for ~25 frames whose
                    // exact frame number shifts vs exe ticks; find it by its
                    // signature (a flood of pure-white pixels on the top
                    // screen) and dump the state the moment it appears.
                    let mut fbx = vec![0u16; 256 * 384];
                    for ly in 0..192u16 {
                        emu4.nds_ppu.render_scanline(ly, &emu4.nds_mmu, &mut fbx);
                    }
                    let white =
                        fbx[..256 * 192].iter().filter(|&&p| p & 0x7FFF == 0x7FFF).count();
                    if white > 200 {
                        eprintln!("  BANDSCAN f{frame}: white={white}");
                    }
                    if (3100..=3220).contains(&frame) && frame % 4 == 0 {
                        // U31g: the band exists only on the per-scanline
                        // (exe) render path -> mid-frame masking suspected.
                        // Dump the window + display state either way.
                        let io = &emu4.nds_mmu.arm9_io;
                        let rh2 = |b: usize| u16::from_le_bytes([io[b], io[b + 1]]);
                        eprintln!(
                            "  WINB f{frame}: dispB_win={:03b} win0h={:04x} win1h={:04x} win0v={:04x} win1v={:04x} winin={:04x} winout={:04x} | dispA_win={:03b} A:{:04x} {:04x} {:04x} {:04x} {:04x} {:04x}",
                            (u32::from_le_bytes([io[0x1000], io[0x1001], io[0x1002], io[0x1003]]) >> 13) & 7,
                            rh2(0x1040), rh2(0x1042), rh2(0x1044), rh2(0x1046),
                            rh2(0x1048), rh2(0x104A),
                            (u32::from_le_bytes([io[0x000], io[0x001], io[0x002], io[0x003]]) >> 13) & 7,
                            rh2(0x040), rh2(0x042), rh2(0x044), rh2(0x046),
                            rh2(0x048), rh2(0x04A),
                        );
                    }
                    if white > 1200 && band_dumps < 3 {
                        band_dumps += 1;
                        let mut ppm = b"P6\n256 384\n255\n".to_vec();
                        for &px in &fbx {
                            ppm.extend_from_slice(&[
                                ((px & 0x1F) << 3) as u8,
                                (((px >> 5) & 0x1F) << 3) as u8,
                                (((px >> 10) & 0x1F) << 3) as u8,
                            ]);
                        }
                        let _ = std::fs::write(
                            format!(
                                concat!(env!("CARGO_MANIFEST_DIR"), "/../frame_dump_band{}.ppm"),
                                band_dumps
                            ),
                            &ppm,
                        );
                        let vc: Vec<String> = emu4
                            .nds_mmu
                            .vram
                            .banks
                            .iter()
                            .map(|b| format!("{:02x}", b.control))
                            .collect();
                        let io = &emu4.nds_mmu.arm9_io;
                        let rwv = |b: usize| {
                            u32::from_le_bytes([io[b], io[b + 1], io[b + 2], io[b + 3]])
                        };
                        eprintln!(
                            "  BAND f{frame}: white={white} VRAMCNT=[{}] dispA={:08x} dispB={:08x} bgcntB=[{:04x} {:04x} {:04x} {:04x}]",
                            vc.join(" "),
                            rwv(0x000),
                            rwv(0x1000),
                            u16::from_le_bytes([io[0x1008], io[0x1009]]),
                            u16::from_le_bytes([io[0x100A], io[0x100B]]),
                            u16::from_le_bytes([io[0x100C], io[0x100D]]),
                            u16::from_le_bytes([io[0x100E], io[0x100F]]),
                        );
                        emu4.nds_mmu.gx.trace_on = true;
                    }
                }
                if matches!(frame, 1400 | 2000 | 2100) {
                    // U32: full OAM decode — (a) who draws the moon vs the
                    // affine Lugia and at what priorities (user: Lugia renders
                    // BEHIND the moon); (b) are the flyover's villagers OBJs
                    // that never show (user: towns are empty).
                    for (eng, base) in [("A", 0usize), ("B", 0x400)] {
                        let mut vis: Vec<String> = Vec::new();
                        for i in 0..128 {
                            let at = base + i * 8;
                            let a0 =
                                u16::from_le_bytes([emu4.nds_mmu.oam[at], emu4.nds_mmu.oam[at + 1]]);
                            let a1 =
                                u16::from_le_bytes([emu4.nds_mmu.oam[at + 2], emu4.nds_mmu.oam[at + 3]]);
                            let a2 =
                                u16::from_le_bytes([emu4.nds_mmu.oam[at + 4], emu4.nds_mmu.oam[at + 5]]);
                            if (a0 & 0x100 == 0 && a0 & 0x200 != 0)
                                || (a0 == 0 && a1 == 0 && a2 == 0)
                            {
                                continue;
                            }
                            if vis.len() < 28 {
                                vis.push(format!("[{i}]{a0:04x}/{a1:04x}/{a2:04x}"));
                            }
                        }
                        eprintln!("  OAMALL f{frame} {eng}: {}", vis.join(" "));
                    }
                }
                if frame == 4600 {
                    // U33: title-only texture pairs + a one-frame raster
                    // census at f5400 (below) name the hole-punching texture.
                    emu4.nds_mmu.gx.engine.tex_pairs.clear();
                }
                if frame == 5390 {
                    emu4.nds_mmu.gx.engine.tex_stats_on = true;
                }
                if frame == 5400 {
                    let eng = &mut emu4.nds_mmu.gx.engine;
                    eng.tex_stats_on = false;
                    let mut st = eng.tex_stats.clone();
                    st.sort_by_key(|e| std::cmp::Reverse(e.3));
                    for &(tp, pb, op, tr) in st.iter().take(16) {
                        eprintln!(
                            "  TEXHOLES fmt={} {}x{} texgen={} c0={} rep={}{} addr={:#07x} pal={:#07x} opaque={op} transparent={tr}",
                            (tp >> 26) & 7,
                            8 << ((tp >> 20) & 7),
                            8 << ((tp >> 23) & 7),
                            (tp >> 30) & 3,
                            (tp >> 29) & 1,
                            (tp >> 16) & 1, (tp >> 17) & 1,
                            (tp & 0xFFFF) * 8,
                            if (tp >> 26) & 7 == 2 { pb * 8 } else { pb * 16 },
                        );
                    }
                }
                if frame == 4700 && std::env::var("PROBE_TITLE_DWELL").is_ok() {
                    // U32: raw texture banks for offline decode — is the Lugia
                    // body stipple in the texture DATA or in our sampling?
                    let _ = std::fs::write(
                        concat!(env!("CARGO_MANIFEST_DIR"), "/../texbank_a.bin"),
                        &emu4.nds_mmu.vram.banks[0].data,
                    );
                    let _ = std::fs::write(
                        concat!(env!("CARGO_MANIFEST_DIR"), "/../texbank_e.bin"),
                        &emu4.nds_mmu.vram.banks[4].data,
                    );
                    eprintln!("  TEXDUMP f4700: banks A+E written");
                }
                if ingame && frame == 13950 {
                    // U34: census the overworld's textures (opaque vs
                    // transparent texels) — a billboard texture sampled
                    // all-transparent is the missing character.
                    emu4.nds_mmu.gx.engine.tex_stats_on = true;
                    emu4.nds_mmu.gx.engine.tex_stats.clear();
                }
                if ingame && frame == 14005 {
                    let mut st = emu4.nds_mmu.gx.engine.tex_stats.clone();
                    emu4.nds_mmu.gx.engine.tex_stats_on = false;
                    st.sort_by_key(|e| std::cmp::Reverse(e.2 + e.3));
                    for &(tp, pb, op, tr) in st.iter().take(20) {
                        eprintln!(
                            "  OWTEX fmt={} {}x{} texgen={} addr={:#07x} pal={:#07x} opaque={op} transparent={tr}",
                            (tp >> 26) & 7,
                            8 << ((tp >> 20) & 7),
                            8 << ((tp >> 23) & 7),
                            (tp >> 30) & 3,
                            (tp & 0xFFFF) * 8,
                            if (tp >> 26) & 7 == 2 { pb * 8 } else { pb * 16 },
                            op = op,
                            tr = tr,
                        );
                    }
                }
                if ingame && frame >= 11000 && frame % 1000 == 0 {
                    // U34: overworld (bedroom) layer evidence — the exact
                    // frame the user's chars/furniture bug is visible.
                    // Decode the first enabled OBJ entries (engine A) so an
                    // on-screen sprite with a valid tile that still doesn't
                    // render points at draw_objs; off-screen/blank points at
                    // the game (or a stale scene).
                    let mut objs: Vec<String> = Vec::new();
                    for i in 0..128 {
                        let at = i * 8;
                        let a0 = u16::from_le_bytes([emu4.nds_mmu.oam[at], emu4.nds_mmu.oam[at + 1]]);
                        let a1 = u16::from_le_bytes([emu4.nds_mmu.oam[at + 2], emu4.nds_mmu.oam[at + 3]]);
                        let a2 = u16::from_le_bytes([emu4.nds_mmu.oam[at + 4], emu4.nds_mmu.oam[at + 5]]);
                        if a0 & 0x100 == 0 && a0 & 0x200 != 0 {
                            continue;
                        }
                        if a0 == 0 && a1 == 0 && a2 == 0 {
                            continue;
                        }
                        let y = a0 & 0xFF;
                        let x = a1 & 0x1FF;
                        // On-screen sprites (not parked at 192,192) — the
                        // player character, if it is an OBJ, is here.
                        if y < 180 && x < 256 && objs.len() < 12 {
                            objs.push(format!("[{i}]y{y}x{x}sh{}sz{}t{:#05x}p{}", (a0 >> 14) & 3, (a1 >> 14) & 3, a2 & 0x3FF, (a2 >> 12) & 0xF));
                        }
                    }
                    eprintln!("  OWOBJ f{frame} A on-screen: {}", if objs.is_empty() { "NONE".into() } else { objs.join(" ") });
                }
                if ingame && matches!(frame, 12000 | 14000 | 16000) {
                    let io = &emu4.nds_mmu.arm9_io;
                    let rw = |b: usize| u32::from_le_bytes([io[b], io[b + 1], io[b + 2], io[b + 3]]);
                    let vc: Vec<String> = emu4
                        .nds_mmu
                        .vram
                        .banks
                        .iter()
                        .map(|b| format!("{:02x}", b.control))
                        .collect();
                    // OBJ census both engines: how many enabled sprites, and
                    // are they regular/affine/bitmap/window mode?
                    for (eng, base) in [("A", 0usize), ("B", 0x400)] {
                        let (mut reg, mut aff, mut bmp, mut win) = (0, 0, 0, 0);
                        for i in 0..128 {
                            let at = base + i * 8;
                            let a0 = u16::from_le_bytes([emu4.nds_mmu.oam[at], emu4.nds_mmu.oam[at + 1]]);
                            if a0 & 0x100 == 0 && a0 & 0x200 != 0 {
                                continue;
                            }
                            match (a0 >> 10) & 3 {
                                2 => win += 1,
                                3 => bmp += 1,
                                _ if a0 & 0x100 != 0 => aff += 1,
                                _ => reg += 1,
                            }
                        }
                        eprintln!("  OVERWORLD f{frame} {eng}: reg={reg} aff={aff} bmp={bmp} win={win}");
                    }
                    let e = &emu4.nds_mmu.gx.engine;
                    eprintln!(
                        "  OVERWORLD f{frame}: dispA={:08x} dispB={:08x} POWCNT={:04x} VRAMCNT=[{}] GX3D tris={} opaque={} swaps={} wbuf",
                        rw(0x000), rw(0x1000),
                        u16::from_le_bytes([io[0x304], io[0x305]]),
                        vc.join(" "),
                        e.last_frame_tris,
                        e.fb.iter().filter(|&&p| p & 0x8000 != 0).count(),
                        e.swap_count,
                    );
                    // Engine-B BG state. The bottom screen renders as backdrop
                    // only; this separates "the game never uploaded tiles" from
                    // "we address or colour them wrong".
                    {
                        let v = &emu4.nds_mmu.vram;
                        let nz = |i: usize| v.banks[i].data.iter().filter(|&&b| b != 0).count();
                        let bgcnt = |bg: usize| {
                            u16::from_le_bytes([
                                io[0x1008 + bg * 2],
                                io[0x1009 + bg * 2],
                            ])
                        };
                        // Engine-B palette lives at palette RAM + 0x400.
                        let pal_nz = emu4.nds_mmu.palette_ram[0x400..0x600]
                            .iter()
                            .filter(|&&b| b != 0)
                            .count();
                        eprintln!(
                            "  OVERWORLD f{frame} ENGB: bgcnt=[{:04x} {:04x} {:04x} {:04x}] bank_nonzero C={} H={} I={} palB_nonzero={}/512",
                            bgcnt(0), bgcnt(1), bgcnt(2), bgcnt(3),
                            nz(2), nz(7), nz(8), pal_nz
                        );
                    }
                    eprintln!(
                        "  OVERWORLD f{frame} 3D-clip: near_rejected={} tex_degenerate={} tex_zero_px={}",
                        emu4.nds_mmu.gx.engine.last_near_rejected,
                        emu4.nds_mmu.gx.engine.last_tex_degenerate,
                        emu4.nds_mmu.gx.engine.last_tex_zero_px,
                    );
                    // SNDCAP state at the overworld — the user's audio complaint.
                    let active: Vec<String> = emu4
                        .nds_mmu
                        .apu
                        .channels
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.active)
                        .map(|(i, c)| format!("ch{i}(fmt{} sad{:#08x})", (c.cnt >> 29) & 3, c.sad))
                        .collect();
                    eprintln!(
                        "  OVERWORLD f{frame} SNDCAP: cnt=[{:#04x} {:#04x}] dad=[{:#010x} {:#010x}] len=[{} {}] writes={} | active={}",
                        emu4.nds_mmu.apu.cap_cnt[0],
                        emu4.nds_mmu.apu.cap_cnt[1],
                        emu4.nds_mmu.apu.cap_dad[0],
                        emu4.nds_mmu.apu.cap_dad[1],
                        emu4.nds_mmu.apu.cap_len[0],
                        emu4.nds_mmu.apu.cap_len[1],
                        emu4.nds_mmu.apu.cap_write_log.len(),
                        active.join(" "),
                    );
                    // Full-frame dump of the bedroom for visual evidence.
                    let mut fb = vec![0u16; 256 * 384];
                    for ly in 0..192u16 {
                        emu4.nds_ppu.render_scanline(ly, &emu4.nds_mmu, &mut fb);
                    }
                    let mut ppm = b"P6\n256 384\n255\n".to_vec();
                    for &px in &fb {
                        ppm.extend_from_slice(&[
                            ((px & 0x1F) << 3) as u8,
                            (((px >> 5) & 0x1F) << 3) as u8,
                            (((px >> 10) & 0x1F) << 3) as u8,
                        ]);
                    }
                    let _ = std::fs::write(
                        format!(concat!(env!("CARGO_MANIFEST_DIR"), "/../frame_dump_ow{}.ppm"), frame),
                        &ppm,
                    );
                }
                if !ingame && matches!(frame, 7000 | 7500 | 7750) {
                    // U34: in-game layer evidence — user reports characters/
                    // furniture invisible in gameplay (environment only).
                    for (eng, base) in [("A", 0usize), ("B", 0x400)] {
                        let (mut aff, mut reg) = (0, 0);
                        for i in 0..128 {
                            let at = base + i * 8;
                            let a0 =
                                u16::from_le_bytes([emu4.nds_mmu.oam[at], emu4.nds_mmu.oam[at + 1]]);
                            if a0 & 0x100 != 0 {
                                aff += 1;
                            } else if a0 & 0x200 == 0 {
                                reg += 1;
                            }
                        }
                        eprintln!("  INGAME f{frame} {eng}: rotscale={aff} regular={reg}");
                    }
                    let eng3d = &emu4.nds_mmu.gx.engine;
                    eprintln!(
                        "  INGAME f{frame} GX3D: tris={} opaque={} wbuf_swaps={}",
                        eng3d.last_frame_tris,
                        eng3d.fb.iter().filter(|&&p| p & 0x8000 != 0).count(),
                        eng3d.swap_count,
                    );
                }
                if matches!(frame, 1300 | 1400 | 1500 | 3400) {
                    // U31: affine-OBJ census (moon scene + the Unown beat,
                    // whose swirl renders a corrupted striped block).
                    for (eng, base) in [("A", 0usize), ("B", 0x400)] {
                        let (mut aff, mut reg) = (0, 0);
                        let mut first: Vec<String> = Vec::new();
                        for i in 0..128 {
                            let at = base + i * 8;
                            let a0 =
                                u16::from_le_bytes([emu4.nds_mmu.oam[at], emu4.nds_mmu.oam[at + 1]]);
                            let a1 =
                                u16::from_le_bytes([emu4.nds_mmu.oam[at + 2], emu4.nds_mmu.oam[at + 3]]);
                            let a2 =
                                u16::from_le_bytes([emu4.nds_mmu.oam[at + 4], emu4.nds_mmu.oam[at + 5]]);
                            if a0 & 0x100 != 0 {
                                aff += 1;
                                if first.len() < 6 {
                                    first.push(format!("[{i}]{a0:04x}/{a1:04x}/{a2:04x}"));
                                }
                            } else if a0 & 0x200 == 0 {
                                reg += 1;
                            }
                        }
                        eprintln!(
                            "  OAMAFF f{frame} {eng}: rotscale={aff} regular={reg} {}",
                            first.join(" ")
                        );
                    }
                    let vc: Vec<String> = emu4
                        .nds_mmu
                        .vram
                        .banks
                        .iter()
                        .map(|b| format!("{:02x}", b.control))
                        .collect();
                    eprintln!("  OAMAFF f{frame} VRAMCNT=[{}]", vc.join(" "));
                }
                // U21: the title screen ("TOUCH TO START") is stable by ~f4000.
                // Press START+A for 30 frames to leave it, then keep running so
                // the main menu renders into the end-of-run frame dump.
                if touch_only {
                    // U27 (PROBE_TOUCH_ONLY set): NO START/A anywhere. Chain
                    // taps through the new-game flow (timeline mapped by the
                    // U27 evidence run: info screen stable by f4600): f4100/
                    // f4400 advance movie/title/menu (fire on pen-down), then
                    // release-fired UI buttons — f4700 on the measured "NO
                    // INFO NEEDED" band (128,153), f5000 + f5300 on the guide
                    // screen's "Touch" button (OAM footprint x=168-248
                    // y=144-176 -> 208,160), then a tap every 300 frames at
                    // (128,96) to advance Oak's dialog boxes.
                    // U27h: the f4700 tap WIGGLES 1px per frame (a perfectly
                    // static stroke may trip an anti-ghost/chatter filter);
                    // the f5600 tap is a SHORT human-like 3-frame tap.
                    let (tx, ty) = match frame {
                        4700..=4999 => (128u16 + (frame % 3) as u16, 153u16 - (frame % 2) as u16),
                        5000..=5599 => (208, 160),
                        _ => (128, 96),
                    };
                    emu4.buttons.nds_touch_x = tx;
                    emu4.buttons.nds_touch_y = ty;
                    // PROBE_TITLE_DWELL: tap once to reach the title, then
                    // let it sit — Lugia fades in over ~10-20 s (ss9 shot).
                    let title_dwell = std::env::var("PROBE_TITLE_DWELL").is_ok();
                    emu4.buttons.nds_touch_pressed = (4100..4110).contains(&frame)
                        || (!title_dwell
                            && ((4400..4410).contains(&frame)
                                || (frame >= 4700
                                    && frame != 5600
                                    && (frame - 4700) % 300 < 10)
                                || (5600..5603).contains(&frame)));
                    if frame == 4690 || frame == 5290 || frame == 5890 {
                        emu4.nds_mmu.spi.tsc.io_log_on = true;
                    }
                    if frame == 4740 || frame == 5360 || frame == 5940 {
                        emu4.nds_mmu.spi.tsc.io_log_on = false;
                    }
                    // U27: value-matched ARM7 write watch across the f4700
                    // tap (press, hold, release + driver's ~5 post-release
                    // sampling frames) — locates the shared TP sample struct.
                    if frame == 4695 {
                        emu4.nds_mmu.tp_watch_on = true;
                        emu4.nds_mmu.fifo_log_on = true;
                    }
                    if frame == 4770 {
                        emu4.nds_mmu.tp_watch_on = false;
                        emu4.nds_mmu.fifo_log_on = false;
                    }
                    // U27g: does the ARM9 READ the delivered touch state, and
                    // equally on a WORKING (menu, f4400) vs DEAD (info,
                    // f4700) tap? Counts of instruction-level reads of the
                    // system TP slot + hot page across each tap window.
                    if frame == 4395 || frame == 4695 {
                        emu4.nds_mmu.tp_slot_reads = [0; 6];
                        emu4.nds_mmu.tp_read_watch_on = true;
                    }
                    if frame == 4420 || frame == 4720 {
                        emu4.nds_mmu.tp_read_watch_on = false;
                        eprintln!(
                            "  TPREADS f{}..{frame} [36B8,36BC,FFA8,FFAC,D460,E640]: {:?}",
                            frame - 25,
                            emu4.nds_mmu.tp_slot_reads
                        );
                    }
                    // U27i: the decoded input-merger (fn 0x0201a570) writes
                    // x/y/new_down/held at +0x20/22/24/26 of the input struct
                    // (literal candidates 0x021d110c / 0x021d114c). Dump both
                    // per frame across the WORKING menu tap and the DEAD info
                    // tap — if new_down pulses in both, the entire input
                    // layer is healthy and the bug is scene-handler logic.
                    if (4395..=4430).contains(&frame) || (4695..=4730).contains(&frame) {
                        let rh = |a: u32| {
                            let o = (a & 0x00FF_FFFF) as usize;
                            u16::from_le_bytes([
                                emu4.nds_mmu.main_ram[o],
                                emu4.nds_mmu.main_ram[o + 1],
                            ])
                        };
                        eprintln!(
                            "  INSTRUCT f{frame}: A[x={} y={} new={:04x} held={:04x}] B[x={} y={} new={:04x} held={:04x}]",
                            rh(0x021d_112c), rh(0x021d_112e), rh(0x021d_1130), rh(0x021d_1132),
                            rh(0x021d_116c), rh(0x021d_116e), rh(0x021d_1170), rh(0x021d_1172),
                        );
                    }
                    // U27k: the TP library itself — TP_GetRaw (0x020da3a0)
                    // and TP_GetCalibratedPoint (0x020da6e0) produce
                    // {0,0,touch,validity}. Dump their code + literal pools
                    // and auto-deref every main-RAM pointer literal: one of
                    // them is the calibrate-param struct (expected zeros).
                    if frame == 4706 {
                        // The RAM user-settings copy's calibration block: if
                        // it holds the real values while the TP params are
                        // zero, the game fed TP_SetCalibrateParam from a
                        // different source (save file / re-read settings).
                        let ram = &emu4.nds_mmu.main_ram;
                        let cal: Vec<String> = (0..12)
                            .map(|k| format!("{:02x}", ram[0x3FFC80 + 0x58 + k]))
                            .collect();
                        eprintln!("  RAMSETTINGS cal@0x027FFCD8: {}", cal.join(" "));
                        let mut lits: Vec<u32> = Vec::new();
                        let mut code = String::new();
                        for k in 0..0x160u32 {
                            let w = emu4.nds_mmu.read_word_arm9(0x020d_a3a0 + k * 4);
                            code.push_str(&format!("{w:08x} "));
                            if (0x0200_0000..0x0240_0000).contains(&w)
                                && !lits.contains(&w)
                                && lits.len() < 12
                            {
                                lits.push(w);
                            }
                        }
                        eprintln!("  TPCODE@0x020da3a0: {code}");
                        for &l in &lits {
                            let ws: Vec<String> = (0..10)
                                .map(|k| {
                                    format!("{:08x}", emu4.nds_mmu.read_word_arm9(l + k * 4))
                                })
                                .collect();
                            eprintln!("  TPDEREF[{l:#010x}]: {}", ws.join(" "));
                        }
                    }
                    // U27g: literal-pool scan — which ARM9 code regions hold
                    // pointers to the touch state words? (LDR literals; the
                    // hit addresses name the reader functions.)
                    if frame == 4705 {
                        let ram = &emu4.nds_mmu.main_ram;
                        let targets = [
                            0x021E_36B8u32,
                            0x027F_FFA8,
                            0x021D_D460,
                            0x021E_3108,
                            // Input-struct candidates (the merger's output) —
                            // their literal sites name the CONSUMERS.
                            0x021D_110C,
                            0x021D_114C,
                        ];
                        for &t in &targets {
                            let mut hits: Vec<u32> = Vec::new();
                            for i in (0..ram.len() - 4).step_by(4) {
                                if u32::from_le_bytes([ram[i], ram[i + 1], ram[i + 2], ram[i + 3]])
                                    == t
                                    && hits.len() < 16
                                {
                                    hits.push(0x0200_0000 + i as u32);
                                }
                            }
                            let s: Vec<String> =
                                hits.iter().map(|a| format!("{a:#010x}")).collect();
                            eprintln!("  LITSCAN {t:#010x}: {}", s.join(" "));
                        }
                        // U27h: raw code dumps around the literal sites found
                        // by the previous run — the function bodies that LDR
                        // these pointers ARE the touch predicate candidates.
                        for site in [
                            0x0200_11d0u32,
                            0x0201_a5d0,
                            0x0203_aea4,
                            0x0209_6588,
                            0x020d_dbb8,
                            0x0223_6034,
                            0x020c_7a8c,
                            0x020c_7ad8,
                            0x020c_7b2c,
                        ] {
                            let words: Vec<String> = (0..28)
                                .map(|k| {
                                    format!(
                                        "{:08x}",
                                        emu4.nds_mmu.read_word_arm9(site - 0x60 + k * 4)
                                    )
                                })
                                .collect();
                            eprintln!("  CODE@{:#010x}-0x60: {}", site, words.join(" "));
                        }
                    }
                    // U27 TP-struct hunt (workstream A evidence 2): mid-hold,
                    // scan main RAM for adjacent u16 pairs holding this tap's
                    // TSC ADC values (spi.rs formulas: X=(128-64)*18+880=2032,
                    // Y=(153-48)*225/10+960=3322) or the calibrated pixel pair
                    // (128,153) — wherever the ARM7 sampler stores pen samples
                    // and the ARM9 UI reads them. Packed-12-bit forms would
                    // be missed; the u16 struct layout is the SDK's.
                    if frame == 4705 || frame == 5905 {
                        let ram = &emu4.nds_mmu.main_ram;
                        let mut hits: Vec<(u32, &str)> = Vec::new();
                        for i in (0..ram.len() - 4).step_by(2) {
                            let a = u16::from_le_bytes([ram[i], ram[i + 1]]);
                            let b = u16::from_le_bytes([ram[i + 2], ram[i + 3]]);
                            let tag = if (a == 2032 && b == 3322) || (a == 3322 && b == 2032) {
                                "adc"
                            } else if (a == 128 && b == 153) || (a == 153 && b == 128) {
                                "px"
                            } else if ram[i] == 128 && ram[i + 1] == 153 {
                                // Byte-sized calibrated pair — the screen is
                                // 256x192, u8 storage is plausible and the
                                // u16 scans miss it.
                                "pxb"
                            } else {
                                continue;
                            };
                            hits.push((0x0200_0000 + i as u32, tag));
                        }
                        eprintln!("  TPSCAN f{frame}: {} hits (cap 24 shown)", hits.len());
                        for &(addr, tag) in hits.iter().take(24) {
                            let c: Vec<String> = (0..4)
                                .map(|k| format!("{:08x}", emu4.nds_mmu.read_word_arm9(addr - 4 + k * 4)))
                                .collect();
                            eprintln!("    [{addr:#010x}] {tag}: {}", c.join(" "));
                            if tp_hit_addrs.len() < 24 {
                                tp_hit_addrs.push(addr);
                            }
                        }
                    }
                    // U27e value-AGNOSTIC delivery evidence: snapshot two
                    // candidate delivery regions before the tap and diff them
                    // mid-hold — catches any format (flags, packed, bytes).
                    // Region 1 = the tag-7 ring window; region 2 = the shared
                    // hot page (0x027FF000) gen-4 games use for ARM7 state.
                    if frame == 4698 {
                        let ram = &emu4.nds_mmu.main_ram;
                        // Covers BOTH the tag-7 command ring (0x021E3xxx) and
                        // the cmd-0x21 double-buffered destination blocks it
                        // names (0x021DD460 / 0x021DE640).
                        tp_snap_ring = ram[0x1D0000..0x1E8000].to_vec();
                        tp_snap_hot = ram[0x3FF000..0x400000].to_vec();
                        eprintln!(
                            "  TAG7 ring addr now: {:#010x}",
                            emu4.nds_mmu.last_tag7_data
                        );
                    }
                    if frame == 4705 || frame == 4715 {
                        let ram = &emu4.nds_mmu.main_ram;
                        let mut shown = 0;
                        eprintln!("  RINGDIFF f{frame} (vs f4698, 0x021D0000+):");
                        for i in (0..0x18000).step_by(4) {
                            let old = &tp_snap_ring[i..i + 4];
                            let new = &ram[0x1D0000 + i..0x1D0000 + i + 4];
                            if old != new && shown < 96 {
                                shown += 1;
                                eprintln!(
                                    "    [{:#010x}] {:02x}{:02x}{:02x}{:02x} -> {:02x}{:02x}{:02x}{:02x}",
                                    0x021D_0000 + i as u32,
                                    old[3], old[2], old[1], old[0],
                                    new[3], new[2], new[1], new[0]
                                );
                            }
                        }
                        for base in [0x1dd460usize, 0x1de640] {
                            let words: Vec<String> = (0..12)
                                .map(|k| {
                                    let o = base + k * 4;
                                    format!(
                                        "{:02x}{:02x}{:02x}{:02x}",
                                        ram[o + 3], ram[o + 2], ram[o + 1], ram[o]
                                    )
                                })
                                .collect();
                            eprintln!(
                                "  CMD21ARG@{:#010x} f{frame}: {}",
                                0x0200_0000 + base as u32,
                                words.join(" ")
                            );
                        }
                        let mut shown2 = 0;
                        eprintln!("  HOTDIFF f{frame} (vs f4698, 0x027FF000+):");
                        for i in (0..0x1000).step_by(4) {
                            let old = &tp_snap_hot[i..i + 4];
                            let new = &ram[0x3FF000 + i..0x3FF000 + i + 4];
                            if old != new && shown2 < 32 {
                                shown2 += 1;
                                eprintln!(
                                    "    [{:#010x}] {:02x}{:02x}{:02x}{:02x} -> {:02x}{:02x}{:02x}{:02x}",
                                    0x027F_F000 + i as u32,
                                    old[3], old[2], old[1], old[0],
                                    new[3], new[2], new[1], new[0]
                                );
                            }
                        }
                        // The exact ring the ARM9 requested this frame.
                        let base = emu4.nds_mmu.last_tag7_data & 0x00FF_FFFF;
                        if (0x1E0000..0x3F0000).contains(&base) {
                            let words: Vec<String> = (0..12)
                                .map(|k| {
                                    let o = (base as usize + k * 4) % ram.len();
                                    format!(
                                        "{:02x}{:02x}{:02x}{:02x}",
                                        ram[o + 3], ram[o + 2], ram[o + 1], ram[o]
                                    )
                                })
                                .collect();
                            eprintln!(
                                "  RING@{:#010x}: {}",
                                0x0200_0000 + base,
                                words.join(" ")
                            );
                        }
                    }
                    // Same addresses well after pen release: how the sampler
                    // marks an ended stroke (validity/touch flags vs stale
                    // coordinates).
                    if (frame == 4760 || frame == 5960) && !tp_hit_addrs.is_empty() {
                        eprintln!("  TPSCAN-RELEASE f{frame}:");
                        for &addr in tp_hit_addrs.iter() {
                            let c: Vec<String> = (0..4)
                                .map(|k| format!("{:08x}", emu4.nds_mmu.read_word_arm9(addr - 4 + k * 4)))
                                .collect();
                            eprintln!("    [{addr:#010x}]: {}", c.join(" "));
                        }
                    }
                } else {
                    if frame == 4100 {
                        emu4.buttons.start = true;
                        emu4.buttons.a = true;
                    }
                    if frame == 4130 {
                        emu4.buttons.start = false;
                        emu4.buttons.a = false;
                    }
                    // U24: pulse A every 200 frames after the menu appears —
                    // menu / Oak dialog advance on presses, so the end-of-run
                    // dump lands as deep as the renderer can currently show.
                    if frame >= 4300 {
                        emu4.buttons.a = (frame - 4300) % 200 < 8;
                    }
                    // U25 touch evidence: tap the menu "Touch" button late in
                    // the run and count TSC conversions at the end.
                    emu4.buttons.nds_touch_x = 215;
                    emu4.buttons.nds_touch_y = 170;
                    emu4.buttons.nds_touch_pressed = (5800..5808).contains(&frame);
                }
                // U34: in-game override — mash A + touch the (212,174) "Touch"
                // button to blast through title/info/guide/Oak/wake-up into the
                // overworld, matching the headless exe schedule that reaches
                // the bedroom. Overrides whatever the touch_only block set.
                if ingame {
                    // Mirror the headless-exe schedule proven to reach the
                    // bedroom: A@4400 (title->info), NO-INFO-NEEDED touch
                    // (128,153) @4900, guide "Touch" (212,174) @5300, then A
                    // mash + (212,174) touch every 90 frames to blast Oak's
                    // dialogs and the wake-up into the overworld.
                    emu4.buttons.start = false;
                    emu4.buttons.select = false;
                    let mut a = false;
                    let (mut tx, mut ty, mut tp) = (212u16, 174u16, false);
                    if (4400..4408).contains(&frame) {
                        a = true;
                    } else if (4900..4908).contains(&frame) {
                        tx = 128;
                        ty = 153;
                        tp = true;
                    } else if (5300..5308).contains(&frame) {
                        tp = true;
                    } else if frame >= 5600 {
                        let ph = frame % 90;
                        a = ph < 8;
                        tp = (30..38).contains(&ph);
                    }
                    emu4.buttons.a = a;
                    emu4.buttons.nds_touch_x = tx;
                    emu4.buttons.nds_touch_y = ty;
                    emu4.buttons.nds_touch_pressed = tp;
                }
                // The exact call tick() makes each frame: syncs nds_mmu.buttons
                // (KEYINPUT/EXTKEYIN) + the SPI TSC stylus inputs.
                emu4.poll_nds_touch_penirq();
                let mut a9run = 0u32;
                while a9run < 560_190 {
                    let mut run9 = 0u32;
                    while run9 < 64 {
                        let thumb = emu4.nds_arm9.cpu.registers.get_flag(crate::nds::cpu::FLAG_T);
                        let pc = emu4.nds_arm9.cpu.registers.gpr[15].wrapping_sub(if thumb { 4 } else { 8 });
                        let in_rtos = (0x020d_d000..0x020d_e000).contains(&pc);
                        if in_rtos && !prev_in_rtos {
                            let lr = emu4.nds_arm9.cpu.registers.gpr[14];
                            entries.push((pc, lr));
                            if entries.len() > 32 {
                                entries.remove(0);
                            }
                        }
                        prev_in_rtos = in_rtos;
                        ring.push(pc);
                        if ring.len() > 48 {
                            ring.remove(0);
                        }
                        // Overlay-38 decompressor entry: dump the footer AS
                        // LOADED (it's consumed in-place, unverifiable later).
                        // If it differs from the ROM file's own footer bytes,
                        // the FS load was wrong; if it matches, the divergence
                        // is inside the decompressor's execution.
                        let exec9_early = if emu4.nds_arm9.cpu.pc_modified {
                            emu4.nds_arm9.cpu.registers.gpr[15] & !(if thumb { 1 } else { 3 })
                        } else {
                            pc
                        };
                        if exec9_early == 0x020d_3aa8 && frame >= 300 {
                            *spin_lrs.entry(emu4.nds_arm9.cpu.registers.gpr[14]).or_insert(0u64) += 1;
                        }
                        if exec9_early == 0x0200_0970 && decomp_dumps < 6 {
                            decomp_dumps += 1;
                            let r = emu4.nds_arm9.cpu.registers.gpr;
                            eprintln!(
                                "  DECOMP-ENTRY f{frame}: r0={:#010x} lr={:#010x} footer=[{:#010x} {:#010x}] (file: 0x0a006493 0x00004020)",
                                r[0],
                                r[14],
                                emu4.nds_mmu.read_word_arm9(r[0].wrapping_sub(8)),
                                emu4.nds_mmu.read_word_arm9(r[0].wrapping_sub(4)),
                            );
                            eprintln!("    region head [0x0221ba00..]: {:08x} {:08x} {:08x} {:08x} (file: 43010401 60014801 46c04770 02226020)",
                                emu4.nds_mmu.read_word_arm9(0x0221_ba00),
                                emu4.nds_mmu.read_word_arm9(0x0221_ba04),
                                emu4.nds_mmu.read_word_arm9(0x0221_ba08),
                                emu4.nds_mmu.read_word_arm9(0x0221_ba0c),
                            );
                            eprintln!("    pre-footer [0x02221fe0..]: {:08x} {:08x} {:08x} {:08x}",
                                emu4.nds_mmu.read_word_arm9(0x0222_1fe0),
                                emu4.nds_mmu.read_word_arm9(0x0222_1fe4),
                                emu4.nds_mmu.read_word_arm9(0x0222_1fe8),
                                emu4.nds_mmu.read_word_arm9(0x0222_1fec),
                            );
                            // The COMPRESSED input exactly as loaded, byte-
                            // comparable against the ROM file: separates
                            // input-corruption from execution-corruption.
                            if emu4.nds_arm9.cpu.registers.gpr[0] == 0x0222_2000 {
                                let base = (0x0221_ba00u32 - 0x0200_0000) as usize;
                                let _ = std::fs::write(
                                    concat!(env!("CARGO_MANIFEST_DIR"), "/../ov38_comp_emu.bin"),
                                    &emu4.nds_mmu.main_ram[base..base + 0x6600],
                                );
                                eprintln!("    wrote ov38_comp_emu.bin (compressed input as loaded)");
                            }
                        }
                        if (pc == 0x020d_d920 || pc == 0x020d_db3c || pc == 0x020d_dbf0)
                            && panic_dumps < 6
                        {
                            panic_dumps += 1;
                            let r = emu4.nds_arm9.cpu.registers.gpr;
                            eprintln!(
                                "  PANIC-TRAP #{panic_dumps} @pc={pc:#010x} f{frame}: r0={:#010x} r1={:#010x} r2={:#010x} r3={:#010x} r4={:#010x} r5={:#010x} lr={:#010x} sp={:#010x}",
                                r[0], r[1], r[2], r[3], r[4], r[5], r[14], r[13]
                            );
                            for (name, v) in
                                [("r0", r[0]), ("r1", r[1]), ("r2", r[2]), ("r3", r[3])]
                            {
                                if (0x0200_0000..0x0240_0000).contains(&v) {
                                    let bytes: Vec<u8> = (0..64)
                                        .map(|i| emu4.nds_mmu.read_byte_arm9(v + i))
                                        .collect();
                                    let ascii: String = bytes
                                        .iter()
                                        .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
                                        .collect();
                                    eprintln!("    [{name}]={v:#010x}: |{ascii}|");
                                }
                            }
                        }
                        if pc == 0x020d_3f48 {
                            eprintln!("  ARM9 reached HANG (0x020d3f48) at frame {frame}. RTOS entries (entry_pc <- lr caller):");
                            for (p, l) in entries.iter() {
                                let caller_rtos = (0x020d_d000..0x020d_e000).contains(l);
                                eprintln!("    {p:#010x} <- lr {l:#010x}{}", if caller_rtos { "" } else { "  <== NON-RTOS CALLER" });
                            }
                            eprintln!("  last 48 ARM9 pcs into the hang:");
                            for p in ring.iter() {
                                eprintln!("    {p:#010x}");
                            }
                            reached = true;
                            break 'trace;
                        }
                        // First execution of an all-zero instruction in RAM = the
                        // ARM9 left populated code. Dump how it got there — this
                        // is the ground-truth run, unlike the diverging 2:1 hunt.
                        let inst9 = emu4.nds_arm9.cpu.pipeline[0];
                        let pcmod9 = emu4.nds_arm9.cpu.pc_modified;
                        let sp9 = emu4.nds_arm9.cpu.registers.gpr[13];
                        // pc_modified means the pipeline is stale until step()'s
                        // flush — a zero there is leftover, not a real fetch.
                        // Wild pc = outside every mapped region (RAM/TCM mirrors
                        // below 0x04000000 and the high vectors) — a garbage jump.
                        let wild = pc >= 0x0400_0000 && !(0xFFFF_0000..0xFFFF_8000).contains(&pc);
                        if !zero_trapped
                            && !pcmod9
                            && !emu4.nds_arm9.cpu.halted
                            && (wild || (inst9 == 0 && (0x0200_0000..0x0280_0000).contains(&pc)))
                        {
                            zero_trapped = true;
                            let r = emu4.nds_arm9.cpu.registers.gpr;
                            eprintln!(
                                "  ARM9 {} at f{frame}: pc={pc:#010x} thumb={thumb} r0={:#x} r1={:#010x} r2={:#010x} r3={:#010x} r4={:#010x} r5={:#010x} sp={:#010x} lr={:#010x}",
                                if wild { "WILD-PC" } else { "ZERO-EXEC" },
                                r[0], r[1], r[2], r[3], r[4], r[5], r[13], r[14]
                            );
                            eprintln!("  last 60 ARM9 (exec [A/T] instr sp; *=flush-step) into the zero-exec:");
                            for (p, i, t, m, s) in ring2.iter() {
                                eprintln!(
                                    "    {p:#010x}{} [{}] {i:08x} sp={s:#010x}",
                                    if *m { "*" } else { " " },
                                    if *t { "T" } else { "A" }
                                );
                            }
                            eprintln!("  stack [sp-0x30..sp+0x14]:");
                            for off in (-0x30i32..0x14).step_by(4) {
                                let a = r[13].wrapping_add(off as u32);
                                eprintln!("    [sp{off:+#04x}] {a:#010x} = {:08x}", emu4.nds_mmu.read_word_arm9(a));
                            }
                            eprintln!("  code @0x02025ea0..0x02026030 (fatal fn family, halfwords):");
                            for a in (0x0202_5ea0u32..0x0202_6030).step_by(8) {
                                eprintln!(
                                    "    {a:#010x}: {:04x} {:04x} {:04x} {:04x}",
                                    emu4.nds_mmu.read_halfword_arm9(a),
                                    emu4.nds_mmu.read_halfword_arm9(a + 2),
                                    emu4.nds_mmu.read_halfword_arm9(a + 4),
                                    emu4.nds_mmu.read_halfword_arm9(a + 6)
                                );
                            }
                            // Alarm-node context: is [r4] an isolated bad word or
                            // sitting inside a run of ASCII (file data blasted
                            // over the heap)? Dump around r4 and the list walker.
                            for (name, base) in [("r4-node", r[4].saturating_sub(0x40)), ("r2", r[2].saturating_sub(0x20))] {
                                if (0x0200_0000..0x0240_0000).contains(&base) {
                                    eprintln!("  mem around {name} ({base:#010x}..):");
                                    for a in (base..base + 0xA0).step_by(16) {
                                        let bytes: Vec<u8> = (0..16).map(|i| emu4.nds_mmu.read_byte_arm9(a + i)).collect();
                                        let ascii: String = bytes.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
                                        let words: Vec<String> = (0..4).map(|i| format!("{:08x}", u32::from_le_bytes([bytes[i*4], bytes[i*4+1], bytes[i*4+2], bytes[i*4+3]]))).collect();
                                        eprintln!("    {a:#010x}: {} |{ascii}|", words.join(" "));
                                    }
                                }
                            }
                            eprintln!("  alarm walker @0x020d8be0..0x020d8c60:");
                            for a in (0x020d_8be0u32..0x020d_8c60).step_by(4) {
                                eprintln!("    {a:#010x}: {:08x}", emu4.nds_mmu.read_word_arm9(a));
                            }
                            // Overlay-38 region as the emulator decompressed it,
                            // for byte-diff against an offline BLZ oracle (the
                            // first divergent offset fingerprints the LZ token —
                            // and thus the CPU instruction — that misdecodes).
                            let base = (0x0221_ba00u32 - 0x0200_0000) as usize;
                            let _ = std::fs::write(
                                concat!(env!("CARGO_MANIFEST_DIR"), "/../ov38_emu.bin"),
                                &emu4.nds_mmu.main_ram[base..base + 0xa620],
                            );
                            eprintln!("  wrote ov38_emu.bin (0xa620 bytes @0x0221ba00)");
                            eprintln!("  overlay decompressor @0x02000940..0x02000a60:");
                            for a in (0x0200_0940u32..0x0200_0a60).step_by(4) {
                                eprintln!("    {a:#010x}: {:08x}", emu4.nds_mmu.read_word_arm9(a));
                            }
                        }
                        // Resolved exec addr: after a jump the true target is
                        // gpr15 itself (the flush destination), else gpr15-width*2.
                        let exec9 = if pcmod9 {
                            emu4.nds_arm9.cpu.registers.gpr[15] & !(if thumb { 1 } else { 3 })
                        } else {
                            pc
                        };
                        ring2.push((exec9, inst9, thumb, pcmod9, sp9));
                        if ring2.len() > 60 {
                            ring2.remove(0);
                        }
                        // Steady-state profile (late frames): where does the
                        // ARM9 actually spend its time? Halted steps count
                        // under a sentinel key so idle share is visible.
                        if frame >= 4700 {
                            prof_n += 1;
                            if prof_n % 4 == 0 {
                                let key = if emu4.nds_arm9.cpu.halted { 0xFFFF_FFFF } else { exec9 };
                                *prof9.entry(key).or_insert(0) += 1;
                            }
                        }
                        if !emu4.nds_arm9.cpu.pc_modified && !emu4.nds_arm9.cpu.halted {
                            let is_swi = if thumb {
                                (inst9 & 0xFF00) == 0xDF00
                            } else {
                                (inst9 >> 28) != 0xF && ((inst9 >> 24) & 0xF) == 0xF
                            };
                            if is_swi {
                                swi9_n += 1;
                                if swi9.len() < 20 {
                                    let c = if thumb { inst9 & 0xFF } else { (inst9 >> 16) & 0xFF };
                                    swi9.push((pc, c));
                                }
                            }
                        }
                        let b = snap(&emu4.nds_mmu);
                        let bh = snap_hw(&emu4.nds_mmu);
                        // U28c: catch the exact ARM9 instruction that writes
                        // the TP calibrate-param struct — pc/lr name the
                        // caller (no BL/BLX to TP_SetCalibrateParam exists
                        // in RAM; it's reached via veneer/fn-pointer).
                        let calib_before = emu4.nds_mmu.calib_write_log.len();
                        run9 += emu4.nds_arm9.step(&mut emu4.nds_mmu);
                        if (36..=40).contains(&frame)
                            && emu4.nds_mmu.calib_write_log.len() > calib_before
                            && calib_pc_logs < 24
                        {
                            calib_pc_logs += 1;
                            eprintln!(
                                "  CALIBWRITE-PC f{frame}: pc={pc:#010x} lr={:#010x} wrote {:x?}",
                                emu4.nds_arm9.cpu.registers.gpr[14],
                                &emu4.nds_mmu.calib_write_log[calib_before..]
                            );
                        }
                        let a = snap(&emu4.nds_mmu);
                        let ah = snap_hw(&emu4.nds_mmu);
                        let w = emu4.nds_mmu.read_word_arm9(0x027e_3744);
                        if w != watch_prev && watch_logs < 40 {
                            watch_logs += 1;
                            ev.push(format!(
                                "f{frame} A9@{pc:#010x} WATCH[0x027e3744] {watch_prev:#010x}->{w:#010x}"
                            ));
                        }
                        watch_prev = w;
                        let bt9 = emu4.nds_mmu.read_halfword_arm9(0x027f_ffa8);
                        if bt9 != bootbuf_prev && bootbuf_logs < 12 {
                            bootbuf_logs += 1;
                            eprintln!("  f{frame} A9@{pc:#010x} BOOTBUF[0x027fffa8] {bootbuf_prev:#06x}->{bt9:#06x}");
                        }
                        bootbuf_prev = bt9;
                        let w2 = emu4.nds_mmu.read_word_arm9(0x0222_5024);
                        if w2 != watch2_prev && watch2_logs < 40 {
                            watch2_logs += 1;
                            ev.push(format!(
                                "f{frame} A9@{pc:#010x} WATCH2[0x02225024] {watch2_prev:#010x}->{w2:#010x}"
                            ));
                        }
                        watch2_prev = w2;
                        if emu4.nds_mmu.timers9.control != tctl9_prev {
                            tctl_ring.push(format!(
                                "f{frame} A9@{pc:#010x} TIMER9 ctl {:04x?}->{:04x?}",
                                tctl9_prev, emu4.nds_mmu.timers9.control
                            ));
                            if tctl_ring.len() > 8 {
                                tctl_ring.remove(0);
                            }
                        }
                        tctl9_prev = emu4.nds_mmu.timers9.control;
                        let w4 = emu4.nds_mmu.read_word_arm9(0x021d_43cc);
                        if w4 != watch4_prev {
                            ev.push(format!(
                                "f{frame} A9@{pc:#010x} READY[0x021d43cc] {watch4_prev:#010x}->{w4:#010x}"
                            ));
                        }
                        watch4_prev = w4;
                        let w6 = emu4.nds_mmu.read_word_arm9(0x021d_d404);
                        if w6 != watch6_prev && watch6_logs < 24 {
                            watch6_logs += 1;
                            eprintln!(
                                "  f{frame} A9@{pc:#010x} GXFLAG[0x021dd404] {watch6_prev:#010x}->{w6:#010x}"
                            );
                        }
                        watch6_prev = w6;
                        if frame >= 300 && frame % 400 == 0 && frame != q_last_frame {
                            q_last_frame = frame;
                            eprintln!(
                                "  f{frame} LOOPSTATE [0x021d1176]={:#04x} [0x021d1177]={:#04x} [0x021d116c..]={:#010x} {:#010x} {:#010x} [0x02111860..]={:#010x} {:#010x}",
                                emu4.nds_mmu.read_byte_arm9(0x021d_1176),
                                emu4.nds_mmu.read_byte_arm9(0x021d_1177),
                                emu4.nds_mmu.read_word_arm9(0x021d_116c),
                                emu4.nds_mmu.read_word_arm9(0x021d_1170),
                                emu4.nds_mmu.read_word_arm9(0x021d_1174),
                                emu4.nds_mmu.read_word_arm9(0x0211_1860),
                                emu4.nds_mmu.read_word_arm9(0x0211_1864),
                            );
                        }
                        let w5 = emu4.nds_mmu.read_byte_arm9(0x021d_4400);
                        if w5 != watch5_prev && watch5_logs < 12 {
                            watch5_logs += 1;
                            eprintln!(
                                "  f{frame} A9@{pc:#010x} STREAMSTATE[0x021d4400] {watch5_prev:#04x}->{w5:#04x}"
                            );
                        }
                        watch5_prev = w5;
                        // Sound-init state machine: log each dispatch (state in
                        // r7) and each reply-callback processing (regs) to see
                        // which transition refuses to converge.
                        if frame >= 300 && frame % 100 == 0 && frame != sm_last_frame && sm_logs < 44 {
                            if exec9_early == 0x0209_e9c0 {
                                sm_logs += 1;
                                sm_last_frame = frame;
                                let r = emu4.nds_arm9.cpu.registers.gpr;
                                eprintln!(
                                    "  f{frame} SM-DISPATCH r7={} r6={} [43c8]={:#x} [43cc]={:#x} [43d0]={:#x}",
                                    r[7], r[6],
                                    emu4.nds_mmu.read_word_arm9(0x021d_43c8),
                                    emu4.nds_mmu.read_word_arm9(0x021d_43cc),
                                    emu4.nds_mmu.read_word_arm9(0x021d_43d0),
                                );
                            }
                            if exec9_early == 0x0209_ec6c && st4_dumps < 4 {
                                st4_dumps += 1;
                                let r = emu4.nds_arm9.cpu.registers.gpr;
                                eprintln!(
                                    "  f{frame} ST4-CHECK r10={:#010x} [r10]={:#04x} r0={:#x} r1={:#010x} r2={:#010x} r8={:#010x} r9={:#010x} r11={:#010x}",
                                    r[10],
                                    emu4.nds_mmu.read_byte_arm9(r[10]),
                                    r[0], r[1], r[2], r[8], r[9], r[11]
                                );
                            }
                            if exec9_early == 0x0209_ef0c {
                                sm_logs += 1;
                                let r = emu4.nds_arm9.cpu.registers.gpr;
                                eprintln!(
                                    "  f{frame} SM-CALLBACK r0={:#010x} r1={:#010x} r2={:#010x} r3={:#010x} r4={:#010x}",
                                    r[0], r[1], r[2], r[3], r[4]
                                );
                            }
                        }
                        // Page-0 first-word watch: our pump writes rom[0x1a3600]
                        // (0x43010401) there — catch whoever zeroes it after.
                        if (200..=210).contains(&frame) {
                            let w3 = emu4.nds_mmu.read_word_arm9(0x0221_ba00);
                            if w3 != watch3_prev {
                                ev.push(format!(
                                    "f{frame} A9@{pc:#010x} WATCH3[0x0221ba00] {watch3_prev:#010x}->{w3:#010x}"
                                ));
                            }
                            watch3_prev = w3;
                        }
                        // Per-word port-consumption trace for a couple of pages:
                        // the pc consuming each word tells PIO from DMA apart —
                        // and exactly who eats the first word of every page.
                        if (205..=206).contains(&frame) {
                            let s = emu4.nds_mmu.gamecard.src;
                            if s != gcsrc_prev {
                                ev.push(format!(
                                    "f{frame} A9@{pc:#010x} PORT src {gcsrc_prev:#x}->{s:#x} left={:#x}",
                                    emu4.nds_mmu.gamecard.bytes_left
                                ));
                            }
                            gcsrc_prev = s;
                        }
                        if b != a {
                            ev.push(format!("f{frame} A9@{pc:#010x}{}", delta(&b, &a)));
                        }
                        if bh != ah {
                            let cmd = &emu4.nds_mmu.arm9_io[0x1A8..0x1B0];
                            ev.push(format!(
                                "f{frame} A9@{pc:#010x} HW romctrl:{:#010x}->{:#010x} dma1:{:#010x}->{:#010x} cmd={:02x} gcsrc={:#x} gcleft={:#x} dst1={:#010x}",
                                bh.0, ah.0, bh.2, ah.2,
                                cmd[0],
                                emu4.nds_mmu.gamecard.src,
                                emu4.nds_mmu.gamecard.bytes_left,
                                emu4.nds_mmu.dma9_internal_dst[1]
                            ));
                        }
                    }
                    a9run += run9;
                    let mut run7 = 0u32;
                    while run7 < 32 {
                        let thumb7 = emu4.nds_arm7.cpu.registers.get_flag(crate::nds::cpu::FLAG_T);
                        let pc7 = emu4.nds_arm7.cpu.registers.gpr[15].wrapping_sub(if thumb7 { 4 } else { 8 });
                        if !emu4.nds_arm7.cpu.pc_modified && !emu4.nds_arm7.cpu.halted {
                            let i7 = emu4.nds_arm7.cpu.pipeline[0];
                            let is_swi = if thumb7 {
                                (i7 & 0xFF00) == 0xDF00
                            } else {
                                (i7 >> 28) != 0xF && ((i7 >> 24) & 0xF) == 0xF
                            };
                            if is_swi {
                                swi7_n += 1;
                                if swi7.len() < 20 {
                                    let c = if thumb7 { i7 & 0xFF } else { (i7 >> 16) & 0xFF };
                                    swi7.push((pc7, c));
                                }
                            }
                        }
                        let b = snap(&emu4.nds_mmu);
                        run7 += emu4.nds_arm7.step(&mut emu4.nds_mmu);
                        let a = snap(&emu4.nds_mmu);
                        if b != a {
                            let halted = if emu4.nds_arm7.cpu.halted { "H" } else { "" };
                            let d = delta(&b, &a);
                            if frame >= 350 && reply_dumps < 2 && d.contains("7to9:0->1") {
                                reply_dumps += 1;
                                let sp7 = emu4.nds_arm7.cpu.registers.gpr[13];
                                eprintln!("  A7-REPLY caller chain (sp={sp7:#010x}):");
                                for off in (0..0x40u32).step_by(4) {
                                    let w = emu4.nds_mmu.read_word_arm7(sp7.wrapping_add(off));
                                    let tag = if (0x037f_8000..0x0380_0000).contains(&w)
                                        || (0x0380_0000..0x0381_0000).contains(&w)
                                    {
                                        "  <- code?"
                                    } else {
                                        ""
                                    };
                                    eprintln!("    [sp+{off:#04x}] = {w:#010x}{tag}");
                                }
                            }
                            ev.push(format!(
                                "f{frame} A7@{pc7:#010x}(lr={:#010x}){halted}{d}",
                                emu4.nds_arm7.cpu.registers.gpr[14]
                            ));
                        }
                        // Mode-or-I-bit transition = IRQ entry/exit (or critical
                        // section). If the ARM7 takes ONE IRQ and I never drops
                        // back to 0, its IRQ-driven world (PXI, PM, touch) is dead.
                        let c7 = emu4.nds_arm7.cpu.registers.cpsr;
                        if (c7 ^ a7_prev_cpsr) & 0x9F != 0 && a7_cpsr_events < 40 {
                            a7_cpsr_events += 1;
                            ev.push(format!(
                                "f{frame} A7-CPSR {a7_prev_cpsr:#010x}->{c7:#010x} @pc={pc7:#010x} lr={:#010x}",
                                emu4.nds_arm7.cpu.registers.gpr[14]
                            ));
                        }
                        a7_prev_cpsr = c7;
                        {
                            let t7e = emu4.nds_arm7.cpu.registers.get_flag(crate::nds::cpu::FLAG_T);
                            let e7 = if emu4.nds_arm7.cpu.pc_modified {
                                emu4.nds_arm7.cpu.registers.gpr[15] & !(if t7e { 1 } else { 3 })
                            } else {
                                pc7
                            };
                            if e7 == 0x0380_3e5c && strb_dumps < 8 {
                                strb_dumps += 1;
                                let r = emu4.nds_arm7.cpu.registers.gpr;
                                eprintln!(
                                    "  f{frame} FLASH-STATUS-STRB [{:#010x}] <- {:#04x} lr={:#010x}",
                                    r[0], r[1] & 0xFF, r[14]
                                );
                            }
                            if e7 == 0x0380_0d94 && jobcb_dumps < 4 {
                                jobcb_dumps += 1;
                                let r = emu4.nds_arm7.cpu.registers.gpr;
                                eprintln!(
                                    "  f{frame} JOB-CB-EXEC r0={:#x} r1={:#010x} r2={:#010x} lr={:#010x}",
                                    r[0], r[1], r[2], r[14]
                                );
                            }
                        }
                        // ARM7 sound-driver reply generator: r6 holds the packed
                        // reply, LR points into the sub-handler that produced
                        // the result byte (the "3" the ARM9 rejects).
                        {
                            let t7 = emu4.nds_arm7.cpu.registers.get_flag(crate::nds::cpu::FLAG_T);
                            let exec7 = if emu4.nds_arm7.cpu.pc_modified {
                                emu4.nds_arm7.cpu.registers.gpr[15] & !(if t7 { 1 } else { 3 })
                            } else {
                                pc7
                            };
                            if exec7 == 0x0380_0ba0 && frame >= 300 && replygen_dumps < 3 {
                                replygen_dumps += 1;
                                let r = emu4.nds_arm7.cpu.registers.gpr;
                                eprintln!(
                                    "  A7 REPLY-GEN f{frame}: r0={:#x} r1={:#010x} r2={:#010x} r4={:#x} r6={:#010x} lr={:#010x} sp={:#010x}",
                                    r[0], r[1], r[2], r[4], r[6], r[14], r[13]
                                );
                                for off in (0..0x28u32).step_by(4) {
                                    let w = emu4.nds_mmu.read_word_arm7(r[13].wrapping_add(off));
                                    eprintln!("    [sp+{off:#04x}] = {w:#010x}");
                                }
                            }
                        }
                        let bt = emu4.nds_mmu.read_halfword_arm7(0x027f_ffa8);
                        if bt != bootbuf_prev && bootbuf_logs < 12 {
                            bootbuf_logs += 1;
                            eprintln!("  f{frame} A7@{pc7:#010x} BOOTBUF[0x027fffa8] {bootbuf_prev:#06x}->{bt:#06x}");
                        }
                        bootbuf_prev = bt;
                        if emu4.nds_mmu.timers7.control != tctl7_prev {
                            tctl_ring.push(format!(
                                "f{frame} A7@{pc7:#010x} TIMER7 ctl {:04x?}->{:04x?}",
                                tctl7_prev, emu4.nds_mmu.timers7.control
                            ));
                            if tctl_ring.len() > 8 {
                                tctl_ring.remove(0);
                            }
                        }
                        tctl7_prev = emu4.nds_mmu.timers7.control;
                        let w4 = emu4.nds_mmu.read_word_arm9(0x021d_43cc);
                        if w4 != watch4_prev {
                            ev.push(format!(
                                "f{frame} A7@{pc7:#010x} READY[0x021d43cc] {watch4_prev:#010x}->{w4:#010x}"
                            ));
                        }
                        watch4_prev = w4;
                        if frame >= 4700 {
                            prof_n7 += 1;
                            if prof_n7 % 4 == 0 {
                                let key = if emu4.nds_arm7.cpu.halted {
                                    0xFFFF_FFFF
                                } else {
                                    let t7 = emu4.nds_arm7.cpu.registers.get_flag(crate::nds::cpu::FLAG_T);
                                    if emu4.nds_arm7.cpu.pc_modified {
                                        emu4.nds_arm7.cpu.registers.gpr[15] & !(if t7 { 1 } else { 3 })
                                    } else {
                                        emu4.nds_arm7.cpu.registers.gpr[15].wrapping_sub(if t7 { 4 } else { 8 })
                                    }
                                };
                                *prof7.entry(key).or_insert(0) += 1;
                            }
                        }
                    }
                    emu4.nds_mmu.tick_nds_timers(run9); // mirror tick()'s cadence
                    emu4.nds_mmu.tick_apu(run9, &mut abuf, 0, 1.0);
                    emu4.nds_ppu.tick(run9, &mut emu4.nds_mmu, &mut vo_buf, false);
                }
                // Drain this frame's mixed audio into the RMS/peak accumulators
                // and rewind the resampler cursor so `abuf` never overflows.
                let n = emu4.nds_mmu.apu.resampler.sample_count.min(abuf.len() / 2);
                for &s in &abuf[..n * 2] {
                    rms_acc += (s as f64) * (s as f64);
                    rms_n += 1;
                    let a = (s as i32).abs();
                    if a > audio_peak {
                        audio_peak = a;
                        if audio_first_frame == u32::MAX && a > 0 {
                            audio_first_frame = frame;
                        }
                    }
                }
                emu4.nds_mmu.apu.resampler.sample_count = 0;
                let psg_live = emu4
                    .nds_mmu
                    .apu
                    .channels
                    .iter()
                    .filter(|c| c.active && c.format() == 3)
                    .count();
                psg_max_live = psg_max_live.max(psg_live);
                if psg_live > 0 && psg_first_frame == u32::MAX {
                    psg_first_frame = frame;
                }
                // Scene-change evidence around the input events: a full-frame
                // hash that differs across a tap proves a touch-driven change.
                // U27: a dense strip through the intro (workstream D — which
                // BG modes/fade regs the transitions use) and around every
                // tap (workstream A — map the shifted timeline).
                // Widened to 4600: the CLOSING part of the intro (the sky shot
                // around f4150, where the reported glitch patch appears) fell
                // outside the old 600..=3800 window, so no per-frame 2D/3D
                // state was ever captured for it.
                let intro_strip = (600..=4600).contains(&frame) && frame % 50 == 0;
                let tap_strip = (4600..=7790).contains(&frame) && frame % 50 == 0;
                if intro_strip
                    || tap_strip
                    || matches!(frame, 3050 | 3340 | 3360 | 3380 | 4095 | 4150 | 4250 | 4390 | 4500)
                {
                    let mut fb = vec![0u16; 256 * 384];
                    for ly in 0..192u16 {
                        emu4.nds_ppu.render_scanline(ly, &emu4.nds_mmu, &mut fb);
                    }
                    let mut hh = 0u64;
                    for &p in &fb {
                        hh = hh.wrapping_mul(31).wrapping_add(p as u64);
                    }
                    let distinct: std::collections::HashSet<u16> = fb.iter().copied().collect();
                    // Game-frame vs PPU-frame rate evidence (U30): the game's
                    // VBlank counter HW_VBLANK_COUNT_BUF (0x027FFC3C). ~1:1
                    // with `frame` = healthy VBlank waits; ~3:1 = the main
                    // loop spins multiple game-frames per real frame.
                    let vb = u32::from_le_bytes(
                        // 0x027FFC3C is a main-RAM mirror: offset % 4 MB.
                        emu4.nds_mmu.main_ram[0x3FFC3C..0x3FFC40].try_into().unwrap(),
                    );
                    eprintln!(
                        "  FRAMEHASH f{frame}: {hh:#018x} ({} distinct colors) vbcnt={vb}",
                        distinct.len()
                    );
                    if intro_strip {
                        // U27 workstream D: name the BG types + fade/blend
                        // registers each transition frame actually uses. The
                        // text-only renderer skips any non-text BG, so a mode
                        // != 0 or an affine BGxCNT here = a dropped layer.
                        let io = &emu4.nds_mmu.arm9_io;
                        let rh = |b: usize| u16::from_le_bytes([io[b], io[b + 1]]);
                        let rw = |b: usize| u32::from_le_bytes([io[b], io[b + 1], io[b + 2], io[b + 3]]);
                        eprintln!(
                            "    2D f{frame}: A mode={} en={:#04x} bgcnt=[{:04x} {:04x} {:04x} {:04x}] bright={:04x} bld={:04x}/{:04x} pa..pd=[{:04x} {:04x} {:04x} {:04x}] | B mode={} en={:#04x} bgcnt=[{:04x} {:04x} {:04x} {:04x}] bright={:04x} bld={:04x}/{:04x} xpA={} xpB={} H={:02x}",
                            rw(0x000) & 7, (rw(0x000) >> 8) & 0x1F,
                            rh(0x008), rh(0x00A), rh(0x00C), rh(0x00E),
                            rh(0x06C), rh(0x050), rh(0x052),
                            rh(0x020), rh(0x022), rh(0x024), rh(0x026),
                            rw(0x1000) & 7, (rw(0x1000) >> 8) & 0x1F,
                            rh(0x1008), rh(0x100A), rh(0x100C), rh(0x100E),
                            rh(0x106C), rh(0x1050), rh(0x1052),
                            // U31d: ext-palette enables + bank H (engine-B BG
                            // ext-pal slot) — the corrupt Unown band suspect.
                            rw(0x000) >> 30, rw(0x1000) >> 30,
                            emu4.nds_mmu.vram.banks[7].control,
                        );
                        // 3D-layer state for the same frame. If the 3D BG0 is
                        // enabled but the engine drew nothing, the "unloaded
                        // patch" is a missing 3D layer rather than a 2D one.
                        let e = &emu4.nds_mmu.gx.engine;
                        eprintln!(
                            "    3D f{frame}: bg0_3d={} tris={} culled={} opaque_px={} clear={:04x} disp3dcnt={:04x} swaps={}",
                            (rw(0x000) >> 3) & 1,
                            e.last_frame_tris,
                            e.last_culled,
                            e.fb.iter().filter(|&&p| p & 0x8000 != 0).count(),
                            e.clear_px,
                            rh(0x060),
                            e.swap_count,
                        );
                    }
                    if matches!(frame, 900 | 1400 | 1500 | 1600 | 2000 | 2100 | 2300 | 3000 | 3050 | 3300 | 3340 | 3360 | 3380 | 3400 | 4150 | 4220 | 4600 | 5000 | 5500 | 6000 | 6500 | 7000 | 7500 | 7750)
                        || (std::env::var("PROBE_TITLE_DWELL").is_ok()
                            && matches!(frame, 4600 | 5000 | 5400))
                    {
                        // Per-scene 3D evidence (U30 defect table): geometry
                        // submitted vs rasterized at each dumped scene.
                        let eng = &emu4.nds_mmu.gx.engine;
                        eprintln!(
                            "    GX3D f{frame}: last_frame_tris={} fb_opaque={} clear_px={:#06x}",
                            eng.last_frame_tris,
                            eng.fb.iter().filter(|&&p| p & 0x8000 != 0).count(),
                            eng.clear_px,
                        );
                        // Transition-glitch visual evidence for workstream D.
                        let mut ppm = b"P6\n256 384\n255\n".to_vec();
                        for &px in &fb {
                            ppm.extend_from_slice(&[
                                ((px & 0x1F) << 3) as u8,
                                (((px >> 5) & 0x1F) << 3) as u8,
                                (((px >> 10) & 0x1F) << 3) as u8,
                            ]);
                        }
                        let _ = std::fs::write(
                            format!(
                                concat!(env!("CARGO_MANIFEST_DIR"), "/../frame_dump_intro_f{}.ppm"),
                                frame
                            ),
                            &ppm,
                        );
                    }
                    if frame == 4095 && touch_only {
                        // Pre-tap reference image (the title screen).
                        let mut ppm = b"P6\n256 384\n255\n".to_vec();
                        for &px in &fb {
                            ppm.extend_from_slice(&[
                                ((px & 0x1F) << 3) as u8,
                                (((px >> 5) & 0x1F) << 3) as u8,
                                (((px >> 10) & 0x1F) << 3) as u8,
                            ]);
                        }
                        let _ = std::fs::write(
                            concat!(env!("CARGO_MANIFEST_DIR"), "/../frame_dump_title.ppm"),
                            &ppm,
                        );
                        eprintln!("  wrote frame_dump_title.ppm (pre-tap title)");
                    }
                }
            }
            eprintln!(
                "  PSG: first active frame={} max simultaneous={}",
                if psg_first_frame == u32::MAX { -1i64 } else { psg_first_frame as i64 },
                psg_max_live
            );
            eprintln!(
                "  AUDIO: {} samples mixed, RMS={:.1}, peak={}, first nonzero frame={}",
                rms_n,
                if rms_n > 0 { (rms_acc / rms_n as f64).sqrt() } else { 0.0 },
                audio_peak,
                if audio_first_frame == u32::MAX { -1i64 } else { audio_first_frame as i64 }
            );
            if !reached {
                eprintln!("  ARM9 did NOT reach the hang in 6200 frames (diverged or progressed)");
                eprintln!(
                    "  final: a9={:#010x} a7={:#010x} DISPCNT_A={:#010x}",
                    emu4.nds_arm9.cpu.registers.gpr[15],
                    emu4.nds_arm7.cpu.registers.gpr[15],
                    rd32(&emu4.nds_mmu.arm9_io, 0x000)
                );
            }
            // FREEZE evidence (U21): the steady-state profile pinned the ARM9 to a
            // 9-instruction loop split across 0x020cf720-72c / 0x020cf9d8-9e8.
            // Dump both clusters + full regs so the poll target can be named.
            {
                let r = emu4.nds_arm9.cpu.registers.gpr;
                eprintln!(
                    "  FINAL A9 regs: r0={:#010x} r1={:#010x} r2={:#010x} r3={:#010x} r4={:#010x} r5={:#010x} r6={:#010x} r7={:#010x} r8={:#010x} r9={:#010x} r10={:#010x} r11={:#010x} r12={:#010x} sp={:#010x} lr={:#010x}",
                    r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], r[8], r[9], r[10], r[11], r[12], r[13], r[14]
                );
                for (name, lo, hi) in [
                    // U21 second stall: 3-instr poll of [0x021dd404] (lr=0x020c25ec).
                    ("post-GX spin @0x020c2430", 0x020c_2430u32, 0x020c_2490u32),
                    ("post-GX spin caller @0x020c25a0", 0x020c_25a0, 0x020c_2610),
                ] {
                    eprintln!("  {name}:");
                    for a in (lo..hi).step_by(4) {
                        eprintln!("    {a:#010x}: {:08x}", emu4.nds_mmu.read_word_arm9(a));
                    }
                }
                eprintln!("  GXFLAG [0x021dd404] = {:#010x}", emu4.nds_mmu.read_word_arm9(0x021d_d404));
                // U24: name the layer the menu text lives on — full 2D register
                // state of both engines + the VRAM bank map at end of run.
                let rw = |b: usize| u32::from_le_bytes([
                    emu4.nds_mmu.arm9_io[b], emu4.nds_mmu.arm9_io[b + 1],
                    emu4.nds_mmu.arm9_io[b + 2], emu4.nds_mmu.arm9_io[b + 3],
                ]);
                let rh = |b: usize| u16::from_le_bytes([emu4.nds_mmu.arm9_io[b], emu4.nds_mmu.arm9_io[b + 1]]);
                eprintln!(
                    "  2D regs: DISPCNT_A={:#010x} DISPCNT_B={:#010x} POWCNT1={:#06x}",
                    rw(0x000), rw(0x1000), rh(0x304)
                );
                eprintln!(
                    "  BGCNT A=[{:#06x} {:#06x} {:#06x} {:#06x}] B=[{:#06x} {:#06x} {:#06x} {:#06x}]",
                    rh(0x008), rh(0x00A), rh(0x00C), rh(0x00E),
                    rh(0x1008), rh(0x100A), rh(0x100C), rh(0x100E)
                );
                let vc: Vec<String> = (0..9).map(|i| format!("{:02x}", emu4.nds_mmu.vram.banks[i].control)).collect();
                eprintln!("  VRAMCNT=[{}]", vc.join(" "));
                for chn in 0..4usize {
                    let b = 0xB0 + chn * 0x0C;
                    eprintln!(
                        "  DMA9 ch{chn}: SAD={:#010x} DAD={:#010x} CNT={:#010x}",
                        u32::from_le_bytes(emu4.nds_mmu.arm9_io[b..b + 4].try_into().unwrap()),
                        u32::from_le_bytes(emu4.nds_mmu.arm9_io[b + 4..b + 8].try_into().unwrap()),
                        u32::from_le_bytes(emu4.nds_mmu.arm9_io[b + 8..b + 12].try_into().unwrap()),
                    );
                }
                let io = &emu4.nds_mmu.arm9_io;
                let w = |b: usize| u32::from_le_bytes([io[b], io[b + 1], io[b + 2], io[b + 3]]);
                eprintln!(
                    "  GX state: has_3d_activity={} DISP3DCNT={:#010x} GXSTAT_raw={:#010x} SWAP(0x540)={:#010x} VCOUNT={} DISPSTAT9={:#06x}",
                    emu4.nds_mmu.has_3d_activity, w(0x60), w(0x600), w(0x540),
                    u16::from_le_bytes([io[6], io[7]]), u16::from_le_bytes([io[4], io[5]])
                );
                // APU evidence: live channel state (the APU owns these registers
                // now — arm7_io no longer sees sound writes).
                let apu = &emu4.nds_mmu.apu;
                eprintln!(
                    "  SOUNDCNT={:#06x} SOUNDBIAS={:#06x}",
                    apu.soundcnt, apu.soundbias
                );
                for (i, ch) in apu.channels.iter().enumerate() {
                    if ch.cnt != 0 || ch.sad != 0 {
                        // U25 pitch sanity: effective source sample rate in Hz
                        // (bus clock / period); plausible instruments = 8-48 kHz.
                        let rate = crate::nds::apu::NDS_CYCLES_PER_SEC
                            / ch.period_cycles() as f64;
                        eprintln!(
                            "  SND ch{i}: CNT={:#010x} (vol={} div={} pan={} fmt={} rep={} duty={} active={}) SAD={:#010x} TMR={:#06x} PNT={:#06x} LEN={:#07x} cursor={:#x} rate={rate:.0}Hz",
                            ch.cnt, ch.cnt & 0x7F, (ch.cnt >> 8) & 3, (ch.cnt >> 16) & 0x7F,
                            ch.format(), ch.repeat_mode(), (ch.cnt >> 24) & 7, ch.active,
                            ch.sad, ch.tmr, ch.pnt, ch.len, ch.cursor
                        );
                    }
                }
                // U25 touch evidence: did the ARM7 driver ever start a TSC
                // conversion (per channel: 1=Y 5=X 3=Z1 4=Z2), and is the SPI/
                // pen IRQ world armed?
                eprintln!(
                    "  TOUCH: tsc conv_counts(ch0..7)={:?} spicnt={:#06x} extkeyin={:#06x} ie7 b22(hinge)={} b23(spi)={} if7 b22={} b23={}",
                    emu4.nds_mmu.spi.tsc.conv_counts,
                    emu4.nds_mmu.spi.spicnt,
                    emu4.nds_mmu.get_extkeyin(),
                    (emu4.nds_mmu.arm7_ie >> 22) & 1,
                    (emu4.nds_mmu.arm7_ie >> 23) & 1,
                    (emu4.nds_mmu.arm7_if >> 22) & 1,
                    (emu4.nds_mmu.arm7_if >> 23) & 1,
                );
                // U26: raw TSC transactions captured around the info-button
                // taps (in>out hex): names the exact command the driver sends
                // and what we answer — the dead-UI-buttons evidence.
                let tlog = &emu4.nds_mmu.spi.tsc.io_log;
                if !tlog.is_empty() {
                    // All three capture windows (f4690+, f5290+, f5890+) in
                    // full — the raw command bytes are workstream A's primary
                    // evidence and the log is bounded at 4096 entries anyway.
                    eprintln!("  TSC RAW ({} transactions, in>out):", tlog.len());
                    for chunk in tlog.chunks(12) {
                        let s: Vec<String> =
                            chunk.iter().map(|(i, o)| format!("{i:02x}>{o:02x}")).collect();
                        eprintln!("    {}", s.join(" "));
                    }
                }
                // U27: where did the ARM7 write the tap's TSC values? The
                // unique-address list IS the TP sample struct candidate set.
                {
                    let log = &emu4.nds_mmu.tp_watch_log;
                    let mut per_addr: std::collections::HashMap<u32, u32> = Default::default();
                    for &(a, _) in log {
                        *per_addr.entry(a).or_insert(0) += 1;
                    }
                    let mut addrs: Vec<_> = per_addr.into_iter().collect();
                    addrs.sort_unstable();
                    eprintln!(
                        "  TPWATCH: {} value-matched ARM7 writes (f4695-4770), {} unique addrs",
                        log.len(),
                        addrs.len()
                    );
                    for (a, n) in addrs.iter().take(32) {
                        eprintln!("    [{a:#010x}] x{n}");
                    }
                    for chunk in log.iter().take(60).collect::<Vec<_>>().chunks(6) {
                        let s: Vec<String> =
                            chunk.iter().map(|(a, v)| format!("{a:08x}={v:08x}")).collect();
                        eprintln!("    seq: {}", s.join(" "));
                    }
                }
                // U27: PXI traffic across the f4700 tap — the ARM9's only
                // window into the ARM7-private TP sampler. Direction 9/7 =
                // sender; payloads carry the SDK tag in the low bits.
                {
                    eprintln!(
                        "  TAG7: sent={} rx7={} dropped_full={}",
                        emu4.nds_mmu.tag7_sent, emu4.nds_mmu.tag7_rx7, emu4.nds_mmu.tag7_dropped_full
                    );
                    let log = &emu4.nds_mmu.fifo_log;
                    eprintln!("  FIFOLOG: {} entries (f4695-4770; 9/7=send, 1=a7 rx, 3=a9 rx, F=dropped)", log.len());
                    for chunk in log.chunks(6) {
                        let s: Vec<String> =
                            chunk.iter().map(|(d, v)| format!("{d}>{v:08x}")).collect();
                        eprintln!("    {}", s.join(" "));
                    }
                }
                // U27 workstream B: sound-capture evidence. An armed capture
                // (cnt bit7) whose DAD matches a looping ch1/ch3 SAD = the
                // SDK reverb path that silently plays stale RAM today.
                {
                    let apu = &emu4.nds_mmu.apu;
                    eprintln!(
                        "  SNDCAP: cnt=[{:#04x} {:#04x}] dad=[{:#010x} {:#010x}] len(words)=[{:#06x} {:#06x}] writes_logged={}",
                        apu.cap_cnt[0], apu.cap_cnt[1], apu.cap_dad[0], apu.cap_dad[1],
                        apu.cap_len[0], apu.cap_len[1], apu.cap_write_log.len()
                    );
                    for chunk in apu.cap_write_log.chunks(8) {
                        let s: Vec<String> =
                            chunk.iter().map(|(o, v)| format!("{o:#05x}={v:02x}")).collect();
                        eprintln!("    cap writes: {}", s.join(" "));
                    }
                }
                // W0.1: AUXSPI (cartridge backup chip) census. The gating
                // question for the whole save workstream is whether the ARM7
                // backup driver is reached at all; if `writes` is 0 the bug is
                // upstream of the chip and no chip model can help.
                //
                // Frame reconstruction mirrors `NdsMmu::write_auxspi_byte`:
                // AUXSPICNT bit 6 means "deselect AFTER this transfer", so a
                // frame ends on the DATA write that was clocked with bit 6
                // clear, or when bit 15 powers the slot down — never on a
                // control write alone. The first byte of each frame is the
                // opcode; the three bytes after an address-taking opcode are
                // accumulated big-endian, which makes `max_addr` a direct
                // measurement of the chip size the driver assumes.
                {
                    let log = &emu4.nds_mmu.aux_bus_log;
                    let mut opcodes = std::collections::BTreeMap::<u8, u32>::new();
                    let mut cnt_values = std::collections::BTreeSet::<u16>::new();
                    let mut data_writes = 0u32;
                    let mut max_addr = 0u32;
                    let mut cnt_lo = 0u8;
                    let mut cnt_hi = 0u8;
                    let mut selected = false;
                    let mut addr_left = 0u8;
                    let mut addr_acc = 0u32;
                    // Whole frames, byte for byte — the opcode histogram alone
                    // cannot show what a multi-byte transaction actually looked
                    // like, and that shape is what has to match the driver.
                    let mut frame: Vec<u8> = Vec::new();
                    let mut frames = std::collections::BTreeMap::<String, u32>::new();
                    let mut per_core = [0u32; 2];
                    // Tag layout is `register_index | core_bit`; see
                    // `NdsMmu::note_aux_write`. Index 0/1 = AUXSPICNT low/high,
                    // 2 = AUXSPIDATA.
                    for &(reg, val) in log.iter() {
                        per_core[usize::from(reg & 0x80 != 0)] += 1;
                        match reg & 0x7F {
                            0 | 1 => {
                                if reg & 0x7F == 0 {
                                    cnt_lo = val;
                                } else {
                                    cnt_hi = val;
                                }
                                cnt_values.insert(u16::from_le_bytes([cnt_lo, cnt_hi]));
                                // Only powering the slot down ends a frame here.
                                if cnt_hi & 0x80 == 0 {
                                    selected = false;
                                    addr_left = 0;
                                }
                            }
                            2 => {
                                data_writes += 1;
                                // Mirror the chip: a leading 0x00 is a
                                // bus-settling byte, not an opcode, so it must
                                // not consume the command slot here either or
                                // every address phase decodes one byte late.
                                if !selected && val == 0x00 {
                                    frame.push(val);
                                    if cnt_lo & 0x40 == 0 {
                                        selected = false;
                                    }
                                } else if !selected {
                                    selected = true;
                                    *opcodes.entry(val).or_default() += 1;
                                    // READ / PP / PW / PE / SE take a 3-byte address.
                                    addr_left = match val {
                                        0x03 | 0x02 | 0x0A | 0xDB | 0xD8 => 3,
                                        _ => 0,
                                    };
                                    addr_acc = 0;
                                } else if addr_left > 0 {
                                    addr_acc = (addr_acc << 8) | val as u32;
                                    addr_left -= 1;
                                    if addr_left == 0 {
                                        max_addr = max_addr.max(addr_acc);
                                    }
                                }
                                frame.push(val);
                                // Hold clear on the byte just clocked ends the frame.
                                if cnt_lo & 0x40 == 0 {
                                    selected = false;
                                    addr_left = 0;
                                }
                                if !selected && !frame.is_empty() {
                                    let key: Vec<String> =
                                        frame.iter().map(|b| format!("{b:02x}")).collect();
                                    *frames.entry(key.join(" ")).or_default() += 1;
                                    frame.clear();
                                }
                            }
                            _ => {}
                        }
                    }
                    let ops: Vec<String> =
                        opcodes.iter().map(|(o, n)| format!("{o:#04x}x{n}")).collect();
                    let cnts: Vec<String> =
                        cnt_values.iter().map(|c| format!("{c:#06x}")).collect();
                    // Slot ownership (EXMEMCNT bit 11) at the time of each write,
                    // split by the core that issued it. If the ARM7 drives the
                    // backup chip while the ARM9 still owns the slot, hardware
                    // would ignore those writes and we would not.
                    let mut owned_by_arm7 = [0u32; 2];
                    for &(reg, _) in log.iter() {
                        if reg & 0x40 != 0 {
                            owned_by_arm7[usize::from(reg & 0x80 != 0)] += 1;
                        }
                    }
                    eprintln!(
                        "  AUXSPI: entries={} (arm7={} arm9={}) dropped={} data_writes={} max_addr={:#08x} cross_deselects={}",
                        log.len(),
                        per_core[0],
                        per_core[1],
                        emu4.nds_mmu.aux_bus_dropped,
                        data_writes,
                        max_addr,
                        emu4.nds_mmu.aux_cross_deselects
                    );
                    eprintln!(
                        "    AUXSPI writes issued while ARM7 owned the slot: arm7={}/{} arm9={}/{}",
                        owned_by_arm7[0], per_core[0], owned_by_arm7[1], per_core[1]
                    );
                    eprintln!(
                        "    AUXSPI arm7 gamecard writes={} block_starts={}",
                        emu4.nds_mmu.aux7_card_writes, emu4.nds_mmu.aux7_block_starts
                    );
                    // Backup command channel. Boot reads 2x0x23000 bytes in
                    // 256-byte chunks before the first frame, so a healthy run
                    // shows ~1120 op-6 requests; anything less localises the
                    // failure to the ARM9 request side, not the chip model.
                    {
                        let m = &emu4.nds_mmu;
                        let ops: Vec<String> = m
                            .tag11_9to7
                            .iter()
                            .enumerate()
                            .filter(|(_, &n)| n > 0)
                            .map(|(op, n)| format!("op{op}x{n}"))
                            .collect();
                        eprintln!(
                            "    PXI tag0B: 9->7 {} (wide={}) | 7->9 replies={}",
                            if ops.is_empty() { "none".into() } else { ops.join(" ") },
                            m.tag11_9to7_wide,
                            m.tag11_7to9
                        );
                        let head: Vec<String> = m
                            .tag11_head
                            .iter()
                            .take(24)
                            .map(|(d, v)| format!("{d}:{:08x}", v))
                            .collect();
                        eprintln!("    PXI tag0B head: {}", head.join(" "));
                        eprintln!("    PXI tag0B argptr={:#010x}", m.tag11_argptr);
                        for (op, a) in m.tag11_args.iter() {
                            let w = |o: usize| {
                                u32::from_le_bytes([a[o], a[o + 1], a[o + 2], a[o + 3]])
                            };
                            eprintln!(
                                "      op{op}: mask[0x58]={:#010x} allows={} src[0x0c]={:#010x} dst[0x10]={:#010x} len[0x14]={:#010x} res[0x18]={:#010x}",
                                w(0x58),
                                (w(0x58) >> op) & 1,
                                w(0x0C),
                                w(0x10),
                                w(0x14),
                                w(0x18)
                            );
                        }
                    }
                    eprintln!("    AUXSPI cnt values: {}", cnts.join(" "));
                    eprintln!("    AUXSPI opcodes:    {}", ops.join(" "));
                    // Frames run to hundreds of bytes once real reads flow;
                    // print the command+address head, which is what identifies
                    // the transaction.
                    for (f, n) in frames.iter().take(24) {
                        let head: String = f.chars().take(23).collect();
                        eprintln!("    AUXSPI frame x{n:<5} [{head}...] len={}", (f.len() + 1) / 3);
                    }
                    let io: Vec<String> = emu4
                        .nds_mmu
                        .aux_io_log
                        .iter()
                        .take(40)
                        .map(|(w, r)| format!("{w:02x}>{r:02x}"))
                        .collect();
                    eprintln!("    AUXSPI io (written>returned): {}", io.join(" "));
                    let fmt_entry = |r: u8, v: u8| {
                        let name = ["CNTL", "CNTH", "DATA", "DAT1"][(r & 0x03) as usize];
                        format!(
                            "{}{name}{}={v:02x}",
                            if r & 0x80 != 0 { "9:" } else { "7:" },
                            if r & 0x40 != 0 { "*" } else { "" }
                        )
                    };
                    for (label, want9) in [("arm7", false), ("arm9", true)] {
                        let head: Vec<String> = log
                            .iter()
                            .filter(|(r, _)| (r & 0x80 != 0) == want9)
                            .take(40)
                            .map(|&(r, v)| fmt_entry(r, v))
                            .collect();
                        eprintln!("    AUXSPI head {label}:  {}", head.join(" "));
                    }
                    // Interleaved view: the ordering is what shows one core
                    // tearing the other's transaction apart.
                    let mixed: Vec<String> = log
                        .iter()
                        .skip_while(|(r, _)| r & 0x80 != 0)
                        .take(40)
                        .map(|&(r, v)| fmt_entry(r, v))
                        .collect();
                    eprintln!("    AUXSPI interleaved: {}", mixed.join(" "));
                }
                {
                    let a = &emu4.nds_mmu.apu;
                    eprintln!(
                        "  AUDIO: peak={:.4} clip={}/{} soundcnt_seen={:#06x} routing_bits={:#x} ch13_bypass={}",
                        a.dbg_peak,
                        a.dbg_clip,
                        a.dbg_samples,
                        a.dbg_soundcnt_seen,
                        (a.dbg_soundcnt_seen >> 8) & 0xF,
                        (a.dbg_soundcnt_seen >> 12) & 0x3
                    );
                }
                // W1: NDS DMA census. VBlank/HBlank/display-sync timings are
                // armed-but-never-run today; a timing whose `armed` count is
                // nonzero while `fired` stays 0 is a channel the game waits on
                // that can never complete.
                {
                    let m = &emu4.nds_mmu;
                    const NAMES: [&str; 8] = [
                        "immediate", "vblank", "hblank", "dispsync", "mainmem", "cardslot",
                        "gbaslot", "gxfifo",
                    ];
                    for (core, label) in [(0usize, "arm9"), (1, "arm7")] {
                        let rows: Vec<String> = (0..8)
                            .filter(|&t| m.dma_armed[core][t] > 0 || m.dma_fired[core][t] > 0)
                            .map(|t| {
                                format!(
                                    "{}(armed={} fired={} units={})",
                                    NAMES[t],
                                    m.dma_armed[core][t],
                                    m.dma_fired[core][t],
                                    m.dma_units[core][t]
                                )
                            })
                            .collect();
                        eprintln!(
                            "  DMA {label}: max_count={:#x} {}",
                            m.dma_max_count[core],
                            if rows.is_empty() { "none".into() } else { rows.join(" ") }
                        );
                    }
                }
                // U27 workstream C: which geometry commands the game actually
                // issues — the 3D milestone implements exactly this list.
                {
                    let gx = &emu4.nds_mmu.gx;
                    eprintln!(
                        "  GX: total={} unknown_cmd_bytes={} tex_fmts(0=none,1=A3I5,2=pal4,3=pal16,4=pal256,5=4x4c,6=A5I3,7=direct)={:?} begin(tri,quad,tstrip,qstrip)={:?}",
                        gx.total(), gx.unknown_cmds, gx.tex_fmt_histo, gx.begin_histo
                    );
                    eprintln!(
                        "  GXATTR: mode(0=mod,1=decal,2=toon,3=SHADOW)={:?} cull(0=none,1=back,2=front,3=both)={:?} fmt_tris={:?} culled={} ({:.1}% of submitted)",
                        gx.engine.attr_mode,
                        gx.engine.attr_cull,
                        gx.engine.fmt_tris,
                        gx.engine.last_culled,
                        100.0 * gx.engine.last_culled as f64
                            / (gx.engine.attr_cull.iter().sum::<u32>().max(1) as f64)
                    );
                    eprintln!(
                        "  GX3D: swaps={} max_tris/frame={} dropped={} fb_opaque_px={}",
                        gx.engine.swap_count,
                        gx.engine.max_tris_per_frame,
                        gx.engine.tris_dropped,
                        gx.engine.fb.iter().filter(|&&p| p & 0x8000 != 0).count()
                    );
                    for (cmd, &n) in gx.histo.iter().enumerate() {
                        if n > 0 {
                            eprintln!(
                                "    GXCMD {:#04x} {:<14} x{n}",
                                cmd,
                                crate::nds::gx::cmd_name(cmd as u8)
                            );
                        }
                    }
                    for (i, (cmd, params)) in gx.trace.iter().take(96).enumerate() {
                        let ps: Vec<String> = params.iter().map(|p| format!("{p:08x}")).collect();
                        eprintln!(
                            "    GXTRACE[{i:3}] {:<14} {}",
                            crate::nds::gx::cmd_name(*cmd),
                            ps.join(" ")
                        );
                    }
                    // U32: unique texture/palette pairs across the whole run.
                    for (i, &(tp, pb)) in gx.engine.tex_pairs.iter().enumerate() {
                        eprintln!(
                            "    TEXPAIR[{i:2}] fmt={} {}x{} texgen={} c0={} addr={:#07x} pal_addr={:#07x}",
                            (tp >> 26) & 7,
                            8 << ((tp >> 20) & 7),
                            8 << ((tp >> 23) & 7),
                            (tp >> 30) & 3,
                            (tp >> 29) & 1,
                            (tp & 0xFFFF) * 8,
                            if (tp >> 26) & 7 == 2 { pb * 8 } else { pb * 16 },
                        );
                    }
                    // U31: every traced TEXIMAGE_PARAM decoded — names the
                    // formats/texgen modes the title's textures actually use.
                    for (i, (cmd, params)) in
                        gx.trace.iter().enumerate().filter(|(_, (c, _))| *c == 0x2A).take(40)
                    {
                        let _ = cmd;
                        let p = params.first().copied().unwrap_or(0);
                        eprintln!(
                            "    GXTEX[{i:3}] fmt={} {}x{} texgen={} color0={} repeat={}{} flip={}{} addr={:#07x}",
                            (p >> 26) & 7,
                            8 << ((p >> 20) & 7),
                            8 << ((p >> 23) & 7),
                            (p >> 30) & 3,
                            (p >> 29) & 1,
                            (p >> 16) & 1, (p >> 17) & 1,
                            (p >> 18) & 1, (p >> 19) & 1,
                            (p & 0xFFFF) * 8,
                        );
                    }
                }
                // U25 sprite-garbage evidence: decode every populated OAM entry
                // of both engines at the end-of-run screen, plus the DISPCNT
                // OBJ bits draw_objs() keys off — names exactly which OBJ
                // feature the garbled sprites need.
                for (eng, base, dcnt) in [("A", 0usize, rw(0x000)), ("B", 0x400, rw(0x1000))] {
                    eprintln!(
                        "  OAM {eng}: DISPCNT={dcnt:#010x} obj_en={} 1D={} bound={} bmp_map={} bmp_1d_bound={} extpal_obj={} extpal_bg={}",
                        (dcnt >> 12) & 1, (dcnt >> 4) & 1, (dcnt >> 20) & 3,
                        (dcnt >> 5) & 3, (dcnt >> 22) & 1, (dcnt >> 31) & 1, (dcnt >> 30) & 1
                    );
                    let mut shown = 0;
                    for i in 0..128usize {
                        let at = base + i * 8;
                        let a0 = u16::from_le_bytes([emu4.nds_mmu.oam[at], emu4.nds_mmu.oam[at + 1]]);
                        let a1 = u16::from_le_bytes([emu4.nds_mmu.oam[at + 2], emu4.nds_mmu.oam[at + 3]]);
                        let a2 = u16::from_le_bytes([emu4.nds_mmu.oam[at + 4], emu4.nds_mmu.oam[at + 5]]);
                        if a0 == 0 && a1 == 0 && a2 == 0 {
                            continue;
                        }
                        let affine = a0 & 0x100 != 0;
                        if !affine && a0 & 0x200 != 0 {
                            continue; // hidden
                        }
                        if shown == 48 {
                            eprintln!("    ... (more entries omitted)");
                            break;
                        }
                        shown += 1;
                        let x = { let v = (a1 & 0x1FF) as i32; if v >= 256 { v - 512 } else { v } };
                        eprintln!(
                            "    [{i:3}] y={:3} x={x:4} mode={} aff={} dbl={} 256c={} shape={} size={} grp={:2} hv={}{} tile={:#05x} prio={} pal={:2}",
                            a0 & 0xFF, (a0 >> 10) & 3, affine as u8, (a0 >> 9) & 1,
                            (a0 >> 13) & 1, (a0 >> 14) & 3, (a1 >> 14) & 3, (a1 >> 9) & 0x1F,
                            if !affine && a1 & 0x1000 != 0 { "H" } else { "-" },
                            if !affine && a1 & 0x2000 != 0 { "V" } else { "-" },
                            a2 & 0x3FF, (a2 >> 10) & 3, (a2 >> 12) & 0xF
                        );
                    }
                    // Affine parameter groups 0-3 (PA,PB,PC,PD as 8.8 fixed).
                    for g in 0..4usize {
                        let p = |k: usize| {
                            let o = base + g * 32 + 6 + k * 8;
                            i16::from_le_bytes([emu4.nds_mmu.oam[o], emu4.nds_mmu.oam[o + 1]])
                        };
                        eprintln!("    aff grp{g}: PA={:#06x} PB={:#06x} PC={:#06x} PD={:#06x}",
                            p(0), p(1), p(2), p(3));
                    }
                }
            }
            eprintln!(
                "  end state: CNT9={:#06x} CNT7={:#06x} rawctl9={:#06x} rawctl7={:#06x} len9to7={} len7to9={} ie9={:#x} if9={:#x} ime9={:#x} ie7={:#x} if7={:#x} ime7={:#x} a9halt={} a7halt={}",
                emu4.nds_mmu.read_ipc_fifo_cnt_arm9(),
                emu4.nds_mmu.read_ipc_fifo_cnt_arm7(),
                emu4.nds_mmu.ipc.fifo_control_arm9,
                emu4.nds_mmu.ipc.fifo_control_arm7,
                emu4.nds_mmu.ipc.fifo_9to7.len(),
                emu4.nds_mmu.ipc.fifo_7to9.len(),
                emu4.nds_mmu.arm9_ie,
                emu4.nds_mmu.arm9_if,
                emu4.nds_mmu.arm9_ime,
                emu4.nds_mmu.arm7_ie,
                emu4.nds_mmu.arm7_if,
                emu4.nds_mmu.arm7_ime,
                emu4.nds_arm9.cpu.halted,
                emu4.nds_arm7.cpu.halted
            );
            eprintln!(
                "  a7 cpsr={:#010x} (mode={:#04x} I={}) a9 cpsr={:#010x} (mode={:#04x} I={})",
                emu4.nds_arm7.cpu.registers.cpsr,
                emu4.nds_arm7.cpu.registers.cpsr & 0x1F,
                (emu4.nds_arm7.cpu.registers.cpsr >> 7) & 1,
                emu4.nds_arm9.cpu.registers.cpsr,
                emu4.nds_arm9.cpu.registers.cpsr & 0x1F,
                (emu4.nds_arm9.cpu.registers.cpsr >> 7) & 1
            );
            // Render one full frame with the final state and dump it as PPM:
            // the boot has configured DISPCNT/VRAM by now, so this is the
            // game's actual intro image (copyright / Game Freak screen).
            {
                let mut fb = vec![0u16; 256 * 384];
                for ly in 0..192u16 {
                    emu4.nds_ppu.render_scanline(ly, &emu4.nds_mmu, &mut fb);
                }
                let mut ppm = format!("P6
256 384
255
").into_bytes();
                for &px in &fb {
                    let r = ((px & 0x1F) << 3) as u8;
                    let g = (((px >> 5) & 0x1F) << 3) as u8;
                    let b = (((px >> 10) & 0x1F) << 3) as u8;
                    ppm.extend_from_slice(&[r, g, b]);
                }
                let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../frame_dump.ppm");
                let _ = std::fs::write(path, &ppm);
                let distinct: std::collections::HashSet<u16> = fb.iter().copied().collect();
                eprintln!("  FRAME DUMP -> frame_dump.ppm ({} distinct colors)", distinct.len());
            }
            eprintln!("  SWI usage: ARM9 total={swi9_n} first={swi9:x?}  ARM7 total={swi7_n} first={swi7:x?}");
            // Steady-state profile + the graphics-adjacent registers the boot
            // must eventually touch (POWCNT1 enables the engines/LCD, VRAMCNT
            // maps the banks) — if they stay 0 the game hasn't reached
            // graphics init at all.
            let dump_prof = |name: &str, m: &std::collections::HashMap<u32, u64>| {
                let mut v: Vec<(&u32, &u64)> = m.iter().collect();
                v.sort_by(|a, b| b.1.cmp(a.1));
                let total: u64 = m.values().sum();
                eprintln!("  {name} steady-state profile (f350+, {total} samples; 0xffffffff = halted):");
                for (pc, n) in v.iter().take(14) {
                    eprintln!("    {:#010x}: {:5.1}%", pc, **n as f64 * 100.0 / total as f64);
                }
            };
            dump_prof("ARM9", &prof9);
            dump_prof("ARM7", &prof7);
            // Who on the ARM7 side knows the stream-state pointer? Scan its
            // RAM for the value 0x021d4400 (and the cmd-struct 0x021d43c8):
            // the slot that holds it leads to the code that stores through it.
            for (name, base, buf) in [
                ("arm7_wram", 0x0380_0000u32, &emu4.nds_mmu.arm7_wram),
                ("shared_wram(arm7 view 0x037f8000)", 0x037f_8000u32, &emu4.nds_mmu.shared_wram),
            ] {
                for (i, w) in buf.chunks_exact(4).enumerate() {
                    let v = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                    if v == 0x021d_4400
                        || v == 0x0380_9140
                        || v == 0x0380_9060
                        || v == 0x0380_9080
                        || v == 0x0380_9050
                    {
                        eprintln!("  PTR-SCAN {name}+{:#x} (addr {:#010x}) = {v:#010x}", i * 4, base + (i as u32) * 4);
                    }
                }
            }
            eprintln!(
                "  dispatcher mask words: [0x037f876c]={:#010x} [0x037f865c]={:#010x} [0x03806a80]={:#010x} [0x03806a84]={:#010x}",
                emu4.nds_mmu.read_word_arm7(0x037f_876c),
                emu4.nds_mmu.read_word_arm7(0x037f_865c),
                emu4.nds_mmu.read_word_arm7(0x0380_6a80),
                emu4.nds_mmu.read_word_arm7(0x0380_6a84),
            );
            eprintln!(
                "  driver dispatcher flags [0x03806a88]={:#010x} [0x03806a8c]={:#010x} [0x03806a90]={:#010x}",
                emu4.nds_mmu.read_word_arm7(0x0380_6a88),
                emu4.nds_mmu.read_word_arm7(0x0380_6a8c),
                emu4.nds_mmu.read_word_arm7(0x0380_6a90),
            );
            eprintln!("  queue struct @0x03809050..0x03809090 + first entry @0x03809168..0x03809198:");
            for a in (0x0380_9050u32..0x0380_9090).step_by(16) {
                eprintln!(
                    "    {a:#010x}: {:08x} {:08x} {:08x} {:08x}",
                    emu4.nds_mmu.read_word_arm7(a),
                    emu4.nds_mmu.read_word_arm7(a + 4),
                    emu4.nds_mmu.read_word_arm7(a + 8),
                    emu4.nds_mmu.read_word_arm7(a + 12)
                );
            }
            for a in (0x0380_9140u32..0x0380_91a0).step_by(16) {
                eprintln!(
                    "    {a:#010x}: {:08x} {:08x} {:08x} {:08x}",
                    emu4.nds_mmu.read_word_arm7(a),
                    emu4.nds_mmu.read_word_arm7(a + 4),
                    emu4.nds_mmu.read_word_arm7(a + 8),
                    emu4.nds_mmu.read_word_arm7(a + 12)
                );
            }
            eprintln!("  last 8 timer-control changes:");
            for l in &tctl_ring {
                eprintln!("    {l}");
            }
            let lit = emu4.nds_mmu.read_word_arm9(0x020d_b670);
            eprintln!(
                "  spin-checker polled reg: [0x020db670]={lit:#010x} -> current value={:#010x}",
                emu4.nds_mmu.read_word_arm9(lit)
            );
            {
                let mut v: Vec<(&u32, &u64)> = spin_lrs.iter().collect();
                v.sort_by(|a, b| b.1.cmp(a.1));
                eprintln!("  OS_SpinWait callers (lr -> count, f300+):");
                for (lr, n) in v.iter().take(8) {
                    eprintln!("    lr={lr:#010x}: {n}");
                }
            }
            eprintln!("  outer-loop callers (thumb) @0x02000d80..0x02000e00 y @0x02001100..0x020011a0:");
            for (lo, hi) in [(0x0200_0d80u32, 0x0200_0e00u32), (0x0200_1100, 0x0200_11f0)] {
                for a in (lo..hi).step_by(8) {
                    eprintln!(
                        "    {a:#010x}: {:04x} {:04x} {:04x} {:04x}",
                        emu4.nds_mmu.read_halfword_arm9(a),
                        emu4.nds_mmu.read_halfword_arm9(a + 2),
                        emu4.nds_mmu.read_halfword_arm9(a + 4),
                        emu4.nds_mmu.read_halfword_arm9(a + 6)
                    );
                }
            }
            eprintln!("  checker inner fn @0x020db284..0x020db300 + struct @0x020dada0:");
            for a in (0x020d_b284u32..0x020d_b300).step_by(16) {
                eprintln!(
                    "    {a:#010x}: {:08x} {:08x} {:08x} {:08x}",
                    emu4.nds_mmu.read_word_arm9(a),
                    emu4.nds_mmu.read_word_arm9(a + 4),
                    emu4.nds_mmu.read_word_arm9(a + 8),
                    emu4.nds_mmu.read_word_arm9(a + 12)
                );
            }
            for a in (0x020d_ada0u32..0x020d_ade0).step_by(16) {
                eprintln!(
                    "    S {a:#010x}: {:08x} {:08x} {:08x} {:08x}",
                    emu4.nds_mmu.read_word_arm9(a),
                    emu4.nds_mmu.read_word_arm9(a + 4),
                    emu4.nds_mmu.read_word_arm9(a + 8),
                    emu4.nds_mmu.read_word_arm9(a + 12)
                );
            }
            eprintln!("  checker fn @0x020db300..0x020db3a0:");
            for a in (0x020d_b300u32..0x020d_b3a0).step_by(16) {
                eprintln!(
                    "    {a:#010x}: {:08x} {:08x} {:08x} {:08x}",
                    emu4.nds_mmu.read_word_arm9(a),
                    emu4.nds_mmu.read_word_arm9(a + 4),
                    emu4.nds_mmu.read_word_arm9(a + 8),
                    emu4.nds_mmu.read_word_arm9(a + 12)
                );
            }
            eprintln!("  ARM9 spin-checker @0x020db4f0..0x020db680:");
            for a in (0x020d_b4f0u32..0x020d_b680).step_by(16) {
                eprintln!(
                    "    {a:#010x}: {:08x} {:08x} {:08x} {:08x}",
                    emu4.nds_mmu.read_word_arm9(a),
                    emu4.nds_mmu.read_word_arm9(a + 4),
                    emu4.nds_mmu.read_word_arm9(a + 8),
                    emu4.nds_mmu.read_word_arm9(a + 12)
                );
            }
            {
                let r = emu4.nds_arm9.cpu.registers.gpr;
                eprintln!("  ARM9 final stack walk (sp={:#010x}):", r[13]);
                for off in (0..0x60u32).step_by(4) {
                    let w = emu4.nds_mmu.read_word_arm9(r[13].wrapping_add(off));
                    let tag = if (0x0200_0000..0x0240_0000).contains(&w) { "  <- code?" } else { "" };
                    eprintln!("    [sp+{off:#04x}] = {w:#010x}{tag}");
                }
            }
            eprintln!("  ARM9 busy-wait code @0x0209e990..0x0209ed40:");
            for a in (0x0209_e990u32..0x0209_ed40).step_by(16) {
                eprintln!(
                    "    {a:#010x}: {:08x} {:08x} {:08x} {:08x}",
                    emu4.nds_mmu.read_word_arm9(a),
                    emu4.nds_mmu.read_word_arm9(a + 4),
                    emu4.nds_mmu.read_word_arm9(a + 8),
                    emu4.nds_mmu.read_word_arm9(a + 12)
                );
            }
            {
                let r = emu4.nds_arm9.cpu.registers.gpr;
                eprintln!(
                    "  ARM9 regs now: r0={:#010x} r1={:#010x} r2={:#010x} r3={:#010x} r4={:#010x} r5={:#010x} r6={:#010x} r7={:#010x} sp={:#010x} lr={:#010x}",
                    r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], r[13], r[14]
                );
                eprintln!("  reply-callback fn @0x0209eee0..0x0209ef60:");
                for a in (0x0209_eee0u32..0x0209_ef60).step_by(16) {
                    eprintln!(
                        "    {a:#010x}: {:08x} {:08x} {:08x} {:08x}",
                        emu4.nds_mmu.read_word_arm9(a),
                        emu4.nds_mmu.read_word_arm9(a + 4),
                        emu4.nds_mmu.read_word_arm9(a + 8),
                        emu4.nds_mmu.read_word_arm9(a + 12)
                    );
                }
                // The poll at 0x0209ebf0 waits for [[0x0209ed30]+4] == 1 (the
                // sound-driver-ready flag the ARM7 side must raise).
                let lit = emu4.nds_mmu.read_word_arm9(0x0209_ed30);
                eprintln!(
                    "  sound-ready poll: lit[0x0209ed30]={lit:#010x} [lit+0..0x10]={:#010x} {:#010x} {:#010x} {:#010x}",
                    emu4.nds_mmu.read_word_arm9(lit),
                    emu4.nds_mmu.read_word_arm9(lit.wrapping_add(4)),
                    emu4.nds_mmu.read_word_arm9(lit.wrapping_add(8)),
                    emu4.nds_mmu.read_word_arm9(lit.wrapping_add(12)),
                );
            }
            eprintln!(
                "  POWCNT1={:#010x} VRAMCNT A-I=[{:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}] DISPSTAT9={:#06x} DISPCNT_B={:#010x}",
                rd32(&emu4.nds_mmu.arm9_io, 0x304),
                emu4.nds_mmu.vram.banks[0].control,
                emu4.nds_mmu.vram.banks[1].control,
                emu4.nds_mmu.vram.banks[2].control,
                emu4.nds_mmu.vram.banks[3].control,
                emu4.nds_mmu.vram.banks[4].control,
                emu4.nds_mmu.vram.banks[5].control,
                emu4.nds_mmu.vram.banks[6].control,
                emu4.nds_mmu.vram.banks[7].control,
                emu4.nds_mmu.vram.banks[8].control,
                ((emu4.nds_mmu.arm9_io[5] as u16) << 8) | emu4.nds_mmu.arm9_io[4] as u16,
                rd32(&emu4.nds_mmu.arm9_io, 0x1000),
            );
            // IE bit3 (Timer0) is enabled — do the cores actually arm timers we
            // don't implement? TMxCNT_L/H live at 0x04000100..0x0F on each CPU.
            eprintln!("  arm9 TM0..3 io[0x100..0x110]: {:02x?}", &emu4.nds_mmu.arm9_io[0x100..0x110]);
            eprintln!("  arm7 TM0..3 io[0x100..0x110]: {:02x?}", &emu4.nds_mmu.arm7_io[0x100..0x110]);
            ev.dump();
        }
        // Hunt the ARM9 derail (post-handshake) while BOTH cores run 2:1. Trace
        // the ARM9's true exec address (gpr[15]-8 / pipeline[0]) until it leaves
        // valid code space, and dump the instructions that led there.
        let mut emu2 = Emulator::new();
        assert!(emu2.load_rom(&rom));
        eprintln!("--- ARM9 derail hunt (both cores 2:1) ---");
        // The game reconfigures ITCM to base 0 / 32MB, so its code legitimately
        // runs from ITCM mirrors anywhere in 0..0x02000000 (e.g. 0x01ff8xxx). The
        // real derail signal is `instr == 0` (executing empty memory), checked
        // separately; here we only reject wildly out-of-map PCs.
        let valid9 = |pc: u32| {
            pc < 0x0400_0000                            // ITCM-mirror (base 0) + main RAM + WRAM
                || (0xFFFF_0000..0xFFFF_8000).contains(&pc) // high vectors
        };
        let mut ring: Vec<(u32, u32, bool)> = Vec::new();
        // Phase 1: fast-forward (2:1, untracked) past the ARM9 BLZ decompression
        // (~22M cycles) until it reaches the IPCSYNC handshake region.
        for _ in 0..40_000_000u32 {
            let a9 = emu2.nds_arm9.cpu.registers.gpr[15];
            if (0x020d_6000..0x020d_7000).contains(&a9) {
                break;
            }
            emu2.nds_arm9.step(&mut emu2.nds_mmu);
            emu2.nds_arm9.step(&mut emu2.nds_mmu);
            emu2.nds_arm7.step(&mut emu2.nds_mmu);
        }
        eprintln!(
            "  warmed to handshake: arm9={:#010x} a7->a9={:#x} a9->a7={:#x}",
            emu2.nds_arm9.cpu.registers.gpr[15],
            emu2.nds_mmu.ipc.arm7_to_arm9_sync,
            emu2.nds_mmu.ipc.arm9_to_arm7_sync
        );
        let itcm_nz = emu2.nds_mmu.itcm.iter().filter(|&&b| b != 0).count();
        let dtcm_nz = emu2.nds_mmu.dtcm.iter().filter(|&&b| b != 0).count();
        let dtcm_hp = u32::from_le_bytes([
            emu2.nds_mmu.dtcm[0x3FFC],
            emu2.nds_mmu.dtcm[0x3FFD],
            emu2.nds_mmu.dtcm[0x3FFE],
            emu2.nds_mmu.dtcm[0x3FFF],
        ]);
        eprintln!("  TCM state @handshake: itcm_nonzero_bytes={itcm_nz} dtcm_nonzero_bytes={dtcm_nz} dtcm[0x3FFC](arm9 irq handler)={dtcm_hp:#010x}");
        eprintln!(
            "  CP15 @handshake: control={:#x} itcm_control={:#x} dtcm_control={:#x} itcm_enabled={} dtcm_enabled={} itcm_base={:#x}",
            emu2.nds_mmu.arm9_cp15.control,
            emu2.nds_mmu.arm9_cp15.itcm_control,
            emu2.nds_mmu.arm9_cp15.dtcm_control,
            emu2.nds_mmu.itcm_enabled(),
            emu2.nds_mmu.dtcm_enabled(),
            emu2.nds_mmu.itcm_base()
        );
        eprintln!("  itcm[0..0x20]:");
        for a in (0u32..0x20).step_by(4) {
            eprintln!("    itcm+{a:#x}: {:08x}", u32::from_le_bytes(emu2.nds_mmu.itcm[a as usize..a as usize + 4].try_into().unwrap()));
        }
        // Phase 2: trace the ARM9 until it leaves valid code (the post-handshake
        // derail). Log EVERY ARM9 instruction (not every-other) so the branch that
        // performs the derailing jump is visible; keep the 2:1 ARM9:ARM7 ratio by
        // ticking the ARM7 on odd sub-steps.
        for step in 0..6_000_000u32 {
            let thumb = emu2.nds_arm9.cpu.registers.get_flag(crate::nds::cpu::FLAG_T);
            let pc = emu2
                .nds_arm9
                .cpu
                .registers
                .gpr[15]
                .wrapping_sub(if thumb { 4 } else { 8 });
            let instr = emu2.nds_arm9.cpu.pipeline[0];
            if step % 10000 == 0 {
                let inz = emu2.nds_mmu.itcm.iter().filter(|&&b| b != 0).count();
                let dnz = emu2.nds_mmu.dtcm.iter().filter(|&&b| b != 0).count();
                let a9bios_nz = emu2.nds_mmu.arm9_bios[0x100..].iter().filter(|&&b| b != 0).count();
                eprintln!("  phase2 step {step}: arm9≈{pc:#010x} itcm_nz={inz} dtcm_nz={dnz} arm9bios[0x100..]_nz={a9bios_nz}");
            }
            // Detect entry into the infinite WFI idle-halt loop (enterCritical;
            // WFI; loop @0x020d3f4c). Dump the ring so we can see HOW the ARM9 got
            // there — an error/hang path vs a normal "boot done, wait" path.
            if (0x020d_3f4c..0x020d_3f58).contains(&pc) {
                let r = emu2.nds_arm9.cpu.registers.gpr;
                eprintln!("  ARM9 REACHED IDLE-HALT at phase2 step {step}: pc={pc:#010x} lr={:#010x} sp={:#010x}", r[14], r[13]);
                eprintln!("  path into idle-halt (ring, oldest->newest):");
                for (p, i, t) in ring.iter() {
                    eprintln!("    {p:#010x} [{}] {i:08x}", if *t { "T" } else { "A" });
                }
                break;
            }
            if !valid9(pc) || instr == 0 {
                let r = emu2.nds_arm9.cpu.registers.gpr;
                eprintln!("  ARM9 DERAILED at phase2 step {step}: exec={pc:#010x} instr={instr:08x} thumb={thumb}");
                eprintln!(
                    "  r0={:#x} r1={:#x} r2={:#x} r3={:#x} r4={:#x} r5={:#x} sp={:#010x} lr={:#010x}",
                    r[0], r[1], r[2], r[3], r[4], r[5], r[13], r[14]
                );
                // The instruction that set LR (the branch into the bad region) sits
                // at lr-4; decode it to tell BL (stays ARM) from BLX (→Thumb).
                let bsrc = r[14].wrapping_sub(4);
                let binstr = emu2.nds_mmu.read_word_arm9(bsrc);
                let kind = if (binstr >> 28) == 0xF && (binstr & 0x0E00_0000) == 0x0A00_0000 {
                    "BLX(imm)->Thumb"
                } else if (binstr & 0x0F00_0000) == 0x0B00_0000 {
                    "BL->ARM"
                } else {
                    "other"
                };
                eprintln!("  branch@lr-4 [{bsrc:#010x}]={binstr:08x} ({kind})");
                // Is the derail target actually ITCM-mirror? show what physical ITCM
                // holds at that mirror offset, and whether the *target region* around
                // the last valid caller looks like code or data.
                let mirror = (pc & 0x7FFF) as usize;
                eprintln!(
                    "  derail pc mirror-> itcm[{mirror:#x}]={:02x}{:02x}{:02x}{:02x}",
                    emu2.nds_mmu.itcm.get(mirror.wrapping_add(3)).copied().unwrap_or(0),
                    emu2.nds_mmu.itcm.get(mirror.wrapping_add(2)).copied().unwrap_or(0),
                    emu2.nds_mmu.itcm.get(mirror.wrapping_add(1)).copied().unwrap_or(0),
                    emu2.nds_mmu.itcm.get(mirror).copied().unwrap_or(0),
                );
                // Dump the memory around the branch source's *target* (what the last
                // caller jumped into) to judge code-vs-data.
                let tgt = ring.last().map(|(p, _, _)| *p & !0xF).unwrap_or(0);
                eprintln!("  mem around last-valid pc {tgt:#010x}:");
                for a in (tgt..tgt.wrapping_add(0x20)).step_by(4) {
                    eprintln!("    [{a:#010x}] = {:08x}", emu2.nds_mmu.read_word_arm9(a));
                }
                for (p, i, t) in ring.iter() {
                    eprintln!("    {p:#010x} [{}] {i:08x}", if *t { "T" } else { "A" });
                }
                break;
            }
            ring.push((pc, instr, thumb));
            if ring.len() > 60 {
                ring.remove(0);
            }
            emu2.nds_arm9.step(&mut emu2.nds_mmu);
            if step % 2 == 1 {
                emu2.nds_arm7.step(&mut emu2.nds_mmu);
            }
        }

        // Coordination experiment: run ARM9-ONLY until it reaches its IPCSYNC
        // wait (0x020d66xx = "I'm done, waiting for ARM7"), then check whether it
        // has populated 0x037f8000 (the routine the ARM7 BX-jumps to). This tells
        // us if the fix is "hold ARM7 until ARM9 ready" vs something deeper.
        let mut emu3 = Emulator::new();
        assert!(emu3.load_rom(&rom));
        let mut reached = false;
        for _ in 0..60_000_000u32 {
            let pc = emu3.nds_arm9.cpu.registers.gpr[15];
            if (0x020d_6600..0x020d_6640).contains(&pc) {
                reached = true;
                break;
            }
            emu3.nds_arm9.step(&mut emu3.nds_mmu);
        }
        let populated =
            (0x037f_8000u32..0x037f_8100).step_by(4).any(|a| emu3.nds_mmu.read_word_arm7(a) != 0);
        eprintln!(
            "--- coord exp: ARM9-only reached IPCSYNC-wait={reached} pc={:#010x} wramcnt={:#x} 0x037f8000_populated={populated}",
            emu3.nds_arm9.cpu.registers.gpr[15],
            emu3.nds_mmu.wram_control
        );
        eprintln!(
            "  ARM9 poll: r3(addr)={:#010x} lr(expected)={:#x} ipc.a7->a9={:#x} ipc.a9->a7={:#x}",
            emu3.nds_arm9.cpu.registers.gpr[3],
            emu3.nds_arm9.cpu.registers.gpr[14],
            emu3.nds_mmu.ipc.arm7_to_arm9_sync,
            emu3.nds_mmu.ipc.arm9_to_arm7_sync
        );
        for a in (0x037f_8000u32..0x037f_8020).step_by(4) {
            eprintln!("  {a:#010x}: {:08x}", emu3.nds_mmu.read_word_arm7(a));
        }
        // Force the handshake: pretend the ARM7 sent a non-zero sync, then run the
        // ARM9 more and see whether it NOW populates 0x037f8000 + sets WRAMCNT.
        // If yes → the ARM9 does the population post-sync (fix = get ARM7 to sync).
        emu3.nds_mmu.ipc.arm7_to_arm9_sync = 1;
        for _ in 0..8_000_000u32 {
            emu3.nds_arm9.step(&mut emu3.nds_mmu);
        }
        let pop2 =
            (0x037f_8000u32..0x037f_8100).step_by(4).any(|a| emu3.nds_mmu.read_word_arm7(a) != 0);
        eprintln!(
            "  after forcing ARM7 sync=1: ARM9 pc={:#010x} wramcnt={:#x} 0x037f8000_populated={pop2} a9->a7_sync={:#x}",
            emu3.nds_arm9.cpu.registers.gpr[15],
            emu3.nds_mmu.wram_control,
            emu3.nds_mmu.ipc.arm9_to_arm7_sync
        );
    }
}
