// core/src/nds/cpu.rs
//
// The NDS has two ARM cores: an ARM946E-S (ARM9, ARMv5TE) and an ARM7TDMI
// (ARM7, ARMv4T). Both reuse the shared ARM interpreter in `gba::cpu` (a full
// ARM7TDMI) via the `CpuBus` trait, so there is a single source of truth for
// ARM/Thumb decoding. Each core here only owns what the shared interpreter does
// NOT: the per-core memory-map adapter, the NDS interrupt model (field-based
// IE/IF/IME and NDS exception vectors), and — for the ARM9 — the CP15
// coprocessor that controls the TCMs. ARMv5-only opcodes for the ARM9 are added
// in a later milestone.

use crate::cpu_bus::CpuBus;
use crate::gba::cpu::{CpuMode, GbaCpu, SwiMode};
use crate::nds::mmu::NdsMmu;

pub const FLAG_I: u32 = 1 << 7;
pub const FLAG_T: u32 = 1 << 5;

#[derive(Clone, Copy, Debug, Default)]
pub struct Cp15Registers {
    pub control: u32,
    pub itcm_control: u32,
    pub dtcm_control: u32,
}

impl crate::snapshot::Snap for Cp15Registers {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.control.snap(v);
        self.itcm_control.snap(v);
        self.dtcm_control.snap(v);
    }
}

impl crate::snapshot::Snap for Arm9Cpu {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.cpu.snap(v);
        self.cp15.snap(v);
        // The recompiler is not machine state and is deliberately absent from
        // the field list above. But a *restore* replaces the whole memory image
        // without a single store passing through the MMU, so no page version
        // moves and every compiled block silently survives code it no longer
        // matches. Discard them here, where "a load just happened" is known.
        if v.loading() {
            if let Some(jit) = self.jit.as_mut() {
                jit.clear();
            }
        }
    }
}

impl crate::snapshot::Snap for Arm7Cpu {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.cpu.snap(v);
        // Same rule as the ARM9 impl above: a restore replaces memory without
        // one store passing through the MMU, so compiled blocks must not
        // survive it.
        if v.loading() {
            if let Some(jit) = self.jit.as_mut() {
                jit.clear();
            }
        }
    }
}

/// Presents the ARM9 view of NDS memory to the shared interpreter. A newtype over
/// `&mut NdsMmu`, so it is zero-cost after inlining.
pub struct Arm9Bus<'a>(pub &'a mut NdsMmu);

impl CpuBus for Arm9Bus<'_> {
    fn read_byte(&mut self, addr: u32) -> u8 {
        if self.0.tp_read_watch_on {
            self.0.tp_note_read(addr);
        }
        self.0.read_byte_arm9(addr)
    }
    fn read_halfword(&mut self, addr: u32) -> u16 {
        if self.0.tp_read_watch_on {
            self.0.tp_note_read(addr);
        }
        self.0.read_halfword_arm9(addr)
    }
    /// LDR word. An unaligned address does NOT fault on either DS core: the bus
    /// fetches the *aligned* word and the CPU rotates it right by
    /// `(addr & 3) * 8`, so `LDR r0,[0x02000001]` over `AABBCCDD` yields
    /// `DDAABBCC`. This lives in the bus adapter rather than in `read_word_arm*`
    /// because DMA, the boot HLE and the probes share those MMU entry points and
    /// must see the plain aligned word — rotating there would corrupt every
    /// block copy. The GBA path already rotates inside its own MMU.
    fn read_word(&mut self, addr: u32) -> u32 {
        if self.0.tp_read_watch_on {
            self.0.tp_note_read(addr);
        }
        // Side-effecting reads must route through the &mut MMU, not the shared &self
        // read path: the IPC receive FIFO pops a word (and can raise the sender's
        // send-empty IRQ), and the Gamecard data port advances the block cursor.
        match addr {
            0x0410_0000 => self.0.read_ipc_fifo_rx_arm9(),
            0x0410_0010 => self.0.gamecard_read_data(),
            _ => self.0.read_word_arm9(addr & !3).rotate_right((addr & 3) * 8),
        }
    }
    /// Instruction fetch. Everything [`Self::read_word`] does on top of the raw
    /// MMU read is data-read-only concern — the rotate (the caller aligned this
    /// address), the IPC-FIFO and gamecard ports (not executable) and the touch
    /// read watch (fetches are not data reads) — and this is the hottest path in
    /// the emulator, taken once per emulated instruction. See
    /// [`CpuBus::fetch_word`].
    fn fetch_word(&mut self, addr: u32) -> u32 {
        self.0.read_word_arm9(addr)
    }
    fn fetch_halfword(&mut self, addr: u32) -> u16 {
        self.0.read_halfword_arm9(addr)
    }
    /// Decode the region once for the whole LDM block. Declines (falls back to
    /// the default per-word loop) whenever the block is not a single contiguous
    /// run — I/O, VRAM, the shared-WRAM window, a straddled mirror — so no side
    /// effect is bypassed. See [`CpuBus::read_words`].
    fn read_words(&mut self, addr: u32, out: &mut [u32]) {
        let bytes = out.len() as u32 * 4;
        if let Some(src) = self.0.contiguous_arm9(addr, bytes) {
            for (w, b) in out.iter_mut().zip(src.chunks_exact(4)) {
                *w = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            }
            return;
        }
        for (i, w) in out.iter_mut().enumerate() {
            *w = self.read_word(addr.wrapping_add(i as u32 * 4));
        }
    }
    /// Write-side twin of [`Self::read_words`]. `contiguous_arm9_mut` already
    /// declines whenever a write watch is armed, so the diagnostics keep seeing
    /// every byte. See [`CpuBus::write_words`].
    fn write_words(&mut self, addr: u32, vals: &[u32]) {
        let bytes = vals.len() as u32 * 4;
        if let Some(dst) = self.0.contiguous_arm9_mut(addr, bytes) {
            for (d, v) in dst.chunks_exact_mut(4).zip(vals) {
                d.copy_from_slice(&v.to_le_bytes());
            }
            return;
        }
        for (i, v) in vals.iter().enumerate() {
            self.write_word(addr.wrapping_add(i as u32 * 4), *v);
        }
    }
    fn write_byte(&mut self, addr: u32, val: u8) {
        self.0.write_byte_arm9(addr, val);
    }
    fn write_halfword(&mut self, addr: u32, val: u16) {
        self.0.write_halfword_arm9(addr, val);
    }
    /// STR word. The low two address bits are ignored on both cores — the store
    /// lands on the aligned word — so an unaligned STR must not be allowed to
    /// straddle two words the way a byte-wise decomposition would.
    fn write_word(&mut self, addr: u32, val: u32) {
        // IPC send FIFO (0x04000188) is a 32-bit push with side effects (raises the
        // receiver's recv IRQ) — route the whole word to the FIFO, not byte-wise.
        if addr == 0x0400_0188 {
            self.0.write_ipc_fifo_tx_arm9(val);
            return;
        }
        self.0.write_word_arm9(addr & !3, val);
    }
}

/// Presents the ARM7 view of NDS memory to the shared interpreter.
pub struct Arm7Bus<'a>(pub &'a mut NdsMmu);

impl CpuBus for Arm7Bus<'_> {
    fn read_byte(&mut self, addr: u32) -> u8 {
        self.0.read_byte_arm7(addr)
    }
    fn read_halfword(&mut self, addr: u32) -> u16 {
        self.0.read_halfword_arm7(addr)
    }
    /// LDR word. An unaligned address does NOT fault on either DS core: the bus
    /// fetches the *aligned* word and the CPU rotates it right by
    /// `(addr & 3) * 8`, so `LDR r0,[0x02000001]` over `AABBCCDD` yields
    /// `DDAABBCC`. This lives in the bus adapter rather than in `read_word_arm*`
    /// because DMA, the boot HLE and the probes share those MMU entry points and
    /// must see the plain aligned word — rotating there would corrupt every
    /// block copy. The GBA path already rotates inside its own MMU.
    fn read_word(&mut self, addr: u32) -> u32 {
        // IPC receive FIFO pop (side-effecting) — route through the &mut MMU.
        if addr == 0x0410_0000 {
            return self.0.read_ipc_fifo_rx_arm7();
        }
        self.0.read_word_arm7(addr & !3).rotate_right((addr & 3) * 8)
    }
    /// ARM7 twin of [`Arm9Bus::fetch_word`].
    fn fetch_word(&mut self, addr: u32) -> u32 {
        self.0.read_word_arm7(addr)
    }
    fn fetch_halfword(&mut self, addr: u32) -> u16 {
        self.0.read_halfword_arm7(addr)
    }
    /// ARM7 twin of [`Arm9Bus::read_words`].
    fn read_words(&mut self, addr: u32, out: &mut [u32]) {
        let bytes = out.len() as u32 * 4;
        if let Some(src) = self.0.contiguous_arm7(addr, bytes) {
            for (w, b) in out.iter_mut().zip(src.chunks_exact(4)) {
                *w = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            }
            return;
        }
        for (i, w) in out.iter_mut().enumerate() {
            *w = self.read_word(addr.wrapping_add(i as u32 * 4));
        }
    }
    /// ARM7 twin of [`Arm9Bus::write_words`].
    fn write_words(&mut self, addr: u32, vals: &[u32]) {
        let bytes = vals.len() as u32 * 4;
        if let Some(dst) = self.0.contiguous_arm7_mut(addr, bytes) {
            for (d, v) in dst.chunks_exact_mut(4).zip(vals) {
                d.copy_from_slice(&v.to_le_bytes());
            }
            return;
        }
        for (i, v) in vals.iter().enumerate() {
            self.write_word(addr.wrapping_add(i as u32 * 4), *v);
        }
    }
    fn write_byte(&mut self, addr: u32, val: u8) {
        self.0.write_byte_arm7(addr, val);
    }
    fn write_halfword(&mut self, addr: u32, val: u16) {
        self.0.write_halfword_arm7(addr, val);
    }
    fn write_word(&mut self, addr: u32, val: u32) {
        // IPC send FIFO (0x04000188) — 32-bit push, raises the ARM9 recv IRQ.
        if addr == 0x0400_0188 {
            self.0.write_ipc_fifo_tx_arm7(val);
            return;
        }
        self.0.write_word_arm7(addr & !3, val); // see the ARM9 note: STR ignores addr[1:0]
    }
}

pub struct Arm9Cpu {
    /// Shared ARM core: registers, pipeline and the ARM/Thumb interpreter.
    pub cpu: GbaCpu,
    pub cp15: Cp15Registers,
    /// Block recompiler, when one is enabled.
    ///
    /// `Option<Box<..>>` rather than a plain field: it is absent by default, so
    /// an ARM9 that never enables it carries one null pointer and the run loop
    /// pays one never-taken branch. Not machine state — see the [`Snap`] impl
    /// above for the restore hook that keeps it from outliving the memory it was
    /// compiled from.
    ///
    /// [`Snap`]: crate::snapshot::Snap
    pub(crate) jit: Option<Box<crate::jit::runner::Arm9Jit>>,
}

impl Arm9Cpu {
    pub fn new() -> Self {
        let mut cpu = GbaCpu::new();
        cpu.swi_mode = SwiMode::Nds;
        cpu.armv5 = true; // ARM946E-S
        let mut me = Self {
            cpu,
            cp15: Cp15Registers::default(),
            jit: None,
        };
        // **On by default** — measured +25-28% on NDS fast-forward with every
        // correctness gate bit-identical (boot-handshake frame hash, in-game
        // audio, 4000-seed differential corpora in both instruction sets).
        // `EMU_ARM9_JIT=0` restores the pure interpreter, which is how the
        // two are still measured alternately in one session.
        if crate::jit::runner::env_flag("EMU_ARM9_JIT", true) {
            me.set_jit_enabled(true);
        }
        me
    }

    /// Turn the block recompiler on or off. Turning it off discards every
    /// compiled block, so this is always safe to call mid-run.
    pub fn set_jit_enabled(&mut self, on: bool) {
        self.jit = if on { Some(Box::new(crate::jit::runner::Arm9Jit::new())) } else { None };
    }



    /// Diagnostics from the recompiler, if one is running.
    pub fn jit_stats(&self) -> Option<crate::jit::runner::JitStats> {
        self.jit.as_ref().map(|j| j.stats())
    }

    /// Successor links written and link teardowns, or `None` without a
    /// recompiler. See `Arm9Jit::link_stats`.
    pub fn jit_link_stats(&self) -> Option<(u64, u64)> {
        self.jit.as_ref().map(|j| j.link_stats())
    }

    /// Why chains ended, or `None` without a recompiler. Diagnostics-gated;
    /// see `Arm9Jit::chain_ends` for the slot meanings.
    pub fn jit_chain_end_counts(&self) -> Option<[u64; 5]> {
        self.jit.as_ref().map(|j| j.chain_end_counts())
    }

    /// Dispatch-table entries written, or `None` without a recompiler.
    pub fn jit_dispatch_stats(&self) -> Option<u64> {
        self.jit.as_ref().map(|j| j.dispatch_stats())
    }

    /// Why link/dispatch writes were refused, or `None` without a recompiler.
    /// Diagnostics-gated; see `Arm9Jit::link_refusals`.
    pub fn jit_link_refusal_counts(&self) -> Option<[u64; 4]> {
        self.jit.as_ref().map(|j| j.link_refusal_counts())
    }

    /// The most-refused uncompiled link targets. Diagnostics-gated; see
    /// `Arm9Jit::refused_target_top`.
    #[allow(clippy::type_complexity)]
    pub fn jit_refused_target_top(
        &self,
        n: usize,
    ) -> Option<Vec<(u32, u64, bool, bool, u32, Option<u32>)>> {
        self.jit.as_ref().map(|j| j.refused_target_top(n))
    }

    /// Emit dispatching exits, whatever the deployment default is. A no-op
    /// without a recompiler; implies linking and discards compiled blocks.
    pub fn set_jit_dispatch_enabled(&mut self, on: bool) {
        if let Some(jit) = self.jit.as_mut() {
            jit.set_dispatch_enabled(on);
        }
    }

    /// Compile successor-linked exits, whatever the deployment default is.
    /// A no-op without a recompiler; discards compiled blocks on a change.
    pub fn set_jit_link_enabled(&mut self, on: bool) {
        if let Some(jit) = self.jit.as_mut() {
            jit.set_link_enabled(on);
        }
    }

    /// Enable the recompiler's stop/exit accounting. For probes only — it is
    /// off by default because it is not free.
    pub fn set_jit_diagnostics(&mut self, on: bool) {
        if let Some(jit) = self.jit.as_mut() {
            jit.set_diagnostics(on);
        }
    }

    /// Why the recompiler stood down, per `StopReason`.
    pub fn jit_stop_counts(&self) -> Option<[u64; 10]> {
        self.jit.as_ref().map(|j| j.stop_counts())
    }

    /// Address-filter evictions and the filter's size, for the probe that
    /// decides whether the filter is big enough.
    pub fn jit_hot_filter_stats(&self) -> Option<(u64, usize)> {
        self.jit.as_ref().map(|j| j.hot_filter_stats())
    }

    /// Why built blocks ended, per scanner `ExitReason`.
    pub fn jit_exit_counts(&self) -> Option<[u64; 14]> {
        self.jit.as_ref().map(|j| j.exit_counts())
    }

    /// The same, restricted to scans that produced no body at all — the
    /// encodings that are blocking coverage rather than merely ending blocks.
    pub fn jit_empty_exit_counts(&self) -> Option<[u64; 14]> {
        self.jit.as_ref().map(|j| j.empty_exit_counts())
    }

    /// Block exits weighted by entries rather than by scans — the distribution
    /// that says which terminator is actually costing block entries.
    /// Successor edges, and the subset an emitted chain could have taken.
    /// See `Arm9Jit::chain_edges`.
    pub fn jit_chain_stats(&self) -> Option<(u64, u64)> {
        self.jit.as_ref().map(|j| j.chain_stats())
    }

    /// Why the unlinkable edges were unlinkable; see `Arm9Jit::chain_lost`.
    pub fn jit_chain_loss_counts(&self) -> Option<[u64; 12]> {
        self.jit.as_ref().map(|j| j.chain_loss_counts())
    }

    pub fn jit_entry_exit_counts(&self) -> Option<[u64; 14]> {
        self.jit.as_ref().map(|j| j.entry_exit_counts())
    }

    pub fn reset(&mut self, mmu: &mut NdsMmu) {
        self.cpu.reset();
        self.cpu.swi_mode = SwiMode::Nds;
        self.cpu.armv5 = true; // ARM946E-S
        self.cp15 = Cp15Registers::default();
        mmu.set_cp15(self.cp15);
    }

    pub fn flush_pipeline(&mut self, mmu: &mut NdsMmu) {
        let mut bus = Arm9Bus(mmu);
        self.cpu.flush_pipeline(&mut bus);
    }

    pub fn step(&mut self, mmu: &mut NdsMmu) -> u32 {
        if self.cpu.halted {
            // GBATEK "Halt": the CPU leaves low-power when an enabled interrupt
            // is REQUESTED (IE & IF != 0), regardless of IME and the CPSR I-bit;
            // IME/I only gate whether the IRQ is then taken. Requiring IME here
            // would deadlock WFI loops that run inside critical sections.
            if (mmu.arm9_ie & mmu.arm9_if) != 0 {
                self.cpu.halted = false;
            }
            mmu.arm9_halt_cycles = mmu.arm9_halt_cycles.wrapping_add(1);
            return 1;
        }

        // Normalise the pipeline before deriving the IRQ return link from gpr[15].
        if self.cpu.pc_modified {
            self.flush_pipeline(mmu);
        }

        // Service a pending, enabled IRQ before fetching the next instruction.
        if (mmu.arm9_ime & 1) != 0
            && !self.cpu.registers.get_flag(FLAG_I)
            && (mmu.arm9_ie & mmu.arm9_if) != 0
        {
            mmu.arm9_irqs_taken = mmu.arm9_irqs_taken.wrapping_add(1);
            self.trigger_irq();
            return 4;
        }

        let is_thumb = self.cpu.registers.get_flag(FLAG_T);
        let instr_size = if is_thumb { 2 } else { 4 };

        let inst = self.cpu.pipeline[0];
        self.cpu.pipeline[0] = self.cpu.pipeline[1];

        let fetch_pc = self.cpu.registers.gpr[15];
        // Publish the address of the instruction about to run. R15 leads by the
        // pipeline stage plus the prefetch, so the executing address is two
        // instruction widths back. The MMU's write paths use this to attribute a
        // register write to the game function that made it.
        //
        // Refuted, do not re-attempt: gating these two stores behind
        // `NdsMmu::pc_watch_armed` (they serve watches disarmed in every
        // non-diagnostic run) measured **0%** — cpu 5.35 ms/frame and
        // 25.3 ns/instr, both unchanged. They hit an already-hot cache line.
        mmu.arm9_exec_pc = fetch_pc.wrapping_sub(2 * instr_size as u32);
        // LR too: the instruction that writes a register is usually inside a
        // shared helper (a display-list blitter, a memcpy), so the return address
        // is what names the *caller* worth disassembling.
        mmu.arm9_exec_lr = self.cpu.registers.gpr[14];
        {
            let mut bus = Arm9Bus(mmu);
            self.cpu.pipeline[1] = if is_thumb {
                bus.read_halfword(fetch_pc & !1) as u32
            } else {
                bus.read_word(fetch_pc & !3)
            };
        }

        // The shared ARMv4 core does not implement the CP15 coprocessor the ARM9
        // uses to configure its TCMs/caches, so intercept MCR/MRC p15 here and
        // advance like any non-branch instruction.
        // ponytail: condition code is not re-checked (ARM9 CP15 setup is
        // unconditional); add a check_condition guard if a game issues a
        // predicated MCR.
        if !is_thumb && Self::is_cp15_transfer(inst) {
            self.execute_cp15(mmu, inst);
            self.cpu.registers.gpr[15] = self.cpu.registers.gpr[15].wrapping_add(instr_size);
            return 1;
        }

        let cycles = {
            let mut bus = Arm9Bus(mmu);
            if is_thumb {
                self.cpu.execute_thumb(inst as u16, &mut bus)
            } else {
                self.cpu.execute_arm(inst, &mut bus)
            }
        };

        if !self.cpu.pc_modified {
            self.cpu.registers.gpr[15] = self.cpu.registers.gpr[15].wrapping_add(instr_size);
        }

        cycles
    }

    /// Run this core for `budget` bus cycles, returning the cycles consumed
    /// (which may overshoot by the last instruction's cost, exactly as the
    /// `step` loop it replaces did).
    ///
    /// A core that is halted for the whole window is charged in one go instead
    /// of one `step` per cycle. That is an identity, not an approximation:
    /// nothing inside the window can raise an interrupt, because the timers,
    /// the PPU, the APU and the *other* core all tick after it in
    /// `Emulator::tick`. Measured on the SoulSilver overworld the ARM9 idles
    /// 46% of its budget and the ARM7 80% of its, so the old loop paid about
    /// 483,000 no-op calls per frame.
    pub fn run(&mut self, mmu: &mut NdsMmu, budget: u32) -> u32 {
        let mut used = 0;
        while used < budget {
            let was_halted = self.cpu.halted;
            // The remaining slice is the recompiler's chain budget: a linked
            // chain checks it at every block exit, so it stands down exactly
            // where this loop's own `used < budget` would have.
            used += self.step_or_block(mmu, budget - used);
            if was_halted && self.cpu.halted {
                let idle = budget.saturating_sub(used);
                mmu.arm9_halt_cycles = mmu.arm9_halt_cycles.wrapping_add(u64::from(idle));
                return budget;
            }
            // Pinged the ARM7 over IPCSYNC: give it the bus now rather than up
            // to a slice later. See `NdsMmu::ipc_yield`.
            if mmu.ipc_yield {
                mmu.ipc_yield = false;
                break;
            }
        }
        used
    }

    /// Run one compiled block if the recompiler has one, otherwise interpret a
    /// single instruction. The body is the core-generic
    /// [`crate::jit::runner::step_or_block`], shared verbatim with the ARM7.
    #[inline]
    fn step_or_block(&mut self, mmu: &mut NdsMmu, budget: u32) -> u32 {
        crate::jit::runner::step_or_block::<crate::jit::runner::Arm9Core>(self, mmu, budget)
    }

    /// True for an `MCR`/`MRC` targeting coprocessor 15 (bits 27-24 = `1110`,
    /// coproc field = 15, bit 4 = 1 = register transfer).
    fn is_cp15_transfer(inst: u32) -> bool {
        ((inst >> 24) & 0xF) == 0xE && ((inst >> 8) & 0xF) == 0xF && (inst & 0x10) != 0
    }

    fn execute_cp15(&mut self, mmu: &mut NdsMmu, inst: u32) {
        let mcr = ((inst >> 20) & 1) == 0;
        let crn = ((inst >> 16) & 0xF) as u8;
        let rd = ((inst >> 12) & 0xF) as usize;
        let crm = (inst & 0xF) as u8;
        let opcode_2 = ((inst >> 5) & 0x7) as u8;

        if rd < 15 {
            let mut rd_val = self.cpu.registers.gpr[rd];
            self.execute_cp15_transfer(mmu, mcr, crn, crm, opcode_2, &mut rd_val);
            if !mcr {
                self.cpu.registers.gpr[rd] = rd_val;
            }
        }
    }

    pub fn execute_cp15_transfer(
        &mut self,
        mmu: &mut NdsMmu,
        mcr: bool,
        crn: u8,
        crm: u8,
        opcode_2: u8,
        rd_val: &mut u32,
    ) {
        match (crn, crm, opcode_2) {
            // Control Register (c1, c0, 0). `self.cp15` is this core's
            // authoritative copy; the MMU mirrors it for address routing, and
            // `set_cp15` is what re-derives the TCM windows from it — so every
            // MCR arm below updates the field and then re-publishes the whole
            // set, rather than mirroring field by field.
            (1, 0, 0) => {
                if mcr {
                    self.cp15.control = *rd_val;
                    mmu.set_cp15(self.cp15);
                } else {
                    *rd_val = self.cp15.control;
                }
            }
            // DTCM Control Register (c9, c1, 0)
            (9, 1, 0) => {
                if mcr {
                    self.cp15.dtcm_control = *rd_val;
                    mmu.set_cp15(self.cp15);
                    // The HLE IRQ handler dispatches through [DTCM+0x3FFC]; keep its
                    // embedded literal pointing at the new DTCM base.
                    mmu.update_arm9_irq_handler_ptr();
                } else {
                    *rd_val = self.cp15.dtcm_control;
                }
            }
            // ITCM Control Register (c9, c1, 1)
            (9, 1, 1) => {
                if mcr {
                    self.cp15.itcm_control = *rd_val;
                    mmu.set_cp15(self.cp15);
                } else {
                    *rd_val = self.cp15.itcm_control;
                }
            }
            // Wait For Interrupt (c7, c0, 4) — ARM946E-S low-power halt. The
            // NitroSDK idle thread runs `MCR p15,0,rX,c7,c0,4` in a loop; without
            // this the ARM9 busy-spins, and a thread blocked on an IPC reply can
            // never be preempted into. Wake handling lives in `step` (IE & IF).
            (7, 0, 4) => {
                if mcr {
                    self.cpu.halted = true;
                }
            }
            _ => {}
        }
    }

    fn trigger_irq(&mut self) {
        let regs = &mut self.cpu.registers;
        let old_cpsr = regs.cpsr;
        let old_mode = regs.get_mode();
        regs.swap_mode(old_mode, CpuMode::Irq);
        regs.spsr = old_cpsr;

        let is_thumb = (old_cpsr & FLAG_T) != 0;
        let return_link = if is_thumb {
            regs.gpr[15]
        } else {
            regs.gpr[15].wrapping_sub(4)
        };
        regs.gpr[14] = return_link;

        regs.set_flag(FLAG_T, false);
        regs.set_flag(FLAG_I, true);

        // CP15 control bit 13 selects high vectors (0xFFFF0000) or low (0x0).
        let vector = if (self.cp15.control & (1 << 13)) != 0 {
            0xFFFF_0018
        } else {
            0x0000_0018
        };
        regs.gpr[15] = vector;
        self.cpu.pc_modified = true;
    }
}

pub struct Arm7Cpu {
    /// Shared ARM core: registers, pipeline and the ARM/Thumb interpreter.
    pub cpu: GbaCpu,
    /// Block recompiler, when one is enabled — the ARM7 instantiation of the
    /// same machinery the ARM9 runs. Same `Option<Box<..>>` reasoning and the
    /// same restore hook; see [`Arm9Cpu::jit`].
    pub(crate) jit: Option<Box<crate::jit::runner::Arm7Jit>>,
}

impl Arm7Cpu {
    pub fn new() -> Self {
        let mut cpu = GbaCpu::new();
        cpu.swi_mode = SwiMode::Nds;
        let mut me = Self { cpu, jit: None };
        // **On by default** — measured +5.4% @5x / +4.6% @4x (3 alternating
        // pairs, GBA control steady) with every gate bit-identical: the boot
        // frame-hash oracle, the in-game audio identity, a 4000-tick
        // register-exact lockstep against the interpreter, and 4000-seed
        // differential corpora. Runs in exact-slice, blocks-only mode (see
        // `JitCore::EXACT_SLICES`); `EMU_ARM7_JIT=0` restores the pure
        // interpreter for A/B.
        if crate::jit::runner::env_flag("EMU_ARM7_JIT", true) {
            me.set_jit_enabled(true);
        }
        me
    }

    /// Turn the block recompiler on or off. Turning it off discards every
    /// compiled block, so this is always safe to call mid-run.
    pub fn set_jit_enabled(&mut self, on: bool) {
        self.jit = if on { Some(Box::new(crate::jit::runner::Arm7Jit::new())) } else { None };
    }

    /// Diagnostics from the recompiler, if one is running.
    pub fn jit_stats(&self) -> Option<crate::jit::runner::JitStats> {
        self.jit.as_ref().map(|j| j.stats())
    }

    /// Enable the recompiler's stop/exit accounting. For probes only.
    pub fn set_jit_diagnostics(&mut self, on: bool) {
        if let Some(jit) = self.jit.as_mut() {
            jit.set_diagnostics(on);
        }
    }

    /// Why the recompiler stood down, per `StopReason`.
    pub fn jit_stop_counts(&self) -> Option<[u64; 10]> {
        self.jit.as_ref().map(|j| j.stop_counts())
    }

    /// Block exits weighted by entries; see [`Arm9Cpu::jit_entry_exit_counts`].
    pub fn jit_entry_exit_counts(&self) -> Option<[u64; 14]> {
        self.jit.as_ref().map(|j| j.entry_exit_counts())
    }

    /// Successor links written and teardowns; see [`Arm9Cpu::jit_link_stats`].
    pub fn jit_link_stats(&self) -> Option<(u64, u64)> {
        self.jit.as_ref().map(|j| j.link_stats())
    }

    /// Dispatch-table entries written; see [`Arm9Cpu::jit_dispatch_stats`].
    pub fn jit_dispatch_stats(&self) -> Option<u64> {
        self.jit.as_ref().map(|j| j.dispatch_stats())
    }

    /// Why chains ended; see [`Arm9Cpu::jit_chain_end_counts`].
    pub fn jit_chain_end_counts(&self) -> Option<[u64; 5]> {
        self.jit.as_ref().map(|j| j.chain_end_counts())
    }

    /// Guard revalidations that ran instead of retranslations.
    pub fn jit_revalidations(&self) -> Option<u64> {
        self.jit.as_ref().map(|j| j.revalidations)
    }

    /// The most-hit permanently-declined addresses; see `Jit::declined_top`.
    pub fn jit_declined_top(&self, n: usize) -> Option<Vec<(u32, u64, u32)>> {
        self.jit.as_ref().map(|j| j.declined_top(n))
    }

    pub fn reset(&mut self) {
        self.cpu.reset();
        self.cpu.swi_mode = SwiMode::Nds;
    }

    pub fn flush_pipeline(&mut self, mmu: &mut NdsMmu) {
        let mut bus = Arm7Bus(mmu);
        self.cpu.flush_pipeline(&mut bus);
    }

    pub fn step(&mut self, mmu: &mut NdsMmu) -> u32 {
        if self.cpu.halted {
            // Same wake rule as the ARM9: IE & IF alone ends the halt (GBATEK).
            if (mmu.arm7_ie & mmu.arm7_if) != 0 {
                self.cpu.halted = false;
            }
            mmu.arm7_halt_cycles = mmu.arm7_halt_cycles.wrapping_add(1);
            return 1;
        }

        if self.cpu.pc_modified {
            self.flush_pipeline(mmu);
        }

        if (mmu.arm7_ime & 1) != 0
            && !self.cpu.registers.get_flag(FLAG_I)
            && (mmu.arm7_ie & mmu.arm7_if) != 0
        {
            mmu.arm7_irqs_taken = mmu.arm7_irqs_taken.wrapping_add(1);
            self.trigger_irq();
            return 4;
        }

        let is_thumb = self.cpu.registers.get_flag(FLAG_T);
        let instr_size = if is_thumb { 2 } else { 4 };

        let inst = self.cpu.pipeline[0];
        self.cpu.pipeline[0] = self.cpu.pipeline[1];

        let fetch_pc = self.cpu.registers.gpr[15];
        {
            let mut bus = Arm7Bus(mmu);
            self.cpu.pipeline[1] = if is_thumb {
                bus.read_halfword(fetch_pc & !1) as u32
            } else {
                bus.read_word(fetch_pc & !3)
            };
        }

        let cycles = {
            let mut bus = Arm7Bus(mmu);
            if is_thumb {
                self.cpu.execute_thumb(inst as u16, &mut bus)
            } else {
                self.cpu.execute_arm(inst, &mut bus)
            }
        };

        if !self.cpu.pc_modified {
            self.cpu.registers.gpr[15] = self.cpu.registers.gpr[15].wrapping_add(instr_size);
        }

        cycles
    }

    /// ARM7 counterpart of [`Arm9Cpu::run`] — same identity, same reasoning.
    /// The remaining slice is the recompiler's chain budget, exactly as on
    /// the ARM9.
    pub fn run(&mut self, mmu: &mut NdsMmu, budget: u32) -> u32 {
        let mut used = 0;
        while used < budget {
            let was_halted = self.cpu.halted;
            used += crate::jit::runner::step_or_block::<crate::jit::runner::Arm7Core>(
                self,
                mmu,
                budget - used,
            );
            if was_halted && self.cpu.halted {
                let idle = budget.saturating_sub(used);
                mmu.arm7_halt_cycles = mmu.arm7_halt_cycles.wrapping_add(u64::from(idle));
                return budget;
            }
            // ARM9 twin; see `Arm9Cpu::run`.
            if mmu.ipc_yield {
                mmu.ipc_yield = false;
                break;
            }
        }
        used
    }

    fn trigger_irq(&mut self) {
        let regs = &mut self.cpu.registers;
        let old_cpsr = regs.cpsr;
        let old_mode = regs.get_mode();
        regs.swap_mode(old_mode, CpuMode::Irq);
        regs.spsr = old_cpsr;

        let is_thumb = (old_cpsr & FLAG_T) != 0;
        let return_link = if is_thumb {
            regs.gpr[15]
        } else {
            regs.gpr[15].wrapping_sub(4)
        };
        regs.gpr[14] = return_link;

        regs.set_flag(FLAG_T, false);
        regs.set_flag(FLAG_I, true);

        // ARM7TDMI has no CP15: exception vectors are always at the low base.
        regs.gpr[15] = 0x0000_0018;
        self.cpu.pc_modified = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nds::mmu::NdsMmu;

    /// Load `instrs` at 0x02000000 in Main RAM and start the ARM9 there in ARM
    /// System state with the pipeline primed.
    fn boot_arm9(instrs: &[u32]) -> (Arm9Cpu, NdsMmu) {
        let mut mmu = NdsMmu::new();
        for (i, &w) in instrs.iter().enumerate() {
            mmu.write_word_arm9(0x0200_0000 + (i as u32) * 4, w);
        }
        let mut cpu = Arm9Cpu::new();
        cpu.cpu.registers.cpsr = 0x1F; // System mode, ARM state
        cpu.cpu.registers.gpr[15] = 0x0200_0000;
        cpu.flush_pipeline(&mut mmu);
        (cpu, mmu)
    }

    /// `run` is an optimization, so it must be an identity: for a core that is
    /// executing, it has to leave exactly the state the `step` loop it replaced
    /// left, and consume exactly as many cycles.
    #[test]
    fn run_matches_the_step_loop_it_replaced() {
        // A loop with a memory write, so divergence shows in both registers and
        // RAM: MOV r0,#0 ; MOV r1,#0x02000000 ; ADD r0,r0,#1 ; STR r0,[r1,#0x40]
        // ; B back to the ADD.
        const PROG: [u32; 5] = [
            0xE3A0_0000,
            0xE3A0_1402,
            0xE280_0001,
            0xE581_0040,
            0xEAFF_FFFC,
        ];
        let (mut a, mut ma) = boot_arm9(&PROG);
        let (mut b, mut mb) = boot_arm9(&PROG);

        let used_run = a.run(&mut ma, 200);
        let mut used_step = 0;
        while used_step < 200 {
            used_step += b.step(&mut mb);
        }

        assert_eq!(used_run, used_step, "cycle accounting must match");
        assert_eq!(a.cpu.registers.gpr, b.cpu.registers.gpr, "registers must match");
        assert_eq!(
            ma.read_word_arm9(0x0200_0040),
            mb.read_word_arm9(0x0200_0040),
            "memory effects must match"
        );
        assert!(ma.read_word_arm9(0x0200_0040) > 1, "the program actually ran");
    }

    /// A core halted for the whole window is charged in one go rather than one
    /// no-op `step` per cycle — the identity the halt collapse rests on.
    #[test]
    fn run_collapses_a_fully_halted_window() {
        let (mut cpu, mut mmu) = boot_arm9(&[0xE3A0_0000]);
        cpu.cpu.halted = true;
        mmu.arm9_ie = 0;
        mmu.arm9_if = 0;
        mmu.arm9_halt_cycles = 0;

        assert_eq!(cpu.run(&mut mmu, 64), 64, "the whole budget is idled away");
        assert!(cpu.cpu.halted, "nothing in the window can end the halt");
        assert_eq!(mmu.arm9_halt_cycles, 64, "every idle cycle is still counted");
    }

    /// ...but the collapse must not swallow a wake-up that is already pending:
    /// `IE & IF` ends the halt and the core executes for the rest of the window.
    #[test]
    fn run_wakes_a_halted_core_with_a_pending_irq() {
        // MOV r0,#0x42 at the reset vector, so "did it execute" is observable.
        let (mut cpu, mut mmu) = boot_arm9(&[0xE3A0_0042]);
        cpu.cpu.halted = true;
        mmu.arm9_ie = 1;
        mmu.arm9_if = 1;
        mmu.arm9_ime = 0; // wake without taking the IRQ: GBATEK's halt rule

        let used = cpu.run(&mut mmu, 64);
        assert!(!cpu.cpu.halted, "IE & IF must end the halt");
        assert_eq!(cpu.cpu.registers.gpr[0], 0x42, "the core resumed executing");
        assert!(used >= 64, "the window is still fully consumed");
    }

    /// The shared ARM7TDMI interpreter must really execute ARM instructions
    /// against the NDS ARM9 memory map (this is the whole point of M1b: before
    /// it, `step` only decoded CP15 and the game never ran).
    #[test]
    fn arm9_executes_arm_and_writes_memory() {
        // MOV r0,#0x42 ; MOV r1,#0x02000000 ; STR r0,[r1,#0x10]
        let (mut cpu, mut mmu) = boot_arm9(&[0xE3A0_0042, 0xE3A0_1402, 0xE581_0010]);
        for _ in 0..3 {
            cpu.step(&mut mmu);
        }
        assert_eq!(cpu.cpu.registers.gpr[0], 0x42, "MOV r0 executed");
        assert_eq!(cpu.cpu.registers.gpr[1], 0x0200_0000, "MOV r1 executed");
        assert_eq!(
            mmu.read_word_arm9(0x0200_0010),
            0x42,
            "STR reached Main RAM through Arm9Bus"
        );
    }

    /// A backward branch must actually redirect fetch (proves pipeline flush via
    /// the generic `flush_pipeline` works on the NDS bus).
    #[test]
    fn arm9_branch_redirects_execution() {
        // 0x00: MOV r0,#1 ; 0x04: B +0 (to 0x0C, skipping 0x08) ; 0x08: MOV r0,#9
        // 0x0C: MOV r2,#7
        // B offset: target = pc(0x04)+8 + (imm<<2). For target 0x0C: imm = 0.
        let (mut cpu, mut mmu) = boot_arm9(&[
            0xE3A0_0001, // MOV r0,#1
            0xEA00_0000, // B  +0  -> lands at 0x0C
            0xE3A0_0009, // MOV r0,#9  (must be skipped)
            0xE3A0_2007, // MOV r2,#7
        ]);
        for _ in 0..3 {
            cpu.step(&mut mmu);
        }
        assert_eq!(cpu.cpu.registers.gpr[0], 1, "branch skipped MOV r0,#9");
        assert_eq!(cpu.cpu.registers.gpr[2], 7, "landed on instruction after branch");
    }

    /// ARMv5 CLZ (ARM9 only) — the ARM7 must NOT decode it (checked separately).
    #[test]
    fn arm9_clz() {
        // MOV r0,#0x00010000 ; CLZ r1,r0 ; MOV r2,#0 ; CLZ r3,r2
        let (mut cpu, mut mmu) = boot_arm9(&[
            0xE3A0_0801, // MOV r0,#0x00010000
            0xE16F_1F10, // CLZ r1,r0
            0xE3A0_2000, // MOV r2,#0
            0xE16F_3F12, // CLZ r3,r2
        ]);
        for _ in 0..4 {
            cpu.step(&mut mmu);
        }
        assert_eq!(cpu.cpu.registers.gpr[1], 15, "CLZ(0x00010000) == 15");
        assert_eq!(cpu.cpu.registers.gpr[3], 32, "CLZ(0) == 32");
    }

    /// ARMv5 BLX(imm) must switch the ARM9 into Thumb state and run the target.
    #[test]
    fn arm9_blx_imm_switches_to_thumb() {
        let mut mmu = NdsMmu::new();
        mmu.write_word_arm9(0x0200_0000, 0xFA00_0000); // BLX #0 -> 0x02000008 (Thumb)
        mmu.write_word_arm9(0x0200_0004, 0xE3A0_00FF); // MOV r0,#0xFF (ARM, skipped)
        mmu.write_halfword_arm9(0x0200_0008, 0x2055); // Thumb: MOV r0,#0x55
        let mut cpu = Arm9Cpu::new();
        cpu.cpu.registers.cpsr = 0x1F;
        cpu.cpu.registers.gpr[15] = 0x0200_0000;
        cpu.flush_pipeline(&mut mmu);
        cpu.step(&mut mmu); // BLX: link + switch to Thumb
        cpu.step(&mut mmu); // (flush Thumb) + MOV r0,#0x55
        assert!(cpu.cpu.registers.get_flag(FLAG_T), "BLX switched to Thumb");
        assert_eq!(cpu.cpu.registers.gpr[0], 0x55, "ran the Thumb target");
        assert_eq!(cpu.cpu.registers.gpr[14], 0x0200_0004, "LR = return address");
    }

    /// ARMv5T interworking load: `ldmfd sp!,{pc}` (POP pc) with bit 0 set in the
    /// popped value must switch the ARM9 into Thumb and run the target there. This
    /// is the idiom Thumb-heavy DS code uses to return from an ARM function back to
    /// a Thumb caller; without it the ARM9 executes Thumb as ARM and derails.
    #[test]
    fn arm9_ldm_pc_interworks_to_thumb() {
        let mut mmu = NdsMmu::new();
        mmu.write_word_arm9(0x0200_0000, 0xE8BD_8000); // LDMFD sp!,{pc}  (pop pc)
        mmu.write_halfword_arm9(0x0200_0008, 0x2055); // Thumb: MOV r0,#0x55
        mmu.write_word_arm9(0x0200_1000, 0x0200_0009); // stacked Thumb return addr (bit0=1)
        let mut cpu = Arm9Cpu::new();
        cpu.cpu.registers.cpsr = 0x1F; // System, ARM state
        cpu.cpu.registers.gpr[13] = 0x0200_1000; // sp
        cpu.cpu.registers.gpr[15] = 0x0200_0000;
        cpu.flush_pipeline(&mut mmu);
        cpu.step(&mut mmu); // LDM pc -> interwork to Thumb @0x02000008
        cpu.step(&mut mmu); // (flush Thumb) + MOV r0,#0x55
        assert!(
            cpu.cpu.registers.get_flag(FLAG_T),
            "LDM pc with bit0=1 switched ARM9 to Thumb"
        );
        assert_eq!(cpu.cpu.registers.gpr[0], 0x55, "ran the Thumb target after LDM interwork");
    }

    /// The mirror-image guard: the ARM7 (ARMv4T) has no load-to-PC interworking —
    /// the same POP must clear bit 0 and stay in ARM state. Protects the GBA/ARM7
    /// path from the ARMv5 change above.
    #[test]
    fn arm7_ldm_pc_does_not_interwork() {
        let mut mmu = NdsMmu::new();
        mmu.write_word_arm7(0x0380_0000, 0xE8BD_8000); // LDMFD sp!,{pc}
        mmu.write_word_arm7(0x0380_1000, 0x0380_0009); // stacked addr with bit0=1
        let mut cpu = Arm7Cpu::new();
        cpu.cpu.registers.cpsr = 0x1F; // System, ARM state
        cpu.cpu.registers.gpr[13] = 0x0380_1000; // sp
        cpu.cpu.registers.gpr[15] = 0x0380_0000;
        cpu.flush_pipeline(&mut mmu);
        cpu.step(&mut mmu); // LDM pc: ARMv4T ignores bit0
        assert!(
            !cpu.cpu.registers.get_flag(FLAG_T),
            "ARM7 (ARMv4T) must not interwork on LDM pc"
        );
    }

    /// ARMv5TE LDRD/STRD (L=0, SH=10/11 in the halfword-transfer space) must
    /// move the Rd/Rd+1 register PAIR. Decoded as the v4 STRH fallback, an LDRD
    /// *writes to its source* instead of loading — SoulSilver's overlay
    /// decompressor garbled its own output exactly this way.
    #[test]
    fn arm9_ldrd_strd_move_register_pairs() {
        let mut mmu = NdsMmu::new();
        mmu.write_word_arm9(0x0200_0000, 0xE1C0_40D0); // LDRD r4,[r0]
        mmu.write_word_arm9(0x0200_0004, 0xE1C1_40F0); // STRD r4,[r1]
        mmu.write_word_arm9(0x0200_0100, 0x1111_1111);
        mmu.write_word_arm9(0x0200_0104, 0x2222_2222);
        let mut cpu = Arm9Cpu::new();
        cpu.cpu.registers.cpsr = 0x1F;
        cpu.cpu.registers.gpr[0] = 0x0200_0100;
        cpu.cpu.registers.gpr[1] = 0x0200_0200;
        cpu.cpu.registers.gpr[15] = 0x0200_0000;
        cpu.flush_pipeline(&mut mmu);
        cpu.step(&mut mmu); // LDRD
        assert_eq!(cpu.cpu.registers.gpr[4], 0x1111_1111, "LDRD loaded Rd");
        assert_eq!(cpu.cpu.registers.gpr[5], 0x2222_2222, "LDRD loaded Rd+1");
        assert_eq!(
            mmu.read_word_arm9(0x0200_0100),
            0x1111_1111,
            "LDRD must not write to its source (the old STRH-fallback bug)"
        );
        cpu.step(&mut mmu); // STRD
        assert_eq!(mmu.read_word_arm9(0x0200_0200), 0x1111_1111, "STRD stored Rd");
        assert_eq!(mmu.read_word_arm9(0x0200_0204), 0x2222_2222, "STRD stored Rd+1");
    }

    /// Thumb `BLX Rm` (0x4780 | rm<<3, ARMv5T) must LINK: LR = next instr | 1.
    /// Our decoder treated it as plain BX, so a callee's `BX LR` "returned" to
    /// the previous call site, double-ran that epilogue with a shifted SP and
    /// popped stack data as PC — SoulSilver's post-boot DTCM derail.
    #[test]
    fn arm9_thumb_blx_reg_links() {
        let mut mmu = NdsMmu::new();
        mmu.write_halfword_arm9(0x0200_0000, 0x4790); // BLX r2
        mmu.write_halfword_arm9(0x0200_0100, 0x2055); // target: MOV r0,#0x55
        let mut cpu = Arm9Cpu::new();
        cpu.cpu.registers.cpsr = 0x1F | 0x20; // System, Thumb
        cpu.cpu.registers.gpr[2] = 0x0200_0101; // Thumb target (bit0 = 1)
        cpu.cpu.registers.gpr[15] = 0x0200_0000;
        cpu.flush_pipeline(&mut mmu);
        cpu.step(&mut mmu); // BLX r2
        assert_eq!(
            cpu.cpu.registers.gpr[14],
            0x0200_0003,
            "LR = address after the BLX, with the Thumb bit"
        );
        cpu.step(&mut mmu); // (flush) + MOV r0,#0x55 at the target
        assert_eq!(cpu.cpu.registers.gpr[0], 0x55, "ran the BLX target");
    }

    /// ARMv4T (ARM7) has no Thumb BLX(reg): the same encoding must stay a plain
    /// BX and must NOT clobber LR — protects the GBA/ARM7 path from the fix.
    #[test]
    fn arm7_thumb_blx_encoding_does_not_link() {
        let mut mmu = NdsMmu::new();
        mmu.write_halfword_arm7(0x0380_0000, 0x4790); // BX-family, h1=1
        let mut cpu = Arm7Cpu::new();
        cpu.cpu.registers.cpsr = 0x1F | 0x20; // System, Thumb
        cpu.cpu.registers.gpr[2] = 0x0380_0101;
        cpu.cpu.registers.gpr[14] = 0xDEAD_BEEF; // sentinel
        cpu.cpu.registers.gpr[15] = 0x0380_0000;
        cpu.flush_pipeline(&mut mmu);
        cpu.step(&mut mmu);
        assert_eq!(
            cpu.cpu.registers.gpr[14],
            0xDEAD_BEEF,
            "ARMv4T must not link on the BLX(reg) encoding"
        );
    }

    /// ARM946E-S WFI (`MCR p15,0,r0,c7,c0,4`) must halt the ARM9, and the halt
    /// must break when an enabled interrupt is REQUESTED (IE & IF) even with
    /// IME=0 — GBATEK "Halt": IME/CPSR-I only gate *taking* the IRQ, not leaving
    /// low-power. The RTOS idle thread relies on exactly this.
    #[test]
    fn arm9_wfi_halts_and_wakes_on_ie_and_if_without_ime() {
        let (mut cpu, mut mmu) = boot_arm9(&[
            0xEE07_0F90, // MCR p15,0,r0,c7,c0,4 (WFI)
            0xE3A0_1042, // MOV r1,#0x42 (must run only after wake)
        ]);
        cpu.step(&mut mmu); // WFI -> halted
        assert!(cpu.cpu.halted, "WFI halted the ARM9");
        cpu.step(&mut mmu); // no pending IRQ: stays halted, executes nothing
        assert!(cpu.cpu.halted, "stays halted without IE & IF");
        assert_eq!(cpu.cpu.registers.gpr[1], 0, "no execution while halted");
        // Request an enabled interrupt with IME OFF: must wake, not take the IRQ.
        mmu.arm9_ime = 0;
        mmu.arm9_ie = 1 << 18;
        mmu.arm9_if = 1 << 18;
        cpu.step(&mut mmu); // wake tick
        assert!(!cpu.cpu.halted, "IE & IF wakes the halt regardless of IME");
        cpu.step(&mut mmu); // resumes at the instruction after the WFI
        assert_eq!(cpu.cpu.registers.gpr[1], 0x42, "execution continued after WFI");
    }

    /// NDS BIOS SWI 0x0E (GetCRC16): r0=init, r1=src, r2=byte length →
    /// r0 = CRC16 poly 0xA001 LSB-first. The ARM7's firmware-settings
    /// validator computes copy CRCs through this call; as a no-op it failed
    /// every copy and zeroed the TP calibration (dead UI touch, U27).
    #[test]
    fn arm9_swi_get_crc16_returns_bios_crc() {
        let (mut cpu, mut mmu) = boot_arm9(&[
            0xEF0E_0000, // SWI 0x0E (comment field 0x0E0000)
        ]);
        // 4 known bytes at a scratch main-RAM address.
        for (i, b) in [0x05u8, 0x00, 0x01, 0xFF].iter().enumerate() {
            mmu.write_byte_arm9(0x0200_4000 + i as u32, *b);
        }
        cpu.cpu.registers.gpr[0] = 0xFFFF;
        cpu.cpu.registers.gpr[1] = 0x0200_4000;
        cpu.cpu.registers.gpr[2] = 4;
        cpu.step(&mut mmu);
        // Reference CRC16-MODBUS(init 0xFFFF) over [05 00 01 FF].
        let mut crc = 0xFFFFu32;
        for &b in &[0x05u8, 0x00, 0x01, 0xFF] {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
            }
        }
        assert_eq!(cpu.cpu.registers.gpr[0], crc, "SWI 0x0E computes the BIOS CRC16");
    }

    /// NDS BIOS SWI 0x09 (Div): r0/r1 -> r0 = quotient, r1 = remainder,
    /// r3 = |quotient|. The TP calibrate-param computation divides ADC spans
    /// through this call; as a no-op the calibration dot factors were zero.
    #[test]
    fn arm9_swi_div_returns_quotient_remainder_abs() {
        let (mut cpu, mut mmu) = boot_arm9(&[0xEF09_0000, 0xEF09_0000]);
        cpu.cpu.registers.gpr[0] = 100;
        cpu.cpu.registers.gpr[1] = 7;
        cpu.step(&mut mmu);
        assert_eq!(cpu.cpu.registers.gpr[0], 14);
        assert_eq!(cpu.cpu.registers.gpr[1], 2);
        assert_eq!(cpu.cpu.registers.gpr[3], 14);
        // Negative dividend: -100 / 7 = -14 rem -2, |q| = 14.
        cpu.cpu.registers.gpr[0] = (-100i32) as u32;
        cpu.cpu.registers.gpr[1] = 7;
        cpu.step(&mut mmu);
        assert_eq!(cpu.cpu.registers.gpr[0] as i32, -14);
        assert_eq!(cpu.cpu.registers.gpr[1] as i32, -2);
        assert_eq!(cpu.cpu.registers.gpr[3], 14);
    }

    /// The ARM7 (ARMv4T) must treat the ARMv5 CLZ encoding as a normal data-proc
    /// instruction, never as CLZ — proves the `armv5` gate isolates the ARM9.
    #[test]
    fn arm7_does_not_decode_clz() {
        let mut mmu = NdsMmu::new();
        // Put the same program in ARM7's WRAM window and run it there.
        mmu.write_word_arm7(0x0380_0000, 0xE3A0_0801); // MOV r0,#0x00010000
        mmu.write_word_arm7(0x0380_0004, 0xE16F_1F10); // (CLZ pattern) -> NOT CLZ on ARM7
        let mut cpu = Arm7Cpu::new();
        cpu.cpu.registers.cpsr = 0x1F;
        cpu.cpu.registers.gpr[15] = 0x0380_0000;
        cpu.flush_pipeline(&mut mmu);
        cpu.step(&mut mmu);
        cpu.step(&mut mmu);
        assert_ne!(cpu.cpu.registers.gpr[1], 15, "ARM7 must not compute CLZ");
    }
}
