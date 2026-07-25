use crate::ffi::ButtonState;
use crate::gbc::apu::Apu;
use crate::gbc::mbc3::Mbc3;

pub struct Mmu {
    pub mbc: Mbc3,
    pub vram: Vec<u8>,   // 16KB VRAM (2 banks of 8KB)
    pub wram: Vec<u8>,   // 32KB WRAM (8 banks of 4KB)
    pub oam: [u8; 160],  // 160 bytes Object Attribute Memory
    pub io: [u8; 128],   // 128 bytes I/O registers
    pub hram: [u8; 127], // 127 bytes HRAM
    pub ie: u8,          // Interrupt Enable register
    pub buttons: ButtonState,

    // GBC Sound Unit
    pub apu: Apu,

    // GBC Color Palette memory
    pub bg_palette_ram: [u8; 64],
    pub obj_palette_ram: [u8; 64],
    pub bcps: u8,
    pub ocps: u8,

    // H-Blank DMA (HDMA, FF51-FF55). Transient mid-transfer state.
    // ponytail: not serialized — a savestate captured between H-Blanks loses only the
    // in-flight blocks, negligible (savestate.rs serializes fields explicitly).
    pub hdma_active: bool, // an H-Blank DMA is in progress
    pub hdma_src: u16,     // next source address (already masked)
    pub hdma_dst: u16,     // next dest address in VRAM (0x8000-0x9FF0)
    pub hdma_blocks: u8,   // remaining 16-byte blocks
}

impl Mmu {
    pub fn new(rom: Vec<u8>, initial_ram: Option<Vec<u8>>) -> Self {
        let mbc = Mbc3::new(rom, initial_ram);
        let mut io = [0u8; 128];
        // Initialize I/O registers to standard default values
        io[0x00] = 0xFF; // Joypad
        io[0x44] = 0x90; // LY: typical V-blank line
        io[0x41] = 0x85; // STAT
        io[0x40] = 0x91; // LCDC
        io[0x05] = 0x00; // TIMA
        io[0x06] = 0x00; // TMA
        io[0x07] = 0x00; // TAC
        io[0x0F] = 0xE1; // IF
        io[0x26] = 0xF1; // NR52 (Sound status/enable)

        Self {
            mbc,
            vram: vec![0u8; 16 * 1024],
            wram: vec![0u8; 32 * 1024],
            oam: [0u8; 160],
            io,
            hram: [0u8; 127],
            ie: 0,
            buttons: ButtonState {
                up: false,
                down: false,
                left: false,
                right: false,
                a: false,
                b: false,
                start: false,
                select: false,
                l: false,
                r: false,
                x: false,
                y: false,
                nds_touch_x: 0,
                nds_touch_y: 0,
                nds_touch_pressed: false,
            },
            apu: Apu::new(),
            bg_palette_ram: [0xFF; 64], // Default to all white
            obj_palette_ram: [0xFF; 64],
            bcps: 0,
            ocps: 0,
            hdma_active: false,
            hdma_src: 0,
            hdma_dst: 0,
            hdma_blocks: 0,
        }
    }

    pub fn read_io(&self, offset: u8) -> u8 {
        self.io[offset as usize]
    }

    pub fn write_io(&mut self, offset: u8, value: u8) {
        self.io[offset as usize] = value;
    }

    pub fn read_byte(&self, address: u16) -> u8 {
        match address {
            0x0000..=0x7FFF => self.mbc.read_rom(address),
            0x8000..=0x9FFF => {
                let bank = (self.io[0x4F] & 0x01) as usize;
                let offset = bank * 8192 + (address as usize - 0x8000);
                self.vram[offset]
            }
            0xA000..=0xBFFF => self.mbc.read_ram_or_rtc(address),
            0xC000..=0xCFFF => self.wram[address as usize - 0xC000],
            0xD000..=0xDFFF => {
                let bank = match self.io[0x70] & 0x07 {
                    0 => 1,
                    b => b,
                } as usize;
                let offset = bank * 4096 + (address as usize - 0xD000);
                self.wram[offset]
            }
            0xE000..=0xFDFF => self.read_byte(address - 0x2000),
            0xFE00..=0xFE9F => self.oam[address as usize - 0xFE00],
            0xFEA0..=0xFEFF => 0x00,
            0xFF00..=0xFF7F => {
                let offset = (address - 0xFF00) as usize;
                if offset == 0x00 {
                    let joyp = self.io[0];
                    // P1/JOYP (FF00): bit5 (0x20)=0 selects the Action buttons,
                    // bit4 (0x10)=0 selects the Direction buttons. A pressed key reads as 0.
                    // When both lines are selected (both bits clear) hardware ANDs both
                    // nibbles, so start from all-released and AND-in each selected nibble.
                    let mut nibble = 0x0F;
                    if (joyp & 0x20) == 0 {
                        if self.buttons.a {
                            nibble &= !0x01;
                        }
                        if self.buttons.b {
                            nibble &= !0x02;
                        }
                        if self.buttons.select {
                            nibble &= !0x04;
                        }
                        if self.buttons.start {
                            nibble &= !0x08;
                        }
                    }
                    if (joyp & 0x10) == 0 {
                        if self.buttons.right {
                            nibble &= !0x01;
                        }
                        if self.buttons.left {
                            nibble &= !0x02;
                        }
                        if self.buttons.up {
                            nibble &= !0x04;
                        }
                        if self.buttons.down {
                            nibble &= !0x08;
                        }
                    }
                    ((joyp | 0xC0) & 0xF0) | nibble
                } else if offset == 0x04 {
                    self.io[0x04]
                } else if offset == 0x68 {
                    self.bcps
                } else if offset == 0x69 {
                    self.bg_palette_ram[(self.bcps & 0x3F) as usize]
                } else if offset == 0x6A {
                    self.ocps
                } else if offset == 0x6B {
                    self.obj_palette_ram[(self.ocps & 0x3F) as usize]
                } else {
                    self.io[offset]
                }
            }
            0xFF80..=0xFFFE => self.hram[address as usize - 0xFF80],
            0xFFFF => self.ie,
        }
    }

    pub fn write_byte(&mut self, address: u16, value: u8) {
        match address {
            0x0000..=0x7FFF => self.mbc.write_rom(address, value),
            0x8000..=0x9FFF => {
                let bank = (self.io[0x4F] & 0x01) as usize;
                let offset = bank * 8192 + (address as usize - 0x8000);
                self.vram[offset] = value;
            }
            0xA000..=0xBFFF => self.mbc.write_ram_or_rtc(address, value),
            0xC000..=0xCFFF => {
                self.wram[address as usize - 0xC000] = value;
            }
            0xD000..=0xDFFF => {
                let bank = match self.io[0x70] & 0x07 {
                    0 => 1,
                    b => b,
                } as usize;
                let offset = bank * 4096 + (address as usize - 0xD000);
                self.wram[offset] = value;
            }
            0xE000..=0xFDFF => {
                self.write_byte(address - 0x2000, value);
            }
            0xFE00..=0xFE9F => self.oam[address as usize - 0xFE00] = value,
            0xFEA0..=0xFEFF => {}
            0xFF00..=0xFF7F => {
                let offset = (address - 0xFF00) as usize;
                if offset == 0x46 {
                    let source_base = (value as u16) << 8;
                    for i in 0..160 {
                        let val = self.read_byte(source_base + i);
                        self.oam[i as usize] = val;
                    }
                    self.io[0x46] = value;
                } else if offset == 0x55 {
                    // FF55: bit7 selects the DMA mode. bit7=0 => General-Purpose DMA
                    // (whole block copied immediately, CPU "stalled"); bit7=1 => H-Blank DMA
                    // (16 bytes per H-Blank, driven from the PPU). Writing bit7=0 while an
                    // HDMA is in flight cancels it.
                    let src = ((self.io[0x51] as u16) << 8) | (self.io[0x52] & 0xF0) as u16;
                    let dst = 0x8000
                        | (((self.io[0x53] & 0x1F) as u16) << 8)
                        | (self.io[0x54] & 0xF0) as u16;
                    if (value & 0x80) == 0 {
                        if self.hdma_active {
                            // Cancel the running HDMA. bit7=1 marks it stopped; low 7 bits
                            // report blocks-remaining minus 1.
                            self.hdma_active = false;
                            self.io[0x55] = self.hdma_blocks.wrapping_sub(1) | 0x80;
                        } else {
                            // General-Purpose DMA: copy everything now.
                            let length = ((value & 0x7F) as u32 + 1) * 16;
                            let bank = (self.io[0x4F] & 0x01) as usize;
                            for i in 0..length {
                                let val = self.read_byte(src.wrapping_add(i as u16));
                                let dst_addr = dst.wrapping_add(i as u16);
                                if (0x8000..=0x9FFF).contains(&dst_addr) {
                                    let off = bank * 8192 + (dst_addr as usize - 0x8000);
                                    self.vram[off] = val;
                                }
                            }
                            self.io[0x55] = 0xFF;
                        }
                    } else {
                        // Start an H-Blank DMA.
                        self.hdma_src = src;
                        self.hdma_dst = dst;
                        self.hdma_blocks = (value & 0x7F) + 1;
                        self.hdma_active = true;
                        self.io[0x55] = value & 0x7F; // bit7=0 => active
                    }
                } else if offset == 0x68 {
                    self.bcps = value;
                } else if offset == 0x69 {
                    self.bg_palette_ram[(self.bcps & 0x3F) as usize] = value;
                    if (self.bcps & 0x80) != 0 {
                        self.bcps =
                            (self.bcps & 0x80) | ((self.bcps & 0x3F).wrapping_add(1) & 0x3F);
                    }
                } else if offset == 0x6A {
                    self.ocps = value;
                } else if offset == 0x6B {
                    self.obj_palette_ram[(self.ocps & 0x3F) as usize] = value;
                    if (self.ocps & 0x80) != 0 {
                        self.ocps =
                            (self.ocps & 0x80) | ((self.ocps & 0x3F).wrapping_add(1) & 0x3F);
                    }
                } else if (0x10..=0x26).contains(&offset) || (0x30..=0x3F).contains(&offset) {
                    self.apu.write_register(offset as u8, value, &mut self.io);
                    self.io[offset] = value;
                } else {
                    self.io[offset] = value;
                }
            }
            0xFF80..=0xFFFE => self.hram[address as usize - 0xFF80] = value,
            0xFFFF => self.ie = value,
        }
    }

    /// Transfer one 16-byte HDMA block. Call once per H-Blank of a visible scanline.
    /// No-op when no H-Blank DMA is active.
    pub fn hdma_step(&mut self) {
        if !self.hdma_active {
            return;
        }
        let bank = (self.io[0x4F] & 0x01) as usize;
        for i in 0..16 {
            let val = self.read_byte(self.hdma_src.wrapping_add(i));
            let dst = self.hdma_dst.wrapping_add(i);
            if (0x8000..=0x9FFF).contains(&dst) {
                self.vram[bank * 8192 + (dst as usize - 0x8000)] = val;
            }
        }
        self.hdma_src = self.hdma_src.wrapping_add(16);
        self.hdma_dst = self.hdma_dst.wrapping_add(16);
        self.hdma_blocks -= 1;
        if self.hdma_blocks == 0 {
            self.hdma_active = false;
            self.io[0x55] = 0xFF; // done
        } else {
            self.io[0x55] = self.hdma_blocks - 1; // remaining-1, bit7=0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdma_streams_blocks_per_hblank() {
        let mut mmu = Mmu::new(vec![0u8; 0x8000], None);
        // Source = WRAM 0xC000.., 32 distinct bytes.
        for i in 0..32u16 {
            mmu.write_byte(0xC000 + i, (i + 1) as u8);
        }
        // Program HDMA: src 0xC000, dst 0x8000 (VRAM bank 0), 2 blocks (length byte = 1).
        mmu.write_byte(0xFF51, 0xC0);
        mmu.write_byte(0xFF52, 0x00);
        mmu.write_byte(0xFF53, 0x00); // dst high 5 bits -> 0x8000
        mmu.write_byte(0xFF54, 0x00);
        mmu.write_byte(0xFF55, 0x81); // bit7=1 (HDMA), 1 => 2 blocks

        assert!(mmu.hdma_active);
        assert_eq!(mmu.read_byte(0xFF55), 0x01); // active, 1 = blocks-1

        mmu.hdma_step(); // first 16 bytes
        assert!(mmu.hdma_active);
        assert_eq!(mmu.read_byte(0xFF55), 0x00); // 1 block left -> remaining-1 = 0

        mmu.hdma_step(); // last 16 bytes
        assert!(!mmu.hdma_active);
        assert_eq!(mmu.read_byte(0xFF55), 0xFF); // done

        for i in 0..32usize {
            assert_eq!(mmu.vram[i], (i + 1) as u8);
        }
        // A further step must be a no-op.
        mmu.hdma_step();
        assert_eq!(mmu.read_byte(0xFF55), 0xFF);
    }
}
