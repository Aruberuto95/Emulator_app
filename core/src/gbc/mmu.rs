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
            },
            apu: Apu::new(),
            bg_palette_ram: [0xFF; 64], // Default to all white
            obj_palette_ram: [0xFF; 64],
            bcps: 0,
            ocps: 0,
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
                    let mut result = joyp | 0xC0;
                    if (joyp & 0x10) == 0 {
                        let mut btn = 0x0F;
                        if self.buttons.a {
                            btn &= !0x01;
                        }
                        if self.buttons.b {
                            btn &= !0x02;
                        }
                        if self.buttons.select {
                            btn &= !0x04;
                        }
                        if self.buttons.start {
                            btn &= !0x08;
                        }
                        result = (result & 0xF0) | btn;
                    } else if (joyp & 0x20) == 0 {
                        let mut dir = 0x0F;
                        if self.buttons.right {
                            dir &= !0x01;
                        }
                        if self.buttons.left {
                            dir &= !0x02;
                        }
                        if self.buttons.up {
                            dir &= !0x04;
                        }
                        if self.buttons.down {
                            dir &= !0x08;
                        }
                        result = (result & 0xF0) | dir;
                    } else {
                        result = (result & 0xF0) | 0x0F;
                    }
                    result
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
                    let active = (value & 0x80) == 0;
                    if active {
                        let length = ((value & 0x7F) as u32 + 1) * 16;
                        let src = ((self.io[0x51] as u16) << 8) | (self.io[0x52] & 0xF0) as u16;
                        let dst = 0x8000
                            | (((self.io[0x53] & 0x1F) as u16) << 8)
                            | (self.io[0x54] & 0xF0) as u16;
                        for i in 0..length {
                            let val = self.read_byte(src + i as u16);
                            let dst_addr = dst + i as u16;
                            if (0x8000..=0x9FFF).contains(&dst_addr) {
                                let bank = (self.io[0x4F] & 0x01) as usize;
                                let off = bank * 8192 + (dst_addr as usize - 0x8000);
                                self.vram[off] = val;
                            }
                        }
                        self.io[0x55] = 0xFF;
                    } else {
                        self.io[0x55] = value & 0x7F;
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
}
