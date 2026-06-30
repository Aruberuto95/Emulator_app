use crate::gbc::mmu::Mmu;

/// Sharp LR35902 CPU Flags masks.
pub const FLAG_Z: u8 = 0x80; // Zero flag
pub const FLAG_N: u8 = 0x40; // Subtract flag
pub const FLAG_H: u8 = 0x20; // Half-carry flag
pub const FLAG_C: u8 = 0x10; // Carry flag

/// Sharp LR35902 CPU Register File.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CpuRegisters {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub sp: u16,
    pub pc: u16,
}

impl CpuRegisters {
    pub fn get_af(&self) -> u16 {
        ((self.a as u16) << 8) | (self.f as u16)
    }

    pub fn set_af(&mut self, val: u16) {
        self.a = (val >> 8) as u8;
        self.f = (val & 0xF0) as u8; // Lower 4 bits are always 0
    }

    pub fn get_bc(&self) -> u16 {
        ((self.b as u16) << 8) | (self.c as u16)
    }

    pub fn set_bc(&mut self, val: u16) {
        self.b = (val >> 8) as u8;
        self.c = val as u8;
    }

    pub fn get_de(&self) -> u16 {
        ((self.d as u16) << 8) | (self.e as u16)
    }

    pub fn set_de(&mut self, val: u16) {
        self.d = (val >> 8) as u8;
        self.e = val as u8;
    }

    pub fn get_hl(&self) -> u16 {
        ((self.h as u16) << 8) | (self.l as u16)
    }

    pub fn set_hl(&mut self, val: u16) {
        self.h = (val >> 8) as u8;
        self.l = val as u8;
    }

    pub fn get_flag(&self, mask: u8) -> bool {
        (self.f & mask) != 0
    }

    pub fn set_flag(&mut self, mask: u8, val: bool) {
        if val {
            self.f |= mask;
        } else {
            self.f &= !mask;
        }
    }
}

/// Sharp LR35902 CPU Emulator Core.
pub struct Cpu {
    pub registers: CpuRegisters,
    pub ime: bool,
    pub halted: bool,
    pub double_speed: bool,
    pub ei_delay: bool,
    pub stop_mode: bool,
    pub stop_cycles_left: u32,
    pub div_counter: u16,
}

impl Cpu {
    /// Creates a new CPU instance initialized to post-boot GBC state.
    pub fn new() -> Self {
        let mut registers = CpuRegisters::default();
        // Standard GBC post-boot state values
        registers.a = 0x11;
        registers.f = 0x80;
        registers.b = 0x00;
        registers.c = 0x00;
        registers.d = 0xFF;
        registers.e = 0x56;
        registers.h = 0x00;
        registers.l = 0x0D;
        registers.sp = 0xFFFE;
        registers.pc = 0x0100; // ROM Entry Point

        Self {
            registers,
            ime: false,
            halted: false,
            double_speed: false,
            ei_delay: false,
            stop_mode: false,
            stop_cycles_left: 0,
            div_counter: 0x1800, // Typical initial DIV counter
        }
    }

    /// Resets CPU registers and control flags to GBC startup state.
    pub fn reset(&mut self) {
        self.registers.a = 0x11;
        self.registers.f = 0x80;
        self.registers.b = 0x00;
        self.registers.c = 0x00;
        self.registers.d = 0xFF;
        self.registers.e = 0x56;
        self.registers.h = 0x00;
        self.registers.l = 0x0D;
        self.registers.sp = 0xFFFE;
        self.registers.pc = 0x0100;
        self.ime = false;
        self.halted = false;
        self.double_speed = false;
        self.ei_delay = false;
        self.stop_mode = false;
        self.stop_cycles_left = 0;
        self.div_counter = 0x1800;
    }

    /// Pushes a 16-bit word onto the stack.
    pub fn push_word(&mut self, val: u16, mmu: &mut Mmu) {
        self.registers.sp = self.registers.sp.wrapping_sub(1);
        mmu.write_byte(self.registers.sp, (val >> 8) as u8);
        self.registers.sp = self.registers.sp.wrapping_sub(1);
        mmu.write_byte(self.registers.sp, (val & 0xFF) as u8);
    }

    /// Pops a 16-bit word from the stack.
    pub fn pop_word(&mut self, mmu: &Mmu) -> u16 {
        let low = mmu.read_byte(self.registers.sp) as u16;
        self.registers.sp = self.registers.sp.wrapping_add(1);
        let high = mmu.read_byte(self.registers.sp) as u16;
        self.registers.sp = self.registers.sp.wrapping_add(1);
        (high << 8) | low
    }

    /// Helper for timer clock signal.
    fn get_timer_signal(&self, tac: u8, double_speed: bool) -> bool {
        if (tac & 0x04) == 0 {
            return false;
        }
        let bit_index = match tac & 0x03 {
            0 => {
                if double_speed {
                    10
                } else {
                    9
                }
            }
            1 => {
                if double_speed {
                    4
                } else {
                    3
                }
            }
            2 => {
                if double_speed {
                    6
                } else {
                    5
                }
            }
            3 => {
                if double_speed {
                    8
                } else {
                    7
                }
            }
            _ => unreachable!(),
        };
        ((self.div_counter >> bit_index) & 1) != 0
    }

    /// Updates internal CPU timers and Divider registers.
    pub fn update_timers(&mut self, cycles: u32, mmu: &mut Mmu) {
        let double_speed = self.double_speed;
        let tac = mmu.read_io(0x07);

        for _ in 0..cycles {
            let prev_signal = self.get_timer_signal(tac, double_speed);
            self.div_counter = self.div_counter.wrapping_add(1);
            let current_signal = self.get_timer_signal(tac, double_speed);

            // Periodically synchronize CPU div_counter with MMU DIV register
            // DIV register maps to upper 8 bits of internal counter
            let div_reg_val = if double_speed {
                (self.div_counter >> 9) as u8
            } else {
                (self.div_counter >> 8) as u8
            };
            mmu.write_io(0x04, div_reg_val);

            // Falling edge transition detection for TIMA increment
            if prev_signal && !current_signal {
                let mut tima = mmu.read_io(0x05);
                if tima == 0xFF {
                    tima = mmu.read_io(0x06); // Reload TMA
                    let mut iff = mmu.read_io(0x0F);
                    iff |= 0x04; // Request Timer Interrupt
                    mmu.write_io(0x0F, iff);
                } else {
                    tima = tima.wrapping_add(1);
                }
                mmu.write_io(0x05, tima);
            }
        }
    }

    /// Checks for and services pending interrupts.
    pub fn check_interrupts(&mut self, mmu: &mut Mmu) -> Option<u32> {
        let pending = mmu.ie & mmu.read_io(0x0F);
        if pending != 0 {
            self.halted = false;
            if self.ime {
                for bit in 0..5 {
                    if (pending & (1 << bit)) != 0 {
                        self.ime = false;
                        let mut iff = mmu.read_io(0x0F);
                        iff &= !(1 << bit);
                        mmu.write_io(0x0F, iff);

                        let pc = self.registers.pc;
                        self.push_word(pc, mmu);

                        let vector = match bit {
                            0 => 0x0040, // VBlank
                            1 => 0x0048, // LCD STAT
                            2 => 0x0050, // Timer
                            3 => 0x0058, // Serial
                            4 => 0x0060, // Joypad
                            _ => unreachable!(),
                        };
                        self.registers.pc = vector;
                        return Some(20); // Servicing consumes 20 cycles
                    }
                }
            }
        }
        None
    }

    /// Single step execution: services interrupts, fetches, decodes, and runs instruction.
    pub fn step(&mut self, mmu: &mut Mmu) -> u32 {
        if self.stop_mode {
            if self.stop_cycles_left > 4 {
                self.stop_cycles_left -= 4;
                self.update_timers(4, mmu);
                return 4;
            } else {
                let left = self.stop_cycles_left;
                self.stop_cycles_left = 0;
                self.stop_mode = false;
                self.update_timers(left, mmu);
                return left;
            }
        }

        // Service interrupts
        if let Some(cycles) = self.check_interrupts(mmu) {
            self.update_timers(cycles, mmu);
            return cycles;
        }

        if self.halted {
            self.update_timers(4, mmu);
            return 4; // HALT mode ticks timers at 4 cycles per instruction slot
        }

        let pc = self.registers.pc;
        let opcode = mmu.read_byte(pc);
        self.registers.pc = self.registers.pc.wrapping_add(1);

        if self.ei_delay {
            self.ime = true;
            self.ei_delay = false;
        }

        let cycles = self.execute_opcode(opcode, mmu);
        self.update_timers(cycles, mmu);
        cycles
    }

    /// Executes a single LR35902 CPU instruction.
    fn execute_opcode(&mut self, opcode: u8, mmu: &mut Mmu) -> u32 {
        match opcode {
            0x00 => 4, // NOP

            // 16-bit Loads
            0x01 => {
                // LD BC, d16
                let val = self.read_immediate_u16(mmu);
                self.registers.set_bc(val);
                12
            }
            0x11 => {
                // LD DE, d16
                let val = self.read_immediate_u16(mmu);
                self.registers.set_de(val);
                12
            }
            0x21 => {
                // LD HL, d16
                let val = self.read_immediate_u16(mmu);
                self.registers.set_hl(val);
                12
            }
            0x31 => {
                // LD SP, d16
                let val = self.read_immediate_u16(mmu);
                self.registers.sp = val;
                12
            }

            // Stack PUSH / POP
            0xC5 => {
                self.push_word(self.registers.get_bc(), mmu);
                16
            }
            0xD5 => {
                self.push_word(self.registers.get_de(), mmu);
                16
            }
            0xE5 => {
                self.push_word(self.registers.get_hl(), mmu);
                16
            }
            0xF5 => {
                self.push_word(self.registers.get_af(), mmu);
                16
            }

            0xC1 => {
                let val = self.pop_word(mmu);
                self.registers.set_bc(val);
                12
            }
            0xD1 => {
                let val = self.pop_word(mmu);
                self.registers.set_de(val);
                12
            }
            0xE1 => {
                let val = self.pop_word(mmu);
                self.registers.set_hl(val);
                12
            }
            0xF1 => {
                let val = self.pop_word(mmu);
                self.registers.set_af(val);
                12
            }

            // 8-bit Loads
            0x06 => {
                self.registers.b = self.read_immediate_u8(mmu);
                8
            }
            0x0E => {
                self.registers.c = self.read_immediate_u8(mmu);
                8
            }
            0x16 => {
                self.registers.d = self.read_immediate_u8(mmu);
                8
            }
            0x1E => {
                self.registers.e = self.read_immediate_u8(mmu);
                8
            }
            0x26 => {
                self.registers.h = self.read_immediate_u8(mmu);
                8
            }
            0x2E => {
                self.registers.l = self.read_immediate_u8(mmu);
                8
            }
            0x36 => {
                let val = self.read_immediate_u8(mmu);
                mmu.write_byte(self.registers.get_hl(), val);
                12
            }

            0x7F => {
                self.registers.a = self.registers.a;
                4
            }
            0x78 => {
                self.registers.a = self.registers.b;
                4
            }
            0x79 => {
                self.registers.a = self.registers.c;
                4
            }
            0x7A => {
                self.registers.a = self.registers.d;
                4
            }
            0x7B => {
                self.registers.a = self.registers.e;
                4
            }
            0x7C => {
                self.registers.a = self.registers.h;
                4
            }
            0x7D => {
                self.registers.a = self.registers.l;
                4
            }
            0x7E => {
                self.registers.a = mmu.read_byte(self.registers.get_hl());
                8
            }

            0x40 => {
                self.registers.b = self.registers.b;
                4
            }
            0x41 => {
                self.registers.b = self.registers.c;
                4
            }
            0x42 => {
                self.registers.b = self.registers.d;
                4
            }
            0x43 => {
                self.registers.b = self.registers.e;
                4
            }
            0x44 => {
                self.registers.b = self.registers.h;
                4
            }
            0x45 => {
                self.registers.b = self.registers.l;
                4
            }
            0x46 => {
                self.registers.b = mmu.read_byte(self.registers.get_hl());
                8
            }

            0x48 => {
                self.registers.c = self.registers.b;
                4
            }
            0x49 => {
                self.registers.c = self.registers.c;
                4
            }
            0x4A => {
                self.registers.c = self.registers.d;
                4
            }
            0x4B => {
                self.registers.c = self.registers.e;
                4
            }
            0x4C => {
                self.registers.c = self.registers.h;
                4
            }
            0x4D => {
                self.registers.c = self.registers.l;
                4
            }
            0x4E => {
                self.registers.c = mmu.read_byte(self.registers.get_hl());
                8
            }

            0x50 => {
                self.registers.d = self.registers.b;
                4
            }
            0x51 => {
                self.registers.d = self.registers.c;
                4
            }
            0x52 => {
                self.registers.d = self.registers.d;
                4
            }
            0x53 => {
                self.registers.d = self.registers.e;
                4
            }
            0x54 => {
                self.registers.d = self.registers.h;
                4
            }
            0x55 => {
                self.registers.d = self.registers.l;
                4
            }
            0x56 => {
                self.registers.d = mmu.read_byte(self.registers.get_hl());
                8
            }

            0x58 => {
                self.registers.e = self.registers.b;
                4
            }
            0x59 => {
                self.registers.e = self.registers.c;
                4
            }
            0x5A => {
                self.registers.e = self.registers.d;
                4
            }
            0x5B => {
                self.registers.e = self.registers.e;
                4
            }
            0x5C => {
                self.registers.e = self.registers.h;
                4
            }
            0x5D => {
                self.registers.e = self.registers.l;
                4
            }
            0x5E => {
                self.registers.e = mmu.read_byte(self.registers.get_hl());
                8
            }

            0x60 => {
                self.registers.h = self.registers.b;
                4
            }
            0x61 => {
                self.registers.h = self.registers.c;
                4
            }
            0x62 => {
                self.registers.h = self.registers.d;
                4
            }
            0x63 => {
                self.registers.h = self.registers.e;
                4
            }
            0x64 => {
                self.registers.h = self.registers.h;
                4
            }
            0x65 => {
                self.registers.h = self.registers.l;
                4
            }
            0x66 => {
                self.registers.h = mmu.read_byte(self.registers.get_hl());
                8
            }

            0x68 => {
                self.registers.l = self.registers.b;
                4
            }
            0x69 => {
                self.registers.l = self.registers.c;
                4
            }
            0x6A => {
                self.registers.l = self.registers.d;
                4
            }
            0x6B => {
                self.registers.l = self.registers.e;
                4
            }
            0x6C => {
                self.registers.l = self.registers.h;
                4
            }
            0x6D => {
                self.registers.l = self.registers.l;
                4
            }
            0x6E => {
                self.registers.l = mmu.read_byte(self.registers.get_hl());
                8
            }

            0x70 => {
                mmu.write_byte(self.registers.get_hl(), self.registers.b);
                8
            }
            0x71 => {
                mmu.write_byte(self.registers.get_hl(), self.registers.c);
                8
            }
            0x72 => {
                mmu.write_byte(self.registers.get_hl(), self.registers.d);
                8
            }
            0x73 => {
                mmu.write_byte(self.registers.get_hl(), self.registers.e);
                8
            }
            0x74 => {
                mmu.write_byte(self.registers.get_hl(), self.registers.h);
                8
            }
            0x75 => {
                mmu.write_byte(self.registers.get_hl(), self.registers.l);
                8
            }

            0x47 => {
                self.registers.b = self.registers.a;
                4
            }
            0x4F => {
                self.registers.c = self.registers.a;
                4
            }
            0x57 => {
                self.registers.d = self.registers.a;
                4
            }
            0x5F => {
                self.registers.e = self.registers.a;
                4
            }
            0x67 => {
                self.registers.h = self.registers.a;
                4
            }
            0x6F => {
                self.registers.l = self.registers.a;
                4
            }

            0x02 => {
                mmu.write_byte(self.registers.get_bc(), self.registers.a);
                8
            }
            0x12 => {
                mmu.write_byte(self.registers.get_de(), self.registers.a);
                8
            }
            0x0A => {
                self.registers.a = mmu.read_byte(self.registers.get_bc());
                8
            }
            0x1A => {
                self.registers.a = mmu.read_byte(self.registers.get_de());
                8
            }

            0xEA => {
                // LD [nn], A
                let addr = self.read_immediate_u16(mmu);
                mmu.write_byte(addr, self.registers.a);
                16
            }
            0xFA => {
                // LD A, [nn]
                let addr = self.read_immediate_u16(mmu);
                self.registers.a = mmu.read_byte(addr);
                16
            }

            // High RAM/IO Loads
            0xE0 => {
                // LDH [n], A
                let offset = self.read_immediate_u8(mmu) as u16;
                mmu.write_byte(0xFF00 + offset, self.registers.a);
                12
            }
            0xF0 => {
                // LDH A, [n]
                let offset = self.read_immediate_u8(mmu) as u16;
                self.registers.a = mmu.read_byte(0xFF00 + offset);
                12
            }
            0xE2 => {
                // LDH [C], A
                mmu.write_byte(0xFF00 + self.registers.c as u16, self.registers.a);
                8
            }
            0xF2 => {
                // LDH A, [C]
                self.registers.a = mmu.read_byte(0xFF00 + self.registers.c as u16);
                8
            }

            // Memory HL plus/minus loads
            0x22 => {
                // LD [HL+], A
                let hl = self.registers.get_hl();
                mmu.write_byte(hl, self.registers.a);
                self.registers.set_hl(hl.wrapping_add(1));
                8
            }
            0x32 => {
                // LD [HL-], A
                let hl = self.registers.get_hl();
                mmu.write_byte(hl, self.registers.a);
                self.registers.set_hl(hl.wrapping_sub(1));
                8
            }
            0x2A => {
                // LD A, [HL+]
                let hl = self.registers.get_hl();
                self.registers.a = mmu.read_byte(hl);
                self.registers.set_hl(hl.wrapping_add(1));
                8
            }
            0x3A => {
                // LD A, [HL-]
                let hl = self.registers.get_hl();
                self.registers.a = mmu.read_byte(hl);
                self.registers.set_hl(hl.wrapping_sub(1));
                8
            }

            // Arithmetic & Logical (8-bit ALU)
            0x87 => {
                self.alu_add(self.registers.a, false);
                4
            }
            0x80 => {
                self.alu_add(self.registers.b, false);
                4
            }
            0x81 => {
                self.alu_add(self.registers.c, false);
                4
            }
            0x82 => {
                self.alu_add(self.registers.d, false);
                4
            }
            0x83 => {
                self.alu_add(self.registers.e, false);
                4
            }
            0x84 => {
                self.alu_add(self.registers.h, false);
                4
            }
            0x85 => {
                self.alu_add(self.registers.l, false);
                4
            }
            0x86 => {
                let val = mmu.read_byte(self.registers.get_hl());
                self.alu_add(val, false);
                8
            }
            0xC6 => {
                let val = self.read_immediate_u8(mmu);
                self.alu_add(val, false);
                8
            }

            0x8F => {
                self.alu_add(self.registers.a, true);
                4
            }
            0x88 => {
                self.alu_add(self.registers.b, true);
                4
            }
            0x89 => {
                self.alu_add(self.registers.c, true);
                4
            }
            0x8A => {
                self.alu_add(self.registers.d, true);
                4
            }
            0x8B => {
                self.alu_add(self.registers.e, true);
                4
            }
            0x8C => {
                self.alu_add(self.registers.h, true);
                4
            }
            0x8D => {
                self.alu_add(self.registers.l, true);
                4
            }
            0x8E => {
                let val = mmu.read_byte(self.registers.get_hl());
                self.alu_add(val, true);
                8
            }
            0xCE => {
                let val = self.read_immediate_u8(mmu);
                self.alu_add(val, true);
                8
            }

            0x97 => {
                self.alu_sub(self.registers.a, false);
                4
            }
            0x90 => {
                self.alu_sub(self.registers.b, false);
                4
            }
            0x91 => {
                self.alu_sub(self.registers.c, false);
                4
            }
            0x92 => {
                self.alu_sub(self.registers.d, false);
                4
            }
            0x93 => {
                self.alu_sub(self.registers.e, false);
                4
            }
            0x94 => {
                self.alu_sub(self.registers.h, false);
                4
            }
            0x95 => {
                self.alu_sub(self.registers.l, false);
                4
            }
            0x96 => {
                let val = mmu.read_byte(self.registers.get_hl());
                self.alu_sub(val, false);
                8
            }
            0xD6 => {
                let val = self.read_immediate_u8(mmu);
                self.alu_sub(val, false);
                8
            }

            0x9F => {
                self.alu_sub(self.registers.a, true);
                4
            }
            0x98 => {
                self.alu_sub(self.registers.b, true);
                4
            }
            0x99 => {
                self.alu_sub(self.registers.c, true);
                4
            }
            0x9A => {
                self.alu_sub(self.registers.d, true);
                4
            }
            0x9B => {
                self.alu_sub(self.registers.e, true);
                4
            }
            0x9C => {
                self.alu_sub(self.registers.h, true);
                4
            }
            0x9D => {
                self.alu_sub(self.registers.l, true);
                4
            }
            0x9E => {
                let val = mmu.read_byte(self.registers.get_hl());
                self.alu_sub(val, true);
                8
            }
            0xDE => {
                let val = self.read_immediate_u8(mmu);
                self.alu_sub(val, true);
                8
            }

            0xA7 => {
                self.alu_and(self.registers.a);
                4
            }
            0xA0 => {
                self.alu_and(self.registers.b);
                4
            }
            0xA1 => {
                self.alu_and(self.registers.c);
                4
            }
            0xA2 => {
                self.alu_and(self.registers.d);
                4
            }
            0xA3 => {
                self.alu_and(self.registers.e);
                4
            }
            0xA4 => {
                self.alu_and(self.registers.h);
                4
            }
            0xA5 => {
                self.alu_and(self.registers.l);
                4
            }
            0xA6 => {
                let val = mmu.read_byte(self.registers.get_hl());
                self.alu_and(val);
                8
            }
            0xE6 => {
                let val = self.read_immediate_u8(mmu);
                self.alu_and(val);
                8
            }

            0xAF => {
                self.alu_xor(self.registers.a);
                4
            }
            0xA8 => {
                self.alu_xor(self.registers.b);
                4
            }
            0xA9 => {
                self.alu_xor(self.registers.c);
                4
            }
            0xAA => {
                self.alu_xor(self.registers.d);
                4
            }
            0xAB => {
                self.alu_xor(self.registers.e);
                4
            }
            0xAC => {
                self.alu_xor(self.registers.h);
                4
            }
            0xAD => {
                self.alu_xor(self.registers.l);
                4
            }
            0xAE => {
                let val = mmu.read_byte(self.registers.get_hl());
                self.alu_xor(val);
                8
            }
            0xEE => {
                let val = self.read_immediate_u8(mmu);
                self.alu_xor(val);
                8
            }

            0xB7 => {
                self.alu_or(self.registers.a);
                4
            }
            0xB0 => {
                self.alu_or(self.registers.b);
                4
            }
            0xB1 => {
                self.alu_or(self.registers.c);
                4
            }
            0xB2 => {
                self.alu_or(self.registers.d);
                4
            }
            0xB3 => {
                self.alu_or(self.registers.e);
                4
            }
            0xB4 => {
                self.alu_or(self.registers.h);
                4
            }
            0xB5 => {
                self.alu_or(self.registers.l);
                4
            }
            0xB6 => {
                let val = mmu.read_byte(self.registers.get_hl());
                self.alu_or(val);
                8
            }
            0xF6 => {
                let val = self.read_immediate_u8(mmu);
                self.alu_or(val);
                8
            }

            0xBF => {
                self.alu_cp(self.registers.a);
                4
            }
            0xB8 => {
                self.alu_cp(self.registers.b);
                4
            }
            0xB9 => {
                self.alu_cp(self.registers.c);
                4
            }
            0xBA => {
                self.alu_cp(self.registers.d);
                4
            }
            0xBB => {
                self.alu_cp(self.registers.e);
                4
            }
            0xBC => {
                self.alu_cp(self.registers.h);
                4
            }
            0xBD => {
                self.alu_cp(self.registers.l);
                4
            }
            0xBE => {
                let val = mmu.read_byte(self.registers.get_hl());
                self.alu_cp(val);
                8
            }
            0xFE => {
                let val = self.read_immediate_u8(mmu);
                self.alu_cp(val);
                8
            }

            // Increment / Decrement
            0x3C => {
                self.registers.a = self.alu_inc(self.registers.a);
                4
            }
            0x04 => {
                self.registers.b = self.alu_inc(self.registers.b);
                4
            }
            0x0C => {
                self.registers.c = self.alu_inc(self.registers.c);
                4
            }
            0x14 => {
                self.registers.d = self.alu_inc(self.registers.d);
                4
            }
            0x1C => {
                self.registers.e = self.alu_inc(self.registers.e);
                4
            }
            0x24 => {
                self.registers.h = self.alu_inc(self.registers.h);
                4
            }
            0x2C => {
                self.registers.l = self.alu_inc(self.registers.l);
                4
            }
            0x34 => {
                let addr = self.registers.get_hl();
                let val = mmu.read_byte(addr);
                mmu.write_byte(addr, self.alu_inc(val));
                12
            }

            0x3D => {
                self.registers.a = self.alu_dec(self.registers.a);
                4
            }
            0x05 => {
                self.registers.b = self.alu_dec(self.registers.b);
                4
            }
            0x0D => {
                self.registers.c = self.alu_dec(self.registers.c);
                4
            }
            0x15 => {
                self.registers.d = self.alu_dec(self.registers.d);
                4
            }
            0x1D => {
                self.registers.e = self.alu_dec(self.registers.e);
                4
            }
            0x25 => {
                self.registers.h = self.alu_dec(self.registers.h);
                4
            }
            0x2D => {
                self.registers.l = self.alu_dec(self.registers.l);
                4
            }
            0x30 => {
                // JR NC, r8
                let offset = self.read_immediate_i8(mmu);
                if !self.registers.get_flag(FLAG_C) {
                    self.registers.pc = self.registers.pc.wrapping_add(offset as u16);
                    12
                } else {
                    8
                }
            }
            0x35 => {
                let addr = self.registers.get_hl();
                let val = mmu.read_byte(addr);
                mmu.write_byte(addr, self.alu_dec(val));
                12
            }

            // 16-bit Arithmetic
            0x09 => {
                self.alu_add_hl(self.registers.get_bc());
                8
            }
            0x19 => {
                self.alu_add_hl(self.registers.get_de());
                8
            }
            0x29 => {
                self.alu_add_hl(self.registers.get_hl());
                8
            }
            0x39 => {
                self.alu_add_hl(self.registers.sp);
                8
            }

            0x03 => {
                let val = self.registers.get_bc().wrapping_add(1);
                self.registers.set_bc(val);
                8
            }
            0x13 => {
                let val = self.registers.get_de().wrapping_add(1);
                self.registers.set_de(val);
                8
            }
            0x23 => {
                let val = self.registers.get_hl().wrapping_add(1);
                self.registers.set_hl(val);
                8
            }
            0x33 => {
                self.registers.sp = self.registers.sp.wrapping_add(1);
                8
            }

            0x0B => {
                let val = self.registers.get_bc().wrapping_sub(1);
                self.registers.set_bc(val);
                8
            }
            0x1B => {
                let val = self.registers.get_de().wrapping_sub(1);
                self.registers.set_de(val);
                8
            }
            0x2B => {
                let val = self.registers.get_hl().wrapping_sub(1);
                self.registers.set_hl(val);
                8
            }
            0x3B => {
                self.registers.sp = self.registers.sp.wrapping_sub(1);
                8
            }

            // Rotates & Shifts (Accumulator)
            0x07 => {
                // RLCA
                let bit7 = (self.registers.a & 0x80) != 0;
                self.registers.a = (self.registers.a << 1) | (bit7 as u8);
                self.registers.f = 0;
                self.registers.set_flag(FLAG_C, bit7);
                4
            }
            0x17 => {
                // RLA
                let carry = self.registers.get_flag(FLAG_C) as u8;
                let bit7 = (self.registers.a & 0x80) != 0;
                self.registers.a = (self.registers.a << 1) | carry;
                self.registers.f = 0;
                self.registers.set_flag(FLAG_C, bit7);
                4
            }
            0x0F => {
                // RRCA
                let bit0 = (self.registers.a & 0x01) != 0;
                self.registers.a = (self.registers.a >> 1) | ((bit0 as u8) << 7);
                self.registers.f = 0;
                self.registers.set_flag(FLAG_C, bit0);
                4
            }
            0x1F => {
                // RRA
                let carry = self.registers.get_flag(FLAG_C) as u8;
                let bit0 = (self.registers.a & 0x01) != 0;
                self.registers.a = (self.registers.a >> 1) | (carry << 7);
                self.registers.f = 0;
                self.registers.set_flag(FLAG_C, bit0);
                4
            }

            // Control flow: Jumps
            0xC3 => {
                // JP nn
                self.registers.pc = self.read_immediate_u16(mmu);
                16
            }
            0xC2 => {
                // JP NZ, nn
                let addr = self.read_immediate_u16(mmu);
                if !self.registers.get_flag(FLAG_Z) {
                    self.registers.pc = addr;
                    16
                } else {
                    12
                }
            }
            0xCA => {
                // JP Z, nn
                let addr = self.read_immediate_u16(mmu);
                if self.registers.get_flag(FLAG_Z) {
                    self.registers.pc = addr;
                    16
                } else {
                    12
                }
            }
            0xD2 => {
                // JP NC, nn
                let addr = self.read_immediate_u16(mmu);
                if !self.registers.get_flag(FLAG_C) {
                    self.registers.pc = addr;
                    16
                } else {
                    12
                }
            }
            0xDA => {
                // JP C, nn
                let addr = self.read_immediate_u16(mmu);
                if self.registers.get_flag(FLAG_C) {
                    self.registers.pc = addr;
                    16
                } else {
                    12
                }
            }
            0xE9 => {
                // JP HL
                self.registers.pc = self.registers.get_hl();
                4
            }

            // Relative jumps
            0x18 => {
                // JR e8
                let offset = self.read_immediate_i8(mmu);
                self.registers.pc = self.registers.pc.wrapping_add(offset as u16);
                12
            }
            0x20 => {
                // JR NZ, e8
                let offset = self.read_immediate_i8(mmu);
                if !self.registers.get_flag(FLAG_Z) {
                    self.registers.pc = self.registers.pc.wrapping_add(offset as u16);
                    12
                } else {
                    8
                }
            }
            0x28 => {
                // JR Z, e8
                let offset = self.read_immediate_i8(mmu);
                if self.registers.get_flag(FLAG_Z) {
                    self.registers.pc = self.registers.pc.wrapping_add(offset as u16);
                    12
                } else {
                    8
                }
            }
            0x38 => {
                // JR C, e8
                let offset = self.read_immediate_i8(mmu);
                if self.registers.get_flag(FLAG_C) {
                    self.registers.pc = self.registers.pc.wrapping_add(offset as u16);
                    12
                } else {
                    8
                }
            }

            // Calls & Returns
            0xCD => {
                // CALL nn
                let addr = self.read_immediate_u16(mmu);
                let next_pc = self.registers.pc;
                self.push_word(next_pc, mmu);
                self.registers.pc = addr;
                24
            }
            0xC4 => {
                // CALL NZ, nn
                let addr = self.read_immediate_u16(mmu);
                if !self.registers.get_flag(FLAG_Z) {
                    let next_pc = self.registers.pc;
                    self.push_word(next_pc, mmu);
                    self.registers.pc = addr;
                    24
                } else {
                    12
                }
            }
            0xCC => {
                // CALL Z, nn
                let addr = self.read_immediate_u16(mmu);
                if self.registers.get_flag(FLAG_Z) {
                    let next_pc = self.registers.pc;
                    self.push_word(next_pc, mmu);
                    self.registers.pc = addr;
                    24
                } else {
                    12
                }
            }
            0xD4 => {
                // CALL NC, nn
                let addr = self.read_immediate_u16(mmu);
                if !self.registers.get_flag(FLAG_C) {
                    let next_pc = self.registers.pc;
                    self.push_word(next_pc, mmu);
                    self.registers.pc = addr;
                    24
                } else {
                    12
                }
            }
            0xDC => {
                // CALL C, nn
                let addr = self.read_immediate_u16(mmu);
                if self.registers.get_flag(FLAG_C) {
                    let next_pc = self.registers.pc;
                    self.push_word(next_pc, mmu);
                    self.registers.pc = addr;
                    24
                } else {
                    12
                }
            }

            0xC9 => {
                // RET
                self.registers.pc = self.pop_word(mmu);
                16
            }
            0xC0 => {
                // RET NZ
                if !self.registers.get_flag(FLAG_Z) {
                    self.registers.pc = self.pop_word(mmu);
                    20
                } else {
                    8
                }
            }
            0xC8 => {
                // RET Z
                if self.registers.get_flag(FLAG_Z) {
                    self.registers.pc = self.pop_word(mmu);
                    20
                } else {
                    8
                }
            }
            0xD0 => {
                // RET NC
                if !self.registers.get_flag(FLAG_C) {
                    self.registers.pc = self.pop_word(mmu);
                    20
                } else {
                    8
                }
            }
            0xD8 => {
                // RET C
                if self.registers.get_flag(FLAG_C) {
                    self.registers.pc = self.pop_word(mmu);
                    20
                } else {
                    8
                }
            }
            0xD9 => {
                // RETI (Return from Interrupt)
                self.registers.pc = self.pop_word(mmu);
                self.ime = true;
                16
            }

            // Restarts (RST)
            0xC7 => {
                self.push_word(self.registers.pc, mmu);
                self.registers.pc = 0x00;
                16
            }
            0xCF => {
                self.push_word(self.registers.pc, mmu);
                self.registers.pc = 0x08;
                16
            }
            0xD7 => {
                self.push_word(self.registers.pc, mmu);
                self.registers.pc = 0x10;
                16
            }
            0xDF => {
                self.push_word(self.registers.pc, mmu);
                self.registers.pc = 0x18;
                16
            }
            0xE7 => {
                self.push_word(self.registers.pc, mmu);
                self.registers.pc = 0x20;
                16
            }
            0xEF => {
                self.push_word(self.registers.pc, mmu);
                self.registers.pc = 0x28;
                16
            }
            0xF7 => {
                self.push_word(self.registers.pc, mmu);
                self.registers.pc = 0x30;
                16
            }
            0xFF => {
                self.push_word(self.registers.pc, mmu);
                self.registers.pc = 0x38;
                16
            }

            // Prefix CB Opcodes
            0xCB => {
                let cb_opcode = self.read_immediate_u8(mmu);
                self.execute_cb_opcode(cb_opcode, mmu)
            }

            // CPU Control
            0xF3 => {
                // DI
                self.ime = false;
                4
            }
            0xFB => {
                // EI
                self.ei_delay = true;
                4
            }
            0x76 => {
                // HALT
                self.halted = true;
                4
            }
            0x10 => {
                // STOP / speed switch
                let _dummy = self.read_immediate_u8(mmu); // STOP instruction is 2 bytes
                let key1 = mmu.read_io(0x4D);
                if (key1 & 0x01) != 0 {
                    // Double speed switch requested!
                    let new_speed = (key1 & 0x80) == 0;
                    let updated_key1 = if new_speed { 0x80 } else { 0x00 };
                    mmu.write_io(0x4D, updated_key1);
                    self.double_speed = new_speed;
                    // Disable CPU for 128,000 cycles
                    self.stop_mode = true;
                    self.stop_cycles_left = 128000;
                }
                4
            }
            0x27 => {
                // DAA (Decimal Adjust Accumulator)
                let mut a = self.registers.a as u16;
                if !self.registers.get_flag(FLAG_N) {
                    if self.registers.get_flag(FLAG_H) || (a & 0x0F) > 9 {
                        a += 0x06;
                    }
                    if self.registers.get_flag(FLAG_C) || a > 0x9F {
                        a += 0x60;
                        self.registers.set_flag(FLAG_C, true);
                    }
                } else {
                    if self.registers.get_flag(FLAG_H) {
                        a = a.wrapping_sub(6) & 0xFF;
                    }
                    if self.registers.get_flag(FLAG_C) {
                        a = a.wrapping_sub(0x60);
                    }
                }
                self.registers.a = a as u8;
                self.registers.set_flag(FLAG_Z, self.registers.a == 0);
                self.registers.set_flag(FLAG_H, false);
                4
            }
            0x2F => {
                // CPL (Complement A)
                self.registers.a = !self.registers.a;
                self.registers.set_flag(FLAG_N, true);
                self.registers.set_flag(FLAG_H, true);
                4
            }
            0x37 => {
                // SCF (Set Carry Flag)
                self.registers.set_flag(FLAG_N, false);
                self.registers.set_flag(FLAG_H, false);
                self.registers.set_flag(FLAG_C, true);
                4
            }
            0x3F => {
                // CCF (Complement Carry Flag)
                let c = self.registers.get_flag(FLAG_C);
                self.registers.set_flag(FLAG_N, false);
                self.registers.set_flag(FLAG_H, false);
                self.registers.set_flag(FLAG_C, !c);
                4
            }

            // Miscellaneous SP operations
            0xF8 => {
                // LD HL, SP+r8
                let sp = self.registers.sp as i32;
                let offset = self.read_immediate_i8(mmu) as i32;
                let res = sp.wrapping_add(offset);
                self.registers.set_hl(res as u16);
                self.registers.set_flag(FLAG_Z, false);
                self.registers.set_flag(FLAG_N, false);

                // Half-carry and carry from SP low byte
                let sp_u8 = (self.registers.sp & 0xFF) as i32;
                let offset_u8 = (offset & 0xFF) as i32;
                self.registers
                    .set_flag(FLAG_H, (sp_u8 & 0x0F) + (offset_u8 & 0x0F) > 0x0F);
                self.registers
                    .set_flag(FLAG_C, (sp_u8 & 0xFF) + (offset_u8 & 0xFF) > 0xFF);
                12
            }
            0xF9 => {
                // LD SP, HL
                self.registers.sp = self.registers.get_hl();
                8
            }
            0xE8 => {
                // ADD SP, r8
                let sp = self.registers.sp as i32;
                let offset = self.read_immediate_i8(mmu) as i32;
                let res = sp.wrapping_add(offset);
                self.registers.sp = res as u16;
                self.registers.set_flag(FLAG_Z, false);
                self.registers.set_flag(FLAG_N, false);
                let sp_u8 = (self.registers.sp & 0xFF) as i32;
                let offset_u8 = (offset & 0xFF) as i32;
                self.registers
                    .set_flag(FLAG_H, (sp_u8 & 0x0F) + (offset_u8 & 0x0F) > 0x0F);
                self.registers
                    .set_flag(FLAG_C, (sp_u8 & 0xFF) + (offset_u8 & 0xFF) > 0xFF);
                16
            }
            0x08 => {
                // LD [nn], SP
                let addr = self.read_immediate_u16(mmu);
                mmu.write_byte(addr, (self.registers.sp & 0xFF) as u8);
                mmu.write_byte(addr + 1, (self.registers.sp >> 8) as u8);
                20
            }

            _ => 4, // Default fallback cycles for undefined/unhandled opcodes
        }
    }

    /// Decodes and executes CB-prefixed instructions.
    fn execute_cb_opcode(&mut self, cb_opcode: u8, mmu: &mut Mmu) -> u32 {
        let mode = cb_opcode >> 6; // 0: Shift/rotate, 1: BIT, 2: RES, 3: SET
        let bit = (cb_opcode >> 3) & 0x07; // Target bit index
        let reg_idx = cb_opcode & 0x07; // Target register index

        let mut val = match reg_idx {
            0 => self.registers.b,
            1 => self.registers.c,
            2 => self.registers.d,
            3 => self.registers.e,
            4 => self.registers.h,
            5 => self.registers.l,
            6 => mmu.read_byte(self.registers.get_hl()),
            7 => self.registers.a,
            _ => unreachable!(),
        };

        let mut cycles = if reg_idx == 6 { 16 } else { 8 };

        match mode {
            0 => {
                // Shift/Rotate
                let op = bit;
                let c = self.registers.get_flag(FLAG_C) as u8;
                let mut carry = false;
                match op {
                    0 => {
                        // RLC
                        carry = (val & 0x80) != 0;
                        val = (val << 1) | (carry as u8);
                    }
                    1 => {
                        // RRC
                        carry = (val & 0x01) != 0;
                        val = (val >> 1) | ((carry as u8) << 7);
                    }
                    2 => {
                        // RL
                        carry = (val & 0x80) != 0;
                        val = (val << 1) | c;
                    }
                    3 => {
                        // RR
                        carry = (val & 0x01) != 0;
                        val = (val >> 1) | (c << 7);
                    }
                    4 => {
                        // SLA
                        carry = (val & 0x80) != 0;
                        val <<= 1;
                    }
                    5 => {
                        // SRA
                        carry = (val & 0x01) != 0;
                        val = ((val as i8) >> 1) as u8;
                    }
                    6 => {
                        // SWAP
                        val = (val >> 4) | (val << 4);
                        self.registers.f = 0;
                    }
                    7 => {
                        // SRL
                        carry = (val & 0x01) != 0;
                        val >>= 1;
                    }
                    _ => {}
                }
                if op != 6 {
                    self.registers.f = 0;
                    self.registers.set_flag(FLAG_C, carry);
                    self.registers.set_flag(FLAG_Z, val == 0);
                } else {
                    self.registers.set_flag(FLAG_Z, val == 0);
                }
            }
            1 => {
                // BIT
                let bit_val = (val & (1 << bit)) != 0;
                self.registers.set_flag(FLAG_Z, !bit_val);
                self.registers.set_flag(FLAG_N, false);
                self.registers.set_flag(FLAG_H, true);
                if reg_idx == 6 {
                    cycles = 12; // BIT [HL] takes 12 cycles
                }
                return cycles;
            }
            2 => {
                // RES
                val &= !(1 << bit);
            }
            3 => {
                // SET
                val |= 1 << bit;
            }
            _ => {}
        }

        match reg_idx {
            0 => self.registers.b = val,
            1 => self.registers.c = val,
            2 => self.registers.d = val,
            3 => self.registers.e = val,
            4 => self.registers.h = val,
            5 => self.registers.l = val,
            6 => mmu.write_byte(self.registers.get_hl(), val),
            7 => self.registers.a = val,
            _ => {}
        }

        cycles
    }

    // ALU helper functions
    fn alu_add(&mut self, val: u8, use_carry: bool) {
        let carry = if use_carry {
            self.registers.get_flag(FLAG_C) as u16
        } else {
            0
        };
        let a = self.registers.a as u16;
        let v = val as u16;
        let res = a + v + carry;

        self.registers.f = 0;
        self.registers.set_flag(FLAG_Z, (res & 0xFF) == 0);
        self.registers
            .set_flag(FLAG_H, (a & 0x0F) + (v & 0x0F) + carry > 0x0F);
        self.registers.set_flag(FLAG_C, res > 0xFF);
        self.registers.a = res as u8;
    }

    fn alu_sub(&mut self, val: u8, use_carry: bool) {
        let carry = if use_carry {
            self.registers.get_flag(FLAG_C) as u16
        } else {
            0
        };
        let a = self.registers.a as u16;
        let v = val as u16;
        let res = a.wrapping_sub(v).wrapping_sub(carry);

        self.registers.f = FLAG_N;
        self.registers.set_flag(FLAG_Z, (res & 0xFF) == 0);
        self.registers
            .set_flag(FLAG_H, (a & 0x0F) < (v & 0x0F) + carry);
        self.registers.set_flag(FLAG_C, a < v + carry);
        self.registers.a = res as u8;
    }

    fn alu_and(&mut self, val: u8) {
        self.registers.a &= val;
        self.registers.f = FLAG_H;
        self.registers.set_flag(FLAG_Z, self.registers.a == 0);
    }

    fn alu_xor(&mut self, val: u8) {
        self.registers.a ^= val;
        self.registers.f = 0;
        self.registers.set_flag(FLAG_Z, self.registers.a == 0);
    }

    fn alu_or(&mut self, val: u8) {
        self.registers.a |= val;
        self.registers.f = 0;
        self.registers.set_flag(FLAG_Z, self.registers.a == 0);
    }

    fn alu_cp(&mut self, val: u8) {
        let a = self.registers.a;
        self.registers.f = FLAG_N;
        self.registers.set_flag(FLAG_Z, a == val);
        self.registers.set_flag(FLAG_H, (a & 0x0F) < (val & 0x0F));
        self.registers.set_flag(FLAG_C, a < val);
    }

    fn alu_inc(&mut self, val: u8) -> u8 {
        let res = val.wrapping_add(1);
        self.registers.set_flag(FLAG_N, false);
        self.registers.set_flag(FLAG_Z, res == 0);
        self.registers.set_flag(FLAG_H, (val & 0x0F) == 0x0F);
        res
    }

    fn alu_dec(&mut self, val: u8) -> u8 {
        let res = val.wrapping_sub(1);
        self.registers.set_flag(FLAG_N, true);
        self.registers.set_flag(FLAG_Z, res == 0);
        self.registers.set_flag(FLAG_H, (val & 0x0F) == 0x00);
        res
    }

    fn alu_add_hl(&mut self, val: u16) {
        let hl = self.registers.get_hl() as u32;
        let v = val as u32;
        let res = hl + v;

        self.registers.set_flag(FLAG_N, false);
        self.registers
            .set_flag(FLAG_H, (hl & 0x0FFF) + (v & 0x0FFF) > 0x0FFF);
        self.registers.set_flag(FLAG_C, res > 0xFFFF);
        self.registers.set_hl(res as u16);
    }

    // Read helpers
    fn read_immediate_u8(&mut self, mmu: &Mmu) -> u8 {
        let pc = self.registers.pc;
        self.registers.pc = self.registers.pc.wrapping_add(1);
        mmu.read_byte(pc)
    }

    fn read_immediate_i8(&mut self, mmu: &Mmu) -> i8 {
        self.read_immediate_u8(mmu) as i8
    }

    fn read_immediate_u16(&mut self, mmu: &Mmu) -> u16 {
        let low = self.read_immediate_u8(mmu) as u16;
        let high = self.read_immediate_u8(mmu) as u16;
        (high << 8) | low
    }
}
