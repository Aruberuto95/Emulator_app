use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlashState {
    Ready,
    CommandSeq1,
    CommandSeq2,
    Identify,
    Write,
    BankSelect,
    EraseSeq,
    EraseSeq1,
    EraseSeq2,
}

pub struct Flash128 {
    pub data: Vec<u8>,
    pub state: FlashState,
    pub bank: usize,
    pub is_dirty: bool,
    pub manufacturer_id: u8,
    pub device_id: u8,
}

impl Flash128 {
    /// Creates a new Flash128 instance with Sanyo IDs by default (0x62, 0x13).
    pub fn new() -> Self {
        Self {
            data: vec![0xFF; 128 * 1024],
            state: FlashState::Ready,
            bank: 0,
            is_dirty: false,
            manufacturer_id: 0x62, // Sanyo ID
            device_id: 0x13,       // Sanyo 128K chip ID
        }
    }

    pub fn read_byte(&self, address: u32) -> u8 {
        let offset = (address & 0xFFFF) as usize;
        if self.state == FlashState::Identify {
            if offset == 0 {
                return self.manufacturer_id;
            } else if offset == 1 {
                return self.device_id;
            }
        }
        // Saturating, because `bank` is a `pub usize` that `load_state` restores
        // verbatim from an unhashed JSON savestate: `usize::MAX * 65536` is an
        // overflowing multiply, which panics in debug — and a Rust panic aborts
        // the process across the cxx FFI boundary. Saturating lands past
        // `data.len()`, so the bounds test below falls through to open bus,
        // which is what an unmapped bank reads as on a real cartridge.
        let global_offset = self.bank.saturating_mul(64 * 1024).saturating_add(offset);
        if global_offset < self.data.len() {
            self.data[global_offset]
        } else {
            0xFF
        }
    }

    pub fn write_byte(&mut self, address: u32, value: u8) {
        let offset = (address & 0xFFFF) as usize;

        match self.state {
            FlashState::Ready => {
                if offset == 0x5555 && value == 0xAA {
                    self.state = FlashState::CommandSeq1;
                }
            }
            FlashState::CommandSeq1 => {
                if offset == 0x2AAA && value == 0x55 {
                    self.state = FlashState::CommandSeq2;
                } else {
                    self.state = FlashState::Ready;
                }
            }
            FlashState::CommandSeq2 => {
                if offset == 0x5555 {
                    match value {
                        0x90 => self.state = FlashState::Identify,
                        0xA0 => self.state = FlashState::Write,
                        0xB0 => self.state = FlashState::BankSelect,
                        0x80 => self.state = FlashState::EraseSeq,
                        0xF0 => self.state = FlashState::Ready,
                        _ => self.state = FlashState::Ready,
                    }
                } else {
                    self.state = FlashState::Ready;
                }
            }
            FlashState::Identify => {
                if value == 0xF0 {
                    self.state = FlashState::Ready;
                }
            }
            FlashState::Write => {
                // Saturating, because `bank` is a `pub usize` that `load_state` restores
        // verbatim from an unhashed JSON savestate: `usize::MAX * 65536` is an
        // overflowing multiply, which panics in debug — and a Rust panic aborts
        // the process across the cxx FFI boundary. Saturating lands past
        // `data.len()`, so the bounds test below falls through to open bus,
        // which is what an unmapped bank reads as on a real cartridge.
        let global_offset = self.bank.saturating_mul(64 * 1024).saturating_add(offset);
                if global_offset < self.data.len() {
                    let current_val = self.data[global_offset];
                    self.data[global_offset] = current_val & value; // Flash bits only transition 1 -> 0
                    self.is_dirty = true;
                }
                self.state = FlashState::Ready;
            }
            FlashState::BankSelect => {
                if offset == 0 {
                    self.bank = (value & 1) as usize;
                }
                self.state = FlashState::Ready;
            }
            FlashState::EraseSeq => {
                if offset == 0x5555 && value == 0xAA {
                    self.state = FlashState::EraseSeq1;
                } else {
                    self.state = FlashState::Ready;
                }
            }
            FlashState::EraseSeq1 => {
                if offset == 0x2AAA && value == 0x55 {
                    self.state = FlashState::EraseSeq2;
                } else {
                    self.state = FlashState::Ready;
                }
            }
            FlashState::EraseSeq2 => {
                if offset == 0x5555 && value == 0x10 {
                    // Chip Erase
                    self.data.fill(0xFF);
                    self.is_dirty = true;
                } else if value == 0x30 {
                    // Sector Erase (4 KB sector based on sector address)
                    let sector_start = offset & 0xF000;
                    let global_sector_start = (self.bank * 64 * 1024) + sector_start;
                    if global_sector_start + 4096 <= self.data.len() {
                        for i in 0..4096 {
                            self.data[global_sector_start + i] = 0xFF;
                        }
                        self.is_dirty = true;
                    }
                }
                self.state = FlashState::Ready;
            }
        }
    }

    pub fn save_flash_to_disk(&self, rom_path: &Path, base_dir: &Path) -> Result<(), String> {
        crate::rom::write_battery_file(rom_path, base_dir, &self.data)
    }

    pub fn load_flash_from_disk(&mut self, rom_path: &Path, base_dir: &Path) -> Result<(), String> {
        crate::rom::read_battery_file(rom_path, base_dir, &mut self.data).map(|_| ())
    }
}
