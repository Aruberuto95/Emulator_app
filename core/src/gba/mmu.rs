use crate::cpu_bus::CpuBus;
use crate::gba::apu::GbaApu;
use crate::gba::dma::GbaDma;
use crate::gba::flash::Flash128;

/// `n` bytes at `off` inside a region that repeats every `size` bytes, or `None`
/// when the access would wrap the mirror.
///
/// Returning `None` at the wrap is what keeps the fast paths in
/// [`GbaMmu::contiguous`] and [`GbaMmu::contiguous_mut`] exactly equivalent to
/// the byte-at-a-time decode they short-circuit: the byte path folds each byte
/// independently, so a straddling access must go there and be folded per byte.
/// `size` is passed rather than read from `buf.len()` so these stay pinned to
/// the same literals `GbaMmu::read_byte` uses and cannot drift from it.
#[inline]
fn mirrored(buf: &[u8], off: u32, size: u32, n: u32) -> Option<&[u8]> {
    let o = (off % size) as usize;
    buf.get(o..o + n as usize)
}

/// Mutable twin of [`mirrored`]; see [`GbaMmu::contiguous_mut`] for why byte
/// writes must never reach it.
#[inline]
fn mirrored_mut(buf: &mut [u8], off: u32, size: u32, n: u32) -> Option<&mut [u8]> {
    let o = (off % size) as usize;
    buf.get_mut(o..o + n as usize)
}

#[derive(Clone)]
pub struct GbaTimer {
    pub counter: u16,
    pub reload: u16,
    pub control: u16,
    pub cycle_accumulator: u32,
    pub overflowed: bool,
}

impl GbaTimer {
    pub fn new() -> Self {
        Self {
            counter: 0,
            reload: 0,
            control: 0,
            cycle_accumulator: 0,
            overflowed: false,
        }
    }

    pub fn get_prescaler(&self) -> u32 {
        match self.control & 3 {
            0 => 1,
            1 => 64,
            2 => 256,
            _ => 1024,
        }
    }
}

pub struct GbaMmu {
    pub bios: Vec<u8>,
    pub ewram: Vec<u8>,
    pub iwram: Vec<u8>,
    pub palette_ram: [u8; 1024],
    pub vram: Vec<u8>,
    pub oam: [u8; 1024],
    pub rom: Vec<u8>,

    pub flash: Flash128,
    pub apu: GbaApu,
    pub dma: GbaDma,
    pub timers: [GbaTimer; 4],

    pub io: [u8; 1024],
    pub waitcnt: u16,
    pub ie: u16,
    pub r_if: u16,
    pub ime: u32,

    pub bios_protected: bool,
    pub last_bios_read: u32,

    // Previous VBlank/HBlank DISPSTAT flag state, for edge-triggering timed DMAs.
    // process_dmas() runs every cycle and the flags stay high for the whole blank
    // period, so a repeat DMA must fire only on the rising edge (once per blank),
    // not every cycle — otherwise it re-transfers thousands of times and walks its
    // source/dest pointers through memory, corrupting RAM.
    dma_prev_vblank: bool,
    dma_prev_hblank: bool,
    /// W1 evidence: DMA triggers per start timing, and the VCOUNT range over
    /// which HBlank-timed transfers fired. Hardware does not start HBlank DMA
    /// during VBlank, so a correct core triggers on 160 lines per frame; this
    /// core has no VCOUNT gate and is expected to show 228. Emerald drives its
    /// whole scanline-effect engine (74 HBlank-DMA control literals in the ROM)
    /// through that path, so the difference mis-phases every per-line effect.
    pub dbg_dma_trigger: [u64; 4],
    pub dbg_hblank_dma_vcount_min: u8,
    pub dbg_hblank_dma_vcount_max: u8,
    pub dbg_frames: u64,

    // Batching scheduler state (see Emulator::tick GBA loop). `pending_cycles` is
    // the CPU-cycle debt accumulated since the last tick_system_components() flush;
    // it lives here (not in the exec loop) so the IO read path can derive live
    // register values (timer counters) mid-batch. `io_dirty` is set by any CPU
    // write into the IO region and tells the exec loop to flush at the next
    // instruction boundary, so writes that change component config (timer control,
    // DMA enable, sound regs) take effect on the same instruction boundary as the
    // old per-instruction ticking.
    pub pending_cycles: u32,
    pub io_dirty: bool,

    pub rom_path: std::path::PathBuf,
    pub base_dir: std::path::PathBuf,
}

impl GbaMmu {
    pub fn new(rom_data: Vec<u8>) -> Self {
        let mut bios = vec![0u8; 16384];
        // SWI bodies are intercepted directly at the instruction level, but the
        // hardware IRQ path still vectors to 0x18 and runs the BIOS handler there.
        // With no real BIOS that address is zero, so a fired IRQ would execute
        // garbage. Inject the canonical BIOS IRQ dispatcher at 0x18 so interrupts
        // reach the game's own handler at [0x03FFFFFC] (mirror of 0x03007FFC):
        //   STMFD sp!, {r0-r3,r12,lr};  MOV r0,#0x4000000;  ADD lr,pc,#0
        //   LDR pc,[r0,#-4];            LDMFD sp!, {r0-r3,r12,lr};  SUBS pc,lr,#4
        const IRQ_HANDLER: [u32; 6] = [
            0xE92D_500F, 0xE3A0_0301, 0xE28F_E000, 0xE510_F004, 0xE8BD_500F, 0xE25E_F004,
        ];
        for (i, word) in IRQ_HANDLER.iter().enumerate() {
            bios[0x18 + i * 4..0x18 + i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }

        // GBA power-on / BIOS-handoff state: DISPCNT = 0x0080 (Forced Blank, bit 7).
        // The real BIOS hands off with the screen blanked. Games rely on this: e.g.
        // pokeemerald's GPU register manager writes REG_DISPSTAT (VBlank-IRQ-enable)
        // immediately to hardware only while forced-blank is set (or during VBlank),
        // otherwise it defers the write to the next VBlank IRQ — which can never
        // arrive if the enable itself was deferred. Without forced-blank at boot,
        // the VBlank IRQ is never armed and the game hangs in WaitForVBlank.
        let mut io = [0u8; 1024];
        io[0] = 0x80; // DISPCNT low byte: Forced Blank = bit 7 (0x0080)
        // BIOS hands off with SOUNDBIAS = 0x0200 (bias level 512). MP2K/"Sappy" sound
        // drivers read-modify-write SOUNDBIAS to set the sampling-rate/amplitude bits;
        // if io[] reads back 0 here their RMW drops the bias level to 0, which the
        // output DAC models as a hard clip that rectifies Direct Sound to silence-like
        // distortion. Seed the BIOS default (must stay in sync with GbaApu::new).
        io[0x88] = 0x00;
        io[0x89] = 0x02;

        Self {
            bios,
            ewram: vec![0u8; 256 * 1024],
            iwram: vec![0u8; 32 * 1024],
            palette_ram: [0u8; 1024],
            vram: vec![0u8; 96 * 1024],
            oam: [0u8; 1024],
            rom: rom_data,
            flash: Flash128::new(),
            apu: GbaApu::new(),
            dma: GbaDma::new(),
            timers: [
                GbaTimer::new(),
                GbaTimer::new(),
                GbaTimer::new(),
                GbaTimer::new(),
            ],
            io,
            waitcnt: 0,
            ie: 0,
            r_if: 0,
            ime: 0,
            bios_protected: false,
            last_bios_read: 0xEA00002E, // standard branch opcode
            dma_prev_vblank: false,
            dma_prev_hblank: false,
            dbg_dma_trigger: [0; 4],
            dbg_hblank_dma_vcount_min: 255,
            dbg_hblank_dma_vcount_max: 0,
            dbg_frames: 0,
            pending_cycles: 0,
            io_dirty: false,
            rom_path: std::path::PathBuf::new(),
            base_dir: std::path::PathBuf::new(),
        }
    }

    pub fn trigger_interrupt(&mut self, interrupt_bit: u16) {
        self.r_if |= interrupt_bit;
    }

    /// Cycles until timer `i` overflows, or None if it is disabled or cascade
    /// (cascade timers advance on the prior timer's overflow, not on cycles).
    /// This is the same math the tick_system_components pre-scan uses to clamp
    /// batch steps, so both agree on the exact overflow cycle.
    fn timer_cycles_to_overflow(&self, i: usize) -> Option<u32> {
        let enabled = (self.timers[i].control & 0x0080) != 0;
        let cascade = (self.timers[i].control & 0x0004) != 0;
        if !enabled || cascade {
            return None;
        }
        let prescaler = self.timers[i].get_prescaler();
        let acc = self.timers[i].cycle_accumulator;
        let cycles_to_next_tick = if prescaler > acc { prescaler - acc } else { 1 };
        let ticks_to_overflow = 0x10000 - self.timers[i].counter as u32;
        Some(cycles_to_next_tick + (ticks_to_overflow - 1) * prescaler)
    }

    /// Cycles until the next event whose effect is observable by the CPU: a PPU
    /// boundary (HBlank/VBlank/VCOUNT flags + IRQs, scanline render), a timer
    /// overflow (IRQ, FIFO drain, cascade step), or an APU frame-sequencer step
    /// (NR52/length state). Between now and that point, ticking the system
    /// components is pure bookkeeping, so the exec loop may run the CPU freely
    /// and flush the accumulated cycles in one batch.
    ///
    /// Also capped at the resampler's cycles_per_sample so the DS/PSG mix is
    /// evaluated at least once per output sample (audio fidelity bound, not a
    /// correctness bound). The cap scales with speed, so fast-forward batches
    /// grow exactly when fidelity matters least.
    pub fn cycles_to_next_event(&self, ppu: &crate::gba::ppu::GbaPpu, speed: f32) -> u32 {
        let mut next = ppu.cycles_to_next_boundary();
        for i in 0..4 {
            if let Some(c) = self.timer_cycles_to_overflow(i) {
                next = next.min(c);
            }
        }
        next = next.min(self.apu.cycles_to_next_frame_seq());
        let cycles_per_sample =
            ((16_777_216.0 * speed as f64) / self.apu.resampler.output_hz()) as u32;
        next.min(cycles_per_sample).max(1)
    }

    // --- Timing and Ticking ---
    pub fn tick_system_components(
        &mut self,
        elapsed: u32,
        video_slice: &mut [u16],
        audio_buf: &mut [i16],
        audio_off: usize,
        speed: f32,
        ppu: &mut crate::gba::ppu::GbaPpu,
        is_render_tick: bool,
    ) {
        // Fast path: the per-step pre-scan below only ever *lowers* `step` when an
        // enabled, non-cascade timer is running (cascade timers advance on the prior
        // timer's overflow, not on cycles, so they never clamp). When no such timer
        // exists, `step` is always == `remaining`, the loop runs exactly once, and the
        // 4-timer scan is pure overhead. Detect that once up front and skip the scan.
        // This is byte-identical to the general path: same tick_timers/ppu/apu/dma
        // call sequence, same `step` values, same overflow-interrupt cycle boundaries.
        let any_clamp_timer = (0..4).any(|i| {
            (self.timers[i].control & 0x0080) != 0 && (self.timers[i].control & 0x0004) == 0
        });

        let mut remaining = elapsed;
        while remaining > 0 {
            let mut step = remaining;
            if any_clamp_timer {
                for i in 0..4 {
                    if let Some(cycles_to_overflow) = self.timer_cycles_to_overflow(i) {
                        if cycles_to_overflow < step {
                            step = cycles_to_overflow;
                        }
                    }
                }
                if step == 0 {
                    step = 1;
                }
            }

            // Advance components by step cycles. APU strictly BEFORE timers: the step is
            // clamped to end exactly on any timer overflow, and that overflow pops the
            // DirectSound FIFO into the current_sample latch. The step covers [t, t+step),
            // whose audible state is the latch at t — ticking the APU first keeps the pop
            // out of the interval, so the sample transition lands on the exact overflow
            // cycle. Timers-first rendered the popped sample retroactively over the whole
            // step (up to a full batch early), i.e. per-sample phase jitter of up to ~30%
            // of the MP2K FIFO period — audible as constant faint rasp. PPU stays before
            // process_dmas (it refreshes the DISPSTAT blank edges DMA triggers read).
            self.apu.tick(step, audio_buf, audio_off, speed);
            self.tick_timers(step);
            ppu.tick(step, self, video_slice, is_render_tick);
            self.process_dmas();

            remaining -= step;
        }
    }

    fn tick_timers(&mut self, elapsed: u32) {
        let mut overflow_signals = [false; 4];
        for i in 0..4 {
            let enabled = (self.timers[i].control & 0x0080) != 0;
            if !enabled {
                self.timers[i].overflowed = false;
                continue;
            }

            let cascade = (self.timers[i].control & 0x0004) != 0;
            if cascade {
                if i > 0 && overflow_signals[i - 1] {
                    let (new_count, overflow) = self.timers[i].counter.overflowing_add(1);
                    if overflow {
                        self.timers[i].counter = self.timers[i].reload;
                        overflow_signals[i] = true;
                        self.timers[i].overflowed = true;
                        self.apu.on_timer_overflow(i);
                        if (self.timers[i].control & 0x0040) != 0 {
                            self.trigger_interrupt(1 << (3 + i)); // Timer 0-3 interrupts are bits 3-6
                        }
                    } else {
                        self.timers[i].counter = new_count;
                        self.timers[i].overflowed = false;
                    }
                } else {
                    self.timers[i].overflowed = false;
                }
            } else {
                self.timers[i].cycle_accumulator += elapsed;
                let prescaler = self.timers[i].get_prescaler();
                while self.timers[i].cycle_accumulator >= prescaler {
                    self.timers[i].cycle_accumulator -= prescaler;
                    let (new_count, overflow) = self.timers[i].counter.overflowing_add(1);
                    if overflow {
                        self.timers[i].counter = self.timers[i].reload;
                        overflow_signals[i] = true;
                        self.timers[i].overflowed = true;
                        self.apu.on_timer_overflow(i);
                        if (self.timers[i].control & 0x0040) != 0 {
                            self.trigger_interrupt(1 << (3 + i));
                        }
                    } else {
                        self.timers[i].counter = new_count;
                        self.timers[i].overflowed = false;
                    }
                }
            }
        }
    }

    fn process_dmas(&mut self) {
        // Edge-detect the VBlank/HBlank flags once per call: a timed DMA fires on
        // the rising edge only (VBlank/HBlank *start*), not every cycle the flag is
        // high. Level-triggering here would re-run a repeat DMA hundreds of times
        // per blank period and corrupt memory as its pointers run off.
        //
        // DISPSTAT VBlank(bit0)/HBlank(bit1) both live in the low byte io[4]; read it
        // directly instead of read_halfword_safe(0x04000004), which does two full
        // region-decoded read_byte() calls + a rotate just to extract these two bits.
        // The PPU keeps io[4] current every scanline via write_halfword_safe(0x04000004,..),
        // so this is byte-identical. This function is called ~150k times/frame — the bus
        // decode was ~40% of the per-instruction system-tick cost.
        let dispstat_lo = self.io[4];
        let vblank = (dispstat_lo & 0x01) != 0;
        let hblank = (dispstat_lo & 0x02) != 0;
        let vblank_edge = vblank && !self.dma_prev_vblank;
        let hblank_edge = hblank && !self.dma_prev_hblank;
        // Update edge state UNCONDITIONALLY, before any early-return, so prev flags never
        // go stale. A timed DMA enabled mid-blank later thus sees no spurious rising edge.
        self.dma_prev_vblank = vblank;
        self.dma_prev_hblank = hblank;

        // Idle fast-path: with no active channel and no pending sound-FIFO request there is
        // nothing any trigger could fire, so skip the 4-channel scan entirely. Edge state is
        // already latched above, so future edges remain correct. (In this state the loop
        // below would `continue` past every channel and transfer nothing.)
        if !self.dma.channels[0].active
            && !self.dma.channels[1].active
            && !self.dma.channels[2].active
            && !self.dma.channels[3].active
            && !self.apu.dma_request_a
            && !self.apu.dma_request_b
        {
            return;
        }

        // DMA 0 to 3 Priority Order
        for ch in 0..4 {
            let active = self.dma.channels[ch].active;
            if !active {
                continue;
            }

            let timing = (self.dma.channels[ch].control >> 12) & 3;
            let mut trigger = false;

            match timing {
                0 => trigger = true,      // Immediate
                1 => trigger = vblank_edge, // VBlank start
                // HBlank start. Hardware sets the DISPSTAT HBlank flag and
                // raises its IRQ on all 228 scanlines, but does NOT start
                // HBlank-timed DMA during VBlank (GBATEK) — so the gate belongs
                // here and not in the PPU, which must keep flagging every line
                // for the IRQ to stay correct. io[6] is VCOUNT.
                2 => trigger = hblank_edge && self.io[6] < 160,
                3 => {
                    // Special trigger (sound FIFO); self-clears via the APU request.
                    // Hardware routes by DESTINATION, not channel number: DMA1 and
                    // DMA2 each serve whichever FIFO their DAD points at (GBATEK), so
                    // an inverted assignment (DMA1->FIFO B) must still refill. A DAD
                    // that is neither FIFO port never triggers — same as hardware.
                    // When both channels target one FIFO, the lower channel wins the
                    // request (this loop runs in priority order) — also hardware.
                    if ch == 1 || ch == 2 {
                        match self.dma.channels[ch].dad & !3 {
                            0x0400_00A0 if self.apu.dma_request_a => {
                                trigger = true;
                                self.apu.dma_request_a = false;
                            }
                            0x0400_00A4 if self.apu.dma_request_b => {
                                trigger = true;
                                self.apu.dma_request_b = false;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }

            if trigger {
                // Evidence: triggers per start timing, and for HBlank which
                // scanlines actually fired. io[6] is VCOUNT, kept current by
                // the PPU every line.
                self.dbg_dma_trigger[timing as usize] += 1;
                if timing == 2 {
                    let vc = self.io[6];
                    self.dbg_hblank_dma_vcount_min = self.dbg_hblank_dma_vcount_min.min(vc);
                    self.dbg_hblank_dma_vcount_max = self.dbg_hblank_dma_vcount_max.max(vc);
                }
                self.execute_dma_channel(ch);
            }
        }
    }

    fn execute_dma_channel(&mut self, ch: usize) {
        let dest_ctrl = (self.dma.channels[ch].control >> 5) & 3;
        let src_ctrl = (self.dma.channels[ch].control >> 7) & 3;
        let repeat = (self.dma.channels[ch].control & 0x0200) != 0;
        let timing = (self.dma.channels[ch].control >> 12) & 3;

        // Sound-FIFO DMA (special timing on ch1/ch2) is a hardware special case: the
        // controller ignores the programmed DEST address control and transfer size,
        // forcing a FIXED destination (the FIFO port) and 32-bit units, transferring
        // exactly 4 words (16 bytes) per request. Games (e.g. the MP2K/"Sappy" engine
        // in Pokémon) legitimately program DEST=increment here; honoring it walks
        // cur_dest off 0x040000A0/A4 after the first word, starving the FIFO (total
        // silence) and corrupting the adjacent DMA registers at 0x040000B0. Force the
        // hardware behavior instead of trusting the ROM-supplied fields.
        let is_fifo = timing == 3 && (ch == 1 || ch == 2);
        let is_32bit = is_fifo || (self.dma.channels[ch].control & 0x0400) != 0;
        let unit_bytes = if is_32bit { 4 } else { 2 };
        let count = if is_fifo {
            4
        } else {
            self.dma.channels[ch].cur_count
        };

        for _ in 0..count {
            let src_addr = self.dma.channels[ch].cur_src;
            let dest_addr = self.dma.channels[ch].cur_dest;

            if is_32bit {
                let val = self.read_word_safe(src_addr);
                self.write_word_safe(dest_addr, val);
            } else {
                let val = self.read_halfword_safe(src_addr);
                self.write_halfword_safe(dest_addr, val);
            }

            // Update source address
            match src_ctrl {
                0 => {
                    self.dma.channels[ch].cur_src =
                        self.dma.channels[ch].cur_src.wrapping_add(unit_bytes)
                } // Increment
                1 => {
                    self.dma.channels[ch].cur_src =
                        self.dma.channels[ch].cur_src.wrapping_sub(unit_bytes)
                } // Decrement
                _ => {} // Fixed
            }

            // Update dest address. Skipped for FIFO DMA: hardware holds the
            // destination fixed at the FIFO port regardless of dest_ctrl.
            if !is_fifo {
                match dest_ctrl {
                    0 | 3 => {
                        self.dma.channels[ch].cur_dest =
                            self.dma.channels[ch].cur_dest.wrapping_add(unit_bytes)
                    } // Increment / Increment & Reload
                    1 => {
                        self.dma.channels[ch].cur_dest =
                            self.dma.channels[ch].cur_dest.wrapping_sub(unit_bytes)
                    } // Decrement
                    _ => {} // Fixed
                }
            }
        }

        // Trigger IRQ if requested
        if (self.dma.channels[ch].control & 0x4000) != 0 {
            self.trigger_interrupt(1 << (8 + ch)); // DMA 0-3 interrupts are bits 8-11
        }

        if repeat && timing != 0 {
            // Repeat: reload count, reload dest if dest_ctrl is Reload (3)
            self.dma.channels[ch].cur_count = self.dma.channels[ch].count;
            if dest_ctrl == 3 {
                self.dma.channels[ch].cur_dest = self.dma.channels[ch].dad;
            }
        } else {
            // Disable DMA channel
            self.dma.channels[ch].control &= !0x8000;
            self.dma.channels[ch].active = false;
        }
    }

    // --- Memory Read/Write Operations ---

    /// Directly set the VCOUNT register (0x04000006). The PPU owns this value and
    /// updates it every scanline; CPU writes to 0x06/0x07 are (correctly) ignored
    /// by `write_byte`, which would otherwise also swallow the PPU's own updates
    /// and freeze VCOUNT at 0 (games busy-wait on it -> permanent black screen).
    pub fn set_vcount(&mut self, vcount: u16) {
        self.io[0x06] = (vcount & 0xFF) as u8;
        self.io[0x07] = ((vcount >> 8) & 0xFF) as u8;
    }

    pub fn read_byte(&self, address: u32) -> u8 {
        let region = (address >> 24) & 0x0F;
        let offset = address & 0x00FF_FFFF;

        match region {
            0x00 => {
                // BIOS ROM
                if offset < 16384 {
                    self.bios[offset as usize]
                } else {
                    0
                }
            }
            0x02 => {
                // EWRAM (256 KB)
                let ew_offset = (offset % (256 * 1024)) as usize;
                self.ewram[ew_offset]
            }
            0x03 => {
                // IWRAM (32 KB)
                let iw_offset = (offset % (32 * 1024)) as usize;
                self.iwram[iw_offset]
            }
            0x04 => {
                // I/O registers
                if offset < 1024 {
                    match offset {
                        // Direct Sound FIFOs are write-only, read returns 0
                        0xA0 | 0xA4 => 0,
                        // Sound control registers are HLE'd in the APU; io[] holds the
                        // raw written bytes and would leak write-only bits (FIFO-reset
                        // strobes) and miss the live read-only PSG channel-active
                        // flags of SOUNDCNT_X. Same canonical-field pattern as IE/IF.
                        0x82..=0x85 => self.apu.read_register(offset),
                        // Timer counters (TMxCNT_L) return the live counter, not the
                        // last-written reload latch. The counter lives in
                        // self.timers[i], so io[] would be stale (0) and games that
                        // busy-wait on a timer would hang forever.
                        0x100 | 0x101 | 0x104 | 0x105 | 0x108 | 0x109 | 0x10C | 0x10D => {
                            let idx = ((offset - 0x100) / 4) as usize;
                            let shift = (offset & 0x1) * 8; // 0 = low, 8 = high byte
                            // The batching exec loop may owe the components up to
                            // `pending_cycles` of un-ticked time; derive the live
                            // counter value for an enabled non-cascade timer instead
                            // of returning the stale stored one. No overflow can be
                            // pending: the loop always flushes at or before the next
                            // timer overflow (cycles_to_next_event), so the derived
                            // value never wraps. Cascade timers advance only at
                            // flushed overflows, so their stored counter is exact.
                            let t = &self.timers[idx];
                            let enabled = (t.control & 0x0080) != 0;
                            let cascade = (t.control & 0x0004) != 0;
                            let counter = if enabled && !cascade {
                                let ticks =
                                    (t.cycle_accumulator + self.pending_cycles) / t.get_prescaler();
                                t.counter as u32 + ticks
                            } else {
                                t.counter as u32
                            };
                            ((counter >> shift) & 0xFF) as u8
                        }
                        // Interrupt / wait-state registers are tracked in dedicated
                        // fields (peripherals raise IF via `trigger_interrupt`, not
                        // via io[]). Reading io[] here would miss those and never
                        // clear on ack, so return the canonical field bytes.
                        0x200 => self.ie as u8,
                        0x201 => (self.ie >> 8) as u8,
                        0x202 => self.r_if as u8,
                        0x203 => (self.r_if >> 8) as u8,
                        0x204 => self.waitcnt as u8,
                        0x205 => (self.waitcnt >> 8) as u8,
                        0x208 => self.ime as u8,
                        0x209 => (self.ime >> 8) as u8,
                        0x20A => (self.ime >> 16) as u8,
                        0x20B => (self.ime >> 24) as u8,
                        _ => self.io[offset as usize],
                    }
                } else {
                    0
                }
            }
            0x05 => {
                // Palette RAM (1 KB)
                let pal_offset = (offset % 1024) as usize;
                self.palette_ram[pal_offset]
            }
            0x06 => {
                // VRAM (96 KB)
                let vram_offset = (offset % (96 * 1024)) as usize;
                self.vram[vram_offset]
            }
            0x07 => {
                // OAM (1 KB)
                let oam_offset = (offset % 1024) as usize;
                self.oam[oam_offset]
            }
            0x08 | 0x09 | 0x0A | 0x0B | 0x0C | 0x0D => {
                // Game Pak ROM (up to 32 MB)
                if (offset as usize) < self.rom.len() {
                    self.rom[offset as usize]
                } else {
                    0
                }
            }
            0x0E => {
                // Flash backup save space
                self.flash.read_byte(address)
            }
            _ => 0,
        }
    }

    pub fn write_byte(&mut self, address: u32, value: u8) {
        let region = (address >> 24) & 0x0F;
        let offset = address & 0x00FF_FFFF;

        match region {
            0x02 => {
                let ew_offset = (offset % (256 * 1024)) as usize;
                self.ewram[ew_offset] = value;
            }
            0x03 => {
                let iw_offset = (offset % (32 * 1024)) as usize;
                self.iwram[iw_offset] = value;
            }
            0x04 => {
                if offset < 1024 {
                    // VCOUNT is read-only. Do not allow CPU writes to 0x06 and 0x07.
                    if offset != 0x06 && offset != 0x07 {
                        self.io[offset as usize] = value;
                        self.on_io_write_byte(offset, value);
                        // Tell the batching exec loop to flush at the next instruction
                        // boundary: this write may have changed component config (timer
                        // control, DMA enable, sound regs), so pending cycles must be
                        // ticked and the next-event distance recomputed. Writes made
                        // *during* a flush (PPU DISPSTAT, DMA-to-FIFO) also land here;
                        // the loop clears the flag after flushing, so those self-discard.
                        self.io_dirty = true;
                    }
                }
            }
            0x05 => {
                // Palette RAM: byte writes are duplicated into halfwords
                let pal_offset = ((offset & !1) % 1024) as usize;
                self.palette_ram[pal_offset] = value;
                self.palette_ram[pal_offset + 1] = value;
            }
            0x06 => {
                // VRAM: byte writes to BG (first 64KB) are duplicated into halfwords.
                // OBJ VRAM (at offset >= 64KB) byte writes are ignored.
                let vram_offset = (offset % (96 * 1024)) as usize;
                if vram_offset < 64 * 1024 {
                    let aligned = vram_offset & !1;
                    self.vram[aligned] = value;
                    self.vram[aligned + 1] = value;
                }
            }
            0x07 => {
                // OAM: byte writes are ignored
            }
            0x0E => {
                self.flash.write_byte(address, value);
            }
            _ => {}
        }
    }

    // --- I/O register byte write hook ---
    fn on_io_write_byte(&mut self, offset: u32, value: u8) {
        match offset {
            // PSG channels 1-4 registers (SOUND1-4CNT at 0x60-0x7F) + NR50/NR51 (0x80/0x81).
            0x60..=0x81 => {
                self.apu.write_psg_register(offset, value);
            }
            // PSG wave RAM (channel 3), 0x90-0x9F.
            0x90..=0x9F => {
                self.apu.write_psg_register(offset, value);
            }

            // Sound registers HLE writes: SOUNDCNT_H (0x82/0x83), SOUNDCNT_X (0x84/0x85),
            // SOUNDBIAS (0x88/0x89).
            0x82 | 0x83 | 0x84 | 0x85 | 0x88 | 0x89 => {
                self.apu.write_register(offset, value);
            }

            // FIFO A writes
            0xA0..=0xA3 => {
                self.apu.fifo_a.push(value as i8);
            }
            // FIFO B writes
            0xA4..=0xA7 => {
                self.apu.fifo_b.push(value as i8);
            }

            // Interrupt registers
            0x200 => self.ie = (self.ie & 0xFF00) | (value as u16),
            0x201 => self.ie = (self.ie & 0x00FF) | ((value as u16) << 8),
            0x202 => {
                // IF: write 1 clears the flag
                let clear_mask = value as u16;
                self.r_if &= !clear_mask;
            }
            0x203 => {
                let clear_mask = (value as u16) << 8;
                self.r_if &= !clear_mask;
            }
            0x204 => self.waitcnt = (self.waitcnt & 0xFF00) | (value as u16),
            0x205 => self.waitcnt = (self.waitcnt & 0x00FF) | ((value as u16) << 8),

            0x208 => self.ime = (self.ime & 0xFFFFFF00) | (value as u32),
            0x209 => self.ime = (self.ime & 0xFFFF00FF) | ((value as u32) << 8),
            0x20A => self.ime = (self.ime & 0xFF00FFFF) | ((value as u32) << 16),
            0x20B => self.ime = (self.ime & 0x00FFFFFF) | ((value as u32) << 24),

            // DMA Registers
            0xB0..=0xDF => {
                let dma_idx = ((offset - 0xB0) / 12) as usize;
                let reg_offset = (offset - 0xB0) % 12;
                if dma_idx < 4 {
                    match reg_offset {
                        0..=3 => self.dma.channels[dma_idx].write_sad(reg_offset, value),
                        4..=7 => self.dma.channels[dma_idx].write_dad(reg_offset - 4, value),
                        8 | 9 => self.dma.channels[dma_idx].write_count(reg_offset - 8, value),
                        10 | 11 => self.dma.channels[dma_idx].write_control(reg_offset - 10, value),
                        _ => {}
                    }
                }
            }

            // Timers Registers
            0x100..=0x10F => {
                let timer_idx = ((offset - 0x100) / 4) as usize;
                let reg_offset = (offset - 0x100) % 4;
                if timer_idx < 4 {
                    match reg_offset {
                        // Reload writes invalidate the DirectSound interpolation period
                        // ONLY when the value actually changes: sound drivers re-write
                        // the same reload defensively (and 16-bit writes land as two
                        // byte writes), and a no-op reset per write would degrade the
                        // ramp to ZOH over and over — an audible click train.
                        0 => {
                            let old = self.timers[timer_idx].reload;
                            self.timers[timer_idx].reload = (old & 0xFF00) | (value as u16);
                            if self.timers[timer_idx].reload != old
                                && (timer_idx == 0 || timer_idx == 1)
                            {
                                self.apu.handle_timer_change(timer_idx);
                            }
                        }
                        1 => {
                            let old = self.timers[timer_idx].reload;
                            self.timers[timer_idx].reload = (old & 0x00FF) | ((value as u16) << 8);
                            if self.timers[timer_idx].reload != old
                                && (timer_idx == 0 || timer_idx == 1)
                            {
                                self.apu.handle_timer_change(timer_idx);
                            }
                        }
                        2 => {
                            // The counter is latched from the reload value only on the
                            // 0->1 enable edge (real hardware). Reloading on every write
                            // with bit7 set would reset the DirectSound sample-rate timer's
                            // phase whenever a game re-touches TMxCNT_H (e.g. to change the
                            // prescaler/IRQ), producing audible clicks.
                            let was_enabled = (self.timers[timer_idx].control & 0x0080) != 0;
                            self.timers[timer_idx].control =
                                (self.timers[timer_idx].control & 0xFF00) | (value as u16);
                            let now_enabled = (value & 0x80) != 0;
                            if now_enabled && !was_enabled {
                                self.timers[timer_idx].counter = self.timers[timer_idx].reload;
                                self.timers[timer_idx].cycle_accumulator = 0;
                                if timer_idx == 0 || timer_idx == 1 {
                                    self.apu.handle_timer_change(timer_idx);
                                }
                            }
                        }
                        3 => {
                            self.timers[timer_idx].control =
                                (self.timers[timer_idx].control & 0x00FF) | ((value as u16) << 8);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // --- Aligned Reads with Rotations ---

    /// `n` bytes of plain memory at `addr`, when the region decode lands in one
    /// of the contiguous byte arrays and the access fits without wrapping the
    /// region's mirror.
    ///
    /// Throughput only, and it is the hottest decision on the GBA side.
    /// [`Self::read_word`] is the instruction-fetch path (every pipeline fill in
    /// `gba/cpu.rs`) as well as every `LDR`, and it was built out of four
    /// [`Self::read_byte`] calls — so one 32-bit access re-ran the
    /// `addr >> 24` dispatch and the region modulo four times over.
    ///
    /// Only regions that are genuinely plain arrays qualify; **I/O (0x04),
    /// flash (0x0E), BIOS and open bus all fall through to the byte path**, so
    /// no register read, no live timer counter and no open-bus behaviour can be
    /// bypassed. The arms mirror [`Self::read_byte`]'s own branches one for one,
    /// including the mirror sizes; if that decode ever changes this must change
    /// with it, which is why the arithmetic is repeated rather than shared with
    /// a helper that could drift.
    #[inline]
    fn contiguous(&self, addr: u32, n: u32) -> Option<&[u8]> {
        let off = addr & 0x00FF_FFFF;
        match (addr >> 24) & 0x0F {
            0x02 => mirrored(&self.ewram, off, 256 * 1024, n),
            0x03 => mirrored(&self.iwram, off, 32 * 1024, n),
            0x05 => mirrored(&self.palette_ram, off, 1024, n),
            0x06 => mirrored(&self.vram, off, 96 * 1024, n),
            0x07 => mirrored(&self.oam, off, 1024, n),
            // Game Pak ROM does not mirror: `read_byte` returns 0 past the end,
            // so a partially out-of-range access must take the byte path.
            0x08..=0x0D => self.rom.get(off as usize..(off as usize) + n as usize),
            _ => None,
        }
    }

    /// Write-side twin of [`Self::contiguous`], for **16- and 32-bit stores only**.
    ///
    /// Deliberately not reachable from [`Self::write_byte`]: palette, VRAM and
    /// OAM sit on a 16-bit bus where a byte write is duplicated across the
    /// halfword (and dropped entirely for OBJ VRAM), which is the quirk
    /// [`Self::write_halfword`] exists to document. At halfword granularity and
    /// wider every region listed here stores verbatim, so a straight copy is the
    /// same observable behaviour. I/O (0x04) is excluded because its writes have
    /// side effects (`on_io_write_byte`, `io_dirty`), and flash (0x0E) because
    /// it is a command-driven state machine, not memory.
    #[inline]
    fn contiguous_mut(&mut self, addr: u32, n: u32) -> Option<&mut [u8]> {
        let off = addr & 0x00FF_FFFF;
        match (addr >> 24) & 0x0F {
            0x02 => mirrored_mut(&mut self.ewram, off, 256 * 1024, n),
            0x03 => mirrored_mut(&mut self.iwram, off, 32 * 1024, n),
            0x05 => mirrored_mut(&mut self.palette_ram, off, 1024, n),
            0x06 => mirrored_mut(&mut self.vram, off, 96 * 1024, n),
            0x07 => mirrored_mut(&mut self.oam, off, 1024, n),
            _ => None,
        }
    }

    /// Split fast/cold so the *check* inlines into the caller and the decode
    /// does not: `contiguous` is a handful of compares, but it used to sit
    /// inside a function large enough that LLVM never inlined the whole thing
    /// into `CpuBus::read_halfword`, so every access paid a call even when the
    /// fast path hit — which is nearly always.
    #[inline(always)]
    pub fn read_halfword(&self, address: u32) -> u16 {
        // If address is odd, trigger rotation (or just alignment in GBA depending on region)
        // Usually, halfword read at odd address yields: rotated value
        let aligned_addr = address & !1;
        let val = match self.contiguous(aligned_addr, 2) {
            Some(b) => u16::from_le_bytes([b[0], b[1]]),
            None => self.read_halfword_cold(aligned_addr),
        };

        if (address & 1) != 0 {
            // Rotate right by 8 bits
            (val >> 8) | (val << 8)
        } else {
            val
        }
    }

    /// Byte-path tail of [`Self::read_halfword`]; takes the ALREADY-ALIGNED
    /// address, because the rotation is the caller's business.
    #[inline(never)]
    fn read_halfword_cold(&self, aligned_addr: u32) -> u16 {
        let b0 = self.read_byte(aligned_addr) as u16;
        let b1 = self.read_byte(aligned_addr + 1) as u16;
        b0 | (b1 << 8)
    }

    /// The instruction-fetch path. Split fast/cold for the reason given on
    /// [`Self::read_halfword`].
    #[inline(always)]
    pub fn read_word(&self, address: u32) -> u32 {
        let aligned_addr = address & !3;
        let val = match self.contiguous(aligned_addr, 4) {
            Some(b) => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            None => self.read_word_cold(aligned_addr),
        };

        let rotation = (address & 3) * 8;
        if rotation > 0 {
            (val >> rotation) | (val << (32 - rotation))
        } else {
            val
        }
    }

    /// Byte-path tail of [`Self::read_word`]; takes the ALREADY-ALIGNED address.
    #[inline(never)]
    fn read_word_cold(&self, aligned_addr: u32) -> u32 {
        let b0 = self.read_byte(aligned_addr) as u32;
        let b1 = self.read_byte(aligned_addr + 1) as u32;
        let b2 = self.read_byte(aligned_addr + 2) as u32;
        let b3 = self.read_byte(aligned_addr + 3) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    pub fn write_halfword(&mut self, address: u32, value: u16) {
        let aligned_addr = address & !1;
        // VRAM/Palette/OAM have a 16-bit bus: a 16-bit write stores both bytes verbatim.
        // Routing through write_byte would trigger the 8-bit-bus duplication quirk (each
        // byte fanned out across the halfword), corrupting every 16-bit write — fatal for
        // tile data and bitmap-mode pixels. Write those regions directly instead.
        let bytes = value.to_le_bytes();
        if let Some(dst) = self.contiguous_mut(aligned_addr, 2) {
            dst.copy_from_slice(&bytes);
            return;
        }
        self.write_byte(aligned_addr, bytes[0]);
        self.write_byte(aligned_addr + 1, bytes[1]);
    }

    pub fn write_word(&mut self, address: u32, value: u32) {
        let aligned_addr = address & !3;
        let bytes = value.to_le_bytes();
        if let Some(dst) = self.contiguous_mut(aligned_addr, 4) {
            dst.copy_from_slice(&bytes);
            return;
        }
        // Same 16-bit-bus reasoning as write_halfword; a 32-bit write is two 16-bit writes.
        self.write_halfword(aligned_addr, (value & 0xFFFF) as u16);
        self.write_halfword(aligned_addr + 2, ((value >> 16) & 0xFFFF) as u16);
    }

    // --- Boundary Safe Methods for CPU/SWI HLE ---

    pub fn read_byte_safe(&self, address: u32) -> u8 {
        self.read_byte(address)
    }

    pub fn read_halfword_safe(&self, address: u32) -> u16 {
        self.read_halfword(address)
    }

    pub fn read_word_safe(&self, address: u32) -> u32 {
        self.read_word(address)
    }

    pub fn write_byte_safe(&mut self, address: u32, value: u8) {
        self.write_byte(address, value);
    }

    pub fn write_halfword_safe(&mut self, address: u32, value: u16) {
        self.write_halfword(address, value);
    }

    pub fn write_word_safe(&mut self, address: u32, value: u32) {
        self.write_word(address, value);
    }

    // --- PPU Layer Access Functions ---

    pub fn read_vram_byte(&self, address: u32) -> u8 {
        let offset = (address % (96 * 1024)) as usize;
        self.vram[offset]
    }

    pub fn read_vram_halfword(&self, address: u32) -> u16 {
        let aligned = address & !1;
        let offset = (aligned % (96 * 1024)) as usize;
        let b0 = self.vram[offset] as u16;
        let b1 = self.vram[offset + 1] as u16;
        b0 | (b1 << 8)
    }

    pub fn read_palette_halfword(&self, address: u32) -> u16 {
        let aligned = address & !1;
        let offset = (aligned % 1024) as usize;
        let b0 = self.palette_ram[offset] as u16;
        let b1 = self.palette_ram[offset + 1] as u16;
        b0 | (b1 << 8)
    }

    pub fn read_oam_halfword(&self, address: u32) -> u16 {
        let aligned = address & !1;
        let offset = (aligned % 1024) as usize;
        let b0 = self.oam[offset] as u16;
        let b1 = self.oam[offset + 1] as u16;
        b0 | (b1 << 8)
    }

    // --- BIOS RegisterRamReset clears ---

    pub fn clear_ewram(&mut self) {
        self.ewram.fill(0);
    }

    pub fn clear_iwram_safe(&mut self) {
        // BIOS RegisterRamReset preserves the last 0x200 bytes of IWRAM
        // (0x03007E00-0x03007FFF): interrupt vector at 0x03007FFC, interrupt/BIOS
        // stacks, and the BIOS interrupt-flag words. Wiping them corrupts the IRQ
        // return path. Clear only 0x00000-0x07E00.
        let keep_from = 0x7E00;
        self.iwram[..keep_from].fill(0);
    }

    pub fn clear_palette_ram(&mut self) {
        self.palette_ram.fill(0);
    }

    pub fn clear_vram(&mut self) {
        self.vram.fill(0);
    }

    pub fn clear_oam(&mut self) {
        self.oam.fill(0);
    }

    pub fn reset_sio_registers(&mut self) {
        // Clear SIO registers at 0x120..0x12F
        for i in 0x120..0x130 {
            self.io[i] = 0;
        }
    }

    pub fn reset_sound_registers(&mut self) {
        // Clear sound registers at 0x60..0xAF
        for i in 0x60..0xB0 {
            self.io[i] = 0;
        }
        self.apu = GbaApu::new();
        // Keep io[] in sync with the APU's SOUNDBIAS default (0x0200) so a driver that
        // read-modify-writes SOUNDBIAS after a reset preserves the bias level. See new().
        self.io[0x88] = 0x00;
        self.io[0x89] = 0x02;
    }

    pub fn reset_other_io_registers(&mut self) {
        // Clear remaining IO (except key status, etc.)
        for i in 0..1024 {
            if (i < 0x60 || i >= 0xB0) && (i < 0x120 || i >= 0x130) {
                self.io[i] = 0;
            }
        }
        // BIOS RegisterRamReset leaves the display in Forced Blank (DISPCNT bit 7);
        // games (pokeemerald) depend on it so their immediate REG_DISPSTAT writes
        // reach hardware before the first VBlank. See GbaMmu::new for the full why.
        self.io[0] = 0x80; // DISPCNT low byte: Forced Blank (0x0080)
    }
}

/// Drives the shared ARM interpreter against GBA memory. Each method forwards to
/// the inherent `GbaMmu` method of the same name (disambiguated as `GbaMmu::…`
/// so it binds to the inherent method, not this trait method — no recursion),
/// so the generic interpreter emits the same code it did with `&mut GbaMmu`.
/// The `*_safe` variants use the trait's default forwards, matching what the
/// old inherent `*_safe` methods did.
impl CpuBus for GbaMmu {
    fn read_byte(&mut self, addr: u32) -> u8 {
        GbaMmu::read_byte(self, addr)
    }
    fn read_halfword(&mut self, addr: u32) -> u16 {
        GbaMmu::read_halfword(self, addr)
    }
    fn read_word(&mut self, addr: u32) -> u32 {
        GbaMmu::read_word(self, addr)
    }
    fn write_byte(&mut self, addr: u32, val: u8) {
        GbaMmu::write_byte(self, addr, val);
    }
    fn write_halfword(&mut self, addr: u32, val: u16) {
        GbaMmu::write_halfword(self, addr, val);
    }
    fn write_word(&mut self, addr: u32, val: u32) {
        GbaMmu::write_word(self, addr, val);
    }

    /// Same predicate as the trait default, read straight out of the canonical
    /// fields instead of through the region decode. `read_byte` already routes
    /// 0x200-0x203 and 0x208-0x20B to exactly these three (see its I/O arm), so
    /// this is the same value by construction, not an approximation.
    fn irq_pending(&mut self) -> bool {
        (self.ime & 1) != 0 && (self.ie & self.r_if) != 0
    }

    fn clear_iwram_safe(&mut self) {
        GbaMmu::clear_iwram_safe(self);
    }
    fn clear_ewram(&mut self) {
        GbaMmu::clear_ewram(self);
    }
    fn clear_vram(&mut self) {
        GbaMmu::clear_vram(self);
    }
    fn clear_palette_ram(&mut self) {
        GbaMmu::clear_palette_ram(self);
    }
    fn clear_oam(&mut self) {
        GbaMmu::clear_oam(self);
    }
    fn reset_sound_registers(&mut self) {
        GbaMmu::reset_sound_registers(self);
    }
    fn reset_sio_registers(&mut self) {
        GbaMmu::reset_sio_registers(self);
    }
    fn reset_other_io_registers(&mut self) {
        GbaMmu::reset_other_io_registers(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hardware flags HBlank on all 228 scanlines but only starts HBlank-timed
    /// DMA on the 160 visible ones. Without the VCOUNT gate a per-scanline
    /// effect engine consumes 68 extra entries per frame and every line lands
    /// on the wrong row.
    #[test]
    fn hblank_dma_does_not_fire_during_vblank() {
        let mut video: [u16; 240 * 160] = [0; 240 * 160];
        // A full frame of ticking produces real audio samples; size the sink
        // for them rather than tripping the resampler's overflow guard.
        let mut audio: [i16; 4096] = [0; 4096];
        let mut mmu = GbaMmu::new(vec![]);
        let mut ppu = crate::gba::ppu::GbaPpu::new();

        // DMA0: HBlank timing, repeat, 16-bit, count 1, EWRAM -> IWRAM.
        for (addr, val) in [
            (0x040000B0u32, 0x00u8), (0x040000B1, 0x00), (0x040000B2, 0x00), (0x040000B3, 0x02),
            (0x040000B4, 0x00), (0x040000B5, 0x00), (0x040000B6, 0x00), (0x040000B7, 0x03),
            (0x040000B8, 0x01), (0x040000B9, 0x00),
            (0x040000BA, 0x00), (0x040000BB, 0xA2), // control high byte last: enable edge
        ] {
            mmu.write_byte(addr, val);
        }
        assert!(mmu.dma.channels[0].active, "channel must arm on the enable edge");

        // One full frame, stopping at the HBlank boundary of each scanline the
        // way the batching exec loop does: ticking a whole 1232-cycle line at
        // once would set and clear the flag inside one batch, so `process_dmas`
        // would never observe the edge.
        let mut hblank_flag_seen_in_vblank = false;
        for _ in 0..228 {
            mmu.tick_system_components(960, &mut video, &mut audio, 0, 1.0, &mut ppu, false);
            // Sampled inside the HBlank window, which is where the flag lives.
            if mmu.io[6] >= 160 && mmu.read_halfword_safe(0x04000004) & 0x0002 != 0 {
                hblank_flag_seen_in_vblank = true;
            }
            mmu.tick_system_components(272, &mut video, &mut audio, 0, 1.0, &mut ppu, false);
        }
        assert_eq!(
            mmu.dbg_dma_trigger[2], 160,
            "HBlank DMA must fire on the 160 visible lines only, not all 228"
        );
        assert!(
            mmu.dbg_hblank_dma_vcount_max < 160,
            "no HBlank DMA may fire during VBlank; max VCOUNT was {}",
            mmu.dbg_hblank_dma_vcount_max
        );
        // The HBlank flag itself must still be raised during VBlank, because
        // the HBlank IRQ depends on it — only the DMA start is gated.
        assert!(
            hblank_flag_seen_in_vblank,
            "HBlank flag must still be set on VBlank lines"
        );
    }

    // The batching exec loop trusts cycles_to_next_event() to be exact: ticking
    // one cycle short of it must raise no interrupt, and the next cycle must.
    #[test]
    fn next_event_is_exact() {
        let mut video: [u16; 0] = [];
        let mut audio: [i16; 0] = [];

        // Timer overflow event. Reload 0xFF00, prescaler 1, IRQ enable:
        // overflow after exactly 256 cycles.
        let mut mmu = GbaMmu::new(vec![]);
        let mut ppu = crate::gba::ppu::GbaPpu::new();
        mmu.write_byte(0x04000100, 0x00);
        mmu.write_byte(0x04000101, 0xFF);
        mmu.write_byte(0x04000102, 0xC0); // enable + IRQ, prescaler = 1
        let next = mmu.cycles_to_next_event(&ppu, 1.0);
        assert_eq!(next, 256, "timer overflow must be the nearest event");
        mmu.tick_system_components(next - 1, &mut video, &mut audio, 0, 1.0, &mut ppu, false);
        assert_eq!(mmu.r_if & 0x0008, 0, "no timer IRQ one cycle early");
        mmu.tick_system_components(1, &mut video, &mut audio, 0, 1.0, &mut ppu, false);
        assert_ne!(mmu.r_if & 0x0008, 0, "timer IRQ exactly at the event cycle");

        // PPU HBlank event at cycle 960. At speed 4.0 the audio-sample cap
        // (~1522) is above it, so the PPU boundary is the nearest event.
        let mut mmu = GbaMmu::new(vec![]);
        let mut ppu = crate::gba::ppu::GbaPpu::new();
        let next = mmu.cycles_to_next_event(&ppu, 4.0);
        assert_eq!(next, 960, "HBlank boundary must be the nearest event");
        mmu.tick_system_components(next - 1, &mut video, &mut audio, 0, 4.0, &mut ppu, false);
        assert_eq!(mmu.read_halfword_safe(0x04000004) & 0x0002, 0, "no HBlank one cycle early");
        mmu.tick_system_components(1, &mut video, &mut audio, 0, 4.0, &mut ppu, false);
        assert_ne!(mmu.read_halfword_safe(0x04000004) & 0x0002, 0, "HBlank flag exactly at 960");
    }

    // Mid-batch the components are owed mmu.pending_cycles of time; a CPU read
    // of a running timer's counter must return the live derived value, and the
    // stored value must match once the batch is flushed.
    #[test]
    fn timer_counter_live_read_with_pending() {
        let mut mmu = GbaMmu::new(vec![]);
        let mut ppu = crate::gba::ppu::GbaPpu::new();
        mmu.write_byte(0x04000102, 0x81); // enable, prescaler = 64, counter = reload = 0
        mmu.pending_cycles = 130; // 130/64 = 2 whole ticks owed
        assert_eq!(mmu.read_byte(0x04000100), 2, "derived live counter mid-batch");

        let mut video: [u16; 0] = [];
        let mut audio: [i16; 0] = [];
        mmu.pending_cycles = 0;
        mmu.tick_system_components(130, &mut video, &mut audio, 0, 1.0, &mut ppu, false);
        assert_eq!(mmu.timers[0].counter, 2, "flushed counter matches the derived value");
        assert_eq!(mmu.read_byte(0x04000100), 2);
    }

    // Any CPU write into the IO region must flag the exec loop to flush pending
    // cycles at the next instruction boundary; non-IO writes must not.
    #[test]
    fn io_write_sets_dirty() {
        let mut mmu = GbaMmu::new(vec![]);
        assert!(!mmu.io_dirty);
        mmu.write_byte(0x02000000, 0x42); // EWRAM
        assert!(!mmu.io_dirty, "non-IO writes must not force a flush");
        mmu.write_byte(0x04000102, 0x80); // timer control
        assert!(mmu.io_dirty, "IO writes must force a flush");
    }

    #[test]
    fn vram_halfword_write_is_not_duplicated() {
        let mut mmu = GbaMmu::new(vec![]);
        mmu.write_halfword(0x06000000, 0x1234);
        assert_eq!(mmu.read_vram_halfword(0), 0x1234);
        assert_eq!(mmu.read_vram_byte(0), 0x34);
        assert_eq!(mmu.read_vram_byte(1), 0x12);
    }

    #[test]
    fn vram_word_write_is_not_duplicated() {
        let mut mmu = GbaMmu::new(vec![]);
        mmu.write_word(0x06000000, 0xAABB_CCDD);
        assert_eq!(mmu.read_vram_halfword(0), 0xCCDD);
        assert_eq!(mmu.read_vram_halfword(2), 0xAABB);
    }

    /// Addresses that between them hit every arm of the region decode, both
    /// straddle cases and both edges of every mirror. Shared by the two
    /// equivalence tests below so they cannot drift apart.
    ///
    /// The fast paths in `contiguous`/`contiguous_mut` are a duplicate of
    /// `read_byte`'s decode, kept deliberately (see the doc comments there), so
    /// what has to be pinned is that the duplicate never disagrees with the
    /// original — not any particular literal value.
    fn bus_probe_addresses() -> Vec<u32> {
        vec![
            0x0000_0000, 0x0000_3FFE, 0x0000_3FFF, // BIOS, and past its end
            0x0200_0000, 0x0203_FFFE, 0x0203_FFFF, // EWRAM base and mirror edge
            0x0204_0000, 0x020F_FFFE, // first mirror, deep mirror
            0x0300_0000, 0x0300_7FFE, 0x0300_7FFF, // IWRAM base and mirror edge
            0x0300_8000, 0x03FF_FFFC, // mirrors
            0x0400_0000, 0x0400_0006, 0x0400_0088, 0x0400_00A0, // I/O: DISPCNT, VCOUNT, SOUNDBIAS, FIFO
            0x0400_0100, 0x0400_0200, 0x0400_0202, 0x0400_0208, // live timer, IE, IF, IME
            0x0400_03FE, 0x0400_0400, // last I/O byte, and past the 1 KB window
            0x0500_0000, 0x0500_03FE, 0x0500_03FF, 0x0500_0400, // palette + mirror edge
            0x0600_0000, 0x0601_7FFE, 0x0601_7FFF, 0x0601_8000, // VRAM 96 KB edge
            0x0700_0000, 0x0700_03FE, 0x0700_03FF, // OAM edge
            0x0800_0000, 0x0800_0002, // ROM
            0x0E00_0000, 0x0E00_0001, // flash state machine
            0x0F00_0000, 0x1000_0000, // unmapped / above the decode
        ]
    }

    /// Every 16- and 32-bit READ must equal its byte-by-byte composition.
    ///
    /// `read_word` is the instruction-fetch path, so it was rebuilt on a
    /// `contiguous` fast path that skips three of four region decodes. This
    /// compares against `read_byte` rather than against literals, so it stays
    /// true if the decode is ever changed — the failure mode it exists to catch
    /// is the fast path and the byte path disagreeing, especially at a mirror
    /// wrap or on a region with side effects.
    #[test]
    fn word_reads_match_the_byte_path_everywhere() {
        let rom: Vec<u8> = (0..4096u32).map(|i| (i ^ (i >> 5)) as u8).collect();
        let mut mmu = GbaMmu::new(rom);
        // Fill the plain regions with a position-dependent pattern so a wrong
        // offset cannot coincidentally read the right value.
        for (i, b) in mmu.ewram.iter_mut().enumerate() {
            *b = (i * 7 + 1) as u8;
        }
        for (i, b) in mmu.iwram.iter_mut().enumerate() {
            *b = (i * 11 + 2) as u8;
        }
        for (i, b) in mmu.vram.iter_mut().enumerate() {
            *b = (i * 13 + 3) as u8;
        }
        for (i, b) in mmu.palette_ram.iter_mut().enumerate() {
            *b = (i * 17 + 4) as u8;
        }
        for (i, b) in mmu.oam.iter_mut().enumerate() {
            *b = (i * 19 + 5) as u8;
        }

        for base in bus_probe_addresses() {
            // Unaligned offsets too: both accessors rotate, and the rotation
            // must be applied to the same assembled value either way.
            for delta in 0..4u32 {
                let addr = base.wrapping_add(delta);

                let a = u32::from(mmu.read_byte(addr & !1));
                let b = u32::from(mmu.read_byte((addr & !1) + 1));
                let want16 = (a | (b << 8)) as u16;
                let want16 = if addr & 1 != 0 {
                    (want16 >> 8) | (want16 << 8)
                } else {
                    want16
                };
                assert_eq!(
                    mmu.read_halfword(addr),
                    want16,
                    "read_halfword({addr:#010X}) disagrees with the byte path"
                );

                let aligned = addr & !3;
                let want32 = (0..4u32)
                    .map(|i| u32::from(mmu.read_byte(aligned + i)) << (i * 8))
                    .fold(0u32, |acc, x| acc | x);
                let rot = (addr & 3) * 8;
                let want32 = if rot > 0 {
                    (want32 >> rot) | (want32 << (32 - rot))
                } else {
                    want32
                };
                assert_eq!(
                    mmu.read_word(addr),
                    want32,
                    "read_word({addr:#010X}) disagrees with the byte path"
                );
            }
        }
    }

    /// Every 16- and 32-bit WRITE must leave memory exactly as the pre-fast-path
    /// implementation did.
    ///
    /// The reference here is deliberately NOT `write_byte`: palette, VRAM and
    /// OAM sit on a 16-bit bus where a byte write duplicates across the halfword,
    /// which is why `write_halfword` had explicit arms for them in the first
    /// place. The reference is that a 32-bit store equals two 16-bit stores, and
    /// that a 16-bit store lands its two bytes verbatim wherever a halfword
    /// store is legal at all — read back through `read_byte`, which the test
    /// above independently pins.
    #[test]
    fn word_writes_match_the_halfword_path_everywhere() {
        for base in bus_probe_addresses() {
            let mut fast = GbaMmu::new(vec![0xFF; 4096]);
            let mut slow = GbaMmu::new(vec![0xFF; 4096]);
            let addr = base & !3;

            fast.write_word(addr, 0xAABB_CCDD);
            // The pre-change decomposition, byte for byte.
            slow.write_halfword(addr, 0xCCDD);
            slow.write_halfword(addr + 2, 0xAABB);

            for i in 0..4u32 {
                assert_eq!(
                    fast.read_byte(addr + i),
                    slow.read_byte(addr + i),
                    "write_word({addr:#010X}) byte {i} differs from two halfword writes"
                );
            }
            // I/O writes have side effects; the fast path must not have taken
            // them, so the flush flag has to agree as well.
            assert_eq!(
                fast.io_dirty, slow.io_dirty,
                "write_word({addr:#010X}) disagrees on io_dirty"
            );
        }
    }

    // The BIOS hands the cartridge a display in Forced Blank (DISPCNT bit 7). Games
    // rely on it so their immediate REG_DISPSTAT writes reach hardware before the
    // first VBlank; without it Emerald hangs in WaitForVBlank.
    #[test]
    fn dispcnt_forced_blank_at_boot_and_after_ram_reset() {
        let mut mmu = GbaMmu::new(vec![]);
        assert_eq!(mmu.read_halfword_safe(0x04000000) & 0x0080, 0x0080);
        mmu.io[0] = 0; // game turns the display on, then calls RegisterRamReset
        mmu.reset_other_io_registers();
        assert_eq!(mmu.io[0] & 0x80, 0x80, "RegisterRamReset must re-blank the display");
    }

    // RegisterRamReset clears IWRAM but must preserve the top 0x200 bytes
    // (interrupt vector, stacks, BIOS interrupt flags).
    #[test]
    fn ram_reset_preserves_iwram_top() {
        let mut mmu = GbaMmu::new(vec![]);
        mmu.iwram[0x0100] = 0xAB;
        mmu.iwram[0x7FFC] = 0xCD; // 0x03007FFC = INTR_VECTOR
        mmu.clear_iwram_safe();
        assert_eq!(mmu.iwram[0x0100], 0, "low IWRAM should be cleared");
        assert_eq!(mmu.iwram[0x7FFC], 0xCD, "top 0x200 must be preserved");
    }

    // A repeat VBlank-timed DMA must fire once per VBlank edge, not every cycle the
    // flag is high. Here the VBlank flag stays high across many process_dmas() calls
    // yet only a single 4-halfword transfer (dest += 8) is performed.
    #[test]
    fn vblank_dma_fires_once_per_edge() {
        let mut mmu = GbaMmu::new(vec![]);
        mmu.dma.channels[0].sad = 0x0200_0000;
        mmu.dma.channels[0].dad = 0x0200_0100;
        mmu.dma.channels[0].count = 4;
        // control = enable | repeat | VBlank timing (bits 12-13 = 01), 16-bit, inc/inc.
        mmu.dma.channels[0].write_control(0, 0x00);
        mmu.dma.channels[0].write_control(1, 0x92); // 0x9200
        assert!(mmu.dma.channels[0].active);

        mmu.io[4] = 0x01; // DISPSTAT VBlank flag high (stays high all of VBlank)
        for _ in 0..8 {
            mmu.process_dmas();
        }
        assert_eq!(
            mmu.dma.channels[0].cur_dest, 0x0200_0108,
            "one VBlank edge = one 4-halfword transfer (dest += 8)"
        );

        // Next frame: flag drops then rises again -> a second edge, second transfer.
        mmu.io[4] = 0x00;
        mmu.process_dmas();
        mmu.io[4] = 0x01;
        mmu.process_dmas();
        assert_eq!(mmu.dma.channels[0].cur_dest, 0x0200_0110, "second edge transfers again");
    }

    // process_dmas() has an idle fast-path that early-returns when no channel is active.
    // It MUST still latch the VBlank/HBlank edge state on every call, so a DMA enabled
    // mid-VBlank does not see a stale (spurious) rising edge and fire a frame early.
    #[test]
    fn idle_dma_fastpath_keeps_edge_state_fresh() {
        let mut mmu = GbaMmu::new(vec![]);
        // No channel active: drive a full VBlank while idle (fast-path taken each call).
        mmu.io[4] = 0x00;
        mmu.process_dmas();
        mmu.io[4] = 0x01; // VBlank rises while all channels are idle
        mmu.process_dmas();
        mmu.process_dmas(); // still high, still idle
        assert!(mmu.dma_prev_vblank, "idle fast-path must still latch the VBlank flag");

        // Now enable a VBlank-timed DMA while VBlank is STILL high (mid-blank enable).
        mmu.dma.channels[0].sad = 0x0200_0000;
        mmu.dma.channels[0].dad = 0x0200_0100;
        mmu.dma.channels[0].count = 4;
        mmu.dma.channels[0].write_control(0, 0x00);
        mmu.dma.channels[0].write_control(1, 0x92); // enable | repeat | VBlank timing
        assert!(mmu.dma.channels[0].active);

        // io[4] still 0x01 (no fresh 0->1 edge), so the DMA must NOT fire this call.
        mmu.process_dmas();
        assert_eq!(
            mmu.dma.channels[0].cur_dest, 0x0200_0100,
            "no spurious fire: dma_prev_vblank was kept true during the idle period"
        );

        // A genuine new edge (low then high) fires it exactly once.
        mmu.io[4] = 0x00;
        mmu.process_dmas();
        mmu.io[4] = 0x01;
        mmu.process_dmas();
        assert_eq!(mmu.dma.channels[0].cur_dest, 0x0200_0108, "fresh VBlank edge transfers once");
    }

    // A timer's counter is latched from its reload value only on the 0->1 enable edge.
    // Re-writing TMxCNT_H while the timer is already enabled (to change prescaler/IRQ)
    // must NOT re-latch the counter, else the DirectSound sample-rate phase resets.
    #[test]
    fn timer_counter_reloads_only_on_enable_edge() {
        let mut mmu = GbaMmu::new(vec![]);
        // Reload = 0x1000 (TM0CNT_L).
        mmu.write_byte(0x04000100, 0x00);
        mmu.write_byte(0x04000101, 0x10);
        // Enable timer 0 (TM0CNT_H bit7). 0->1 edge latches counter = reload.
        mmu.write_byte(0x04000102, 0x80);
        assert_eq!(mmu.timers[0].counter, 0x1000, "enable edge latches reload");

        // Simulate the counter having advanced, then re-write control with enable still set.
        mmu.timers[0].counter = 0x1234;
        mmu.write_byte(0x04000102, 0xC0); // enable still 1, also set IRQ bit
        assert_eq!(
            mmu.timers[0].counter, 0x1234,
            "re-writing control while enabled must not re-latch the counter"
        );

        // Disable then enable again -> a real edge -> re-latch.
        mmu.write_byte(0x04000102, 0x00);
        mmu.write_byte(0x04000102, 0x80);
        assert_eq!(mmu.timers[0].counter, 0x1000, "fresh enable edge re-latches reload");
    }

    // Sound-FIFO DMA must hold its destination FIXED at the FIFO port even when the ROM
    // programs DEST=increment (which MP2K/"Sappy" does: control 0xB600). Regression guard
    // for the total-audio-silence bug: honoring dest_ctrl walked cur_dest off 0x040000A0
    // after the first word, starving the FIFO (silence) and corrupting the DMA registers.
    // A batch step is clamped to end exactly on the FIFO timer's overflow; the APU
    // must render those cycles with the sample latched BEFORE the pop (the popped
    // sample takes effect at the overflow cycle, i.e. from the next step on).
    // Rendering the pop retroactively over the step shifts every DirectSound sample
    // transition earlier by a variable batch length — audible phase jitter. This
    // pins the apu-before-timers order in tick_system_components.
    #[test]
    fn ds_sample_transition_lands_on_overflow() {
        let mut mmu = GbaMmu::new(vec![]);
        let mut ppu = crate::gba::ppu::GbaPpu::new();
        let mut video: [u16; 0] = [];
        let mut audio: [i16; 0] = [];

        // DS A -> both sides, 100% volume, sourced from timer 0 (bit 10 clear).
        mmu.apu.soundcnt_h = 0x0200 | 0x0100 | 0x0004;
        mmu.apu.current_sample_a = 100; // old latch
        mmu.apu.fifo_a.push(50); // next sample, pops at the overflow

        // Timer 0: prescaler 1, overflow in exactly 16 cycles.
        mmu.timers[0].counter = 0xFFF0;
        mmu.timers[0].reload = 0xFFF0;
        mmu.timers[0].control = 0x0080;

        mmu.tick_system_components(16, &mut video, &mut audio, 0, 1.0, &mut ppu, false);

        assert_eq!(mmu.apu.current_sample_a, 50, "overflow must latch the new sample");
        // All 16 cycles render the OLD latch: (100/128) * DS_GAIN(0.5) per cycle.
        // 16 < cycles_per_sample (~380), so no sample was emitted and the raw
        // integration is still inspectable in left_sum.
        let expected = 16.0 * (100.0 / 128.0) * 0.5;
        let got = mmu.apu.resampler.left_sum;
        assert!(
            (got - expected).abs() < 1e-6,
            "step audio must integrate the pre-overflow latch: got {got}, expected {expected}"
        );
    }

    #[test]
    fn fifo_dma_holds_destination_fixed() {
        let mut mmu = GbaMmu::new(vec![]);
        // Source: 4 words = 16 signed bytes (1..=16) in EWRAM.
        for i in 0..16u8 {
            mmu.ewram[i as usize] = i + 1;
        }
        let ch = 1;
        mmu.dma.channels[ch].sad = 0x0200_0000;
        mmu.dma.channels[ch].dad = 0x0400_00A0; // FIFO A port
        mmu.dma.channels[ch].count = 4;
        // enable | special/FIFO timing (bits 12-13 = 11) | 32-bit | repeat |
        // DEST increment (dest_ctrl = 0) | SRC increment  ==  0xB600 (as observed).
        mmu.dma.channels[ch].write_control(0, 0x00);
        mmu.dma.channels[ch].write_control(1, 0xB6);
        assert!(mmu.dma.channels[ch].active);
        assert_eq!(mmu.dma.channels[ch].cur_dest, 0x0400_00A0);

        mmu.apu.dma_request_a = true; // Direct Sound A asks for a refill
        mmu.process_dmas();

        assert_eq!(
            mmu.dma.channels[ch].cur_dest, 0x0400_00A0,
            "FIFO DMA destination must stay fixed at the FIFO port, not walk"
        );
        assert_eq!(mmu.apu.fifo_a.count, 16, "all 4 words (16 bytes) must land in FIFO A");
        for expected in 1..=16i8 {
            assert_eq!(
                mmu.apu.fifo_a.pop(),
                Some(expected),
                "FIFO bytes must arrive in order"
            );
        }
        assert_eq!(
            mmu.dma.channels[ch].cur_src, 0x0200_0010,
            "source address still advances by 16 bytes"
        );
    }

    #[test]
    fn fifo_dma_routes_by_destination_not_channel_number() {
        // Hardware picks the FIFO by DAD: a game may legitimately point DMA1 at
        // FIFO B. The old channel-number routing never consumed the B request on
        // DMA1, starving that FIFO into a permanently held (flat) latch.
        let mut mmu = GbaMmu::new(vec![]);
        for i in 0..16u8 {
            mmu.ewram[i as usize] = i + 1;
        }
        let ch = 1;
        mmu.dma.channels[ch].sad = 0x0200_0000;
        mmu.dma.channels[ch].dad = 0x0400_00A4; // FIFO B port on DMA channel 1
        mmu.dma.channels[ch].count = 4;
        mmu.dma.channels[ch].write_control(0, 0x00);
        mmu.dma.channels[ch].write_control(1, 0xB6); // enable | special | 32-bit | repeat

        // An A request must NOT trigger this channel (its DAD is FIFO B).
        mmu.apu.dma_request_a = true;
        mmu.process_dmas();
        assert!(mmu.apu.dma_request_a, "A request must stay pending — no A-dest channel");
        assert_eq!(mmu.apu.fifo_b.count, 0, "no transfer without a matching request");

        // A B request triggers it and lands the bytes in FIFO B.
        mmu.apu.dma_request_a = false;
        mmu.apu.dma_request_b = true;
        mmu.process_dmas();
        assert!(!mmu.apu.dma_request_b, "B request consumed by the B-dest channel");
        assert_eq!(mmu.apu.fifo_b.count, 16, "all 4 words must land in FIFO B");
        assert_eq!(mmu.apu.fifo_a.count, 0, "FIFO A untouched");
    }

    #[test]
    fn test_timer_change_triggers_apu_reset() {
        let mut mmu = GbaMmu::new(vec![]);
        // Initialize APU interpolation state
        mmu.apu.cycles_since_overflow_a = 100;
        mmu.apu.overflow_period_a = 200;
        mmu.apu.cycles_since_overflow_b = 150;
        mmu.apu.overflow_period_b = 300;

        // Configure DS B to use Timer 1 (set soundcnt_h to 0x4000) so it does not reset on Timer 0 reload write
        mmu.apu.soundcnt_h = 0x4000;

        // 1. Write to reload of Timer 0 (0x04000100)
        // Since DirectSound A uses Timer 0 by default, this should reset DS A interpolation state.
        mmu.write_byte(0x04000100, 0x55);
        assert_eq!(mmu.apu.overflow_period_a, 0, "DS A interpolation should reset on reload write");
        assert_eq!(mmu.apu.overflow_period_b, 300, "DS B should not reset");

        // Reset APU states and restore soundcnt_h to 0
        mmu.apu.overflow_period_a = 200;
        mmu.apu.overflow_period_b = 300;
        mmu.apu.soundcnt_h = 0;

        // 2. Write to reload of Timer 1 (0x04000104)
        // Since soundcnt_h is 0, DS A uses Timer 0 and DS B uses Timer 0. Neither uses Timer 1.
        mmu.write_byte(0x04000104, 0x66);
        assert_eq!(mmu.apu.overflow_period_a, 200);
        assert_eq!(mmu.apu.overflow_period_b, 300);

        // Configure DS B to use Timer 1 (bit 14 of soundcnt_h = 0x4000)
        mmu.apu.soundcnt_h = 0x4000;
        // Now writing to Timer 1 reload should reset DS B
        mmu.write_byte(0x04000104, 0x77);
        assert_eq!(mmu.apu.overflow_period_a, 200);
        assert_eq!(mmu.apu.overflow_period_b, 0, "DS B interpolation should reset on reload write");

        // Reset APU states
        mmu.apu.overflow_period_b = 300;

        // 3. Check transitions of control register
        // Currently Timer 0 is disabled (control is 0).
        // Transition from disabled to enabled (write 0x80 to 0x04000102)
        // Since DS A is set to Timer 0, it should trigger reset.
        mmu.write_byte(0x04000102, 0x80);
        assert_eq!(mmu.apu.overflow_period_a, 0, "DS A should reset on disabled->enabled transition");

        // Reset APU states
        mmu.apu.overflow_period_a = 200;

        // Re-write control with enable still set (no transition)
        mmu.write_byte(0x04000102, 0xC0);
        assert_eq!(mmu.apu.overflow_period_a, 200, "no transition, should not reset");

        // Transition from enabled to disabled, then disabled to enabled again
        mmu.write_byte(0x04000102, 0x00);
        mmu.write_byte(0x04000102, 0x80);
        assert_eq!(mmu.apu.overflow_period_a, 0, "transitioned again, should reset");

        // 4. Re-writing the SAME reload value must NOT reset: sound drivers rewrite
        // the reload defensively, and a per-write reset degrades the DS ramp to ZOH
        // repeatedly (audible click train). Timer 0 reload low byte is 0x55 (step 1).
        mmu.apu.overflow_period_a = 200;
        mmu.write_byte(0x04000100, 0x55);
        assert_eq!(
            mmu.apu.overflow_period_a, 200,
            "same-value reload write must not reset interpolation"
        );
        mmu.write_byte(0x04000100, 0x56); // value actually changes
        assert_eq!(
            mmu.apu.overflow_period_a, 0,
            "changed reload write must reset interpolation"
        );
    }

    #[test]
    fn test_directsound_interpolation_adversarial_edge_cases() {
        let mut mmu = GbaMmu::new(vec![]);
        // DS A uses Timer 0, DS B uses Timer 0 by default.

        // Scenario 1: Timer disabled -> enable transition
        mmu.apu.cycles_since_overflow_a = 100;
        mmu.apu.overflow_period_a = 200;
        // Enable Timer 0 (was disabled by default)
        mmu.write_byte(0x04000102, 0x80);
        assert_eq!(mmu.apu.overflow_period_a, 0, "Enabling disabled timer should reset interpolation");

        // Scenario 2: Timer enabled -> disable transition
        mmu.apu.cycles_since_overflow_a = 100;
        mmu.apu.overflow_period_a = 200;
        // Disable Timer 0
        mmu.write_byte(0x04000102, 0x00);
        assert_eq!(mmu.apu.overflow_period_a, 200, "Disabling enabled timer should NOT reset interpolation (allows smooth ramp to complete)");

        // Scenario 3: Reload modified while disabled vs enabled
        // 3a. Modify reload while disabled
        mmu.apu.cycles_since_overflow_a = 100;
        mmu.apu.overflow_period_a = 200;
        mmu.write_byte(0x04000100, 0xAA); // write low byte of reload
        assert_eq!(mmu.apu.overflow_period_a, 0, "Modifying reload while disabled should reset interpolation");

        // 3b. Modify reload while enabled
        // First enable timer
        mmu.write_byte(0x04000102, 0x80);
        mmu.apu.cycles_since_overflow_a = 100;
        mmu.apu.overflow_period_a = 200;
        mmu.write_byte(0x04000101, 0xBB); // write high byte of reload
        assert_eq!(mmu.apu.overflow_period_a, 0, "Modifying reload while enabled should reset interpolation");

        // Scenario 4: Prescaler modification while enabled (timer not transitioned to enabled)
        mmu.write_byte(0x04000102, 0x80); // Ensure enabled, prescaler 0
        mmu.apu.cycles_since_overflow_a = 100;
        mmu.apu.overflow_period_a = 200;
        // Write control register changing prescaler to 1, but keeping enable set
        mmu.write_byte(0x04000102, 0x81);
        assert_eq!(mmu.apu.overflow_period_a, 200, "Changing prescaler/IRQ on already-enabled timer should NOT reset interpolation immediately");

        // Scenario 5: Repeated toggle
        for _ in 0..5 {
            // Disable
            mmu.write_byte(0x04000102, 0x00);
            assert_eq!(mmu.apu.overflow_period_a, 200, "Disabling should not reset");
            
            // Enable
            mmu.write_byte(0x04000102, 0x80);
            assert_eq!(mmu.apu.overflow_period_a, 0, "Enabling should reset");
            
            // Re-set mock period
            mmu.apu.overflow_period_a = 200;
        }
    }
}

