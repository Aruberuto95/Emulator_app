// core/src/nds/spi.rs

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TscState {
    ExpectControl,
    ExpectData,
}

impl Default for TscState {
    fn default() -> Self {
        TscState::ExpectControl
    }
}

#[derive(Clone, Debug, Default)]
pub struct TouchScreenController {
    // Current coordinate inputs from the frontend
    pub touch_x: u16,
    pub touch_y: u16,
    pub touch_pressed: bool,

    // TSC internal state machine
    pub state: TscState,
    pub current_channel: u8,
    pub mode_8bit: bool,
    pub result: u16,
    pub byte_count: usize,

    /// Conversions started per channel since boot — diagnostic evidence for
    /// the touch bring-up (does the ARM7 driver sample the TSC at all, and
    /// which channels?). Cheap enough to keep always-on.
    pub conv_counts: [u32; 8],

    /// Raw transaction log (byte in -> byte out), captured only while
    /// `io_log_on` (the probe arms it around a tap) — U26 evidence for the
    /// dead-UI-buttons hunt: shows the exact command bytes the ARM7 driver
    /// sends during a tap and what we answer. Bounded at 4096 entries.
    pub io_log: Vec<(u8, u8)>,
    pub io_log_on: bool,
}

impl TouchScreenController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn process_byte(&mut self, val: u8) -> u8 {
        let out = self.process_byte_inner(val);
        if self.io_log_on && self.io_log.len() < 4096 {
            self.io_log.push((val, out));
        }
        out
    }

    /// Latch a control byte (start bit set): select the channel, run the
    /// conversion, and enter the data phase.
    fn latch_control(&mut self, val: u8) {
        self.current_channel = (val >> 4) & 0x7;
        self.conv_counts[self.current_channel as usize] =
            self.conv_counts[self.current_channel as usize].wrapping_add(1);
        self.mode_8bit = (val & 0x08) != 0;
        self.byte_count = 0;
        self.state = TscState::ExpectData;
        self.result = self.calculate_result();
    }

    fn process_byte_inner(&mut self, val: u8) -> u8 {
        match self.state {
            TscState::ExpectControl => {
                // Control byte format: bit 7 = start bit.
                if (val & 0x80) != 0 {
                    self.latch_control(val);
                }
                0 // Return 0 during control byte write
            }
            TscState::ExpectData => {
                let response = if self.mode_8bit {
                    // 8-bit mode: only 1 data byte is returned
                    if self.byte_count == 0 {
                        self.byte_count += 1;
                        self.state = TscState::ExpectControl; // Done
                        ((self.result >> 4) & 0xFF) as u8
                    } else {
                        0
                    }
                } else {
                    // 12-bit mode: 2 data bytes are returned (MSB first)
                    // Byte 0: upper bits (result >> 5) & 0x7F
                    // Byte 1: lower bits (result << 3) & 0xF8
                    if self.byte_count == 0 {
                        self.byte_count += 1;
                        ((self.result >> 5) & 0x7F) as u8
                    } else if self.byte_count == 1 {
                        self.byte_count += 1;
                        self.state = TscState::ExpectControl; // Done after 2 data bytes
                        ((self.result << 3) & 0xF8) as u8
                    } else {
                        0
                    }
                };
                // TSC2046 OVERLAPPED conversions: the host may clock the NEXT
                // control byte while reading the current result's data bytes
                // (SoulSilver's ARM7 driver pipelines its 5x X / 5x Y bursts
                // as d1,00,d1,00,... — the control rides on the low-bits
                // read). Treating it as plain data made every SECOND
                // conversion return 0, so the SDK's chattering filter
                // rejected the whole burst and UI touch buttons never fired.
                // The response above still carries the PREVIOUS conversion's
                // bits; the new control takes effect for the next byte.
                if (val & 0x80) != 0 {
                    self.latch_control(val);
                }
                response
            }
        }
    }

    pub fn calculate_result(&self) -> u16 {
        if !self.touch_pressed {
            // Pen-up signature (GBATEK, TSC2046 with the panel open): the Y
            // position input is pulled to the rail by the PENIRQ pull-up and
            // reads 0xFFF (Z2 likewise); X and Z1 float low. The SDK driver
            // keeps sampling ~12 frames past pen-up (measured U27: 840
            // conversions for 80 pressed frames) and keys its pen-up/validity
            // detection on this signature. Returning a "clean" 0 for Y made
            // those post-release samples decode as a VALID touch at the
            // screen corner — the stroke "slid off" the button, so UI
            // buttons, which fire on release-inside-button, cancelled
            // instead of firing (U27 root cause; title/menu scenes fire on
            // pen-DOWN and never noticed).
            return match self.current_channel {
                1 | 4 => 0xFFF, // Y position, Z2
                _ => 0,         // X position, Z1, aux/temp
            };
        }

        match self.current_channel {
            1 => {
                // Y-coordinate
                // Screen Y1 = 48, ADC Y1 = 960
                // Screen Y2 = 144, ADC Y2 = 3120
                // raw_y = (touch_y - 48) * 225 / 10 + 960
                let raw_y = (self.touch_y as i32 - 48) * 225 / 10 + 960;
                std::cmp::max(0, std::cmp::min(4095, raw_y)) as u16
            }
            5 => {
                // X-coordinate
                // Screen X1 = 64, ADC X1 = 880
                // Screen X2 = 192, ADC X2 = 3184
                // raw_x = (touch_x - 64) * 18 + 880
                let raw_x = (self.touch_x as i32 - 64) * 18 + 880;
                std::cmp::max(0, std::cmp::min(4095, raw_x)) as u16
            }
            3 => {
                // Z1 (touch pressure measurement)
                100
            }
            4 => {
                // Z2 (touch pressure measurement)
                200
            }
            _ => 0, // Other channels (temperature, aux, etc.)
        }
    }

    pub fn reset_state(&mut self) {
        self.state = TscState::ExpectControl;
        self.byte_count = 0;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PmicStateEnum {
    ExpectCommand,
    ExpectData,
}

impl Default for PmicStateEnum {
    fn default() -> Self {
        PmicStateEnum::ExpectCommand
    }
}

#[derive(Clone, Debug, Default)]
pub struct PmicState {
    pub control: u8,
    pub reg_addr: u8,
    pub is_read: bool,
    pub state: PmicStateEnum,
}

impl PmicState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn process_byte(&mut self, val: u8) -> u8 {
        match self.state {
            PmicStateEnum::ExpectCommand => {
                self.is_read = (val & 0x80) != 0;
                self.reg_addr = val & 0x7F;
                self.state = PmicStateEnum::ExpectData;
                0
            }
            PmicStateEnum::ExpectData => {
                self.state = PmicStateEnum::ExpectCommand;
                if self.is_read {
                    match self.reg_addr {
                        0 => 0x03, // Brightness/backlight status (both screens on)
                        1 => 0x00, // Battery status (0 = full battery)
                        _ => 0,
                    }
                } else {
                    // PMIC Write register
                    0
                }
            }
        }
    }
}

/// Firmware serial flash (SPI device 1), minimal command machine.
///
/// The ARM7 sound/settings code polls the flash STATUS register (RDSR, 0x05)
/// and requires bit 0 (Write-In-Progress) to be 0 before proceeding — the old
/// always-0xFF stub reported "busy forever", which propagated all the way up
/// to SoulSilver's ARM9 sound-init handshake retrying eternally and blocking
/// boot before any graphics setup. Writes complete instantly here, so WIP is
/// always 0; the WEL latch (WREN/WRDI) is tracked for drivers that verify it.
#[derive(Clone, Debug, Default)]
pub struct FirmwareState {
    /// Command byte of the current chip-select transaction (`None` = the next
    /// byte is a command).
    cmd: Option<u8>,
    /// Payload bytes consumed since the command byte (address phase tracking).
    idx: u32,
    /// 24-bit read cursor for command 0x03.
    addr: u32,
    /// Write-enable latch: set by WREN (0x06), cleared by WRDI (0x04).
    wel: bool,
    /// Flash contents (256 KB, lazily initialised to erased 0xFF). The driver
    /// VERIFIES its writes by reading them back — a store-less flash makes
    /// every write "fail" verification and retries forever.
    data: Vec<u8>,
    /// Remaining RDSR reads that still report an in-flight write (WIP=1).
    /// Real flash keeps WIP set for a few ms after a program command; the
    /// driver's init sequence must OBSERVE that in-flight state (it advances
    /// on seeing WEL/WIP set, then completes on WIP clear) — an instantly
    /// finished write is invisible and stalls it forever.
    wip_reads: u8,
    /// U27p probe evidence: log the start address of each READ (cmd 0x03)
    /// transaction while armed — shows which firmware pages the ARM7's boot
    /// actually fetches (bounded).
    pub log_on: bool,
    pub read_log: Vec<(u32, u8)>,
}

impl FirmwareState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Lazy backing-store init: a minimal COHERENT 256 KB firmware image, not
    /// just erased 0xFF. Games read the header's user-settings offset
    /// ([0x20], units of 8 bytes) to locate where to read/write their
    /// persisted settings — with an erased header SoulSilver computed a
    /// bogus address (0x7FBF8) and re-verified its settings write forever,
    /// blocking the sound-init handshake that gates all graphics setup.
    /// Real 256 KB units use 0x7FC0 -> user settings at 0x3FE00.
    fn ensure_data(&mut self) {
        if self.data.is_empty() {
            let mut d = vec![0xFF; 0x40000];
            d[0x20] = 0xC0; // user-settings offset /8, little-endian: 0x7FC0
            d[0x21] = 0x7F;
            let settings = crate::nds::hle::build_user_settings();
            d[0x3FE00..0x3FE00 + settings.len()].copy_from_slice(&settings);
            self.data = d;
        }
    }

    /// Chip-select release: a new transaction starts with a command byte.
    /// A completed write/program command starts the simulated busy window;
    /// WEL auto-clears when that window ends (see `process_byte` RDSR).
    pub fn reset_state(&mut self) {
        if matches!(self.cmd, Some(0x02) | Some(0x0A) | Some(0x01) | Some(0xDB)) {
            self.wip_reads = 2;
        }
        self.cmd = None;
        self.idx = 0;
        self.addr = 0;
    }

    pub fn process_byte(&mut self, val: u8) -> u8 {
        match self.cmd {
            None => {
                self.cmd = Some(val);
                match val {
                    0x06 => self.wel = true,  // WREN
                    0x04 => self.wel = false, // WRDI
                    _ => {}
                }
                0x00
            }
            // RDSR: bit 0 = WIP (busy for `wip_reads` polls after a write),
            // bit 1 = WEL (cleared when the write window completes).
            Some(0x05) => {
                let mut status = 0u8;
                if self.wip_reads > 0 {
                    status |= 0x01;
                    if self.wel {
                        status |= 0x02;
                    }
                    self.wip_reads -= 1;
                    if self.wip_reads == 0 {
                        self.wel = false; // write completed: WEL auto-clears
                    }
                } else if self.wel {
                    status |= 0x02;
                }
                status
            }
            // READ: 3 address bytes, then data streamed from the backing
            // store. We ship no real firmware image (the user-settings block
            // games consume is HLE'd into main RAM at boot); unwritten bytes
            // read as erased 0xFF, written bytes read back exactly.
            Some(0x03) => {
                self.idx += 1;
                if self.idx <= 3 {
                    self.addr = (self.addr << 8) | val as u32;
                    0x00
                } else {
                    self.ensure_data();
                    let b0 = self.data[(self.addr as usize) & 0x3FFFF];
                    if self.idx == 4 && self.log_on && self.read_log.len() < 48 {
                        self.read_log.push((self.addr, b0));
                    }
                    let b = b0;
                    self.addr = self.addr.wrapping_add(1);
                    b
                }
            }
            // Page program / write (0x02 / 0x0A): 3 address bytes, then data
            // bytes stored to the backing store.
            Some(0x02) | Some(0x0A) => {
                self.idx += 1;
                if self.idx <= 3 {
                    self.addr = (self.addr << 8) | val as u32;
                } else {
                    self.ensure_data();
                    self.data[(self.addr as usize) & 0x3FFFF] = val;
                    self.addr = self.addr.wrapping_add(1);
                }
                0x00
            }
            Some(_) => 0x00,
        }
    }
}

#[cfg(test)]
mod tsc_tests {
    use super::*;

    /// SoulSilver's ARM7 touch driver pipelines conversions TSC2046-style:
    /// after the first control byte, every low-bits data byte carries the
    /// NEXT control byte (d1,00,d1,00,...). Every conversion in the burst
    /// must return the same ADC value — the old state machine answered 0 for
    /// every second one, so the SDK chattering filter rejected the burst and
    /// UI touch buttons never reacted (U26 root cause).
    #[test]
    fn tsc_overlapped_pipelined_conversions_repeat_result() {
        let mut tsc = TouchScreenController::new();
        tsc.touch_x = 128;
        tsc.touch_y = 96;
        tsc.touch_pressed = true;

        assert_eq!(tsc.process_byte(0xD1), 0); // X, differential, 12-bit
        let hi1 = tsc.process_byte(0x00);
        let lo1 = tsc.process_byte(0xD1); // low bits out + next control in
        let hi2 = tsc.process_byte(0x00);
        let lo2 = tsc.process_byte(0xD1);
        let hi3 = tsc.process_byte(0x00);
        assert_eq!((hi2, lo2), (hi1, lo1), "conversion 2 must repeat, not zero");
        assert_eq!(hi3, hi1, "conversion 3 must repeat, not zero");
        let adc = ((hi1 as u16) << 5) | ((lo1 as u16) >> 3);
        assert_eq!(adc, (128 - 64) * 18 + 880, "X ADC per calibration");

        // Switching channel mid-pipeline latches the new channel: the byte
        // after the overlapped 0x91 control must carry the Y result.
        let _lo = tsc.process_byte(0x91); // Y control rides the low-bits read
        let hi_y = tsc.process_byte(0x00);
        let lo_y = tsc.process_byte(0x00);
        let adc_y = ((hi_y as u16) << 5) | ((lo_y as u16) >> 3);
        assert_eq!(adc_y, (96 - 48) * 225 / 10 + 960, "Y ADC per calibration");
    }

    /// UI buttons fire on pen-RELEASE-inside-button. The driver samples for
    /// ~12 frames past pen-up; those conversions must read the open-panel
    /// rail signature (Y=0xFFF, X=0) so the SDK flags them as pen-up rather
    /// than decoding a valid corner touch that cancels the press (U27).
    #[test]
    fn tsc_pen_up_conversions_read_rail_signature() {
        let mut tsc = TouchScreenController::new();
        tsc.touch_pressed = false;
        tsc.process_byte(0x91); // Y position conversion
        let hi = tsc.process_byte(0x00);
        let lo = tsc.process_byte(0x00);
        assert_eq!(((hi as u16) << 5) | ((lo as u16) >> 3), 0xFFF, "Y rails high");
        tsc.process_byte(0xD1); // X position conversion
        let hi = tsc.process_byte(0x00);
        assert_eq!(hi, 0, "X floats low");
        // Pen back down: position conversions resume real coordinates.
        tsc.touch_x = 128;
        tsc.touch_y = 96;
        tsc.touch_pressed = true;
        tsc.process_byte(0x00); // drain pending data byte
        tsc.process_byte(0xD1);
        let hi = tsc.process_byte(0x00);
        let lo = tsc.process_byte(0x00);
        assert_eq!(
            ((hi as u16) << 5) | ((lo as u16) >> 3),
            (128 - 64) * 18 + 880
        );
    }
}

#[derive(Clone, Debug, Default)]
pub struct SpiController {
    pub spicnt: u16,
    pub spidata: u16,
    pub tsc: TouchScreenController,
    pub pmic: PmicState,
    pub firmware: FirmwareState,
}

impl SpiController {
    pub fn new() -> Self {
        Self {
            firmware: FirmwareState::new(),
            ..Self::default()
        }
    }

    pub fn read_spicnt(&self) -> u16 {
        self.spicnt
    }

    pub fn write_spicnt(&mut self, val: u16) {
        // Writable bits: Baudrate (0-1), CS Select (8-9), CS Hold (10), Trans Size (11), IRQ (14), Enable (15)
        self.spicnt = val & 0xCF03;

        // If SPI disabled, reset device state machines
        if (self.spicnt & 0x8000) == 0 {
            self.tsc.reset_state();
            self.pmic.state = PmicStateEnum::ExpectCommand;
            self.firmware.reset_state();
        }
    }

    pub fn read_spidata(&self) -> u16 {
        self.spidata
    }

    pub fn write_spidata(&mut self, val: u16) {
        if (self.spicnt & 0x8000) == 0 {
            return;
        }

        let device = (self.spicnt >> 8) & 3;

        // Always an 8-bit transfer: SPICNT bit 11 ("16-bit size") is bugged /
        // non-functional on retail NDS hardware — devices clock one byte per
        // SPIDATA write regardless. SoulSilver's ARM7 firmware-flash reads set
        // bit 11; honoring it as 16-bit bypassed the device state machines and
        // returned 0xFFFF, so the flash STATUS byte (the sound-init gate the
        // ARM9 polls at [0x021d4400]) was never produced.
        let input_byte = (val & 0xFF) as u8;
        let output_byte = match device {
            0 => self.pmic.process_byte(input_byte),
            1 => self.firmware.process_byte(input_byte),
            2 => self.tsc.process_byte(input_byte),
            _ => 0xFF,
        };
        self.spidata = output_byte as u16;

        // Release chip-select when CS-Hold is clear. On NDS7 SPICNT the hold
        // bit is BIT 11 (bit 10 is the bugged "transfer size"): the firmware
        // driver writes 0x8900 (hold) for every byte of a multi-byte command
        // and 0x8100 for the final one. Testing bit 10 released CS after
        // every byte, so address phases never accumulated and every flash
        // command decoded as a fresh single-byte command.
        if (self.spicnt & 0x0800) == 0 {
            self.tsc.reset_state();
            self.pmic.state = PmicStateEnum::ExpectCommand;
            self.firmware.reset_state();
        }
    }
}
