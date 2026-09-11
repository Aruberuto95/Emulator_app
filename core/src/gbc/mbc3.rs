use std::path::Path;
use std::time::SystemTime;

/// Real-Time Clock state container for MBC3.
#[derive(Clone)]
pub struct RealTimeClock {
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    pub days: u16,
    pub halt: bool,
    pub day_overflow: bool,

    // Latched snapshot counters
    pub latched_seconds: u8,
    pub latched_minutes: u8,
    pub latched_hours: u8,
    pub latched_days_low: u8,
    pub latched_days_high: u8,

    // Timing helper
    pub cycle_accumulator: u64,
}

impl RealTimeClock {
    pub fn new() -> Self {
        Self {
            seconds: 0,
            minutes: 0,
            hours: 0,
            days: 0,
            halt: false,
            day_overflow: false,
            latched_seconds: 0,
            latched_minutes: 0,
            latched_hours: 0,
            latched_days_low: 0,
            latched_days_high: 0,
            cycle_accumulator: 0,
        }
    }

    pub fn tick_cycles(&mut self, cycles: u64, double_speed: bool) {
        if self.halt {
            return;
        }
        // Normalize cycles. In double-speed mode, the CPU runs twice as fast.
        let normalized_cycles = if double_speed { cycles / 2 } else { cycles };
        self.cycle_accumulator += normalized_cycles;

        // 4,194,304 Hz is the normal speed GBC clock frequency.
        while self.cycle_accumulator >= 4_194_304 {
            self.cycle_accumulator -= 4_194_304;
            self.increment_second();
        }
    }

    pub fn increment_second(&mut self) {
        self.seconds += 1;
        if self.seconds >= 60 {
            self.seconds = 0;
            self.minutes += 1;
            if self.minutes >= 60 {
                self.minutes = 0;
                self.hours += 1;
                if self.hours >= 24 {
                    self.hours = 0;
                    self.days += 1;
                    if self.days >= 512 {
                        self.days = 0;
                        self.day_overflow = true;
                    }
                }
            }
        }
    }

    /// Catch up a battery clock without iterating once per elapsed second.
    /// Carry each counter directly, preserving the register-write behavior for
    /// values like seconds=63 (the next second resets it to zero and carries).
    fn advance_seconds(&mut self, elapsed: u64) {
        if self.halt || elapsed == 0 {
            return;
        }
        fn advance(value: u64, limit: u64, ticks: u64) -> (u64, u64) {
            let until_carry = limit.saturating_sub(value).max(1);
            if ticks < until_carry {
                return (value + ticks, 0);
            }
            let remaining = ticks - until_carry;
            (remaining % limit, 1 + remaining / limit)
        }
        let (seconds, minutes) = advance(self.seconds as u64, 60, elapsed);
        let (minutes, hours) = advance(self.minutes as u64, 60, minutes);
        let (hours, days) = advance(self.hours as u64, 24, hours);
        let (days, wraps) = advance(self.days as u64, 512, days);
        self.seconds = seconds as u8;
        self.minutes = minutes as u8;
        self.hours = hours as u8;
        self.days = days as u16;
        self.day_overflow |= wraps != 0;
    }

    pub fn latch(&mut self) {
        self.latched_seconds = self.seconds;
        self.latched_minutes = self.minutes;
        self.latched_hours = self.hours;
        self.latched_days_low = (self.days & 0xFF) as u8;

        let mut dh = 0u8;
        if self.days > 255 {
            dh |= 0x01;
        }
        if self.halt {
            dh |= 0x40;
        }
        if self.day_overflow {
            dh |= 0x80;
        }
        self.latched_days_high = dh;
    }

    pub fn write_register(&mut self, reg: u8, val: u8) {
        match reg {
            0x08 => self.seconds = val & 0x3F,
            0x09 => self.minutes = val & 0x3F,
            0x0A => self.hours = val & 0x1F,
            0x0B => self.days = (self.days & 0x100) | (val as u16),
            0x0C => {
                if (val & 0x01) != 0 {
                    self.days |= 0x100;
                } else {
                    self.days &= 0x0FF;
                }
                self.halt = (val & 0x40) != 0;
                self.day_overflow = (val & 0x80) != 0;
            }
            _ => {}
        }
    }

    pub fn read_register(&self, reg: u8) -> u8 {
        match reg {
            0x08 => self.latched_seconds,
            0x09 => self.latched_minutes,
            0x0A => self.latched_hours,
            0x0B => self.latched_days_low,
            0x0C => self.latched_days_high,
            _ => 0xFF,
        }
    }
}

/// Memory Bank Controller 3 implementation with RTC support.
#[derive(Clone)]
pub struct Mbc3 {
    pub rom: Vec<u8>,
    pub ram: Vec<u8>,
    pub rom_bank: u8,
    pub ram_bank_or_rtc_reg: u8,
    pub ram_rtc_enabled: bool,
    pub rtc: RealTimeClock,
    pub latch_state: u8,
    pub is_dirty: bool,
}

impl Mbc3 {
    pub fn new(rom: Vec<u8>, initial_ram: Option<Vec<u8>>) -> Self {
        let ram = initial_ram.unwrap_or_else(|| vec![0u8; 32 * 1024]);
        Self {
            rom,
            ram,
            rom_bank: 1,
            ram_bank_or_rtc_reg: 0,
            ram_rtc_enabled: false,
            rtc: RealTimeClock::new(),
            latch_state: 0xFF,
            is_dirty: false,
        }
    }

    pub fn read_rom(&self, address: u16) -> u8 {
        // Bounds-checked: out-of-range banks read as open-bus 0xFF instead of
        // wrapping (`% rom.len()`) back into bank 0, which would feed the CPU
        // wrong opcodes for over-large bank selections.
        let index = if address < 0x4000 {
            address as usize // ROM Bank 0 (fixed)
        } else {
            let bank = if self.rom_bank == 0 { 1 } else { self.rom_bank };
            (bank as usize * 0x4000) + (address as usize - 0x4000)
        };
        self.rom.get(index).copied().unwrap_or(0xFF)
    }

    pub fn write_rom(&mut self, address: u16, value: u8) {
        if address < 0x2000 {
            // Enable SRAM/RTC
            self.ram_rtc_enabled = (value & 0x0F) == 0x0A;
        } else if address < 0x4000 {
            // Select ROM Bank
            let mut bank = value & 0x7F;
            if bank == 0 {
                bank = 1;
            }
            self.rom_bank = bank;
        } else if address < 0x6000 {
            // Select RAM Bank or RTC Register
            self.ram_bank_or_rtc_reg = value;
        } else if address < 0x8000 {
            // Latch Clock Data
            if self.latch_state == 0x00 && value == 0x01 {
                self.rtc.latch();
            }
            self.latch_state = value;
        }
    }

    pub fn read_ram_or_rtc(&self, address: u16) -> u8 {
        if !self.ram_rtc_enabled {
            return 0xFF;
        }

        let reg = self.ram_bank_or_rtc_reg;
        if reg <= 0x03 {
            // SRAM bank read (bounds-checked: out-of-range -> open-bus 0xFF)
            let offset = (reg as usize * 8 * 1024) + (address as usize - 0xA000);
            self.ram.get(offset).copied().unwrap_or(0xFF)
        } else if (0x08..=0x0C).contains(&reg) {
            // RTC read
            self.rtc.read_register(reg)
        } else {
            0xFF
        }
    }

    pub fn write_ram_or_rtc(&mut self, address: u16, value: u8) {
        if !self.ram_rtc_enabled {
            return;
        }

        let reg = self.ram_bank_or_rtc_reg;
        if reg <= 0x03 {
            // SRAM bank write (bounds-checked: out-of-range writes are ignored,
            // never wrapped into a valid cell)
            let offset = (reg as usize * 8 * 1024) + (address as usize - 0xA000);
            if let Some(cell) = self.ram.get_mut(offset) {
                *cell = value;
                self.is_dirty = true;
            }
        } else if (0x08..=0x0C).contains(&reg) {
            // RTC write
            self.rtc.write_register(reg, value);
            self.is_dirty = true;
        }
    }

    pub fn save_sram(&self, rom_path: &Path, base_dir: &Path) -> Result<(), String> {
        let mut save_data = Vec::with_capacity(self.ram.len() + 28);
        save_data.extend_from_slice(&self.ram);

        // Serialize RTC
        save_data.extend_from_slice(&(self.rtc.seconds as u32).to_le_bytes());
        save_data.extend_from_slice(&(self.rtc.minutes as u32).to_le_bytes());
        save_data.extend_from_slice(&(self.rtc.hours as u32).to_le_bytes());
        save_data.extend_from_slice(&(self.rtc.days as u32).to_le_bytes());

        let mut flags = 0u32;
        if self.rtc.halt {
            flags |= 0x01;
        }
        if self.rtc.day_overflow {
            flags |= 0x02;
        }
        save_data.extend_from_slice(&flags.to_le_bytes());

        // Serialize unix timestamp
        let current_time = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        save_data.extend_from_slice(&current_time.to_le_bytes());

        crate::rom::write_battery_file(rom_path, base_dir, &save_data)
    }

    pub fn load_sram(&mut self, rom_path: &Path, base_dir: &Path) -> Result<bool, String> {
        use std::io::Read;
        let safe_save_path = crate::rom::battery_path(rom_path, base_dir)?;
        if !safe_save_path.exists() {
            return Ok(false);
        }
        let file = std::fs::File::open(&safe_save_path)
            .map_err(|e| format!("Failed to read GBC battery: {e}"))?;
        // Only SRAM and the optional RTC footer belong to this device.
        let mut save_data = Vec::with_capacity(32 * 1024 + 28);
        file.take((32 * 1024 + 28) as u64).read_to_end(&mut save_data)
            .map_err(|e| format!("Failed to read GBC battery: {e}"))?;
        if save_data.len() < 32 * 1024 {
            return Err("Truncated GBC battery save".to_string());
        }

        self.ram = save_data[..32 * 1024].to_vec();

        // Check if RTC data is appended
        if save_data.len() >= 32 * 1024 + 28 {
            let footer = &save_data[32 * 1024..];
            let seconds = u32::from_le_bytes(footer[0..4].try_into().unwrap()) as u8;
            let minutes = u32::from_le_bytes(footer[4..8].try_into().unwrap()) as u8;
            let hours = u32::from_le_bytes(footer[8..12].try_into().unwrap()) as u8;
            let days = u32::from_le_bytes(footer[12..16].try_into().unwrap()) as u16;
            let flags = u32::from_le_bytes(footer[16..20].try_into().unwrap());
            let saved_timestamp = u64::from_le_bytes(footer[20..28].try_into().unwrap());

            self.rtc.seconds = seconds;
            self.rtc.minutes = minutes;
            self.rtc.hours = hours;
            self.rtc.days = days;
            self.rtc.halt = (flags & 0x01) != 0;
            self.rtc.day_overflow = (flags & 0x02) != 0;

            // RTC catch-up
            if !self.rtc.halt && saved_timestamp > 0 {
                let current_time = SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if current_time > saved_timestamp {
                    let diff_seconds = current_time - saved_timestamp;
                    self.rtc.advance_seconds(diff_seconds);
                }
            }
        }

        Ok(true)
    }
}

#[cfg(test)]
mod mbc3_bounds_tests {
    use super::*;

    fn rtc_fields(rtc: &RealTimeClock) -> (u8, u8, u8, u16, bool) {
        (rtc.seconds, rtc.minutes, rtc.hours, rtc.days, rtc.day_overflow)
    }

    #[test]
    fn maintenance_rtc_bulk_advance_matches_secondwise_carries() {
        for (seconds, minutes, hours, days) in [(0, 0, 0, 0), (59, 59, 23, 511), (63, 63, 31, 511), (12, 34, 5, 90)] {
            for elapsed in [0, 1, 59, 60, 3600, 86401] {
                let mut slow = RealTimeClock::new();
                slow.seconds = seconds;
                slow.minutes = minutes;
                slow.hours = hours;
                slow.days = days;
                let mut fast = slow.clone();
                for _ in 0..elapsed { slow.increment_second(); }
                fast.advance_seconds(elapsed);
                assert_eq!(rtc_fields(&fast), rtc_fields(&slow), "elapsed={elapsed}");
            }
        }
    }

    #[test]
    fn maintenance_rtc_bulk_advance_handles_decades_halt_and_large_timestamps() {
        let mut rtc = RealTimeClock::new();
        let elapsed = 40 * 365 * 86400 + 3661;
        rtc.advance_seconds(elapsed);
        assert_eq!(rtc_fields(&rtc), (1, 1, 1, (40 * 365 % 512) as u16, true));
        rtc.halt = true;
        let before = rtc_fields(&rtc);
        rtc.advance_seconds(u64::MAX);
        assert_eq!(rtc_fields(&rtc), before);
        rtc.halt = false;
        rtc.advance_seconds(u64::MAX);
        assert!(rtc.seconds < 60 && rtc.minutes < 60 && rtc.hours < 24 && rtc.days < 512);
        assert!(rtc.day_overflow);
    }

    struct BatteryFixture(std::path::PathBuf);
    impl BatteryFixture {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("emu_maintenance_battery_{label}_{}_{nonce}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for BatteryFixture {
        fn drop(&mut self) {
            if self.0.parent() == Some(std::env::temp_dir().as_path()) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn maintenance_gbc_battery_preserves_rtc_and_accepts_legacy_and_oversized_files() {
        let dir = BatteryFixture::new("roundtrip");
        let rom = dir.0.join("game.gbc");
        let mut original = Mbc3::new(vec![], None);
        original.ram[0] = 0x73;
        original.ram[32767] = 0xC9;
        original.rtc.seconds = 37;
        original.rtc.days = 400;
        original.rtc.halt = true;
        original.rtc.day_overflow = true;
        original.save_sram(&rom, &dir.0).unwrap();
        let mut loaded = Mbc3::new(vec![], None);
        assert!(loaded.load_sram(&rom, &dir.0).unwrap());
        assert_eq!(loaded.ram, original.ram);
        assert_eq!(rtc_fields(&loaded.rtc), rtc_fields(&original.rtc));
        assert!(loaded.rtc.halt);

        // A large trailing body is irrelevant to the fixed SRAM + RTC payload.
        let file = std::fs::OpenOptions::new().write(true).open(rom.with_extension("sav")).unwrap();
        file.set_len(16 * 1024 * 1024).unwrap();
        drop(file);
        assert!(loaded.load_sram(&rom, &dir.0).unwrap());
        assert_eq!(loaded.ram, original.ram);
        assert_eq!(rtc_fields(&loaded.rtc), rtc_fields(&original.rtc));

        // Legacy battery files contain only SRAM; the optional footer is absent.
        std::fs::write(rom.with_extension("sav"), &original.ram).unwrap();
        let mut legacy = Mbc3::new(vec![], None);
        assert!(legacy.load_sram(&rom, &dir.0).unwrap());
        assert_eq!(legacy.ram, original.ram);
        assert_eq!(rtc_fields(&legacy.rtc), (0, 0, 0, 0, false));
        std::fs::write(rom.with_extension("sav"), [0xFF; 12]).unwrap();
        assert!(legacy.load_sram(&rom, &dir.0).is_err());
        assert_eq!(legacy.ram, original.ram, "a truncated file must not partially apply");
    }

    #[test]
    fn maintenance_gbc_battery_rejects_rom_overwrite_and_preserves_previous_save_on_failure() {
        let dir = BatteryFixture::new("write_guard");
        let mbc = Mbc3::new(vec![], None);
        let disguised_rom = dir.0.join("cartridge.sav");
        std::fs::write(&disguised_rom, b"original ROM").unwrap();
        assert!(mbc.save_sram(&disguised_rom, &dir.0).is_err());
        assert_eq!(std::fs::read(&disguised_rom).unwrap(), b"original ROM");

        // A cartridge can also share the temporary name without sharing .sav.
        let temporary_named_rom = dir.0.join("temporary.tmp");
        std::fs::write(&temporary_named_rom, b"ROM with tmp extension").unwrap();
        assert!(mbc.save_sram(&temporary_named_rom, &dir.0).is_err());
        assert_eq!(std::fs::read(&temporary_named_rom).unwrap(), b"ROM with tmp extension");

        #[cfg(windows)]
        {
            let uppercase_rom = dir.0.join("uppercase.SAV");
            std::fs::write(&uppercase_rom, b"case-insensitive ROM").unwrap();
            assert!(mbc.save_sram(&uppercase_rom, &dir.0).is_err());
            assert_eq!(std::fs::read(&uppercase_rom).unwrap(), b"case-insensitive ROM");
        }

        let rom = dir.0.join("game.gbc");
        std::fs::write(rom.with_extension("sav"), b"previous battery").unwrap();
        std::fs::create_dir(rom.with_extension("tmp")).unwrap();
        assert!(mbc.save_sram(&rom, &dir.0).is_err());
        assert_eq!(std::fs::read(rom.with_extension("sav")).unwrap(), b"previous battery");
    }

    #[cfg(unix)]
    #[test]
    fn maintenance_gbc_battery_rejects_temporary_symlink_escape() {
        let dir = BatteryFixture::new("symlink");
        let outside = BatteryFixture::new("outside");
        let target = outside.0.join("untouched");
        std::fs::write(&target, b"outside file").unwrap();
        let rom = dir.0.join("game.gbc");
        std::os::unix::fs::symlink(&target, rom.with_extension("tmp")).unwrap();
        let mbc = Mbc3::new(vec![], None);
        assert!(mbc.save_sram(&rom, &dir.0).is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"outside file");
    }

    #[test]
    fn read_rom_out_of_range_returns_open_bus_not_wrapped() {
        // 2-bank ROM (32 KiB). Mark bank 0 and bank 1 distinctly.
        let mut rom = vec![0u8; 0x8000];
        rom[0x0000] = 0xAA; // bank 0, addr 0x0000
        rom[0x4000] = 0xBB; // bank 1, addr 0x4000
        let mut mbc = Mbc3::new(rom, None);

        // Valid: bank 1 selected, read 0x4000 -> 0xBB
        mbc.rom_bank = 1;
        assert_eq!(mbc.read_rom(0x4000), 0xBB);
        // Fixed bank 0 always readable
        assert_eq!(mbc.read_rom(0x0000), 0xAA);

        // Out-of-range bank: must be open-bus 0xFF, NOT wrapped back into bank 0.
        mbc.rom_bank = 0x40; // bank 64 -> offset 0x100000, well past 0x8000
        assert_eq!(mbc.read_rom(0x4000), 0xFF, "out-of-range bank must read 0xFF");
    }

    #[test]
    fn ram_out_of_range_write_is_ignored() {
        let rom = vec![0u8; 0x8000];
        // 1 bank of SRAM (8 KiB) covers exactly 0xA000-0xBFFF for bank 0.
        let mut mbc = Mbc3::new(rom, Some(vec![0u8; 8 * 1024]));
        mbc.ram_rtc_enabled = true;

        // Out-of-range bank (1) -> offset 0x2000, past the single 8 KiB bank.
        mbc.ram_bank_or_rtc_reg = 0x01;
        mbc.write_ram_or_rtc(0xA000, 0x55); // must be ignored, not wrapped into bank 0
        assert_eq!(mbc.read_ram_or_rtc(0xA000), 0xFF, "out-of-range read is open-bus");

        // Bank 0 cell must be untouched by the out-of-range write (no wrap corruption).
        mbc.ram_bank_or_rtc_reg = 0x00;
        assert_eq!(mbc.read_ram_or_rtc(0xA000), 0x00, "no wrap-around corruption");
    }
}
