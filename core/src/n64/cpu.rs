use std::fmt;

#[derive(Default, Clone, Copy, Debug)]
pub struct CpuRegisters {
    pub(crate) gpr: [u64; 32],
    pub pc: u64,
    pub hi: u64,
    pub lo: u64,
}

impl CpuRegisters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.gpr.fill(0);
        self.pc = 0;
        self.hi = 0;
        self.lo = 0;
    }

    #[inline(always)]
    pub fn read(&self, reg: usize) -> u64 {
        if reg == 0 {
            0
        } else {
            self.gpr[reg]
        }
    }

    #[inline(always)]
    pub fn write(&mut self, reg: usize, val: u64) {
        if reg != 0 {
            self.gpr[reg] = val;
        }
    }
}

#[derive(Clone, Default, Debug)]
pub struct Cp0 {
    pub index: u32,
    pub random: u32,
    pub entry_lo0: u64,
    pub entry_lo1: u64,
    pub context: u64,
    pub page_mask: u32,
    pub wired: u32,
    pub bad_vaddr: u64,
    pub count: u32,
    pub entry_hi: u64,
    pub compare: u32,
    pub status: u32,
    pub cause: u32,
    pub epc: u64,
    pub prid: u32,
    pub config: u32,
    pub error_epc: u64,
}

impl Cp0 {
    pub fn new() -> Self {
        Self {
            prid: 0x0000_0B22, // Imp = 0x0B (VR4300), Rev = 0x22
            config: 0x0006_8006, // Big-endian, cached KSEG0
            random: 31,
            ..Default::default()
        }
    }

    pub fn reset(&mut self) {
        let saved_prid = self.prid;
        let saved_config = self.config;
        *self = Self::default();
        self.prid = saved_prid;
        self.config = saved_config;
        self.random = 31;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundingMode {
    Nearest = 0,
    Zero = 1,
    PosInf = 2,
    NegInf = 3,
}

pub struct Cp1 {
    pub fgr: [u64; 32],
    pub fcr31: u32,
    pub fcr0: u32,
}

impl Cp1 {
    pub fn new() -> Self {
        Self {
            fgr: [0; 32],
            fcr31: 0,
            fcr0: 0x0000_0B22,
        }
    }

    pub fn reset(&mut self) {
        self.fgr.fill(0);
        self.fcr31 = 0;
    }

    pub fn rounding_mode(&self) -> RoundingMode {
        match self.fcr31 & 3 {
            0 => RoundingMode::Nearest,
            1 => RoundingMode::Zero,
            2 => RoundingMode::PosInf,
            3 => RoundingMode::NegInf,
            _ => unreachable!(),
        }
    }

    pub fn get_cc(&self, cc: usize) -> bool {
        let bit = if cc == 0 { 23 } else { 24 + cc };
        ((self.fcr31 >> bit) & 1) != 0
    }

    pub fn set_cc(&mut self, cc: usize, val: bool) {
        let bit = if cc == 0 { 23 } else { 24 + cc };
        if val {
            self.fcr31 |= 1 << bit;
        } else {
            self.fcr31 &= !(1 << bit);
        }
    }

    pub fn update_exception(&mut self, causes: u32) -> bool {
        self.fcr31 = (self.fcr31 & !(0x1F << 12)) | ((causes & 0x1F) << 12);
        self.fcr31 |= (causes & 0x1F) << 2; // accumulate sticky flags
        let enables = (self.fcr31 >> 7) & 0x1F;
        let triggered = (causes & enables) != 0;
        let unimplemented = (self.fcr31 & (1 << 17)) != 0;
        triggered || unimplemented
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct TlbEntry {
    pub page_mask: u32,
    pub entry_hi: u64,
    pub entry_lo0: u64,
    pub entry_lo1: u64,
}

impl TlbEntry {
    pub fn is_global(&self) -> bool {
        (self.entry_lo0 & self.entry_lo1 & 1) != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceptionType {
    Interrupt = 0,
    TlbModification = 1,
    TlbMissLoad = 2,
    TlbMissStore = 3,
    TlbInvalidLoad = 4,   // Replaced by 2 in Cause code mapping
    TlbInvalidStore = 5,  // Replaced by 3 in Cause code mapping
    AddrErrLoad = 6,
    AddrErrStore = 7,
    BusErrInstruction = 8,
    BusErrData = 9,
    Syscall = 10,
    Breakpoint = 11,
    ReservedInstruction = 12,
    CoprocessorUnusable = 13,
    Overflow = 14,
    Trap = 15,
    FloatingPointException = 16,
}

impl ExceptionType {
    pub fn exc_code(&self) -> u32 {
        match self {
            Self::Interrupt => 0,
            Self::TlbModification => 1,
            Self::TlbMissLoad | Self::TlbInvalidLoad => 2,
            Self::TlbMissStore | Self::TlbInvalidStore => 3,
            Self::AddrErrLoad => 4,
            Self::AddrErrStore => 5,
            Self::BusErrInstruction => 6,
            Self::BusErrData => 7,
            Self::Syscall => 8,
            Self::Breakpoint => 9,
            Self::ReservedInstruction => 10,
            Self::CoprocessorUnusable => 11,
            Self::Overflow => 12,
            Self::Trap => 13,
            Self::FloatingPointException => 15,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Exception {
    pub exc_type: ExceptionType,
    pub bad_vaddr: u64,
}

impl Exception {
    pub fn new(exc_type: ExceptionType, bad_vaddr: u64) -> Self {
        Self { exc_type, bad_vaddr }
    }
    pub fn new_addr_err(bad_vaddr: u64, is_write: bool) -> Self {
        let exc_type = if is_write { ExceptionType::AddrErrStore } else { ExceptionType::AddrErrLoad };
        Self { exc_type, bad_vaddr }
    }
    pub fn tlb_invalid_load(bad_vaddr: u64) -> Self {
        Self { exc_type: ExceptionType::TlbInvalidLoad, bad_vaddr }
    }
    pub fn tlb_invalid_store(bad_vaddr: u64) -> Self {
        Self { exc_type: ExceptionType::TlbInvalidStore, bad_vaddr }
    }
    pub fn tlb_miss_load(bad_vaddr: u64) -> Self {
        Self { exc_type: ExceptionType::TlbMissLoad, bad_vaddr }
    }
    pub fn tlb_miss_store(bad_vaddr: u64) -> Self {
        Self { exc_type: ExceptionType::TlbMissStore, bad_vaddr }
    }
    pub fn tlb_modification(bad_vaddr: u64) -> Self {
        Self { exc_type: ExceptionType::TlbModification, bad_vaddr }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuMode {
    Kernel = 0b00,
    Supervisor = 0b01,
    User = 0b10,
}

pub struct Cpu {
    pub regs: CpuRegisters,
    pub cp0: Cp0,
    pub cp1: Cp1,
    pub tlb: [TlbEntry; 32],
    
    // Delay slot state
    pub in_delay_slot: bool,
    pub delay_slot_pc: u64,
    pub nullify_delay_slot: bool,
}

impl Cpu {
    pub fn new() -> Self {
        Self {
            regs: CpuRegisters::new(),
            cp0: Cp0::new(),
            cp1: Cp1::new(),
            tlb: [TlbEntry::default(); 32],
            in_delay_slot: false,
            delay_slot_pc: 0,
            nullify_delay_slot: false,
        }
    }

    pub fn reset(&mut self) {
        self.regs.reset();
        self.cp0.reset();
        self.cp1.reset();
        self.tlb.fill(TlbEntry::default());
        self.in_delay_slot = false;
        self.delay_slot_pc = 0;
        self.nullify_delay_slot = false;
        
        // N64 Boot PC is 0xBFC00000 in compatibility mode segment
        self.regs.pc = 0xFFFFFFFF_BFC0_0000;
    }

    pub fn hle_boot(&mut self, mmu: &mut crate::n64::mmu::N64Mmu) {
        let rom_len = mmu.cart_rom.len();
        let src_start = 0x1000;
        let dest_start = 0x400;
        let copy_len = 1024 * 1024; // 1MB

        if rom_len > src_start {
            let avail = rom_len - src_start;
            let actual_copy = std::cmp::min(copy_len, avail);
            mmu.rdram.data[dest_start..dest_start + actual_copy]
                .copy_from_slice(&mmu.cart_rom[src_start..src_start + actual_copy]);
        }

        // Copy 4032 bytes of IPL3 (0x40..0x1000) to SP DMEM
        if rom_len >= 0x1000 {
            mmu.sp_dmem[0..4032].copy_from_slice(&mmu.cart_rom[0x40..0x1000]);
        }

        // Write PIF RAM status flags
        let country_code = if rom_len >= 64 { mmu.cart_rom[62] } else { b'E' };
        let is_pal = match country_code {
            b'D' | b'F' | b'I' | b'P' | b'S' | b'U' | b'X' | b'Y' => true,
            _ => false,
        };
        let pif_country = if is_pal { 0x3F } else { 0x7F };
        for i in 0x24..0x28 {
            mmu.pif_ram[i] = pif_country;
        }
        mmu.pif_ram[63] = 0x80;

        // Set Entry Point PC (read from header offset 0x08, sign-extend to 64-bit)
        let entry_point = if rom_len >= 12 {
            u32::from_be_bytes(mmu.cart_rom[8..12].try_into().unwrap())
        } else {
            0x8000_0400
        };
        let pc = (entry_point as i32) as i64 as u64;
        self.regs.pc = pc;

        // Initialize CPU registers
        let cic_type = crate::rom::detect_cic_type(&mmu.cart_rom);
        let seed = match cic_type {
            crate::rom::CicType::Cic6101 => 0x3F,
            crate::rom::CicType::Cic6103 => 0x78,
            crate::rom::CicType::Cic6105 => 0x91,
            crate::rom::CicType::Cic6106 => 0x85,
            crate::rom::CicType::Cic5101 => 0xAC,
            crate::rom::CicType::Unknown => 0x3F,
        };

        self.regs.write(20, 1); // $s4 = 1
        self.regs.write(22, seed); // $s6 = CIC seed
        self.regs.write(29, 0xFFFFFFFF_A400_1FF0); // $sp = 0xFFFFFFFF_A400_1FF0
        self.regs.write(31, 0xFFFFFFFF_A400_1550); // $ra = 0xFFFFFFFF_A400_1550

        // Initialize CP0 registers
        self.cp0.status = 0x3400_0000;
        self.cp0.config = 0x7006_EC43;
    }

    pub fn get_cpu_mode(&self) -> CpuMode {
        let status = self.cp0.status;
        let exl = (status & 0x02) != 0;
        let erl = (status & 0x01) != 0;
        if exl || erl {
            CpuMode::Kernel
        } else {
            match (status >> 3) & 0x3 {
                0b00 => CpuMode::Kernel,
                0b01 => CpuMode::Supervisor,
                0b10 => CpuMode::User,
                _ => CpuMode::Kernel,
            }
        }
    }

    pub fn decrement_random(&mut self) {
        let wired = self.cp0.wired & 0x1F;
        if self.cp0.random <= wired || self.cp0.random > 31 {
            self.cp0.random = 31;
        } else {
            self.cp0.random -= 1;
        }
    }

    pub fn translate_tlb(&self, vaddr: u32, is_write: bool) -> Result<u32, Exception> {
        let current_asid = (self.cp0.entry_hi & 0xFF) as u8;

        for entry in self.tlb.iter() {
            let mask = entry.page_mask & 0x01FF_E000;
            let vpn_mask = !(mask | 0x1FFF);
            let entry_vpn = (entry.entry_hi as u32) & vpn_mask;
            let vaddr_vpn = vaddr & vpn_mask;

            if entry_vpn == vaddr_vpn {
                if entry.is_global() || ((entry.entry_hi & 0xFF) as u8 == current_asid) {
                    let select_bit = (mask >> 1) + 0x1000;
                    let is_odd = (vaddr & select_bit) != 0;
                    let entry_lo = if is_odd { entry.entry_lo1 } else { entry.entry_lo0 };

                    if (entry_lo & 0x2) == 0 {
                        return Err(if is_write {
                            Exception::tlb_invalid_store(vaddr as u64)
                        } else {
                            Exception::tlb_invalid_load(vaddr as u64)
                        });
                    }

                    if is_write && ((entry_lo & 0x4) == 0) {
                        return Err(Exception::tlb_modification(vaddr as u64));
                    }

                    let offset_mask = select_bit - 1;
                    let pfn = ((entry_lo >> 6) & 0x00FF_FFFF) as u32;
                    let phys_page_base = pfn << 12;

                    let paddr = (phys_page_base & !offset_mask) | (vaddr & offset_mask);
                    return Ok(paddr);
                }
            }
        }

        Err(if is_write {
            Exception::tlb_miss_store(vaddr as u64)
        } else {
            Exception::tlb_miss_load(vaddr as u64)
        })
    }

    pub fn translate_vaddr(&self, vaddr: u64, is_write: bool) -> Result<u32, Exception> {
        let mode = self.get_cpu_mode();
        let vaddr_32 = vaddr as u32;
        
        match vaddr_32 {
            // kuseg
            0x0000_0000..=0x7FFF_FFFF => {
                self.translate_tlb(vaddr_32, is_write)
            }
            // kseg0
            0x8000_0000..=0x9FFF_FFFF => {
                if mode != CpuMode::Kernel {
                    return Err(Exception::new_addr_err(vaddr, is_write));
                }
                Ok(vaddr_32 - 0x8000_0000)
            }
            // kseg1
            0xA000_0000..=0xBFFF_FFFF => {
                if mode != CpuMode::Kernel {
                    return Err(Exception::new_addr_err(vaddr, is_write));
                }
                Ok(vaddr_32 - 0xA000_0000)
            }
            // ksseg
            0xC000_0000..=0xDFFF_FFFF => {
                if mode == CpuMode::User {
                    return Err(Exception::new_addr_err(vaddr, is_write));
                }
                self.translate_tlb(vaddr_32, is_write)
            }
            // kseg3
            0xE000_0000..=0xFFFF_FFFF => {
                if mode != CpuMode::Kernel {
                    return Err(Exception::new_addr_err(vaddr, is_write));
                }
                self.translate_tlb(vaddr_32, is_write)
            }
        }
    }

    pub fn dispatch_exception(&mut self, exc: Exception, pc: u64, in_delay_slot: bool) {
        if exc.exc_type == ExceptionType::AddrErrLoad
            || exc.exc_type == ExceptionType::AddrErrStore
            || exc.exc_type == ExceptionType::TlbMissLoad
            || exc.exc_type == ExceptionType::TlbMissStore
            || exc.exc_type == ExceptionType::TlbInvalidLoad
            || exc.exc_type == ExceptionType::TlbInvalidStore
            || exc.exc_type == ExceptionType::TlbModification
        {
            self.cp0.bad_vaddr = exc.bad_vaddr;
            let bad_vpn2 = ((exc.bad_vaddr >> 13) & 0x7F_FFFF) as u64;
            self.cp0.context = (self.cp0.context & 0xFFFF_FF00_0000_0000) | (bad_vpn2 << 4);
        }

        let was_exl_set = (self.cp0.status & 0x2) != 0;
        if !was_exl_set {
            if in_delay_slot {
                self.cp0.epc = pc.wrapping_sub(4);
                self.cp0.cause |= 0x8000_0000;
            } else {
                self.cp0.epc = pc;
                self.cp0.cause &= !0x8000_0000;
            }
        }

        self.cp0.cause = (self.cp0.cause & !(0x1F << 2)) | ((exc.exc_type.exc_code() as u32) << 2);
        self.cp0.status |= 0x2; // EXL = 1

        let is_bev = (self.cp0.status & 0x0040_0000) != 0;
        let base = if is_bev { 0xFFFFFFFF_BFC0_0000 } else { 0xFFFFFFFF_8000_0000 };

        let is_tlb_refill = exc.exc_type == ExceptionType::TlbMissLoad || exc.exc_type == ExceptionType::TlbMissStore;
        let offset = if is_tlb_refill && !was_exl_set {
            if is_bev { 0x200 } else { 0x000 }
        } else {
            if is_bev { 0x380 } else { 0x180 }
        };

        self.regs.pc = base.wrapping_add(offset);
        self.in_delay_slot = false;
        self.nullify_delay_slot = false;
    }

    #[inline]
    pub fn get_fgr_32(&self, reg_idx: usize) -> u32 {
        self.cp1.fgr[reg_idx] as u32
    }

    #[inline]
    pub fn set_fgr_32(&mut self, reg_idx: usize, val: u32) {
        self.cp1.fgr[reg_idx] = val as u64;
    }

    #[inline]
    pub fn get_fgr_64(&self, reg_idx: usize) -> u64 {
        let fr = (self.cp0.status & (1 << 26)) != 0;
        if fr {
            self.cp1.fgr[reg_idx]
        } else {
            let even = reg_idx & !1;
            let low = self.cp1.fgr[even] as u32 as u64;
            let high = self.cp1.fgr[even + 1] as u32 as u64;
            (high << 32) | low
        }
    }

    #[inline]
    pub fn set_fgr_64(&mut self, reg_idx: usize, val: u64) {
        let fr = (self.cp0.status & (1 << 26)) != 0;
        if fr {
            self.cp1.fgr[reg_idx] = val;
        } else {
            let even = reg_idx & !1;
            self.cp1.fgr[even] = (val & 0xFFFFFFFF) as u64;
            self.cp1.fgr[even + 1] = (val >> 32) as u64;
        }
    }

    fn read_bus_byte(&mut self, vaddr: u64, mmu: &crate::n64::mmu::N64Mmu) -> Result<u8, Exception> {
        let paddr = self.translate_vaddr(vaddr, false)?;
        Ok(mmu.read_byte(paddr))
    }

    fn write_bus_byte(&mut self, vaddr: u64, val: u8, mmu: &mut crate::n64::mmu::N64Mmu) -> Result<(), Exception> {
        let paddr = self.translate_vaddr(vaddr, true)?;
        mmu.write_byte(paddr, val);
        Ok(())
    }

    fn read_bus_halfword(&mut self, vaddr: u64, mmu: &crate::n64::mmu::N64Mmu) -> Result<u16, Exception> {
        if vaddr & 1 != 0 {
            return Err(Exception::new_addr_err(vaddr, false));
        }
        let paddr = self.translate_vaddr(vaddr, false)?;
        Ok(mmu.read_halfword(paddr))
    }

    fn write_bus_halfword(&mut self, vaddr: u64, val: u16, mmu: &mut crate::n64::mmu::N64Mmu) -> Result<(), Exception> {
        if vaddr & 1 != 0 {
            return Err(Exception::new_addr_err(vaddr, true));
        }
        let paddr = self.translate_vaddr(vaddr, true)?;
        mmu.write_halfword(paddr, val);
        Ok(())
    }

    fn read_bus_word(&mut self, vaddr: u64, mmu: &crate::n64::mmu::N64Mmu) -> Result<u32, Exception> {
        if vaddr & 3 != 0 {
            return Err(Exception::new_addr_err(vaddr, false));
        }
        let paddr = self.translate_vaddr(vaddr, false)?;
        Ok(mmu.read_word(paddr))
    }

    fn write_bus_word(&mut self, vaddr: u64, val: u32, mmu: &mut crate::n64::mmu::N64Mmu) -> Result<(), Exception> {
        if vaddr & 3 != 0 {
            return Err(Exception::new_addr_err(vaddr, true));
        }
        let paddr = self.translate_vaddr(vaddr, true)?;
        mmu.write_word(paddr, val);
        Ok(())
    }

    fn read_bus_doubleword(&mut self, vaddr: u64, mmu: &crate::n64::mmu::N64Mmu) -> Result<u64, Exception> {
        if vaddr & 7 != 0 {
            return Err(Exception::new_addr_err(vaddr, false));
        }
        let paddr = self.translate_vaddr(vaddr, false)?;
        Ok(mmu.read_doubleword(paddr))
    }

    fn write_bus_doubleword(&mut self, vaddr: u64, val: u64, mmu: &mut crate::n64::mmu::N64Mmu) -> Result<(), Exception> {
        if vaddr & 7 != 0 {
            return Err(Exception::new_addr_err(vaddr, true));
        }
        let paddr = self.translate_vaddr(vaddr, true)?;
        mmu.write_doubleword(paddr, val);
        Ok(())
    }

    fn tlb_probe(&mut self) {
        let entry_hi = self.cp0.entry_hi;
        let vpn2 = entry_hi & 0xFFFF_E000;
        let asid = entry_hi & 0xFF;
        
        let mut found = false;
        for (idx, entry) in self.tlb.iter().enumerate() {
            let mask = entry.page_mask & 0x01FF_E000;
            let vpn_mask = !((mask | 0x1FFF) as u64);
            let entry_vpn = entry.entry_hi & vpn_mask;
            let vaddr_vpn = vpn2 & vpn_mask;
            
            if entry_vpn == vaddr_vpn {
                if entry.is_global() || (entry.entry_hi & 0xFF) == asid {
                    self.cp0.index = idx as u32;
                    found = true;
                    break;
                }
            }
        }
        
        if !found {
            self.cp0.index = 0x8000_0000;
        }
    }

    pub fn step(&mut self, mmu: &mut crate::n64::mmu::N64Mmu) -> u32 {
        self.decrement_random();
        
        // Count register increments at half CPU frequency
        self.cp0.count = self.cp0.count.wrapping_add(1);
        if self.cp0.count == self.cp0.compare {
            // Set pending timer interrupt (IP7 = Cause bit 15)
            self.cp0.cause |= 1 << 15;
        }

        // Sync MI interrupts to CPU CP0 Cause IP2 (bit 10)
        if mmu.is_rcp_interrupt_pending() {
            self.cp0.cause |= 1 << 10; // Set IP2
        } else {
            self.cp0.cause &= !(1 << 10); // Clear IP2
        }

        // Dispatch general interrupts if enabled and pending
        let ie = (self.cp0.status & 1) != 0;
        let exl = (self.cp0.status & 2) != 0;
        let erl = (self.cp0.status & 4) != 0;
        let pending = (self.cp0.cause & self.cp0.status) & 0xFF00;
        
        let current_pc = self.regs.pc;

        if ie && !exl && !erl && pending != 0 {
            self.dispatch_exception(Exception::new(ExceptionType::Interrupt, 0), current_pc, self.in_delay_slot);
            return 1; // Take interrupt exception instead of normal instruction
        }

        if current_pc & 3 != 0 {
            self.dispatch_exception(Exception::new_addr_err(current_pc, false), current_pc, self.in_delay_slot);
            return 1;
        }

        let paddr = match self.translate_vaddr(current_pc, false) {
            Ok(addr) => addr,
            Err(e) => {
                self.dispatch_exception(e, current_pc, self.in_delay_slot);
                return 1;
            }
        };

        let instr = mmu.read_word(paddr);
        let mut next_pc = current_pc.wrapping_add(4);

        let was_in_delay_slot = self.in_delay_slot;
        let was_nullified = self.nullify_delay_slot;

        if was_in_delay_slot {
            next_pc = self.delay_slot_pc;
            self.in_delay_slot = false;
        }

        if was_nullified {
            self.nullify_delay_slot = false;
            self.regs.pc = next_pc;
            return 1;
        }

        let exc = self.execute_instruction(instr, current_pc, mmu);

        if let Err(e) = exc {
            self.dispatch_exception(e, current_pc, was_in_delay_slot);
        } else {
            if !self.in_delay_slot {
                self.regs.pc = next_pc;
            } else {
                self.regs.pc = current_pc.wrapping_add(4);
            }
        }

        1 // Returns 1 cycle
    }

    fn execute_instruction(&mut self, instr: u32, current_pc: u64, mmu: &mut crate::n64::mmu::N64Mmu) -> Result<(), Exception> {
        let op = instr >> 26;
        let rs = ((instr >> 21) & 0x1F) as usize;
        let rt = ((instr >> 16) & 0x1F) as usize;
        let rd = ((instr >> 11) & 0x1F) as usize;
        let sa = (instr >> 6) & 0x1F;
        let func = instr & 0x3F;
        let imm = (instr & 0xFFFF) as u16;
        let target = instr & 0x03FF_FFFF;

        let sign_extended_imm = imm as i16 as i64 as u64;
        let zero_extended_imm = imm as u64;

        let fs = rd; // FPU fs field is rd in COP1 instructions

        match op {
            0x00 => { // SPECIAL
                match func {
                    0x00 => { // SLL
                        let res = (self.regs.read(rt) as u32) << sa;
                        self.regs.write(rd, res as i32 as i64 as u64);
                    }
                    0x02 => { // SRL
                        let res = (self.regs.read(rt) as u32) >> sa;
                        self.regs.write(rd, res as i32 as i64 as u64);
                    }
                    0x03 => { // SRA
                        let res = (self.regs.read(rt) as i32) >> sa;
                        self.regs.write(rd, res as i64 as u64);
                    }
                    0x04 => { // SLLV
                        let shift = self.regs.read(rs) & 0x1F;
                        let res = (self.regs.read(rt) as u32) << shift;
                        self.regs.write(rd, res as i32 as i64 as u64);
                    }
                    0x06 => { // SRLV
                        let shift = self.regs.read(rs) & 0x1F;
                        let res = (self.regs.read(rt) as u32) >> shift;
                        self.regs.write(rd, res as i32 as i64 as u64);
                    }
                    0x07 => { // SRAV
                        let shift = self.regs.read(rs) & 0x1F;
                        let res = (self.regs.read(rt) as i32) >> shift;
                        self.regs.write(rd, res as i64 as u64);
                    }
                    0x08 => { // JR
                        let target_pc = self.regs.read(rs);
                        self.in_delay_slot = true;
                        self.delay_slot_pc = target_pc;
                    }
                    0x09 => { // JALR
                        let target_pc = self.regs.read(rs);
                        self.regs.write(rd, current_pc.wrapping_add(8));
                        self.in_delay_slot = true;
                        self.delay_slot_pc = target_pc;
                    }
                    0x0C => { // SYSCALL
                        return Err(Exception::new(ExceptionType::Syscall, current_pc));
                    }
                    0x0D => { // BREAK
                        return Err(Exception::new(ExceptionType::Breakpoint, current_pc));
                    }
                    0x10 => { // MFHI
                        self.regs.write(rd, self.regs.hi);
                    }
                    0x11 => { // MTHI
                        self.regs.hi = self.regs.read(rs);
                    }
                    0x12 => { // MFLO
                        self.regs.write(rd, self.regs.lo);
                    }
                    0x13 => { // MTLO
                        self.regs.lo = self.regs.read(rs);
                    }
                    0x14 => { // DSLLV
                        let shift = self.regs.read(rs) & 0x3F;
                        self.regs.write(rd, self.regs.read(rt) << shift);
                    }
                    0x16 => { // DSRLV
                        let shift = self.regs.read(rs) & 0x3F;
                        self.regs.write(rd, self.regs.read(rt) >> shift);
                    }
                    0x17 => { // DSRAV
                        let shift = self.regs.read(rs) & 0x3F;
                        let res = (self.regs.read(rt) as i64) >> shift;
                        self.regs.write(rd, res as u64);
                    }
                    0x18 => { // MULT
                        let a = self.regs.read(rs) as i32 as i64;
                        let b = self.regs.read(rt) as i32 as i64;
                        let prod = a * b;
                        self.regs.lo = (prod as u32) as i32 as i64 as u64;
                        self.regs.hi = ((prod >> 32) as u32) as i32 as i64 as u64;
                    }
                    0x19 => { // MULTU
                        let a = (self.regs.read(rs) as u32) as u64;
                        let b = (self.regs.read(rt) as u32) as u64;
                        let prod = a * b;
                        self.regs.lo = (prod as u32) as i32 as i64 as u64;
                        self.regs.hi = ((prod >> 32) as u32) as i32 as i64 as u64;
                    }
                    0x1A => { // DIV
                        let divisor = self.regs.read(rt) as i32;
                        if divisor != 0 {
                            let dividend = self.regs.read(rs) as i32;
                            self.regs.lo = (dividend / divisor) as i64 as u64;
                            self.regs.hi = (dividend % divisor) as i64 as u64;
                        }
                    }
                    0x1B => { // DIVU
                        let divisor = self.regs.read(rt) as u32;
                        if divisor != 0 {
                            let dividend = self.regs.read(rs) as u32;
                            self.regs.lo = (dividend / divisor) as i32 as i64 as u64;
                            self.regs.hi = (dividend % divisor) as i32 as i64 as u64;
                        }
                    }
                    0x1C => { // DMULT
                        let a = self.regs.read(rs) as i64 as i128;
                        let b = self.regs.read(rt) as i64 as i128;
                        let prod = a * b;
                        self.regs.lo = prod as u64;
                        self.regs.hi = (prod >> 64) as u64;
                    }
                    0x1D => { // DMULTU
                        let a = self.regs.read(rs) as u128;
                        let b = self.regs.read(rt) as u128;
                        let prod = a * b;
                        self.regs.lo = prod as u64;
                        self.regs.hi = (prod >> 64) as u64;
                    }
                    0x1E => { // DDIV
                        let divisor = self.regs.read(rt) as i64;
                        if divisor != 0 {
                            let dividend = self.regs.read(rs) as i64;
                            self.regs.lo = (dividend / divisor) as u64;
                            self.regs.hi = (dividend % divisor) as u64;
                        }
                    }
                    0x1F => { // DDIVU
                        let divisor = self.regs.read(rt);
                        if divisor != 0 {
                            let dividend = self.regs.read(rs);
                            self.regs.lo = dividend / divisor;
                            self.regs.hi = dividend % divisor;
                        }
                    }
                    0x20 => { // ADD
                        let a = self.regs.read(rs) as i32;
                        let b = self.regs.read(rt) as i32;
                        match a.checked_add(b) {
                            Some(res) => self.regs.write(rd, res as i64 as u64),
                            None => return Err(Exception::new(ExceptionType::Overflow, current_pc)),
                        }
                    }
                    0x21 => { // ADDU
                        let a = self.regs.read(rs) as i32;
                        let b = self.regs.read(rt) as i32;
                        let res = a.wrapping_add(b);
                        self.regs.write(rd, res as i64 as u64);
                    }
                    0x22 => { // SUB
                        let a = self.regs.read(rs) as i32;
                        let b = self.regs.read(rt) as i32;
                        match a.checked_sub(b) {
                            Some(res) => self.regs.write(rd, res as i64 as u64),
                            None => return Err(Exception::new(ExceptionType::Overflow, current_pc)),
                        }
                    }
                    0x23 => { // SUBU
                        let a = self.regs.read(rs) as i32;
                        let b = self.regs.read(rt) as i32;
                        let res = a.wrapping_sub(b);
                        self.regs.write(rd, res as i64 as u64);
                    }
                    0x24 => { // AND
                        self.regs.write(rd, self.regs.read(rs) & self.regs.read(rt));
                    }
                    0x25 => { // OR
                        self.regs.write(rd, self.regs.read(rs) | self.regs.read(rt));
                    }
                    0x26 => { // XOR
                        self.regs.write(rd, self.regs.read(rs) ^ self.regs.read(rt));
                    }
                    0x27 => { // NOR
                        self.regs.write(rd, !(self.regs.read(rs) | self.regs.read(rt)));
                    }
                    0x2A => { // SLT
                        let res = if (self.regs.read(rs) as i64) < (self.regs.read(rt) as i64) { 1 } else { 0 };
                        self.regs.write(rd, res);
                    }
                    0x2B => { // SLTU
                        let res = if self.regs.read(rs) < self.regs.read(rt) { 1 } else { 0 };
                        self.regs.write(rd, res);
                    }
                    0x2C => { // DADD
                        let a = self.regs.read(rs) as i64;
                        let b = self.regs.read(rt) as i64;
                        match a.checked_add(b) {
                            Some(res) => self.regs.write(rd, res as u64),
                            None => return Err(Exception::new(ExceptionType::Overflow, current_pc)),
                        }
                    }
                    0x2D => { // DADDU
                        let a = self.regs.read(rs);
                        let b = self.regs.read(rt);
                        self.regs.write(rd, a.wrapping_add(b));
                    }
                    0x2E => { // DSUB
                        let a = self.regs.read(rs) as i64;
                        let b = self.regs.read(rt) as i64;
                        match a.checked_sub(b) {
                            Some(res) => self.regs.write(rd, res as u64),
                            None => return Err(Exception::new(ExceptionType::Overflow, current_pc)),
                        }
                    }
                    0x2F => { // DSUBU
                        let a = self.regs.read(rs);
                        let b = self.regs.read(rt);
                        self.regs.write(rd, a.wrapping_sub(b));
                    }
                    0x38 => { // DSLL
                        self.regs.write(rd, self.regs.read(rt) << sa);
                    }
                    0x3A => { // DSRL
                        self.regs.write(rd, self.regs.read(rt) >> sa);
                    }
                    0x3B => { // DSRA
                        let res = (self.regs.read(rt) as i64) >> sa;
                        self.regs.write(rd, res as u64);
                    }
                    0x3C => { // DSLL32
                        self.regs.write(rd, self.regs.read(rt) << (sa + 32));
                    }
                    0x3E => { // DSRL32
                        self.regs.write(rd, self.regs.read(rt) >> (sa + 32));
                    }
                    0x3F => { // DSRA32
                        let res = (self.regs.read(rt) as i64) >> (sa + 32);
                        self.regs.write(rd, res as u64);
                    }
                    _ => return Err(Exception::new(ExceptionType::ReservedInstruction, current_pc)),
                }
            }
            0x01 => { // REGIMM
                let branch_target = current_pc.wrapping_add(4).wrapping_add(sign_extended_imm << 2);
                let rt_op = rt;
                match rt_op {
                    0x00 => { // BLTZ
                        if (self.regs.read(rs) as i64) < 0 {
                            self.in_delay_slot = true;
                            self.delay_slot_pc = branch_target;
                        }
                    }
                    0x01 => { // BGEZ
                        if (self.regs.read(rs) as i64) >= 0 {
                            self.in_delay_slot = true;
                            self.delay_slot_pc = branch_target;
                        }
                    }
                    0x02 => { // BLTZL
                        if (self.regs.read(rs) as i64) < 0 {
                            self.in_delay_slot = true;
                            self.delay_slot_pc = branch_target;
                        } else {
                            self.nullify_delay_slot = true;
                        }
                    }
                    0x03 => { // BGEZL
                        if (self.regs.read(rs) as i64) >= 0 {
                            self.in_delay_slot = true;
                            self.delay_slot_pc = branch_target;
                        } else {
                            self.nullify_delay_slot = true;
                        }
                    }
                    _ => return Err(Exception::new(ExceptionType::ReservedInstruction, current_pc)),
                }
            }
            0x02 => { // J
                let target_pc = ((current_pc.wrapping_add(4)) & 0xFFFFFFFF_F000_0000) | ((target as u64) << 2);
                self.in_delay_slot = true;
                self.delay_slot_pc = target_pc;
            }
            0x03 => { // JAL
                let target_pc = ((current_pc.wrapping_add(4)) & 0xFFFFFFFF_F000_0000) | ((target as u64) << 2);
                self.regs.write(31, current_pc.wrapping_add(8));
                self.in_delay_slot = true;
                self.delay_slot_pc = target_pc;
            }
            0x04 => { // BEQ
                let branch_target = current_pc.wrapping_add(4).wrapping_add(sign_extended_imm << 2);
                if self.regs.read(rs) == self.regs.read(rt) {
                    self.in_delay_slot = true;
                    self.delay_slot_pc = branch_target;
                }
            }
            0x05 => { // BNE
                let branch_target = current_pc.wrapping_add(4).wrapping_add(sign_extended_imm << 2);
                if self.regs.read(rs) != self.regs.read(rt) {
                    self.in_delay_slot = true;
                    self.delay_slot_pc = branch_target;
                }
            }
            0x06 => { // BLEZ
                let branch_target = current_pc.wrapping_add(4).wrapping_add(sign_extended_imm << 2);
                if (self.regs.read(rs) as i64) <= 0 {
                    self.in_delay_slot = true;
                    self.delay_slot_pc = branch_target;
                }
            }
            0x07 => { // BGTZ
                let branch_target = current_pc.wrapping_add(4).wrapping_add(sign_extended_imm << 2);
                if (self.regs.read(rs) as i64) > 0 {
                    self.in_delay_slot = true;
                    self.delay_slot_pc = branch_target;
                }
            }
            0x08 => { // ADDI
                let a = self.regs.read(rs) as i32;
                let b = sign_extended_imm as i32;
                match a.checked_add(b) {
                    Some(res) => self.regs.write(rt, res as i64 as u64),
                    None => return Err(Exception::new(ExceptionType::Overflow, current_pc)),
                }
            }
            0x09 => { // ADDIU
                let a = self.regs.read(rs) as i32;
                let b = sign_extended_imm as i32;
                let res = a.wrapping_add(b);
                self.regs.write(rt, res as i64 as u64);
            }
            0x0A => { // SLTI
                let res = if (self.regs.read(rs) as i64) < (sign_extended_imm as i64) { 1 } else { 0 };
                self.regs.write(rt, res);
            }
            0x0B => { // SLTIU
                let res = if self.regs.read(rs) < sign_extended_imm { 1 } else { 0 };
                self.regs.write(rt, res);
            }
            0x0C => { // ANDI
                self.regs.write(rt, self.regs.read(rs) & zero_extended_imm);
            }
            0x0D => { // ORI
                self.regs.write(rt, self.regs.read(rs) | zero_extended_imm);
            }
            0x0E => { // XORI
                self.regs.write(rt, self.regs.read(rs) ^ zero_extended_imm);
            }
            0x0F => { // LUI
                let res = (zero_extended_imm << 16) as i32 as i64 as u64;
                self.regs.write(rt, res);
            }
            0x10 => { // COP0
                match rs {
                    0x00 => { // MFC0
                        let val = match rd {
                            0 => self.cp0.index as u64,
                            1 => self.cp0.random as u64,
                            2 => self.cp0.entry_lo0,
                            3 => self.cp0.entry_lo1,
                            4 => self.cp0.context,
                            5 => self.cp0.page_mask as u64,
                            6 => self.cp0.wired as u64,
                            8 => self.cp0.bad_vaddr,
                            9 => self.cp0.count as u64,
                            10 => self.cp0.entry_hi,
                            11 => self.cp0.compare as u64,
                            12 => self.cp0.status as u64,
                            13 => self.cp0.cause as u64,
                            14 => self.cp0.epc,
                            15 => self.cp0.prid as u64,
                            16 => self.cp0.config as u64,
                            30 => self.cp0.error_epc,
                            _ => 0,
                        };
                        self.regs.write(rt, val);
                    }
                    0x04 => { // MTC0
                        let val = self.regs.read(rt);
                        match rd {
                            0 => self.cp0.index = (val as u32) & 0x8000001F,
                            1 => {} // Random is read-only
                            2 => self.cp0.entry_lo0 = val & 0x03FF_FFFF,
                            3 => self.cp0.entry_lo1 = val & 0x03FF_FFFF,
                            4 => self.cp0.context = val & 0xFFFF_FFF0,
                            5 => self.cp0.page_mask = (val as u32) & 0x01FF_E000,
                            6 => self.cp0.wired = (val as u32) & 0x1F,
                            8 => {} // BadVAddr is read-only
                            9 => self.cp0.count = val as u32,
                            10 => self.cp0.entry_hi = val & 0xC000_00FF_FFFF_E0FF,
                            11 => {
                                self.cp0.compare = val as u32;
                                self.cp0.cause &= !(1 << 15); // Clear pending timer interrupt
                            }
                            12 => self.cp0.status = (val as u32) & 0xFF57_FFFF,
                            13 => self.cp0.cause = (self.cp0.cause & !0x0000_0300) | ((val as u32) & 0x0000_0300),
                            14 => self.cp0.epc = val,
                            16 => self.cp0.config = (self.cp0.config & !0xF) | ((val as u32) & 0xF),
                            30 => self.cp0.error_epc = val,
                            _ => {}
                        }
                    }
                    0x10 => { // TLB instructions (MIPS TLBWI, TLBWR, TLBR, TLBP, ERET)
                        match func {
                            0x01 => { // TLBR
                                let idx = (self.cp0.index & 0x1F) as usize;
                                let entry = self.tlb[idx];
                                self.cp0.page_mask = entry.page_mask;
                                self.cp0.entry_hi = entry.entry_hi;
                                self.cp0.entry_lo0 = entry.entry_lo0;
                                self.cp0.entry_lo1 = entry.entry_lo1;
                            }
                            0x02 => { // TLBWI
                                let idx = (self.cp0.index & 0x1F) as usize;
                                self.tlb[idx] = TlbEntry {
                                    page_mask: self.cp0.page_mask,
                                    entry_hi: self.cp0.entry_hi,
                                    entry_lo0: self.cp0.entry_lo0,
                                    entry_lo1: self.cp0.entry_lo1,
                                };
                            }
                            0x06 => { // TLBWR
                                let idx = (self.cp0.random & 0x1F) as usize;
                                self.tlb[idx] = TlbEntry {
                                    page_mask: self.cp0.page_mask,
                                    entry_hi: self.cp0.entry_hi,
                                    entry_lo0: self.cp0.entry_lo0,
                                    entry_lo1: self.cp0.entry_lo1,
                                };
                            }
                            0x08 => { // TLBP
                                self.tlb_probe();
                            }
                            0x18 => { // ERET
                                if (self.cp0.status & 0x2) != 0 {
                                    self.regs.pc = self.cp0.epc;
                                    self.cp0.status &= !0x2; // Clear Status.EXL
                                } else {
                                    self.regs.pc = self.cp0.error_epc;
                                    self.cp0.status &= !0x1; // Clear Status.ERL
                                }
                                self.in_delay_slot = false;
                                self.nullify_delay_slot = false;
                                // Jump directly to new PC, skip normal branch offset additions
                                return Ok(());
                            }
                            _ => return Err(Exception::new(ExceptionType::ReservedInstruction, current_pc)),
                        }
                    }
                    _ => return Err(Exception::new(ExceptionType::ReservedInstruction, current_pc)),
                }
            }
            0x11 => { // COP1 (FPU)
                // Check Status.CU1
                if (self.cp0.status & (1 << 29)) == 0 {
                    let exc = Exception::new(ExceptionType::CoprocessorUnusable, current_pc);
                    // Coprocessor 1 is code 1
                    self.cp0.cause = (self.cp0.cause & !(3 << 28)) | (1 << 28);
                    return Err(exc);
                }

                let sub = rs;
                let fs_reg = ((instr >> 11) & 0x1F) as usize;
                match sub {
                    0x00 => { // MFC1
                        let val = self.get_fgr_32(fs_reg) as i32 as i64 as u64;
                        self.regs.write(rt, val);
                    }
                    0x01 => { // DMFC1
                        let val = self.get_fgr_64(fs_reg);
                        self.regs.write(rt, val);
                    }
                    0x04 => { // MTC1
                        let val = self.regs.read(rt) as u32;
                        self.set_fgr_32(fs_reg, val);
                    }
                    0x05 => { // DMTC1
                        let val = self.regs.read(rt);
                        self.set_fgr_64(fs_reg, val);
                    }
                    0x02 => { // CFC1
                        let val = if fs_reg == 31 {
                            self.cp1.fcr31 as i32 as i64 as u64
                        } else if fs_reg == 0 {
                            self.cp1.fcr0 as i32 as i64 as u64
                        } else {
                            0
                        };
                        self.regs.write(rt, val);
                    }
                    0x06 => { // CTC1
                        let val = self.regs.read(rt) as u32;
                        if fs_reg == 31 {
                            self.cp1.fcr31 = val;
                            // Check for FPE trigger
                            let enables = (self.cp1.fcr31 >> 7) & 0x1F;
                            let causes = (self.cp1.fcr31 >> 12) & 0x1F;
                            if (enables & causes) != 0 || (self.cp1.fcr31 & (1 << 17)) != 0 {
                                return Err(Exception::new(ExceptionType::FloatingPointException, current_pc));
                            }
                        }
                    }
                    0x08 => { // BC1
                        let cc = (instr >> 18) & 7;
                        let nd = (instr >> 17) & 1;
                        let tf = (instr >> 16) & 1;
                        let branch_target = current_pc.wrapping_add(4).wrapping_add(sign_extended_imm << 2);
                        
                        let cond_met = self.cp1.get_cc(cc as usize) == (tf != 0);
                        if nd != 0 { // Branch Likely
                            if cond_met {
                                self.in_delay_slot = true;
                                self.delay_slot_pc = branch_target;
                            } else {
                                self.nullify_delay_slot = true;
                            }
                        } else { // Normal Branch
                            if cond_met {
                                self.in_delay_slot = true;
                                self.delay_slot_pc = branch_target;
                            }
                        }
                    }
                    _ => { // Floating point arithmetic (fmt)
                        let fmt = sub;
                        let fd_reg = rd;
                        let ft_reg = rt;

                        if fmt == 16 { // Single Precision (S)
                            let fs_val = f32::from_bits(self.get_fgr_32(fs_reg));
                            let ft_val = f32::from_bits(self.get_fgr_32(ft_reg));

                            match func {
                                0x00 => { // ADD.S
                                     let res = fs_val + ft_val;
                                     self.set_fgr_32(fd_reg, res.to_bits());
                                }
                                0x01 => { // SUB.S
                                     let res = fs_val - ft_val;
                                     self.set_fgr_32(fd_reg, res.to_bits());
                                }
                                0x02 => { // MUL.S
                                     let res = fs_val * ft_val;
                                     self.set_fgr_32(fd_reg, res.to_bits());
                                }
                                0x03 => { // DIV.S
                                     let res = fs_val / ft_val;
                                     self.set_fgr_32(fd_reg, res.to_bits());
                                }
                                0x04 => { // SQRT.S
                                     let res = fs_val.sqrt();
                                     self.set_fgr_32(fd_reg, res.to_bits());
                                }
                                0x05 => { // ABS.S
                                     let res = fs_val.abs();
                                     self.set_fgr_32(fd_reg, res.to_bits());
                                }
                                0x06 => { // MOV.S
                                     self.set_fgr_32(fd_reg, fs_val.to_bits());
                                }
                                0x07 => { // NEG.S
                                     let res = -fs_val;
                                     self.set_fgr_32(fd_reg, res.to_bits());
                                }
                                0x11 => { // MOVT.S / MOVF.S
                                     let cc = (instr >> 18) & 7;
                                     let tf = (instr >> 16) & 1;
                                     if self.cp1.get_cc(cc as usize) == (tf != 0) {
                                         self.set_fgr_32(fd_reg, fs_val.to_bits());
                                     }
                                }
                                0x21 => { // CVT.D.S
                                     let res = fs_val as f64;
                                     self.set_fgr_64(fd_reg, res.to_bits());
                                }
                                0x24 => { // CVT.W.S
                                     let mode = self.cp1.rounding_mode();
                                     let res = round_by_mode(fs_val as f64, mode) as i32;
                                     self.set_fgr_32(fd_reg, res as u32);
                                }
                                0x25 => { // CVT.L.S
                                     let mode = self.cp1.rounding_mode();
                                     let res = round_by_mode(fs_val as f64, mode) as i64;
                                     self.set_fgr_64(fd_reg, res as u64);
                                }
                                0x08 | 0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x0E | 0x0F => { // Explicit Rounding S
                                     let rounded = match func {
                                         8 | 12 => round_nearest_even(fs_val as f64), // ROUND.L / ROUND.W
                                         9 | 13 => (fs_val as f64).trunc(),          // TRUNC.L / TRUNC.W
                                         10 | 14 => (fs_val as f64).ceil(),          // CEIL.L / CEIL.W
                                         11 | 15 => (fs_val as f64).floor(),         // FLOOR.L / FLOOR.W
                                         _ => unreachable!(),
                                     };
                                     if func & 4 != 0 { // .W
                                         self.set_fgr_32(fd_reg, rounded as i32 as u32);
                                     } else { // .L
                                         self.set_fgr_64(fd_reg, rounded as i64 as u64);
                                     }
                                }
                                0x30..=0x3F => { // Compare C.cond.S
                                     let cond = func & 0xF;
                                     let (res, signal_invalid) = execute_compare(cond, fs_val as f64, ft_val as f64);
                                     let cc = (instr >> 8) & 7;
                                     self.cp1.set_cc(cc as usize, res);
                                     if signal_invalid {
                                         if self.cp1.update_exception(1) { // 1 = Invalid Operation
                                             return Err(Exception::new(ExceptionType::FloatingPointException, current_pc));
                                         }
                                     }
                                }
                                _ => return Err(Exception::new(ExceptionType::ReservedInstruction, current_pc)),
                            }
                        } else if fmt == 17 { // Double Precision (D)
                            let fs_val = f64::from_bits(self.get_fgr_64(fs_reg));
                            let ft_val = f64::from_bits(self.get_fgr_64(ft_reg));

                            match func {
                                0x00 => { // ADD.D
                                     let res = fs_val + ft_val;
                                     self.set_fgr_64(fd_reg, res.to_bits());
                                }
                                0x01 => { // SUB.D
                                     let res = fs_val - ft_val;
                                     self.set_fgr_64(fd_reg, res.to_bits());
                                }
                                0x02 => { // MUL.D
                                     let res = fs_val * ft_val;
                                     self.set_fgr_64(fd_reg, res.to_bits());
                                }
                                0x03 => { // DIV.D
                                     let res = fs_val / ft_val;
                                     self.set_fgr_64(fd_reg, res.to_bits());
                                }
                                0x04 => { // SQRT.D
                                     let res = fs_val.sqrt();
                                     self.set_fgr_64(fd_reg, res.to_bits());
                                }
                                0x05 => { // ABS.D
                                     let res = fs_val.abs();
                                     self.set_fgr_64(fd_reg, res.to_bits());
                                }
                                0x06 => { // MOV.D
                                     self.set_fgr_64(fd_reg, fs_val.to_bits());
                                }
                                0x07 => { // NEG.D
                                     let res = -fs_val;
                                     self.set_fgr_64(fd_reg, res.to_bits());
                                }
                                0x11 => { // MOVT.D / MOVF.D
                                     let cc = (instr >> 18) & 7;
                                     let tf = (instr >> 16) & 1;
                                     if self.cp1.get_cc(cc as usize) == (tf != 0) {
                                         self.set_fgr_64(fd_reg, fs_val.to_bits());
                                     }
                                }
                                0x20 => { // CVT.S.D
                                     let res = fs_val as f32;
                                     self.set_fgr_32(fd_reg, res.to_bits());
                                }
                                0x24 => { // CVT.W.D
                                     let mode = self.cp1.rounding_mode();
                                     let res = round_by_mode(fs_val, mode) as i32;
                                     self.set_fgr_32(fd_reg, res as u32);
                                }
                                0x25 => { // CVT.L.D
                                     let mode = self.cp1.rounding_mode();
                                     let res = round_by_mode(fs_val, mode) as i64;
                                     self.set_fgr_64(fd_reg, res as u64);
                                }
                                0x08 | 0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x0E | 0x0F => { // Explicit Rounding D
                                     let rounded = match func {
                                         8 | 12 => round_nearest_even(fs_val), // ROUND.L / ROUND.W
                                         9 | 13 => fs_val.trunc(),            // TRUNC.L / TRUNC.W
                                         10 | 14 => fs_val.ceil(),            // CEIL.L / CEIL.W
                                         11 | 15 => fs_val.floor(),           // FLOOR.L / FLOOR.W
                                         _ => unreachable!(),
                                     };
                                     if func & 4 != 0 { // .W
                                         self.set_fgr_32(fd_reg, rounded as i32 as u32);
                                     } else { // .L
                                         self.set_fgr_64(fd_reg, rounded as i64 as u64);
                                     }
                                }
                                0x30..=0x3F => { // Compare C.cond.D
                                     let cond = func & 0xF;
                                     let (res, signal_invalid) = execute_compare(cond, fs_val, ft_val);
                                     let cc = (instr >> 8) & 7;
                                     self.cp1.set_cc(cc as usize, res);
                                     if signal_invalid {
                                         if self.cp1.update_exception(1) { // 1 = Invalid Operation
                                             return Err(Exception::new(ExceptionType::FloatingPointException, current_pc));
                                         }
                                     }
                                }
                                _ => return Err(Exception::new(ExceptionType::ReservedInstruction, current_pc)),
                            }
                        } else if fmt == 20 { // Word (W)
                            let fs_val = self.get_fgr_32(fs_reg) as i32;
                            match func {
                                0x20 => { // CVT.S.W
                                    let res = fs_val as f32;
                                    self.set_fgr_32(fd_reg, res.to_bits());
                                }
                                0x21 => { // CVT.D.W
                                    let res = fs_val as f64;
                                    self.set_fgr_64(fd_reg, res.to_bits());
                                }
                                _ => return Err(Exception::new(ExceptionType::ReservedInstruction, current_pc)),
                            }
                        } else if fmt == 21 { // Long (L)
                            let fs_val = self.get_fgr_64(fs_reg) as i64;
                            match func {
                                0x20 => { // CVT.S.L
                                    let res = fs_val as f32;
                                    self.set_fgr_32(fd_reg, res.to_bits());
                                }
                                0x21 => { // CVT.D.L
                                    let res = fs_val as f64;
                                    self.set_fgr_64(fd_reg, res.to_bits());
                                }
                                _ => return Err(Exception::new(ExceptionType::ReservedInstruction, current_pc)),
                            }
                        } else {
                            return Err(Exception::new(ExceptionType::ReservedInstruction, current_pc));
                        }
                    }
                }
            }
            0x14 => { // BEQL
                let branch_target = current_pc.wrapping_add(4).wrapping_add(sign_extended_imm << 2);
                if self.regs.read(rs) == self.regs.read(rt) {
                    self.in_delay_slot = true;
                    self.delay_slot_pc = branch_target;
                } else {
                    self.nullify_delay_slot = true;
                }
            }
            0x15 => { // BNEL
                let branch_target = current_pc.wrapping_add(4).wrapping_add(sign_extended_imm << 2);
                if self.regs.read(rs) != self.regs.read(rt) {
                    self.in_delay_slot = true;
                    self.delay_slot_pc = branch_target;
                } else {
                    self.nullify_delay_slot = true;
                }
            }
            0x16 => { // BLEZL
                let branch_target = current_pc.wrapping_add(4).wrapping_add(sign_extended_imm << 2);
                if (self.regs.read(rs) as i64) <= 0 {
                    self.in_delay_slot = true;
                    self.delay_slot_pc = branch_target;
                } else {
                    self.nullify_delay_slot = true;
                }
            }
            0x17 => { // BGTZL
                let branch_target = current_pc.wrapping_add(4).wrapping_add(sign_extended_imm << 2);
                if (self.regs.read(rs) as i64) > 0 {
                    self.in_delay_slot = true;
                    self.delay_slot_pc = branch_target;
                } else {
                    self.nullify_delay_slot = true;
                }
            }
            0x18 => { // DADDI
                let a = self.regs.read(rs) as i64;
                let b = sign_extended_imm as i64;
                match a.checked_add(b) {
                    Some(res) => self.regs.write(rt, res as u64),
                    None => return Err(Exception::new(ExceptionType::Overflow, current_pc)),
                }
            }
            0x19 => { // DADDIU
                let a = self.regs.read(rs);
                let b = sign_extended_imm;
                self.regs.write(rt, a.wrapping_add(b));
            }
            0x1A => { // LDL
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let byte_offset = (vaddr & 7) as u32;
                let paddr = self.translate_vaddr(vaddr, false)?;
                let aligned_dword = mmu.read_doubleword(paddr & !7);
                let rt_val = self.regs.read(rt);
                let new_val = match byte_offset {
                    0 => aligned_dword,
                    1 => (aligned_dword << 8) | (rt_val & 0xFF),
                    2 => (aligned_dword << 16) | (rt_val & 0xFFFF),
                    3 => (aligned_dword << 24) | (rt_val & 0xFF_FFFF),
                    4 => (aligned_dword << 32) | (rt_val & 0xFF_FFFF_FF),
                    5 => (aligned_dword << 40) | (rt_val & 0xFF_FFFF_FF_FF),
                    6 => (aligned_dword << 48) | (rt_val & 0xFF_FFFF_FF_FF_FF),
                    7 => (aligned_dword << 56) | (rt_val & 0xFF_FFFF_FF_FF_FF_FF),
                    _ => unreachable!(),
                };
                self.regs.write(rt, new_val);
            }
            0x1B => { // LDR
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let byte_offset = (vaddr & 7) as u32;
                let paddr = self.translate_vaddr(vaddr, false)?;
                let aligned_dword = mmu.read_doubleword(paddr & !7);
                let rt_val = self.regs.read(rt);
                let new_val = match byte_offset {
                    0 => (aligned_dword >> 56) | (rt_val & 0xFFFF_FFFF_FFFF_FF00),
                    1 => (aligned_dword >> 48) | (rt_val & 0xFFFF_FFFF_FFFF_0000),
                    2 => (aligned_dword >> 40) | (rt_val & 0xFFFF_FFFF_FF00_0000),
                    3 => (aligned_dword >> 32) | (rt_val & 0xFFFF_FFFF_0000_0000),
                    4 => (aligned_dword >> 24) | (rt_val & 0xFFFF_FF00_0000_0000),
                    5 => (aligned_dword >> 16) | (rt_val & 0xFFFF_0000_0000_0000),
                    6 => (aligned_dword >> 8) | (rt_val & 0xFF00_0000_0000_0000),
                    7 => aligned_dword,
                    _ => unreachable!(),
                };
                self.regs.write(rt, new_val);
            }
            0x20 => { // LB
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let byte = self.read_bus_byte(vaddr, mmu)?;
                self.regs.write(rt, byte as i8 as i64 as u64);
            }
            0x21 => { // LH
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let hw = self.read_bus_halfword(vaddr, mmu)?;
                self.regs.write(rt, hw as i16 as i64 as u64);
            }
            0x22 => { // LWL
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let byte_offset = (vaddr & 3) as u32;
                let paddr = self.translate_vaddr(vaddr, false)?;
                let aligned_word = mmu.read_word(paddr & !3) as u64;
                let rt_val = self.regs.read(rt);
                let new_val_32 = match byte_offset {
                    0 => aligned_word,
                    1 => (aligned_word << 8) | (rt_val & 0xFF),
                    2 => (aligned_word << 16) | (rt_val & 0xFFFF),
                    3 => (aligned_word << 24) | (rt_val & 0xFF_FFFF),
                    _ => unreachable!(),
                };
                self.regs.write(rt, (new_val_32 as u32) as i32 as i64 as u64);
            }
            0x23 => { // LW
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let w = self.read_bus_word(vaddr, mmu)?;
                self.regs.write(rt, w as i32 as i64 as u64);
            }
            0x24 => { // LBU
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let byte = self.read_bus_byte(vaddr, mmu)?;
                self.regs.write(rt, byte as u64);
            }
            0x25 => { // LHU
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let hw = self.read_bus_halfword(vaddr, mmu)?;
                self.regs.write(rt, hw as u64);
            }
            0x26 => { // LWR
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let byte_offset = (vaddr & 3) as u32;
                let paddr = self.translate_vaddr(vaddr, false)?;
                let aligned_word = mmu.read_word(paddr & !3) as u64;
                let rt_val = self.regs.read(rt);
                let new_val_32 = match byte_offset {
                    0 => (aligned_word >> 24) | (rt_val & 0xFFFF_FF00),
                    1 => (aligned_word >> 16) | (rt_val & 0xFFFF_0000),
                    2 => (aligned_word >> 8) | (rt_val & 0xFF00_0000),
                    3 => aligned_word,
                    _ => unreachable!(),
                };
                self.regs.write(rt, (new_val_32 as u32) as i32 as i64 as u64);
            }
            0x27 => { // LWU
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let w = self.read_bus_word(vaddr, mmu)?;
                self.regs.write(rt, w as u64);
            }
            0x28 => { // SB
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                self.write_bus_byte(vaddr, self.regs.read(rt) as u8, mmu)?;
            }
            0x29 => { // SH
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                self.write_bus_halfword(vaddr, self.regs.read(rt) as u16, mmu)?;
            }
            0x2A => { // SWL
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let byte_offset = (vaddr & 3) as u32;
                let paddr = self.translate_vaddr(vaddr, true)?;
                let target_addr = paddr & !3;
                let aligned_word = mmu.read_word(target_addr);
                let rt_val = self.regs.read(rt);
                let new_word = match byte_offset {
                    0 => rt_val as u32,
                    1 => (aligned_word & 0xFF00_0000) | ((rt_val >> 8) as u32 & 0x00FF_FFFF),
                    2 => (aligned_word & 0xFFFF_0000) | ((rt_val >> 16) as u32 & 0x0000_FFFF),
                    3 => (aligned_word & 0xFFFF_FF00) | ((rt_val >> 24) as u32 & 0x0000_00FF),
                    _ => unreachable!(),
                };
                mmu.write_word(target_addr, new_word);
            }
            0x2B => { // SW
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                self.write_bus_word(vaddr, self.regs.read(rt) as u32, mmu)?;
            }
            0x2C => { // SDL
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let byte_offset = (vaddr & 7) as u32;
                let paddr = self.translate_vaddr(vaddr, true)?;
                let target_addr = paddr & !7;
                let aligned_dword = mmu.read_doubleword(target_addr);
                let rt_val = self.regs.read(rt);
                let new_dword = match byte_offset {
                    0 => rt_val,
                    1 => (aligned_dword & 0xFF00_0000_0000_0000) | ((rt_val >> 8) & 0x00FF_FFFF_FFFF_FFFF),
                    2 => (aligned_dword & 0xFFFF_0000_0000_0000) | ((rt_val >> 16) & 0x0000_FFFF_FFFF_FFFF),
                    3 => (aligned_dword & 0xFFFF_FF00_0000_0000) | ((rt_val >> 24) & 0x0000_00FF_FFFF_FFFF),
                    4 => (aligned_dword & 0xFFFF_FFFF_0000_0000) | ((rt_val >> 32) & 0x0000_0000_FFFF_FFFF),
                    5 => (aligned_dword & 0xFFFF_FFFF_FF00_0000) | ((rt_val >> 40) & 0x0000_0000_00FF_FFFF),
                    6 => (aligned_dword & 0xFFFF_FFFF_FFFF_0000) | ((rt_val >> 48) & 0x0000_0000_0000_FFFF),
                    7 => (aligned_dword & 0xFFFF_FFFF_FFFF_FF00) | ((rt_val >> 56) & 0x0000_0000_0000_00FF),
                    _ => unreachable!(),
                };
                mmu.write_doubleword(target_addr, new_dword);
            }
            0x2D => { // SDR
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let byte_offset = (vaddr & 7) as u32;
                let paddr = self.translate_vaddr(vaddr, true)?;
                let target_addr = paddr & !7;
                let aligned_dword = mmu.read_doubleword(target_addr);
                let rt_val = self.regs.read(rt);
                let new_dword = match byte_offset {
                    0 => (aligned_dword & 0x00FF_FFFF_FFFF_FFFF) | ((rt_val << 56) & 0xFF00_0000_0000_0000),
                    1 => (aligned_dword & 0x0000_FFFF_FFFF_FFFF) | ((rt_val << 48) & 0xFFFF_0000_0000_0000),
                    2 => (aligned_dword & 0x0000_00FF_FFFF_FFFF) | ((rt_val << 40) & 0xFFFF_0000_0000_0000),
                    3 => (aligned_dword & 0x0000_0000_FFFF_FFFF) | ((rt_val << 32) & 0xFFFF_FFFF_0000_0000),
                    4 => (aligned_dword & 0x0000_0000_00FF_FFFF) | ((rt_val << 24) & 0xFFFF_FFFF_FF00_0000),
                    5 => (aligned_dword & 0x0000_0000_0000_FFFF) | ((rt_val << 16) & 0xFFFF_FFFF_FFFF_0000),
                    6 => (aligned_dword & 0x0000_0000_0000_00FF) | ((rt_val << 8) & 0xFFFF_FFFF_FFFF_FF00),
                    7 => rt_val,
                    _ => unreachable!(),
                };
                mmu.write_doubleword(target_addr, new_dword);
            }
            0x2E => { // SWR
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let byte_offset = (vaddr & 3) as u32;
                let paddr = self.translate_vaddr(vaddr, true)?;
                let target_addr = paddr & !3;
                let aligned_word = mmu.read_word(target_addr);
                let rt_val = self.regs.read(rt);
                let new_word = match byte_offset {
                    0 => (aligned_word & 0x00FF_FFFF) | ((rt_val << 24) as u32 & 0xFF00_0000),
                    1 => (aligned_word & 0x0000_FFFF) | ((rt_val << 16) as u32 & 0xFFFF_0000),
                    2 => (aligned_word & 0x0000_00FF) | ((rt_val << 8) as u32 & 0xFFFF_FF00),
                    3 => rt_val as u32,
                    _ => unreachable!(),
                };
                mmu.write_word(target_addr, new_word);
            }
            0x31 => { // LWC1
                // Check Status.CU1
                if (self.cp0.status & (1 << 29)) == 0 {
                    self.cp0.cause = (self.cp0.cause & !(3 << 28)) | (1 << 28);
                    return Err(Exception::new(ExceptionType::CoprocessorUnusable, current_pc));
                }
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let val = self.read_bus_word(vaddr, mmu)?;
                self.set_fgr_32(rt, val);
            }
            0x35 => { // LDC1
                // Check Status.CU1
                if (self.cp0.status & (1 << 29)) == 0 {
                    self.cp0.cause = (self.cp0.cause & !(3 << 28)) | (1 << 28);
                    return Err(Exception::new(ExceptionType::CoprocessorUnusable, current_pc));
                }
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let val = self.read_bus_doubleword(vaddr, mmu)?;
                self.set_fgr_64(rt, val);
            }
            0x37 => { // LD
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let dw = self.read_bus_doubleword(vaddr, mmu)?;
                self.regs.write(rt, dw);
            }
            0x39 => { // SWC1
                // Check Status.CU1
                if (self.cp0.status & (1 << 29)) == 0 {
                    self.cp0.cause = (self.cp0.cause & !(3 << 28)) | (1 << 28);
                    return Err(Exception::new(ExceptionType::CoprocessorUnusable, current_pc));
                }
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let val = self.get_fgr_32(rt);
                self.write_bus_word(vaddr, val, mmu)?;
            }
            0x3D => { // SDC1
                // Check Status.CU1
                if (self.cp0.status & (1 << 29)) == 0 {
                    self.cp0.cause = (self.cp0.cause & !(3 << 28)) | (1 << 28);
                    return Err(Exception::new(ExceptionType::CoprocessorUnusable, current_pc));
                }
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                let val = self.get_fgr_64(rt);
                self.write_bus_doubleword(vaddr, val, mmu)?;
            }
            0x3F => { // SD
                let vaddr = self.regs.read(rs).wrapping_add(sign_extended_imm);
                self.write_bus_doubleword(vaddr, self.regs.read(rt), mmu)?;
            }
            _ => return Err(Exception::new(ExceptionType::ReservedInstruction, current_pc)),
        }

        Ok(())
    }
}

fn round_nearest_even(val: f64) -> f64 {
    let r = val.round();
    let diff = r - val;
    if diff.abs() == 0.5 {
        if r % 2.0 != 0.0 {
            r - diff.signum()
        } else {
            r
        }
    } else {
        r
    }
}

fn round_by_mode(val: f64, mode: RoundingMode) -> f64 {
    let _ = val;
    let _ = mode;
    match mode {
        RoundingMode::Nearest => round_nearest_even(val),
        RoundingMode::Zero => val.trunc(),
        RoundingMode::PosInf => val.ceil(),
        RoundingMode::NegInf => val.floor(),
    }
}

pub fn execute_compare(cond: u8, v1: f64, v2: f64) -> (bool, bool) {
    let nan1 = v1.is_nan();
    let nan2 = v2.is_nan();
    let unordered = nan1 || nan2;
    
    let (result, signal_invalid) = match cond {
        0 => (false, false),                  // F
        1 => (unordered, false),               // UN
        2 => (v1 == v2, false),                // EQ
        3 => (v1 == v2 || unordered, false),   // UEQ
        4 => (v1 < v2, false),                 // OLT
        5 => (v1 < v2 || unordered, false),    // ULT
        6 => (v1 <= v2, false),                // OLE
        7 => (v1 <= v2 || unordered, false),   // ULE
        8 => (false, true),                    // SF
        9 => (unordered, true),                // NGLE
        10 => (v1 == v2, true),                // SEQ
        11 => (v1 == v2 || unordered, true),   // NGL
        12 => (v1 < v2, true),                 // LT
        13 => (v1 < v2 || unordered, true),    // NGE
        14 => (v1 <= v2, true),                // LE
        15 => (v1 <= v2 || unordered, true),   // NGT
        _ => (false, false),
    };
    (result, signal_invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::n64::mmu::N64Mmu;

    #[test]
    fn test_register_r0() {
        let mut regs = CpuRegisters::new();
        regs.write(0, 0xDEADBEEF);
        assert_eq!(regs.read(0), 0);
    }

    #[test]
    fn test_sign_extension_rules() {
        let mut cpu = Cpu::new();
        cpu.regs.write(1, 0xFFFFFFFF_80000000);
        cpu.regs.write(2, 0x00000000_00000001);
        
        // Run ADDU 3, 1, 2 => 0x80000001 sign-extended
        let instr = (0x00 << 26) | (1 << 21) | (2 << 16) | (3 << 11) | (0 << 6) | 0x21;
        let mut mmu = N64Mmu::new(vec![]);
        cpu.execute_instruction(instr, 0, &mut mmu).unwrap();
        
        assert_eq!(cpu.regs.read(3), 0xFFFFFFFF_80000001);
    }

    #[test]
    fn test_rounding_nearest_even() {
        assert_eq!(round_nearest_even(1.5), 2.0);
        assert_eq!(round_nearest_even(2.5), 2.0);
        assert_eq!(round_nearest_even(3.5), 4.0);
        assert_eq!(round_nearest_even(-1.5), -2.0);
        assert_eq!(round_nearest_even(-2.5), -2.0);
    }

    #[test]
    fn test_fgr_organization() {
        let mut cpu = Cpu::new();
        // FR = 0 (32-bit mode)
        cpu.cp0.status = 0; 
        cpu.set_fgr_64(0, 0x11223344_55667788);
        assert_eq!(cpu.get_fgr_32(0), 0x55667788);
        assert_eq!(cpu.get_fgr_32(1), 0x11223344);
        assert_eq!(cpu.get_fgr_64(0), 0x11223344_55667788);

        // FR = 1 (64-bit mode)
        cpu.cp0.status = 1 << 26;
        cpu.set_fgr_64(0, 0x11223344_55667788);
        cpu.set_fgr_64(1, 0xAAAABBBB_CCCCDDDD);
        assert_eq!(cpu.get_fgr_64(0), 0x11223344_55667788);
        assert_eq!(cpu.get_fgr_64(1), 0xAAAABBBB_CCCCDDDD);
    }

    #[test]
    fn test_tlb_page_size_formula() {
        let mut cpu = Cpu::new();
        // Set up a 16KB page mapping (PageMask = 0x6000)
        cpu.cp0.entry_hi = (0x10000 << 13) | 1;
        let entry = TlbEntry {
            page_mask: 0x6000,
            entry_hi: (0x10000 << 13) | 1,
            entry_lo0: (0x01000 << 6) | 0x6, // PFN = 0x1000, Valid=1, Dirty=1, Global=0
            entry_lo1: (0x02000 << 6) | 0x6, // PFN = 0x2000, Valid=1, Dirty=1, Global=0
        };
        cpu.tlb[0] = entry;

        // Translate virtual address 0x20000 (even page)
        let paddr_even = cpu.translate_tlb(0x20000, false).unwrap();
        assert_eq!(paddr_even, 0x01000000);

        // Translate virtual address 0x24000 (odd page)
        let paddr_odd = cpu.translate_tlb(0x24000, false).unwrap();
        assert_eq!(paddr_odd, 0x02000000);
    }

    #[test]
    fn test_unaligned_loads_stores() {
        let mut cpu = Cpu::new();
        cpu.cp0.status = 0; // Kernel mode
        let mut mmu = N64Mmu::new(vec![]);
        
        // Write pattern to memory
        mmu.write_word(0, 0x01234567);
        mmu.write_word(4, 0x89ABCDEF);

        // LWL to register 1 (rt) at unaligned address 0xA0000001
        let instr_lwl = (0x22 << 26) | (0 << 21) | (1 << 16) | 1;
        cpu.execute_instruction(instr_lwl, 0, &mut mmu).unwrap();
        assert_eq!(cpu.regs.read(1), 0x00000000_23456700);
    }

    #[test]
    fn test_hle_boot() {
        let mut rom = vec![0u8; 0x2000];
        rom[0..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        rom[8..12].copy_from_slice(&[0x80, 0x05, 0x43, 0x21]); // Entry Point PC
        rom[62] = b'E'; // NTSC country code

        for i in 0x40..0x1000 {
            rom[i] = (i & 0xFF) as u8;
        }

        rom[0x1000] = 0xAA;
        rom[0x1001] = 0xBB;
        rom[0x1002] = 0xCC;

        let mut mmu = N64Mmu::new(rom);
        let mut cpu = Cpu::new();

        cpu.hle_boot(&mut mmu);

        // Verify PC
        assert_eq!(cpu.regs.pc, 0xFFFFFFFF_8005_4321);

        // Verify registers
        assert_eq!(cpu.regs.read(20), 1); // $s4 = 1
        assert_eq!(cpu.regs.read(29), 0xFFFFFFFF_A400_1FF0); // $sp
        assert_eq!(cpu.regs.read(31), 0xFFFFFFFF_A400_1550); // $ra

        // Verify CP0
        assert_eq!(cpu.cp0.status, 0x3400_0000);
        assert_eq!(cpu.cp0.config, 0x7006_EC43);

        // Verify RDRAM copy
        assert_eq!(mmu.rdram.data[0x400], 0xAA);
        assert_eq!(mmu.rdram.data[0x401], 0xBB);
        assert_eq!(mmu.rdram.data[0x402], 0xCC);

        // Verify SP DMEM copy
        for i in 0..4032 {
            assert_eq!(mmu.sp_dmem[i], ((0x40 + i) & 0xFF) as u8);
        }

        // Verify PIF RAM country code and control byte
        assert_eq!(mmu.pif_ram[0x24], 0x7F);
        assert_eq!(mmu.pif_ram[0x25], 0x7F);
        assert_eq!(mmu.pif_ram[63], 0x80);
    }
}
