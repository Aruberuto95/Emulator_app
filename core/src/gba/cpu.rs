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

pub struct GbaCpu {
    pub registers: CpuRegisters,
    pub pipeline: [u32; 2],
    pub pc_modified: bool,
    pub halted: bool,
    pub exception_depth: u32,
    pub intr_wait_flags: u32,
}

const FLAG_N: u32 = 1 << 31;
const FLAG_Z: u32 = 1 << 30;
const FLAG_C: u32 = 1 << 29;
const FLAG_V: u32 = 1 << 28;
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

    pub fn flush_pipeline(&mut self, mmu: &mut GbaMmu) {
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
    fn execute_arm(&mut self, inst: u32, mmu: &mut GbaMmu) -> u32 {
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

        // PSR transfer (MRS/MSR): data-proc opcodes 1000..1011 with S clear are
        // really PSR transfers, not TST/TEQ/CMP/CMN. Must precede data processing.
        if is_dp_class && !s && (0x8..=0xB).contains(&opcode) {
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

    fn arm_single_transfer(&mut self, inst: u32, mmu: &mut GbaMmu) -> u32 {
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
                self.write_pc(val & !1, false);
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

    fn arm_halfword_transfer(&mut self, inst: u32, mmu: &mut GbaMmu) -> u32 {
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
            // Only STRH is a valid store in this space.
            mmu.write_halfword(addr, self.registers.gpr[rd] as u16);
            if writeback || !pre {
                self.registers.gpr[rn] = offset_addr;
            }
        }
        3
    }

    fn arm_block_transfer(&mut self, inst: u32, mmu: &mut GbaMmu) -> u32 {
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
                        self.write_pc(val & !1, false); // ARM7TDMI: no interworking on LDM
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

    fn arm_swap(&mut self, inst: u32, mmu: &mut GbaMmu) -> u32 {
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
    fn execute_thumb(&mut self, inst: u16, mmu: &mut GbaMmu) -> u32 {
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
        let mut do_shift = |cpu: &mut Self, shift_type: u32| {
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
            _ => { self.write_pc(b, true); } // BX (interworking)
        }
        1
    }

    fn thumb_pc_load(&mut self, inst: u16, mmu: &mut GbaMmu) -> u32 {
        let rd = ((inst >> 8) & 7) as usize;
        let off = ((inst & 0xFF) as u32) << 2;
        let addr = (self.registers.gpr[15] & !2).wrapping_add(off);
        self.registers.gpr[rd] = mmu.read_word(addr);
        3
    }

    fn thumb_ldst_reg(&mut self, inst: u16, mmu: &mut GbaMmu) -> u32 {
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

    fn thumb_ldst_sign(&mut self, inst: u16, mmu: &mut GbaMmu) -> u32 {
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

    fn thumb_ldst_imm(&mut self, inst: u16, mmu: &mut GbaMmu) -> u32 {
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

    fn thumb_ldst_half(&mut self, inst: u16, mmu: &mut GbaMmu) -> u32 {
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

    fn thumb_sp_ldst(&mut self, inst: u16, mmu: &mut GbaMmu) -> u32 {
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

    fn thumb_push_pop(&mut self, inst: u16, mmu: &mut GbaMmu) -> u32 {
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
                self.write_pc(val & !1, false); // ARM7TDMI: stay in THUMB
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

    fn thumb_block(&mut self, inst: u16, mmu: &mut GbaMmu) -> u32 {
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
    pub fn handle_swi(&mut self, comment: u8, mmu: &mut GbaMmu) {
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

    /// SoftReset (SWI 0x00): clear the user/system stacks, jump to the entry the
    /// boot flag selects, and return to System mode in ARM state.
    fn hle_soft_reset(&mut self, mmu: &mut GbaMmu) {
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
    fn flush_uncomp(mmu: &mut GbaMmu, dest: u32, data: &[u8]) {
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
    fn hle_lz77_uncomp(&mut self, mmu: &mut GbaMmu) {
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
    fn hle_rl_uncomp(&mut self, mmu: &mut GbaMmu) {
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
    fn hle_huff_uncomp(&mut self, mmu: &mut GbaMmu) {
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

    fn hle_register_ram_reset(&mut self, mmu: &mut GbaMmu) {
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

    fn hle_intr_wait(&mut self, mmu: &mut GbaMmu) {
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

    fn hle_vblank_intr_wait(&mut self, mmu: &mut GbaMmu) {
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
            let quotient = numerator / denominator;
            let remainder = numerator % denominator;
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

    fn hle_cpu_set(&mut self, mmu: &mut GbaMmu) {
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

        // Prevent massive out of bounds writes / DOS loops
        let count = std::cmp::min(count, 0x40000); // Limit to 256KB

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

    fn hle_cpu_fast_set(&mut self, mmu: &mut GbaMmu) {
        let src = self.registers.gpr[0];
        let dest = self.registers.gpr[1];
        let control = self.registers.gpr[2];

        let count = control & 0x1F_FFFF;
        // CpuFastSet is always 32-bit; bit 24 = Fill (0=copy, 1=fill). Was bit 26.
        let is_fill = (control & 0x0100_0000) != 0;

        let count = std::cmp::min(count, 0x40000);
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
    fn hle_bg_affine_set(&mut self, mmu: &mut GbaMmu) {
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
    fn hle_obj_affine_set(&mut self, mmu: &mut GbaMmu) {
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
