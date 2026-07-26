use crate::cpu_bus::CpuBus;
use crate::gba::mmu::GbaMmu;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CpuMode {
    User = 0x10,
    Fiq = 0x11,
    Irq = 0x12,
    Supervisor = 0x13,
    Abort = 0x17,
    Undefined = 0x1B,
    System = 0x1F,
}

pub struct CpuRegisters {
    pub gpr: [u32; 16],
    pub cpsr: u32,
    pub spsr: u32,

    // Banked Registers Storage
    pub(crate) r8_usr: [u32; 5],
    pub(crate) r8_fiq: [u32; 5],

    pub(crate) r13_usr: u32,
    pub(crate) r14_usr: u32,

    pub(crate) r13_svc: u32,
    pub(crate) r14_svc: u32,
    pub(crate) spsr_svc: u32,

    pub(crate) r13_irq: u32,
    pub(crate) r14_irq: u32,
    pub(crate) spsr_irq: u32,

    pub(crate) r13_abt: u32,
    pub(crate) r14_abt: u32,
    pub(crate) spsr_abt: u32,

    pub(crate) r13_und: u32,
    pub(crate) r14_und: u32,
    pub(crate) spsr_und: u32,

    pub(crate) r13_fiq: u32,
    pub(crate) r14_fiq: u32,
    pub(crate) spsr_fiq: u32,
}

impl CpuRegisters {
    pub fn new() -> Self {
        let mut regs = Self {
            gpr: [0; 16],
            cpsr: 0x1F, // System Mode initially
            spsr: 0,
            r8_usr: [0; 5],
            r8_fiq: [0; 5],
            r13_usr: 0x03007F00, // SP initial usr
            r14_usr: 0,
            r13_svc: 0x03007FE0, // SP initial svc
            r14_svc: 0,
            spsr_svc: 0,
            r13_irq: 0x03007FA0, // SP initial irq
            r14_irq: 0,
            spsr_irq: 0,
            r13_abt: 0,
            r14_abt: 0,
            spsr_abt: 0,
            r13_und: 0,
            r14_und: 0,
            spsr_und: 0,
            r13_fiq: 0,
            r14_fiq: 0,
            spsr_fiq: 0,
        };
        regs.gpr[13] = 0x03007F00; // SP active
        regs
    }

    pub fn get_flag(&self, bit: u32) -> bool {
        (self.cpsr & bit) != 0
    }

    pub fn set_flag(&mut self, bit: u32, val: bool) {
        if val {
            self.cpsr |= bit;
        } else {
            self.cpsr &= !bit;
        }
    }

    pub fn get_mode(&self) -> CpuMode {
        match self.cpsr & 0x1F {
            0x10 => CpuMode::User,
            0x11 => CpuMode::Fiq,
            0x12 => CpuMode::Irq,
            0x13 => CpuMode::Supervisor,
            0x17 => CpuMode::Abort,
            0x1B => CpuMode::Undefined,
            _ => CpuMode::System,
        }
    }

    pub fn swap_mode(&mut self, old_mode: CpuMode, new_mode: CpuMode) {
        if old_mode == new_mode {
            return;
        }

        // 1. Save active registers to old mode bank
        match old_mode {
            CpuMode::User | CpuMode::System => {
                self.r13_usr = self.gpr[13];
                self.r14_usr = self.gpr[14];
            }
            CpuMode::Supervisor => {
                self.r13_svc = self.gpr[13];
                self.r14_svc = self.gpr[14];
                self.spsr_svc = self.spsr;
            }
            CpuMode::Irq => {
                self.r13_irq = self.gpr[13];
                self.r14_irq = self.gpr[14];
                self.spsr_irq = self.spsr;
            }
            CpuMode::Abort => {
                self.r13_abt = self.gpr[13];
                self.r14_abt = self.gpr[14];
                self.spsr_abt = self.spsr;
            }
            CpuMode::Undefined => {
                self.r13_und = self.gpr[13];
                self.r14_und = self.gpr[14];
                self.spsr_und = self.spsr;
            }
            CpuMode::Fiq => {
                self.r13_fiq = self.gpr[13];
                self.r14_fiq = self.gpr[14];
                self.spsr_fiq = self.spsr;
                self.r8_fiq.copy_from_slice(&self.gpr[8..13]);
            }
        }

        // If old mode was FIQ and new mode is not, restore r8-r12 from USR bank
        if old_mode == CpuMode::Fiq && new_mode != CpuMode::Fiq {
            self.gpr[8..13].copy_from_slice(&self.r8_usr);
        }
        // If old mode was USR/SYS and new mode is FIQ, save r8-r12 to USR bank
        if (old_mode == CpuMode::User || old_mode == CpuMode::System) && new_mode == CpuMode::Fiq {
            self.r8_usr.copy_from_slice(&self.gpr[8..13]);
        }

        // 2. Load active registers from new mode bank
        match new_mode {
            CpuMode::User | CpuMode::System => {
                self.gpr[13] = self.r13_usr;
                self.gpr[14] = self.r14_usr;
                self.spsr = 0;
            }
            CpuMode::Supervisor => {
                self.gpr[13] = self.r13_svc;
                self.gpr[14] = self.r14_svc;
                self.spsr = self.spsr_svc;
            }
            CpuMode::Irq => {
                self.gpr[13] = self.r13_irq;
                self.gpr[14] = self.r14_irq;
                self.spsr = self.spsr_irq;
            }
            CpuMode::Abort => {
                self.gpr[13] = self.r13_abt;
                self.gpr[14] = self.r14_abt;
                self.spsr = self.spsr_abt;
            }
            CpuMode::Undefined => {
                self.gpr[13] = self.r13_und;
                self.gpr[14] = self.r14_und;
                self.spsr = self.spsr_und;
            }
            CpuMode::Fiq => {
                self.gpr[13] = self.r13_fiq;
                self.gpr[14] = self.r14_fiq;
                self.spsr = self.spsr_fiq;
                self.gpr[8..13].copy_from_slice(&self.r8_fiq);
            }
        }

        // Update CPSR mode bits
        self.cpsr = (self.cpsr & !0x1F) | (new_mode as u32);
    }
}

/// Selects which BIOS the shared interpreter's `SWI` instruction dispatches to.
/// The ARM7TDMI core is shared between the GBA and the NDS, but their BIOS SWI
/// tables differ, so the owning core tags itself once and `handle_swi` routes
/// accordingly. Defaults to `Gba`; the NDS CPUs set `Nds` after construction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SwiMode {
    Gba,
    Nds,
}

pub struct GbaCpu {
    pub registers: CpuRegisters,
    pub pipeline: [u32; 2],
    pub pc_modified: bool,
    pub halted: bool,
    pub exception_depth: u32,
    pub intr_wait_flags: u32,
    pub swi_mode: SwiMode,
    /// True for the NDS ARM9 (ARM946E-S, ARMv5TE): enables the ARMv5-only opcodes
    /// (BLX, CLZ, Thumb-BLX). Left false for the GBA and NDS ARM7 (both ARMv4T),
    /// so their decode is completely unaffected.
    pub armv5: bool,
}

// ---------------------------------------------------------------------------
// Snapshot support (see `crate::snapshot`): the field lists below are the
// authoritative "what is CPU state" answer for both save and restore.
// ---------------------------------------------------------------------------

impl crate::snapshot::Snap for CpuRegisters {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.gpr.snap(v);
        self.cpsr.snap(v);
        self.spsr.snap(v);
        self.r8_usr.snap(v);
        self.r8_fiq.snap(v);
        self.r13_usr.snap(v);
        self.r14_usr.snap(v);
        self.r13_svc.snap(v);
        self.r14_svc.snap(v);
        self.spsr_svc.snap(v);
        self.r13_irq.snap(v);
        self.r14_irq.snap(v);
        self.spsr_irq.snap(v);
        self.r13_abt.snap(v);
        self.r14_abt.snap(v);
        self.spsr_abt.snap(v);
        self.r13_und.snap(v);
        self.r14_und.snap(v);
        self.spsr_und.snap(v);
        self.r13_fiq.snap(v);
        self.r14_fiq.snap(v);
        self.spsr_fiq.snap(v);
    }
}

impl crate::snapshot::Snap for GbaCpu {
    /// `swi_mode` and `armv5` are deliberately absent: they describe *which
    /// core this is*, fixed when the emulator constructed it, and a snapshot
    /// that could flip them would let an ARM7 resume as an ARM9.
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.registers.snap(v);
        self.pipeline.snap(v);
        self.pc_modified.snap(v);
        self.halted.snap(v);
        self.exception_depth.snap(v);
        self.intr_wait_flags.snap(v);
    }
}

const FLAG_N: u32 = 1 << 31;
const FLAG_Z: u32 = 1 << 30;
const FLAG_C: u32 = 1 << 29;
const FLAG_V: u32 = 1 << 28;
/// ARMv5TE sticky saturation flag. Set by the saturating-arithmetic and
/// signed-multiply-accumulate instructions; never cleared implicitly.
const FLAG_Q: u32 = 1 << 27;

/// Signed 32-bit add that saturates instead of wrapping, plus whether it did.
/// The ARMv5TE Q-family instructions clamp to i32::MIN..=i32::MAX rather than
/// wrapping, which is the entire point of them in fixed-point DSP code.
#[inline]
fn sat_add(a: i32, b: i32) -> (i32, bool) {
    match a.checked_add(b) {
        Some(v) => (v, false),
        None => (if a > 0 { i32::MAX } else { i32::MIN }, true),
    }
}

/// Signed 32-bit subtract that saturates instead of wrapping; see [`sat_add`].
#[inline]
fn sat_sub(a: i32, b: i32) -> (i32, bool) {
    match a.checked_sub(b) {
        Some(v) => (v, false),
        None => (if a >= 0 { i32::MAX } else { i32::MIN }, true),
    }
}
const FLAG_I: u32 = 1 << 7;
const _FLAG_F: u32 = 1 << 6;
const FLAG_T: u32 = 1 << 5;

impl GbaCpu {
    pub fn new() -> Self {
        Self {
            registers: CpuRegisters::new(),
            pipeline: [0; 2],
            pc_modified: false,
            halted: false,
            exception_depth: 0,
            intr_wait_flags: 0,
            swi_mode: SwiMode::Gba,
            armv5: false,
        }
    }

    pub fn reset(&mut self) {
        self.registers = CpuRegisters::new();
        self.pipeline = [0; 2];
        self.pc_modified = false;
        self.halted = false;
        self.exception_depth = 0;
        self.intr_wait_flags = 0;
    }

    /// Reset the CPU and start executing the cartridge directly (BIOS skipped).
    /// Sets PC to the ROM entry point in ARM/System state and primes the
    /// pipeline. Without this the CPU would run from the zeroed BIOS region and
    /// never reach game code (permanent black screen).
    pub fn boot(&mut self, mmu: &mut GbaMmu) {
        self.reset();
        self.registers.cpsr = CpuMode::System as u32; // ARM state (T clear), IRQ enabled
        self.registers.gpr[15] = 0x0800_0000; // cartridge entry point
        self.flush_pipeline(mmu);
    }

    pub fn flush_pipeline<B: CpuBus>(&mut self, mmu: &mut B) {
        let is_thumb = self.registers.get_flag(FLAG_T);
        if is_thumb {
            let pc = self.registers.gpr[15] & !1;
            self.pipeline[0] = mmu.read_halfword(pc) as u32;
            self.pipeline[1] = mmu.read_halfword(pc.wrapping_add(2)) as u32;
            self.registers.gpr[15] = pc.wrapping_add(4);
        } else {
            let pc = self.registers.gpr[15] & !3;
            self.pipeline[0] = mmu.read_word(pc);
            self.pipeline[1] = mmu.read_word(pc.wrapping_add(4));
            self.registers.gpr[15] = pc.wrapping_add(8);
        }
        self.pc_modified = false;
    }

    /// Execute one instruction and return the cycles it consumed.
    ///
    /// Pipeline invariant: while the instruction at address `A` executes,
    /// `gpr[15]` reads as `A + 8` in ARM state and `A + 4` in THUMB state — the
    /// value real ARM7TDMI hardware exposes via R15. `pipeline[0]` holds the
    /// instruction currently executing; `pipeline[1]` the already-fetched next
    /// one. A control-flow change sets `pc_modified`, and the next step refills
    /// the pipeline from the branch target via `flush_pipeline`.
    pub fn step(&mut self, mmu: &mut GbaMmu) -> u32 {
        if self.halted {
            // Wait for interrupts
            let reg_ie = mmu.read_halfword_safe(0x04000200) as u32;
            let reg_if = mmu.read_halfword_safe(0x04000202) as u32;
            if (reg_if & self.intr_wait_flags) != 0 || (reg_if & reg_ie) != 0 {
                self.halted = false;
            }
            return 1;
        }

        // Refill the pipeline first if a previous instruction moved PC, so that
        // gpr[15] is normalised to the invariant before the IRQ return link is
        // computed from it below.
        if self.pc_modified {
            self.flush_pipeline(mmu);
        }

        // Service a pending, enabled IRQ before fetching the next instruction.
        let ime = mmu.read_word_safe(0x04000208);
        if (ime & 1) != 0 && !self.registers.get_flag(FLAG_I) {
            let ie = mmu.read_halfword_safe(0x04000200);
            let r_if = mmu.read_halfword_safe(0x04000202);
            if (ie & r_if) != 0 {
                self.trigger_irq(mmu);
                return 4;
            }
        }

        let is_thumb = self.registers.get_flag(FLAG_T);
        let instr_size = if is_thumb { 2 } else { 4 };

        let inst = self.pipeline[0];
        self.pipeline[0] = self.pipeline[1];

        // Prefetch the next instruction at the current fetch address
        // (gpr[15] == exec_addr + 2 * instr_size).
        self.pipeline[1] = if is_thumb {
            mmu.read_halfword(self.registers.gpr[15] & !1) as u32
        } else {
            mmu.read_word(self.registers.gpr[15] & !3)
        };

        let cycles = if is_thumb {
            self.execute_thumb(inst as u16, mmu)
        } else {
            self.execute_arm(inst, mmu)
        };

        // Advance sequentially unless the instruction already moved PC (a branch
        // will instead refill the pipeline on the next step).
        if !self.pc_modified {
            self.registers.gpr[15] = self.registers.gpr[15].wrapping_add(instr_size);
        }

        cycles
    }

    fn trigger_irq(&mut self, _mmu: &mut GbaMmu) {
        let old_cpsr = self.registers.cpsr;
        let old_mode = self.registers.get_mode();
        self.registers.swap_mode(old_mode, CpuMode::Irq);
        self.registers.spsr = old_cpsr;

        // The handler returns via `SUBS PC, LR, #4`, so LR must hold
        // (interrupted instruction address + 4). Under the pipeline invariant
        // gpr[15] is interrupted+8 (ARM) / interrupted+4 (THUMB) at this point.
        let is_thumb = (old_cpsr & FLAG_T) != 0;
        let return_link = if is_thumb {
            self.registers.gpr[15] // (interrupted + 4)
        } else {
            self.registers.gpr[15].wrapping_sub(4) // (interrupted + 8) - 4
        };
        self.registers.gpr[14] = return_link;

        self.registers.set_flag(FLAG_T, false); // Switch to ARM state
        self.registers.set_flag(FLAG_I, true); // Disable IRQ

        self.registers.gpr[15] = 0x00000018; // IRQ Exception Vector
        self.pc_modified = true;
    }

    // --- Shared ALU / addressing primitives ---

    /// Barrel shifter. Returns `(shifted_value, carry_out)`. `carry_in` (the
    /// current C flag) feeds the LSL#0 / RRX special cases. Encoded-immediate
    /// shift amounts pass straight through: a 0 amount means #32 for LSR/ASR and
    /// RRX for ROR. Register-specified shifts must guard `amount == 0` at the
    /// call site (there it means "no shift, carry unaffected").
    fn barrel_shift(value: u32, shift_type: u32, amount: u32, carry_in: bool) -> (u32, bool) {
        match shift_type & 3 {
            0 => {
                // LSL
                if amount == 0 {
                    (value, carry_in)
                } else if amount < 32 {
                    (value << amount, (value >> (32 - amount)) & 1 != 0)
                } else if amount == 32 {
                    (0, value & 1 != 0)
                } else {
                    (0, false)
                }
            }
            1 => {
                // LSR (#0 encodes #32)
                if amount == 0 || amount == 32 {
                    (0, (value >> 31) & 1 != 0)
                } else if amount < 32 {
                    (value >> amount, (value >> (amount - 1)) & 1 != 0)
                } else {
                    (0, false)
                }
            }
            2 => {
                // ASR (#0 encodes #32)
                if amount == 0 || amount >= 32 {
                    let neg = (value >> 31) & 1 != 0;
                    (if neg { 0xFFFF_FFFF } else { 0 }, neg)
                } else {
                    ((value as i32 >> amount) as u32, (value >> (amount - 1)) & 1 != 0)
                }
            }
            _ => {
                // ROR (#0 encodes RRX)
                if amount == 0 {
                    let carry_out = value & 1 != 0;
                    ((value >> 1) | ((carry_in as u32) << 31), carry_out)
                } else {
                    let a = amount & 31;
                    if a == 0 {
                        (value, (value >> 31) & 1 != 0)
                    } else {
                        (value.rotate_right(a), (value >> (a - 1)) & 1 != 0)
                    }
                }
            }
        }
    }

    /// Register-specified shift: `amount == 0` leaves the value (and carry)
    /// untouched, otherwise defers to the barrel shifter.
    #[inline]
    fn reg_shift(value: u32, shift_type: u32, amount: u32, carry_in: bool) -> (u32, bool) {
        if amount == 0 {
            (value, carry_in)
        } else {
            Self::barrel_shift(value, shift_type, amount, carry_in)
        }
    }

    #[inline]
    fn set_nz(&mut self, result: u32) {
        self.registers.set_flag(FLAG_N, (result & 0x8000_0000) != 0);
        self.registers.set_flag(FLAG_Z, result == 0);
    }

    /// `a + b + carry_in`, writing N/Z/C/V when `set_flags`. Subtraction reuses
    /// this via `a + !b + carry`: SUB = adc(a, !b, 1), SBC = adc(a, !b, C),
    /// RSB = adc(!a, b, 1), CMP = SUB (discarded), CMN = ADD.
    fn adc(&mut self, a: u32, b: u32, carry_in: u32, set_flags: bool) -> u32 {
        let (r1, c1) = a.overflowing_add(b);
        let (result, c2) = r1.overflowing_add(carry_in);
        if set_flags {
            let carry = c1 || c2;
            let overflow = (!(a ^ b) & (a ^ result)) & 0x8000_0000 != 0;
            self.set_nz(result);
            self.registers.set_flag(FLAG_C, carry);
            self.registers.set_flag(FLAG_V, overflow);
        }
        result
    }

    /// Flags for a logical op: N/Z plus C from the barrel shifter; V preserved.
    #[inline]
    fn set_logic_flags(&mut self, result: u32, shifter_carry: bool, set_flags: bool) {
        if set_flags {
            self.set_nz(result);
            self.registers.set_flag(FLAG_C, shifter_carry);
        }
    }

    /// Store a value into PC and request a pipeline refill. When
    /// `thumb_from_bit0` is set (BX-style interworking) bit 0 selects THUMB state.
    #[inline]
    fn write_pc(&mut self, value: u32, thumb_from_bit0: bool) {
        if thumb_from_bit0 {
            self.registers.set_flag(FLAG_T, (value & 1) != 0);
        }
        self.registers.gpr[15] = value;
        self.pc_modified = true;
    }

    /// Write PC from a *word load* that reaches PC (LDR pc, LDM {..,pc}, Thumb
    /// POP {pc}). This is the one place the ARMv4T/ARMv5T split matters: on
    /// ARMv5T (the NDS ARM9) bit 0 of the loaded value selects the instruction
    /// set — Thumb if 1, ARM if 0 — so a Thumb-heavy game can `ldmfd sp!,{..,pc}`
    /// straight back into Thumb. ARMv4T (GBA / NDS ARM7) has no such interworking:
    /// it forces bit 0 low and stays in the current state. Data-processing writes
    /// to PC (`mov pc,lr`, `add pc,..`) deliberately do NOT route here — they
    /// never interwork on either architecture; only their S-bit form (which
    /// restores CPSR from SPSR) changes state. Exception-return LDM (`{..,pc}^`)
    /// also bypasses this: T there comes from SPSR, not bit 0.
    #[inline]
    fn load_pc(&mut self, value: u32) {
        if self.armv5 {
            self.write_pc(value, true); // bit 0 -> Thumb/ARM (ARMv5T interworking load)
        } else {
            self.write_pc(value & !1, false); // ARMv4T: no interworking, stay in state
        }
    }

    /// Read a register; R15 gets `extra` added to model the extra pipeline step
    /// register-specified shifts see (PC+12 instead of PC+8).
    #[inline]
    fn reg_pc_extra(&self, r: usize, extra: u32) -> u32 {
        if r == 15 {
            self.registers.gpr[15].wrapping_add(extra)
        } else {
            self.registers.gpr[r]
        }
    }

    /// Restore CPSR from the current mode's SPSR (exception return), switching
    /// register banks if the mode changed.
    fn restore_cpsr_from_spsr(&mut self) {
        let spsr = self.registers.spsr;
        let target_mode = Self::mode_from_bits(spsr);
        let cur_mode = self.registers.get_mode();
        self.registers.swap_mode(cur_mode, target_mode);
        self.registers.cpsr = spsr; // full restore: flags + control + T
    }

    fn mode_from_bits(psr: u32) -> CpuMode {
        match psr & 0x1F {
            0x10 => CpuMode::User,
            0x11 => CpuMode::Fiq,
            0x12 => CpuMode::Irq,
            0x13 => CpuMode::Supervisor,
            0x17 => CpuMode::Abort,
            0x1B => CpuMode::Undefined,
            _ => CpuMode::System,
        }
    }

    // --- ARM Interpreter ---
    pub(crate) fn execute_arm<B: CpuBus>(&mut self, inst: u32, mmu: &mut B) -> u32 {
        // ARMv5 (ARM9): the cond==0b1111 encoding space is not "never" — it holds
        // BLX(immediate). Decode it before the condition check below, which treats
        // 0xF as an always-fail condition on ARMv4.
        if self.armv5 && (inst >> 28) == 0xF {
            if (inst & 0x0E00_0000) == 0x0A00_0000 {
                // BLX(imm): 1111 101H imm24 — link, always switch to Thumb; the H
                // bit contributes an extra halfword (bit 1) to the target.
                let h = (inst >> 24) & 1;
                let mut offset = (inst & 0x00FF_FFFF) as i32;
                if (offset & 0x0080_0000) != 0 {
                    offset |= !0x00FF_FFFF; // sign-extend 24 -> 32
                }
                let target =
                    ((self.registers.gpr[15] as i32).wrapping_add(offset << 2) as u32) | (h << 1);
                self.registers.gpr[14] = self.registers.gpr[15].wrapping_sub(4);
                self.write_pc(target | 1, true); // bit0 set -> Thumb state
                return 3;
            }
            return 1; // other NV-space encodings (PLD, ...) are no-ops here
        }

        let cond = inst >> 28;
        if !self.check_condition(cond) {
            return 1; // failed condition: 1 cycle, no effect
        }

        // Software interrupt (bits 27-24 = 1111).
        if (inst & 0x0F00_0000) == 0x0F00_0000 {
            self.handle_swi(((inst >> 16) & 0xFF) as u8, mmu);
            return 3;
        }

        // Branch / Branch with Link (bits 27-25 = 101).
        if (inst & 0x0E00_0000) == 0x0A00_0000 {
            let link = (inst & 0x0100_0000) != 0;
            let mut offset = (inst & 0x00FF_FFFF) as i32;
            if (offset & 0x0080_0000) != 0 {
                offset |= !0x00FF_FFFF; // sign-extend 24 -> 32
            }
            let target = (self.registers.gpr[15] as i32).wrapping_add(offset << 2) as u32;
            if link {
                self.registers.gpr[14] = self.registers.gpr[15].wrapping_sub(4);
            }
            self.write_pc(target, false);
            return 3;
        }

        // Branch and Exchange (BX Rn).
        if (inst & 0x0FFF_FFF0) == 0x012F_FF10 {
            let target = self.registers.gpr[(inst & 0xF) as usize];
            self.write_pc(target, true);
            return 3;
        }

        // ARMv5 (ARM9): BLX(reg) and CLZ share the BX encoding family.
        if self.armv5 {
            // BLX(reg): 0x012FFF3x — like BX but also links (LR = next instr).
            if (inst & 0x0FFF_FFF0) == 0x012F_FF30 {
                let target = self.registers.gpr[(inst & 0xF) as usize];
                self.registers.gpr[14] = self.registers.gpr[15].wrapping_sub(4);
                self.write_pc(target, true);
                return 3;
            }
            // CLZ Rd,Rm: 0x016F0F1x — count leading zeros (32 when Rm == 0).
            if (inst & 0x0FFF_0FF0) == 0x016F_0F10 {
                let rd = ((inst >> 12) & 0xF) as usize;
                let rm = self.registers.gpr[(inst & 0xF) as usize];
                self.registers.gpr[rd] = rm.leading_zeros();
                return 1;
            }
        }

        // Multiply (bits 27-22 = 000000, bits 7-4 = 1001).
        if (inst & 0x0FC0_00F0) == 0x0000_0090 {
            return self.arm_multiply(inst);
        }
        // Multiply long (bits 27-23 = 00001, bits 7-4 = 1001).
        if (inst & 0x0F80_00F0) == 0x0080_0090 {
            return self.arm_multiply_long(inst);
        }
        // Single data swap (bits 27-23 = 00010, bits 11-4 = 00001001).
        if (inst & 0x0FB0_0FF0) == 0x0100_0090 {
            return self.arm_swap(inst, mmu);
        }
        // Halfword / signed data transfer (bits 27-25 = 000, bit 7 = 1, bit 4 = 1).
        // Multiply/swap (which share bits 7,4) are decoded above, so only the
        // SH = 01/10/11 forms reach here.
        if (inst & 0x0E00_0090) == 0x0000_0090 {
            return self.arm_halfword_transfer(inst, mmu);
        }

        let is_dp_class = (inst & 0x0C00_0000) == 0;
        let opcode = (inst >> 21) & 0xF;
        let s = (inst & 0x0010_0000) != 0;

        // ARMv5TE control-instruction extension space. Must precede the PSR
        // gate below, which cannot tell these apart on its own.
        if self.armv5 {
            if let Some(cycles) = self.arm_dsp_extension(inst) {
                return cycles;
            }
        }

        // PSR transfer (MRS/MSR): data-proc opcodes 1000..1011 with S clear are
        // really PSR transfers, not TST/TEQ/CMP/CMN. Must precede data processing.
        //
        // The bits 7-4 test is load-bearing, not belt-and-braces. MRS is
        // `cond 00010 R 00 1111 Rd 0000 0000 0000` and MSR(register) is
        // `cond 00010 R 10 mask 1111 0000 0000 Rm`: both require bits 7-4 == 0.
        // Every other encoding in the same bits-27-23 / bit-20 window with bit 7
        // or bit 4 set is a different instruction, and without this test the
        // whole ARMv5TE extension space aliased onto the PSR path -- QADD wrote
        // CPSR into Rd instead of saturating, and SMULxy was executed as
        // `MSR SPSR` and rewrote a byte of the saved status register. BX, BLX
        // and CLZ were already carved out of this window by explicit bits-7-4
        // matches above, which is the same admission. MSR(immediate) sets bit 25
        // and puts its rotated immediate in bits 7-0, so it is exempt.
        let msr_immediate = (inst & 0x0200_0000) != 0;
        if is_dp_class && !s && (0x8..=0xB).contains(&opcode) && (msr_immediate || inst & 0xF0 == 0)
        {
            return self.arm_psr_transfer(inst);
        }

        // Data processing (bits 27-26 = 00).
        if is_dp_class {
            return self.arm_data_processing(inst);
        }
        // Single data transfer LDR/STR (bits 27-26 = 01).
        if (inst & 0x0C00_0000) == 0x0400_0000 {
            return self.arm_single_transfer(inst, mmu);
        }
        // Block data transfer LDM/STM (bits 27-25 = 100).
        if (inst & 0x0E00_0000) == 0x0800_0000 {
            return self.arm_block_transfer(inst, mmu);
        }

        // Coprocessor / undefined: consume a cycle without trapping.
        1
    }

    /// ARMv5TE control-instruction extension space: the saturating arithmetic
    /// (QADD/QSUB/QDADD/QDSUB) and signed 16-bit multiply (SMLAxy/SMLAWy/
    /// SMULWy/SMLALxy/SMULxy) families. Returns the cycle count, or `None` when
    /// `inst` is not one of them.
    ///
    /// These share the `cond 00010 xx 0` window with MRS/MSR and are told apart
    /// only by bits 7-4: `0b0101` selects the saturating family, and bit 7 set
    /// with bit 4 clear selects the multiply family. Their register fields are
    /// NOT the data-processing ones — Rd is bits 19-16, Rn bits 15-12, Rs bits
    /// 11-8, Rm bits 3-0 — which is why misdecoding them wrote to the wrong
    /// registers as well as doing the wrong arithmetic.
    ///
    /// The ARM9 in the DS is an ARM946E-S, so the SDK's fixed-point maths uses
    /// these freely; on the ARM7TDMI (`armv5` false) they do not exist and the
    /// caller does not consult this.
    ///
    /// ponytail: the Q flag (CPSR bit 27) is set on saturation and on
    /// accumulate overflow, as hardware does, but nothing reads it back — there
    /// is no MRS-based Q test in the decoder's coverage yet. Ceiling: a game
    /// branching on Q sees a flag that is correct but never sticky-cleared by
    /// `MSR CPSR_f`. Upgrade path: mask Q into the MSR field write.
    fn arm_dsp_extension(&mut self, inst: u32) -> Option<u32> {
        // bits 27-23 == 0b00010 and bit 20 == 0.
        //
        // Bit 23 is load-bearing and its omission was a live bug: testing only
        // bits 27-24 also admits data-processing opcodes 0xC-0xF (ORR/MOV/BIC/
        // MVN) with S clear, which set bit 24 AND bit 23. Those are among the
        // most common instructions there are, and any of them carrying a shift
        // immediate of 16 or more has bit 7 set with bit 4 clear -- the exact
        // signature this function uses to recognise a signed multiply. So
        // `MOV r0, r1, ASR #17` was executed as SMLAxy, writing a bogus product
        // to the wrong register. Requiring bit 23 clear narrows the test to the
        // 00010 window that MRS/MSR and the extension space actually share.
        if (inst & 0x0F80_0000) != 0x0100_0000 || (inst & 0x0010_0000) != 0 {
            return None;
        }
        let op = (inst >> 21) & 0x3; // 00 = add/SMLA, 01 = sub/SMLAW, 10 = QDADD/SMLAL, 11 = QDSUB/SMUL
        // The two families do NOT share a register layout, which is the trap in
        // this encoding space: the signed multiplies put Rd at bits 19-16 and Rn
        // at 15-12, while the saturating ones put Rn at 19-16 and Rd at 15-12.
        // Using one layout for both silently writes the wrong register.
        let rd_hi = ((inst >> 16) & 0xF) as usize; // multiplies: Rd / SMLAL RdHi
        let rn_lo = ((inst >> 12) & 0xF) as usize; // multiplies: Rn / SMLAL RdLo
        let rs = ((inst >> 8) & 0xF) as usize;
        let rm = (inst & 0xF) as usize;
        let lo_nibble = (inst >> 4) & 0xF;

        // Saturating add/subtract: bits 7-4 == 0b0101, Rd at 15-12, Rn at 19-16.
        if lo_nibble == 0b0101 {
            let (rd, rn) = (rn_lo, rd_hi);
            let a = self.registers.gpr[rm] as i32;
            let b = self.registers.gpr[rn] as i32;
            let (value, saturated) = match op {
                0b00 => sat_add(a, b),                    // QADD  Rd, Rm, Rn
                0b01 => sat_sub(a, b),                    // QSUB  Rd, Rm, Rn
                0b10 => {
                    let (dbl, q1) = sat_add(b, b); // QDADD Rd, Rm, Rn
                    let (res, q2) = sat_add(a, dbl);
                    (res, q1 || q2)
                }
                _ => {
                    let (dbl, q1) = sat_add(b, b); // QDSUB Rd, Rm, Rn
                    let (res, q2) = sat_sub(a, dbl);
                    (res, q1 || q2)
                }
            };
            self.registers.gpr[rd] = value as u32;
            if saturated {
                self.registers.set_flag(FLAG_Q, true);
            }
            return Some(1);
        }

        // Signed 16-bit multiplies: bit 7 set, bit 4 clear. Bit 5 (`x`) picks the
        // half of Rm and bit 6 (`y`) the half of Rs — top when set, bottom when
        // clear — except for SMLAW/SMULW, where bit 5 distinguishes the two
        // instructions and only `y` selects a half.
        if inst & 0x80 == 0 || inst & 0x10 != 0 {
            return None;
        }
        let half = |v: u32, top: bool| -> i32 {
            if top {
                ((v >> 16) as u16) as i16 as i32
            } else {
                (v as u16) as i16 as i32
            }
        };
        let x = inst & 0x20 != 0;
        let y = inst & 0x40 != 0;
        let (rd, rn) = (rd_hi, rn_lo);
        let m = self.registers.gpr[rm];
        let s = self.registers.gpr[rs];
        match op {
            // SMLAxy Rd, Rm, Rs, Rn : Rd = Rm.x * Rs.y + Rn, Q on add overflow.
            0b00 => {
                let product = half(m, x).wrapping_mul(half(s, y));
                let acc = self.registers.gpr[rn] as i32;
                let (value, overflow) = product.overflowing_add(acc);
                self.registers.gpr[rd] = value as u32;
                if overflow {
                    self.registers.set_flag(FLAG_Q, true);
                }
            }
            // SMLAWy / SMULWy: the full 32-bit Rm times one half of Rs, keeping
            // bits 47-16 of the 48-bit product.
            0b01 => {
                let product = ((m as i32 as i64) * (half(s, y) as i64)) >> 16;
                if x {
                    // SMULWy Rd, Rm, Rs — no accumulate, no Q.
                    self.registers.gpr[rd] = product as u32;
                } else {
                    let acc = self.registers.gpr[rn] as i32;
                    let (value, overflow) = (product as i32).overflowing_add(acc);
                    self.registers.gpr[rd] = value as u32;
                    if overflow {
                        self.registers.set_flag(FLAG_Q, true);
                    }
                }
            }
            // SMLALxy RdLo, RdHi, Rm, Rs : the 64-bit accumulate. RdHi is the
            // bits-19-16 field and RdLo the bits-15-12 one. No Q flag: a 64-bit
            // accumulator cannot overflow from a 32-bit product in one step.
            0b10 => {
                let product = i64::from(half(m, x).wrapping_mul(half(s, y)));
                let acc = ((u64::from(self.registers.gpr[rd]) << 32)
                    | u64::from(self.registers.gpr[rn])) as i64;
                let value = acc.wrapping_add(product) as u64;
                self.registers.gpr[rn] = value as u32;
                self.registers.gpr[rd] = (value >> 32) as u32;
            }
            // SMULxy Rd, Rm, Rs : the plain 16x16 product, no accumulate, no Q.
            _ => {
                self.registers.gpr[rd] = half(m, x).wrapping_mul(half(s, y)) as u32;
            }
        }
        Some(1)
    }

    fn arm_data_processing(&mut self, inst: u32) -> u32 {
        let opcode = (inst >> 21) & 0xF;
        let s = (inst & 0x0010_0000) != 0;
        let rn = ((inst >> 16) & 0xF) as usize;
        let rd = ((inst >> 12) & 0xF) as usize;
        let carry_in = self.registers.get_flag(FLAG_C);
        let reg_shift = (inst & 0x0200_0000) == 0 && (inst & 0x10) != 0;

        // Operand 2 and the shifter carry-out.
        let (op2, shifter_carry) = if (inst & 0x0200_0000) != 0 {
            let imm = inst & 0xFF;
            let rot = ((inst >> 8) & 0xF) * 2;
            if rot == 0 {
                (imm, carry_in)
            } else {
                let v = imm.rotate_right(rot);
                (v, (v >> 31) & 1 != 0)
            }
        } else {
            let shift_type = (inst >> 5) & 3;
            let rm_idx = (inst & 0xF) as usize;
            if reg_shift {
                let rs = ((inst >> 8) & 0xF) as usize;
                let amount = self.registers.gpr[rs] & 0xFF;
                Self::reg_shift(self.reg_pc_extra(rm_idx, 4), shift_type, amount, carry_in)
            } else {
                let amount = (inst >> 7) & 0x1F;
                Self::barrel_shift(self.registers.gpr[rm_idx], shift_type, amount, carry_in)
            }
        };

        let op1 = self.reg_pc_extra(rn, if reg_shift { 4 } else { 0 });

        let mut write = true;
        let result = match opcode {
            0x0 => { let r = op1 & op2; self.set_logic_flags(r, shifter_carry, s); r } // AND
            0x1 => { let r = op1 ^ op2; self.set_logic_flags(r, shifter_carry, s); r } // EOR
            0x2 => self.adc(op1, !op2, 1, s),                                          // SUB
            0x3 => self.adc(!op1, op2, 1, s),                                          // RSB
            0x4 => self.adc(op1, op2, 0, s),                                           // ADD
            0x5 => self.adc(op1, op2, carry_in as u32, s),                             // ADC
            0x6 => self.adc(op1, !op2, carry_in as u32, s),                            // SBC
            0x7 => self.adc(!op1, op2, carry_in as u32, s),                            // RSC
            0x8 => { write = false; let r = op1 & op2; self.set_logic_flags(r, shifter_carry, s); r } // TST
            0x9 => { write = false; let r = op1 ^ op2; self.set_logic_flags(r, shifter_carry, s); r } // TEQ
            0xA => { write = false; self.adc(op1, !op2, 1, s) }                        // CMP
            0xB => { write = false; self.adc(op1, op2, 0, s) }                         // CMN
            0xC => { let r = op1 | op2; self.set_logic_flags(r, shifter_carry, s); r } // ORR
            0xD => { self.set_logic_flags(op2, shifter_carry, s); op2 }                // MOV
            0xE => { let r = op1 & !op2; self.set_logic_flags(r, shifter_carry, s); r } // BIC
            _ => { let r = !op2; self.set_logic_flags(r, shifter_carry, s); r }        // MVN
        };

        if write {
            if rd == 15 {
                if s {
                    self.restore_cpsr_from_spsr(); // exception return: CPSR <- SPSR
                }
                self.write_pc(result, false);
            } else {
                self.registers.gpr[rd] = result;
            }
        }
        1
    }

    fn arm_single_transfer<B: CpuBus>(&mut self, inst: u32, mmu: &mut B) -> u32 {
        let reg_offset = (inst & 0x0200_0000) != 0; // bit25: 1 = register offset
        let pre = (inst & 0x0100_0000) != 0; // P
        let up = (inst & 0x0080_0000) != 0; // U
        let byte = (inst & 0x0040_0000) != 0; // B
        let writeback = (inst & 0x0020_0000) != 0; // W
        let load = (inst & 0x0010_0000) != 0; // L
        let rn = ((inst >> 16) & 0xF) as usize;
        let rd = ((inst >> 12) & 0xF) as usize;

        let offset = if reg_offset {
            let rm = self.registers.gpr[(inst & 0xF) as usize];
            let shift_type = (inst >> 5) & 3;
            let amount = (inst >> 7) & 0x1F;
            Self::barrel_shift(rm, shift_type, amount, self.registers.get_flag(FLAG_C)).0
        } else {
            inst & 0xFFF
        };

        let base = self.registers.gpr[rn];
        let offset_addr = if up { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
        let addr = if pre { offset_addr } else { base };

        if load {
            // MMU read_word applies the GBA rotation for unaligned addresses.
            let val = if byte { mmu.read_byte(addr) as u32 } else { mmu.read_word(addr) };
            if (writeback || !pre) && rd != rn {
                self.registers.gpr[rn] = offset_addr;
            }
            if rd == 15 {
                self.load_pc(val); // LDR pc: ARMv5T interworks on bit 0, ARMv4T does not
            } else {
                self.registers.gpr[rd] = val;
            }
        } else {
            let val = if rd == 15 {
                self.registers.gpr[15].wrapping_add(4) // STR stores PC + 12
            } else {
                self.registers.gpr[rd]
            };
            if byte {
                mmu.write_byte(addr, (val & 0xFF) as u8);
            } else {
                mmu.write_word(addr, val);
            }
            if writeback || !pre {
                self.registers.gpr[rn] = offset_addr;
            }
        }
        3
    }

    fn arm_halfword_transfer<B: CpuBus>(&mut self, inst: u32, mmu: &mut B) -> u32 {
        let pre = (inst & 0x0100_0000) != 0;
        let up = (inst & 0x0080_0000) != 0;
        let imm = (inst & 0x0040_0000) != 0; // bit22: 1 = immediate offset
        let writeback = (inst & 0x0020_0000) != 0;
        let load = (inst & 0x0010_0000) != 0;
        let rn = ((inst >> 16) & 0xF) as usize;
        let rd = ((inst >> 12) & 0xF) as usize;
        let sh = (inst >> 5) & 3; // 01 = H, 10 = SB, 11 = SH

        let offset = if imm {
            ((inst >> 4) & 0xF0) | (inst & 0xF) // hi nibble bits 11-8, lo nibble bits 3-0
        } else {
            self.registers.gpr[(inst & 0xF) as usize]
        };

        let base = self.registers.gpr[rn];
        let offset_addr = if up { base.wrapping_add(offset) } else { base.wrapping_sub(offset) };
        let addr = if pre { offset_addr } else { base };

        // ARMv5TE (ARM9) double-word transfers live in the L=0 half of this
        // space: SH=10 = LDRD, SH=11 = STRD (Rd even, moves the Rd/Rd+1 pair).
        // Decoding them as the v4 STRH fallback is catastrophic: an LDRD in a
        // copy loop then WRITES a halfword to the source instead of loading —
        // SoulSilver's overlay decompressor corrupted its own output this way.
        // ARMv4 (GBA/ARM7) keeps the old behavior (encodings unpredictable).
        if self.armv5 && !load && sh >= 2 {
            let rd2 = rd | 1; // odd partner of the (even) Rd pair
            if sh == 2 {
                // LDRD
                let lo = mmu.read_word(addr);
                let hi = mmu.read_word(addr.wrapping_add(4));
                if writeback || !pre {
                    self.registers.gpr[rn] = offset_addr;
                }
                self.registers.gpr[rd] = lo;
                self.registers.gpr[rd2] = hi;
            } else {
                // STRD
                mmu.write_word(addr, self.registers.gpr[rd]);
                mmu.write_word(addr.wrapping_add(4), self.registers.gpr[rd2]);
                if writeback || !pre {
                    self.registers.gpr[rn] = offset_addr;
                }
            }
            return 3;
        }

        if load {
            let val = match sh {
                1 => mmu.read_halfword(addr) as u32,                  // LDRH
                2 => (mmu.read_byte(addr) as i8) as i32 as u32,       // LDRSB
                3 => (mmu.read_halfword(addr) as i16) as i32 as u32,  // LDRSH
                _ => mmu.read_word(addr),                             // (unreached: SWP space)
            };
            if (writeback || !pre) && rd != rn {
                self.registers.gpr[rn] = offset_addr;
            }
            if rd == 15 {
                self.write_pc(val & !1, false);
            } else {
                self.registers.gpr[rd] = val;
            }
        } else {
            // Only STRH is a valid store in this space (v4, and v5 SH=01).
            mmu.write_halfword(addr, self.registers.gpr[rd] as u16);
            if writeback || !pre {
                self.registers.gpr[rn] = offset_addr;
            }
        }
        3
    }

    fn arm_block_transfer<B: CpuBus>(&mut self, inst: u32, mmu: &mut B) -> u32 {
        let pre = (inst & 0x0100_0000) != 0; // P
        let up = (inst & 0x0080_0000) != 0; // U
        let s_bit = (inst & 0x0040_0000) != 0; // S: user-bank / CPSR restore
        let writeback = (inst & 0x0020_0000) != 0; // W
        let load = (inst & 0x0010_0000) != 0; // L
        let rn = ((inst >> 16) & 0xF) as usize;
        let list = inst & 0xFFFF;
        let count = list.count_ones();
        if count == 0 {
            return 1; // empty list: rare/undefined, treat as no-op
        }

        // The lowest-numbered register always maps to the lowest address,
        // regardless of increment/decrement direction.
        let block = count * 4;
        let base = self.registers.gpr[rn];
        let (mut addr, final_base) = if up {
            (base, base.wrapping_add(block))
        } else {
            (base.wrapping_sub(block), base.wrapping_sub(block))
        };
        if up == pre {
            addr = addr.wrapping_add(4); // IB / DB: first access offset by a word
        }

        let transfer_pc = (list & 0x8000) != 0;
        let restore_cpsr = s_bit && load && transfer_pc;
        // S without R15-in-LDM (or any S-STM) accesses the USER-mode bank.
        let user_bank = s_bit && !restore_cpsr;
        let saved_mode = self.registers.get_mode();
        if user_bank {
            self.registers.swap_mode(saved_mode, CpuMode::User);
        }

        if load {
            for r in 0..16 {
                if (list >> r) & 1 != 0 {
                    let val = mmu.read_word(addr);
                    if r == 15 {
                        if restore_cpsr {
                            // `ldm {..,pc}^`: exception return — T comes from SPSR
                            // (restored below), never from the loaded bit 0.
                            self.write_pc(val & !1, false);
                        } else {
                            // Plain LDM pc: ARMv5T interworks on bit 0, ARMv4T does not.
                            self.load_pc(val);
                        }
                    } else {
                        self.registers.gpr[r as usize] = val;
                    }
                    addr = addr.wrapping_add(4);
                }
            }
        } else {
            let mut first = true;
            for r in 0..16 {
                if (list >> r) & 1 != 0 {
                    let val = if r == 15 {
                        self.registers.gpr[15].wrapping_add(4) // STM stores PC + 12
                    } else if r as usize == rn && !first && writeback {
                        final_base
                    } else {
                        self.registers.gpr[r as usize]
                    };
                    mmu.write_word(addr, val);
                    addr = addr.wrapping_add(4);
                    first = false;
                }
            }
        }

        if user_bank {
            self.registers.swap_mode(CpuMode::User, saved_mode);
        }
        if restore_cpsr {
            self.restore_cpsr_from_spsr();
        }
        // Writeback, unless an LDM reloaded the base itself.
        if writeback && !(load && (list >> rn) & 1 != 0) {
            self.registers.gpr[rn] = final_base;
        }
        count + 2
    }

    fn arm_multiply(&mut self, inst: u32) -> u32 {
        let accumulate = (inst & 0x0020_0000) != 0;
        let s = (inst & 0x0010_0000) != 0;
        let rd = ((inst >> 16) & 0xF) as usize;
        let rn = ((inst >> 12) & 0xF) as usize;
        let rs = ((inst >> 8) & 0xF) as usize;
        let rm = (inst & 0xF) as usize;
        let mut result = self.registers.gpr[rm].wrapping_mul(self.registers.gpr[rs]);
        if accumulate {
            result = result.wrapping_add(self.registers.gpr[rn]);
        }
        self.registers.gpr[rd] = result;
        if s {
            self.set_nz(result); // C/V unaffected on ARM7TDMI
        }
        4
    }

    fn arm_multiply_long(&mut self, inst: u32) -> u32 {
        let signed = (inst & 0x0040_0000) != 0;
        let accumulate = (inst & 0x0020_0000) != 0;
        let s = (inst & 0x0010_0000) != 0;
        let rd_hi = ((inst >> 16) & 0xF) as usize;
        let rd_lo = ((inst >> 12) & 0xF) as usize;
        let rs = ((inst >> 8) & 0xF) as usize;
        let rm = (inst & 0xF) as usize;
        let a = self.registers.gpr[rm];
        let b = self.registers.gpr[rs];
        let mut result: u64 = if signed {
            ((a as i32 as i64).wrapping_mul(b as i32 as i64)) as u64
        } else {
            (a as u64).wrapping_mul(b as u64)
        };
        if accumulate {
            let acc = ((self.registers.gpr[rd_hi] as u64) << 32) | self.registers.gpr[rd_lo] as u64;
            result = result.wrapping_add(acc);
        }
        self.registers.gpr[rd_lo] = result as u32;
        self.registers.gpr[rd_hi] = (result >> 32) as u32;
        if s {
            self.registers.set_flag(FLAG_N, (result >> 63) & 1 != 0);
            self.registers.set_flag(FLAG_Z, result == 0);
        }
        5
    }

    fn arm_swap<B: CpuBus>(&mut self, inst: u32, mmu: &mut B) -> u32 {
        let byte = (inst & 0x0040_0000) != 0;
        let rn = ((inst >> 16) & 0xF) as usize;
        let rd = ((inst >> 12) & 0xF) as usize;
        let rm = (inst & 0xF) as usize;
        let addr = self.registers.gpr[rn];
        if byte {
            let old = mmu.read_byte(addr) as u32;
            mmu.write_byte(addr, (self.registers.gpr[rm] & 0xFF) as u8);
            self.registers.gpr[rd] = old;
        } else {
            let old = mmu.read_word(addr);
            mmu.write_word(addr, self.registers.gpr[rm]);
            self.registers.gpr[rd] = old;
        }
        4
    }

    fn arm_psr_transfer(&mut self, inst: u32) -> u32 {
        let use_spsr = (inst & 0x0040_0000) != 0; // bit22: 1 = SPSR
        let is_msr = (inst & 0x0020_0000) != 0; // bit21: 1 = MSR (write)
        if !is_msr {
            // MRS Rd, PSR
            let rd = ((inst >> 12) & 0xF) as usize;
            self.registers.gpr[rd] = if use_spsr { self.registers.spsr } else { self.registers.cpsr };
            return 1;
        }

        // MSR PSR_field, operand
        let operand = if (inst & 0x0200_0000) != 0 {
            let imm = inst & 0xFF;
            let rot = ((inst >> 8) & 0xF) * 2;
            imm.rotate_right(rot)
        } else {
            self.registers.gpr[(inst & 0xF) as usize]
        };
        let mut mask = 0u32;
        if (inst & 0x0008_0000) != 0 {
            mask |= 0xFF00_0000; // flags byte
        }
        if (inst & 0x0001_0000) != 0 {
            mask |= 0x0000_00FF; // control byte (mode + I/F/T)
        }

        if use_spsr {
            self.registers.spsr = (self.registers.spsr & !mask) | (operand & mask);
        } else {
            let new_cpsr = (self.registers.cpsr & !mask) | (operand & mask);
            // Switch banks if the mode field actually changed.
            if (mask & 0xFF) != 0 {
                let old_mode = self.registers.get_mode();
                let new_mode = Self::mode_from_bits(new_cpsr);
                if new_mode != old_mode {
                    self.registers.swap_mode(old_mode, new_mode);
                }
            }
            // MSR never toggles the THUMB execution state.
            let t = self.registers.cpsr & FLAG_T;
            self.registers.cpsr = (new_cpsr & !FLAG_T) | t;
        }
        1
    }

    // --- THUMB Interpreter ---
    pub(crate) fn execute_thumb<B: CpuBus>(&mut self, inst: u16, mmu: &mut B) -> u32 {
        let i = inst as u32;
        // Decoded most-specific first so overlapping masks resolve correctly.
        if (i & 0xF800) == 0x1800 {
            self.thumb_add_sub(inst) // F2
        } else if (i & 0xE000) == 0x0000 {
            self.thumb_move_shifted(inst) // F1
        } else if (i & 0xE000) == 0x2000 {
            self.thumb_alu_imm8(inst) // F3
        } else if (i & 0xFC00) == 0x4000 {
            self.thumb_alu(inst) // F4
        } else if (i & 0xFC00) == 0x4400 {
            self.thumb_hi_reg(inst) // F5
        } else if (i & 0xF800) == 0x4800 {
            self.thumb_pc_load(inst, mmu) // F6
        } else if (i & 0xF200) == 0x5000 {
            self.thumb_ldst_reg(inst, mmu) // F7
        } else if (i & 0xF200) == 0x5200 {
            self.thumb_ldst_sign(inst, mmu) // F8
        } else if (i & 0xE000) == 0x6000 {
            self.thumb_ldst_imm(inst, mmu) // F9
        } else if (i & 0xF000) == 0x8000 {
            self.thumb_ldst_half(inst, mmu) // F10
        } else if (i & 0xF000) == 0x9000 {
            self.thumb_sp_ldst(inst, mmu) // F11
        } else if (i & 0xF000) == 0xA000 {
            self.thumb_load_address(inst) // F12
        } else if (i & 0xFF00) == 0xB000 {
            self.thumb_adjust_sp(inst) // F13
        } else if (i & 0xF600) == 0xB400 {
            self.thumb_push_pop(inst, mmu) // F14
        } else if (i & 0xF000) == 0xC000 {
            self.thumb_block(inst, mmu) // F15
        } else if (i & 0xFF00) == 0xDF00 {
            self.handle_swi((inst & 0xFF) as u8, mmu); // SWI
            3
        } else if (i & 0xF000) == 0xD000 {
            self.thumb_cond_branch(inst) // F16
        } else if self.armv5 && (i & 0xF800) == 0xE800 {
            self.thumb_blx_suffix(inst) // BLX suffix (ARMv5, ARM9)
        } else if (i & 0xF800) == 0xE000 {
            self.thumb_branch(inst) // F18
        } else if (i & 0xF000) == 0xF000 {
            self.thumb_long_branch(inst) // F19
        } else {
            1 // undefined
        }
    }

    fn thumb_move_shifted(&mut self, inst: u16) -> u32 {
        let op = ((inst >> 11) & 3) as u32;
        let amount = ((inst >> 6) & 0x1F) as u32;
        let rs = ((inst >> 3) & 7) as usize;
        let rd = (inst & 7) as usize;
        let (result, carry) =
            Self::barrel_shift(self.registers.gpr[rs], op, amount, self.registers.get_flag(FLAG_C));
        self.registers.gpr[rd] = result;
        self.set_nz(result);
        self.registers.set_flag(FLAG_C, carry);
        1
    }

    fn thumb_add_sub(&mut self, inst: u16) -> u32 {
        let imm = (inst & 0x0400) != 0;
        let sub = (inst & 0x0200) != 0;
        let rn_off = ((inst >> 6) & 7) as u32;
        let rs = ((inst >> 3) & 7) as usize;
        let rd = (inst & 7) as usize;
        let a = self.registers.gpr[rs];
        let b = if imm { rn_off } else { self.registers.gpr[rn_off as usize] };
        self.registers.gpr[rd] = if sub { self.adc(a, !b, 1, true) } else { self.adc(a, b, 0, true) };
        1
    }

    fn thumb_alu_imm8(&mut self, inst: u16) -> u32 {
        let op = (inst >> 11) & 3;
        let rd = ((inst >> 8) & 7) as usize;
        let imm = (inst & 0xFF) as u32;
        let a = self.registers.gpr[rd];
        match op {
            0 => { self.registers.gpr[rd] = imm; self.set_nz(imm); } // MOV
            1 => { self.adc(a, !imm, 1, true); }                     // CMP
            2 => { self.registers.gpr[rd] = self.adc(a, imm, 0, true); } // ADD
            _ => { self.registers.gpr[rd] = self.adc(a, !imm, 1, true); } // SUB
        }
        1
    }

    fn thumb_alu(&mut self, inst: u16) -> u32 {
        let op = (inst >> 6) & 0xF;
        let rs = ((inst >> 3) & 7) as usize;
        let rd = (inst & 7) as usize;
        let a = self.registers.gpr[rd];
        let b = self.registers.gpr[rs];
        let carry_in = self.registers.get_flag(FLAG_C);

        // Helper for the register-amount shift operations (LSL/LSR/ASR/ROR).
        let do_shift = |cpu: &mut Self, shift_type: u32| {
            let (r, c) = Self::reg_shift(a, shift_type, b & 0xFF, carry_in);
            cpu.registers.gpr[rd] = r;
            cpu.set_nz(r);
            cpu.registers.set_flag(FLAG_C, c);
        };

        match op {
            0x0 => { let r = a & b; self.registers.gpr[rd] = r; self.set_nz(r); } // AND
            0x1 => { let r = a ^ b; self.registers.gpr[rd] = r; self.set_nz(r); } // EOR
            0x2 => do_shift(self, 0),                                            // LSL
            0x3 => do_shift(self, 1),                                            // LSR
            0x4 => do_shift(self, 2),                                            // ASR
            0x5 => { self.registers.gpr[rd] = self.adc(a, b, carry_in as u32, true); } // ADC
            0x6 => { self.registers.gpr[rd] = self.adc(a, !b, carry_in as u32, true); } // SBC
            0x7 => do_shift(self, 3),                                            // ROR
            0x8 => { let r = a & b; self.set_nz(r); }                            // TST
            0x9 => { self.registers.gpr[rd] = self.adc(0, !b, 1, true); }        // NEG (0 - b)
            0xA => { self.adc(a, !b, 1, true); }                                 // CMP
            0xB => { self.adc(a, b, 0, true); }                                  // CMN
            0xC => { let r = a | b; self.registers.gpr[rd] = r; self.set_nz(r); } // ORR
            0xD => { let r = a.wrapping_mul(b); self.registers.gpr[rd] = r; self.set_nz(r); } // MUL
            0xE => { let r = a & !b; self.registers.gpr[rd] = r; self.set_nz(r); } // BIC
            _ => { let r = !b; self.registers.gpr[rd] = r; self.set_nz(r); }     // MVN
        }
        1
    }

    fn thumb_hi_reg(&mut self, inst: u16) -> u32 {
        let op = (inst >> 8) & 3;
        let h1 = ((inst >> 7) & 1) as usize;
        let h2 = ((inst >> 6) & 1) as usize;
        let rs = ((inst >> 3) & 7) as usize + h2 * 8;
        let rd = (inst & 7) as usize + h1 * 8;
        let a = self.registers.gpr[rd];
        let b = self.registers.gpr[rs];
        match op {
            0 => {
                let r = a.wrapping_add(b); // ADD (no flags)
                if rd == 15 { self.write_pc(r & !1, false); } else { self.registers.gpr[rd] = r; }
            }
            1 => { self.adc(a, !b, 1, true); } // CMP (flags only)
            2 => {
                if rd == 15 { self.write_pc(b & !1, false); } else { self.registers.gpr[rd] = b; } // MOV
            }
            _ => {
                // BX/BLX(reg) (interworking). On ARMv5T bit 7 (h1) selects
                // BLX: link LR = next instruction | 1 BEFORE the jump —
                // without it a callee's `BX LR` returns to the *previous*
                // call site, re-running that epilogue and popping stack data
                // as PC. ARMv4T (GBA/ARM7) has no Thumb BLX(reg): keep plain
                // BX there (h1 is unpredictable on real v4T hardware).
                if self.armv5 && h1 == 1 {
                    self.registers.gpr[14] = self.registers.gpr[15].wrapping_sub(2) | 1;
                }
                self.write_pc(b, true); // BX (interworking)
            }
        }
        1
    }

    fn thumb_pc_load<B: CpuBus>(&mut self, inst: u16, mmu: &mut B) -> u32 {
        let rd = ((inst >> 8) & 7) as usize;
        let off = ((inst & 0xFF) as u32) << 2;
        let addr = (self.registers.gpr[15] & !2).wrapping_add(off);
        self.registers.gpr[rd] = mmu.read_word(addr);
        3
    }

    fn thumb_ldst_reg<B: CpuBus>(&mut self, inst: u16, mmu: &mut B) -> u32 {
        let load = (inst & 0x0800) != 0;
        let byte = (inst & 0x0400) != 0;
        let ro = ((inst >> 6) & 7) as usize;
        let rb = ((inst >> 3) & 7) as usize;
        let rd = (inst & 7) as usize;
        let addr = self.registers.gpr[rb].wrapping_add(self.registers.gpr[ro]);
        if load {
            self.registers.gpr[rd] = if byte { mmu.read_byte(addr) as u32 } else { mmu.read_word(addr) };
        } else if byte {
            mmu.write_byte(addr, (self.registers.gpr[rd] & 0xFF) as u8);
        } else {
            mmu.write_word(addr, self.registers.gpr[rd]);
        }
        3
    }

    fn thumb_ldst_sign<B: CpuBus>(&mut self, inst: u16, mmu: &mut B) -> u32 {
        let h = (inst & 0x0800) != 0;
        let s = (inst & 0x0400) != 0;
        let ro = ((inst >> 6) & 7) as usize;
        let rb = ((inst >> 3) & 7) as usize;
        let rd = (inst & 7) as usize;
        let addr = self.registers.gpr[rb].wrapping_add(self.registers.gpr[ro]);
        match (s, h) {
            (false, false) => mmu.write_halfword(addr, self.registers.gpr[rd] as u16), // STRH
            (false, true) => self.registers.gpr[rd] = mmu.read_halfword(addr) as u32,  // LDRH
            (true, false) => self.registers.gpr[rd] = (mmu.read_byte(addr) as i8) as i32 as u32, // LDRSB
            (true, true) => self.registers.gpr[rd] = (mmu.read_halfword(addr) as i16) as i32 as u32, // LDRSH
        }
        3
    }

    fn thumb_ldst_imm<B: CpuBus>(&mut self, inst: u16, mmu: &mut B) -> u32 {
        let byte = (inst & 0x1000) != 0;
        let load = (inst & 0x0800) != 0;
        let off5 = ((inst >> 6) & 0x1F) as u32;
        let rb = ((inst >> 3) & 7) as usize;
        let rd = (inst & 7) as usize;
        let base = self.registers.gpr[rb];
        if byte {
            let addr = base.wrapping_add(off5);
            if load {
                self.registers.gpr[rd] = mmu.read_byte(addr) as u32;
            } else {
                mmu.write_byte(addr, (self.registers.gpr[rd] & 0xFF) as u8);
            }
        } else {
            let addr = base.wrapping_add(off5 << 2);
            if load {
                self.registers.gpr[rd] = mmu.read_word(addr);
            } else {
                mmu.write_word(addr, self.registers.gpr[rd]);
            }
        }
        3
    }

    fn thumb_ldst_half<B: CpuBus>(&mut self, inst: u16, mmu: &mut B) -> u32 {
        let load = (inst & 0x0800) != 0;
        let off = (((inst >> 6) & 0x1F) as u32) << 1;
        let rb = ((inst >> 3) & 7) as usize;
        let rd = (inst & 7) as usize;
        let addr = self.registers.gpr[rb].wrapping_add(off);
        if load {
            self.registers.gpr[rd] = mmu.read_halfword(addr) as u32;
        } else {
            mmu.write_halfword(addr, self.registers.gpr[rd] as u16);
        }
        3
    }

    fn thumb_sp_ldst<B: CpuBus>(&mut self, inst: u16, mmu: &mut B) -> u32 {
        let load = (inst & 0x0800) != 0;
        let rd = ((inst >> 8) & 7) as usize;
        let off = ((inst & 0xFF) as u32) << 2;
        let addr = self.registers.gpr[13].wrapping_add(off);
        if load {
            self.registers.gpr[rd] = mmu.read_word(addr);
        } else {
            mmu.write_word(addr, self.registers.gpr[rd]);
        }
        3
    }

    fn thumb_load_address(&mut self, inst: u16) -> u32 {
        let use_sp = (inst & 0x0800) != 0;
        let rd = ((inst >> 8) & 7) as usize;
        let off = ((inst & 0xFF) as u32) << 2;
        let base = if use_sp { self.registers.gpr[13] } else { self.registers.gpr[15] & !2 };
        self.registers.gpr[rd] = base.wrapping_add(off);
        1
    }

    fn thumb_adjust_sp(&mut self, inst: u16) -> u32 {
        let off = ((inst & 0x7F) as u32) << 2;
        if (inst & 0x80) != 0 {
            self.registers.gpr[13] = self.registers.gpr[13].wrapping_sub(off);
        } else {
            self.registers.gpr[13] = self.registers.gpr[13].wrapping_add(off);
        }
        1
    }

    fn thumb_push_pop<B: CpuBus>(&mut self, inst: u16, mmu: &mut B) -> u32 {
        let pop = (inst & 0x0800) != 0;
        let pc_lr = (inst & 0x0100) != 0;
        let list = (inst & 0xFF) as u32;
        let mut sp = self.registers.gpr[13];
        if pop {
            for r in 0..8 {
                if (list >> r) & 1 != 0 {
                    self.registers.gpr[r] = mmu.read_word(sp);
                    sp = sp.wrapping_add(4);
                }
            }
            if pc_lr {
                let val = mmu.read_word(sp);
                sp = sp.wrapping_add(4);
                self.registers.gpr[13] = sp;
                // POP {pc}: ARMv5T interworks on bit 0 (a Thumb function can return
                // to an ARM caller); ARMv4T stays in THUMB.
                self.load_pc(val);
            } else {
                self.registers.gpr[13] = sp;
            }
        } else {
            let count = list.count_ones() + pc_lr as u32;
            sp = sp.wrapping_sub(count * 4);
            self.registers.gpr[13] = sp;
            let mut addr = sp;
            for r in 0..8 {
                if (list >> r) & 1 != 0 {
                    mmu.write_word(addr, self.registers.gpr[r]);
                    addr = addr.wrapping_add(4);
                }
            }
            if pc_lr {
                mmu.write_word(addr, self.registers.gpr[14]); // push LR
            }
        }
        3
    }

    fn thumb_block<B: CpuBus>(&mut self, inst: u16, mmu: &mut B) -> u32 {
        let load = (inst & 0x0800) != 0;
        let rb = ((inst >> 8) & 7) as usize;
        let list = (inst & 0xFF) as u32;
        let mut addr = self.registers.gpr[rb];
        if list == 0 {
            return 1; // empty list: rare/undefined, treat as no-op
        }
        if load {
            for r in 0..8 {
                if (list >> r) & 1 != 0 {
                    self.registers.gpr[r] = mmu.read_word(addr);
                    addr = addr.wrapping_add(4);
                }
            }
            if (list >> rb) & 1 == 0 {
                self.registers.gpr[rb] = addr; // writeback unless Rb was loaded
            }
        } else {
            let final_addr = addr.wrapping_add(list.count_ones() * 4);
            let mut first = true;
            for r in 0..8 {
                if (list >> r) & 1 != 0 {
                    let val = if r == rb && !first { final_addr } else { self.registers.gpr[r] };
                    mmu.write_word(addr, val);
                    addr = addr.wrapping_add(4);
                    first = false;
                }
            }
            self.registers.gpr[rb] = final_addr;
        }
        3
    }

    fn thumb_cond_branch(&mut self, inst: u16) -> u32 {
        let cond = ((inst >> 8) & 0xF) as u32;
        if !self.check_condition(cond) {
            return 1;
        }
        let off = ((inst & 0xFF) as i8 as i32) << 1;
        let target = (self.registers.gpr[15] as i32).wrapping_add(off) as u32;
        self.write_pc(target, false);
        3
    }

    fn thumb_branch(&mut self, inst: u16) -> u32 {
        let mut off = (inst & 0x07FF) as i32;
        if off & 0x0400 != 0 {
            off |= !0x07FF; // sign-extend 11 -> 32
        }
        let target = (self.registers.gpr[15] as i32).wrapping_add(off << 1) as u32;
        self.write_pc(target, false);
        3
    }

    fn thumb_long_branch(&mut self, inst: u16) -> u32 {
        let off = (inst & 0x07FF) as u32;
        if (inst & 0x0800) == 0 {
            // First half: LR = PC + (sign-extended offset << 12).
            let mut s = off as i32;
            if s & 0x0400 != 0 {
                s |= !0x07FF;
            }
            self.registers.gpr[14] = self.registers.gpr[15].wrapping_add((s << 12) as u32);
            1
        } else {
            // Second half: PC = LR + (offset << 1); LR = return address | 1.
            let target = self.registers.gpr[14].wrapping_add(off << 1);
            self.registers.gpr[14] = self.registers.gpr[15].wrapping_sub(2) | 1;
            self.write_pc(target & !1, false); // stay in THUMB
            3
        }
    }

    /// BLX suffix (ARMv5, ARM9 only): pairs with the shared `11110`-prefix half
    /// (which already set LR = PC + off_hi<<12). Computes the word-aligned ARM
    /// target and switches to ARM state.
    fn thumb_blx_suffix(&mut self, inst: u16) -> u32 {
        let off = (inst & 0x07FF) as u32;
        let target = self.registers.gpr[14].wrapping_add(off << 1) & !3;
        self.registers.gpr[14] = self.registers.gpr[15].wrapping_sub(2) | 1;
        self.registers.set_flag(FLAG_T, false); // exchange to ARM state
        self.write_pc(target, false);
        3
    }

    fn check_condition(&self, cond: u32) -> bool {
        match cond {
            0x0 => self.registers.get_flag(FLAG_Z),  // EQ
            0x1 => !self.registers.get_flag(FLAG_Z), // NE
            0x2 => self.registers.get_flag(FLAG_C),  // CS/HS
            0x3 => !self.registers.get_flag(FLAG_C), // CC/LO
            0x4 => self.registers.get_flag(FLAG_N),  // MI
            0x5 => !self.registers.get_flag(FLAG_N), // PL
            0x6 => self.registers.get_flag(FLAG_V),  // VS
            0x7 => !self.registers.get_flag(FLAG_V), // VC
            0x8 => self.registers.get_flag(FLAG_C) && !self.registers.get_flag(FLAG_Z), // HI
            0x9 => !self.registers.get_flag(FLAG_C) || self.registers.get_flag(FLAG_Z), // LS
            0xA => self.registers.get_flag(FLAG_N) == self.registers.get_flag(FLAG_V), // GE
            0xB => self.registers.get_flag(FLAG_N) != self.registers.get_flag(FLAG_V), // LT
            0xC => {
                !self.registers.get_flag(FLAG_Z)
                    && (self.registers.get_flag(FLAG_N) == self.registers.get_flag(FLAG_V))
            } // GT
            0xD => {
                self.registers.get_flag(FLAG_Z)
                    || (self.registers.get_flag(FLAG_N) != self.registers.get_flag(FLAG_V))
            } // LE
            0xE => true,                             // AL
            _ => true,
        }
    }

    // --- SWI BIOS HLE Subsystem ---
    pub fn handle_swi<B: CpuBus>(&mut self, comment: u8, mmu: &mut B) {
        if self.swi_mode == SwiMode::Nds {
            return self.handle_swi_nds(comment, mmu);
        }
        match comment {
            0x00 => self.hle_soft_reset(mmu),
            0x01 => self.hle_register_ram_reset(mmu),
            0x02 | 0x03 => self.halted = true, // Halt / Stop: wait for an interrupt
            0x04 => self.hle_intr_wait(mmu),
            0x05 => self.hle_vblank_intr_wait(mmu),
            0x06 => self.hle_div(),
            0x07 => self.hle_div_arm(),
            0x08 => self.hle_sqrt(),
            0x09 => self.hle_arctan(),
            0x0A => self.hle_arctan2(),
            0x0B => self.hle_cpu_set(mmu),
            0x0C => self.hle_cpu_fast_set(mmu),
            0x0D => self.hle_get_bios_checksum(),
            0x0E => self.hle_bg_affine_set(mmu),
            0x0F => self.hle_obj_affine_set(mmu),
            0x11 | 0x12 => self.hle_lz77_uncomp(mmu), // LZ77 -> WRAM / VRAM
            0x13 => self.hle_huff_uncomp(mmu),        // Huffman
            0x14 | 0x15 => self.hle_rl_uncomp(mmu),   // run-length -> WRAM / VRAM
            _ => {
                eprintln!("Unhandled BIOS SWI call: 0x{:02X}", comment);
            }
        }
    }

    /// NDS BIOS SWI HLE (GBATEK "BIOS Functions", NDS7/NDS9 tables).
    ///
    /// Every arm is here because a real cartridge was measured calling it. The
    /// default arm warns instead of returning silently: an unimplemented SWI
    /// leaves `r0` holding whatever the caller passed in, and the caller then
    /// uses that as the BIOS's answer. That is not a hypothetical — SoulSilver
    /// calls `GetPitchTable`/`GetVolumeTable` 18973 times each over 900 frames,
    /// and getting its own index back collapsed every musical interval inside an
    /// octave to under 20 cents (see [`crate::nds::sound_tables`]).
    fn handle_swi_nds<B: CpuBus>(&mut self, comment: u8, mmu: &mut B) {
        match comment {
            // WaitByLoop (03h): the SDK's busy delay. Deliberately a no-op.
            // ponytail: `handle_swi` cannot report consumed cycles back to the
            // run loop, and the ARM7 calls this 303084 times per 900 frames
            // (measured), so charging it must be exact and cheap at once.
            // Ceiling: SDK settling delays (SPI, RTC) complete instantly.
            // Upgrade path: return a cycle count from `handle_swi`, charge r0*4.
            0x03 => {}
            0x04 | 0x05 | 0x06 => self.halted = true,
            // Div (GBATEK NDS SWI 09h): r0/r1 -> r0 = quotient, r1 =
            // remainder, r3 = |quotient|. Neither NDS core has a divide
            // instruction; the SDK routes all integer division here. The TP
            // calibrate-param computation divides the ADC spans through this
            // call — as a no-op the dot factors came out zero even after
            // the settings copy was accepted (U28), so touch calibration
            // stayed (0,0). Division by zero leaves the registers untouched
            // (real BIOS returns garbage; games never divide by zero here).
            0x09 => {
                let num = self.registers.gpr[0] as i32;
                let den = self.registers.gpr[1] as i32;
                if den != 0 {
                    let q = num.wrapping_div(den);
                    self.registers.gpr[0] = q as u32;
                    self.registers.gpr[1] = num.wrapping_rem(den) as u32;
                    self.registers.gpr[3] = q.unsigned_abs();
                }
            }
            // GetCRC16 (GBATEK SWI 0Eh): r0 = initial value, r1 = source,
            // r2 = length in bytes; returns the CRC in r0 (poly 0xA001,
            // LSB-first). The dead-UI-touch saga terminated here: the ARM7
            // settings validator CRCs the firmware user-settings copies via
            // this BIOS call (thunk 0x038008F4 = `svc #0x0E`); as a no-op it
            // returned the init value, both copies failed validation, and
            // the SDK memset zero defaults over 0x027FFC80 — so the TP
            // calibration params were zero and TP_GetCalibratedPoint mapped
            // every pen sample to (0,0), killing all UI hit-tests.
            0x0E => {
                let mut crc = self.registers.gpr[0] & 0xFFFF;
                let src = self.registers.gpr[1];
                let len = self.registers.gpr[2];
                for i in 0..len {
                    crc ^= mmu.read_byte_safe(src.wrapping_add(i)) as u32;
                    for _ in 0..8 {
                        crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
                    }
                }
                self.registers.gpr[0] = crc;
            }
            // CpuSet (0Bh) / CpuFastSet (0Ch) are the same block copy/fill the
            // GBA BIOS provides, at the same SWI numbers and with the same
            // control word, so they share the implementation rather than being
            // duplicated for the NDS table.
            0x0B => self.hle_cpu_set(mmu),
            0x0C => self.hle_cpu_fast_set(mmu),
            // Sqrt (0Dh on the NDS table; the GBA puts it at 08h). Same integer
            // square root, so it shares the implementation. Unimplemented it
            // returned the operand in r0 — the identical silent-wrong-value
            // failure mode as the sound tables, and nothing in this cartridge's
            // SWI census calls it, which is exactly why it went unnoticed.
            0x0D => self.hle_sqrt(),
            // Sound tables (1Ah GetSineTable, 1Bh GetPitchTable, 1Ch
            // GetVolumeTable): the NitroSDK sound driver asks the BIOS for the
            // pitch and volume of every note it starts. See the module docs for
            // what returning `r0` unchanged sounded like.
            0x1A => {
                self.registers.gpr[0] =
                    u32::from(crate::nds::sound_tables::sine(self.registers.gpr[0]))
            }
            0x1B => {
                self.registers.gpr[0] =
                    u32::from(crate::nds::sound_tables::pitch(self.registers.gpr[0]))
            }
            0x1C => {
                self.registers.gpr[0] =
                    u32::from(crate::nds::sound_tables::volume(self.registers.gpr[0]))
            }
            _ => {
                // Warn ONCE per SWI number. Silence here is what hid the sound
                // tables for six sessions of audio debugging: the caller cannot
                // tell "not implemented" from "the BIOS answered", so the defect
                // surfaces only as wrong output somewhere else entirely. Bounded
                // to 256 lines for the whole process, so it is safe in a hot
                // loop. One bit per `comment` value across four words rather than
                // `1 << (comment & 31)`, which aliased numbers 32 apart and would
                // have silenced the second of any such pair — the exact failure
                // mode this arm exists to prevent.
                use std::sync::atomic::{AtomicU64, Ordering};
                static WARNED: [AtomicU64; 4] = [
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                    AtomicU64::new(0),
                ];
                let word = &WARNED[(comment >> 6) as usize];
                let bit = 1u64 << (comment & 63);
                if word.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
                    eprintln!(
                        "Unimplemented NDS BIOS SWI 0x{comment:02X}: returning with r0 unchanged"
                    );
                }
            }
        }
    }

    /// SoftReset (SWI 0x00): clear the user/system stacks, jump to the entry the
    /// boot flag selects, and return to System mode in ARM state.
    fn hle_soft_reset<B: CpuBus>(&mut self, mmu: &mut B) {
        // 0x03007FFA selects the entry: 0 = ROM (0x08000000), non-zero = EWRAM.
        let to_ewram = mmu.read_byte_safe(0x0300_7FFA) != 0;
        let entry = if to_ewram { 0x0200_0000 } else { 0x0800_0000 };
        self.registers = CpuRegisters::new();
        self.registers.cpsr = CpuMode::System as u32;
        self.registers.gpr[15] = entry;
        self.pc_modified = true;
    }

    /// Flush decompressed bytes 16-bit-bus-safe. VRAM/palette/OAM have a 16-bit
    /// bus: routing decompressed output through `write_byte` would trigger the
    /// duplication quirk (each byte fanned across a halfword, low byte lost) for
    /// BG VRAM and drop it entirely for OBJ VRAM — the cause of the garbled
    /// backgrounds. Halfword writes store both bytes verbatim in every region, so
    /// this is correct for the WRAM (0x11/0x14) and VRAM (0x12/0x15) variants
    /// alike. A trailing odd byte (only possible for WRAM targets) goes byte-wise.
    fn flush_uncomp<B: CpuBus>(mmu: &mut B, dest: u32, data: &[u8]) {
        let mut addr = dest;
        let mut chunks = data.chunks_exact(2);
        for c in &mut chunks {
            mmu.write_halfword_safe(addr, u16::from_le_bytes([c[0], c[1]]));
            addr += 2;
        }
        if let [b] = chunks.remainder() {
            mmu.write_byte_safe(addr, *b);
        }
    }

    /// LZ77 decompression (SWI 0x11/0x12). Header word at the source: bits 8-31
    /// hold the decompressed size; data follows as flag-byte + 8 blocks. Output
    /// is buffered so back-references resolve from bytes we produced (not from a
    /// 16-bit-bus VRAM read-back) and the whole run flushes via halfword writes.
    fn hle_lz77_uncomp<B: CpuBus>(&mut self, mmu: &mut B) {
        let mut src = self.registers.gpr[0] & !3;
        let dest = self.registers.gpr[1];
        let header = mmu.read_word_safe(src);
        src += 4;
        let limit = ((header >> 8) & 0x00FF_FFFF).min(0x40000) as usize; // bound output (256KB)
        let mut out: Vec<u8> = Vec::with_capacity(limit);

        while out.len() < limit {
            let flags = mmu.read_byte_safe(src);
            src += 1;
            for bit in 0..8 {
                if out.len() >= limit {
                    break;
                }
                if (flags >> (7 - bit)) & 1 == 0 {
                    // Literal byte.
                    out.push(mmu.read_byte_safe(src));
                    src += 1;
                } else {
                    // Back-reference: 2 bytes -> length (3..18) + 12-bit distance.
                    let b0 = mmu.read_byte_safe(src) as usize;
                    let b1 = mmu.read_byte_safe(src + 1) as usize;
                    src += 2;
                    let length = (b0 >> 4) + 3;
                    let disp = (((b0 & 0xF) << 8) | b1) + 1;
                    for _ in 0..length {
                        if out.len() >= limit {
                            break;
                        }
                        // Out-of-range displacement (malformed stream) reads 0.
                        let byte = out.get(out.len().wrapping_sub(disp)).copied().unwrap_or(0);
                        out.push(byte);
                    }
                }
            }
        }
        Self::flush_uncomp(mmu, dest, &out);
    }

    /// Run-length decompression (SWI 0x14/0x15). Header word holds the size;
    /// each block is a flag byte: top bit set -> run, clear -> literal copy.
    /// Buffered + halfword-flushed for the same 16-bit-bus reason as LZ77.
    fn hle_rl_uncomp<B: CpuBus>(&mut self, mmu: &mut B) {
        let mut src = self.registers.gpr[0] & !3;
        let dest = self.registers.gpr[1];
        let header = mmu.read_word_safe(src);
        src += 4;
        let limit = ((header >> 8) & 0x00FF_FFFF).min(0x40000) as usize;
        let mut out: Vec<u8> = Vec::with_capacity(limit);

        while out.len() < limit {
            let flag = mmu.read_byte_safe(src);
            src += 1;
            if flag & 0x80 != 0 {
                let length = (flag & 0x7F) as usize + 3;
                let byte = mmu.read_byte_safe(src);
                src += 1;
                for _ in 0..length {
                    if out.len() >= limit {
                        break;
                    }
                    out.push(byte);
                }
            } else {
                let length = (flag & 0x7F) as usize + 1;
                for _ in 0..length {
                    if out.len() >= limit {
                        break;
                    }
                    out.push(mmu.read_byte_safe(src));
                    src += 1;
                }
            }
        }
        Self::flush_uncomp(mmu, dest, &out);
    }

    /// Huffman decompression (SWI 0x13). Header: bits 0-3 = symbol bit-width,
    /// bits 8-31 = output size. A tree table precedes a bitstream read MSB-first.
    fn hle_huff_uncomp<B: CpuBus>(&mut self, mmu: &mut B) {
        let src = self.registers.gpr[0] & !3;
        let mut dest = self.registers.gpr[1];
        let header = mmu.read_word_safe(src);
        let sym_bits = header & 0xF;
        let out_size = ((header >> 8) & 0x00FF_FFFF).min(0x40000);
        if sym_bits == 0 || sym_bits > 8 {
            return; // only 1..8-bit symbols are produced by the GBA encoder
        }

        let tree_base = src + 4;
        let tree_size = (mmu.read_byte_safe(tree_base) as u32 + 1) * 2;
        let mut stream = tree_base + tree_size; // 32-bit words follow the tree

        let mut node = tree_base + 1; // root is at tree_base+1
        let mut out_word = 0u32;
        let mut out_bits = 0u32;
        let mut written = 0u32;
        let mut cur = mmu.read_word_safe(stream);
        stream += 4;
        let mut bit = 32u32;

        // Guard against malformed streams running unbounded.
        let mut guard = out_size.saturating_mul(64) + 256;
        while written < out_size && guard > 0 {
            guard -= 1;
            if bit == 0 {
                cur = mmu.read_word_safe(stream);
                stream += 4;
                bit = 32;
            }
            bit -= 1;
            let direction = (cur >> bit) & 1;
            let node_val = mmu.read_byte_safe(node);
            // Children sit at the next even offset after the current node.
            let next_base = (node & !1).wrapping_add(((node_val & 0x3F) as u32 + 1) * 2);
            let child = next_base + direction;
            let is_leaf = (node_val >> (6 + direction)) & 1 != 0;
            if is_leaf {
                let symbol = mmu.read_byte_safe(child) as u32;
                out_word |= (symbol & ((1 << sym_bits) - 1)) << out_bits;
                out_bits += sym_bits;
                if out_bits >= 32 {
                    mmu.write_word_safe(dest, out_word);
                    dest += 4;
                    written += 4;
                    out_word = 0;
                    out_bits = 0;
                }
                node = tree_base + 1; // back to root
            } else {
                node = child;
            }
        }
    }

    fn hle_register_ram_reset<B: CpuBus>(&mut self, mmu: &mut B) {
        let flags = self.registers.gpr[0];
        if (flags & 0x01) != 0 {
            mmu.clear_ewram();
        }
        if (flags & 0x02) != 0 {
            mmu.clear_iwram_safe();
        }
        if (flags & 0x04) != 0 {
            mmu.clear_palette_ram();
        }
        if (flags & 0x08) != 0 {
            mmu.clear_vram();
        }
        if (flags & 0x10) != 0 {
            mmu.clear_oam();
        }
        if (flags & 0x20) != 0 {
            mmu.reset_sio_registers();
        }
        if (flags & 0x40) != 0 {
            mmu.reset_sound_registers();
        }
        if (flags & 0x80) != 0 {
            mmu.reset_other_io_registers();
        }
    }

    fn hle_intr_wait<B: CpuBus>(&mut self, mmu: &mut B) {
        let check_once = self.registers.gpr[0] != 0;
        let wait_flags = self.registers.gpr[1];

        let reg_ie = mmu.read_halfword_safe(0x04000200) as u32;
        let reg_if = mmu.read_halfword_safe(0x04000202) as u32;
        let active_wait = wait_flags & reg_ie;

        if (reg_if & active_wait) != 0 {
            return;
        }
        if check_once {
            return;
        }

        self.halted = true;
        self.intr_wait_flags = active_wait;
    }

    fn hle_vblank_intr_wait<B: CpuBus>(&mut self, mmu: &mut B) {
        self.registers.gpr[0] = 0;
        self.registers.gpr[1] = 1; // Wait specifically for VBlank (bit 0)
        self.hle_intr_wait(mmu);
    }

    fn hle_div(&mut self) {
        let numerator = self.registers.gpr[0] as i32;
        let denominator = self.registers.gpr[1] as i32;

        if denominator == 0 {
            // Prevent division by zero lockup: return safe default
            self.registers.gpr[0] = numerator as u32;
            self.registers.gpr[1] = numerator as u32;
            self.registers.gpr[3] = numerator.unsigned_abs();
        } else {
            // Wrapping, not plain `/` and `%`. The one input pair that is not a
            // division by zero yet still overflows is i32::MIN / -1, whose true
            // quotient (2^31) does not fit in i32: Rust panics on it in every
            // profile, and a panic here aborts the whole process across the cxx
            // FFI boundary. The wrapping result is i32::MIN with remainder 0,
            // which is what the ARM hardware divide produces for the same
            // operands, so this is the hardware answer rather than a guess.
            let quotient = numerator.wrapping_div(denominator);
            let remainder = numerator.wrapping_rem(denominator);
            self.registers.gpr[0] = quotient as u32;
            self.registers.gpr[1] = remainder as u32;
            self.registers.gpr[3] = quotient.unsigned_abs();
        }
    }

    fn hle_sqrt(&mut self) {
        let val = self.registers.gpr[0] as f64;
        let root = val.sqrt() as u32;
        self.registers.gpr[0] = root & 0xFFFF;
    }

    fn hle_arctan2(&mut self) {
        let x = (self.registers.gpr[0] as i16) as f64 / 32768.0;
        let y = (self.registers.gpr[1] as i16) as f64 / 32768.0;
        let angle_rad = y.atan2(x);
        let angle_fixed = (angle_rad / std::f64::consts::PI * 32768.0) as i32;
        self.registers.gpr[0] = (angle_fixed.clamp(-32768, 32767) as i16) as u16 as u32;
    }

    fn hle_cpu_set<B: CpuBus>(&mut self, mmu: &mut B) {
        let src = self.registers.gpr[0];
        let dest = self.registers.gpr[1];
        let control = self.registers.gpr[2];

        let count = control & 0x1F_FFFF;
        // GBATEK CpuSet control word: bit 24 = Fill (0=copy, 1=fill by src[0]),
        // bit 26 = datasize (0=16-bit, 1=32-bit). These were previously swapped,
        // turning a 32-bit copy into a 16-bit fill (e.g. m4a's SoundMainRAM copy
        // into IWRAM got splattered with one word, then crashed on BX into it).
        let is_32bit = (control & 0x0400_0000) != 0;
        let is_fill = (control & 0x0100_0000) != 0;

        // The 21-bit field is the hardware bound and the mask above already
        // applies it: 0x1FFFFF units is at most 8 MB, which every accessor here
        // handles by wrapping within its region, so the loop is bounded without
        // a second cap. The previous `min(count, 0x40000)` silently truncated any
        // larger request — a partial copy with no error, which is the same
        // silent-wrong-result class as the unimplemented SWIs — and its comment
        // said "256KB" for what is 1 MB in 32-bit mode.

        if is_32bit {
            let src_aligned = src & !3;
            let dest_aligned = dest & !3;
            if is_fill {
                let val = mmu.read_word_safe(src_aligned);
                for i in 0..count {
                    mmu.write_word_safe(dest_aligned + i * 4, val);
                }
            } else {
                for i in 0..count {
                    let val = mmu.read_word_safe(src_aligned + i * 4);
                    mmu.write_word_safe(dest_aligned + i * 4, val);
                }
            }
        } else {
            let src_aligned = src & !1;
            let dest_aligned = dest & !1;
            if is_fill {
                let val = mmu.read_halfword_safe(src_aligned);
                for i in 0..count {
                    mmu.write_halfword_safe(dest_aligned + i * 2, val);
                }
            } else {
                for i in 0..count {
                    let val = mmu.read_halfword_safe(src_aligned + i * 2);
                    mmu.write_halfword_safe(dest_aligned + i * 2, val);
                }
            }
        }
    }

    fn hle_cpu_fast_set<B: CpuBus>(&mut self, mmu: &mut B) {
        let src = self.registers.gpr[0];
        let dest = self.registers.gpr[1];
        let control = self.registers.gpr[2];

        // CpuFastSet is always 32-bit; bit 24 = Fill (0=copy, 1=fill). Was bit 26.
        let is_fill = (control & 0x0100_0000) != 0;

        // GBATEK: CpuFastSet moves 8 words per iteration of an unrolled
        // LDMIA/STMIA block, so a count that is not a multiple of 8 is rounded
        // UP — hardware writes `(count + 7) & !7` words. Emulating the exact
        // count under-writes the 1..7 word tail, and fill callers rely on the
        // round-up to clear a tail they deliberately did not count. Rounding
        // before the mask would let 0x1FFFF9..0x1FFFFF overflow the 21-bit
        // field, so it is applied after.
        let count = ((control & 0x1F_FFFF) + 7) & !7;
        let src_aligned = src & !3;
        let dest_aligned = dest & !3;

        if is_fill {
            let val = mmu.read_word_safe(src_aligned);
            for i in 0..count {
                mmu.write_word_safe(dest_aligned + i * 4, val);
            }
        } else {
            for i in 0..count {
                let val = mmu.read_word_safe(src_aligned + i * 4);
                mmu.write_word_safe(dest_aligned + i * 4, val);
            }
        }
    }

    /// SWI 0x07 DivArm: identical to Div but with operands swapped
    /// (r0 = denominator, r1 = numerator). Reuse hle_div to stay DRY.
    fn hle_div_arm(&mut self) {
        self.registers.gpr.swap(0, 1);
        self.hle_div();
    }

    /// SWI 0x09 ArcTan: r0 = Tan in signed Q1.14; returns r0 = angle in the
    /// range C000h..4000h (-PI/2..PI/2), where 4000h == PI/2.
    fn hle_arctan(&mut self) {
        let tan = (self.registers.gpr[0] as i16) as f64 / 16384.0;
        let out = (tan.atan() * (32768.0 / std::f64::consts::PI)).round() as i32;
        self.registers.gpr[0] = (out as i16) as u16 as u32;
    }

    /// SWI 0x0D GetBiosChecksum: retail GBA BIOS checksum. BIOS-detection /
    /// anti-tamper code compares r0 against this constant.
    fn hle_get_bios_checksum(&mut self) {
        self.registers.gpr[0] = 0xBAAE_187F;
    }

    /// GBA BIOS affine sin/cos. The BIOS indexes a 256-entry Q1.14 sine table by
    /// the upper 8 bits of the 16-bit angle. Returns (cos, sin) in Q1.14 so the
    /// caller's `(scale_q8_8 * v) >> 14` yields Q8.8 matrix terms, matching hardware.
    // ponytail: libm instead of the hard-coded 256-entry BIOS table — within
    // ±1 LSB, visually identical. Upgrade to the exact table only if a game needs
    // bit-exact register read-back (none known).
    fn bios_affine_sin_cos(angle: u16) -> (i32, i32) {
        let theta = (angle >> 8) as f64 * (std::f64::consts::PI / 128.0); // 2*PI / 256 per step
        let cos = (theta.cos() * 16384.0).round() as i32;
        let sin = (theta.sin() * 16384.0).round() as i32;
        (cos, sin)
    }

    /// SWI 0x0E BgAffineSet: build BG2/BG3 rotation/scaling matrices (PA-PD) and
    /// start coordinates from a source list. r0 = source ptr, r1 = dest ptr,
    /// r2 = count. Source entry = 20 bytes (cx:s32, cy:s32, dispx:s16, dispy:s16,
    /// scalex:s16, scaley:s16, angle:u16, pad:2); dest entry = 16 bytes.
    fn hle_bg_affine_set<B: CpuBus>(&mut self, mmu: &mut B) {
        let mut src = self.registers.gpr[0];
        let mut dest = self.registers.gpr[1];
        let count = self.registers.gpr[2].min(0x1000); // bound runaway lists

        for _ in 0..count {
            let cx = mmu.read_word_safe(src) as i32; // Q19.8 centre
            let cy = mmu.read_word_safe(src + 4) as i32;
            let disp_x = mmu.read_halfword_safe(src + 8) as i16 as i32;
            let disp_y = mmu.read_halfword_safe(src + 10) as i16 as i32;
            let sx = mmu.read_halfword_safe(src + 12) as i16 as i32; // Q8.8 scale
            let sy = mmu.read_halfword_safe(src + 14) as i16 as i32;
            let angle = mmu.read_halfword_safe(src + 16);
            src += 20;

            let (cos, sin) = Self::bios_affine_sin_cos(angle);
            let pa = (sx * cos) >> 14; // Q8.8
            let pb = (sx * sin) >> 14;
            let pc = (sy * sin) >> 14;
            let pd = (sy * cos) >> 14;
            mmu.write_halfword_safe(dest, pa as u16);
            mmu.write_halfword_safe(dest + 2, (-pb) as u16);
            mmu.write_halfword_safe(dest + 4, pc as u16);
            mmu.write_halfword_safe(dest + 6, pd as u16);

            // start = centre - matrix * displacement. Compute in i64 to stay
            // panic-free on garbage inputs, then truncate to 32-bit as the bus does.
            let start_x = cx as i64 - pa as i64 * disp_x as i64 + pb as i64 * disp_y as i64;
            let start_y = cy as i64 - pc as i64 * disp_x as i64 - pd as i64 * disp_y as i64;
            mmu.write_word_safe(dest + 8, start_x as u32);
            mmu.write_word_safe(dest + 12, start_y as u32);
            dest += 16;
        }
    }

    /// SWI 0x0F ObjAffineSet: build OBJ affine matrices from scale/angle entries.
    /// r0 = source ptr, r1 = dest ptr, r2 = count, r3 = dest stride (2 = packed
    /// matrix, 8 = interleave into OAM attribute slots). Source entry = 8 bytes
    /// (scalex:s16, scaley:s16, angle:u16, pad:2).
    fn hle_obj_affine_set<B: CpuBus>(&mut self, mmu: &mut B) {
        let mut src = self.registers.gpr[0];
        let mut dest = self.registers.gpr[1];
        let count = self.registers.gpr[2].min(0x1000);
        let stride = self.registers.gpr[3];

        for _ in 0..count {
            let sx = mmu.read_halfword_safe(src) as i16 as i32;
            let sy = mmu.read_halfword_safe(src + 2) as i16 as i32;
            let angle = mmu.read_halfword_safe(src + 4);
            src += 8;

            let (cos, sin) = Self::bios_affine_sin_cos(angle);
            mmu.write_halfword_safe(dest, ((sx * cos) >> 14) as u16);
            mmu.write_halfword_safe(dest + stride, (-((sx * sin) >> 14)) as u16);
            mmu.write_halfword_safe(dest + stride * 2, ((sy * sin) >> 14) as u16);
            mmu.write_halfword_safe(dest + stride * 3, ((sy * cos) >> 14) as u16);
            dest += stride * 4;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ARMv5TE extension space must decode as itself, not as MRS/MSR.
    ///
    /// Every encoding here previously fell into `arm_psr_transfer`, because the
    /// PSR gate tested only the data-processing opcode and the S bit. The
    /// damage was not merely "unimplemented": QADD returned CPSR in Rd, SMLABB
    /// overwrote its own accumulator register with CPSR, and SMULBB was run as
    /// `MSR SPSR` and rewrote a byte of the saved status register. The last
    /// assertion pins that ordinary MRS still works, so the new bits-7-4 test
    /// cannot have gone the other way.
    #[test]
    fn armv5te_extension_space_is_not_decoded_as_psr_transfer() {
        let mut cpu = GbaCpu::new();
        let mut mmu = GbaMmu::new(vec![]);
        cpu.armv5 = true;

        // QADD r0, r1, r2 = 0xE1020051 (Rn=2 bits19-16, Rd=0 bits15-12, Rm=1).
        // Saturating, so 0x7FFFFFFF + 1 clamps instead of wrapping to i32::MIN.
        cpu.registers.gpr[1] = 0x7FFF_FFFF;
        cpu.registers.gpr[2] = 1;
        cpu.execute_arm(0xE102_0051, &mut mmu);
        assert_eq!(cpu.registers.gpr[0], 0x7FFF_FFFF, "QADD must saturate");
        assert!(cpu.registers.get_flag(FLAG_Q), "saturation sets Q");

        // QSUB r0, r1, r2 with no saturation: -5 - 3 = -8.
        cpu.registers.gpr[1] = (-5i32) as u32;
        cpu.registers.gpr[2] = 3;
        cpu.execute_arm(0xE122_0051, &mut mmu);
        assert_eq!(cpu.registers.gpr[0] as i32, -8, "QSUB");

        // SMULBB r0, r1, r2 = 0xE1600281: Rd=0, Rs=2, Rm=1, bottom x bottom.
        cpu.registers.gpr[1] = 0xFFFF_0003; // bottom half = 3
        cpu.registers.gpr[2] = 0x1111_0007; // bottom half = 7
        let spsr_before = cpu.registers.spsr;
        cpu.execute_arm(0xE160_0281, &mut mmu);
        assert_eq!(cpu.registers.gpr[0], 21, "SMULBB = 3 * 7");
        assert_eq!(cpu.registers.spsr, spsr_before, "SMULBB must not touch SPSR");

        // SMULTB r0, r1, r2 = 0xE16002A1 (x set: top half of Rm).
        cpu.registers.gpr[1] = 0x0002_0003; // top half = 2
        cpu.registers.gpr[2] = 0x0000_0007; // bottom half = 7
        cpu.execute_arm(0xE160_02A1, &mut mmu);
        assert_eq!(cpu.registers.gpr[0], 14, "SMULTB = 2 * 7");

        // Signed halves: -2 * 3 = -6.
        cpu.registers.gpr[1] = 0x0000_FFFE; // bottom half = -2
        cpu.registers.gpr[2] = 0x0000_0003;
        cpu.execute_arm(0xE160_0281, &mut mmu);
        assert_eq!(cpu.registers.gpr[0] as i32, -6, "halves are signed");

        // SMLABB r0, r1, r2, r3 = 0xE1003281: Rd=0, Rn=3, Rs=2, Rm=1.
        cpu.registers.gpr[1] = 0x0000_0004;
        cpu.registers.gpr[2] = 0x0000_0005;
        cpu.registers.gpr[3] = 100;
        cpu.execute_arm(0xE100_3281, &mut mmu);
        assert_eq!(cpu.registers.gpr[0], 120, "SMLABB = 4 * 5 + 100");
        assert_eq!(cpu.registers.gpr[3], 100, "the accumulator must survive");

        // Data processing must NOT be captured. `MOV r0, r1, ASR #17` is
        // 0xE1A008C1: bits 27-24 are 0001 like the extension space, but bit 23
        // is set (opcode 0xD), and its shift immediate puts bit 7 high with bit
        // 4 low -- the signed-multiply signature. A guard that tests only bits
        // 27-24 executes it as SMLAxy and the ARM9 stops rendering.
        cpu.registers.gpr[1] = 0x4000_0000;
        cpu.execute_arm(0xE1A0_08C1, &mut mmu);
        assert_eq!(cpu.registers.gpr[0], 0x0000_2000, "MOV r0,r1,ASR #17 must stay data processing");

        // Likewise BIC/ORR/MVN with a large shift immediate.
        cpu.registers.gpr[1] = 0xFFFF_FFFF;
        cpu.registers.gpr[2] = 0x0001_0000;
        cpu.execute_arm(0xE1C1_0A02, &mut mmu); // BIC r0, r1, r2, LSL #20
        assert_eq!(cpu.registers.gpr[0], 0xFFFF_FFFF, "BIC r0,r1,r2,LSL #20 stays data processing");

        // MRS r0, CPSR = 0xE10F0000 — bits 7-4 clear, so still a PSR transfer.
        cpu.registers.gpr[0] = 0xDEAD_BEEF;
        cpu.execute_arm(0xE10F_0000, &mut mmu);
        assert_eq!(cpu.registers.gpr[0], cpu.registers.cpsr, "MRS still decodes");
    }

    /// CpuFastSet (SWI 0x0C) moves 8 words per unrolled LDMIA/STMIA block, so
    /// GBATEK rounds a non-multiple-of-8 count UP. Emulating the exact count
    /// leaves the 1..7 word tail holding pre-call bytes, and fill callers rely
    /// on the round-up to clear a tail they deliberately did not count.
    /// CpuSet (0x0B) has no block behaviour and must NOT round.
    #[test]
    fn cpu_fast_set_rounds_the_count_up_to_a_multiple_of_eight() {
        let mut cpu = GbaCpu::new();
        let mut mmu = GbaMmu::new(vec![]);
        let (src, dst) = (0x0200_0000u32, 0x0200_1000u32);
        mmu.write_word_safe(src, 0xA5A5_A5A5);
        for i in 0..8u32 {
            mmu.write_word_safe(dst + i * 4, 0xDEAD_BEEF);
        }
        cpu.registers.gpr[0] = src;
        cpu.registers.gpr[1] = dst;
        cpu.registers.gpr[2] = 5 | 0x0100_0000; // fill, 5 words -> hardware writes 8
        cpu.hle_cpu_fast_set(&mut mmu);
        for i in 0..8u32 {
            assert_eq!(
                mmu.read_word_safe(dst + i * 4),
                0xA5A5_A5A5,
                "word {i} must be filled: the count rounds 5 up to 8"
            );
        }

        // The same count through CpuSet writes exactly 5 words; word 5 is
        // untouched. This is what makes the round-up specific to CpuFastSet.
        for i in 0..8u32 {
            mmu.write_word_safe(dst + i * 4, 0xDEAD_BEEF);
        }
        cpu.registers.gpr[2] = 5 | 0x0100_0000 | 0x0400_0000; // 32-bit fill, 5 words
        cpu.hle_cpu_set(&mut mmu);
        assert_eq!(mmu.read_word_safe(dst + 4 * 4), 0xA5A5_A5A5, "CpuSet writes 5");
        assert_eq!(mmu.read_word_safe(dst + 5 * 4), 0xDEAD_BEEF, "CpuSet must not round");
    }

    // CpuSet (SWI 0x0B) control-word decode. GBATEK: bit24 = Fill (0=copy,
    // 1=fill), bit26 = datasize (0=16-bit, 1=32-bit). These bits were once
    // swapped, turning m4a's 32-bit copy of its IWRAM sound driver into a
    // 16-bit fill of one word — the game then crashed branching into it.
    #[test]
    fn cpu_set_copies_distinct_words_not_fills() {
        let mut cpu = GbaCpu::new();
        let mut mmu = GbaMmu::new(vec![]);
        let (src, dst) = (0x0200_0000u32, 0x0200_1000u32);
        for i in 0..4u32 {
            mmu.write_word_safe(src + i * 4, 0x1000_0000 + i);
        }
        cpu.registers.gpr[0] = src;
        cpu.registers.gpr[1] = dst;
        cpu.registers.gpr[2] = 4 | 0x0400_0000; // 32-bit copy (bit26 set, bit24 clear)
        cpu.hle_cpu_set(&mut mmu);
        for i in 0..4u32 {
            // A fill (the old bug) would leave 0x1000_0000 in every word.
            assert_eq!(mmu.read_word_safe(dst + i * 4), 0x1000_0000 + i);
        }
    }

    #[test]
    fn cpu_set_fill_replicates_source_word() {
        let mut cpu = GbaCpu::new();
        let mut mmu = GbaMmu::new(vec![]);
        let (src, dst) = (0x0200_0000u32, 0x0200_1000u32);
        mmu.write_word_safe(src, 0xABCD_1234);
        cpu.registers.gpr[0] = src;
        cpu.registers.gpr[1] = dst;
        cpu.registers.gpr[2] = 4 | 0x0400_0000 | 0x0100_0000; // 32-bit fill (bit24 set)
        cpu.hle_cpu_set(&mut mmu);
        for i in 0..4u32 {
            assert_eq!(mmu.read_word_safe(dst + i * 4), 0xABCD_1234);
        }
    }

    // LZ77UnCompVram (SWI 0x12) must land byte-exact in VRAM. Writing output
    // byte-by-byte used to trip the 16-bit-bus duplication quirk (01 02 03 04 ->
    // 02 02 04 04), garbling every decompressed BG tile. Exercises literal + a
    // back-reference (distance 4, length 4) that repeats the first four bytes.
    #[test]
    fn lz77_uncomp_to_vram_is_byte_exact() {
        let mut cpu = GbaCpu::new();
        let mut mmu = GbaMmu::new(vec![]);
        let src = 0x0200_0000u32; // EWRAM source (no bus quirk)
        let dst = 0x0600_0000u32; // BG VRAM destination
        let stream: [u8; 11] = [
            0x10, 0x08, 0x00, 0x00, // header: type 0x10 (LZ77), size = 8
            0x08, // flags: tokens 0-3 literal, token 4 back-reference
            0x01, 0x02, 0x03, 0x04, // four literal bytes
            0x10, 0x03, // back-ref: length 4, distance 4 -> repeats 01 02 03 04
        ];
        for (i, b) in stream.iter().enumerate() {
            mmu.write_byte_safe(src + i as u32, *b);
        }
        cpu.registers.gpr[0] = src;
        cpu.registers.gpr[1] = dst;
        cpu.hle_lz77_uncomp(&mut mmu);
        let expected = [0x01u8, 0x02, 0x03, 0x04, 0x01, 0x02, 0x03, 0x04];
        for (i, e) in expected.iter().enumerate() {
            assert_eq!(mmu.read_vram_byte(dst + i as u32), *e, "vram byte {i}");
        }
    }

    // RLUnCompVram (SWI 0x15) byte-exact in VRAM: a run block (3x 0x07) followed
    // by a literal block (01 02 03). Same duplication-quirk regression guard.
    #[test]
    fn rl_uncomp_to_vram_is_byte_exact() {
        let mut cpu = GbaCpu::new();
        let mut mmu = GbaMmu::new(vec![]);
        let src = 0x0200_0000u32;
        let dst = 0x0600_0000u32;
        let stream: [u8; 10] = [
            0x30, 0x06, 0x00, 0x00, // header: type 0x30 (run-length), size = 6
            0x80, 0x07, // run: length 3 of byte 0x07
            0x02, 0x01, 0x02, 0x03, // literal: 3 bytes 01 02 03
        ];
        for (i, b) in stream.iter().enumerate() {
            mmu.write_byte_safe(src + i as u32, *b);
        }
        cpu.registers.gpr[0] = src;
        cpu.registers.gpr[1] = dst;
        cpu.hle_rl_uncomp(&mut mmu);
        let expected = [0x07u8, 0x07, 0x07, 0x01, 0x02, 0x03];
        for (i, e) in expected.iter().enumerate() {
            assert_eq!(mmu.read_vram_byte(dst + i as u32), *e, "vram byte {i}");
        }
    }

    // BgAffineSet (SWI 0x0E) identity: angle 0, scale 1.0 (0x100), no displacement,
    // centre 0 must yield PA=PD=0x100, PB=PC=0, start=(0,0). Pins the Q8.8/Q1.14
    // fixed-point math and the source/dest struct strides.
    #[test]
    fn bg_affine_set_identity() {
        let mut cpu = GbaCpu::new();
        let mut mmu = GbaMmu::new(vec![]);
        let (src, dst) = (0x0200_0000u32, 0x0200_0100u32);
        // cx, cy = 0 (Q19.8)
        mmu.write_word_safe(src, 0);
        mmu.write_word_safe(src + 4, 0);
        // dispx, dispy = 0
        mmu.write_halfword_safe(src + 8, 0);
        mmu.write_halfword_safe(src + 10, 0);
        // scalex, scaley = 1.0 in Q8.8
        mmu.write_halfword_safe(src + 12, 0x0100);
        mmu.write_halfword_safe(src + 14, 0x0100);
        // angle = 0
        mmu.write_halfword_safe(src + 16, 0);

        cpu.registers.gpr[0] = src;
        cpu.registers.gpr[1] = dst;
        cpu.registers.gpr[2] = 1;
        cpu.hle_bg_affine_set(&mut mmu);

        assert_eq!(mmu.read_halfword_safe(dst) as i16, 0x0100); // PA
        assert_eq!(mmu.read_halfword_safe(dst + 2) as i16, 0); // PB
        assert_eq!(mmu.read_halfword_safe(dst + 4) as i16, 0); // PC
        assert_eq!(mmu.read_halfword_safe(dst + 6) as i16, 0x0100); // PD
        assert_eq!(mmu.read_word_safe(dst + 8), 0); // start_x
        assert_eq!(mmu.read_word_safe(dst + 12), 0); // start_y
    }

    // DivArm (SWI 0x07) is Div with operands swapped: r0=denominator, r1=numerator.
    // 7 / 2 -> quotient 3, remainder 1.
    #[test]
    fn div_arm_swaps_operands() {
        let mut cpu = GbaCpu::new();
        cpu.registers.gpr[0] = 2; // denominator
        cpu.registers.gpr[1] = 7; // numerator
        cpu.hle_div_arm();
        assert_eq!(cpu.registers.gpr[0], 3); // quotient
        assert_eq!(cpu.registers.gpr[1], 1); // remainder
    }

    // Barrel shifter: the carry-out and amount-zero special cases are the part
    // most likely to break a conditional, so pin them down.
    #[test]
    fn barrel_shift_edge_cases() {
        // LSL #0: value unchanged, carry passes through.
        assert_eq!(GbaCpu::barrel_shift(0x1234, 0, 0, true), (0x1234, true));
        assert_eq!(GbaCpu::barrel_shift(0x1234, 0, 0, false), (0x1234, false));
        // LSL #1: carry = old bit 31.
        assert_eq!(GbaCpu::barrel_shift(0x8000_0001, 0, 1, false), (0x0000_0002, true));
        // LSR #0 encodes #32: result 0, carry = bit 31.
        assert_eq!(GbaCpu::barrel_shift(0x8000_0000, 1, 0, false), (0, true));
        // ASR #0 encodes #32: fills with the sign bit.
        assert_eq!(GbaCpu::barrel_shift(0x8000_0000, 2, 0, false), (0xFFFF_FFFF, true));
        assert_eq!(GbaCpu::barrel_shift(0x7000_0000, 2, 0, false), (0, false));
        // ROR #0 encodes RRX: rotate right through carry.
        assert_eq!(GbaCpu::barrel_shift(0x0000_0001, 3, 0, true), (0x8000_0000, true));
        assert_eq!(GbaCpu::barrel_shift(0x0000_0002, 3, 0, false), (0x0000_0001, false));
        // ROR by 4.
        assert_eq!(GbaCpu::barrel_shift(0x0000_000F, 3, 4, false), (0xF000_0000, true));
    }

    // adc drives every add/sub flag; check the C/V boundaries that the old stub
    // never set (and which silently broke conditionals).
    #[test]
    fn adc_flags_boundaries() {
        let mut cpu = GbaCpu::new();

        // 1 - 1 == 0: zero set, no borrow so carry set, no overflow.
        let r = cpu.adc(1, !1, 1, true); // SUB 1,1
        assert_eq!(r, 0);
        assert!(cpu.registers.get_flag(FLAG_Z));
        assert!(cpu.registers.get_flag(FLAG_C)); // carry = !borrow
        assert!(!cpu.registers.get_flag(FLAG_V));

        // 0 - 1 borrows: carry clear, negative set.
        let r = cpu.adc(0, !1, 1, true); // SUB 0,1
        assert_eq!(r, 0xFFFF_FFFF);
        assert!(!cpu.registers.get_flag(FLAG_C));
        assert!(cpu.registers.get_flag(FLAG_N));

        // Signed overflow: 0x7FFFFFFF + 1 -> negative.
        let r = cpu.adc(0x7FFF_FFFF, 1, 0, true); // ADD
        assert_eq!(r, 0x8000_0000);
        assert!(cpu.registers.get_flag(FLAG_V));
        assert!(!cpu.registers.get_flag(FLAG_C));

        // Unsigned carry-out on add.
        let r = cpu.adc(0xFFFF_FFFF, 1, 0, true); // ADD
        assert_eq!(r, 0);
        assert!(cpu.registers.get_flag(FLAG_C));
        assert!(cpu.registers.get_flag(FLAG_Z));
    }
}
