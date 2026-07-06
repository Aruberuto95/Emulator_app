pub struct Rsp {
    pub gpr: [u32; 32],
    pub pc: u16,
    pub vpr: [[i16; 8]; 32],
    pub acc: [i64; 8],
    pub vco: u16,
    pub vcc: u16,
    pub vce: u8,
    pub status: u32,
    pub semaphore: std::cell::Cell<u32>,
    pub halted: bool,
    pub broke: bool,
    pub single_step: bool,
    pub intr_on_break: bool,
    pub signals: [bool; 8],
    pub dma_busy: bool,
    pub dma_full: bool,
    pub in_delay_slot: bool,
    pub delay_slot_pc: u16,

    // RSP control registers
    pub sp_mem_addr: u32,
    pub sp_dram_addr: u32,
    pub sp_rd_len: u32,
    pub sp_wr_len: u32,
}

impl Rsp {
    pub fn new() -> Self {
        Self {
            gpr: [0; 32],
            pc: 0,
            vpr: [[0; 8]; 32],
            acc: [0; 8],
            vco: 0,
            vcc: 0,
            vce: 0,
            status: 1, // Start halted
            semaphore: std::cell::Cell::new(0),
            halted: true,
            broke: false,
            single_step: false,
            intr_on_break: false,
            signals: [false; 8],
            dma_busy: false,
            dma_full: false,
            in_delay_slot: false,
            delay_slot_pc: 0,
            sp_mem_addr: 0,
            sp_dram_addr: 0,
            sp_rd_len: 0,
            sp_wr_len: 0,
        }
    }

    pub fn reset(&mut self) {
        self.gpr.fill(0);
        self.pc = 0;
        self.vpr.fill([0; 8]);
        self.acc.fill(0);
        self.vco = 0;
        self.vcc = 0;
        self.vce = 0;
        self.status = 1;
        self.semaphore.set(0);
        self.halted = true;
        self.broke = false;
        self.single_step = false;
        self.intr_on_break = false;
        self.signals.fill(false);
        self.dma_busy = false;
        self.dma_full = false;
        self.in_delay_slot = false;
        self.delay_slot_pc = 0;
        self.sp_mem_addr = 0;
        self.sp_dram_addr = 0;
        self.sp_rd_len = 0;
        self.sp_wr_len = 0;
    }

    pub fn get_status(&self) -> u32 {
        let mut status = 0u32;
        if self.halted { status |= 1 << 0; }
        if self.broke { status |= 1 << 1; }
        if self.dma_busy { status |= 1 << 2; }
        if self.dma_full { status |= 1 << 3; }
        if self.single_step { status |= 1 << 4; }
        if self.intr_on_break { status |= 1 << 5; }
        for i in 0..8 {
            if self.signals[i] {
                status |= 1 << (6 + i);
            }
        }
        status
    }

    pub fn step(&mut self, sp_dmem: &mut [u8; 0x1000], sp_imem: &mut [u8; 0x1000]) {
        if self.halted {
            return;
        }

        let current_pc = self.pc;
        let pc_idx = (current_pc & 0xFFC) as usize;
        let instr = u32::from_be_bytes([
            sp_imem[pc_idx],
            sp_imem[pc_idx + 1],
            sp_imem[pc_idx + 2],
            sp_imem[pc_idx + 3],
        ]);

        let was_in_delay_slot = self.in_delay_slot;
        self.in_delay_slot = false; // Reset unless set by branch/jump

        // Advance PC by 4 (this points to the delay slot or next instruction)
        self.pc = self.pc.wrapping_add(4) & 0xFFF;

        let op = instr >> 26;
        let rs = ((instr >> 21) & 0x1F) as usize;
        let rt = ((instr >> 16) & 0x1F) as usize;
        let rd = ((instr >> 11) & 0x1F) as usize;
        let sa = (instr >> 6) & 0x1F;
        let func = instr & 0x3F;
        let imm = (instr & 0xFFFF) as u16;
        let target = instr & 0x03FF_FFFF;

        let sign_extended_imm = imm as i16 as i32 as u32;
        let zero_extended_imm = imm as u32;

        match op {
            0x00 => { // SPECIAL
                match func {
                    0x00 => { // SLL
                        let res = self.gpr[rt] << sa;
                        self.gpr[rd] = res;
                    }
                    0x02 => { // SRL
                        let res = self.gpr[rt] >> sa;
                        self.gpr[rd] = res;
                    }
                    0x03 => { // SRA
                        let res = (self.gpr[rt] as i32) >> sa;
                        self.gpr[rd] = res as u32;
                    }
                    0x04 => { // SLLV
                        let shift = self.gpr[rs] & 0x1F;
                        let res = self.gpr[rt] << shift;
                        self.gpr[rd] = res;
                    }
                    0x06 => { // SRLV
                        let shift = self.gpr[rs] & 0x1F;
                        let res = self.gpr[rt] >> shift;
                        self.gpr[rd] = res;
                    }
                    0x07 => { // SRAV
                        let shift = self.gpr[rs] & 0x1F;
                        let res = (self.gpr[rt] as i32) >> shift;
                        self.gpr[rd] = res as u32;
                    }
                    0x08 => { // JR
                        let target_pc = (self.gpr[rs] & 0xFFF) as u16;
                        self.in_delay_slot = true;
                        self.delay_slot_pc = target_pc;
                    }
                    0x09 => { // JALR
                        // Pre-read rs to avoid self-overwrite when rs == rd
                        let target_pc = (self.gpr[rs] & 0xFFF) as u16;
                        self.gpr[rd] = (self.pc.wrapping_add(4) & 0xFFF) as u32;
                        self.in_delay_slot = true;
                        self.delay_slot_pc = target_pc;
                    }
                    0x0D => { // BREAK
                        self.halted = true;
                        self.broke = true;
                    }
                    0x20 => { // ADD
                        self.gpr[rd] = self.gpr[rs].wrapping_add(self.gpr[rt]);
                    }
                    0x21 => { // ADDU
                        self.gpr[rd] = self.gpr[rs].wrapping_add(self.gpr[rt]);
                    }
                    0x22 => { // SUB
                        self.gpr[rd] = self.gpr[rs].wrapping_sub(self.gpr[rt]);
                    }
                    0x23 => { // SUBU
                        self.gpr[rd] = self.gpr[rs].wrapping_sub(self.gpr[rt]);
                    }
                    0x24 => { // AND
                        self.gpr[rd] = self.gpr[rs] & self.gpr[rt];
                    }
                    0x25 => { // OR
                        self.gpr[rd] = self.gpr[rs] | self.gpr[rt];
                    }
                    0x26 => { // XOR
                        self.gpr[rd] = self.gpr[rs] ^ self.gpr[rt];
                    }
                    0x27 => { // NOR
                        self.gpr[rd] = !(self.gpr[rs] | self.gpr[rt]);
                    }
                    0x2A => { // SLT
                        let s = self.gpr[rs] as i32;
                        let t = self.gpr[rt] as i32;
                        self.gpr[rd] = if s < t { 1 } else { 0 };
                    }
                    0x2B => { // SLTU
                        self.gpr[rd] = if self.gpr[rs] < self.gpr[rt] { 1 } else { 0 };
                    }
                    _ => {}
                }
            }
            0x01 => { // REGIMM
                let rt_field = rt;
                let s = self.gpr[rs] as i32;
                match rt_field {
                    0x00 => { // BLTZ
                        if s < 0 {
                            let offset = (sign_extended_imm << 2) as i32;
                            self.in_delay_slot = true;
                            self.delay_slot_pc = ((self.pc as i32).wrapping_add(offset) & 0xFFF) as u16;
                        }
                    }
                    0x01 => { // BGEZ
                        if s >= 0 {
                            let offset = (sign_extended_imm << 2) as i32;
                            self.in_delay_slot = true;
                            self.delay_slot_pc = ((self.pc as i32).wrapping_add(offset) & 0xFFF) as u16;
                        }
                    }
                    _ => {}
                }
            }
            0x02 => { // J
                let target_pc = ((target & 0x3FF) << 2) as u16;
                self.in_delay_slot = true;
                self.delay_slot_pc = target_pc;
            }
            0x03 => { // JAL
                self.gpr[31] = (self.pc.wrapping_add(4) & 0xFFF) as u32;
                let target_pc = ((target & 0x3FF) << 2) as u16;
                self.in_delay_slot = true;
                self.delay_slot_pc = target_pc;
            }
            0x04 => { // BEQ
                if self.gpr[rs] == self.gpr[rt] {
                    let offset = (sign_extended_imm << 2) as i32;
                    self.in_delay_slot = true;
                    self.delay_slot_pc = ((self.pc as i32).wrapping_add(offset) & 0xFFF) as u16;
                }
            }
            0x05 => { // BNE
                if self.gpr[rs] != self.gpr[rt] {
                    let offset = (sign_extended_imm << 2) as i32;
                    self.in_delay_slot = true;
                    self.delay_slot_pc = ((self.pc as i32).wrapping_add(offset) & 0xFFF) as u16;
                }
            }
            0x06 => { // BLEZ
                if (self.gpr[rs] as i32) <= 0 {
                    let offset = (sign_extended_imm << 2) as i32;
                    self.in_delay_slot = true;
                    self.delay_slot_pc = ((self.pc as i32).wrapping_add(offset) & 0xFFF) as u16;
                }
            }
            0x07 => { // BGTZ
                if (self.gpr[rs] as i32) > 0 {
                    let offset = (sign_extended_imm << 2) as i32;
                    self.in_delay_slot = true;
                    self.delay_slot_pc = ((self.pc as i32).wrapping_add(offset) & 0xFFF) as u16;
                }
            }
            0x08 | 0x09 => { // ADDI / ADDIU
                self.gpr[rt] = self.gpr[rs].wrapping_add(sign_extended_imm);
            }
            0x0A => { // SLTI
                let s = self.gpr[rs] as i32;
                let imm_val = sign_extended_imm as i32;
                self.gpr[rt] = if s < imm_val { 1 } else { 0 };
            }
            0x0B => { // SLTIU
                let s = self.gpr[rs];
                self.gpr[rt] = if s < sign_extended_imm { 1 } else { 0 };
            }
            0x0C => { // ANDI
                self.gpr[rt] = self.gpr[rs] & zero_extended_imm;
            }
            0x0D => { // ORI
                self.gpr[rt] = self.gpr[rs] | zero_extended_imm;
            }
            0x0E => { // XORI
                self.gpr[rt] = self.gpr[rs] ^ zero_extended_imm;
            }
            0x0F => { // LUI
                self.gpr[rt] = zero_extended_imm << 16;
            }
            0x10 => { // COP0
                let cop_op = rs; // rs field selects COP0 sub-op
                match cop_op {
                    0x00 => { // MFC0
                        let reg = rd;
                        self.gpr[rt] = match reg {
                            0 => self.sp_mem_addr,
                            1 => self.sp_dram_addr,
                            2 => self.sp_rd_len,
                            3 => self.sp_wr_len,
                            4 => self.get_status(),
                            5 => if self.dma_full { 1 } else { 0 },
                            6 => if self.dma_busy { 1 } else { 0 },
                            7 => {
                                let val = self.semaphore.get();
                                self.semaphore.set(1);
                                val
                            }
                            8 => self.pc as u32,
                            _ => 0,
                        };
                    }
                    0x04 => { // MTC0
                        let reg = rd;
                        let val = self.gpr[rt];
                        match reg {
                            0 => self.sp_mem_addr = val & 0x1FFF,
                            1 => self.sp_dram_addr = val & 0x00FF_FFF8,
                            2 => self.sp_rd_len = val,
                            3 => self.sp_wr_len = val,
                            4 => {
                                // Write SP_STATUS_REG control commands
                                if (val & (1 << 0)) != 0 { self.halted = false; }
                                if (val & (1 << 1)) != 0 { self.halted = true; }
                                if (val & (1 << 2)) != 0 { self.broke = false; }
                                if (val & (1 << 5)) != 0 { self.single_step = false; }
                                if (val & (1 << 6)) != 0 { self.single_step = true; }
                                if (val & (1 << 7)) != 0 { self.intr_on_break = false; }
                                if (val & (1 << 8)) != 0 { self.intr_on_break = true; }
                                for i in 0..8 {
                                    let clear_bit = 9 + (i * 2);
                                    let set_bit = 10 + (i * 2);
                                    if (val & (1 << clear_bit)) != 0 { self.signals[i] = false; }
                                    if (val & (1 << set_bit)) != 0 { self.signals[i] = true; }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            0x12 => { // COP2 (Vector Unit)
                let cop_op = rs;
                match cop_op {
                    0x00 => { // MFC2
                        let elem = (instr >> 7) & 0xF;
                        let lane = (elem & 7) as usize; // Element offset mapping
                        self.gpr[rt] = self.vpr[rd][lane] as i32 as u32;
                    }
                    0x02 => { // CFC2
                        let reg = rd;
                        let val = match reg {
                            0 => self.vco as u32,
                            1 => self.vcc as u32,
                            2 => self.vce as u32,
                            _ => 0,
                        };
                        self.gpr[rt] = val;
                    }
                    0x04 => { // MTC2
                        let elem = (instr >> 7) & 0xF;
                        let lane = (elem & 7) as usize; // Element offset mapping
                        self.vpr[rd][lane] = self.gpr[rt] as i16;
                    }
                    0x06 => { // CTC2
                        let reg = rd;
                        let val = self.gpr[rt];
                        match reg {
                            0 => self.vco = val as u16,
                            1 => self.vcc = val as u16,
                            2 => self.vce = val as u8,
                            _ => {}
                        }
                    }
                    _ => {
                        // Vector instruction when bit 25 is 1
                        if (instr & (1 << 25)) != 0 {
                            let vector_op = func;
                            let e = ((instr >> 21) & 0xF) as u8;
                            let vs_idx = rd;
                            let vt_idx = rt;
                            let vd_idx = sa as usize;
                            for lane in 0..8 {
                                let vs_val = self.vpr[vs_idx][lane];
                                let vt_val = get_vt_lane(&self.vpr[vt_idx], e, lane);
                                match vector_op {
                                    0x00 => { // VADD
                                        let res = vs_val as i32 + vt_val as i32;
                                        self.vpr[vd_idx][lane] = res.clamp(-32768, 32767) as i16;
                                    }
                                    0x01 => { // VSUB
                                        let res = vs_val as i32 - vt_val as i32;
                                        self.vpr[vd_idx][lane] = res.clamp(-32768, 32767) as i16;
                                    }
                                    0x1D => { // VMRG
                                        let test = (self.vco & (1 << lane)) != 0;
                                        self.vpr[vd_idx][lane] = if test { vs_val } else { vt_val };
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
            0x20 => { // LB
                let addr = (self.gpr[rs].wrapping_add(sign_extended_imm) & 0xFFF) as usize;
                self.gpr[rt] = sp_dmem[addr] as i8 as i32 as u32;
            }
            0x21 => { // LH
                let addr = (self.gpr[rs].wrapping_add(sign_extended_imm) & 0xFFE) as usize;
                let val = u16::from_be_bytes([sp_dmem[addr], sp_dmem[addr + 1]]);
                self.gpr[rt] = val as i16 as i32 as u32;
            }
            0x23 => { // LW
                let addr = (self.gpr[rs].wrapping_add(sign_extended_imm) & 0xFFC) as usize;
                let val = u32::from_be_bytes([
                    sp_dmem[addr],
                    sp_dmem[addr + 1],
                    sp_dmem[addr + 2],
                    sp_dmem[addr + 3],
                ]);
                self.gpr[rt] = val;
            }
            0x24 => { // LBU
                let addr = (self.gpr[rs].wrapping_add(sign_extended_imm) & 0xFFF) as usize;
                self.gpr[rt] = sp_dmem[addr] as u32;
            }
            0x25 => { // LHU
                let addr = (self.gpr[rs].wrapping_add(sign_extended_imm) & 0xFFE) as usize;
                let val = u16::from_be_bytes([sp_dmem[addr], sp_dmem[addr + 1]]);
                self.gpr[rt] = val as u32;
            }
            0x28 => { // SB
                let addr = (self.gpr[rs].wrapping_add(sign_extended_imm) & 0xFFF) as usize;
                sp_dmem[addr] = self.gpr[rt] as u8;
            }
            0x29 => { // SH
                let addr = (self.gpr[rs].wrapping_add(sign_extended_imm) & 0xFFE) as usize;
                let bytes = (self.gpr[rt] as u16).to_be_bytes();
                sp_dmem[addr] = bytes[0];
                sp_dmem[addr + 1] = bytes[1];
            }
            0x2B => { // SW
                let addr = (self.gpr[rs].wrapping_add(sign_extended_imm) & 0xFFC) as usize;
                let bytes = self.gpr[rt].to_be_bytes();
                sp_dmem[addr..addr + 4].copy_from_slice(&bytes);
            }
            0x32 => { // LWC2 (Vector Load)
                let base = rs;
                let vt = rt; // VPR register to load into
                let element = (instr >> 7) & 0xF;
                let offset = (instr & 0x7F) as i8 as i32; // 7-bit signed offset
                let vaddr = (self.gpr[base].wrapping_add(offset as u32) & 0xFFF) as usize;
                
                let load_type = rd;
                match load_type {
                    0x00 => { // LBV (Load Byte Vector)
                        let lane = (element & 7) as usize;
                        self.vpr[vt][lane] = (sp_dmem[vaddr] as i8 as i16) << 8;
                    }
                    0x01 => { // LSV (Load Short Vector)
                        let lane = (element & 7) as usize;
                        if vaddr + 1 < 0x1000 {
                            let val = i16::from_be_bytes([sp_dmem[vaddr], sp_dmem[vaddr + 1]]);
                            self.vpr[vt][lane] = val;
                        }
                    }
                    0x02 => { // LDV (Load Doubleword Vector)
                        let start_lane = (element & 4) as usize;
                        for i in 0..4 {
                            let lane = start_lane + i;
                            let addr = vaddr + i * 2;
                            if addr + 1 < 0x1000 {
                                self.vpr[vt][lane] = i16::from_be_bytes([sp_dmem[addr], sp_dmem[addr + 1]]);
                            }
                        }
                    }
                    0x03 => { // LQV (Load Quadword Vector)
                        let start_lane = (element & 7) as usize;
                        for i in 0..8 {
                            let lane = (start_lane + i) & 7;
                            let addr = vaddr + i * 2;
                            if addr + 1 < 0x1000 {
                                self.vpr[vt][lane] = i16::from_be_bytes([sp_dmem[addr], sp_dmem[addr + 1]]);
                            }
                        }
                    }
                    _ => {
                        for i in 0..8 {
                            let addr = (vaddr + i * 2) & 0xFFF;
                            self.vpr[vt][i] = i16::from_be_bytes([sp_dmem[addr], sp_dmem[addr + 1]]);
                        }
                    }
                }
            }
            0x3A => { // SWC2 (Vector Store)
                let base = rs;
                let vt = rt; // VPR register to store from
                let element = (instr >> 7) & 0xF;
                let offset = (instr & 0x7F) as i8 as i32; // 7-bit signed offset
                let vaddr = (self.gpr[base].wrapping_add(offset as u32) & 0xFFF) as usize;
                
                let store_type = rd;
                match store_type {
                    0x00 => { // SBV (Store Byte Vector)
                        let lane = (element & 7) as usize;
                        sp_dmem[vaddr] = (self.vpr[vt][lane] >> 8) as u8;
                    }
                    0x01 => { // SSV (Store Short Vector)
                        let lane = (element & 7) as usize;
                        let bytes = self.vpr[vt][lane].to_be_bytes();
                        if vaddr + 1 < 0x1000 {
                            sp_dmem[vaddr] = bytes[0];
                            sp_dmem[vaddr + 1] = bytes[1];
                        }
                    }
                    0x02 => { // SDV (Store Doubleword Vector)
                        let start_lane = (element & 4) as usize;
                        for i in 0..4 {
                            let lane = start_lane + i;
                            let bytes = self.vpr[vt][lane].to_be_bytes();
                            let addr = vaddr + i * 2;
                            if addr + 1 < 0x1000 {
                                sp_dmem[addr] = bytes[0];
                                sp_dmem[addr + 1] = bytes[1];
                            }
                        }
                    }
                    0x03 => { // SQV (Store Quadword Vector)
                        let start_lane = (element & 7) as usize;
                        for i in 0..8 {
                            let lane = (start_lane + i) & 7;
                            let bytes = self.vpr[vt][lane].to_be_bytes();
                            let addr = vaddr + i * 2;
                            if addr + 1 < 0x1000 {
                                sp_dmem[addr] = bytes[0];
                                sp_dmem[addr + 1] = bytes[1];
                            }
                        }
                    }
                    _ => {
                        for i in 0..8 {
                            let bytes = self.vpr[vt][i].to_be_bytes();
                            let addr = (vaddr + i * 2) & 0xFFF;
                            sp_dmem[addr] = bytes[0];
                            sp_dmem[addr + 1] = bytes[1];
                        }
                    }
                }
            }
            _ => {}
        }

        if was_in_delay_slot {
            self.pc = self.delay_slot_pc;
        }
        self.gpr[0] = 0; // Hard-wire register $0 to 0
    }

    pub fn execute_sp_dma(
        &mut self,
        is_write: bool,
        sp_dmem: &mut [u8; 0x1000],
        sp_imem: &mut [u8; 0x1000],
        rdram: &mut crate::n64::rdram::Rdram,
    ) {
        let mem_addr_reg = self.sp_mem_addr;
        let dram_addr_reg = self.sp_dram_addr;
        let len_reg = if is_write {
            self.sp_wr_len
        } else {
            self.sp_rd_len
        };

        let mut dram_addr = dram_addr_reg & 0x00FF_FFF8;
        let mem_addr = mem_addr_reg & 0x1FF8;
        let is_imem = (mem_addr & 0x1000) != 0;
        let mut mem_offset = mem_addr & 0x0FFF;

        let length = ((len_reg & 0x0FF8) + 8) as u32;
        let count = ((len_reg >> 12) & 0x00FF) + 1;
        let skip = (len_reg >> 20) & 0x00FF;

        for _ in 0..count {
            for i in 0..length {
                let cur_dram = dram_addr + i;
                let cur_mem = (mem_offset + i) & 0x0FFF;
                if is_write {
                    let byte = if is_imem { sp_imem[cur_mem as usize] } else { sp_dmem[cur_mem as usize] };
                    rdram.write_u8(cur_dram, byte);
                } else {
                    let byte = rdram.read_u8(cur_dram);
                    if is_imem {
                        sp_imem[cur_mem as usize] = byte;
                    } else {
                        sp_dmem[cur_mem as usize] = byte;
                    }
                }
            }
            dram_addr += length + skip;
            mem_offset = (mem_offset + length) & 0x0FFF;
        }

        // DMA register writebacks:
        // Update mem_addr and dram_addr upon completion of DMA
        self.sp_mem_addr = (mem_offset & 0x1FFF) | if is_imem { 0x1000 } else { 0 };
        self.sp_dram_addr = dram_addr & 0x00FF_FFF8;
    }
}

#[inline(always)]
fn get_vt_lane(vt: &[i16; 8], e: u8, lane: usize) -> i16 {
    if (e & 0b1000) == 0 {
        vt[(e & 7) as usize]
    } else {
        match e {
            0x8 | 0x9 => vt[lane],
            0xA => vt[lane & !1],
            0xB => vt[lane | 1],
            0xC => vt[lane & !3],
            0xD => vt[(lane & !3) | 1],
            0xE => vt[(lane & !3) | 2],
            0xF => vt[(lane & !3) | 3],
            _ => vt[lane],
        }
    }
}
