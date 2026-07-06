use crate::emulator::bgr555;

#[derive(Clone, Copy)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    pub fn lowpass(fs: f64, fc: f64, q: f64) -> Self {
        let w0 = std::f64::consts::TAU * fc / fs;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - cos) / 2.0 / a0,
            b1: (1.0 - cos) / a0,
            b2: (1.0 - cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

pub struct LinearResampler {
    pub ratio: f64,
    pub phase: f64,
    pub last_left: f64,
    pub last_right: f64,
    pub current_left: f64,
    pub current_right: f64,
    pub sample_count: usize,
    dc_l: crate::resampler::DcBlocker,
    dc_r: crate::resampler::DcBlocker,
    lp_l: Biquad,
    lp_r: Biquad,
}

impl LinearResampler {
    pub fn new() -> Self {
        Self {
            ratio: 1.0,
            phase: 0.0,
            last_left: 0.0,
            last_right: 0.0,
            current_left: 0.0,
            current_right: 0.0,
            sample_count: 0,
            dc_l: crate::resampler::DcBlocker::new(),
            dc_r: crate::resampler::DcBlocker::new(),
            lp_l: Biquad::lowpass(44_100.0, 15_000.0, std::f64::consts::FRAC_1_SQRT_2),
            lp_r: Biquad::lowpass(44_100.0, 15_000.0, std::f64::consts::FRAC_1_SQRT_2),
        }
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.last_left = 0.0;
        self.last_right = 0.0;
        self.current_left = 0.0;
        self.current_right = 0.0;
        self.sample_count = 0;
        self.dc_l.reset();
        self.dc_r.reset();
        self.lp_l.reset();
        self.lp_r.reset();
    }

    pub fn update_rates(&mut self, dac_rate: f64) {
        let rate = if dac_rate > 0.0 { dac_rate } else { 44100.0 };
        self.ratio = 44_100.0 / rate;
    }

    pub fn push_sample(&mut self, in_l: f64, in_r: f64, audio_buffer: &mut [i16], audio_offset: usize) {
        self.last_left = self.current_left;
        self.last_right = self.current_right;
        self.current_left = in_l;
        self.current_right = in_r;

        let step = 1.0 / self.ratio;

        while self.phase < 1.0 {
            let out_l = self.last_left * (1.0 - self.phase) + self.current_left * self.phase;
            let out_r = self.last_right * (1.0 - self.phase) + self.current_right * self.phase;

            let filtered_l = self.lp_l.process(self.dc_l.process(out_l));
            let filtered_r = self.lp_r.process(self.dc_r.process(out_r));

            let final_l = (filtered_l * 30000.0).clamp(-32768.0, 32767.0) as i16;
            let final_r = (filtered_r * 30000.0).clamp(-32768.0, 32767.0) as i16;

            let buffer_idx = audio_offset + self.sample_count * 2;
            if buffer_idx + 1 < audio_buffer.len() {
                audio_buffer[buffer_idx] = final_l;
                audio_buffer[buffer_idx + 1] = final_r;
                self.sample_count += 1;
            } else {
                break;
            }

            self.phase += step;
        }

        self.phase -= 1.0;
    }
}

fn resolve_segmented(addr: u32, segments: &[u32; 16]) -> u32 {
    let segment = ((addr >> 24) & 0xF) as usize;
    let offset = addr & 0x00FF_FFFF;
    (segments[segment] & 0x00FF_FFFF) + offset
}

pub struct N64Mmu {
    pub rdram: crate::n64::rdram::Rdram,
    pub sp_dmem: [u8; 0x1000],   // 4 KB RSP Data Memory
    pub sp_imem: [u8; 0x1000],   // 4 KB RSP Instruction Memory
    
    // Memory-mapped Register Regions
    pub rdram_regs: [u8; 0x20],  // 0x03F0_0000 - 0x03F0_001F
    pub sp_regs: [u8; 0x20],     // 0x0404_0000 - 0x0404_001F
    pub sp_pc_regs: [u8; 0x08],  // 0x0408_0000 - 0x0408_0007
    pub dpc_regs: [u8; 0x20],    // 0x0410_0000 - 0x0410_001F
    pub dps_regs: [u8; 0x10],    // 0x0420_0000 - 0x0420_000F
    pub mi_regs: [u8; 0x10],     // 0x0430_0000 - 0x0430_000F
    pub vi_regs: [u8; 0x40],     // 0x0440_0000 - 0x0440_003F
    pub ai_regs: [u8; 0x18],     // 0x0450_0000 - 0x0450_0017
    pub pi_regs: [u8; 0x34],     // 0x0460_0000 - 0x0460_0033
    pub si_regs: [u8; 0x1C],     // 0x0470_0000 - 0x0470_001B
    pub ri_regs: [u8; 0x20],     // 0x0480_0000 - 0x0480_001F
    
    // Other Physical Memory Regions
    pub pif_rom: [u8; 0x7C0],    // 1984 bytes at 0x1FC0_0000
    pub pif_ram: [u8; 0x40],     // 64 bytes at 0x1FC0_07C0
    pub cart_rom: Vec<u8>,       // Cartridge ROM (variable size)

    // VI Line timing and interrupt tracking
    pub vi_cycles: u32,
    pub vi_intr_triggered: bool,

    // Joybus controller input state
    pub buttons: crate::ffi::ButtonState,

    // AI DMA queue state
    pub ai_dma_address: [u32; 2],
    pub ai_dma_length: [u32; 2],
    pub ai_dma_count: usize,
    pub ai_dma_len_remaining: u32,
    pub ai_dma_byte_accumulator: f64,
    pub ai_programmed_addr: u32,
    pub ai_control: u32,
    pub ai_dacrate: u32,
    pub ai_bitrate: u32,
    pub ai_resampler: LinearResampler,

    // RSP and RDP subsystems
    pub rsp: crate::n64::rsp::Rsp,
    pub rdp: crate::n64::rdp::Rdp,
    pub n64_rom_loaded: bool,
}

impl N64Mmu {
    pub fn new(cart_rom: Vec<u8>) -> Self {
        let n64_rom_loaded = !cart_rom.is_empty();
        Self {
            rdram: crate::n64::rdram::Rdram::new(),
            sp_dmem: [0; 0x1000],
            sp_imem: [0; 0x1000],
            rdram_regs: [0; 0x20],
            sp_regs: [0; 0x20],
            sp_pc_regs: [0; 0x08],
            dpc_regs: [0; 0x20],
            dps_regs: [0; 0x10],
            mi_regs: [0; 0x10],
            vi_regs: [0; 0x40],
            ai_regs: [0; 0x18],
            pi_regs: [0; 0x34],
            si_regs: [0; 0x1C],
            ri_regs: [0; 0x20],
            pif_rom: [0; 0x7C0],
            pif_ram: [0; 0x40],
            cart_rom,
            vi_cycles: 0,
            vi_intr_triggered: false,
            buttons: crate::ffi::ButtonState {
                up: false, down: false, left: false, right: false,
                a: false, b: false, start: false, select: false,
                l: false, r: false,
                stick_x: 0, stick_y: 0, z: false,
                c_up: false, c_down: false, c_left: false, c_right: false,
            },
            ai_dma_address: [0; 2],
            ai_dma_length: [0; 2],
            ai_dma_count: 0,
            ai_dma_len_remaining: 0,
            ai_dma_byte_accumulator: 0.0,
            ai_programmed_addr: 0,
            ai_control: 0,
            ai_dacrate: 0,
            ai_bitrate: 0,
            ai_resampler: LinearResampler::new(),
            rsp: crate::n64::rsp::Rsp::new(),
            rdp: crate::n64::rdp::Rdp::new(),
            n64_rom_loaded,
        }
    }

    pub fn set_mi_interrupt(&mut self, source_bit: u8) {
        let mut intr = self.read_mi_reg(2);
        intr |= 1 << source_bit;
        self.write_mi_reg(2, intr);
    }

    pub fn clear_mi_interrupt(&mut self, source_bit: u8) {
        let mut intr = self.read_mi_reg(2);
        intr &= !(1 << source_bit);
        self.write_mi_reg(2, intr);
    }

    pub fn is_rcp_interrupt_pending(&self) -> bool {
        let intr = self.read_mi_reg(2);
        let mask = self.read_mi_reg(3);
        (intr & mask) != 0
    }

    fn read_mi_reg(&self, reg_idx: usize) -> u32 {
        let offset = reg_idx * 4;
        u32::from_be_bytes([self.mi_regs[offset], self.mi_regs[offset+1], self.mi_regs[offset+2], self.mi_regs[offset+3]])
    }

    fn write_mi_reg(&mut self, reg_idx: usize, val: u32) {
        let offset = reg_idx * 4;
        self.mi_regs[offset..offset+4].copy_from_slice(&val.to_be_bytes());
    }

    fn read_vi_reg(&self, reg_idx: usize) -> u32 {
        let offset = reg_idx * 4;
        u32::from_be_bytes([self.vi_regs[offset], self.vi_regs[offset+1], self.vi_regs[offset+2], self.vi_regs[offset+3]])
    }

    fn write_vi_reg(&mut self, reg_idx: usize, val: u32) {
        let offset = reg_idx * 4;
        self.vi_regs[offset..offset+4].copy_from_slice(&val.to_be_bytes());
    }

    fn read_pi_reg(&self, reg_idx: usize) -> u32 {
        let offset = reg_idx * 4;
        u32::from_be_bytes([self.pi_regs[offset], self.pi_regs[offset+1], self.pi_regs[offset+2], self.pi_regs[offset+3]])
    }

    fn write_pi_reg(&mut self, reg_idx: usize, val: u32) {
        let offset = reg_idx * 4;
        self.pi_regs[offset..offset+4].copy_from_slice(&val.to_be_bytes());
    }

    fn read_si_reg(&self, reg_idx: usize) -> u32 {
        let offset = reg_idx * 4;
        u32::from_be_bytes([self.si_regs[offset], self.si_regs[offset+1], self.si_regs[offset+2], self.si_regs[offset+3]])
    }

    fn write_si_reg(&mut self, reg_idx: usize, val: u32) {
        let offset = reg_idx * 4;
        self.si_regs[offset..offset+4].copy_from_slice(&val.to_be_bytes());
    }

    fn execute_pi_dma(&mut self, is_read: bool) {
        let dram_start = self.read_pi_reg(0) & 0x007F_FFFF;
        let cart_start = self.read_pi_reg(1);
        let len_val = if is_read { self.read_pi_reg(2) } else { self.read_pi_reg(3) };
        let length = (len_val & 0x00FF_FFFF) + 1;

        // Set PI DMA Busy status (Bit 0)
        let mut status = self.read_pi_reg(4);
        status |= 1;
        self.write_pi_reg(4, status);

        // Perform the data transfer
        if is_read {
            for i in 0..length {
                let byte = self.read_byte(cart_start + i);
                self.write_byte(dram_start + i, byte);
            }
        } else {
            for i in 0..length {
                let byte = self.read_byte(dram_start + i);
                self.write_byte(cart_start + i, byte);
            }
        }

        // Clear PI DMA Busy status, set PI Interrupt Pending status (Bit 3)
        status &= !1;
        status |= 8;
        self.write_pi_reg(4, status);

        // Assert PI Interrupt in MIPS Interface (MI bit 4)
        self.set_mi_interrupt(4);
    }

    fn execute_si_dma_read(&mut self) {
        let dram_addr = self.read_si_reg(0) & 0x007F_FFFF;

        self.set_si_status_busy(true);

        for i in 0..64 {
            let byte = self.pif_ram[i];
            self.write_byte(dram_addr + i as u32, byte);
        }

        self.set_si_status_busy(false);
        self.set_si_status_interrupt();
        self.set_mi_interrupt(1); // SI Interrupt is bit 1
    }

    fn execute_si_dma_write(&mut self) {
        let dram_addr = self.read_si_reg(0) & 0x007F_FFFF;

        self.set_si_status_busy(true);

        for i in 0..64 {
            let byte = self.read_byte(dram_addr + i as u32);
            self.pif_ram[i] = byte;
        }

        if self.pif_ram[63] == 0x01 {
            let cic_type = crate::rom::detect_cic_type(&self.cart_rom);
            if cic_type == crate::rom::CicType::Cic6105 {
                let key: [u8; 16] = [
                    0xDF, 0x5D, 0xAC, 0x3F, 0x1E, 0x0F, 0x30, 0x14,
                    0x1B, 0x3B, 0x2D, 0x1C, 0x2F, 0x27, 0x12, 0x38,
                ];
                let mut response = [0u8; 30];
                for i in 0..30 {
                    let challenge_byte = self.pif_ram[i];
                    let key_byte = key[i % 16];
                    response[i] = (challenge_byte ^ key_byte).rotate_left(3) ^ 0x5A;
                }
                self.pif_ram[30..60].copy_from_slice(&response);
                self.pif_ram[63] = 0x00;
            } else {
                self.process_joybus_commands();
                self.pif_ram[63] = 0x00;
            }
        }

        self.set_si_status_busy(false);
        self.set_si_status_interrupt();
        self.set_mi_interrupt(1); // SI Interrupt is bit 1
    }

    fn set_si_status_busy(&mut self, busy: bool) {
        let mut status = self.read_si_reg(6);
        if busy {
            status |= 1 << 0;
        } else {
            status &= !(1 << 0);
        }
        self.write_si_reg(6, status);
    }

    fn set_si_status_interrupt(&mut self) {
        let mut status = self.read_si_reg(6);
        status |= 1 << 12;
        self.write_si_reg(6, status);
    }

    fn process_joybus_commands(&mut self) {
        let mut idx = 0;
        let mut channel = 0;

        while idx < 63 {
            if idx + 2 > 64 {
                break;
            }
            let tx_len = self.pif_ram[idx];
            if tx_len == 0xFE {
                break;
            }
            if tx_len == 0xFF {
                idx += 1;
                continue;
            }
            if tx_len == 0xFD {
                channel = 0;
                idx += 1;
                continue;
            }
            
            let rx_len_raw = self.pif_ram[idx + 1];
            let rx_len = rx_len_raw & 0x3F;
            
            if tx_len == 0 && rx_len == 0 {
                idx += 2;
                continue;
            }

            let resp_start = idx + 2 + tx_len as usize;
            if resp_start + rx_len as usize > 64 {
                break;
            }

            if idx + 2 >= 64 {
                break;
            }
            let cmd = self.pif_ram[idx + 2];

            match cmd {
                0x00 | 0xFF => { // Info or Reset
                    if channel == 0 {
                        self.pif_ram[idx + 1] &= 0x3F; // Clear error bits
                        if rx_len >= 1 && resp_start < 64 { self.pif_ram[resp_start] = 0x05; }
                        if rx_len >= 2 && resp_start + 1 < 64 { self.pif_ram[resp_start + 1] = 0x00; }
                        if rx_len >= 3 && resp_start + 2 < 64 { self.pif_ram[resp_start + 2] = 0x02; }
                    } else if channel < 4 {
                        self.pif_ram[idx + 1] |= 0x80;
                        if rx_len >= 1 && resp_start < 64 { self.pif_ram[resp_start] = 0xFF; }
                        if rx_len >= 2 && resp_start + 1 < 64 { self.pif_ram[resp_start + 1] = 0xFF; }
                        if rx_len >= 3 && resp_start + 2 < 64 { self.pif_ram[resp_start + 2] = 0xFF; }
                    }
                }
                0x01 => { // Read Button State
                    if channel == 0 {
                        self.pif_ram[idx + 1] &= 0x3F; // Clear error bits
                        
                        let mut b0 = 0u8;
                        if self.buttons.a { b0 |= 1 << 7; }
                        if self.buttons.b { b0 |= 1 << 6; }
                        if self.buttons.select { b0 |= 1 << 5; } // Map Z to select
                        if self.buttons.start { b0 |= 1 << 4; }
                        if self.buttons.up { b0 |= 1 << 3; }
                        if self.buttons.down { b0 |= 1 << 2; }
                        if self.buttons.left { b0 |= 1 << 1; }
                        if self.buttons.right { b0 |= 1 << 0; }

                        let mut b1 = 0u8;
                        if self.buttons.l { b1 |= 1 << 5; }
                        if self.buttons.r { b1 |= 1 << 4; }

                        let b2 = if self.buttons.left { 0xB0 } else if self.buttons.right { 0x50 } else { 0x00 }; // Analog X
                        let b3 = if self.buttons.down { 0xB0 } else if self.buttons.up { 0x50 } else { 0x00 }; // Analog Y

                        if rx_len >= 1 && resp_start < 64 { self.pif_ram[resp_start] = b0; }
                        if rx_len >= 2 && resp_start + 1 < 64 { self.pif_ram[resp_start + 1] = b1; }
                        if rx_len >= 3 && resp_start + 2 < 64 { self.pif_ram[resp_start + 2] = b2; }
                        if rx_len >= 4 && resp_start + 3 < 64 { self.pif_ram[resp_start + 3] = b3; }
                    } else if channel < 4 {
                        self.pif_ram[idx + 1] |= 0x80;
                        if rx_len >= 1 && resp_start < 64 { self.pif_ram[resp_start] = 0xFF; }
                        if rx_len >= 2 && resp_start + 1 < 64 { self.pif_ram[resp_start + 1] = 0xFF; }
                        if rx_len >= 3 && resp_start + 2 < 64 { self.pif_ram[resp_start + 2] = 0xFF; }
                        if rx_len >= 4 && resp_start + 3 < 64 { self.pif_ram[resp_start + 3] = 0xFF; }
                    }
                }
                0x02 | 0x03 => { // Read/Write Accessory Pak
                    self.pif_ram[idx + 1] |= 0x80; // Timeout
                }
                _ => {}
            }

            channel += 1;
            idx += 2 + tx_len as usize + rx_len as usize;
        }
    }

    pub fn tick_vi(&mut self, cycles: u32) {
        self.vi_cycles += cycles;

        let mut v_sync = self.read_vi_reg(6) & 0x3FF; // VI_V_SYNC_REG offset 0x18
        if v_sync == 0 {
            v_sync = 525;
        }
        
        let cycles_per_half_line = if v_sync == 625 { 3000 } else { 2976 };

        let current_half_line = (self.vi_cycles / cycles_per_half_line) % v_sync;

        // Update VI_V_CURRENT_LINE_REG in MMU
        self.write_vi_reg(4, current_half_line);

        // Check for vertical interrupt triggers
        let v_intr = self.read_vi_reg(3) & 0x3FF; // VI_V_INTR_REG offset 0x0C

        if current_half_line == v_intr && !self.vi_intr_triggered {
            self.set_mi_interrupt(3); // Trigger VI Interrupt in MI (bit 3)
            self.vi_intr_triggered = true;
        }

        // Frame boundary reset
        if self.vi_cycles >= v_sync * cycles_per_half_line {
            self.vi_cycles -= v_sync * cycles_per_half_line;
            self.vi_intr_triggered = false;
        }
    }

    pub fn tick_ai(
        &mut self,
        cycles: u32,
        speed: f32,
        audio_buffer: &mut [i16],
        audio_offset: usize,
    ) {
        if (self.ai_control & 1) == 0 || self.ai_dma_count == 0 {
            return;
        }

        let clock = 48_681_812.0; // NTSC
        let dac_freq = clock / ((self.ai_dacrate & 0x3FFF) + 1) as f64;
        let cpu_freq = 16_853_760.0 * speed as f64;
        
        let bytes_per_cycle = (4.0 * dac_freq) / cpu_freq;
        self.ai_dma_byte_accumulator += cycles as f64 * bytes_per_cycle;

        let mut bytes_to_consume = self.ai_dma_byte_accumulator.floor() as u32;
        self.ai_dma_byte_accumulator -= bytes_to_consume as f64;

        while bytes_to_consume >= 4 && self.ai_dma_count > 0 {
            let addr = self.ai_dma_address[0];
            
            let left_val = self.rdram.read_u16(addr) as i16;
            let right_val = self.rdram.read_u16(addr + 2) as i16;

            self.ai_resampler.push_sample(
                left_val as f64 / 32768.0, 
                right_val as f64 / 32768.0,
                audio_buffer,
                audio_offset,
            );

            self.ai_dma_address[0] = self.ai_dma_address[0].wrapping_add(4);
            self.ai_dma_len_remaining = self.ai_dma_len_remaining.saturating_sub(4);
            bytes_to_consume -= 4;

            if self.ai_dma_len_remaining == 0 {
                self.set_mi_interrupt(2); // AI Interrupt is bit 2

                self.ai_dma_count -= 1;
                if self.ai_dma_count > 0 {
                    self.ai_dma_address[0] = self.ai_dma_address[1];
                    self.ai_dma_length[0] = self.ai_dma_length[1];
                    self.ai_dma_len_remaining = self.ai_dma_length[0];
                }
            }
        }
    }

    pub fn read_byte(&self, addr: u32) -> u8 {
        match addr {
            0x0000_0000..=0x007F_FFFF => {
                self.rdram.read_u8(addr)
            }
            0x03F0_0000..=0x03F0_001F => {
                let offset = (addr - 0x03F0_0000) as usize;
                self.rdram_regs[offset]
            }
            0x0400_0000..=0x0400_0FFF => {
                let offset = (addr - 0x0400_0000) as usize;
                self.sp_dmem[offset]
            }
            0x0400_1000..=0x0400_1FFF => {
                let offset = (addr - 0x0400_1000) as usize;
                self.sp_imem[offset]
            }
            0x0404_0000..=0x0404_001F => {
                let offset = (addr - 0x0404_0000) as usize;
                self.sp_regs[offset]
            }
            0x0408_0000..=0x0408_0007 => {
                let offset = (addr - 0x0408_0000) as usize;
                self.sp_pc_regs[offset]
            }
            0x0410_0000..=0x0410_001F => {
                let offset = (addr - 0x0410_0000) as usize;
                self.dpc_regs[offset]
            }
            0x0420_0000..=0x0420_000F => {
                let offset = (addr - 0x0420_0000) as usize;
                self.dps_regs[offset]
            }
            0x0430_0000..=0x0430_000F => {
                let offset = (addr - 0x0430_0000) as usize;
                self.mi_regs[offset]
            }
            0x0440_0000..=0x0440_003F => {
                let offset = (addr - 0x0440_0000) as usize;
                self.vi_regs[offset]
            }
            0x0450_0000..=0x0450_0017 => {
                let offset = (addr - 0x0450_0000) as usize;
                self.ai_regs[offset]
            }
            0x0460_0000..=0x0460_0033 => {
                let offset = (addr - 0x0460_0000) as usize;
                self.pi_regs[offset]
            }
            0x0470_0000..=0x0470_001B => {
                let offset = (addr - 0x0470_0000) as usize;
                self.si_regs[offset]
            }
            0x0480_0000..=0x0480_001F => {
                let offset = (addr - 0x0480_0000) as usize;
                self.ri_regs[offset]
            }
            0x1000_0000..=0x1FFF_FFFF => {
                let offset = (addr - 0x1000_0000) as usize;
                if offset < self.cart_rom.len() {
                    self.cart_rom[offset]
                } else {
                    0
                }
            }
            0x1FC0_0000..=0x1FC0_07BF => {
                let offset = (addr - 0x1FC0_0000) as usize;
                self.pif_rom[offset]
            }
            0x1FC0_07C0..=0x1FC0_07FF => {
                let offset = (addr - 0x1FC0_07C0) as usize;
                self.pif_ram[offset]
            }
            _ => 0,
        }
    }

    pub fn write_byte(&mut self, addr: u32, val: u8) {
        match addr {
            0x0000_0000..=0x007F_FFFF => {
                self.rdram.write_u8(addr, val);
            }
            0x03F0_0000..=0x03F0_001F => {
                let offset = (addr - 0x03F0_0000) as usize;
                self.rdram_regs[offset] = val;
            }
            0x0400_0000..=0x0400_0FFF => {
                let offset = (addr - 0x0400_0000) as usize;
                self.sp_dmem[offset] = val;
            }
            0x0400_1000..=0x0400_1FFF => {
                let offset = (addr - 0x0400_1000) as usize;
                self.sp_imem[offset] = val;
            }
            0x0404_0000..=0x0404_001F => {
                let offset = (addr - 0x0404_0000) as usize;
                self.sp_regs[offset] = val;
            }
            0x0408_0000..=0x0408_0007 => {
                let offset = (addr - 0x0408_0000) as usize;
                self.sp_pc_regs[offset] = val;
            }
            0x0410_0000..=0x0410_001F => {
                let offset = (addr - 0x0410_0000) as usize;
                self.dpc_regs[offset] = val;
            }
            0x0420_0000..=0x0420_000F => {
                let offset = (addr - 0x0420_0000) as usize;
                self.dps_regs[offset] = val;
            }
            0x0430_0000..=0x0430_000F => {
                let offset = (addr - 0x0430_0000) as usize;
                self.mi_regs[offset] = val;
                if offset % 4 == 3 {
                    let reg_idx = offset / 4;
                    let word = u32::from_be_bytes([self.mi_regs[reg_idx*4], self.mi_regs[reg_idx*4+1], self.mi_regs[reg_idx*4+2], self.mi_regs[reg_idx*4+3]]);
                    self.write_mi_reg_word(reg_idx, word);
                }
            }
            0x0440_0000..=0x0440_003F => {
                let offset = (addr - 0x0440_0000) as usize;
                self.vi_regs[offset] = val;
                if offset % 4 == 3 {
                    let reg_idx = offset / 4;
                    let word = u32::from_be_bytes([self.vi_regs[reg_idx*4], self.vi_regs[reg_idx*4+1], self.vi_regs[reg_idx*4+2], self.vi_regs[reg_idx*4+3]]);
                    self.write_vi_reg_word(reg_idx, word);
                }
            }
            0x0450_0000..=0x0450_0017 => {
                let offset = (addr - 0x0450_0000) as usize;
                self.ai_regs[offset] = val;
                if offset % 4 == 3 {
                    let reg_idx = offset / 4;
                    let word = u32::from_be_bytes([self.ai_regs[reg_idx*4], self.ai_regs[reg_idx*4+1], self.ai_regs[reg_idx*4+2], self.ai_regs[reg_idx*4+3]]);
                    self.write_ai_reg_word(reg_idx, word);
                }
            }
            0x0460_0000..=0x0460_0033 => {
                let offset = (addr - 0x0460_0000) as usize;
                self.pi_regs[offset] = val;
                if offset % 4 == 3 {
                    let reg_idx = offset / 4;
                    let word = u32::from_be_bytes([self.pi_regs[reg_idx*4], self.pi_regs[reg_idx*4+1], self.pi_regs[reg_idx*4+2], self.pi_regs[reg_idx*4+3]]);
                    self.write_pi_reg_word(reg_idx, word);
                }
            }
            0x0470_0000..=0x0470_001B => {
                let offset = (addr - 0x0470_0000) as usize;
                self.si_regs[offset] = val;
                if offset % 4 == 3 {
                    let reg_idx = offset / 4;
                    let word = u32::from_be_bytes([self.si_regs[reg_idx*4], self.si_regs[reg_idx*4+1], self.si_regs[reg_idx*4+2], self.si_regs[reg_idx*4+3]]);
                    self.write_si_reg_word(reg_idx, word);
                }
            }
            0x0480_0000..=0x0480_001F => {
                let offset = (addr - 0x0480_0000) as usize;
                self.ri_regs[offset] = val;
            }
            0x1000_0000..=0x1FFF_FFFF => {}
            0x1FC0_0000..=0x1FC0_07BF => {}
            0x1FC0_07C0..=0x1FC0_07FF => {
                let offset = (addr - 0x1FC0_07C0) as usize;
                self.pif_ram[offset] = val;
            }
            _ => {}
        }
    }

    fn write_mi_reg_word(&mut self, reg_idx: usize, val: u32) {
        match reg_idx {
            0 => { // MI_INIT_MODE_REG
                let mut mode = self.read_mi_reg(0);
                mode = (mode & !0x7F) | (val & 0x7F);
                if (val & (1 << 7)) != 0 { mode &= !(1 << 7); } // Clear init mode
                if (val & (1 << 8)) != 0 { mode |= 1 << 7; }    // Set init mode
                if (val & (1 << 9)) != 0 { mode &= !(1 << 8); } // Clear ebus test mode
                if (val & (1 << 10)) != 0 { mode |= 1 << 8; }   // Set ebus test mode
                if (val & (1 << 11)) != 0 { self.clear_mi_interrupt(5); } // Clear DP interrupt
                if (val & (1 << 12)) != 0 { mode &= !(1 << 9); } // Clear RDRAM reg mode
                if (val & (1 << 13)) != 0 { mode |= 1 << 9; }    // Set RDRAM reg mode
                self.write_mi_reg(0, mode);
            }
            3 => { // MI_INTR_MASK_REG
                let mut mask = self.read_mi_reg(3);
                if (val & (1 << 0)) != 0 { mask &= !(1 << 0); } // Clear SP mask
                if (val & (1 << 1)) != 0 { mask |= 1 << 0; }    // Set SP mask
                if (val & (1 << 2)) != 0 { mask &= !(1 << 1); } // Clear SI mask
                if (val & (1 << 3)) != 0 { mask |= 1 << 1; }    // Set SI mask
                if (val & (1 << 4)) != 0 { mask &= !(1 << 2); } // Clear AI mask
                if (val & (1 << 5)) != 0 { mask |= 1 << 2; }    // Set AI mask
                if (val & (1 << 6)) != 0 { mask &= !(1 << 3); } // Clear VI mask
                if (val & (1 << 7)) != 0 { mask |= 1 << 3; }    // Set VI mask
                if (val & (1 << 8)) != 0 { mask &= !(1 << 4); } // Clear PI mask
                if (val & (1 << 9)) != 0 { mask |= 1 << 4; }    // Set PI mask
                if (val & (1 << 10)) != 0 { mask &= !(1 << 5); } // Clear DP mask
                if (val & (1 << 11)) != 0 { mask |= 1 << 5; }   // Set DP mask
                self.write_mi_reg(3, mask);
            }
            _ => {}
        }
    }

    fn write_vi_reg_word(&mut self, reg_idx: usize, val: u32) {
        let offset = reg_idx * 4;
        if offset + 4 <= self.vi_regs.len() {
            self.vi_regs[offset..offset+4].copy_from_slice(&val.to_be_bytes());
        }
        if reg_idx == 4 { // VI_V_CURRENT_LINE_REG
            self.clear_mi_interrupt(3); // Clear VI interrupt (bit 3)
        }
    }

    fn write_ai_reg_word(&mut self, reg_idx: usize, val: u32) {
        match reg_idx {
            0 => { // AI_DRAM_ADDR_REG
                self.ai_programmed_addr = val & 0x00FF_FFF8;
            }
            1 => { // AI_LEN_REG
                let length = val & 0x3_FFF8;
                if self.ai_dma_count < 2 {
                    self.ai_dma_address[self.ai_dma_count] = self.ai_programmed_addr;
                    self.ai_dma_length[self.ai_dma_count] = length;
                    
                    if self.ai_dma_count == 0 {
                        self.ai_dma_len_remaining = length;
                    }
                    self.ai_dma_count += 1;
                }
            }
            2 => { // AI_CONTROL_REG
                self.ai_control = val & 1;
            }
            3 => { // AI_STATUS_REG
                self.clear_mi_interrupt(2); // Clear AI interrupt (bit 2)
            }
            4 => { // AI_DACRATE_REG
                self.ai_dacrate = val & 0x3FFF;
                let dac_freq = 48_681_812.0 / ((self.ai_dacrate & 0x3FFF) + 1) as f64;
                self.ai_resampler.update_rates(dac_freq);
            }
            5 => { // AI_BITRATE_REG
                self.ai_bitrate = val & 0xF;
            }
            _ => {}
        }
    }

    fn write_pi_reg_word(&mut self, reg_idx: usize, val: u32) {
        match reg_idx {
            0 => self.write_pi_reg(0, val), // PI_DRAM_ADDR_REG
            1 => self.write_pi_reg(1, val), // PI_CART_ADDR_REG
            2 => { // PI_RD_LEN_REG
                self.write_pi_reg(2, val);
                self.execute_pi_dma(true);
            }
            3 => { // PI_WR_LEN_REG
                self.write_pi_reg(3, val);
                self.execute_pi_dma(false);
            }
            4 => { // PI_STATUS_REG
                let mut status = self.read_pi_reg(4);
                if (val & (1 << 0)) != 0 {
                    status &= !1; // Clear DMA busy
                    status &= !8; // Clear interrupt pending
                    self.clear_mi_interrupt(4); // Clear PI interrupt in MI
                }
                if (val & (1 << 1)) != 0 {
                    status &= !8;
                    self.clear_mi_interrupt(4);
                }
                self.write_pi_reg(4, status);
            }
            5..=12 => self.write_pi_reg(reg_idx, val),
            _ => {}
        }
    }

    fn write_si_reg_word(&mut self, reg_idx: usize, val: u32) {
        match reg_idx {
            0 => self.write_si_reg(0, val), // SI_DRAM_ADDR_REG
            1 => { // SI_PIF_ADDR_RD_REG
                self.write_si_reg(1, val);
                self.execute_si_dma_read();
            }
            2 => { // SI_PIF_ADDR_WR_REG
                self.write_si_reg(2, val);
                self.execute_si_dma_write();
            }
            6 => { // SI_STATUS_REG
                let mut status = self.read_si_reg(6);
                status &= !(1 << 12); // Clear SI interrupt pending
                self.write_si_reg(6, status);
                self.clear_mi_interrupt(1); // Clear SI interrupt in MI (bit 1)
            }
            _ => {}
        }
    }

    fn read_sp_reg_word(&self, reg_idx: usize) -> u32 {
        match reg_idx {
            0 => self.rsp.sp_mem_addr,
            1 => self.rsp.sp_dram_addr,
            2 => self.rsp.sp_rd_len,
            3 => self.rsp.sp_wr_len,
            4 => self.rsp.get_status(),
            5 => if self.rsp.dma_full { 1 } else { 0 },
            6 => if self.rsp.dma_busy { 1 } else { 0 },
            7 => {
                let val = self.rsp.semaphore.get();
                self.rsp.semaphore.set(1);
                val
            }
            _ => 0,
        }
    }

    fn write_sp_reg_word(&mut self, reg_idx: usize, val: u32) {
        let offset = reg_idx * 4;
        self.sp_regs[offset..offset+4].copy_from_slice(&val.to_be_bytes());
        match reg_idx {
            0 => {
                self.rsp.sp_mem_addr = val & 0x1FFF;
            }
            1 => {
                self.rsp.sp_dram_addr = val & 0x00FF_FFF8;
            }
            2 => {
                self.rsp.sp_rd_len = val;
                let mut dmem = self.sp_dmem;
                let mut imem = self.sp_imem;
                self.rsp.execute_sp_dma(false, &mut dmem, &mut imem, &mut self.rdram);
                self.sp_dmem = dmem;
                self.sp_imem = imem;
                // Sync updated values back to sp_regs
                self.sp_regs[0..4].copy_from_slice(&self.rsp.sp_mem_addr.to_be_bytes());
                self.sp_regs[4..8].copy_from_slice(&self.rsp.sp_dram_addr.to_be_bytes());
            }
            3 => {
                self.rsp.sp_wr_len = val;
                let mut dmem = self.sp_dmem;
                let mut imem = self.sp_imem;
                self.rsp.execute_sp_dma(true, &mut dmem, &mut imem, &mut self.rdram);
                self.sp_dmem = dmem;
                self.sp_imem = imem;
                // Sync updated values back to sp_regs
                self.sp_regs[0..4].copy_from_slice(&self.rsp.sp_mem_addr.to_be_bytes());
                self.sp_regs[4..8].copy_from_slice(&self.rsp.sp_dram_addr.to_be_bytes());
            }
            4 => {
                if (val & (1 << 0)) != 0 { self.rsp.halted = false; }
                if (val & (1 << 1)) != 0 { self.rsp.halted = true; }
                if (val & (1 << 2)) != 0 { self.rsp.broke = false; }
                if (val & (1 << 3)) != 0 { self.clear_mi_interrupt(0); }
                if (val & (1 << 4)) != 0 { self.set_mi_interrupt(0); }
                if (val & (1 << 5)) != 0 { self.rsp.single_step = false; }
                if (val & (1 << 6)) != 0 { self.rsp.single_step = true; }
                if (val & (1 << 7)) != 0 { self.rsp.intr_on_break = false; }
                if (val & (1 << 8)) != 0 { self.rsp.intr_on_break = true; }
                for i in 0..8 {
                    let clear_bit = 9 + (i * 2);
                    let set_bit = 10 + (i * 2);
                    if (val & (1 << clear_bit)) != 0 { self.rsp.signals[i] = false; }
                    if (val & (1 << set_bit)) != 0 { self.rsp.signals[i] = true; }
                }
                if !self.rsp.halted {
                    self.rsp_hle_execute();
                }
            }
            7 => {
                self.rsp.semaphore.set(0);
            }
            _ => {}
        }
    }

    fn read_sp_pc_reg_word(&self, reg_idx: usize) -> u32 {
        match reg_idx {
            0 => self.rsp.pc as u32,
            _ => 0,
        }
    }

    fn write_sp_pc_reg_word(&mut self, reg_idx: usize, val: u32) {
        match reg_idx {
            0 => self.rsp.pc = (val & 0xFFC) as u16,
            _ => {}
        }
    }

    fn read_dpc_reg_word(&self, reg_idx: usize) -> u32 {
        let offset = reg_idx * 4;
        u32::from_be_bytes([self.dpc_regs[offset], self.dpc_regs[offset+1], self.dpc_regs[offset+2], self.dpc_regs[offset+3]])
    }

    fn write_dpc_reg_word(&mut self, reg_idx: usize, val: u32) {
        let offset = reg_idx * 4;
        self.dpc_regs[offset..offset+4].copy_from_slice(&val.to_be_bytes());
        if reg_idx == 1 {
            let start = u32::from_be_bytes([self.dpc_regs[0], self.dpc_regs[1], self.dpc_regs[2], self.dpc_regs[3]]) & 0x00FF_FFFF;
            // RDP command loop bounds check on end address
            let end = std::cmp::min(val & 0x00FF_FFFF, crate::n64::rdram::Rdram::SIZE as u32);
            let mut cur = start;
            while cur < end {
                if cur + 8 <= crate::n64::rdram::Rdram::SIZE as u32 {
                    let w0 = self.rdram.read_u32(cur);
                    let w1 = self.rdram.read_u32(cur + 4);
                    self.rdp.process_command(w0, w1, &mut self.rdram);
                }
                cur += 8;
            }
            self.dpc_regs[8..12].copy_from_slice(&val.to_be_bytes());
        }
    }

    pub fn rsp_hle_execute(&mut self) {
        let task_type = u32::from_be_bytes([self.sp_dmem[0xFC0], self.sp_dmem[0xFC1], self.sp_dmem[0xFC2], self.sp_dmem[0xFC3]]);
        let data_ptr = u32::from_be_bytes([self.sp_dmem[0xFE0], self.sp_dmem[0xFE1], self.sp_dmem[0xFE2], self.sp_dmem[0xFE3]]);
        
        if task_type == 2 {
            self.hle_execute_graphics_task(data_ptr);
        }
        
        self.rsp.halted = true;
        self.rsp.broke = true;
        self.set_mi_interrupt(0);
    }

    fn hle_execute_graphics_task(&mut self, start_pc: u32) {
        let mut pc = start_pc;
        let mut stack = Vec::new();
        let mut segments = [0u32; 16];
        let mut vertex_cache = [[0.0f32; 4]; 80];
        let mut color_cache = [[0.0f32; 4]; 80];
        let mut uv_cache = [[0.0f32; 2]; 80];
        
        let mut loop_count = 0;
        while loop_count < 10000 {
            loop_count += 1;
            let phys_pc = resolve_segmented(pc, &segments);
            if phys_pc + 8 > crate::n64::rdram::Rdram::SIZE as u32 {
                break;
            }
            let w0 = self.read_word(phys_pc);
            let w1 = self.read_word(phys_pc + 4);
            pc = pc.wrapping_add(8);
            
            let opcode = (w0 >> 24) as u8;
            match opcode {
                0xDF => {
                    if let Some(ret_pc) = stack.pop() {
                        pc = ret_pc;
                    } else {
                        break;
                    }
                }
                0xDE => {
                    let is_branch = ((w0 >> 16) & 0xFF) != 0;
                    let target = w1;
                    if !is_branch {
                        stack.push(pc);
                    }
                    pc = target;
                }
                0xDB => {
                    let index = (w0 >> 16) & 0xFF;
                    if index == 6 || index == 2 {
                        let seg_num = ((w0 >> 8) & 0xF) as usize;
                        segments[seg_num] = w1;
                    }
                }
                0x01 | 0x04 => {
                    let num_vtx = ((w0 >> 12) & 0xFF) as usize;
                    let dest_idx = (w0 & 0xFF) as usize;
                    let src_addr = resolve_segmented(w1, &segments);
                    for i in 0..num_vtx {
                        let v_addr = src_addr + (i * 16) as u32;
                        if v_addr + 16 <= crate::n64::rdram::Rdram::SIZE as u32 {
                            let x = self.read_halfword(v_addr) as i16 as f32;
                            let y = self.read_halfword(v_addr + 2) as i16 as f32;
                            let z = self.read_halfword(v_addr + 4) as i16 as f32;
                            let s = self.read_halfword(v_addr + 8) as i16 as f32;
                            let t = self.read_halfword(v_addr + 10) as i16 as f32;
                            let r = self.read_byte(v_addr + 12) as f32 / 255.0;
                            let g = self.read_byte(v_addr + 13) as f32 / 255.0;
                            let b = self.read_byte(v_addr + 14) as f32 / 255.0;
                            let a = self.read_byte(v_addr + 15) as f32 / 255.0;
                            
                            let idx = dest_idx + i;
                            if idx < 80 {
                                vertex_cache[idx] = [x, y, z, 1.0];
                                color_cache[idx] = [r, g, b, a];
                                uv_cache[idx] = [s, t];
                            }
                        }
                    }
                }
                0x05 | 0xBF => {
                    let v0 = (((w0 >> 16) & 0xFF) / 2) as usize;
                    let v1 = (((w0 >> 8) & 0xFF) / 2) as usize;
                    let v2 = ((w0 & 0xFF) / 2) as usize;
                    if v0 < 80 && v1 < 80 && v2 < 80 {
                        self.rdp.draw_triangle(
                            &mut self.rdram,
                            [vertex_cache[v0][0], vertex_cache[v0][1], vertex_cache[v0][2]],
                            color_cache[v0],
                            uv_cache[v0],
                            [vertex_cache[v1][0], vertex_cache[v1][1], vertex_cache[v1][2]],
                            color_cache[v1],
                            uv_cache[v1],
                            [vertex_cache[v2][0], vertex_cache[v2][1], vertex_cache[v2][2]],
                            color_cache[v2],
                            uv_cache[v2],
                        );
                    }
                }
                0x06 => {
                    let v0 = (((w0 >> 16) & 0xFF) / 2) as usize;
                    let v1 = (((w0 >> 8) & 0xFF) / 2) as usize;
                    let v2 = ((w0 & 0xFF) / 2) as usize;
                    let v3 = (((w1 >> 16) & 0xFF) / 2) as usize;
                    let v4 = (((w1 >> 8) & 0xFF) / 2) as usize;
                    let v5 = ((w1 & 0xFF) / 2) as usize;
                    if v0 < 80 && v1 < 80 && v2 < 80 {
                        self.rdp.draw_triangle(
                            &mut self.rdram,
                            [vertex_cache[v0][0], vertex_cache[v0][1], vertex_cache[v0][2]],
                            color_cache[v0],
                            uv_cache[v0],
                            [vertex_cache[v1][0], vertex_cache[v1][1], vertex_cache[v1][2]],
                            color_cache[v1],
                            uv_cache[v1],
                            [vertex_cache[v2][0], vertex_cache[v2][1], vertex_cache[v2][2]],
                            color_cache[v2],
                            uv_cache[v2],
                        );
                    }
                    if v3 < 80 && v4 < 80 && v5 < 80 {
                        self.rdp.draw_triangle(
                            &mut self.rdram,
                            [vertex_cache[v3][0], vertex_cache[v3][1], vertex_cache[v3][2]],
                            color_cache[v3],
                            uv_cache[v3],
                            [vertex_cache[v4][0], vertex_cache[v4][1], vertex_cache[v4][2]],
                            color_cache[v4],
                            uv_cache[v4],
                            [vertex_cache[v5][0], vertex_cache[v5][1], vertex_cache[v5][2]],
                            color_cache[v5],
                            uv_cache[v5],
                        );
                    }
                }
                _ => {
                    self.rdp.process_command(w0, w1, &mut self.rdram);
                }
            }
        }
    }

    pub fn update_video_buffer(
        &self,
        dest_buffer: &mut [u16],
        dest_width: u32,
        dest_height: u32,
        player_x: u32,
        player_y: u32,
    ) {
        if !self.n64_rom_loaded {
            dest_buffer.fill(bgr555(0, 255, 0));
            let px = player_x as usize;
            let py = player_y as usize;
            let offset = py * dest_width as usize + px;
            if offset < dest_buffer.len() {
                dest_buffer[offset] = bgr555(255, 0, 0);
            }
        } else {
            let control = self.read_vi_reg(0);
            let origin = self.read_vi_reg(1) & 0x00FF_FFFF;
            let width = self.read_vi_reg(2) & 0xFFF;
            let format = control & 3;

            if format >= 2 && width > 0 {
                for y in 0..(dest_height as usize) {
                    for x in 0..(dest_width as usize) {
                        let src_x = (x * width as usize) / dest_width as usize;
                        let src_y = (y * width as usize) / dest_width as usize;
                        let src_x = if src_x < width as usize { src_x } else { width as usize - 1 };
                        
                        let addr = origin + (src_y * width as usize + src_x) as u32 * if format == 2 { 2 } else { 4 };
                        if addr + if format == 2 { 2 } else { 4 } <= crate::n64::rdram::Rdram::SIZE as u32 {
                            let color = if format == 2 {
                                let pixel = self.rdram.read_u16(addr);
                                let r = ((pixel >> 11) & 0x1F) as u8;
                                let g = ((pixel >> 6) & 0x1F) as u8;
                                let b = ((pixel >> 1) & 0x1F) as u8;
                                bgr555((r << 3) | (r >> 2), (g << 3) | (g >> 2), (b << 3) | (b >> 2))
                            } else {
                                let pixel = self.rdram.read_u32(addr);
                                let r = ((pixel >> 24) & 0xFF) as u8;
                                let g = ((pixel >> 16) & 0xFF) as u8;
                                let b = ((pixel >> 8) & 0xFF) as u8;
                                bgr555(r, g, b)
                            };
                            dest_buffer[y * dest_width as usize + x] = color;
                        } else {
                            dest_buffer[y * dest_width as usize + x] = 0;
                        }
                    }
                }
            } else {
                dest_buffer.fill(0);
            }
        }
    }

    pub fn read_halfword(&self, addr: u32) -> u16 {
        let aligned = addr & !1;
        if aligned <= 0x007F_FFFF {
            self.rdram.read_u16(aligned)
        } else {
            let b0 = self.read_byte(aligned);
            let b1 = self.read_byte(aligned + 1);
            u16::from_be_bytes([b0, b1])
        }
    }

    pub fn read_word(&self, addr: u32) -> u32 {
        let aligned = addr & !3;
        if aligned <= 0x007F_FFFF {
            self.rdram.read_u32(aligned)
        } else {
            match aligned {
                0x0404_0000..=0x0404_001F => {
                    self.read_sp_reg_word(((aligned - 0x0404_0000) / 4) as usize)
                }
                0x0408_0000..=0x0408_0007 => {
                    self.read_sp_pc_reg_word(((aligned - 0x0408_0000) / 4) as usize)
                }
                0x0410_0000..=0x0410_001F => {
                    self.read_dpc_reg_word(((aligned - 0x0410_0000) / 4) as usize)
                }
                0x0430_0000..=0x0430_000F => {
                    self.read_mi_reg(((aligned - 0x0430_0000) / 4) as usize)
                }
                0x0440_0000..=0x0440_003F => {
                    self.read_vi_reg(((aligned - 0x0440_0000) / 4) as usize)
                }
                0x0450_0000..=0x0450_0017 => {
                    let reg_idx = ((aligned - 0x0450_0000) / 4) as usize;
                    match reg_idx {
                        0 => {
                            if self.ai_dma_count > 0 {
                                let bytes_played = self.ai_dma_length[0] - self.ai_dma_len_remaining;
                                self.ai_dma_address[0] + bytes_played
                            } else {
                                self.ai_programmed_addr
                            }
                        }
                        1 => self.ai_dma_len_remaining,
                        2 => self.ai_control,
                        3 => {
                            let mut status = 0u32;
                            if self.ai_dma_count == 2 {
                                status |= 1 << 31;
                            }
                            if self.ai_dma_count > 0 && (self.ai_control & 1) != 0 {
                                status |= 1 << 30;
                            }
                            status
                        }
                        4 => self.ai_dacrate,
                        5 => self.ai_bitrate,
                        _ => 0,
                    }
                }
                0x0460_0000..=0x0460_0033 => {
                    self.read_pi_reg(((aligned - 0x0460_0000) / 4) as usize)
                }
                0x0470_0000..=0x0470_001B => {
                    self.read_si_reg(((aligned - 0x0470_0000) / 4) as usize)
                }
                _ => {
                    let b0 = self.read_byte(aligned);
                    let b1 = self.read_byte(aligned + 1);
                    let b2 = self.read_byte(aligned + 2);
                    let b3 = self.read_byte(aligned + 3);
                    u32::from_be_bytes([b0, b1, b2, b3])
                }
            }
        }
    }

    pub fn read_doubleword(&self, addr: u32) -> u64 {
        let aligned = addr & !7;
        if aligned <= 0x007F_FFFF {
            self.rdram.read_u64(aligned)
        } else {
            let w0 = self.read_word(aligned);
            let w1 = self.read_word(aligned + 4);
            ((w0 as u64) << 32) | (w1 as u64)
        }
    }

    pub fn write_halfword(&mut self, addr: u32, val: u16) {
        let aligned = addr & !1;
        if aligned <= 0x007F_FFFF {
            self.rdram.write_u16(aligned, val);
        } else {
            let bytes = val.to_be_bytes();
            self.write_byte(aligned, bytes[0]);
            self.write_byte(aligned + 1, bytes[1]);
        }
    }

    pub fn write_word(&mut self, addr: u32, val: u32) {
        let aligned = addr & !3;
        if aligned <= 0x007F_FFFF {
            self.rdram.write_u32(aligned, val);
        } else {
            match aligned {
                0x0404_0000..=0x0404_001F => {
                    self.write_sp_reg_word(((aligned - 0x0404_0000) / 4) as usize, val);
                }
                0x0408_0000..=0x0408_0007 => {
                    self.write_sp_pc_reg_word(((aligned - 0x0408_0000) / 4) as usize, val);
                }
                0x0410_0000..=0x0410_001F => {
                    self.write_dpc_reg_word(((aligned - 0x0410_0000) / 4) as usize, val);
                }
                0x0430_0000..=0x0430_000F => {
                    self.write_mi_reg_word(((aligned - 0x0430_0000) / 4) as usize, val);
                }
                0x0440_0000..=0x0440_003F => {
                    self.write_vi_reg_word(((aligned - 0x0440_0000) / 4) as usize, val);
                }
                0x0450_0000..=0x0450_0017 => {
                    self.write_ai_reg_word(((aligned - 0x0450_0000) / 4) as usize, val);
                }
                0x0460_0000..=0x0460_0033 => {
                    self.write_pi_reg_word(((aligned - 0x0460_0000) / 4) as usize, val);
                }
                0x0470_0000..=0x0470_001B => {
                    self.write_si_reg_word(((aligned - 0x0470_0000) / 4) as usize, val);
                }
                _ => {
                    let bytes = val.to_be_bytes();
                    self.write_byte(aligned, bytes[0]);
                    self.write_byte(aligned + 1, bytes[1]);
                    self.write_byte(aligned + 2, bytes[2]);
                    self.write_byte(aligned + 3, bytes[3]);
                }
            }
        }
    }

    pub fn write_doubleword(&mut self, addr: u32, val: u64) {
        let aligned = addr & !7;
        if aligned <= 0x007F_FFFF {
            self.rdram.write_u64(aligned, val);
        } else {
            self.write_word(aligned, (val >> 32) as u32);
            self.write_word(aligned + 4, val as u32);
        }
    }
}
