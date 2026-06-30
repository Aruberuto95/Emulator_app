use crate::gba::apu::GbaApu;
use crate::gba::dma::GbaDma;
use crate::gba::flash::Flash128;

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

    pub rom_path: std::path::PathBuf,
    pub base_dir: std::path::PathBuf,
}

impl GbaMmu {
    pub fn new(rom_data: Vec<u8>) -> Self {
        let bios = vec![0u8; 16384];
        // In our BIOS HLE structure, we can optionally fill BIOS with HLE stub calls or just leave it zeroed
        // since we intercept the SWI instructions directly.

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
            io: [0u8; 1024],
            waitcnt: 0,
            ie: 0,
            r_if: 0,
            ime: 0,
            bios_protected: false,
            last_bios_read: 0xEA00002E, // standard branch opcode
            rom_path: std::path::PathBuf::new(),
            base_dir: std::path::PathBuf::new(),
        }
    }

    pub fn trigger_interrupt(&mut self, interrupt_bit: u16) {
        self.r_if |= interrupt_bit;
    }

    // --- Timing and Ticking ---
    pub fn tick_system_components(
        &mut self,
        elapsed: u32,
        video_slice: &mut [u8],
        audio_buf: &mut [i16],
        audio_off: usize,
        speed: f32,
        ppu: &mut crate::gba::ppu::GbaPpu,
        is_render_tick: bool,
    ) {
        let mut remaining = elapsed;
        while remaining > 0 {
            let mut step = remaining;
            for i in 0..4 {
                let enabled = (self.timers[i].control & 0x0080) != 0;
                let cascade = (self.timers[i].control & 0x0004) != 0;
                if enabled && !cascade {
                    let prescaler = self.timers[i].get_prescaler() as u32;
                    let acc = self.timers[i].cycle_accumulator as u32;
                    let cycles_to_next_tick = if prescaler > acc { prescaler - acc } else { 1 };

                    let ticks_to_overflow = (0x10000 - self.timers[i].counter as u32) as u32;
                    let cycles_to_overflow =
                        cycles_to_next_tick + (ticks_to_overflow - 1) * prescaler;
                    if cycles_to_overflow < step {
                        step = cycles_to_overflow;
                    }
                }
            }
            if step == 0 {
                step = 1;
            }

            // Advance components by step cycles
            self.tick_timers(step);
            ppu.tick(step, self, video_slice, is_render_tick);
            self.apu.tick(step, audio_buf, audio_off, speed);
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
        // DMA 0 to 3 Priority Order
        for ch in 0..4 {
            let active = self.dma.channels[ch].active;
            if !active {
                continue;
            }

            let timing = (self.dma.channels[ch].control >> 12) & 3;
            let mut trigger = false;

            match timing {
                0 => trigger = true, // Immediate
                1 => {
                    // VBlank trigger: check if VBlank status is active
                    // We check if PPU is in VBlank (done by PPU tick when setting dispstat)
                    let dispstat = self.read_halfword_safe(0x04000004);
                    if (dispstat & 0x0001) != 0 {
                        trigger = true;
                    }
                }
                2 => {
                    // HBlank trigger
                    let dispstat = self.read_halfword_safe(0x04000004);
                    if (dispstat & 0x0002) != 0 {
                        trigger = true;
                    }
                }
                3 => {
                    // Special trigger
                    if ch == 1 && self.apu.dma_request_a {
                        trigger = true;
                        self.apu.dma_request_a = false;
                    } else if ch == 2 && self.apu.dma_request_b {
                        trigger = true;
                        self.apu.dma_request_b = false;
                    }
                }
                _ => {}
            }

            if trigger {
                self.execute_dma_channel(ch);
            }
        }
    }

    fn execute_dma_channel(&mut self, ch: usize) {
        let is_32bit = (self.dma.channels[ch].control & 0x0400) != 0;
        let dest_ctrl = (self.dma.channels[ch].control >> 5) & 3;
        let src_ctrl = (self.dma.channels[ch].control >> 7) & 3;
        let repeat = (self.dma.channels[ch].control & 0x0200) != 0;

        let unit_bytes = if is_32bit { 4 } else { 2 };
        let timing = (self.dma.channels[ch].control >> 12) & 3;

        // Special FIFO refills transfer exactly 4 words (16 bytes)
        let is_fifo = timing == 3 && (ch == 1 || ch == 2);
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

            // Update dest address
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
                    // Direct Sound FIFOs are write-only, read returns 0
                    if offset == 0xA0 || offset == 0xA4 {
                        0
                    } else {
                        self.io[offset as usize]
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
            // Sound registers HLE writes
            0x82 | 0x83 | 0x84 | 0x85 => {
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
                        0 => {
                            self.timers[timer_idx].reload =
                                (self.timers[timer_idx].reload & 0xFF00) | (value as u16)
                        }
                        1 => {
                            self.timers[timer_idx].reload =
                                (self.timers[timer_idx].reload & 0x00FF) | ((value as u16) << 8)
                        }
                        2 => {
                            self.timers[timer_idx].control =
                                (self.timers[timer_idx].control & 0xFF00) | (value as u16);
                            if (value & 0x80) != 0 {
                                self.timers[timer_idx].counter = self.timers[timer_idx].reload;
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

    pub fn read_halfword(&self, address: u32) -> u16 {
        // If address is odd, trigger rotation (or just alignment in GBA depending on region)
        // Usually, halfword read at odd address yields: rotated value
        let aligned_addr = address & !1;
        let b0 = self.read_byte(aligned_addr) as u16;
        let b1 = self.read_byte(aligned_addr + 1) as u16;
        let val = b0 | (b1 << 8);

        if (address & 1) != 0 {
            // Rotate right by 8 bits
            (val >> 8) | (val << 8)
        } else {
            val
        }
    }

    pub fn read_word(&self, address: u32) -> u32 {
        let aligned_addr = address & !3;
        let b0 = self.read_byte(aligned_addr) as u32;
        let b1 = self.read_byte(aligned_addr + 1) as u32;
        let b2 = self.read_byte(aligned_addr + 2) as u32;
        let b3 = self.read_byte(aligned_addr + 3) as u32;
        let val = b0 | (b1 << 8) | (b2 << 16) | (b3 << 24);

        let rotation = (address & 3) * 8;
        if rotation > 0 {
            (val >> rotation) | (val << (32 - rotation))
        } else {
            val
        }
    }

    pub fn write_halfword(&mut self, address: u32, value: u16) {
        let aligned_addr = address & !1;
        self.write_byte(aligned_addr, (value & 0xFF) as u8);
        self.write_byte(aligned_addr + 1, ((value >> 8) & 0xFF) as u8);
    }

    pub fn write_word(&mut self, address: u32, value: u32) {
        let aligned_addr = address & !3;
        self.write_byte(aligned_addr, (value & 0xFF) as u8);
        self.write_byte(aligned_addr + 1, ((value >> 8) & 0xFF) as u8);
        self.write_byte(aligned_addr + 2, ((value >> 16) & 0xFF) as u8);
        self.write_byte(aligned_addr + 3, ((value >> 24) & 0xFF) as u8);
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
        self.iwram.fill(0);
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
    }

    pub fn reset_other_io_registers(&mut self) {
        // Clear remaining IO (except key status, etc.)
        for i in 0..1024 {
            if (i < 0x60 || i >= 0xB0) && (i < 0x120 || i >= 0x130) {
                self.io[i] = 0;
            }
        }
    }
}
