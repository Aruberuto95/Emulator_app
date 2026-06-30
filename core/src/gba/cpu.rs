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

        // Check if interrupts are pending and IME is enabled
        let ime = mmu.read_word_safe(0x04000208);
        if (ime & 1) != 0 && !self.registers.get_flag(FLAG_I) {
            let ie = mmu.read_halfword_safe(0x04000200);
            let r_if = mmu.read_halfword_safe(0x04000202);
            if (ie & r_if) != 0 {
                // Trigger IRQ exception
                self.trigger_irq(mmu);
                return 4;
            }
        }

        if self.pc_modified {
            self.flush_pipeline(mmu);
        }

        let inst = self.pipeline[0];
        self.pipeline[0] = self.pipeline[1];

        let current_pc = self.registers.gpr[15];
        let is_thumb = self.registers.get_flag(FLAG_T);

        let cycles = if is_thumb {
            self.pipeline[1] = mmu.read_halfword(current_pc & !1) as u32;
            self.registers.gpr[15] = current_pc.wrapping_add(2);
            self.execute_thumb(inst as u16, mmu)
        } else {
            self.pipeline[1] = mmu.read_word(current_pc & !3);
            self.registers.gpr[15] = current_pc.wrapping_add(4);
            self.execute_arm(inst, mmu)
        };

        cycles
    }

    fn trigger_irq(&mut self, _mmu: &mut GbaMmu) {
        let old_cpsr = self.registers.cpsr;
        let old_mode = self.registers.get_mode();
        self.registers.swap_mode(old_mode, CpuMode::Irq);
        self.registers.spsr = old_cpsr;

        // IRQ Exception saves returning PC + 4 (ARM) or PC + 4 (THUMB) depending on state
        // When executing, R15 (PC) is current + 8 or 4.
        let is_thumb = (old_cpsr & FLAG_T) != 0;
        let return_link = if is_thumb {
            self.registers.gpr[15].wrapping_sub(2) // return to PC + 4
        } else {
            self.registers.gpr[15].wrapping_sub(4) // return to PC + 4
        };
        self.registers.gpr[14] = return_link;

        self.registers.set_flag(FLAG_T, false); // Switch to ARM state
        self.registers.set_flag(FLAG_I, true); // Disable IRQ

        self.registers.gpr[15] = 0x00000018; // IRQ Exception Vector
        self.pc_modified = true;
    }

    // --- ARM Interpreter ---
    fn execute_arm(&mut self, inst: u32, mmu: &mut GbaMmu) -> u32 {
        let cond = inst >> 28;
        if !self.check_condition(cond) {
            return 1; // skipped instruction takes 1 cycle
        }

        // SWI Check (bits 24-27 are 0xF)
        if (inst & 0x0F000000) == 0x0F000000 {
            let swi_comment = ((inst >> 16) & 0xFF) as u8;
            self.handle_swi(swi_comment, mmu);
            return 3;
        }

        // Branch and Exchange (BX)
        if (inst & 0x0FFFFFF0) == 0x012FFF10 {
            let rn = (inst & 0x0F) as usize;
            let target = self.registers.gpr[rn];
            self.registers.set_flag(FLAG_T, (target & 1) != 0);
            self.registers.gpr[15] = target;
            self.pc_modified = true;
            return 3;
        }

        // Branch or Branch with Link (B/BL)
        if (inst & 0x0E000000) == 0x0A000000 {
            let is_link = (inst & 0x01000000) != 0;
            let offset = inst & 0x00FFFFFF;
            // Sign extend 24-bit offset to 32-bit, shift by 2
            let mut signed_offset = offset as i32;
            if (signed_offset & 0x00800000) != 0 {
                signed_offset |= !0x00FFFFFF;
            }
            signed_offset = signed_offset.wrapping_shl(2);

            if is_link {
                // Store return address (next instruction) in LR
                self.registers.gpr[14] = self.registers.gpr[15].wrapping_sub(4);
            }
            self.registers.gpr[15] =
                (self.registers.gpr[15] as i32).wrapping_add(signed_offset) as u32;
            self.pc_modified = true;
            return 3;
        }

        // Load/Store Single Data Transfer (LDR/STR)
        if (inst & 0x0C000000) == 0x04000000 {
            let is_load = (inst & 0x00100000) != 0;
            let is_byte = (inst & 0x00400000) != 0;
            let rn = ((inst >> 16) & 0x0F) as usize;
            let rd = ((inst >> 12) & 0x0F) as usize;

            let base = if rn == 15 {
                // R15 reads as PC + 8
                self.registers.gpr[15]
            } else {
                self.registers.gpr[rn]
            };

            let offset = inst & 0x0FFF; // simple immediate offset for simplicity
            let is_up = (inst & 0x00800000) != 0;

            let final_addr = if is_up {
                base.wrapping_add(offset)
            } else {
                base.wrapping_sub(offset)
            };

            if is_load {
                let val = if is_byte {
                    mmu.read_byte_safe(final_addr) as u32
                } else {
                    mmu.read_word_safe(final_addr)
                };
                self.registers.gpr[rd] = val;
                if rd == 15 {
                    self.pc_modified = true;
                }
            } else {
                let val = self.registers.gpr[rd];
                if is_byte {
                    mmu.write_byte_safe(final_addr, (val & 0xFF) as u8);
                } else {
                    mmu.write_word_safe(final_addr, val);
                }
            }
            return 2;
        }

        // Data Processing (ADD, MOV, etc.)
        if (inst & 0x0C000000) == 0x00000000 {
            let opcode = (inst >> 21) & 0x0F;
            let rn = ((inst >> 16) & 0x0F) as usize;
            let rd = ((inst >> 12) & 0x0F) as usize;
            let is_imm = (inst & 0x02000000) != 0;
            let s_bit = (inst & 0x00100000) != 0;

            let op2 = if is_imm {
                let imm8 = inst & 0xFF;
                let rot = ((inst >> 8) & 0x0F) * 2;
                imm8.rotate_right(rot)
            } else {
                let rm = (inst & 0x0F) as usize;
                self.registers.gpr[rm]
            };

            let op1 = if rn == 15 {
                self.registers.gpr[15]
            } else {
                self.registers.gpr[rn]
            };

            let mut result = 0u32;
            let mut update_rd = true;

            match opcode {
                0x0 => result = op1 & op2,             // AND
                0x1 => result = op1 ^ op2,             // EOR
                0x2 => result = op1.wrapping_sub(op2), // SUB
                0x4 => result = op1.wrapping_add(op2), // ADD
                0x8 => {
                    update_rd = false;
                    result = op1 & op2;
                } // TST
                0x9 => {
                    update_rd = false;
                    result = op1 ^ op2;
                } // TEQ
                0xA => {
                    update_rd = false;
                    result = op1.wrapping_sub(op2);
                } // CMP
                0xD => result = op2,                   // MOV
                0xE => result = op1 & !op2,            // BIC
                _ => {}
            }

            if update_rd {
                self.registers.gpr[rd] = result;
                if rd == 15 {
                    self.pc_modified = true;
                }
            }

            if s_bit {
                self.registers.set_flag(FLAG_N, (result & 0x80000000) != 0);
                self.registers.set_flag(FLAG_Z, result == 0);
            }
            return 1;
        }

        1 // fallback
    }

    // --- THUMB Interpreter ---
    fn execute_thumb(&mut self, inst: u16, mmu: &mut GbaMmu) -> u32 {
        // SWI Check (Format 15: SWI)
        if (inst & 0xFF00) == 0xDF00 {
            let swi_comment = (inst & 0xFF) as u8;
            self.handle_swi(swi_comment, mmu);
            return 3;
        }

        // Format 5: Hi register operations / branch exchange
        if (inst & 0xFC00) == 0x4400 {
            let op = (inst >> 8) & 3;
            let h1 = (inst >> 7) & 1;
            let h2 = (inst >> 6) & 1;
            let rs = ((inst >> 3) & 7) as usize + if h2 == 1 { 8 } else { 0 };
            let _rd = (inst & 7) as usize + if h1 == 1 { 8 } else { 0 };

            if op == 3 {
                // BX
                let target = self.registers.gpr[rs];
                self.registers.set_flag(FLAG_T, (target & 1) != 0);
                self.registers.gpr[15] = target;
                self.pc_modified = true;
                return 3;
            }
        }

        // Format 17: Unconditional branch
        if (inst & 0xF800) == 0xE000 {
            let mut offset = (inst & 0x07FF) as i32;
            if (offset & 0x0400) != 0 {
                offset |= !0x07FF;
            }
            offset = offset.wrapping_shl(1);
            self.registers.gpr[15] = (self.registers.gpr[15] as i32).wrapping_add(offset) as u32;
            self.pc_modified = true;
            return 3;
        }

        // Format 16: Conditional branch
        if (inst & 0xF000) == 0xD000 {
            let cond = ((inst >> 8) & 0x0F) as u32;
            let mut offset = (inst & 0xFF) as i8 as i32;
            offset = offset.wrapping_shl(1);
            if self.check_condition(cond) {
                self.registers.gpr[15] =
                    (self.registers.gpr[15] as i32).wrapping_add(offset) as u32;
                self.pc_modified = true;
                return 3;
            }
            return 1;
        }

        // Format 18: Long branch with link (BL)
        if (inst & 0xF000) == 0xF000 {
            let offset11 = (inst & 0x07FF) as u32;
            let is_low = (inst & 0x0800) != 0;
            if !is_low {
                // First instruction: LR = PC + (Offset << 12)
                let mut signed_offset = offset11 as i32;
                if (signed_offset & 0x0400) != 0 {
                    signed_offset |= !0x07FF;
                }
                signed_offset = signed_offset.wrapping_shl(12);
                self.registers.gpr[14] = self.registers.gpr[15].wrapping_add(signed_offset as u32);
            } else {
                // Second instruction: NextPC = LR + (Offset << 1), LR = PC | 1
                let next_pc = self.registers.gpr[14].wrapping_add(offset11 << 1);
                self.registers.gpr[14] = (self.registers.gpr[15].wrapping_sub(2)) | 1;
                self.registers.gpr[15] = next_pc;
                self.pc_modified = true;
            }
            return 1;
        }

        1 // fallback
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
            0x01 => self.hle_register_ram_reset(mmu),
            0x04 => self.hle_intr_wait(mmu),
            0x05 => self.hle_vblank_intr_wait(mmu),
            0x06 => self.hle_div(),
            0x08 => self.hle_sqrt(),
            0x0A => self.hle_arctan2(),
            0x0B => self.hle_cpu_set(mmu),
            0x0C => self.hle_cpu_fast_set(mmu),
            _ => {
                eprintln!("Unhandled BIOS SWI call: 0x{:02X}", comment);
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
        let is_32bit = (control & 0x0100_0000) != 0;
        let is_fill = (control & 0x0400_0000) != 0;

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
        let is_fill = (control & 0x0400_0000) != 0;

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
}
