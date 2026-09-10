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

/// Bus cycles the NDS run loop gives the ARM9 before handing the ARM7 its half.
///
/// 64 is not a guess and not free: it is the loop's dominant fixed cost, since
/// everything per-slice — both CPU dispatches, the timers, the APU integration,
/// the PPU catch-up — is paid once per slice, ~8750 times a frame. Measured on
/// the overworld with `EMU_NDS_SLICE`, coarsening it is worth real throughput:
/// 61.3/61.4 fps at 64, 64.5/68.0 at 128, 64.8/63.7 at 256, with the rendered
/// frame **byte-identical** after 50 frames of walking at all three.
///
/// It used to stay at 64 anyway, because the boot IPC handshake did not survive
/// the coarser grain: a 4000-tick headless boot at 128 ended on a completely
/// different frame (every pixel differs) having produced **silence** — audio RMS
/// 0.0, peak 0. The handshake is a tight IPCSYNC ping-pong where each side polls
/// with a short timeout, so a slice that outlasts the timeout stalls it.
///
/// **That blocker is gone: [`NdsMmu::ipc_yield`] ends the slice on the ping.**
/// A core that writes IPCSYNC returns the bus at the next instruction boundary,
/// so the partner answers within one instruction rather than up to a slice
/// later, and the ping-pong keeps its lock-step at any width. The companion
/// change is that `slice_7` is now derived from the ARM9's *actual* `run_9`
/// rather than the offered `slice_9`; without it an ARM9 that yields early
/// hands the ARM7 a full half-slice and the lock-step breaks the other way.
///
/// The guard for all of this is `nds_boot_handshake_audio_guard`, which fails
/// on silence — the failure mode here is silent by construction, since a stalled
/// handshake still draws a plausible picture.
///
/// `EMU_NDS_SLICE=<cycles>` overrides it, clamped to 8..=4096, so the trade can
/// be re-measured rather than re-argued. Read once and cached.
///
/// Re-measured 2026-07-26 against the current build with a *paired* design —
/// 64 and 256 run back-to-back, four times, because absolutes on this host swing
/// ~25% run to run and a one-shot sweep cannot see a 19% effect through that.
/// 256 won **8/8** comparisons (four pairs x {1x, 5x} requests):
///
/// | pair | 64 @5x | 256 @5x | | 64 @1x | 256 @1x |
/// |---|---|---|---|---|---|
/// | 1 | 1.47x | 1.69x | | 0.99x | 1.27x |
/// | 2 | 1.26x | 1.68x | | 1.20x | 1.26x |
/// | 3 | 1.60x | 1.73x | | 1.20x | 1.26x |
/// | 4 | 1.47x | 1.76x | | 1.13x | 1.30x |
///
/// Mean +19% at a 5x request, +13% at 1x. At the time, wider was not
/// monotonically better: that sweep put 512 at parity and 1024 clearly worse,
/// so 256 was the knee — **for an interpreted CPU**.
///
/// Re-swept 2026-07-29 with both recompilers on, because the slice is now the
/// bound on compiled *chain* length (the ARM9's chain-end census put "slice
/// budget spent" at 14.5%), and the knee moved exactly as that predicts —
/// three alternating pairs per width, GBA row as control, NDS @5x request:
///
/// | width | pair 1 | pair 2 | pair 3 | mean |
/// |---|---|---|---|---|
/// | 256 | 4.06 | 4.04 | 4.19 | 4.10x |
/// | 512 | 4.52 | 4.49 | 4.32 | 4.44x |
/// | 1024 | 4.51-4.70 | 4.43-4.58 | 4.59-4.61 | ~4.57x |
/// | 2048 | 4.85 | 4.95 | 4.96 | 4.92x |
/// | **4096** | **5.36** | **5.17** | **5.20** | **5.24x — the 5.0x target** |
///
/// The default is therefore 4096. The correctness battery at that width:
/// `nds_boot_handshake_audio_guard` frame_hash `0xf36dab31b6802325`
/// **identical** with both recompilers on and both off (audio rms 2451.7,
/// same class as the 256-slice 2461.7 — a re-baselined constant, per the
/// paragraph below), `nds_ingame_audio_and_perf_report` byte-identical
/// JIT-on vs off (dup_blocks 0, short_ticks 0, live-channel histogram
/// unchanged), and the 4000-tick ARM7 boot lockstep register-exact.
///
/// Be aware of what a width change costs, because the warning attached to the
/// first re-baseline was accurate: the slice IS the emulated interleave, so
/// every interleave-derived signature (the boot guard's frame hash, the
/// in-game keyons/RMS literals) is re-baselined at each width and the old
/// values are no longer the oracle. The *identity* properties — JIT-on
/// equals JIT-off, audio comes up, no duplicate/short blocks — are the gates
/// that survive, and all of them held at every width measured.
///
/// `EMU_NDS_SLICE=<cycles>` overrides it, clamped to 8..=8192, so the trade
/// can be re-measured rather than re-argued. Read once and cached.
#[inline]
fn nds_interleave_cycles() -> u32 {
    static CACHE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("EMU_NDS_SLICE")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .map_or(4096, |v| v.clamp(8, 8192))
    })
}

/// Is the run loop inside the last `frames_from_end` video frames of this tick?
///
/// At `speed > 1` one `tick()` sweeps through several video frames but the
/// frontend only ever looks at the last one, so the pixel work for the others is
/// pure cost. Timing — scanline counters, HBlank/VBlank IRQs, DMA, the APU — is
/// never gated on this; only composition and rasterization are.
///
/// `frames_from_end` is 1 for a renderer whose output is consumed in the same
/// frame it is produced (both 2D compositors) and 2 for one consumed a frame
/// later (the NDS 3D rasterizer, whose back buffer is published at VBlank and
/// sampled by the *next* frame's scanlines).
///
/// At `speed == 1`, `cycle_budget == base_cycles` and the answer is always
/// `true` for any `frames_from_end >= 1`, so normal-speed rendering is
/// unaffected by construction.
#[inline]
fn in_final_frames(
    cycles_run: u32,
    base_cycles: u32,
    cycle_budget: u32,
    frames_from_end: u32,
) -> bool {
    // Saturating so a pathological budget (or a `base_cycles * frames_from_end`
    // wider than the budget) reads as "yes, render" rather than wrapping to
    // "no": dropping a frame is a visible defect, drawing a spare one is not.
    cycles_run.saturating_add(base_cycles.saturating_mul(frames_from_end)) >= cycle_budget
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
    /// Sample rate the core emits at, mirrored into every APU's resampler.
    /// Set by the frontend from the audio device's real rate; see
    /// [`Emulator::set_audio_sample_rate`].
    pub(crate) output_hz: u32,
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
            output_hz: crate::resampler::DEFAULT_OUTPUT_HZ,
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
        // The GBA branch above rebuilt its APU, so restore the host rate.
        self.sync_audio_rate();
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
        // Both callers (`load_rom`, `load_rom_path`) replace whole MMUs above,
        // and the GBA APU is rebuilt unconditionally here, so the host rate has
        // to be re-applied last — after every construction this path performs.
        self.sync_audio_rate();
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
        //
        // It MUST equal what `get_audio_buffer` reports outside gameplay, which is
        // `audio_frames_per_tick()`. The old `735 * speed` matched neither: at a
        // 48 kHz host it filled 735 frames of a block reported as 800, so the tail
        // handed to the frontend was whatever the previous tick left there — 65
        // stale frames re-emitted every tick, i.e. a buzz locked to the frame rate.
        let placeholder_samples = self.audio_frames_per_tick();
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

        // Saturating so the divisor is provably >= 1 even if some future path
        // writes `frame_skip` without going through `set_frame_skip`.
        let is_render_tick = self.ticks % self.frame_skip.saturating_add(1) == 0;
        if is_render_tick {
            self.rendered_frames += 1;
        }

        let base_cycles: u32 = match self.console_type {
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
        let cycle_budget = Self::cycle_budget(base_cycles, self.speed);

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

                // Only rasterize the frame that will actually be presented; the
                // intermediate frames of a fast-forward tick run timing-only.
                // See `in_final_frames`.
                let render_pixels =
                    is_render_tick && in_final_frames(cycles_run, base_cycles, cycle_budget, 1);

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
                // bounded by `Emulator::cycle_budget`, so per-tick work stays finite.
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
                        is_render_tick && in_final_frames(cycles_run, base_cycles, cycle_budget, 1);
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
                    // fast-forward frames advance timing-only (see `in_final_frames`).
                    let render_pixels =
                        is_render_tick && in_final_frames(cycles_run, base_cycles, cycle_budget, 1);
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
            let base_cycles: u32 = 560190;
            let cycle_budget = Self::cycle_budget(base_cycles, self.speed);

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
                //
                // ponytail: BOTH cores execute at half their real throughput.
                // `cycle_budget` counts 33.513982 MHz bus cycles (that is what
                // makes 560190 one frame, and what the timers/PPU/APU are fed),
                // but the ARM9 clocks at 67.027964 MHz and the ARM7 at the bus
                // rate — so a faithful loop would give the ARM9 `2 * slice_9`
                // and the ARM7 `slice_9`, not `slice_9` and `slice_9 / 2`. The
                // 2:1 ratio between the cores is right; the absolute scale is
                // not. Ceiling: per-frame CPU work a real DS finishes may not
                // fit, which would show up as a game deferring work a frame.
                // Measured NOT to be happening here — `INGAME CPU` reports the
                // ARM9 idling 46.4% of its budget and the ARM7 79.6% of its, so
                // both cores already finish and halt. Upgrade path: budget the
                // loop in ARM9 cycles (1120380/frame), halve it for the ARM7,
                // and pass `run_9 / 2` to the peripherals to keep them on the
                // bus clock. It roughly doubles interpretation, which the same
                // profile puts at ~60% of the frame, so it needs interpreter
                // work first.
                let slice_9 = std::cmp::min(nds_interleave_cycles(), cycle_budget - arm9_cycles_run);

                // Only the LAST video frame of the budget is composited; the
                // intermediate frames a fast-forward tick sweeps through advance
                // timing-only. Same contract and same expression as the GBA and
                // GBC arms above — `NdsPpu::tick` gates nothing but
                // `render_scanline` on this, so VCOUNT, DISPSTAT, the HBlank and
                // VBlank IRQs and `frame_completed` still run on every frame.
                //
                // Without it the NDS arm passed `is_render_tick` (the *frame-skip*
                // flag, which is `true` on every tick unless the player set
                // frame-skip) and therefore composited all `speed` frames: the
                // measured cost of 4x fast-forward included four full 2D passes
                // per tick instead of one. At `speed == 1`,
                // `cycle_budget == base_cycles` and this is always `true`.
                let final_frame =
                    in_final_frames(arm9_cycles_run, base_cycles, cycle_budget, 1);
                let render_pixels = is_render_tick && final_frame;
                // The 3D rasterizer needs ONE FRAME OF LEAD over the compositor.
                // `swap_buffers` fills the back buffer from a mid-visible-period
                // CPU store; VBlank publishes it (`Gx3d::present`) and the *next*
                // frame's scanlines are what sample it. Gating it on
                // `render_pixels` would therefore composite the final frame
                // against 3D geometry from the previous *tick*.
                //
                // So the frame that must rasterize is the one *before* the
                // composited frame — and at `speed > 1` that is the ONLY one.
                // The final frame's own rasterization is published at its VBlank
                // and then sampled by the first frame of the next tick, which at
                // `speed > 1` is not the frame that tick composites either, so
                // it is drawn and then thrown away. Measured at a 5x request:
                // 1.06 ms of the 3.34 ms per-frame budget went into 3D, for two
                // rasterizations per tick where one is displayed.
                //
                // At `speed == 1` the two windows coincide (`cycle_budget ==
                // base_cycles` makes both predicates true on the single frame),
                // and the guard below leaves that case exactly as it was: a 1x
                // tick still rasterizes its one frame, feeding the next tick.
                //
                // ponytail: this assumes the game swaps at least once per frame,
                // which SoulSilver does. Ceiling: a title that swaps every
                // *other* frame can land its swap on the skipped final frame, so
                // the composited frame shows 3D one swap old — visible only
                // while fast-forwarding, where the 2D layers are already
                // advancing five frames at a time. Upgrade path: gate on
                // `gx.engine.swap_pending` instead of on frame position, once a
                // probe reports per-frame swap counts for a game that does it.
                self.nds_mmu.gx_raster_enabled = is_render_tick
                    && in_final_frames(arm9_cycles_run, base_cycles, cycle_budget, 2)
                    && !(final_frame && cycle_budget > base_cycles);

                // Clock reads are OFF unless a probe asks for them. This is the
                // innermost loop of the whole emulator — ~8750 iterations per
                // frame at a 64-cycle slice — and an unconditional
                // `Instant::now()` pair here is ~17500 QueryPerformanceCounter
                // calls per frame, paid by every player to serve a measurement
                // nobody is reading. `prof_raster_ns` gets away with the same
                // pattern because it samples once per swap; this one does not.
                let prof_t0 = self.nds_mmu.prof_cpu_on.then(std::time::Instant::now);
                let run_9 = self.nds_arm9.run(&mut self.nds_mmu, slice_9);
                arm9_cycles_run += run_9;
                // The ARM9's share, taken before the ARM7 runs. One extra clock
                // read per slice, and only while the gate is on.
                if let Some(t0) = prof_t0 {
                    self.nds_mmu.prof_cpu9_ns =
                        self.nds_mmu.prof_cpu9_ns.wrapping_add(t0.elapsed().as_nanos() as u64);
                }

                // The ARM7's window is derived from what the ARM9 **actually**
                // ran, not from what it was offered. Those were the same number
                // until `NdsMmu::ipc_yield` existed; now an ARM9 that pings its
                // partner and stops after 10 cycles must not hand that partner
                // a full half-slice, or the ARM7 races ahead by the whole
                // remainder — which is the very lock-step the yield exists to
                // protect. The 2:1 ratio is the bus-clock relationship between
                // the cores and is unchanged.
                let slice_7 = run_9 / 2;
                let run_7 = self.nds_arm7.run(&mut self.nds_mmu, slice_7);
                if let Some(t0) = prof_t0 {
                    let prof_cpu = t0.elapsed().as_nanos() as u64;
                    self.nds_mmu.prof_cpu_ns = self.nds_mmu.prof_cpu_ns.wrapping_add(prof_cpu);
                }
                self.nds_mmu.arm7_cycles_run =
                    self.nds_mmu.arm7_cycles_run.wrapping_add(u64::from(run_7));

                // Timers on both cores clock at the 33.513982 MHz bus, the same
                // unit run_9 is budgeted in: 560190 cycles/frame is 355 dots x
                // 263 lines x 6, which is why the DS refreshes at 59.8261 Hz
                // (see `nds::apu::NDS_CYCLES_PER_SEC`) rather than 60.
                self.nds_mmu.tick_nds_timers(run_9 as u32);
                self.nds_mmu
                    .tick_apu(run_9 as u32, &mut self.raw_audio_buffer, audio_off, self.speed);

                let vo = self.video_offset;
                let video_slice = &mut self.raw_video_buffer[vo..vo + 256 * 384];
                self.nds_ppu.tick(
                    run_9 as u32,
                    &mut self.nds_mmu,
                    video_slice,
                    render_pixels,
                );

                if self.nds_ppu.frame_completed {
                    self.nds_ppu.frame_completed = false;
                    // Present only the frame that was actually composited;
                    // presenting a skipped one would publish the previous frame's
                    // pixels a second time.
                    if render_pixels {
                        self.present_frame();
                    }
                }
            }

            // Close the APU's deferral before `get_audio_buffer` is allowed to
            // see this tick's samples: `tick_apu` holds cycles until the mix can
            // change, so without this the tail of every frame would be missing.
            self.nds_mmu
                .flush_apu(&mut self.raw_audio_buffer, audio_off, self.speed);

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
                // Follow the host rate, or the phase steps at every tick boundary
                // on a device that is not 44.1 kHz and the "beep" buzzes.
                let sample_rate = f64::from(self.output_hz);
                for i in 0..placeholder_samples {
                    let t = (self.ticks as f64 * placeholder_samples as f64 + i as f64) / sample_rate;
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
    ///
    /// Public so out-of-crate tests can exercise this one step without booting a
    /// cartridge; `tick` calls it once per emulated frame.
    pub fn poll_nds_touch_penirq(&mut self) {
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

    /// Nominal stereo frames per tick at the current output rate — one tick is
    /// one emulated frame, counted at a round 60 Hz.
    ///
    /// Both uses want the nominal figure rather than an exact one: sizing
    /// `raw_audio_buffer` (which then adds 4x headroom) and the length of the
    /// placeholder/silence blocks, which `get_audio_buffer` reports verbatim
    /// outside gameplay. Real gameplay blocks are sized by the resampler's
    /// `sample_count` instead, and those follow each console's true refresh —
    /// the NDS lands ~0.29% above this figure at 59.8261 Hz (see
    /// `nds::apu::NDS_CYCLES_PER_SEC`).
    ///
    /// Speed-independent: `cycles_per_sample` scales with `speed` exactly as
    /// the cycle budget does.
    fn audio_frames_per_tick(&self) -> usize {
        (self.output_hz as usize / 60).max(1)
    }

    /// Push `output_hz` back into every console's resampler.
    ///
    /// The rate belongs to the *host device*, but the resamplers live inside
    /// the per-console APUs, and those are reconstructed wholesale by
    /// `reset` / `reset_on_rom_load` / `load_rom*` (`GbaApu::new()`,
    /// `GbaMmu::new()`, `gbc::Mmu::new()`) — each handing back a
    /// `BoxResampler::new()` pinned to [`DEFAULT_OUTPUT_HZ`]. Re-applying here
    /// is what keeps "every resampler runs at `self.output_hz`" an invariant
    /// instead of something each construction site has to remember.
    ///
    /// Without it the core silently reverted to 44.1 kHz on an in-app ROM load
    /// or an in-game RESET (the frontend sets the rate once, at device open)
    /// while the device kept running at its own: 735 stereo frames produced
    /// per tick against 800 consumed on a 48 kHz endpoint — the exact deficit
    /// [`BoxResampler::snap`] documents as audible stutter, plus the ~8.8%
    /// pitch/tempo error that comes with playing 44.1 kHz samples at 48 kHz.
    ///
    /// [`DEFAULT_OUTPUT_HZ`]: crate::resampler::DEFAULT_OUTPUT_HZ
    /// [`BoxResampler::snap`]: crate::resampler::BoxResampler
    fn sync_audio_rate(&mut self) {
        self.gbc_mmu.apu.resampler.set_output_rate(self.output_hz);
        self.gba_mmu.apu.resampler.set_output_rate(self.output_hz);
        self.nds_mmu.apu.resampler.set_output_rate(self.output_hz);
    }

    /// Adopt the host audio device's real sample rate.
    ///
    /// Every console's `cycles_per_sample` divides its bus clock by the
    /// resampler's output rate, so setting it here retargets the whole pipeline.
    /// The frontend must call this with the rate the device actually runs at:
    /// left at 44100 against a 48 kHz endpoint, SDL inserts its own resampler on
    /// every queued block, which is a stage this emulator can neither measure
    /// nor control.
    pub fn set_audio_sample_rate(&mut self, hz: u32) {
        let hz = hz.clamp(8_000, 384_000);
        self.output_hz = hz;
        self.sync_audio_rate();
        // Four ticks of headroom, matching the ~3.7 ticks the fixed 44.1 kHz
        // buffer used to carry. Without this a high-rate device would silently
        // truncate in `BoxResampler::tick`.
        let want = self.audio_frames_per_tick() * 4 * 2 + self.audio_offset;
        if self.raw_audio_buffer.len() < want {
            let (buf, off) = allocate_aligned::<i16>(want, 16);
            self.raw_audio_buffer = buf;
            self.audio_offset = off;
        }
    }

    /// Walk the entire NDS machine for a binary snapshot, in both directions
    /// (see [`crate::snapshot`]).
    ///
    /// Not carried, each for a reason:
    /// * `speed`, `is_playing`, `frame_skip` — frontend-owned session settings.
    ///   Restoring a state must not silently change the speed the player set.
    /// * `width`/`height`, `console_type`, `rom_path` — fixed by the cartridge
    ///   already loaded, and the snapshot header refuses a foreign one.
    /// * the audio buffer and the *back* video buffer — regenerated by the next
    ///   `tick`. The visible front buffer *is* carried, so the restored frame
    ///   appears immediately instead of showing the pre-load image for a frame.
    pub(crate) fn snap_nds(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        use crate::snapshot::Snap;
        self.nds_arm9.snap(v);
        self.nds_arm7.snap(v);
        self.nds_mmu.snap(v);
        self.nds_ppu.snap(v);
        self.ticks.snap(v);
        self.cpu_cycles.snap(v);
        self.rendered_frames.snap(v);
        crate::snapshot::snap_enum(
            v,
            &mut self.state,
            |s| match s {
                EmulatorState::Splash => 0,
                EmulatorState::Gameplay => 1,
            },
            |i| match i {
                0 => Some(EmulatorState::Splash),
                1 => Some(EmulatorState::Gameplay),
                _ => None,
            },
        );
        let vo = self.front_offset;
        for px in &mut self.front_video_buffer[vo..vo + 256 * 384] {
            px.snap(v);
        }
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
            self.audio_frames_per_tick()
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

    /// Slowest and fastest emulation multipliers this core accepts.
    ///
    /// Bounded at BOTH ends because `speed` scales every console's
    /// `cycles_per_sample`, and `BoxResampler::tick` emits one output sample
    /// per `cycles_per_sample` cycles: as that quantity approaches zero the
    /// loop emits unboundedly many samples for a single slice. The smallest
    /// value that still leaves a non-zero cycle budget (~3.6e-6 on the GBA)
    /// asks for millions of samples per emulated cycle and wedges `tick`.
    /// The window below is far wider than the frontend's own 0.5x..4x, so no
    /// reachable UI setting is affected — only the `--speed` CLI flag and the
    /// `SET_SPEED` interactive command, which pass their argument through raw.
    pub const MIN_SPEED: f32 = 0.05;
    pub const MAX_SPEED: f32 = 16.0;

    /// Cycles one `tick()` may emulate for a console whose video frame is
    /// `base_cycles`, at `speed`.
    ///
    /// The ceiling is derived from [`Self::MAX_SPEED`] rather than being a flat
    /// literal, so it **cannot bind on a speed [`Self::set_speed`] accepts**.
    /// The previous flat `5_000_000` did: the NDS frame is 560190 cycles, so it
    /// silently capped that console at 8.93x while the setter advertised — and
    /// accepted — up to 16x. A request the API takes and the run loop then
    /// quietly ignores is the same defect class as the frontend's old
    /// out-of-range `--speed`, and just as invisible.
    ///
    /// It is still a real clamp, not dead code: `speed` is `pub(crate)`, so a
    /// future path that sets it without going through the setter is bounded
    /// here rather than handing the run loop an unbounded budget.
    fn cycle_budget(base_cycles: u32, speed: f32) -> u32 {
        let raw = (base_cycles as f32 * speed) as u32;
        std::cmp::min(raw, (base_cycles as f32 * Self::MAX_SPEED) as u32)
    }

    /// Set the emulation speed multiplier. Out-of-range, zero, negative and
    /// non-finite requests are ignored, leaving the previous speed in place.
    pub fn set_speed(&mut self, speed: f32) {
        if speed.is_finite() && (Self::MIN_SPEED..=Self::MAX_SPEED).contains(&speed) {
            self.speed = speed;
        }
    }

    /// Largest accepted frame skip: render at least one frame in ten.
    ///
    /// Bounded because `tick` derives its render gate from
    /// `ticks % (frame_skip + 1)`. `u32::MAX` makes that add overflow — a panic
    /// in debug, and in release a wrap to zero, which is then a remainder by
    /// zero and panics as well. Rust panics abort across the cxx FFI boundary,
    /// so this took the whole process down, and it was reachable: the frontend
    /// holds `frame_skip` as `int` and passes it to a `u32` parameter, so
    /// `--frame-skip -1` arrived here as `u32::MAX`.
    pub const MAX_FRAME_SKIP: u32 = 9;

    /// Set how many frames are skipped between rendered ones. Out-of-range
    /// requests are ignored, leaving the previous value in place — the same
    /// contract as [`Emulator::set_speed`].
    pub fn set_frame_skip(&mut self, frame_skip: u32) {
        if frame_skip <= Self::MAX_FRAME_SKIP {
            self.frame_skip = frame_skip;
        }
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

    /// The fast-forward render window, which is what makes speed > 1 cheaper per
    /// emulated frame than speed == 1.
    ///
    /// Regression for the NDS arm, which passed `is_render_tick` (the *frame
    /// skip* flag — `true` on essentially every tick) where the GBA and GBC arms
    /// passed this window, and therefore composited all `speed` frames per tick.
    /// Measured on the player's SoulSilver overworld save, `--speed 4`:
    /// 1.20x -> 1.46x achieved.
    #[test]
    fn fast_forward_renders_only_the_final_frames() {
        // One video frame of budget: `speed == 1`. Every renderer draws, at every
        // point in the tick, whatever its lead — this is what keeps normal-speed
        // output bit-identical.
        for lead in 1..=2 {
            for cycles_run in [0, 1, 280_895] {
                assert!(
                    in_final_frames(cycles_run, 280_896, 280_896, lead),
                    "speed 1 must always render (lead {lead}, at {cycles_run})"
                );
            }
        }

        // Four frames of budget: `speed == 4`. The compositor draws only inside
        // the last frame; the 3D rasterizer, whose output is consumed a frame
        // later, draws inside the last two.
        let budget = 4 * 280_896;
        let frame = |n: u32| n * 280_896;
        assert!(!in_final_frames(frame(0), 280_896, budget, 1));
        assert!(!in_final_frames(frame(2) - 1, 280_896, budget, 1));
        assert!(in_final_frames(frame(3), 280_896, budget, 1));
        assert!(!in_final_frames(frame(1), 280_896, budget, 2));
        assert!(in_final_frames(frame(2), 280_896, budget, 2));

        // Exactly one composited frame and two rasterized frames per tick, no
        // matter how finely the loop slices the budget — the NDS run loop
        // advances in 64-cycle slices, not in whole frames.
        let slice = 64;
        let composited = (0..budget / slice)
            .filter(|i| {
                !in_final_frames(i * slice, 280_896, budget, 1)
                    && in_final_frames((i + 1) * slice, 280_896, budget, 1)
            })
            .count();
        assert_eq!(composited, 1, "exactly one composite window opens per tick");

        // Saturating, not wrapping: a lead wider than the whole budget must read
        // as "render", never wrap to "skip" and drop the frame entirely.
        assert!(in_final_frames(0, u32::MAX, u32::MAX, 2));
    }

    /// Regression for the GBA fast-forward bug: the per-tick instruction guard in
    /// `tick()` must scale with `speed`. The old fixed `200_000` cap throttled GBA
    /// throughput to ~1.4x regardless of the requested multiplier, so "modify frame
    /// speed" did nothing for GBA while GB worked. This drives the real ARM core and
    /// checks that 4x actually advances ~4x the CPU cycles of 1x.
    ///
    /// Uses a generated cartridge with an ADD/branch loop, so every checkout
    /// runs the regression without reading a player's ROM or battery save.
    #[test]
    fn gba_speed_scales_cpu_throughput() {
        let mut rom = vec![0u8; 0x100];
        rom[..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes()); // B 0x080000C0
        rom[4..0xA0].copy_from_slice(&crate::rom::GBA_LOGO);
        rom[0xA0..0xAC].copy_from_slice(b"SPEED TEST  ");
        rom[0xB2] = 0x96;
        rom[0xBD] = rom[0xA0..0xBD]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_sub(*byte))
            .wrapping_sub(0x19);
        rom[0xC0..0xC4].copy_from_slice(&0xE280_0001u32.to_le_bytes()); // ADD r0,r0,#1
        rom[0xC4..0xC8].copy_from_slice(&0xEAFF_FFFDu32.to_le_bytes()); // B 0x080000C0

        let mut emu = Emulator::new();
        assert!(emu.load_rom(&rom), "synthetic GBA cartridge must load");
        assert!(
            emu.get_console_type() == crate::ffi::ConsoleType::Gba,
            "expected GBA console type"
        );
        emu.play();

        // The first tick reaches the steady loop past the cartridge header.
        emu.set_speed(1.0);
        emu.tick();
        assert!(emu.gba_cpu.registers.gpr[0] > 0, "the real ARM loop must execute");

        let window = |emu: &mut Emulator, speed: f32| -> u64 {
            emu.set_speed(speed);
            let start = emu.get_cpu_cycles();
            for _ in 0..4 {
                emu.tick();
            }
            emu.get_cpu_cycles() - start
        };

        // Exercise repeated speed changes against the same deterministic loop.
        let mut cycles_1x: u64 = 0;
        let mut cycles_4x: u64 = 0;
        for _ in 0..2 {
            cycles_1x += window(&mut emu, 1.0);
            cycles_4x += window(&mut emu, 4.0);
        }

        let ratio = cycles_4x as f64 / cycles_1x as f64;
        eprintln!(
            "GBA cpu-cycle throughput: 1x={cycles_1x}  4x={cycles_4x}  ratio={ratio:.2} (ideal 4.0; pre-fix ~1.4)"
        );

        assert!(
            (3.95..=4.05).contains(&ratio),
            "GBA speed did not scale: 4x/1x throughput ratio {ratio:.2} is not near 4 \
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

    /// Nominal refresh of the GBA and the GBC: 16.777216 MHz / 280896 cycles.
    /// This is the rate `frontend/src/main.cpp` paces a fast-forwarded loop to,
    /// so it is also the denominator of the multiplier the player sees.
    const GBA_GBC_FPS: f64 = 59.7275;
    /// Nominal refresh of the DS: 33.513982 MHz / 560190 cycles (355 dots x 263
    /// lines x 6). See [`crate::nds::apu::NDS_CYCLES_PER_SEC`].
    const NDS_FPS: f64 = 59.8261;

    /// One console's row in [`wall_clock_speed_ceiling_probe`].
    struct SpeedTarget {
        /// Console name, for the report only.
        label: &'static str,
        /// File name under `roms/`. Those images are untracked and copyrighted,
        /// so a missing one is a skip, never a failure.
        rom: &'static str,
        /// Refresh the frontend paces this console to; see the constants above.
        fps: f64,
    }

    /// **The** evidence instrument for "fast-forward does not reach Nx".
    ///
    /// `set_speed(N)` multiplies the per-tick *cycle budget* by N, and the
    /// frontend paces one tick per `1/fps` seconds, so the multiplier the player
    /// actually gets is
    ///
    /// ```text
    /// achieved = (ticks/s) * N / fps
    /// ```
    ///
    /// capped by how fast the core can retire that budget. Running unthrottled
    /// (no frontend limiter in this process) therefore measures the *ceiling*:
    /// if `achieved` comes back below `N`, the pacing logic is not at fault and
    /// no amount of frontend work will help — the core is compute-bound.
    ///
    /// Measured at each speed the UI can request, from the same starting scene,
    /// because the ceiling is **not** speed-independent: at N > 1 a tick spans
    /// several video frames and `tick()` rasterizes only the last of them
    /// (`is_render_tick && cycles_run + base_cycles >= cycle_budget`), so
    /// per-frame overheads amortise and the ceiling rises with N.
    ///
    /// Scene selection matters more than anything else here — the same NDS build
    /// measures 1.48x in the bedroom and 1.08x in the overworld. Set
    /// `EMU_STATE_DIR` (and optionally `EMU_STATE_SLOT`, default 0) to the app's
    /// config directory to measure the player's own scenes; with it unset every
    /// console is measured from a cold boot, which is *not* representative.
    ///
    /// `#[ignore]` because it costs wall-clock time by construction and depends
    /// on ROMs that are not in the tree.
    #[test]
    #[ignore = "manual perf probe; run explicitly with --ignored --nocapture"]
    fn wall_clock_speed_ceiling_probe() {
        use std::time::Instant;

        const TARGETS: [SpeedTarget; 3] = [
            SpeedTarget {
                label: "GBC",
                rom: "Pokemon - Crystal Version (UE) (V1.1) [C][!].gbc",
                fps: GBA_GBC_FPS,
            },
            SpeedTarget {
                label: "GBA",
                rom: "Pokemon - Emerald Version (USA, Europe).gba",
                fps: GBA_GBC_FPS,
            },
            SpeedTarget {
                label: "NDS",
                rom: "Pokemon - SoulSilver Version (USA).nds",
                fps: NDS_FPS,
            },
        ];
        /// 1.0 is the realtime baseline; 4.0 is today's UI maximum; 5.0 is the
        /// target. Keep 1.0 first so the baseline is the warmest measurement.
        const SPEEDS: [f32; 3] = [1.0, 4.0, 5.0];
        /// Long enough that a single scheduler hiccup cannot move the mean, short
        /// enough that the whole probe stays under a minute per console.
        const WINDOW_MS: u128 = 1500;
        /// Ticks discarded before timing: refills the caches and lets any
        /// speed-dependent state (resampler `cycles_per_sample`, the render-skip
        /// phase) settle.
        const WARMUP: u32 = 60;

        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let state_dir = std::env::var("EMU_STATE_DIR").ok();
        let slot = std::env::var("EMU_STATE_SLOT").unwrap_or_else(|_| "0".to_string());
        eprintln!(
            "CEILING PROBE (unthrottled; achieved = ticks/s * speed / fps)\n\
             scene source: {}",
            match &state_dir {
                Some(d) => format!("savestate slot {slot} in {d}"),
                None => "cold boot (set EMU_STATE_DIR for the player's own scenes)".to_string(),
            }
        );

        for target in &TARGETS {
            let rom = repo.join("roms").join(target.rom);
            if !rom.exists() {
                eprintln!("SKIP {}: ROM absent at {}", target.label, rom.display());
                continue;
            }
            let mut emu = Emulator::new();
            let res = emu.load_rom_path(
                rom.to_str().expect("ROM path is UTF-8"),
                repo.to_str().expect("repo path is UTF-8"),
            );
            assert!(res.starts_with("LOAD_ROM_OK"), "{}: {res}", target.label);

            for &speed in &SPEEDS {
                // Same starting scene for every speed. Without this the previous
                // window has advanced the game — possibly into a cheaper or more
                // expensive scene — and the rows are no longer comparable.
                let scene = match state_dir.as_deref() {
                    Some(dir) => {
                        let res = emu.load_state(&slot, dir);
                        if res.starts_with("LOAD_STATE_OK") {
                            "state"
                        } else {
                            eprintln!("  {} slot {slot}: {res} — measuring boot scene", target.label);
                            "boot"
                        }
                    }
                    None => "boot",
                };
                emu.play();
                // After `load_state`: the JSON savestate path restores `speed`,
                // so setting it first would be silently overwritten.
                emu.set_speed(speed);
                assert_eq!(emu.get_speed(), speed, "{}: set_speed rejected", target.label);

                for _ in 0..WARMUP {
                    emu.tick();
                }

                let mut tick_ms: Vec<f64> = Vec::new();
                let window = Instant::now();
                while window.elapsed().as_millis() < WINDOW_MS {
                    let t0 = Instant::now();
                    emu.tick();
                    tick_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
                }
                let secs = window.elapsed().as_secs_f64();
                let ticks = tick_ms.len() as f64;
                let achieved = ticks / secs * f64::from(speed) / target.fps;
                tick_ms.sort_by(|a, b| a.partial_cmp(b).expect("tick times are finite"));
                let pct = |p: f64| tick_ms[((tick_ms.len() - 1) as f64 * p) as usize];

                eprintln!(
                    "  {:<3} scene={scene:<5} requested={speed:>4.1}x  achieved={achieved:>5.2}x  \
                     efficiency={:>4.0}%  ticks/s={:>6.1}  tick_ms med={:>6.2} p95={:>6.2} \
                     max={:>6.2}",
                    target.label,
                    achieved / f64::from(speed) * 100.0,
                    ticks / secs,
                    pct(0.50),
                    pct(0.95),
                    tick_ms[tick_ms.len() - 1],
                );
            }
        }
    }

    /// Why the NDS misses 5x, in the only unit that can be optimised against.
    ///
    /// [`wall_clock_speed_ceiling_probe`] says *that* the NDS is compute-bound
    /// and by how much; it cannot say what to change. This one splits the frame
    /// into the four stages the profile already instruments and divides the
    /// interpretation stage by [`GbaCpu::instrs`], giving **nanoseconds per
    /// emulated instruction** — the number that decides between "micro-optimise
    /// the hot path" (a poor interpreter is 20+ ns/instr; a good one is 5-10)
    /// and "nothing short of a recompiler will do".
    ///
    /// Measured at 1x *and* at the requested speed, because fast-forward changes
    /// the mix: the per-frame render work is skipped for all but the last frame
    /// of a tick (see [`in_final_frames`]), so interpretation's share rises with
    /// speed and only the fast-forward row is evidence about fast-forward.
    ///
    /// Reports what 5x would require, so the verdict is arithmetic and not
    /// opinion: `needed_ns_per_instr` is what the interpreter would have to cost
    /// for a tick to fit in one frame's wall clock with the other stages
    /// unchanged. A negative value means the non-CPU stages alone already
    /// exceed the budget and interpreter work cannot get there on its own.
    ///
    /// Sets `prof_cpu_on` itself rather than honouring `EMU_PROF_CPU`: the split
    /// *is* this probe's output, and the ~2% the clock reads cost is charged to
    /// every row equally, so the shares stay comparable.
    ///
    /// `#[ignore]`: costs wall-clock time and needs a scene (see
    /// [`load_probe_scene`]).
    #[test]
    #[ignore = "manual perf probe; run explicitly with --ignored --nocapture"]
    fn nds_interpreter_cost_probe() {
        use std::time::Instant;

        /// 1.0 anchors the mix; 5.0 is the target the player's speed row offers.
        const SPEEDS: [f32; 2] = [1.0, 5.0];
        /// Ticks measured per row. At 5x a tick is ~44 ms, so this is ~5 s —
        /// long enough that one scheduler hiccup cannot move the mean.
        const TICKS: u32 = 120;
        // (the synthetic probe's own constants live with it, below)
        /// Discarded before timing: refills the caches and lets the
        /// speed-dependent state (resampler, render-skip phase) settle.
        const WARMUP: u32 = 30;

        let mut emu = Emulator::new();
        if let Err(e) = load_probe_scene(&mut emu) {
            eprintln!("SKIP nds_interpreter_cost_probe: {e}");
            return;
        }
        emu.is_playing = true;
        emu.set_audio_sample_rate(48_000);
        emu.nds_mmu.prof_cpu_on = true;

        eprintln!(
            "NDS INTERPRETER COST (per emulated frame; budget {:.2} ms/frame)",
            1000.0 / NDS_FPS
        );
        for &speed in &SPEEDS {
            emu.set_speed(speed);
            assert_eq!(emu.get_speed(), speed, "set_speed rejected {speed}");
            for _ in 0..WARMUP {
                emu.tick();
            }

            emu.nds_mmu.gx.engine.prof_raster_ns = 0;
            emu.nds_ppu.prof_render_ns = 0;
            emu.nds_mmu.prof_cpu_ns = 0;
            emu.nds_mmu.prof_cpu9_ns = 0;
            emu.nds_arm9.cpu.instrs = 0;
            emu.nds_arm7.cpu.instrs = 0;
            emu.nds_arm9.cpu.thumb_instrs = 0;
            emu.nds_arm7.cpu.thumb_instrs = 0;
            emu.nds_arm9.cpu.arm_class_hist = [0; 6];
            emu.nds_arm7.cpu.arm_class_hist = [0; 6];
            let t0 = Instant::now();
            for _ in 0..TICKS {
                emu.tick();
            }
            let wall_ms = t0.elapsed().as_secs_f64() * 1000.0;

            // One tick advances `speed` video frames, so every stage is
            // normalised per *emulated* frame — the unit the 16.72 ms budget is
            // expressed in and the only one comparable across the two rows.
            let frames = f64::from(TICKS) * f64::from(speed);
            let per_frame = |ns: u64| ns as f64 / 1e6 / frames;
            let raster_ms = per_frame(emu.nds_mmu.gx.engine.prof_raster_ns);
            let scanline_ms = per_frame(emu.nds_ppu.prof_render_ns);
            // The 3D rasterizer runs inside the CPU store that writes
            // SWAP_BUFFERS, so its time is already inside `prof_cpu_ns`;
            // subtracting it keeps the four stages disjoint.
            let cpu_ms = per_frame(emu.nds_mmu.prof_cpu_ns) - raster_ms;
            // The ARM9's share of it. The rasterizer runs inside an ARM9 store,
            // so `raster_ms` comes out of the ARM9 side, not the ARM7's.
            let cpu9_ms = per_frame(emu.nds_mmu.prof_cpu9_ns) - raster_ms;
            let cpu7_ms = cpu_ms - cpu9_ms;
            let total_ms = wall_ms / frames;
            let rest_ms = total_ms - cpu_ms - raster_ms - scanline_ms;

            // Per *frame*, to match `cpu_ms`: dividing a per-frame millisecond
            // figure by the whole run's instruction count is off by `frames`.
            let arm9_per_frame = emu.nds_arm9.cpu.instrs as f64 / frames;
            let arm7_per_frame = emu.nds_arm7.cpu.instrs as f64 / frames;
            let instrs = (arm9_per_frame + arm7_per_frame).max(1.0);
            let per_instr_ns = cpu_ms * 1e6 / instrs;
            // What interpretation would have to cost for the whole frame to fit
            // in the pacer's budget, everything else held where it measures. A
            // negative value means the other stages alone already overrun it.
            // A tick is paced to 1/fps seconds and covers `speed` emulated
            // frames, so the wall-clock budget **per emulated frame** is
            // `1/fps / speed` — that division is what fast-forward *is*, and
            // leaving it out makes every row read as comfortably in budget.
            let budget_ms = 1000.0 / NDS_FPS / f64::from(speed);
            let needed_ns = (budget_ms - rest_ms - raster_ms - scanline_ms) * 1e6 / instrs;

            eprintln!(
                "  {speed:>3.1}x  budget={budget_ms:>4.2}ms  frame={total_ms:>6.2}ms = \
                 cpu {cpu_ms:>5.2} (arm9 {cpu9_ms:>4.2} + arm7 {cpu7_ms:>4.2}) + 3d {raster_ms:>4.2} \
                 + 2d {scanline_ms:>4.2} + rest {rest_ms:>4.2}  |  \
                 instr/frame arm9={:>7} arm7={:>7} thumb={:>3.0}%  {per_instr_ns:>5.1} ns/instr  \
                 (fitting the budget needs {needed_ns:>5.1} ns/instr = {:>4.1}x faster)",
                arm9_per_frame as u64,
                arm7_per_frame as u64,
                // Which decoder the frame actually spends its time in. ARM and
                // Thumb have separate cascades, so a change to one moves the
                // frame only in proportion to this share.
                100.0 * (emu.nds_arm9.cpu.thumb_instrs + emu.nds_arm7.cpu.thumb_instrs) as f64
                    / (emu.nds_arm9.cpu.instrs + emu.nds_arm7.cpu.instrs).max(1) as f64,
                per_instr_ns / needed_ns.max(f64::MIN_POSITIVE),
            );
            // Which ARM handlers the scene actually runs. Percentages of all
            // retired instructions (Thumb included in the denominator) so the
            // columns and the `thumb` figure above add up to 100.
            let total = (emu.nds_arm9.cpu.instrs + emu.nds_arm7.cpu.instrs).max(1) as f64;
            let pct = |i: usize| {
                100.0
                    * (emu.nds_arm9.cpu.arm_class_hist[i] + emu.nds_arm7.cpu.arm_class_hist[i])
                        as f64
                    / total
            };
            eprintln!(
                "        ARM class mix: dp_reg {:>4.1}%  dp_imm {:>4.1}%  ldr/str {:>4.1}%  \
                 ldm/stm {:>4.1}%  branch {:>4.1}%  cop/swi {:>4.1}%",
                pct(0),
                pct(1),
                pct(2),
                pct(3),
                pct(4),
                pct(5),
            );
        }
    }

    /// What does one emulated ARM9 instruction cost with a *perfect* memory
    /// system and no peripherals at all?
    ///
    /// [`nds_interpreter_cost_probe`] says interpretation costs ~29 ns per
    /// instruction on the real workload, which is 3-5x what a good ARM
    /// interpreter costs — but it cannot say *why*, and the two candidate
    /// answers demand opposite work:
    ///
    /// * **The interpreter body** (the decode cascade, the handler, the
    ///   per-step bookkeeping in `Arm9Cpu::step`) — then a predecode or
    ///   recompiler is the lever and the memory map is irrelevant.
    /// * **The memory system** (cache misses walking a 4 MB `Vec`, the region
    ///   decode, the `NdsMmu` fields the hot path touches) — then the lever is
    ///   locality, and rewriting the decode would buy nothing.
    ///
    /// This isolates the first: a straight-line loop in ITCM or main RAM,
    /// stepped directly with no timers, no PPU, no APU and no second core, so
    /// the only cost left is `step` plus the fetch. Compare its ns/instr
    /// against the real workload's; the gap is what the memory system and the
    /// run loop add.
    ///
    /// Two regions because they answer different halves: ITCM is 32 KB and
    /// stays in L1, so it is close to the interpreter's floor, while main RAM
    /// is the 4 MB allocation real code runs from.
    ///
    /// Where the ARM9 block recompiler's work goes on the player's scene.
    ///
    /// The recompiler measured **37% slower** than the interpreter at a 5x
    /// request, so the question is not "how much faster" but "what is being
    /// paid for nothing". Four candidates, and this separates them:
    ///
    /// * **declines** — an address the compiler cannot handle re-runs the scan
    ///   and the compile attempt on every visit, for no benefit;
    /// * **block length** — `compiled / entries` says how much emulated work
    ///   each block prologue and epilogue is amortised over;
    /// * **cache thrash** — `compilations` close to `cache_hits` means blocks
    ///   are being invalidated as fast as they are built;
    /// * **coverage** — `compiled` against the ARM9's retired total.
    ///
    /// Tick-lockstep forensics for the ARM7 recompiler: two whole emulators,
    /// identical inputs, ARM7 JIT on in one — compare the ARM7-visible state
    /// every tick and name the first divergence. The differential harness
    /// cannot see this class: it never fires IRQs, never routes IO, never
    /// interleaves cores. This does all three by construction.
    #[test]
    #[ignore = "manual forensics; run with --ignored --nocapture"]
    fn nds_arm7_jit_boot_lockstep() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let rom = repo.join("roms").join("Pokemon - SoulSilver Version (USA).nds");
        if !rom.exists() {
            eprintln!("SKIP: ROM absent");
            return;
        }
        let boot = |jit7: bool| {
            let mut emu = Emulator::new();
            let res = emu
                .load_rom_path(rom.to_str().unwrap(), repo.to_str().unwrap());
            assert!(res.starts_with("LOAD_ROM_OK"), "{res}");
            emu.nds_arm7.set_jit_enabled(jit7);
            emu.nds_arm9.set_jit_enabled(true);
            emu.is_playing = true;
            emu.set_audio_sample_rate(48_000);
            emu
        };
        let mut a = boot(false);
        let mut b = boot(true);
        for tick in 0..4000u32 {
            a.tick();
            b.tick();
            let sa = (
                a.nds_arm7.cpu.registers.gpr,
                a.nds_arm7.cpu.registers.cpsr,
                a.nds_arm7.cpu.instrs,
                a.nds_mmu.arm7_if,
                a.nds_arm9.cpu.instrs,
            );
            let sb = (
                b.nds_arm7.cpu.registers.gpr,
                b.nds_arm7.cpu.registers.cpsr,
                b.nds_arm7.cpu.instrs,
                b.nds_mmu.arm7_if,
                b.nds_arm9.cpu.instrs,
            );
            if sa != sb {
                eprintln!("FIRST DIVERGENCE at tick {tick}");
                eprintln!("  interp: pc={:#010x} cpsr={:#010x} instrs={} if7={:#010x} instrs9={}",
                    sa.0[15], sa.1, sa.2, sa.3, sa.4);
                eprintln!("  jit   : pc={:#010x} cpsr={:#010x} instrs={} if7={:#010x} instrs9={}",
                    sb.0[15], sb.1, sb.2, sb.3, sb.4);
                eprintln!(
                    "  halt7 {} vs {}   irqs7 {} vs {}   halt9 {} vs {}",
                    a.nds_mmu.arm7_halt_cycles,
                    b.nds_mmu.arm7_halt_cycles,
                    a.nds_mmu.arm7_irqs_taken,
                    b.nds_mmu.arm7_irqs_taken,
                    a.nds_mmu.arm9_halt_cycles,
                    b.nds_mmu.arm9_halt_cycles,
                );
                for r in 0..16 {
                    if sa.0[r] != sb.0[r] {
                        eprintln!("  r{r}: {:#010x} vs {:#010x}", sa.0[r], sb.0[r]);
                    }
                }
                let base = sa.0[15].wrapping_sub(0x30) & !3;
                for i in 0..20u32 {
                    let addr = base.wrapping_add(i * 4);
                    eprintln!(
                        "  {addr:#010x}: {:#010x}{}",
                        a.nds_mmu.read_word_arm7(addr),
                        if addr == sa.0[15].wrapping_sub(8) { "   <- interp executing" } else { "" },
                    );
                }
                // Keep the detailed dump, then fail through the same assertion
                // used for every tick below. A divergence must fail the test.
            }
            assert_arm7_lockstep_matches(tick, sa, sb);
        }
        eprintln!("no divergence in 4000 ticks");
    }

    type Arm7LockstepState = ([u32; 16], u32, u64, u32, u64);

    fn assert_arm7_lockstep_matches(tick: u32, reference: Arm7LockstepState, jit: Arm7LockstepState) {
        assert_eq!(reference, jit, "ARM7 diverged at tick {tick}");
    }

    #[test]
    fn arm7_lockstep_accepts_matching_state() {
        let state = ([0; 16], 0x1F, 10, 0, 20);
        assert_arm7_lockstep_matches(0, state, state);
    }

    #[test]
    #[should_panic(expected = "ARM7 diverged at tick 7")]
    fn arm7_lockstep_rejects_divergent_state() {
        let reference = ([0; 16], 0x1F, 10, 0, 20);
        let mut jit = reference;
        jit.0[0] = 1;
        assert_arm7_lockstep_matches(7, reference, jit);
    }

    /// ARM7 twin of [`nds_arm9_jit_report`], plus the cross-core interference
    /// counters: an ARM7 store landing on a page that holds compiled code
    /// moves the global code-write epoch, which also tears down every ARM9
    /// link — if that churns, the ARM9 row here names it. Run with
    /// `EMU_ARM7_JIT=1` (or rely on the explicit enable below).
    #[test]
    #[ignore = "manual perf probe; needs EMU_STATE_DIR; run with --ignored --nocapture"]
    fn nds_arm7_jit_report() {
        const TICKS: u32 = 60;
        const WARMUP: u32 = 20;

        let mut emu = Emulator::new();
        if let Err(e) = load_probe_scene(&mut emu) {
            eprintln!("SKIP nds_arm7_jit_report: {e}");
            return;
        }
        emu.is_playing = true;
        emu.set_audio_sample_rate(48_000);
        emu.set_speed(5.0);
        emu.nds_arm7.set_jit_enabled(true);
        emu.nds_arm7.set_jit_diagnostics(true);
        emu.nds_arm9.set_jit_diagnostics(true);

        for _ in 0..WARMUP {
            emu.tick();
        }
        let before = emu.nds_arm7.jit_stats().expect("the ARM7 recompiler is enabled");
        let instrs_before = emu.nds_arm7.cpu.instrs;
        let thumb_before = emu.nds_arm7.cpu.thumb_instrs;
        let stops_before = emu.nds_arm7.jit_stop_counts().expect("enabled");
        let by_entry_before = emu.nds_arm7.jit_entry_exit_counts().expect("enabled");
        let (links_before, flushes_before) = emu.nds_arm7.jit_link_stats().expect("enabled");
        let dispatches_before = emu.nds_arm7.jit_dispatch_stats().expect("enabled");
        let chain_ends_before = emu.nds_arm7.jit_chain_end_counts().expect("enabled");
        let arm9_links_before = emu.nds_arm9.jit_link_stats();
        for _ in 0..TICKS {
            emu.tick();
        }
        let after = emu.nds_arm7.jit_stats().expect("still enabled");
        let retired = emu.nds_arm7.cpu.instrs - instrs_before;
        let thumb = emu.nds_arm7.cpu.thumb_instrs - thumb_before;

        let compiled = after.compiled_instrs - before.compiled_instrs;
        let hits = after.cache_hits - before.cache_hits;
        let builds = after.compilations - before.compilations;
        let declined = after.declined - before.declined;
        let entries = hits + builds;

        eprintln!("ARM7 JIT REPORT over {TICKS} ticks at 5x");
        eprintln!(
            "  arm7 instructions retired : {retired}  (thumb {thumb} = {:.1}%)",
            100.0 * thumb as f64 / retired.max(1) as f64
        );
        eprintln!(
            "  executed as compiled code : {compiled} ({:.1}% of retired)",
            100.0 * compiled as f64 / retired.max(1) as f64
        );
        eprintln!(
            "  block entries             : {entries}  (mean {:.2} instructions each)",
            compiled as f64 / entries.max(1) as f64
        );
        eprintln!(
            "  cache hits / compilations : {hits} / {builds}  ({:.1}% hit rate) — builds close to hits = GUARD THRASH",
            100.0 * hits as f64 / entries.max(1) as f64
        );
        eprintln!(
            "  guard revalidations       : {}",
            emu.nds_arm7.jit_revalidations().unwrap_or(0)
        );
        eprintln!(
            "  declined                  : {declined}  ({:.1}% of all attempts)",
            100.0 * declined as f64 / (declined + entries).max(1) as f64
        );

        let stops: [u64; 10] = {
            let now = emu.nds_arm7.jit_stop_counts().expect("enabled");
            std::array::from_fn(|i| now[i] - stops_before[i])
        };
        let total_stops: u64 = stops.iter().sum();
        eprintln!("  why the recompiler stood down (total {total_stops}):");
        for (reason, count) in crate::jit::runner::StopReason::ALL.iter().zip(&stops) {
            eprintln!(
                "    {:<38} {count:>12}  ({:.1}%)",
                reason.label(),
                100.0 * *count as f64 / total_stops.max(1) as f64
            );
        }

        let (links_now, flushes_now) = emu.nds_arm7.jit_link_stats().expect("enabled");
        let dispatches_now = emu.nds_arm7.jit_dispatch_stats().expect("enabled");
        eprintln!(
            "  links written / flush passes : {} / {}   dispatch entries {}",
            links_now - links_before,
            flushes_now - flushes_before,
            dispatches_now - dispatches_before,
        );
        let ends: [u64; 5] = {
            let now = emu.nds_arm7.jit_chain_end_counts().expect("enabled");
            std::array::from_fn(|i| now[i] - chain_ends_before[i])
        };
        eprintln!(
            "  chain ends: budget {} store-stop {} not-linked {} no-target {} dispatch-miss {}",
            ends[0], ends[1], ends[2], ends[3], ends[4]
        );
        let by_entry: [u64; 14] = {
            let now = emu.nds_arm7.jit_entry_exit_counts().expect("enabled");
            std::array::from_fn(|i| now[i] - by_entry_before[i])
        };
        eprintln!("  entry-weighted exit slots : {by_entry:?}");

        // The cross-core interference witness: ARM9 link teardowns while the
        // ARM7 recompiler runs. The ARM9's own row without the ARM7 was ~0
        // flush passes on this scene.
        if let (Some((l0, f0)), Some((l1, f1))) =
            (arm9_links_before, emu.nds_arm9.jit_link_stats())
        {
            eprintln!(
                "  ARM9 while ARM7 ran: links written {} / flush passes {}",
                l1 - l0,
                f1 - f0
            );
        }

        // The decline census: which addresses the 7.7M filter hits actually
        // are, with the instruction word the translator refused — the row
        // that names the next encoding to translate.
        if let Some(top) = emu.nds_arm7.jit_declined_top(24) {
            eprintln!("  most-hit declined addresses (addr / hits / first word):");
            for (addr, hits, word) in top {
                eprintln!("    {addr:#010x}  x{hits:<10}  {word:#010x}");
            }
        }
    }

    /// `#[ignore]`: needs the player's savestate, and it is a timing probe.
    #[test]
    #[ignore = "manual perf probe; needs EMU_STATE_DIR; run with --ignored --nocapture"]
    fn nds_arm9_jit_report() {
        const TICKS: u32 = 60;
        const WARMUP: u32 = 20;

        let mut emu = Emulator::new();
        if let Err(e) = load_probe_scene(&mut emu) {
            eprintln!("SKIP nds_arm9_jit_report: {e}");
            return;
        }
        emu.is_playing = true;
        emu.set_audio_sample_rate(48_000);
        emu.set_speed(5.0);
        emu.nds_arm9.set_jit_enabled(true);
        emu.nds_arm9.set_jit_diagnostics(true);

        /// Elementwise delta of two histogram snapshots.
        ///
        /// Every counter here has to cover the same window. `JitStats` was
        /// already differenced against a post-warmup snapshot while the
        /// histograms were read raw, so the two disagreed by exactly the
        /// warmup — 13,207,425 census entries over 80 ticks against 9,905,578
        /// stats entries over 60 — and any ratio spanning both was wrong by
        /// 33%. The counters are monotonic, so a plain subtraction is total.
        fn delta<const N: usize>(after: [u64; N], before: [u64; N]) -> [u64; N] {
            std::array::from_fn(|i| after[i] - before[i])
        }

        for _ in 0..WARMUP {
            emu.tick();
        }
        let before = emu.nds_arm9.jit_stats().expect("the recompiler is enabled");
        let instrs_before = emu.nds_arm9.cpu.instrs;
        let stops_before = emu.nds_arm9.jit_stop_counts().expect("enabled");
        let exits_before = emu.nds_arm9.jit_exit_counts().expect("enabled");
        let by_entry_before = emu.nds_arm9.jit_entry_exit_counts().expect("enabled");
        let empty_before = emu.nds_arm9.jit_empty_exit_counts().expect("enabled");
        let lost_before = emu.nds_arm9.jit_chain_loss_counts().expect("enabled");
        let chain_ends_before = emu.nds_arm9.jit_chain_end_counts().expect("enabled");
        let link_refusals_before = emu.nds_arm9.jit_link_refusal_counts().expect("enabled");
        let (edges_before, linkable_before) = emu.nds_arm9.jit_chain_stats().expect("enabled");
        let (evictions_before, _) = emu.nds_arm9.jit_hot_filter_stats().expect("enabled");
        for _ in 0..TICKS {
            emu.tick();
        }
        let after = emu.nds_arm9.jit_stats().expect("still enabled");
        let retired = emu.nds_arm9.cpu.instrs - instrs_before;

        let compiled = after.compiled_instrs - before.compiled_instrs;
        let hits = after.cache_hits - before.cache_hits;
        let builds = after.compilations - before.compilations;
        let declined = after.declined - before.declined;
        let entries = hits + builds;

        eprintln!("ARM9 JIT REPORT over {TICKS} ticks at 5x");
        eprintln!("  arm9 instructions retired : {retired}");
        eprintln!(
            "  executed as compiled code : {compiled} ({:.1}% of retired)",
            100.0 * compiled as f64 / retired.max(1) as f64
        );
        eprintln!(
            "  block entries             : {entries}  (mean {:.2} instructions each)",
            compiled as f64 / entries.max(1) as f64
        );
        eprintln!(
            "  cache hits / compilations : {hits} / {builds}  ({:.1}% hit rate)",
            100.0 * hits as f64 / entries.max(1) as f64
        );
        eprintln!(
            "  declined                  : {declined}  ({:.1}% of all attempts)",
            100.0 * declined as f64 / (declined + entries).max(1) as f64
        );

        // Why the recompiler had nothing to run. Two coverage milestones were
        // chosen from the *static* instruction mix and both under-delivered, so
        // this reports what is actually stopping blocks rather than what is
        // merely frequent.
        let stops = delta(emu.nds_arm9.jit_stop_counts().expect("enabled"), stops_before);
        let total_stops: u64 = stops.iter().sum();
        eprintln!("  why the recompiler stood down (total {total_stops}):");
        for (reason, count) in crate::jit::runner::StopReason::ALL.iter().zip(&stops) {
            eprintln!(
                "    {:<38} {count:>12}  ({:.1}%)",
                reason.label(),
                100.0 * *count as f64 / total_stops.max(1) as f64
            );
        }

        // A declining address should be scanned once and filtered forever
        // after. Evictions close to the scan count mean it is not: the filter
        // is too small for the code working set and the scans are repeats.
        let scans: u64 = stops[2] + stops[3] + stops[4];
        let (evictions_now, slots) = emu.nds_arm9.jit_hot_filter_stats().expect("enabled");
        let evictions = evictions_now - evictions_before;
        eprintln!(
            "  address filter            : {slots} slots, {evictions} evictions over {scans} declined scans ({:.1}% are repeats)",
            100.0 * evictions as f64 / scans.max(1) as f64
        );

        // And why the blocks that *were* built ended where they did.
        let exits = delta(emu.nds_arm9.jit_exit_counts().expect("enabled"), exits_before);
        let total_exits: u64 = exits.iter().sum();
        let exit_labels = [
            "branch (not folded)",
            "BX / BLX(reg)",
            "writes R15",
            "LDM loads R15",
            "SWI",
            "CP15",
            "coprocessor",
            "MSR",
            "LDM/STM S bit",
            "length cap",
            "after a store",
            "after a folded branch",
            "after a folded branch to own start (LOOP)",
            "after a CONDITIONAL branch (fall-through live)",
        ];
        // Weighted by entries, which is the population that costs time: the
        // per-scan table below is dominated by cold blocks the scanner looked
        // at once, while entries come overwhelmingly from a handful of hot
        // ones. Reading the per-scan shares as if they were dynamic is a
        // mistake this line exists to prevent.
        let by_entry = delta(emu.nds_arm9.jit_entry_exit_counts().expect("enabled"), by_entry_before);
        let total_by_entry: u64 = by_entry.iter().sum();
        eprintln!("  where hot blocks end, PER ENTRY (total {total_by_entry}):");
        for (label, count) in exit_labels.iter().zip(&by_entry) {
            if *count > 0 {
                eprintln!(
                    "    {label:<38} {count:>12}  ({:.1}%)",
                    100.0 * *count as f64 / total_by_entry.max(1) as f64
                );
            }
        }

        // What emitted block chaining could actually remove. Entries are the
        // largest term in the ARM9 stage, but a compiled link can only replace
        // an entry whose block runs *immediately* after another block, with
        // nothing interpreted in between, and whose own block is already
        // compiled and guard-valid. Anything else still costs a full entry, so
        // this ratio — not the entry count — is the ceiling on chaining.
        let (edges_now, linkable_now) = emu.nds_arm9.jit_chain_stats().expect("enabled");
        let (edges, linkable) = (edges_now - edges_before, linkable_now - linkable_before);
        eprintln!(
            "  successor edges           : {edges}  of which linkable {linkable} ({:.1}%)",
            100.0 * linkable as f64 / edges.max(1) as f64
        );
        eprintln!(
            "    linkable share of all entries : {:.1}%  (upper bound on what chaining removes)",
            100.0 * linkable as f64 / total_by_entry.max(1) as f64
        );
        // Successor linking, when EMU_ARM9_JIT_LINK is on. With chains, one
        // `try_step` entry covers several block runs, so `block entries` above
        // is chain entries and `PER ENTRY` below counts chain-ending exits.
        if let Some((links_written, link_flushes)) = emu.nds_arm9.jit_link_stats() {
            let dispatches = emu.nds_arm9.jit_dispatch_stats().unwrap_or(0);
            eprintln!(
                "  successor links           : {links_written} written, {dispatches} dispatch entries, {link_flushes} teardown passes"
            );
        }
        if let Some(ends_now) = emu.nds_arm9.jit_chain_end_counts() {
            let ends = delta(ends_now, chain_ends_before);
            let total: u64 = ends.iter().sum();
            let labels = [
                "slice budget spent",
                "store raised the stop flag",
                "exit not linked (target uncompiled / refused)",
                "no static target (MSR / truncated / thumb)",
                "dispatch probe missed",
            ];
            eprintln!("  why chains ended (total {total}):");
            for (label, count) in labels.iter().zip(&ends) {
                eprintln!(
                    "    {label:<46} {count:>12}  ({:.1}%)",
                    100.0 * *count as f64 / total.max(1) as f64
                );
            }
        }
        if let Some(refusals_now) = emu.nds_arm9.jit_link_refusal_counts() {
            let refusals = delta(refusals_now, link_refusals_before);
            let labels = [
                "target never compiled",
                "instruction-set mismatch",
                "target words changed",
                "target pages stale",
            ];
            eprintln!("  why link/dispatch writes were refused:");
            for (label, count) in labels.iter().zip(&refusals) {
                if *count > 0 {
                    eprintln!("    {label:<46} {count:>12}");
                }
            }
        }
        if let Some(top) = emu.nds_arm9.jit_refused_target_top(12) {
            if !top.is_empty() {
                eprintln!("  most-refused uncompiled targets:");
                for (target, count, exact, declined, from, containing) in top {
                    eprintln!(
                        "    {target:#010x}  x{count:<9} cached-now={} declined={} from={from:#010x} span={}",
                        u8::from(exact),
                        u8::from(declined),
                        containing.map_or("-".into(), |s| format!("{s:#010x}")),
                    );
                    // The words the scanner would see there — what refuses?
                    let w: Vec<u32> = (0..4)
                        .map(|i| emu.nds_mmu.read_word_arm9(target.wrapping_add(4 * i)))
                        .collect();
                    eprintln!(
                        "      words: {:#010x} {:#010x} {:#010x} {:#010x}",
                        w[0], w[1], w[2], w[3]
                    );
                }
            }
        }
        // The rest, split. `edges - linkable` names several unrelated fixes at
        // once, and which of them dominates decides what to build after
        // chaining — a Thumb target and a halted target want opposite work.
        let lost = delta(emu.nds_arm9.jit_chain_loss_counts().expect("enabled"), lost_before);
        let loss_labels = crate::jit::runner::StopReason::ALL
            .iter()
            .map(|r| r.label())
            .chain(["target already in the decline filter", "compiled or scanned on this visit"]);
        for (label, count) in loss_labels.zip(&lost) {
            if *count > 0 {
                eprintln!(
                    "    unlinkable: {label:<38} {count:>12}  ({:.1}% of edges)",
                    100.0 * *count as f64 / edges.max(1) as f64
                );
            }
        }

        eprintln!("  why blocks ended, per scan (total {total_exits}):");
        for (label, count) in exit_labels.iter().zip(&exits) {
            if *count > 0 {
                eprintln!(
                    "    {label:<38} {count:>12}  ({:.1}%)",
                    100.0 * *count as f64 / total_exits.max(1) as f64
                );
            }
        }

        // The subset that produced no body at all. Ending a block is normal;
        // being unable to *start* one is what caps coverage, and these name the
        // encoding to translate next rather than merely counting the loss.
        let empty = delta(emu.nds_arm9.jit_empty_exit_counts().expect("enabled"), empty_before);
        let total_empty: u64 = empty.iter().sum();
        eprintln!("  ...of which produced NO body (total {total_empty}):");
        for (label, count) in exit_labels.iter().zip(&empty) {
            if *count > 0 {
                eprintln!(
                    "    {label:<38} {count:>12}  ({:.1}%)",
                    100.0 * *count as f64 / total_empty.max(1) as f64
                );
            }
        }

        assert!(retired > 0, "the scene did not execute");
    }

    /// **Is the generated code actually faster than the interpreter?**
    ///
    /// Everything measured on the real scene tangles three things together:
    /// codegen quality, how much is covered, and what dispatch costs. This
    /// separates the first one by running a **maximal block** — a body of
    /// `MAX_BODY_INSTRS` straight-line instructions ending in a backward branch
    /// — so the per-block cost is amortised over the longest run the scanner
    /// will produce, and coverage is 100% by construction.
    ///
    /// It is a synthetic, and this project has been burned by synthetics before
    /// (halving the ARM decode cascade: 2x synthetic, 7% real). It is not being
    /// used to predict the frame. It answers one question the frame cannot:
    /// whether the emitter's *ceiling* is high enough to be worth pursuing. If
    /// compiled code is ~3x the interpreter here, then removing dispatch and
    /// raising coverage can reach the 5x target; if it is ~1.2x, no amount of
    /// either will, and the recompiler is the wrong instrument.
    ///
    /// `#[ignore]`: a timing probe.
    #[test]
    #[ignore = "manual perf probe; run explicitly with --ignored --nocapture"]
    fn nds_arm9_jit_ceiling_probe() {
        use crate::nds::cpu::{Arm9Cpu, FLAG_T};
        use crate::nds::mmu::NdsMmu;
        use std::time::Instant;

        const CODE: u32 = 0x0200_0000;
        /// Cycles requested per `run`. Large enough that the call itself is
        /// noise, small enough to stay inside one measurement.
        const BUDGET: u32 = 200_000;
        const ROUNDS: u32 = 200;
        /// Loop bodies to price. Each block is `BODY + 1` instructions — the
        /// body plus the backward branch that closes the loop, which stops the
        /// trace because its target is already inside it.
        const BODIES: [u32; 4] = [2, 4, 8, 15];

        /// Where a `Load` body reads from: main RAM, clear of the code.
        const DATA: u32 = CODE + 0x2000;

        /// What the loop body is made of.
        ///
        /// # Why this is not just `ADD`
        ///
        /// It was, and the 8.19x ratio that came out of it was quoted for
        /// several iterations as *the* codegen ceiling. It is the ceiling for
        /// **data processing only**. Loads and stores are 14.2% of the retired
        /// mix and block transfers another 6.1%, and those compile to calls
        /// into the same `NdsMmu` decode the interpreter runs — so the ratio
        /// that matters for the frame could be far lower, and an ALU-only
        /// microbenchmark cannot see it.
        #[derive(Clone, Copy)]
        enum Kind {
            Alu,
            Load,
        }

        impl Kind {
            fn label(self) -> &'static str {
                match self {
                    Self::Alu => "ADD Rd,Rd,#1",
                    Self::Load => "LDR Rd,[r0,#n]",
                }
            }

            /// The `i`th body instruction. Destinations cycle r1..r6 so the
            /// chain is not a single dependent register.
            fn instr(self, i: u32) -> u32 {
                let reg = 1 + i % 6;
                match self {
                    // The cheapest data-processing form, so anything slower is
                    // the machinery rather than the work.
                    Self::Alu => 0xE280_0001 | (reg << 16) | (reg << 12),
                    // `LDR Rd,[r0,#off]`, offset stepping a word at a time so
                    // the loads are not all one cache line. Never r15.
                    Self::Load => 0xE590_0000 | (reg << 12) | ((i * 4) & 0xFFF),
                }
            }
        }

        fn build(jit: bool, body: u32, kind: Kind) -> (Arm9Cpu, NdsMmu) {
            let mut mmu = NdsMmu::new();
            for i in 0..body {
                mmu.write_word_arm9(CODE + i * 4, kind.instr(i));
            }
            let back = -((body as i32) + 2) as u32 & 0x00FF_FFFF;
            mmu.write_word_arm9(CODE + body * 4, 0xEA00_0000 | back);

            let mut cpu = Arm9Cpu::new();
            cpu.set_jit_enabled(jit);
            cpu.cpu.registers.cpsr = 0x1F; // System, ARM
            assert!(!cpu.cpu.registers.get_flag(FLAG_T));
            cpu.cpu.registers.gpr[0] = DATA; // base for a `Load` body
            cpu.cpu.registers.gpr[15] = CODE;
            cpu.flush_pipeline(&mut mmu);
            (cpu, mmu)
        }

        /// Nanoseconds per retired instruction.
        fn price(jit: bool, body: u32, kind: Kind) -> f64 {
            let (mut cpu, mut mmu) = build(jit, body, kind);
            for _ in 0..20 {
                cpu.run(&mut mmu, BUDGET);
            }
            cpu.cpu.instrs = 0;
            let t0 = Instant::now();
            for _ in 0..ROUNDS {
                cpu.run(&mut mmu, BUDGET);
            }
            let ns = t0.elapsed().as_secs_f64() * 1e9;
            ns / cpu.cpu.instrs.max(1) as f64
        }

        eprintln!("ARM9 JIT COST MODEL (ns per retired instruction)");
        for kind in [Kind::Alu, Kind::Load] {
            eprintln!("  body = {}", kind.label());
            eprintln!("  block   interpreter   recompiler   ratio");
            let mut fits: Vec<(f64, f64)> = Vec::new();
            for body in BODIES {
                let per_block = f64::from(body + 1);
                // Alternated inside each row, because the host drifts even here.
                let interp = (price(false, body, kind) + price(false, body, kind)) / 2.0;
                let jit = (price(true, body, kind) + price(true, body, kind)) / 2.0;
                fits.push((per_block, jit));
                eprintln!(
                    "  {:>5}   {interp:>11.2}   {jit:>10.2}   {:>5.2}x",
                    per_block,
                    interp / jit.max(f64::MIN_POSITIVE),
                );
            }

            // Two points determine the line `ns_per_instr = work + entry / length`.
            // Solving across the shortest and longest rows separates the two.
            let (short_len, short_ns) = fits[0];
            let (long_len, long_ns) = fits[fits.len() - 1];
            let entry = (short_ns - long_ns) / (1.0 / short_len - 1.0 / long_len);
            let work = long_ns - entry / long_len;
            eprintln!("  fit: work {work:.2} ns/instruction + {entry:.1} ns per block entry");
        }
        eprintln!("  (ALU ratio is the emitter's ceiling; the LOAD ratio is what");
        eprintln!("   the thunk-per-access design allows, and 20.3% of the real");
        eprintln!("   mix is loads, stores and block transfers)");
    }

    /// `#[ignore]`: a timing probe, and it deliberately runs tens of millions
    /// of instructions.
    #[test]
    #[ignore = "manual perf probe; run explicitly with --ignored --nocapture"]
    fn nds_arm9_synthetic_throughput_probe() {
        use crate::nds::cpu::Cp15Registers;
        use crate::nds::mmu::NdsMmu;
        use std::time::Instant;

        /// Instructions stepped per row. Large enough that `Instant` resolution
        /// and the loop's own setup are noise.
        const STEPS: u32 = 20_000_000;

        /// `(label, base, itcm_on, body)` — body length in instructions.
        ///
        /// The body length is the **guest code working set**, and it is the
        /// dimension that decides the next design. Every removal of front-end
        /// work so far (the TCM window, the fetch/read split, the const-generic
        /// chunks, gating the diagnostic stores, halving the ARM decode
        /// cascade) moved the real workload by 0-7%, which says the real
        /// workload is not front-end bound. A 63-instruction body is 252 bytes
        /// and lives in L1 forever; real code spans megabytes. If ns/instr
        /// climbs with body size, the cost is guest-code locality — and a
        /// predecode cache, which adds a second and larger stream over the same
        /// code, would make that worse rather than better.
        ///
        /// 63 keeps the backward branch's pipeline flush near 1.5% of the row;
        /// the larger bodies branch even less often, so any difference between
        /// rows is footprint, not branch cost.
        const BODIES: [(&str, u32, bool, u32); 4] = [
            ("itcm/252B", 0x0100_0000, true, 63),
            ("main/252B", 0x0200_0000, false, 63),
            ("main/64KB", 0x0200_0000, false, 16_383),
            ("main/1MB", 0x0200_0000, false, 262_143),
        ];

        /// `ADD Rd, Rd, #1`, the cheapest data-processing form: no barrel
        /// shift, no memory, no flag write. Anything slower than this is the
        /// interpreter, not the instruction.
        fn add_imm1(i: u32) -> u32 {
            let reg = 1 + i % 6; // r1..r6; never r0 (load base) or r15
            0xE280_0001 | (reg << 16) | (reg << 12)
        }
        /// `LDR Rd, [r0]`, the cheapest load: no offset, no writeback. Pairs
        /// with `add_imm1` to price the data path against the fetch path.
        fn ldr_r0(i: u32) -> u32 {
            let reg = 1 + i % 6;
            0xE590_0000 | (reg << 12)
        }
        /// `B` back to `base`, taken from the instruction at `at` (R15 leads by
        /// 8 under the pipeline invariant).
        fn branch_back(at: u32, base: u32) -> u32 {
            let offset = ((base as i32 - (at as i32 + 8)) >> 2) & 0x00FF_FFFF;
            0xEA00_0000 | offset as u32
        }

        /// `LDMIA r0, {r1-r8}` — one instruction, eight bus reads.
        ///
        /// LDM/STM is 7.5% of retired instructions on the overworld but moves up
        /// to 16 registers per instruction, so its share of *bus accesses* is
        /// several times its share of the instruction count. A straight-line
        /// `ADD` stream cannot show that at all.
        fn ldm_r0(_i: u32) -> u32 {
            0xE890_01FE
        }

        /// `STMIA r10, {r1-r8}` — one instruction, eight bus writes.
        ///
        /// Write-side twin of [`ldm_r0`]. It stores through **r10**, not r0,
        /// because r0 points at the body: an STM there would overwrite the
        /// instruction stream it is executing.
        fn stm_r10(_i: u32) -> u32 {
            0xE88A_01FE
        }

        /// `B .+4` — a taken branch to the very next instruction.
        ///
        /// Every taken branch sets `pc_modified`, and the next `step` calls
        /// `flush_pipeline`, which is **two** extra bus fetches. Branches are
        /// 13.5% of retired instructions, so this row prices the worst case and
        /// the real cost is a fraction of the gap between it and `alu`.
        fn branch_next(_i: u32) -> u32 {
            0xEAFF_FFFF
        }

        /// A rotating mix of eight encodings spread across the decode cascade's
        /// arms, none of which branch (so the body stays straight-line and the
        /// only difference from the uniform rows is *which* arm each instruction
        /// takes).
        ///
        /// This is the control for the last hypothesis standing. The uniform
        /// rows execute one instruction word forever, so every conditional
        /// branch inside the interpreter's decode predicts perfectly and the
        /// cascade looks nearly free. Real code has a flat class mix
        /// (`GbaCpu::arm_class_hist`: dp_reg 22%, dp_imm 18%, ldr/str 17%,
        /// ldm/stm 7.5%, branch 13.5%), so those same branches mispredict — and
        /// a mispredict is worth several nanoseconds. If `mixed` costs much more
        /// than `alu`, the interpreter's problem is branch prediction in
        /// dispatch, not the amount of work it does.
        fn mixed(i: u32) -> u32 {
            let rd = 1 + i % 6; // r1..r6; never r0 (the load base) or r15
            match i % 8 {
                0 => 0xE280_0001 | (rd << 16) | (rd << 12), // ADD Rd,Rd,#1   dp_imm
                1 => 0xE080_0002 | (rd << 16) | (rd << 12), // ADD Rd,Rd,r2   dp_reg
                2 => 0xE590_0000 | (rd << 12),              // LDR Rd,[r0]    single
                3 => 0xE3A0_0001 | (rd << 12),              // MOV Rd,#1      dp_imm
                4 => 0xE350_0000 | (rd << 16),              // CMP Rd,#0      dp_imm, S
                5 => 0xE380_0001 | (rd << 16) | (rd << 12), // ORR Rd,Rd,#1   dp_imm
                6 => 0xE020_0002 | (rd << 16) | (rd << 12), // EOR Rd,Rd,r2   dp_reg
                _ => 0xE000_0092 | (rd << 16) | (rd << 8),  // MUL Rd,r2,Rd   multiply
            }
        }

        eprintln!("NDS ARM9 SYNTHETIC THROUGHPUT ({STEPS} steps; body = guest code working set)");
        for (label, base, itcm_on, body) in BODIES {
            for (mix, encode) in [
                ("alu", add_imm1 as fn(u32) -> u32),
                ("ldr", ldr_r0 as fn(u32) -> u32),
                ("mixed", mixed as fn(u32) -> u32),
                ("ldm", ldm_r0 as fn(u32) -> u32),
                ("stm", stm_r10 as fn(u32) -> u32),
                ("branch", branch_next as fn(u32) -> u32),
            ] {
                let mut mmu = NdsMmu::new();
                // ITCM: base from `itcm_control`, size 512 << 6 = 32 KB, enable
                // is control bit 18. Left disabled for the main-RAM rows so the
                // window cannot shadow them.
                mmu.set_cp15(Cp15Registers {
                    control: if itcm_on { 1 << 18 } else { 0 },
                    itcm_control: 0x0100_0000 | (6 << 1),
                    dtcm_control: 0,
                });
                for i in 0..body {
                    // `alu`/`ldr` ignore all but the register number; `mixed`
                    // uses the index to rotate through encoding classes.
                    mmu.write_word_arm9(base + i * 4, encode(i));
                }
                mmu.write_word_arm9(base + body * 4, branch_back(base + body * 4, base));

                let mut cpu = crate::nds::cpu::Arm9Cpu::new();
                cpu.cpu.registers.cpsr = 0x1F; // System mode, ARM state
                // The `ldr` body reads `[r0]`, and this is what decides whether
                // it prices the *fast* path or the cold one. Left at 0 it
                // addresses neither the ITCM window (base 0x01000000) nor main
                // RAM, so every load fell through `contiguous_arm9` into the
                // four-byte-read region decode — measuring the fallback that
                // game code essentially never takes.
                cpu.cpu.registers.gpr[0] = base;
                // `stm` stores through r10, far enough into main RAM that it
                // cannot land on the body even at the 1 MB size.
                cpu.cpu.registers.gpr[10] = 0x0230_0000;
                cpu.cpu.registers.gpr[15] = base;
                cpu.flush_pipeline(&mut mmu);
                cpu.cpu.instrs = 0;

                let t0 = Instant::now();
                for _ in 0..STEPS {
                    cpu.step(&mut mmu);
                }
                let step_ns = t0.elapsed().as_nanos() as f64 / f64::from(STEPS);
                assert_eq!(cpu.cpu.instrs, u64::from(STEPS), "{label}/{mix}: steps must retire");

                // The same instruction with `Arm9Cpu::step` taken out of the
                // picture: no pipeline shuffle, no instruction fetch, no IRQ
                // poll, no `arm9_exec_pc`/`_lr` diagnostic stores, no CP15
                // interception — just the decode cascade and the handler. The
                // difference between the two columns is what `step` itself
                // costs, and it is the number that decides whether a faster
                // interpreter means a better decode or a leaner step.
                //
                // Measured in the same process and back to back, because
                // absolutes on this host drift ~25% between runs.
                // Replay the SAME stream the step loop ran, not one fixed
                // word: with a mixed body a single word prices one arm of the
                // cascade and the subtraction below becomes meaningless (it
                // produced a ~0 ns "step overhead", which is impossible).
                let stream: Vec<u32> = (0..body.min(4096)).map(encode).collect();
                let t0 = Instant::now();
                for k in 0..STEPS {
                    let mut bus = crate::nds::cpu::Arm9Bus(&mut mmu);
                    cpu.cpu.execute_arm(stream[k as usize % stream.len()], &mut bus);
                }
                let exec_ns = t0.elapsed().as_nanos() as f64 / f64::from(STEPS);

                eprintln!(
                    "  {label:<10} {mix:<3}  step {step_ns:>5.1} ns  = decode+handler {exec_ns:>5.1} \
                     + step overhead {:>5.1}",
                    step_ns - exec_ns
                );
            }
        }
    }

    /// Does a **cold boot** still complete its ARM9/ARM7 IPC handshake?
    ///
    /// The guard for every change to the run loop's core interleave —
    /// [`nds_interleave_cycles`], the yield rule, or how `slice_7` is derived.
    /// The handshake is a tight IPCSYNC ping-pong where each side polls with a
    /// short timeout, so a scheduling change that lets one core outrun the other
    /// stalls it, and the failure is **silent**: the machine keeps running,
    /// draws a plausible frame, and produces no sound at all. The in-game
    /// probes cannot see this, because they start from a savestate taken after
    /// the handshake already succeeded.
    ///
    /// Audio is the discriminator, not video: a coarsened slice was measured
    /// producing audio RMS 0.0 / peak 0 where a working boot gives RMS ~1300 /
    /// peak ~10000. The frame checksum is reported (not asserted) as a second
    /// signal — it legitimately moves whenever the interleave changes, whereas
    /// "the sound driver came up" must not.
    ///
    /// `#[ignore]`: ~4000 ticks of cold boot, and it needs the commercial ROM.
    #[test]
    #[ignore = "manual boot guard; run explicitly with --ignored --nocapture"]
    fn nds_boot_handshake_audio_guard() {
        /// Far enough past the handshake that the sound driver has started
        /// streaming; the historical reference numbers were taken here.
        const TICKS: u32 = 4000;
        /// A boot whose handshake died measures exactly 0. Anything in the
        /// hundreds means channels are being keyed and mixed, so this only has
        /// to separate "silence" from "audio", not pin a waveform.
        const SILENCE_FLOOR: f64 = 100.0;

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../roms/Pokemon - SoulSilver Version (USA).nds"
        );
        let rom = match std::fs::read(path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("SKIP nds_boot_handshake_audio_guard (no ROM): {e}");
                return;
            }
        };
        let mut emu = Emulator::new();
        assert!(emu.load_rom(&rom), "load_rom failed");
        emu.is_playing = true;
        emu.set_audio_sample_rate(48_000);

        let mut sum_sq = 0.0f64;
        let mut samples = 0usize;
        let mut peak = 0i16;
        for _ in 0..TICKS {
            emu.tick();
            for &s in emu.get_audio_buffer() {
                sum_sq += f64::from(s) * f64::from(s);
                peak = peak.max(s.abs());
                samples += 1;
            }
        }
        let rms = (sum_sq / samples.max(1) as f64).sqrt();
        // FNV-1a over the presented frame: one number that changes if any pixel
        // does, so a re-baselined interleave is visible rather than assumed.
        let frame_hash = emu
            .get_video_buffer()
            .iter()
            .fold(0xcbf2_9ce4_8422_2325u64, |h, &p| {
                (h ^ u64::from(p)).wrapping_mul(0x1000_0000_01b3)
            });

        eprintln!(
            "BOOT GUARD after {TICKS} ticks: audio rms={rms:.1} peak={peak} samples={samples} \
             frame_hash={frame_hash:#018x}"
        );
        assert!(
            rms > SILENCE_FLOOR,
            "cold boot produced silence (rms={rms:.1}) — the ARM9/ARM7 IPC handshake did not \
             complete; see this test's doc comment"
        );
    }

    /// Smallest ROM image `load_rom` accepts as a Nintendo DS cartridge: a valid
    /// header (title, both binaries' offsets/entries/sizes, the GBA logo the
    /// detector keys on, a correct header CRC) plus four bytes of code per core.
    ///
    /// Shared by the boot test and the savestate test so there is one definition
    /// of "a minimal NDS cartridge".
    fn mock_nds_rom() -> Vec<u8> {
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

        rom_data
    }

    #[test]
    fn test_nds_rom_booting_on_load() {
        let rom_data = mock_nds_rom();

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
                    apply_overworld_input(&mut emu4.buttons, frame);
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
                // Close the APU deferral, as `tick()` does once per frame —
                // `sample_count` below is short by the pending tail otherwise.
                emu4.nds_mmu.flush_apu(&mut abuf, 0, 1.0);
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
            emu2.nds_mmu.cp15().control,
            emu2.nds_mmu.cp15().itcm_control,
            emu2.nds_mmu.cp15().dtcm_control,
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

    /// The host's rate must reach every console's resampler, because each one
    /// derives `cycles_per_sample` from it; a console left behind would emit at
    /// the wrong rate and be resampled by SDL, which is the stage this whole
    /// mechanism exists to eliminate. The buffer must also grow, or a high-rate
    /// device silently truncates in `BoxResampler::tick`.
    #[test]
    fn host_sample_rate_reaches_every_console_and_sizes_the_buffer() {
        let mut emu = Emulator::new();
        assert_eq!(emu.output_hz, crate::resampler::DEFAULT_OUTPUT_HZ);

        emu.set_audio_sample_rate(48_000);
        assert_eq!(emu.output_hz, 48_000);
        for (hz, who) in [
            (emu.gbc_mmu.apu.resampler.output_hz(), "gbc"),
            (emu.gba_mmu.apu.resampler.output_hz(), "gba"),
            (emu.nds_mmu.apu.resampler.output_hz(), "nds"),
        ] {
            assert_eq!(hz, 48_000.0, "{who} resampler kept the old rate");
        }
        assert_eq!(emu.audio_frames_per_tick(), 800, "48000/60 stereo frames per tick");

        // A rate no device offers still must not be able to overrun the buffer.
        emu.set_audio_sample_rate(192_000);
        let need = emu.audio_frames_per_tick() * 2 + emu.audio_offset;
        assert!(
            emu.raw_audio_buffer.len() >= need,
            "buffer {} too small for {need} i16 at 192 kHz",
            emu.raw_audio_buffer.len()
        );

        // Out-of-range requests clamp rather than producing a division by zero
        // in `cycles_per_sample`.
        emu.set_audio_sample_rate(0);
        assert!(emu.output_hz >= 8_000);
    }

    /// `frame_skip` feeds `ticks % (frame_skip + 1)`, so an unvalidated value
    /// near `u32::MAX` either overflows the add (debug) or wraps it to a zero
    /// divisor (release) — a panic either way, and Rust panics abort across the
    /// cxx FFI boundary. It was reachable: the frontend holds `frame_skip` as
    /// `int` and passes it to a `u32` parameter, so `--frame-skip -1` arrived
    /// here as `u32::MAX`.
    #[test]
    fn frame_skip_is_bounded_and_tick_never_divides_by_zero() {
        let mut emu = Emulator::new();
        emu.set_frame_skip(Emulator::MAX_FRAME_SKIP);
        assert_eq!(emu.get_frame_skip(), Emulator::MAX_FRAME_SKIP);

        emu.set_frame_skip(u32::MAX);
        assert_eq!(
            emu.get_frame_skip(),
            Emulator::MAX_FRAME_SKIP,
            "an out-of-range frame skip must be ignored, not stored"
        );

        // And the render gate must hold even if the field is reached directly.
        emu.frame_skip = u32::MAX;
        emu.is_playing = true;
        emu.tick(); // panicked here before the divisor was made saturating
        assert_eq!(emu.get_ticks(), 1, "the tick must have run to completion");
    }

    /// The host rate must SURVIVE the two events that rebuild an APU: loading
    /// a ROM (from the in-app browser, i.e. after the device is already open)
    /// and an in-game RESET.
    ///
    /// The frontend calls `set_audio_sample_rate` exactly once, when it opens
    /// the audio device, so a resampler that falls back to `DEFAULT_OUTPUT_HZ`
    /// here stays there for the whole session: 735 stereo frames produced per
    /// tick into a 48 kHz device that consumes 800 — audible as stutter, with
    /// a ~8.8% pitch/tempo error on top.
    #[test]
    fn host_sample_rate_survives_rom_load_and_reset() {
        let mut emu = Emulator::new();
        emu.set_audio_sample_rate(48_000);

        // `load_rom` -> `reset_on_rom_load`, which rebuilds the GBA APU for
        // every console and replaces whole MMUs on the GBA/GBC branches.
        assert!(emu.load_rom(&mock_nds_rom()), "mock NDS ROM must load");
        for (hz, who) in [
            (emu.gbc_mmu.apu.resampler.output_hz(), "gbc"),
            (emu.gba_mmu.apu.resampler.output_hz(), "gba"),
            (emu.nds_mmu.apu.resampler.output_hz(), "nds"),
        ] {
            assert_eq!(hz, 48_000.0, "{who} resampler reverted on ROM load");
        }

        // In-game RESET: the GBA branch does `apu = GbaApu::new()`.
        emu.console_type = crate::ffi::ConsoleType::Gba;
        emu.reset();
        assert_eq!(
            emu.gba_mmu.apu.resampler.output_hz(),
            48_000.0,
            "gba resampler reverted on RESET"
        );
    }

    /// Input schedule that walks SoulSilver from power-on into the bedroom:
    /// A at 4400 (title -> info), the "no info needed" touch at (128,153) at
    /// 4900, the guide's "Touch" button at (212,174) at 5300, then A plus that
    /// same touch every 90 frames to blast through Oak's dialogs and the
    /// wake-up. Mirrors the headless-exe schedule measured to reach the
    /// overworld; one copy, so the probe and the snapshot capture below cannot
    /// drift apart.
    fn apply_overworld_input(b: &mut ButtonState, frame: u32) {
        b.start = false;
        b.select = false;
        let (mut a, mut tx, mut ty, mut tp) = (false, 212u16, 174u16, false);
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
        b.a = a;
        b.nds_touch_x = tx;
        b.nds_touch_y = ty;
        b.nds_touch_pressed = tp;
    }

    /// Path of the reusable in-game snapshot the tests below share.
    ///
    /// `EMU_SNAP_NAME` selects a different one, so several scenes can be kept
    /// side by side — comparing a scene where characters DO render against one
    /// where they do not is the whole point.
    fn ingame_snapshot_path() -> String {
        let name = std::env::var("EMU_SNAP_NAME").unwrap_or_else(|_| "nds_ingame".to_string());
        format!("{}/{name}.snap", evidence_dir())
    }

    /// Capture an in-game snapshot once, so every later experiment starts in the
    /// overworld instead of paying ~17000 frames of boot and menu-mashing.
    ///
    /// Writes the raw snapshot payload (no file header: this is an internal tool,
    /// not a player-facing slot) plus a PPM of the frame it stopped on, so the
    /// capture can be eyeballed before anything is concluded from it.
    #[test]
    #[ignore = "ROM-gated tool; run with --ignored --nocapture to (re)capture"]
    fn nds_capture_ingame_snapshot() {
        use crate::snapshot::Writer;
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../roms/Pokemon - SoulSilver Version (USA).nds"
        );
        let rom = match std::fs::read(path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("SKIP in-game capture (no ROM): {e}");
                return;
            }
        };
        let ticks: u32 = std::env::var("EMU_INGAME_TICKS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(17000);

        let mut emu = Emulator::new();
        assert!(emu.load_rom(&rom), "load_rom failed");
        emu.is_playing = true;
        for f in 0..ticks {
            apply_overworld_input(&mut emu.buttons, f);
            emu.tick();
        }

        let mut w = Writer::default();
        emu.snap_nds(&mut w);
        let out = ingame_snapshot_path();
        std::fs::write(&out, &w.out).expect("write snapshot");
        let dir = evidence_dir();
        dump_ppm(&format!("{dir}/ingame_capture.ppm"), 256, 384, emu.get_video_buffer(), false);
        eprintln!(
            "CAPTURE {out}: {} bytes at tick {} | 3D swaps={} last_tris={}",
            w.out.len(),
            emu.ticks,
            emu.nds_mmu.gx.engine.swap_count,
            emu.nds_mmu.gx.engine.last_frame_tris,
        );
        assert!(
            emu.nds_mmu.gx.engine.swap_count > 0,
            "no 3D frame was ever swapped: the capture never reached a 3D scene"
        );
    }

    /// Report the in-game scene from the captured snapshot: composite frame, 3D
    /// layer alone, and the per-triangle breakdown of everything the rasterizer
    /// dropped. Loads in well under a second, which is the whole point of the
    /// capture above.
    ///
    /// Set `GX_ZEROPX=1` to also get one line per textured triangle that wrote
    /// no pixels — the missing-character lead.
    #[test]
    #[ignore = "ROM-gated report; needs nds_capture_ingame_snapshot first"]
    fn nds_ingame_scene_report() {
        use crate::snapshot::Reader;
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../roms/Pokemon - SoulSilver Version (USA).nds"
        );
        let (rom, payload) = match (std::fs::read(path), std::fs::read(ingame_snapshot_path())) {
            (Ok(r), Ok(p)) => (r, p),
            (Err(e), _) => {
                eprintln!("SKIP scene report (no ROM): {e}");
                return;
            }
            (_, Err(e)) => {
                eprintln!("SKIP scene report (no capture; run nds_capture_ingame_snapshot): {e}");
                return;
            }
        };
        let mut emu = Emulator::new();
        assert!(emu.load_rom(&rom), "load_rom failed");
        emu.is_playing = true;
        let mut r = Reader::new(&payload);
        emu.snap_nds(&mut r);
        r.finish().expect("captured snapshot is intact");

        // `EMU_SCENE_WALK=n` holds Down for n frames first. The captured state
        // sits on the frame the actor's own sprite has not been uploaded for yet;
        // walking advances into steady-state gameplay, where whatever is still
        // missing is missing for real.
        let walk: u32 = std::env::var("EMU_SCENE_WALK")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        for _ in 0..walk {
            emu.buttons.down = true;
            emu.tick();
        }
        emu.buttons.down = false;

        emu.nds_mmu.vram_writes = 0;
        emu.nds_mmu.oam_writes = 0;
        emu.nds_mmu.arm9_irqs_taken = 0;
        // Cartridge traffic: a scene that streams new graphics reads the card.
        // Immediate-DMA and slot-DMA are counted separately so "asked and never
        // completed" is distinguishable from "never asked".
        emu.nds_mmu.dma_fired = [[0; 8]; 2];
        emu.nds_mmu.dma_armed = [[0; 8]; 2];
        emu.nds_mmu.dma_units = [[0; 8]; 2];
        emu.nds_mmu.gx.engine.tex_stats_on = true;
        emu.nds_mmu.gx.engine.tex_stats.clear();
        // Per-frame command mix. The overworld box-tests each object before
        // drawing it, so BOX_TEST count vs BEGIN_VTXS count says whether the
        // actor pass runs at all: box tests with no geometry behind them means
        // the game considered the object and declined, while zero box tests
        // means nothing was ever registered to consider.
        const REPORT_TICKS: u32 = 10;
        let before = emu.nds_mmu.gx.histo;
        let swaps_before = emu.nds_mmu.gx.engine.swap_count;
        emu.nds_mmu.gx.boxtest_total = 0;
        for _ in 0..REPORT_TICKS {
            emu.tick();
        }
        emu.nds_mmu.gx.engine.tex_stats_on = false;
        // Raw totals, not an average: a rate below one per frame is exactly what
        // needs to be visible here.
        let delta: Vec<(String, u32)> = (0..0x80usize)
            .filter(|&c| emu.nds_mmu.gx.histo[c] > before[c])
            .map(|c| {
                (
                    format!("{}({:#04x})", crate::nds::gx::cmd_name(c as u8), c),
                    emu.nds_mmu.gx.histo[c] - before[c],
                )
            })
            .collect();
        eprintln!(
            "INGAME GXCMD over {REPORT_TICKS} ticks (raw): swaps={} boxtest={} \
             vram_writes={} oam_writes={} arm9_irqs={} {delta:?}",
            emu.nds_mmu.gx.engine.swap_count - swaps_before,
            emu.nds_mmu.gx.boxtest_total,
            emu.nds_mmu.vram_writes,
            emu.nds_mmu.oam_writes,
            emu.nds_mmu.arm9_irqs_taken,
        );
        eprintln!(
            "INGAME DMA over {REPORT_TICKS} ticks: arm9 immediate(armed={} fired={} units={}) \
             cardslot(armed={} fired={} units={}) | gamecard bytes_left={} \
             | gxfifo dma transfers={} words={}",
            emu.nds_mmu.dma_armed[0][0],
            emu.nds_mmu.dma_fired[0][0],
            emu.nds_mmu.dma_units[0][0],
            emu.nds_mmu.dma_armed[0][5],
            emu.nds_mmu.dma_fired[0][5],
            emu.nds_mmu.dma_units[0][5],
            emu.nds_mmu.gamecard.bytes_left,
            emu.nds_mmu.dma_gxfifo_transfers,
            emu.nds_mmu.dma_gxfifo_words,
        );

        // What follows each BOX_TEST? The overworld tests an object's bounding
        // box and then either submits its geometry or skips it. A box test
        // followed by BEGIN_VTXS means "drawn"; one followed by another test or
        // a matrix pop means the game declined — which would put the missing
        // character in the game's own decision, not in the rasterizer.
        emu.nds_mmu.gx.trace_on = true;
        emu.nds_mmu.gx.trace.clear();
        emu.nds_mmu.gx.trace_trigger = Some(0x70); // start capturing at the first BOX_TEST
        // Three whole ticks: the trace is bounded internally, and a rendered
        // frame's box tests are spread across it rather than bunched at the swap.
        for _ in 0..3 {
            emu.tick();
        }
        emu.nds_mmu.gx.trace_on = false;
        let trace = &emu.nds_mmu.gx.trace;
        let mut verdicts: Vec<String> = Vec::new();
        for (i, (cmd, _)) in trace.iter().enumerate() {
            if *cmd != 0x70 {
                continue;
            }
            // Classify by the first "interesting" command after the test.
            let follow = trace[i + 1..]
                .iter()
                .map(|(c, _)| *c)
                .find(|c| matches!(c, 0x40 | 0x70 | 0x12 | 0x50));
            verdicts.push(
                match follow {
                    Some(0x40) => "drew",
                    Some(0x70) => "declined(next test)",
                    Some(0x12) => "declined(mtx pop)",
                    Some(0x50) => "declined(end of frame)",
                    _ => "declined(trace end)",
                }
                .to_string(),
            );
        }
        eprintln!(
            "INGAME BOX_TEST verdicts ({} traced cmds, {} tests): {:?}",
            trace.len(),
            verdicts.len(),
            verdicts
        );
        // Raw structure, run-length compressed: the shape of an object block is
        // what decides whether a BOX_TEST gates the geometry that follows it or
        // merely ends the block before it.
        {
            let mut runs: Vec<String> = Vec::new();
            for (cmd, _) in trace.iter().take(90) {
                let name = crate::nds::gx::cmd_name(*cmd);
                match runs.last_mut() {
                    Some(last) if last.starts_with(name) => {
                        let n: u32 = last
                            .rsplit_once('x')
                            .and_then(|(_, c)| c.parse().ok())
                            .unwrap_or(1);
                        *last = format!("{name}x{}", n + 1);
                    }
                    _ => runs.push(format!("{name}x1")),
                }
            }
            eprintln!("INGAME GX structure: {}", runs.join(" "));
        }

        // Full context for the first few: the box itself (3 packed words = x,y,
        // then w,h,d in 4.12 fixed point) and what the game does next.
        for (i, (cmd, params)) in trace.iter().enumerate().take(trace.len()) {
            if *cmd != 0x70 {
                continue;
            }
            let bbox: Vec<String> = params.iter().map(|w| format!("{w:#010x}")).collect();
            let next: Vec<&str> = trace[i + 1..]
                .iter()
                .take(14)
                .map(|(c, _)| crate::nds::gx::cmd_name(*c))
                .collect();
            eprintln!("  BOXTEST[{i}] bbox={bbox:?} then: {}", next.join(" "));
        }

        let dir = evidence_dir();
        dump_ppm(&format!("{dir}/ingame_composite.ppm"), 256, 384, emu.get_video_buffer(), false);
        dump_ppm(&format!("{dir}/ingame_gx_fb.ppm"), 256, 192, &emu.nds_mmu.gx.engine.fb, true);
        // Every texture this scene sampled, decoded flat. A character sprite
        // present here means its graphics ARE resident and only the geometry is
        // missing; none present means the upload never happened.
        let mut stats = emu.nds_mmu.gx.engine.tex_stats.clone();
        stats.sort_by_key(|e| std::cmp::Reverse(e.2 + e.3));
        for (tex, pal, opaque, clear) in stats.iter().copied().take(24) {
            let (w, h, texels) = crate::nds::gx::decode_texture(&emu.nds_mmu.vram, tex, pal);
            let name = format!("ingame_tex_{:05x}_f{}_p{:x}.ppm", (tex & 0xFFFF) * 8, (tex >> 26) & 7, pal);
            dump_ppm(&format!("{dir}/{name}"), w, h, &texels, true);
            eprintln!("  INGAME TEX {name}: {w}x{h} px_opaque={opaque} px_clear={clear}");
        }
        // 2D layer census. DISPCNT's mode plus each BGCNT decides whether a
        // layer is a text, affine, extended-affine or bitmap background; only
        // text BGs are implemented, so a layer asking for anything else renders
        // as backdrop and its content is simply absent.
        for (io, name) in [(0usize, "A"), (0x1000, "B")] {
            let rd16 = |o: usize| {
                u16::from_le_bytes([emu.nds_mmu.arm9_io[o], emu.nds_mmu.arm9_io[o + 1]])
            };
            let dispcnt = u32::from_le_bytes([
                emu.nds_mmu.arm9_io[io],
                emu.nds_mmu.arm9_io[io + 1],
                emu.nds_mmu.arm9_io[io + 2],
                emu.nds_mmu.arm9_io[io + 3],
            ]);
            let mode = dispcnt & 7;
            let bgs: Vec<String> = (0..4)
                .map(|b| {
                    let cnt = rd16(io + 0x08 + b * 2);
                    // GBATEK's BG type table, indexed by [mode][bg].
                    // GBATEK's BG-type table, by (mode, layer). Only the
                    // non-text entries need naming; everything else is text.
                    let kind = match (mode, b) {
                        (_, 0) if dispcnt & 8 != 0 => "3D",
                        (1 | 2, 3) | (3, 2) | (4, 3) => "affine",
                        (3, 3) | (4, 2) | (5, 2 | 3) => "ext",
                        (6, _) => "large-bitmap",
                        _ => "text",
                    };
                    format!(
                        "BG{b}[{kind} cnt={cnt:04x} prio={} char={:#x} screen={:#x} on={}]",
                        cnt & 3,
                        ((cnt >> 2) & 0xF) as u32 * 0x4000,
                        ((cnt >> 8) & 0x1F) as u32 * 0x800,
                        (dispcnt >> (8 + b)) & 1,
                    )
                })
                .collect();
            eprintln!("INGAME 2D {name}: mode={mode} dispcnt={dispcnt:#010x} {}", bgs.join(" "));
        }

        // Engine-A OBJ census. NitroSDK parks an unused sprite at y=192 (just
        // below the screen), so "how many entries are on-screen" separates "the
        // game is not using 2D sprites here" from "it is, and we drop them".
        for (oam_base, name) in [(0usize, "A"), (0x400, "B")] {
            let mut on_screen = Vec::new();
            // OBJ mode census (attr0 bits 10-11): 0 normal, 1 semi-transparent,
            // 2 OBJ window, 3 bitmap. Modes 1 and 2 are unimplemented, so a
            // nonzero count here is a real visual gap; zero means those debts
            // cost this scene nothing.
            let mut mode_counts = [0u32; 4];
            let mut affine = 0u32;
            for i in 0..128usize {
                let at = oam_base + i * 8;
                let a0 = u16::from_le_bytes([emu.nds_mmu.oam[at], emu.nds_mmu.oam[at + 1]]);
                let a1 = u16::from_le_bytes([emu.nds_mmu.oam[at + 2], emu.nds_mmu.oam[at + 3]]);
                let a2 = u16::from_le_bytes([emu.nds_mmu.oam[at + 4], emu.nds_mmu.oam[at + 5]]);
                let (y, x) = (a0 & 0xFF, a1 & 0x1FF);
                let disabled = (a0 >> 8) & 3 == 2; // rot/scale off + double-size bit = hidden
                if y < 192 && !disabled {
                    mode_counts[((a0 >> 10) & 3) as usize] += 1;
                    if (a0 >> 8) & 1 != 0 {
                        affine += 1;
                    }
                }
                if y < 192 && !disabled {
                    on_screen.push(format!(
                        "[{i}] y={y} x={x} shape={} size={} tile={:#05x} prio={}",
                        a0 >> 14,
                        (a1 >> 14) & 3,
                        a2 & 0x3FF,
                        (a2 >> 10) & 3
                    ));
                }
            }
            eprintln!(
                "INGAME OAM {name}: {} on-screen modes(norm/semi/window/bmp)={mode_counts:?} \
                 affine={affine}{}",
                on_screen.len(),
                if on_screen.is_empty() {
                    String::new()
                } else {
                    format!(" | {}", on_screen[..on_screen.len().min(8)].join(" "))
                }
            );
        }

        let gx = &emu.nds_mmu.gx.engine;
        if gx.tex_focus.is_some() {
            eprintln!(
                "INGAME TEX FOCUS {:#x}: submitted={} culled={} pushed={} dropped_cap={} \
                 | zero 4x3 matrix loads={} | matrix killed by cmd={:#04x} words={:08x?} mode={}",
                gx.tex_focus.unwrap_or(0),
                gx.focus_submitted,
                gx.focus_culled,
                gx.focus_pushed,
                gx.focus_dropped_cap,
                gx.zero_mtx_loads,
                gx.zeroing_cmd,
                gx.zeroing_words,
                gx.zeroing_mode,
            );
        }
        eprintln!(
            "INGAME tick={} tris={} near_rejected={} tex_degenerate={} tex_zero_px={} \
             culled={} opaque_px={} fmt_tris={:?}",
            emu.ticks,
            gx.last_frame_tris,
            gx.last_near_rejected,
            gx.last_tex_degenerate,
            gx.last_tex_zero_px,
            gx.last_culled,
            gx.fb.iter().filter(|&&p| p & 0x8000 != 0).count(),
            gx.fmt_tris,
        );
    }

    /// Walk test: does the missing character *exist*?
    ///
    /// Holding a direction from the captured overworld state separates two very
    /// different bugs. If the map scrolls and the geometry changes, the game's
    /// actor logic is running and only the character's own drawing is lost. If
    /// nothing changes at all, there is no player object to begin with and the
    /// fault is upstream of the renderer.
    ///
    /// Dumps the first, middle and last frame so the motion can be seen, and
    /// reports per-frame triangle counts and frame hashes.
    #[test]
    #[ignore = "ROM-gated report; needs nds_capture_ingame_snapshot first"]
    fn nds_ingame_walk_test() {
        use crate::snapshot::{content_hash, Reader};
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../roms/Pokemon - SoulSilver Version (USA).nds"
        );
        let (rom, payload) = match (std::fs::read(path), std::fs::read(ingame_snapshot_path())) {
            (Ok(r), Ok(p)) => (r, p),
            (Err(e), _) => {
                eprintln!("SKIP walk test (no ROM): {e}");
                return;
            }
            (_, Err(e)) => {
                eprintln!("SKIP walk test (no capture): {e}");
                return;
            }
        };
        let mut emu = Emulator::new();
        assert!(emu.load_rom(&rom), "load_rom failed");
        emu.is_playing = true;
        let mut r = Reader::new(&payload);
        emu.snap_nds(&mut r);
        r.finish().expect("captured snapshot is intact");

        const FRAMES: u32 = 96;
        let dir = evidence_dir();
        let mut hashes: Vec<u64> = Vec::new();
        let mut tris: Vec<usize> = Vec::new();
        // Per-bank VRAM churn. A walking character's animation cell is streamed
        // into texture VRAM every few frames, so a texture bank that never
        // changes says the upload never happens — a different bug from geometry
        // that is uploaded but never drawn.
        let bank_hash = |emu: &Emulator| -> Vec<u64> {
            emu.nds_mmu
                .vram
                .banks
                .iter()
                .map(|b| content_hash(&b.data))
                .collect()
        };
        let mut prev_banks = bank_hash(&emu);
        let mut bank_changes = [0u32; 9];
        let mut oam_changes = 0u32;
        let mut prev_oam = content_hash(&emu.nds_mmu.oam);
        emu.nds_mmu.vram_writes = 0;
        emu.nds_mmu.oam_writes = 0;
        emu.nds_mmu.arm9_irqs_taken = 0;
        emu.nds_mmu.arm7_irqs_taken = 0;
        for f in 0..FRAMES {
            // Hold Down. Everything else released, so nothing else can advance
            // a dialog and confound the result.
            emu.buttons.down = true;
            emu.buttons.up = false;
            emu.buttons.left = false;
            emu.buttons.right = false;
            emu.buttons.a = false;
            emu.buttons.nds_touch_pressed = false;
            emu.tick();
            let frame: Vec<u8> =
                emu.get_video_buffer().iter().flat_map(|p| p.to_le_bytes()).collect();
            hashes.push(content_hash(&frame));
            tris.push(emu.nds_mmu.gx.engine.last_frame_tris);
            let banks = bank_hash(&emu);
            for (i, (now, was)) in banks.iter().zip(prev_banks.iter()).enumerate() {
                if now != was {
                    bank_changes[i] += 1;
                }
            }
            prev_banks = banks;
            let oam_now = content_hash(&emu.nds_mmu.oam);
            if oam_now != prev_oam {
                oam_changes += 1;
            }
            prev_oam = oam_now;
            if matches!(f, 0 | 48 | 95) {
                dump_ppm(
                    &format!("{dir}/walk_f{f}.ppm"),
                    256,
                    384,
                    emu.get_video_buffer(),
                    false,
                );
            }
        }
        // Does the character's sprite actually animate? Its cell is a texture in
        // VRAM, so a walk cycle must either swap the TEXIMAGE address or rewrite
        // the texels. Hash the decoded texture each frame and count how many
        // distinct images appeared: one means the actor is drawn but frozen.
        // A texture bank is not CPU-writable while it is mapped to a texture
        // slot, so uploading a new animation cell means remapping it through
        // VRAMCNT and back. Counting VRAMCNT changes separates "the game never
        // asked to upload" from "it uploaded and we lost the data".
        let mut vramcnt_changes = 0u32;
        let mut prev_vramcnt: Vec<u8> = emu.nds_mmu.arm9_io[0x240..0x24A].to_vec();

        // Animation could also swap the TEXIMAGE address between cells that are
        // already resident, so census every texture selection across the walk.
        emu.nds_mmu.gx_tex_watch_all = true;
        emu.nds_mmu.gx_tex_watch_pcs.clear();

        // Hashing ONE fixed texture address would report "frozen" the instant the
        // actor switches to a different cell — the opposite of the truth. The
        // honest measure is the set of distinct bases the census records below.
        for _ in 0..48u32 {
            let mut b = emu.get_button_state();
            b.down = true;
            emu.inject_input(b);
            emu.tick();
            let now = emu.nds_mmu.arm9_io[0x240..0x24A].to_vec();
            if now != prev_vramcnt {
                vramcnt_changes += 1;
                prev_vramcnt = now;
            }
        }
        emu.nds_mmu.gx_tex_watch_all = false;
        let mut tex_bases: Vec<u32> = emu
            .nds_mmu
            .gx_tex_watch_pcs
            .iter()
            .map(|(_, _, tex)| (tex & 0xFFFF) * 8)
            .collect();
        tex_bases.sort_unstable();
        tex_bases.dedup();

        let distinct = hashes.iter().collect::<std::collections::HashSet<_>>().len();
        let (tmin, tmax) = (
            tris.iter().min().copied().unwrap_or(0),
            tris.iter().max().copied().unwrap_or(0),
        );
        eprintln!(
            "WALK: {FRAMES} frames, {distinct} distinct frames, tris {tmin}..{tmax}, \
             tri series head={:?}\n\
             WALK ANIM: vramcnt remaps={vramcnt_changes} texture bases selected={:x?}\n\
             WALK CHURN: vram bank A-I frames-changed={:?} oam frames-changed={oam_changes} \
             | write attempts: vram={} oam={} | irqs taken: arm9={} arm7={}",
            &tris[..tris.len().min(12)],
            tex_bases,
            bank_changes,
            emu.nds_mmu.vram_writes,
            emu.nds_mmu.oam_writes,
            emu.nds_mmu.arm9_irqs_taken,
            emu.nds_mmu.arm7_irqs_taken,
        );
        assert!(
            distinct > 1,
            "holding Down changed nothing in {FRAMES} frames: the overworld is not \
             accepting input, so the missing character is not a rendering bug"
        );
    }

    /// Name the game function that draws the player's shadow.
    ///
    /// The shadow is the one piece of the player's actor that DOES render, and it
    /// carries a distinctive texture (format 2 at VRAM offset 0x075a0, palette
    /// 0x110). Arming `gx_tex_watch` on it records the ARM9 PC of every write
    /// that selects it, which is the entry point to disassemble: the body quad
    /// that never appears is submitted — or skipped — a few instructions away.
    ///
    /// Also dumps the words around the top PC so the branch can be read without
    /// re-running the emulator.
    #[test]
    #[ignore = "ROM-gated report; needs nds_capture_ingame_snapshot first"]
    fn nds_ingame_shadow_writer_report() {
        use crate::snapshot::Reader;
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../roms/Pokemon - SoulSilver Version (USA).nds"
        );
        let (rom, payload) = match (std::fs::read(path), std::fs::read(ingame_snapshot_path())) {
            (Ok(r), Ok(p)) => (r, p),
            (Err(e), _) => {
                eprintln!("SKIP shadow-writer report (no ROM): {e}");
                return;
            }
            (_, Err(e)) => {
                eprintln!("SKIP shadow-writer report (no capture): {e}");
                return;
            }
        };
        let mut emu = Emulator::new();
        assert!(emu.load_rom(&rom), "load_rom failed");
        emu.is_playing = true;
        let mut r = Reader::new(&payload);
        emu.snap_nds(&mut r);
        r.finish().expect("captured snapshot is intact");

        // TEXIMAGE_PARAM holds the texture base as offset/8 in its low 16 bits.
        // `EMU_TEX_CENSUS=1` widens the watch to every texture selection, which
        // lists the scene's draw call sites instead of one object's.
        const SHADOW_TEX_BASE: u32 = 0x075A0 / 8;
        if std::env::var("EMU_TEX_CENSUS").is_ok() {
            emu.nds_mmu.gx_tex_watch_all = true;
        } else {
            emu.nds_mmu.gx_tex_watch = Some(SHADOW_TEX_BASE);
        }
        emu.nds_mmu.gx_tex_watch_pcs.clear();
        for f in 0..40u32 {
            let mut b = emu.get_button_state();
            b.down = true;
            emu.inject_input(b);
            emu.tick();
            if !emu.nds_mmu.gx_tex_watch_pcs.is_empty() {
                eprintln!("SHADOW WRITER: first hit on frame {f}");
                break;
            }
        }
        emu.nds_mmu.gx_tex_watch = None;
        emu.nds_mmu.gx_tex_watch_all = false;

        // `EMU_RAM_WATCH=<hex>` traps writes to a 16-byte window and names the
        // code that made them. Used to find who writes the zero scale into the
        // actor's display list.
        if let Some(base) = std::env::var("EMU_RAM_WATCH")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        {
            emu.nds_mmu.ram_write_watch = Some((base, base + 16));
            emu.nds_mmu.ram_write_pcs.clear();
            for _ in 0..4 {
                let mut b = emu.get_button_state();
                b.down = true;
                emu.inject_input(b);
                emu.tick();
            }
            emu.nds_mmu.ram_write_watch = None;
            for (addr, val, pc, lr) in emu.nds_mmu.ram_write_pcs.clone() {
                eprintln!("  RAMWRITE [{addr:#010x}] = {val:#04x} by pc={pc:#010x} lr={lr:#010x}");
            }
            if emu.nds_mmu.ram_write_pcs.is_empty() {
                eprintln!("  RAMWRITE: nothing wrote {base:#010x}..+16 in 4 frames");
            }
            // The builder around those stores: this is the code that decides the
            // value, so dump enough of it to read the data flow.
            if let Some((_, _, pc, _)) = emu.nds_mmu.ram_write_pcs.first().copied() {
                for row in 0..10u32 {
                    let addr = pc.saturating_sub(0x50) + row * 16;
                    let words: Vec<String> = (0..4)
                        .map(|w| format!("{:08x}", emu.nds_mmu.read_word_arm9(addr + w * 4)))
                        .collect();
                    eprintln!("  BUILDER {addr:#010x}: {}", words.join(" "));
                }
            }
        }

        // The actor's scale is `VEC_Mag` of each row of a matrix, computed with
        // the hardware square root. Sampling the unit's registers after a tick
        // splits "the game asked for sqrt(0)" (its matrix is already zero, so
        // the fault is upstream) from "it asked for a real value and we answered
        // zero" (the fault is ours).
        for probe in 0..3u32 {
            let mut b = emu.get_button_state();
            b.down = true;
            emu.inject_input(b);
            emu.tick();
            let lo = u64::from(emu.nds_mmu.read_word_arm9(0x0400_02B8));
            let hi = u64::from(emu.nds_mmu.read_word_arm9(0x0400_02BC));
            eprintln!(
                "  SQRT probe {probe}: cnt={:#06x} param={:#018x} result={:#010x}",
                emu.nds_mmu.read_halfword_arm9(0x0400_02B0),
                (hi << 32) | lo,
                emu.nds_mmu.read_word_arm9(0x0400_02B4),
            );
        }

        // `EMU_CODE_DUMP=<hex>` prints ARM9 words at an address — for reading a
        // callee whose address was derived from a BL offset.
        if let Some(at) = std::env::var("EMU_CODE_DUMP")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        {
            for row in 0..14u32 {
                let addr = at + row * 16;
                let words: Vec<String> = (0..4)
                    .map(|w| format!("{:08x}", emu.nds_mmu.read_word_arm9(addr + w * 4)))
                    .collect();
                eprintln!("  DUMP {addr:#010x}: {}", words.join(" "));
            }
        }

        // Who committed the zero MTX_SCALE that collapses an actor's transform?
        for (pc, lr) in emu.nds_mmu.gx_zero_scale_pcs.clone() {
            eprintln!("  ZEROSCALE writer pc={pc:#010x} lr={lr:#010x}");
            for row in 0..6u32 {
                let addr = lr.saturating_sub(0x28) + row * 16;
                let words: Vec<String> = (0..4)
                    .map(|w| format!("{:08x}", emu.nds_mmu.read_word_arm9(addr + w * 4)))
                    .collect();
                eprintln!("    CALLER {addr:#010x}: {}", words.join(" "));
            }
            // The caller hands the blitter a pointer to a display list. Follow
            // the literal pool after the function and dump what those lists
            // actually contain: a list full of zeros means whoever *builds* it
            // never ran, which is a different bug from a list that deliberately
            // scales an object away.
            for row in 0..8u32 {
                let addr = lr + 0x40 + row * 16;
                for w in 0..4u32 {
                    let lit = emu.nds_mmu.read_word_arm9(addr + w * 4);
                    if !(0x0200_0000..0x0240_0000).contains(&lit) {
                        continue;
                    }
                    let dl: Vec<String> = (0..20)
                        .map(|i| format!("{:08x}", emu.nds_mmu.read_word_arm9(lit + i * 4)))
                        .collect();
                    eprintln!("    DL@{lit:#010x} (literal {:#010x}): {}", addr + w * 4, dl.join(" "));
                }
            }
        }

        let hits = emu.nds_mmu.gx_tex_watch_pcs.clone();
        for (pc, lr, tex) in &hits {
            eprintln!(
                "  DRAWSITE lr={lr:#010x} pc={pc:#010x} tex={tex:#010x} \
                 (fmt={} addr={:#07x} size={}x{})",
                (tex >> 26) & 7,
                (tex & 0xFFFF) * 8,
                8 << ((tex >> 20) & 7),
                8 << ((tex >> 23) & 7),
            );
        }
        // Rank by the RETURN address: the write itself sits in a shared
        // display-list blitter, so `lr` is what identifies the game code.
        let mut by_lr: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for (_, lr, _) in &hits {
            *by_lr.entry(*lr).or_default() += 1;
        }
        let mut ranked: Vec<(u32, u32)> = by_lr.into_iter().collect();
        ranked.sort_by_key(|(lr, n)| (std::cmp::Reverse(*n), *lr));
        eprintln!(
            "SHADOW WRITER: {} hits, tex={:#010x}, writer_pcs={:?}, callers(lr)={}",
            hits.len(),
            hits.first().map(|(_, _, t)| *t).unwrap_or(0),
            hits.iter().map(|(pc, _, _)| format!("{pc:#010x}")).take(3).collect::<Vec<_>>(),
            ranked
                .iter()
                .take(8)
                .map(|(lr, n)| format!("{lr:#010x}x{n}"))
                .collect::<Vec<_>>()
                .join(" "),
        );

        // Words around each distinct caller, for disassembly. The busiest one is
        // the shared blitter's own return site; the interesting callers are the
        // rarer ones, which are the game functions that send a display list.
        for (lr, n) in ranked.iter().take(4) {
            let base = lr.saturating_sub(0x30);
            eprintln!("  --- caller {lr:#010x} (x{n})");
            for row in 0..8u32 {
                let addr = base + row * 16;
                let words: Vec<String> = (0..4)
                    .map(|w| format!("{:08x}", emu.nds_mmu.read_word_arm9(addr + w * 4)))
                    .collect();
                eprintln!("  CODE {addr:#010x}: {}", words.join(" "));
            }
            // Literal pool + live globals. These senders gate on a global that
            // must read 0xFFFFFFFF ("nothing queued"); if it does not, the
            // direct-send path is skipped, which is exactly how geometry can go
            // missing without the engine ever seeing it.
            for row in 0..6u32 {
                let addr = base + 0x50 + row * 16;
                for w in 0..4u32 {
                    let word = emu.nds_mmu.read_word_arm9(addr + w * 4);
                    if (0x0200_0000..0x0240_0000).contains(&word) {
                        eprintln!(
                            "  GLOBAL candidate {word:#010x} (literal at {:#010x}) = {:#010x}",
                            addr + w * 4,
                            emu.nds_mmu.read_word_arm9(word),
                        );
                    }
                }
            }
        }
    }

    /// When, if ever, does the overworld upload graphics?
    ///
    /// The per-frame reports sample ten ticks, which cannot tell "uploads never
    /// happen" from "uploads happen on demand and this window missed them". This
    /// walks for a long stretch and buckets VRAM/OAM write attempts plus per-bank
    /// content changes, so an on-demand upload (entering a door, a new animation
    /// cell, a menu opening) would show up as a spike in some bucket.
    #[test]
    #[ignore = "ROM-gated report; needs nds_capture_ingame_snapshot first"]
    fn nds_ingame_upload_timeline() {
        use crate::snapshot::{content_hash, Reader};
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../roms/Pokemon - SoulSilver Version (USA).nds"
        );
        let (rom, payload) = match (std::fs::read(path), std::fs::read(ingame_snapshot_path())) {
            (Ok(r), Ok(p)) => (r, p),
            (Err(e), _) => {
                eprintln!("SKIP upload timeline (no ROM): {e}");
                return;
            }
            (_, Err(e)) => {
                eprintln!("SKIP upload timeline (no capture): {e}");
                return;
            }
        };
        let mut emu = Emulator::new();
        assert!(emu.load_rom(&rom), "load_rom failed");
        emu.is_playing = true;
        let mut r = Reader::new(&payload);
        emu.snap_nds(&mut r);
        r.finish().expect("captured snapshot is intact");

        const BUCKET: u32 = 200;
        const BUCKETS: u32 = 12;
        let mut vram_row: Vec<u64> = Vec::new();
        let mut oam_row: Vec<u64> = Vec::new();
        let mut bank_changes = [0u32; 9];
        let mut prev: Vec<u64> = emu
            .nds_mmu
            .vram
            .banks
            .iter()
            .map(|b| content_hash(&b.data))
            .collect();

        for bucket in 0..BUCKETS {
            emu.nds_mmu.vram_writes = 0;
            emu.nds_mmu.oam_writes = 0;
            emu.nds_mmu.vram.dropped_writes = 0;
            emu.nds_mmu.vram.dropped_by_target = [0; 8];
            for f in 0..BUCKET {
                let mut b = emu.get_button_state();
                // Walk for most of the bucket, then press A: either can trigger
                // an on-demand load (a doorway, a sign, a dialog box).
                b.down = f < BUCKET - 40;
                b.a = f >= BUCKET - 40 && (f % 20) < 6;
                emu.inject_input(b);
                emu.tick();
                let now: Vec<u64> = emu
                    .nds_mmu
                    .vram
                    .banks
                    .iter()
                    .map(|bank| content_hash(&bank.data))
                    .collect();
                for (i, (n, p)) in now.iter().zip(prev.iter()).enumerate() {
                    if n != p {
                        bank_changes[i] += 1;
                    }
                }
                prev = now;
            }
            vram_row.push(emu.nds_mmu.vram_writes);
            oam_row.push(emu.nds_mmu.oam_writes);
            eprintln!(
                "  UPLOAD bucket {bucket} (ticks {}..{}): vram={} oam={} dropped={} by_target={:?}",
                bucket * BUCKET,
                (bucket + 1) * BUCKET,
                emu.nds_mmu.vram_writes,
                emu.nds_mmu.oam_writes,
                emu.nds_mmu.vram.dropped_writes,
                emu.nds_mmu.vram.dropped_by_target,
            );
        }
        eprintln!(
            "UPLOAD TIMELINE over {} ticks: vram per bucket={vram_row:?} oam per bucket={oam_row:?} \
             bank frames-changed={bank_changes:?}",
            BUCKET * BUCKETS
        );
        dump_ppm(
            &format!("{}/upload_timeline_end.ppm", evidence_dir()),
            256,
            384,
            emu.get_video_buffer(),
            false,
        );
    }

    /// Does the *game* ever write the cartridge save chip?
    ///
    /// The chip and its `.sav` round-trip are unit-tested, but that only proves
    /// the emulated device works. This drives the overworld snapshot with the
    /// menu-and-confirm mash a player performs to save (X opens the menu, then A
    /// walks the SAVE prompts) and reports the chip's command census. Boot alone
    /// shows READ and RDSR; a real save must add WREN plus a program (PP/PW).
    ///
    /// Reported, not asserted: the input schedule is a blind mash, so a zero here
    /// means "this schedule did not reach the save prompt", not necessarily a bug.
    #[test]
    #[ignore = "ROM-gated report; needs nds_capture_ingame_snapshot first"]
    fn nds_ingame_save_menu_report() {
        use crate::snapshot::Reader;
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../roms/Pokemon - SoulSilver Version (USA).nds"
        );
        let (rom, payload) = match (std::fs::read(path), std::fs::read(ingame_snapshot_path())) {
            (Ok(r), Ok(p)) => (r, p),
            (Err(e), _) => {
                eprintln!("SKIP save-menu report (no ROM): {e}");
                return;
            }
            (_, Err(e)) => {
                eprintln!("SKIP save-menu report (no capture): {e}");
                return;
            }
        };
        let mut emu = Emulator::new();
        assert!(emu.load_rom(&rom), "load_rom failed");
        emu.is_playing = true;
        let mut r = Reader::new(&payload);
        emu.snap_nds(&mut r);
        r.finish().expect("captured snapshot is intact");
        emu.nds_mmu.backup.cmd_counts = [0; 8];

        // X for a few frames to open the menu, then A every 40 frames to walk
        // "SAVE -> yes -> yes" and dismiss the report screens.
        const FRAMES: u32 = 1200;
        for f in 0..FRAMES {
            let mut b = emu.get_button_state();
            b.x = (20..28).contains(&f);
            b.a = f >= 60 && (f % 40) < 8;
            b.nds_touch_pressed = false;
            emu.inject_input(b);
            emu.tick();
        }

        let census: Vec<String> = crate::nds::backup::CMD_LABELS
            .iter()
            .zip(emu.nds_mmu.backup.cmd_counts.iter())
            .filter(|(_, n)| **n > 0)
            .map(|(name, n)| format!("{name}={n}"))
            .collect();
        eprintln!(
            "SAVE MENU after {FRAMES} frames: chip commands [{}] dirty={}",
            census.join(" "),
            emu.nds_mmu.backup.is_dirty(),
        );
        dump_ppm(
            &format!("{}/save_menu.ppm", evidence_dir()),
            256,
            384,
            emu.get_video_buffer(),
            false,
        );
    }

    /// In-game audio and throughput, measured from the captured snapshot.
    ///
    /// Tests the two mechanisms that can make gameplay audio sound "repetitive
    /// and lagging" without the core's own samples being wrong:
    ///
    /// 1. **Throughput.** The frontend paces on the audio queue, so a core below
    ///    60 emulated fps in-game starves the device and the player hears gaps.
    ///    The title screen measured 151 fps; the overworld is the load that
    ///    matters and had never been measured.
    /// 2. **Block repetition.** Identical consecutive sample blocks are the
    ///    signature of a buffer being re-emitted (the "robotic loop"), as
    ///    opposed to music that merely repeats musically.
    #[test]
    #[ignore = "ROM-gated report; needs nds_capture_ingame_snapshot first"]
    fn nds_ingame_audio_and_perf_report() {
        use std::time::Instant;
        let mut emu = Emulator::new();
        if let Err(e) = load_probe_scene(&mut emu) {
            eprintln!("SKIP in-game audio report: {e}");
            return;
        }
        emu.is_playing = true;
        emu.set_audio_sample_rate(48_000); // the host device's real rate

        const TICKS: u32 = 240; // ~4 s of gameplay
        emu.nds_mmu.apu.key_ons = [0; 16];
        // CPU saturation. The run loop budgets each core a fixed number of bus
        // cycles per frame; a core that never halts has not finished its frame
        // work when that budget runs out, so everything it defers (the ARM7's
        // sound driver refilling streamed channels, the ARM9's VRAM uploads)
        // slips a frame. That is the shape both reported symptoms have.
        emu.nds_mmu.arm9_halt_cycles = 0;
        emu.nds_mmu.arm7_halt_cycles = 0;
        emu.nds_mmu.arm7_cycles_run = 0;
        let cpu_cycles_0 = emu.cpu_cycles;
        // Where the wall clock goes. The two renderers are sampled at their own
        // entry points; everything else (CPU interpretation, timers, APU, DMA)
        // is the remainder, which is the only honest way to attribute it
        // without putting a clock read inside the run loop's inner slice.
        emu.nds_mmu.gx.engine.prof_raster_ns = 0;
        emu.nds_ppu.prof_render_ns = 0;
        emu.nds_mmu.prof_cpu_ns = 0;
        // Opt-in, because the counters are not free: leaving them on would make
        // this report's own `fps` figure describe the profiled build rather than
        // the one players run. `EMU_PROF_CPU=1` trades that for the breakdown.
        // Gates BOTH `prof_cpu_ns` (once per run-loop slice) and
        // `NdsPpu::prof_render_ns` (once per visible scanline); without it those
        // two columns read 0.00 and only `3d_raster` (once per swap) is live.
        emu.nds_mmu.prof_cpu_on = std::env::var("EMU_PROF_CPU").is_ok();
        let mut pcm: Vec<i16> = Vec::new();
        let mut per_tick: Vec<usize> = Vec::new();
        let mut active_hist = [0u32; 17];
        let mut tick_ms: Vec<f64> = Vec::with_capacity(TICKS as usize);
        let t0 = Instant::now();
        for _ in 0..TICKS {
            let t = Instant::now();
            emu.tick();
            tick_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            let block = emu.get_audio_buffer();
            per_tick.push(block.len() / 2);
            pcm.extend_from_slice(block);
            let live = emu.nds_mmu.apu.channels.iter().filter(|c| c.active).count();
            active_hist[live] += 1;
        }
        let secs = t0.elapsed().as_secs_f64();

        // Exact duplicate blocks: a re-queued buffer repeats bit for bit, which
        // real synthesis essentially never does outside silence.
        const BLK: usize = 1024;
        let blocks: Vec<&[i16]> = pcm.chunks_exact(BLK).collect();
        let mut dup = 0usize;
        let mut dup_nonsilent = 0usize;
        for w in blocks.windows(2) {
            if w[0] == w[1] {
                dup += 1;
                if w[0].iter().any(|&s| s != 0) {
                    dup_nonsilent += 1;
                }
            }
        }
        let rms = (pcm.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>()
            / pcm.len().max(1) as f64)
            .sqrt();
        let peak = pcm.iter().map(|s| s.abs()).max().unwrap_or(0);
        let want = emu.audio_frames_per_tick();
        let short = per_tick.iter().filter(|&&n| n + 8 < want).count();

        // Throughput TAIL, not the mean. The device drains in real time, so a
        // tick that takes longer than one frame's worth of wall clock consumes
        // more audio than it produced and the queue drops by the excess. The
        // mean can sit at 1.7x while a single 60 ms tick still empties a 32 ms
        // cushion — which is what "lagging" sounds like. `worst_drain_ms` is
        // that excess for the slowest tick; compare it against the frontend's
        // measured queue floor (~32 ms at `samples=512`).
        let budget_ms = 1000.0 / 59.8261;
        let mut sorted = tick_ms.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).expect("tick times are finite"));
        let pct = |p: f64| sorted[(((sorted.len() - 1) as f64) * p) as usize];
        let over_budget = tick_ms.iter().filter(|&&m| m > budget_ms).count();
        let worst_drain = sorted[sorted.len() - 1] - budget_ms;
        let per_frame_ms =
            |ns: u64| ns as f64 / 1e6 / f64::from(TICKS);
        let raster_ms = per_frame_ms(emu.nds_mmu.gx.engine.prof_raster_ns);
        let scanline_ms = per_frame_ms(emu.nds_ppu.prof_render_ns);
        // The 3D rasterizer runs inside a CPU store (the SWAP_BUFFERS command
        // write), so its time is already inside `prof_cpu_ns` — subtract it or
        // it is counted twice and the remainder goes negative.
        let cpu_ms = per_frame_ms(emu.nds_mmu.prof_cpu_ns) - raster_ms;

        eprintln!(
            "INGAME KEYONS per channel over {TICKS} ticks: {:?}\n\
             INGAME CLICKS: {} sample steps over 8192 of full scale (a retriggering \
             channel or a torn buffer shows up here, not in RMS)\n\
             INGAME PERF: {TICKS} ticks in {secs:.2}s = {:.1} fps ({:.2}x realtime)\n\
             INGAME TICK TAIL: budget={budget_ms:.2}ms med={:.2} p95={:.2} p99={:.2} max={:.2} \
             over_budget={over_budget}/{TICKS} worst_drain={worst_drain:.2}ms \
             (a drain past the frontend's ~32ms queue floor is an underrun)\n\
             INGAME AUDIO: samples={} rms={rms:.1} peak={peak} \
             frames/tick want={want} short_ticks={short} min={} max={}\n\
             INGAME AUDIO dup_blocks={dup}/{} (non-silent {dup_nonsilent}) \
             live_channels_hist={:?}\n\
             INGAME CPU: arm9 {} cyc/frame halted {:.1}% | arm7 {} cyc/frame halted {:.1}% \
             (0% halted = saturated: the core never finished its frame work)\n\
             INGAME PROFILE per frame: 3d_raster={:.2}ms 2d_scanlines={:.2}ms cpu={:.2}ms \
             rest(timers+apu+loop)={:.2}ms of {:.2}ms measured",
            emu.nds_mmu.apu.key_ons,
            pcm.windows(2).filter(|w| (i32::from(w[1]) - i32::from(w[0])).abs() > 8192).count(),
            f64::from(TICKS) / secs,
            f64::from(TICKS) / secs / 59.8261,
            pct(0.50),
            pct(0.95),
            pct(0.99),
            sorted[sorted.len() - 1],
            pcm.len(),
            per_tick.iter().min().copied().unwrap_or(0),
            per_tick.iter().max().copied().unwrap_or(0),
            blocks.len().saturating_sub(1),
            active_hist,
            (emu.cpu_cycles - cpu_cycles_0) / u64::from(TICKS),
            100.0 * emu.nds_mmu.arm9_halt_cycles as f64
                / (emu.cpu_cycles - cpu_cycles_0).max(1) as f64,
            emu.nds_mmu.arm7_cycles_run / u64::from(TICKS),
            100.0 * emu.nds_mmu.arm7_halt_cycles as f64
                / emu.nds_mmu.arm7_cycles_run.max(1) as f64,
            raster_ms,
            scanline_ms,
            cpu_ms,
            tick_ms.iter().sum::<f64>() / f64::from(TICKS) - raster_ms - scanline_ms - cpu_ms,
            tick_ms.iter().sum::<f64>() / f64::from(TICKS),
        );
        assert!(!pcm.is_empty(), "no audio at all in the overworld");
    }

    /// What the channels are actually PLAYING, as opposed to whether the sample
    /// stream is intact.
    ///
    /// Every audio measurement so far has tested stream integrity — duplicate
    /// blocks, clicks, RMS, throughput, modulation spectrum — and all of them
    /// come back clean. None of them can see a *content* defect: a sequencer
    /// that starts the same note over and over, or a driver whose per-note
    /// volume envelope never moves, produces a perfectly continuous,
    /// non-duplicated waveform that still sounds like "the first sound repeated
    /// through a sequence".
    ///
    /// This samples the register file once per tick (no core changes, no new hot
    /// -path state) and reports, per channel, how much the three fields that
    /// define a note actually vary:
    ///
    /// * `sad` — the sample source. One value per channel across a whole song is
    ///   normal (a channel holds one instrument); ONE value across *every*
    ///   channel is the "same sound every time" defect.
    /// * `tmr` — the pitch. A melodic channel must take several distinct values.
    ///   A single value while the channel retriggers is a dead sequencer.
    /// * `vol` — the software ADSR the NitroSDK ARM7 driver writes every driver
    ///   tick. A channel whose volume never moves has no envelope, which is what
    ///   turns decaying notes into a drone.
    #[test]
    #[ignore = "ROM-gated report; needs nds_capture_ingame_snapshot first"]
    fn nds_ingame_channel_content_report() {
        let mut emu = Emulator::new();
        if let Err(e) = load_probe_scene(&mut emu) {
            eprintln!("SKIP in-game channel content report: {e}");
            return;
        }
        emu.is_playing = true;
        emu.set_audio_sample_rate(48_000);

        const TICKS: u32 = 240; // ~4 s, the same window as the audio report
        // Per channel: the distinct values each field took while the channel was
        // playing. Sets, not counts — the question is "does it vary at all".
        let mut sads: [std::collections::BTreeSet<u32>; 16] = Default::default();
        let mut tmrs: [std::collections::BTreeSet<u16>; 16] = Default::default();
        let mut vols: [std::collections::BTreeSet<u8>; 16] = Default::default();
        let mut active_ticks = [0u32; 16];
        emu.nds_mmu.apu.key_ons = [0; 16];

        for _ in 0..TICKS {
            emu.tick();
            for (i, ch) in emu.nds_mmu.apu.channels.iter().enumerate() {
                if !ch.active {
                    continue;
                }
                active_ticks[i] += 1;
                sads[i].insert(ch.sad);
                tmrs[i].insert(ch.tmr);
                vols[i].insert((ch.cnt & 0x7F) as u8);
            }
        }

        // Cross-channel source diversity: how many distinct instruments the whole
        // mix used. One means every voice plays the same waveform.
        let all_sads: std::collections::BTreeSet<u32> =
            sads.iter().flatten().copied().collect();

        eprintln!("INGAME CONTENT over {TICKS} ticks (48 kHz, user scene if EMU_STATE_DIR set)");
        for i in 0..16 {
            if active_ticks[i] == 0 {
                continue;
            }
            eprintln!(
                "  ch{i:<2} active={:<4} keyons={:<3} sads={:<3} tmrs={:<3} vols={:<3} \
                 tmr_range=[{:?}..{:?}] vol_range=[{:?}..{:?}]",
                active_ticks[i],
                emu.nds_mmu.apu.key_ons[i],
                sads[i].len(),
                tmrs[i].len(),
                vols[i].len(),
                tmrs[i].iter().next(),
                tmrs[i].iter().next_back(),
                vols[i].iter().next(),
                vols[i].iter().next_back(),
            );
        }
        eprintln!(
            "INGAME CONTENT distinct sample sources across all channels = {}",
            all_sads.len()
        );

        // A live mix in which no channel ever changes pitch is not music.
        let melodic = (0..16).filter(|&i| tmrs[i].len() > 1).count();
        assert!(
            melodic > 0,
            "no channel ever changed pitch in {TICKS} ticks — the sequencer is not running"
        );
    }

    /// The reported echo, measured on the scene it is reported in: a **cold
    /// boot** through the opening movie, where the chime on each image cut is
    /// heard several times instead of once.
    ///
    /// Every previous audio probe ran from an in-game snapshot, so none of them
    /// ever observed the intro. This one boots from tick 0 and answers the one
    /// question that splits the search space in half:
    ///
    /// * **Upstream** (game/driver/IPC/timers): the same sample source is keyed
    ///   on N times. Then the emulated ARM7 really was told to play it N times
    ///   and the APU is innocent.
    /// * **Downstream** (APU/mixer/resampler/frontend): one key-on, N audible
    ///   attacks. Then something after the register write duplicates it.
    ///
    /// So it records both sides over the same run — the key-on timeline from the
    /// register file, and an onset census computed from the PCM the frontend
    /// would have queued — and prints them together. It also prints the
    /// non-immediate DMA census, because a streamed voice whose refill DMA never
    /// fires loops its first buffer forever, which is the same symptom from a
    /// completely different cause.
    ///
    /// Pure observation: no core state is mutated beyond the counters the other
    /// probes already reset, so it can never itself change what it measures.
    /// `EMU_INTRO_TICKS` (default 6800 = the tick the title screen is up by)
    /// bounds the run; the raw stereo PCM lands in `EMU_EVIDENCE_DIR`.
    #[test]
    #[ignore = "ROM-gated evidence probe; run with --ignored --nocapture"]
    fn nds_intro_audio_echo_report() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../roms/Pokemon - SoulSilver Version (USA).nds"
        );
        let rom = match std::fs::read(path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("SKIP intro audio echo report (no ROM): {e}");
                return;
            }
        };
        let ticks: u32 = std::env::var("EMU_INTRO_TICKS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(6800);

        let mut emu = Emulator::new();
        assert!(emu.load_rom(&rom), "load_rom failed");
        emu.is_playing = true;
        // The host device's real rate: the frontend sets this before the user
        // ever loads a ROM, and producing at a different rate is itself a
        // documented defect class here.
        emu.set_audio_sample_rate(48_000);
        emu.nds_mmu.apu.key_ons = [0; 16];

        // One row per key-on edge. `sad`/`tmr`/`vol` are sampled from the
        // register file at the end of the tick the edge happened in, which is
        // the same resolution the driver writes them at.
        struct KeyOn {
            tick: u32,
            ch: usize,
            sad: u32,
            tmr: u16,
            vol: u8,
            /// SOUNDxCNT bits 8-9: the hardware volume divider (/1 /2 /4 /16).
            /// Whether the driver uses it at all decides how much dynamic range
            /// the BIOS volume table itself has to carry.
            div: u8,
            fmt: u32,
            rep: u32,
        }
        let mut events: Vec<KeyOn> = Vec::new();
        let mut prev_keyons = [0u32; 16];
        let mut pcm: Vec<i16> = Vec::new();
        // Screen content per tick, so a repeated cue can be checked against
        // whether the picture actually changed between the two playings.
        let mut screens: std::collections::HashMap<u32, u64> = Default::default();
        // WRAMCNT modes observed. NOTE these come out of `nds/hle.rs`, which
        // pins mode 3 at boot — so this says which arm is EXERCISED, not which
        // one the cartridge chose. The window is still sized 256 KB where the DS
        // has 32 KB, so the extent below is what says whether that matters.
        let mut wramcnt_seen: std::collections::BTreeSet<u8> = Default::default();

        for t in 0..ticks {
            emu.tick();
            pcm.extend_from_slice(emu.get_audio_buffer());
            let frame: Vec<u8> = emu
                .get_video_buffer()
                .iter()
                .flat_map(|p| p.to_le_bytes())
                .collect();
            screens.insert(t, crate::snapshot::content_hash(&frame));
            wramcnt_seen.insert(emu.nds_mmu.wram_control & 3);
            for i in 0..16 {
                let n = emu.nds_mmu.apu.key_ons[i];
                if n == prev_keyons[i] {
                    continue;
                }
                let ch = &emu.nds_mmu.apu.channels[i];
                // One row per edge, so a channel keyed twice inside one tick is
                // counted twice (both rows carry the tick's final registers).
                for _ in prev_keyons[i]..n {
                    events.push(KeyOn {
                        tick: t,
                        ch: i,
                        sad: ch.sad,
                        tmr: ch.tmr,
                        vol: (ch.cnt & 0x7F) as u8,
                        div: ((ch.cnt >> 8) & 3) as u8,
                        fmt: ch.format(),
                        rep: ch.repeat_mode(),
                    });
                }
                prev_keyons[i] = n;
            }
        }

        let dir = evidence_dir();
        let pcm_path = format!("{dir}/intro_audio.pcm");
        let raw: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let _ = std::fs::write(&pcm_path, &raw);

        // Onset census: 10 ms RMS envelope, and an attack is a frame whose
        // energy jumps well above the running background. This is what "the
        // chime sounded again" is, measured on the samples themselves — it does
        // not care which channel produced it, which is exactly why it can be
        // compared against the key-on count.
        let hz = 48_000usize;
        let win = hz / 100; // 10 ms of stereo frames
        let env: Vec<f64> = pcm
            .chunks(win * 2)
            .map(|c| {
                (c.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>()
                    / c.len().max(1) as f64)
                    .sqrt()
            })
            .collect();
        let mut onsets: Vec<usize> = Vec::new();
        for k in 1..env.len() {
            // 4x the previous frame and clearly above the noise floor, with a
            // 50 ms refractory window so one attack is not counted twice.
            if env[k] > env[k - 1] * 4.0
                && env[k] > 200.0
                && onsets.last().map_or(true, |&p| k - p >= 5)
            {
                onsets.push(k);
            }
        }

        // Repeat groups: the same sample source keyed on again within ~1 s.
        // Musical repetition is spaced by the tempo and lands in the same table,
        // so the discriminator is the *gap* — an echo repeats at tens of ms.
        let mut by_sad: std::collections::BTreeMap<u32, Vec<u32>> = Default::default();
        for e in &events {
            by_sad.entry(e.sad).or_default().push(e.tick);
        }
        let mut tight: Vec<(u32, u32, u32)> = Vec::new(); // (sad, gap ticks, tick)
        for (&sad, ts) in &by_sad {
            for w in ts.windows(2) {
                if w[1] - w[0] <= 12 {
                    tight.push((sad, w[1] - w[0], w[1]));
                }
            }
        }

        eprintln!(
            "INTRO over {ticks} ticks @48kHz: key_ons={} distinct_sads={} \
             onsets={} pcm={} samples -> {pcm_path}",
            events.len(),
            by_sad.len(),
            onsets.len(),
            pcm.len(),
        );
        eprintln!("INTRO key_ons per channel: {:?}", emu.nds_mmu.apu.key_ons);
        eprintln!(
            "INTRO tight re-keys (same SAD within 12 ticks) = {} (first 24: {:?})",
            tight.len(),
            &tight[..tight.len().min(24)]
        );
        // Group the key-ons into CUES: a run of edges no more than 30 ticks
        // apart is one musical event. The user's report is that one cue is heard
        // several times, so the question is whether two cues are the *same* cue —
        // which is a property of the multiset of (sample source, pitch) they
        // start, not of any single channel. Two different jingles cannot share an
        // identical multiset; a cue that repeats identically is the defect.
        // The screen hash at each cue says whether an image transition even
        // happened between them.
        let mut cues: Vec<(u32, u32, u64, usize, u64)> = Vec::new(); // start, end, id, voices, screen
        {
            let mut i = 0;
            while i < events.len() {
                let start = events[i].tick;
                let mut j = i;
                while j + 1 < events.len() && events[j + 1].tick - events[j].tick <= 30 {
                    j += 1;
                }
                let mut voices: Vec<(u32, u16)> =
                    events[i..=j].iter().map(|e| (e.sad, e.tmr)).collect();
                voices.sort_unstable();
                let bytes: Vec<u8> = voices
                    .iter()
                    .flat_map(|(s, t)| {
                        s.to_le_bytes().into_iter().chain(t.to_le_bytes())
                    })
                    .collect();
                cues.push((
                    start,
                    events[j].tick,
                    crate::snapshot::content_hash(&bytes),
                    voices.len(),
                    screens.get(&start).copied().unwrap_or(0),
                ));
                i = j + 1;
            }
        }
        // Screen timeline. "The same cue twice over the same picture" means one
        // thing if the display is animating and quite another if it is frozen,
        // so state which it is instead of inferring it.
        let mut changes: Vec<u32> = Vec::new();
        for t in 1..ticks {
            if screens.get(&t) != screens.get(&(t - 1)) {
                changes.push(t);
            }
        }
        let distinct: std::collections::BTreeSet<u64> = screens.values().copied().collect();
        eprintln!(
            "INTRO screen: {} distinct frames in {ticks} ticks, {} change ticks (first 40: {:?})",
            distinct.len(),
            changes.len(),
            &changes[..changes.len().min(40)]
        );

        eprintln!("INTRO cues (a repeated `id` = the SAME event played again):");
        for (a, b, id, v, scr) in cues.iter().take(16) {
            let dup = cues.iter().filter(|c| c.2 == *id).count();
            eprintln!(
                "  t={a:<5}..{b:<5} voices={v:<2} id={id:#018x} occurrences={dup} screen={scr:#018x}"
            );
        }

        eprintln!("INTRO first 40 key-on events (tick, ch, sad, tmr, vol, fmt, rep):");
        for e in events.iter().take(40) {
            eprintln!(
                "  t={:<5} ch{:<2} sad={:#010x} tmr={:#06x} vol={:<3} div={} fmt={} rep={}",
                e.tick, e.ch, e.sad, e.tmr, e.vol, e.div, e.fmt, e.rep
            );
        }
        eprintln!(
            "INTRO onset ticks (10 ms frames, first 40): {:?}",
            &onsets[..onsets.len().min(40)]
        );
        eprintln!(
            "INTRO SOUNDCNT seen={:#06x} cap_cnt={:?} cap_len={:?} cap_writes={} (first 16: {:?})",
            emu.nds_mmu.apu.dbg_soundcnt_seen,
            emu.nds_mmu.apu.cap_cnt,
            emu.nds_mmu.apu.cap_len,
            emu.nds_mmu.apu.cap_write_log.len(),
            &emu.nds_mmu.apu.cap_write_log
                [..emu.nds_mmu.apu.cap_write_log.len().min(16)],
        );
        eprintln!(
            "INTRO DMA armed[arm9]={:?} fired[arm9]={:?}\nINTRO DMA armed[arm7]={:?} \
             fired[arm7]={:?} (armed-but-never-fired = a refill that never happens)",
            emu.nds_mmu.dma_armed[0],
            emu.nds_mmu.dma_fired[0],
            emu.nds_mmu.dma_armed[1],
            emu.nds_mmu.dma_fired[1],
        );
        // How often each core actually ENTERS its IRQ vector. The NitroSDK sound
        // driver advances the sequence once per driver tick, and a driver ticked
        // more often than hardware would tick it re-strikes notes — which is the
        // reported symptom, from a cause no sample-stream measurement can see. A
        // DS ARM7 takes a handful of IRQs per frame (VBlank, one timer, IPC), so
        // a per-frame figure in the tens or hundreds is the defect itself.
        // Is the shared-WRAM window used at all? The mapping diverges from
        // GBATEK three ways (256 KB where the DS has 32, based at 0x02400000
        // with no 0x03 arm, and modes 2/3 hand each core the OTHER core's half),
        // so reachability is the whole question. In the mode 3 this cartridge
        // selects, our ARM9 maps the upper block and our ARM7 the lower one —
        // so a block that is still entirely zero was never written through that
        // window by the core that owns it, and the divergence stays latent.
        let sw = &emu.nds_mmu.shared_wram;
        let half = sw.len() / 2;
        let nz = |s: &[u8]| s.iter().filter(|&&b| b != 0).count();
        eprintln!(
            "INTRO WRAMCNT modes selected={:?} arm9_ie={:#010x} arm7_ie={:#010x} \
             (ARM7 IE bit 23 = SPI bus, bit 8 = DMA0)\n\
             INTRO shared_wram {} bytes: nonzero lower={} upper={} \
             (mode 3 = the whole window to the ARM7; both 0 = window unused)",
            wramcnt_seen,
            emu.nds_mmu.arm9_ie,
            emu.nds_mmu.arm7_ie,
            sw.len(),
            nz(&sw[..half]),
            nz(&sw[half..]),
        );
        // Extent of the ARM7's use. Hardware gives it 16 KB in mode 3, mirrored
        // across the whole 0x03000000-0x037FFFFF window; this implementation
        // wraps at 128 KB instead. Anything the ARM7 touched at or above 16 KB
        // is memory a real DS does not have, and would have aliased back over
        // the low 16 KB there — so that count is what makes the size divergence
        // live rather than theoretical.
        const HW_ARM7_HALF: usize = 16 * 1024;
        let lower = &sw[..half];
        eprintln!(
            "INTRO shared_wram ARM7 extent: first_nz={:?} last_nz={:?} \
             nonzero<16K={} nonzero>=16K={} (>=16K is memory hardware does not have)",
            lower.iter().position(|&b| b != 0),
            lower.iter().rposition(|&b| b != 0),
            nz(&lower[..HW_ARM7_HALF]),
            nz(&lower[HW_ARM7_HALF..]),
        );
        eprintln!(
            "INTRO IRQ taken: arm9={} ({:.2}/frame) arm7={} ({:.2}/frame)",
            emu.nds_mmu.arm9_irqs_taken,
            emu.nds_mmu.arm9_irqs_taken as f64 / f64::from(ticks),
            emu.nds_mmu.arm7_irqs_taken,
            emu.nds_mmu.arm7_irqs_taken as f64 / f64::from(ticks),
        );

        assert!(!pcm.is_empty(), "no audio at all during the intro");
    }

    /// Walk the actor and dump consecutive frames, so a motion-only visual
    /// defect can be attributed before anything is changed.
    ///
    /// The reported symptom ("a line, and some things look distorted, when I
    /// advance") has two candidate homes and this probe separates them: a tear
    /// from the frontend presenting without VSync lives entirely on the host
    /// side and CANNOT appear here, because these frames come straight out of
    /// the core. So a clean sequence here points at the presenter, and a dirty
    /// one points at the PPU or the rasterizer.
    ///
    /// `EMU_WALK_FRAMES` (default 12) frames are written as `walk_NN.ppm` into
    /// `EMU_EVIDENCE_DIR`, after `EMU_WALK_SKIP` (default 30) frames of walking
    /// so the capture lands in steady-state motion rather than on the first
    /// step. Each frame also gets a row/column discontinuity census: a seam is
    /// a single line whose difference from its neighbour dwarfs the typical
    /// line-to-line difference of the same frame.
    #[test]
    #[ignore = "ROM-gated tool; needs nds_capture_ingame_snapshot first"]
    fn nds_ingame_walk_frames() {
        use crate::snapshot::Reader;
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../roms/Pokemon - SoulSilver Version (USA).nds"
        );
        let (rom, payload) = match (std::fs::read(path), std::fs::read(ingame_snapshot_path())) {
            (Ok(r), Ok(p)) => (r, p),
            (Err(e), _) => {
                eprintln!("SKIP walk frames (no ROM): {e}");
                return;
            }
            (_, Err(e)) => {
                eprintln!("SKIP walk frames (no capture): {e}");
                return;
            }
        };
        let mut emu = Emulator::new();
        assert!(emu.load_rom(&rom), "load_rom failed");
        emu.is_playing = true;
        let mut r = Reader::new(&payload);
        emu.snap_nds(&mut r);
        r.finish().expect("captured snapshot is intact");

        let env_u32 = |k: &str, d: u32| {
            std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d)
        };
        let skip = env_u32("EMU_WALK_SKIP", 30);
        let frames = env_u32("EMU_WALK_FRAMES", 12);
        let dir = evidence_dir();

        // Mean absolute BGR555 channel difference between two 256-pixel lines.
        let line_diff = |a: &[u16], b: &[u16]| -> f64 {
            let mut acc = 0u32;
            for (&p, &q) in a.iter().zip(b.iter()) {
                for sh in [0u16, 5, 10] {
                    let (u, v) = ((p >> sh) & 0x1F, (q >> sh) & 0x1F);
                    acc += u32::from(u.abs_diff(v));
                }
            }
            f64::from(acc) / (a.len() * 3) as f64
        };

        for f in 0..(skip + frames) {
            emu.buttons.down = true;
            emu.tick();
            if f < skip {
                continue;
            }
            let k = f - skip;
            let fb = emu.get_video_buffer();
            dump_ppm(&format!("{dir}/walk_{k:02}.ppm"), 256, 384, fb, false);

            // Row census over the top screen only: the bottom screen is a static
            // menu, so its rows carry no motion signal to compare against.
            let mut diffs: Vec<f64> = (1..192)
                .map(|y| line_diff(&fb[(y - 1) * 256..y * 256], &fb[y * 256..(y + 1) * 256]))
                .collect();
            let mut sorted = diffs.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).expect("pixel diffs are finite"));
            let median = sorted[sorted.len() / 2];
            // A seam has to be both relatively and absolutely large: on a flat
            // scene the median is ~0 and every textured row would "dwarf" it.
            let seams: Vec<(usize, f64)> = diffs
                .drain(..)
                .enumerate()
                .filter(|&(_, d)| d > median * 4.0 + 2.0)
                .map(|(i, d)| (i + 1, d))
                .collect();
            eprintln!(
                "WALK frame {k:02}: row_diff median={median:.2} seams={:?}",
                &seams[..seams.len().min(8)]
            );
        }
        eprintln!("WALK wrote {frames} frames to {dir}/walk_NN.ppm");
    }

    /// Attribute the overworld's moving white sliver to a *layer* before
    /// changing anything.
    ///
    /// The reported "distortion like a line when I advance" reproduces from the
    /// player's own slot-0 savestate in New Bark Town: a long thin near-white
    /// diagonal streak crosses the whole map and moves with the camera. That
    /// shape — thin, straight, spanning the frame — is what a triangle with one
    /// runaway projected vertex looks like, so the first question is whether it
    /// is in the 3D engine's own framebuffer at all. This dumps the composite
    /// and the 3D layer alone for the same frame, plus a census of near-white
    /// pixels in each, which answers it without a human squinting at PPMs.
    ///
    /// Reads a *savestate* (the player-facing container), not the raw capture
    /// the other probes use, because the defect is in the player's scene.
    /// `EMU_STATE_DIR` (default: the app's config dir passed in by the caller)
    /// and `EMU_STATE_SLOT` (default 0) select it; `EMU_WALK_SKIP` frames of
    /// walking put the camera in motion first.
    #[test]
    #[ignore = "savestate-gated probe; run with --ignored --nocapture"]
    fn nds_overworld_sliver_layer_probe() {
        let mut emu = Emulator::new();
        if let Err(e) = load_probe_scene(&mut emu) {
            eprintln!("SKIP sliver probe: {e}");
            return;
        }
        emu.is_playing = true;

        let skip: u32 = std::env::var("EMU_WALK_SKIP")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(50);
        for _ in 0..skip {
            emu.buttons.down = true;
            emu.tick();
        }

        // "Near white" in BGR555: every channel at least 28/31. The overworld's
        // own palette has very little of it (path sand is warm, sky is absent),
        // so the count separates the streak from ordinary content.
        let whitish = |px: &[u16]| -> usize {
            px.iter()
                .filter(|&&p| {
                    p & 0x1F >= 28 && (p >> 5) & 0x1F >= 28 && (p >> 10) & 0x1F >= 28
                })
                .count()
        };

        let dir = evidence_dir();
        let fb3d: Vec<u16> = emu.nds_mmu.gx.engine.fb.clone();
        let composite = emu.get_video_buffer().to_vec();
        dump_ppm(&format!("{dir}/sliver_composite.ppm"), 256, 384, &composite, false);
        dump_ppm(&format!("{dir}/sliver_gx_fb.ppm"), 256, 192, &fb3d, true);
        // The streak's texels traced back to one A3I5 (format 1) texture drawn
        // pure white across several triangles, so decode every A3I5 texture the
        // frame used exactly as the rasterizer samples it. That separates "our
        // sampling mangles the texture" from "this texture really is white and
        // the polygon should not have been opaque".
        for (tex, pal) in emu.nds_mmu.gx.engine.tex_pairs.clone() {
            if (tex >> 26) & 7 != 1 {
                continue;
            }
            let (tw, th, texels) = crate::nds::gx::decode_texture(&emu.nds_mmu.vram, tex, pal);
            let opaque = texels.iter().filter(|p| *p & 0x8000 != 0).count();
            let white = texels
                .iter()
                .filter(|&&p| p & 0x8000 != 0 && p & 0x7FFF == 0x7FFF)
                .count();
            let addr = (tex & 0xFFFF) * 8;
            dump_ppm(
                &format!("{dir}/sliver_tex_{addr:05x}_p{pal:03x}.ppm"),
                tw,
                th,
                &texels,
                true,
            );
            eprintln!(
                "SLIVER a3i5 tex addr={addr:#07x} pal={pal:#x} {tw}x{th} \
                 opaque={opaque}/{} pure_white={white}",
                tw * th
            );
        }

        // Rows at the bottom of the 3D layer with no opaque pixel at all. The
        // nearest geometry projects there, so this is the direct read-out of
        // how much of the scene the near plane is eating.
        let empty_bottom_rows = (0..192)
            .rev()
            .take_while(|&y| fb3d[y * 256..(y + 1) * 256].iter().all(|p| p & 0x8000 == 0))
            .count();
        eprintln!(
            "SLIVER after {skip} walking frames: whitish px composite_top={} gx_layer={} \
             (3D-layer count at/above the composite's means the streak is rasterized geometry)\n\
             SLIVER geometry: tris={} dropped={} near_clipped={} zero_px={} \
             empty_bottom_rows={empty_bottom_rows} viewport={:?} swap_at_vcount={} \n             swaps_in_visible={}/{} (a swap below VCOUNT 192 is a frame the \n             single-buffer rasterizer would have torn on)",
            whitish(&composite[..256 * 192]),
            whitish(&fb3d),
            emu.nds_mmu.gx.engine.last_frame_tris,
            emu.nds_mmu.gx.engine.tris_dropped,
            emu.nds_mmu.gx.engine.last_near_rejected,
            emu.nds_mmu.gx.engine.last_zero_px,
            emu.nds_mmu.gx.engine.viewport(),
            emu.nds_mmu.gx_swap_vcount,
            emu.nds_mmu.gx_swaps_in_visible,
            emu.nds_mmu.gx_swaps_total,
        );
    }

    /// A snapshot must reproduce the machine byte for byte.
    ///
    /// Saves a seeded state, scribbles over one field of every container kind the
    /// codec supports (byte region, fixed vector, capped vector, enum, option,
    /// nested struct, primitives), restores, and re-saves: the two payloads must
    /// be identical. The scribble is asserted to change the payload first, so the
    /// test cannot pass by comparing two copies of an unchanged state.
    #[test]
    fn nds_snapshot_restores_every_visited_field() {
        use crate::snapshot::{Reader, Writer};
        let mut emu = Emulator::new();

        // Seed: distinctive values across the state.
        emu.nds_mmu.main_ram[0x1234] = 0xA5;
        emu.nds_mmu.vram.banks[0].data[0x40] = 0x5A;
        emu.nds_mmu.vram.banks[0].control = 0x83;
        emu.nds_mmu.ipc.fifo_9to7 = vec![0xDEAD_BEEF, 0x0BAD_F00D];
        emu.nds_mmu.timers9.counter[2] = 0x1357;
        emu.nds_mmu.apu.channels[5].cnt = 0x8000_0F7F;
        emu.nds_mmu.apu.channels[5].active = true;
        emu.nds_mmu.apu.soundcnt = 0x807F;
        emu.nds_mmu.spi.tsc.touch_x = 123;
        emu.nds_mmu.spi.tsc.state = crate::nds::spi::TscState::ExpectData;
        emu.nds_mmu.gx.engine.fb[42] = 0x7FFF;
        emu.nds_mmu.gx.engine.clear_px = 0x1234;
        emu.nds_arm9.cpu.registers.gpr[7] = 0x0200_1234;
        emu.nds_arm9.cp15.control = 0x0005_2078;
        emu.nds_arm7.cpu.registers.cpsr = 0x1F;
        emu.nds_ppu.frame_count = 99;
        emu.ticks = 4242;

        let mut w = Writer::default();
        emu.snap_nds(&mut w);
        let saved = w.out;

        // Scribble every one of those back to something else.
        emu.nds_mmu.main_ram[0x1234] = 0;
        emu.nds_mmu.vram.banks[0].data[0x40] = 0;
        emu.nds_mmu.vram.banks[0].control = 0;
        emu.nds_mmu.ipc.fifo_9to7.clear();
        emu.nds_mmu.timers9.counter[2] = 0;
        emu.nds_mmu.apu.channels[5] = Default::default();
        emu.nds_mmu.apu.soundcnt = 0;
        emu.nds_mmu.spi.tsc.touch_x = 0;
        emu.nds_mmu.spi.tsc.state = crate::nds::spi::TscState::ExpectControl;
        emu.nds_mmu.gx.engine.fb[42] = 0;
        emu.nds_mmu.gx.engine.clear_px = 0;
        emu.nds_arm9.cpu.registers.gpr[7] = 0;
        emu.nds_arm9.cp15.control = 0;
        emu.nds_arm7.cpu.registers.cpsr = 0x10;
        emu.nds_ppu.frame_count = 0;
        emu.ticks = 0;

        let mut w = Writer::default();
        emu.snap_nds(&mut w);
        assert_ne!(saved, w.out, "the scribble must actually change the state");

        let mut r = Reader::new(&saved);
        emu.snap_nds(&mut r);
        r.finish().expect("restore consumed the payload exactly");

        let mut w = Writer::default();
        emu.snap_nds(&mut w);
        assert_eq!(saved, w.out, "restored state must re-save byte for byte");
        // Spot-check through the public fields too, so a payload that matches
        // for the wrong reason (e.g. both sides zeroed) still fails.
        assert_eq!(emu.nds_mmu.main_ram[0x1234], 0xA5);
        assert_eq!(emu.nds_mmu.ipc.fifo_9to7, vec![0xDEAD_BEEF, 0x0BAD_F00D]);
        assert_eq!(emu.nds_arm9.cpu.registers.gpr[7], 0x0200_1234);
        assert_eq!(emu.nds_mmu.spi.tsc.state, crate::nds::spi::TscState::ExpectData);
        assert_eq!(emu.ticks, 4242);
    }

    /// The path the player actually uses: `save_state` to a slot file, then
    /// `load_state` back — including the file header, the ROM-identity check and
    /// the integrity hash.
    ///
    /// Exercised on a *mock* NDS cartridge so it runs in the normal suite with no
    /// ROM present; [`Self::nds_snapshot_resumes_identically`] covers the real
    /// game.
    #[test]
    fn nds_savestate_file_round_trips_and_rejects_tampering() {
        let dir = std::env::temp_dir().join("emu_nds_state_test");
        let _ = std::fs::create_dir_all(&dir);
        let base = dir.to_str().expect("utf-8 temp dir");
        // Start from a clean slot so a previous run cannot mask a failure.
        for name in ["savestate_0.sav", "savestate_0.tmp"] {
            let _ = std::fs::remove_file(dir.join(name));
        }

        let mut emu = Emulator::new();
        assert!(emu.load_rom(&mock_nds_rom()), "mock NDS ROM must load");
        assert_eq!(emu.get_console_type(), crate::ffi::ConsoleType::Nds);
        emu.is_playing = true;

        // A value that must survive the trip, chosen where nothing else writes.
        emu.nds_mmu.main_ram[0x2000] = 0xC3;
        emu.ticks = 777;
        assert_eq!(emu.save_state("0", base), "SAVE_STATE_OK");

        let slot = dir.join("savestate_0.sav");
        let bytes = std::fs::read(&slot).expect("slot file written");
        assert_eq!(
            &bytes[..crate::snapshot::MAGIC.len()],
            &crate::snapshot::MAGIC,
            "NDS slots must carry the binary container magic, not JSON"
        );

        emu.nds_mmu.main_ram[0x2000] = 0;
        emu.ticks = 0;
        assert_eq!(emu.load_state("0", base), "LOAD_STATE_OK");
        assert_eq!(emu.nds_mmu.main_ram[0x2000], 0xC3, "RAM restored");
        assert_eq!(emu.ticks, 777, "tick counter restored");

        // A corrupted slot must be refused, not applied: flip a payload byte and
        // confirm the live machine is untouched.
        let mut tampered = bytes.clone();
        *tampered.last_mut().expect("non-empty") ^= 0xFF;
        std::fs::write(&slot, &tampered).expect("rewrite slot");
        let reply = emu.load_state("0", base);
        assert!(reply.starts_with("LOAD_STATE_ERROR"), "tampered slot must fail: {reply}");
        assert_eq!(emu.nds_mmu.main_ram[0x2000], 0xC3, "failed load must not corrupt RAM");

        // A state from a different cartridge must be refused too.
        std::fs::write(&slot, &bytes).expect("restore slot");
        let mut other = Emulator::new();
        let mut other_rom = mock_nds_rom();
        other_rom[0x0C..0x10].copy_from_slice(b"ZZZZ"); // different gamecode
        assert!(other.load_rom(&other_rom));
        other.is_playing = true;
        let reply = other.load_state("0", base);
        assert!(
            reply.contains("different ROM"),
            "a foreign cartridge's state must be refused: {reply}"
        );

        let _ = std::fs::remove_file(&slot);
    }

    /// The determinism oracle for the NDS snapshot: run the real cartridge, save,
    /// diverge, restore, and require the *emulation that follows* to be
    /// identical. This is what catches a field the traversal forgot — such a
    /// field re-saves identically (it is simply never written) yet steers the
    /// machine differently on the next frames.
    ///
    /// `#[ignore]`: needs the ROM and ~1 minute of emulation.
    #[test]
    #[ignore = "ROM-gated determinism oracle; run with --ignored --nocapture"]
    fn nds_snapshot_resumes_identically() {
        use crate::snapshot::{content_hash, Reader, Writer};
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../roms/Pokemon - SoulSilver Version (USA).nds"
        );
        let rom = match std::fs::read(path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("SKIP snapshot determinism (no ROM): {e}");
                return;
            }
        };
        let boot_ticks: u32 = std::env::var("EMU_SNAP_BOOT_TICKS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(600);
        const RESUME_TICKS: u32 = 30;

        let mut emu = Emulator::new();
        assert!(emu.load_rom(&rom), "load_rom failed");
        emu.is_playing = true;
        for _ in 0..boot_ticks {
            emu.tick();
        }

        let mut w = Writer::default();
        emu.snap_nds(&mut w);
        let saved = w.out;

        // Reference: what the machine does next, straight through.
        let fingerprint = |e: &mut Emulator| -> (u64, u64, u32) {
            let mut audio = Vec::new();
            for _ in 0..RESUME_TICKS {
                e.tick();
                audio.extend_from_slice(e.get_audio_buffer());
            }
            let video: Vec<u8> = e
                .get_video_buffer()
                .iter()
                .flat_map(|p| p.to_le_bytes())
                .collect();
            let pcm: Vec<u8> = audio.iter().flat_map(|s| s.to_le_bytes()).collect();
            (content_hash(&video), content_hash(&pcm), e.ticks)
        };
        let reference = fingerprint(&mut emu);

        // Diverge hard, then restore and re-run the same span.
        for _ in 0..90 {
            emu.tick();
        }
        let mut r = Reader::new(&saved);
        emu.snap_nds(&mut r);
        r.finish().expect("restore consumed the payload exactly");
        let resumed = fingerprint(&mut emu);

        assert_eq!(
            reference, resumed,
            "resumed run diverged from the reference: video/audio/tick hashes \
             {reference:?} vs {resumed:?} — a field is missing from the snapshot"
        );
        eprintln!(
            "SNAPSHOT OK: {} bytes, {boot_ticks} boot ticks, hashes {reference:?}",
            saved.len()
        );
    }

    /// Directory the `#[ignore]`d evidence probes write their dumps into.
    /// Defaults to the repo root so a bare `cargo test` run keeps the historic
    /// paths; `EMU_EVIDENCE_DIR` redirects them to a scratch dir.
    fn evidence_dir() -> String {
        std::env::var("EMU_EVIDENCE_DIR")
            .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/..").to_string())
    }

    /// Put `emu` into a gameplay scene for the `#[ignore]`d in-game probes.
    ///
    /// Two sources, one entry point so every probe measures the same thing:
    /// * `EMU_STATE_DIR` set — the *player-facing* savestate container in that
    ///   directory (slot `EMU_STATE_SLOT`, default 0). This is how a defect
    ///   reported from an actual session gets reproduced on the exact scene it
    ///   was seen in, rather than on whatever the repo happens to have captured.
    /// * otherwise — the raw capture `nds_capture_ingame_snapshot` writes, which
    ///   is the historic behaviour and needs no player data.
    ///
    /// Returns the reason to skip; probes are ROM/state-gated by design, so a
    /// missing input is a skip and never a failure.
    fn load_probe_scene(emu: &mut Emulator) -> Result<(), String> {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
        if let Ok(state_dir) = std::env::var("EMU_STATE_DIR") {
            let slot = std::env::var("EMU_STATE_SLOT").unwrap_or_else(|_| "0".to_string());
            let res = emu.load_rom_path("roms/Pokemon - SoulSilver Version (USA).nds", root);
            if !res.starts_with("LOAD_ROM_OK") {
                return Err(res);
            }
            let res = emu.load_state(&slot, &state_dir);
            if !res.starts_with("LOAD_STATE_OK") {
                return Err(res);
            }
            return Ok(());
        }
        let rom = std::fs::read(format!("{root}/roms/Pokemon - SoulSilver Version (USA).nds"))
            .map_err(|e| format!("no ROM: {e}"))?;
        let payload =
            std::fs::read(ingame_snapshot_path()).map_err(|e| format!("no capture: {e}"))?;
        if !emu.load_rom(&rom) {
            return Err("load_rom failed".to_string());
        }
        let mut r = crate::snapshot::Reader::new(&payload);
        emu.snap_nds(&mut r);
        r.finish().map_err(|e| format!("captured snapshot is damaged: {e}"))?;
        Ok(())
    }

    /// Write a BGR555 buffer as a binary PPM. `transparent` marks pixels whose
    /// bit 15 is clear with magenta so a layer's coverage is visible at a glance.
    fn dump_ppm(path: &str, w: usize, h: usize, px: &[u16], transparent: bool) {
        let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
        for &p in px.iter().take(w * h) {
            if transparent && p & 0x8000 == 0 {
                ppm.extend_from_slice(&[255, 0, 255]);
                continue;
            }
            ppm.extend_from_slice(&[
                ((p & 0x1F) << 3) as u8,
                (((p >> 5) & 0x1F) << 3) as u8,
                (((p >> 10) & 0x1F) << 3) as u8,
            ]);
        }
        let _ = std::fs::write(path, &ppm);
    }

    /// Title-screen layer census (ROM-gated, `#[ignore]`).
    ///
    /// Boots SoulSilver with no input for `EMU_EVIDENCE_TICKS` frames (default
    /// 6800 = the "TOUCH TO START" frame the GUI shows) and dumps enough state
    /// to attribute a visual defect to a *layer* before anything is changed:
    /// `title_composite.ppm` is the finished 256x384 frame (byte-identical to
    /// the frontend's `--dump-video`), `title_gx_fb.ppm` is the 3D engine's
    /// 256x192 output alone with transparent pixels in magenta.
    ///
    /// Open defects this exists for: the flat slab that replaces the sea floor
    /// behind "TOUCH TO START", and Lugia's fragmented dorsal fins.
    #[test]
    #[ignore]
    fn nds_title_layer_evidence() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../roms/Pokemon - SoulSilver Version (USA).nds"
        );
        let rom = match std::fs::read(path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("SKIP title-layer evidence (no ROM): {e}");
                return;
            }
        };
        let ticks: u32 = std::env::var("EMU_EVIDENCE_TICKS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(6800);
        let mut emu = Emulator::new();
        assert!(emu.load_rom(&rom), "load_rom failed");
        emu.is_playing = true;
        // `EMU_PROBE_PX=x,y` arms the 3D per-pixel fragment trace for the last
        // few frames only: a full run swaps ~1700 times, so tracing from tick 0
        // would bury the answer.
        let probe_px = std::env::var("EMU_PROBE_PX").ok().and_then(|s| {
            let (a, b) = s.split_once(',')?;
            Some((a.trim().parse::<usize>().ok()?, b.trim().parse::<usize>().ok()?))
        });
        const TRACE_TAIL_TICKS: u32 = 8;
        for t in 0..ticks {
            if t + TRACE_TAIL_TICKS == ticks {
                emu.nds_mmu.gx.engine.probe_px = probe_px;
                // Record which (TEXIMAGE_PARAM, PLTT_BASE) pairs the final
                // frames actually sample, so each can be decoded and eyeballed.
                emu.nds_mmu.gx.engine.tex_stats_on = true;
                emu.nds_mmu.gx.engine.tex_stats.clear();
            }
            emu.tick();
        }
        emu.nds_mmu.gx.engine.probe_px = None;
        emu.nds_mmu.gx.engine.tex_stats_on = false;

        let dir = evidence_dir();
        dump_ppm(&format!("{dir}/title_composite.ppm"), 256, 384, emu.get_video_buffer(), false);
        dump_ppm(&format!("{dir}/title_gx_fb.ppm"), 256, 192, &emu.nds_mmu.gx.engine.fb, true);

        let io = &emu.nds_mmu.arm9_io;
        let rh = |b: usize| u16::from_le_bytes([io[b], io[b + 1]]);
        let rw = |b: usize| u32::from_le_bytes([io[b], io[b + 1], io[b + 2], io[b + 3]]);
        eprintln!("TITLE f{ticks}: powcnt1={:#06x} (bit15 => engine A on top screen)", rh(0x304));
        for (base, name) in [(0usize, "A"), (0x1000, "B")] {
            eprintln!(
                "  {name}: dispcnt={:#010x} (mode={} bg0_3d={} objmap1d={} win={:03b}) \
                 bgcnt=[{:04x} {:04x} {:04x} {:04x}] bld={:04x}/{:04x} bldy={:04x} bright={:04x} \
                 win0h={:04x} win1h={:04x} win0v={:04x} win1v={:04x} winin={:04x} winout={:04x}",
                rw(base),
                rw(base) & 7,
                (rw(base) >> 3) & 1,
                (rw(base) >> 4) & 1,
                (rw(base) >> 13) & 7,
                rh(base + 0x08), rh(base + 0x0A), rh(base + 0x0C), rh(base + 0x0E),
                rh(base + 0x50), rh(base + 0x52), rh(base + 0x54), rh(base + 0x6C),
                rh(base + 0x40), rh(base + 0x42), rh(base + 0x44), rh(base + 0x46),
                rh(base + 0x48), rh(base + 0x4A),
            );
        }
        eprintln!(
            "  3D: tris={} fb_opaque={} clear_px={:#06x} disp3dcnt={:#06x} swaps={}",
            emu.nds_mmu.gx.engine.last_frame_tris,
            emu.nds_mmu.gx.engine.fb.iter().filter(|&&p| p & 0x8000 != 0).count(),
            emu.nds_mmu.gx.engine.clear_px,
            rh(0x060),
            emu.nds_mmu.gx.engine.swap_count,
        );
        // Every texture the closing frames sampled, decoded flat. A texture that
        // looks clean here but ragged on screen indicts sampling/UVs; one that
        // is already ragged indicts the VRAM upload above the renderer.
        let mut stats = emu.nds_mmu.gx.engine.tex_stats.clone();
        stats.sort_by_key(|e| std::cmp::Reverse(e.2 + e.3));
        for (tex, pal, opaque, clear) in stats.iter().copied() {
            let (w, h, texels) = crate::nds::gx::decode_texture(&emu.nds_mmu.vram, tex, pal);
            let name = format!(
                "tex_{:05x}_f{}_p{:x}.ppm",
                (tex & 0xFFFF) * 8,
                (tex >> 26) & 7,
                pal
            );
            dump_ppm(&format!("{dir}/{name}"), w, h, &texels, true);
            eprintln!(
                "  TEX {name}: {w}x{h} param={tex:#010x} px_opaque={opaque} px_clear={clear}"
            );
        }
        let ah = &emu.nds_mmu.gx.engine.attr_alpha_histo;
        eprintln!(
            "  3D POLYGON_ATTR alpha: wireframe(0)={} translucent(1..30)={} opaque(31)={} \
             nonzero={:?}",
            ah[0],
            ah[1..31].iter().sum::<u32>(),
            ah[31],
            ah.iter().enumerate().filter(|(_, &n)| n > 0).collect::<Vec<_>>(),
        );
        // OBJ census per engine: mode 1 = semi-transparent, mode 2 = OBJ window
        // (both currently unimplemented), so a large mode-1/2 sprite over the
        // sea floor would explain a flat slab without touching the 3D engine.
        for (oam_base, name) in [(0usize, "A"), (0x400, "B")] {
            let mut rows: Vec<String> = Vec::new();
            for i in 0..128usize {
                let at = oam_base + i * 8;
                let a0 = u16::from_le_bytes([emu.nds_mmu.oam[at], emu.nds_mmu.oam[at + 1]]);
                let a1 = u16::from_le_bytes([emu.nds_mmu.oam[at + 2], emu.nds_mmu.oam[at + 3]]);
                let a2 = u16::from_le_bytes([emu.nds_mmu.oam[at + 4], emu.nds_mmu.oam[at + 5]]);
                let rotscale = a0 & 0x100 != 0;
                if !rotscale && a0 & 0x200 != 0 {
                    continue;
                }
                rows.push(format!(
                    "#{i}(y={} x={} mode={} shape={} size={} prio={} rs={})",
                    a0 & 0xFF,
                    a1 & 0x1FF,
                    (a0 >> 10) & 3,
                    (a0 >> 14) & 3,
                    (a1 >> 14) & 3,
                    (a2 >> 10) & 3,
                    rotscale as u8,
                ));
            }
            eprintln!("  OBJ {name}: {} visible {}", rows.len(), rows.join(" "));
        }
        // Audio census for the same frame. The title BGM measured byte-clean
        // end to end (core dump == the bytes handed to SDL, queue never
        // starved), so a reported "broken speaker" here would have to be
        // *content*: a channel silently dropped, or the SDK's capture-based
        // reverb that this APU never records into.
        let apu = &emu.nds_mmu.apu;
        let live: Vec<String> = apu
            .channels
            .iter()
            .enumerate()
            .filter(|(_, c)| c.active)
            .map(|(i, c)| {
                format!(
                    "#{i}(fmt={} vol={} div={} pan={} tmr={:#06x} len={})",
                    c.format(),
                    c.cnt & 0x7F,
                    (c.cnt >> 8) & 3,
                    (c.cnt >> 16) & 0x7F,
                    c.tmr,
                    c.len,
                )
            })
            .collect();
        eprintln!(
            "  APU: soundcnt={:#06x} (master_vol={} enable={}) bias={:#06x} live={} {}",
            apu.soundcnt,
            apu.soundcnt & 0x7F,
            (apu.soundcnt >> 15) & 1,
            apu.soundbias,
            live.len(),
            live.join(" "),
        );
        eprintln!(
            "  SNDCAP: cnt={:?} dad={:#x?} len={:?} reg_writes={} (nonzero cnt bit7 => armed, \
             and this APU never records into the ring)",
            apu.cap_cnt,
            apu.cap_dad,
            apu.cap_len,
            apu.cap_write_log.len(),
        );
    }
}
