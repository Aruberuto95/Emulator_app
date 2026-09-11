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
                // Y: the exact inverse of the calibration pair the firmware
                // settings advertise (screen 24 -> 608, screen 168 -> 3488),
                // i.e. 20 ADC counts per pixel with a 128-count offset. The
                // whole 192-pixel panel lands in 128..3948, so the clamp below
                // never fires for an on-screen touch. See `build_user_settings`.
                let raw_y = self.touch_y as i32 * 20 + 128;
                std::cmp::max(0, std::cmp::min(4095, raw_y)) as u16
            }
            5 => {
                // X: same construction, 15 counts per pixel (screen 32 -> 608,
                // screen 224 -> 3488); the 256-pixel panel lands in 128..3953.
                let raw_x = self.touch_x as i32 * 15 + 128;
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
/// Firmware flash size. Every retail DS carries 256 KB, and the image is built
/// eagerly (see the `Default` impl) so `data.len()` is a device invariant rather
/// than a function of how far the machine has run.
pub const FIRMWARE_BYTES: usize = 0x40000;

#[derive(Clone, Debug)]
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

impl Default for FirmwareState {
    /// The image is built here rather than on first access.
    ///
    /// It used to be lazy, with `data.is_empty()` doubling as an
    /// "uninitialised" flag — which made the chip's size depend on whether the
    /// machine had touched it yet. A snapshot cannot express that: a state saved
    /// after boot restored into a fresh machine (or the reverse) mismatched on
    /// region size, and zero-filling to match would have destroyed the coherent
    /// header the settings path depends on.
    fn default() -> Self {
        Self {
            cmd: None,
            idx: 0,
            addr: 0,
            wel: false,
            data: Self::initial_image(),
            wip_reads: 0,
            log_on: false,
            read_log: Vec::new(),
        }
    }
}

impl FirmwareState {
    pub fn new() -> Self {
        Self::default()
    }

    /// A minimal COHERENT 256 KB firmware image, not just erased 0xFF. Games
    /// read the header's user-settings offset ([0x20], units of 8 bytes) to
    /// locate where to read/write their persisted settings — with an erased
    /// header SoulSilver computed a bogus address (0x7FBF8) and re-verified its
    /// settings write forever, blocking the sound-init handshake that gates all
    /// graphics setup. Real 256 KB units use 0x7FC0 -> user settings at 0x3FE00.
    fn initial_image() -> Vec<u8> {
        let mut d = vec![0xFF; FIRMWARE_BYTES];
        d[0x20] = 0xC0; // user-settings offset /8, little-endian: 0x7FC0
        d[0x21] = 0x7F;
        let settings = crate::nds::hle::build_user_settings();
        d[0x3FE00..0x3FE00 + settings.len()].copy_from_slice(&settings);
        d
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

    /// The screen->ADC transform must be the exact inverse of the calibration
    /// pair the firmware advertises, and must never saturate for an on-screen
    /// touch.
    ///
    /// This pins the pair together: `build_user_settings` publishes
    /// (adc_x1 608 @ x 32, adc_x2 3488 @ x 224) and (adc_y1 608 @ y 24,
    /// adc_y2 3488 @ y 168); the SDK inverts exactly that to turn a pen sample
    /// back into a pixel. The previous constants implied 18 ADC counts per pixel
    /// in X and 22.5 in Y, needing 4608 and 4320 counts out of a 12-bit (4096)
    /// converter — so the clamp folded the leftmost 16 columns onto one x, the
    /// rightmost 13 onto another, and 6 top / 4 bottom rows likewise. A control
    /// in the corner of the touch screen could not be pressed at all.
    #[test]
    fn touch_transform_round_trips_without_saturating() {
        // Same constants as `hle::build_user_settings`; a change there without a
        // matching change here is exactly the desync this test exists to catch.
        const ADC_X1: i32 = 608;
        const SCR_X1: i32 = 32;
        const ADC_X2: i32 = 3488;
        const SCR_X2: i32 = 224;
        const ADC_Y1: i32 = 608;
        const SCR_Y1: i32 = 24;
        const ADC_Y2: i32 = 3488;
        const SCR_Y2: i32 = 168;

        let mut tsc = TouchScreenController::new();
        tsc.touch_pressed = true;
        for x in 0..256u16 {
            tsc.touch_x = x;
            tsc.current_channel = 5;
            let adc = tsc.calculate_result() as i32;
            assert!(
                (1..4095).contains(&adc),
                "x={x} produced ADC {adc}, at or past the converter's rail"
            );
            // The SDK's inverse, as it would compute it from the published pair.
            let back = SCR_X1 + (adc - ADC_X1) * (SCR_X2 - SCR_X1) / (ADC_X2 - ADC_X1);
            assert_eq!(back, i32::from(x), "x={x} round-tripped to {back}");
        }
        for y in 0..192u16 {
            tsc.touch_y = y;
            tsc.current_channel = 1;
            let adc = tsc.calculate_result() as i32;
            assert!(
                (1..4095).contains(&adc),
                "y={y} produced ADC {adc}, at or past the converter's rail"
            );
            let back = SCR_Y1 + (adc - ADC_Y1) * (SCR_Y2 - SCR_Y1) / (ADC_Y2 - ADC_Y1);
            assert_eq!(back, i32::from(y), "y={y} round-tripped to {back}");
        }
    }

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
        assert_eq!(adc, 128 * 15 + 128, "X ADC per calibration");

        // Switching channel mid-pipeline latches the new channel: the byte
        // after the overlapped 0x91 control must carry the Y result.
        let _lo = tsc.process_byte(0x91); // Y control rides the low-bits read
        let hi_y = tsc.process_byte(0x00);
        let lo_y = tsc.process_byte(0x00);
        let adc_y = ((hi_y as u16) << 5) | ((lo_y as u16) >> 3);
        assert_eq!(adc_y, 96 * 20 + 128, "Y ADC per calibration");
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
        assert_eq!(((hi as u16) << 5) | ((lo as u16) >> 3), 128 * 15 + 128);
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

// ---------------------------------------------------------------------------
// Snapshot support (see `crate::snapshot`). Device state machines are restored
// in the exact phase they were suspended in: a controller resumed at the wrong
// byte of a transaction desynchronises the driver talking to it.
// ---------------------------------------------------------------------------

use crate::snapshot::{snap_bytes, snap_enum};

impl crate::snapshot::Snap for TouchScreenController {
    /// `conv_counts` and the io log are probe instrumentation, not state.
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.touch_x.snap(v);
        self.touch_y.snap(v);
        self.touch_pressed.snap(v);
        snap_enum(
            v,
            &mut self.state,
            |s| match s {
                TscState::ExpectControl => 0,
                TscState::ExpectData => 1,
            },
            |i| match i {
                0 => Some(TscState::ExpectControl),
                1 => Some(TscState::ExpectData),
                _ => None,
            },
        );
        self.current_channel.snap(v);
        self.mode_8bit.snap(v);
        self.result.snap(v);
        self.byte_count.snap(v);
    }
}

impl crate::snapshot::Snap for PmicState {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.control.snap(v);
        self.reg_addr.snap(v);
        self.is_read.snap(v);
        snap_enum(
            v,
            &mut self.state,
            |s| match s {
                PmicStateEnum::ExpectCommand => 0,
                PmicStateEnum::ExpectData => 1,
            },
            |i| match i {
                0 => Some(PmicStateEnum::ExpectCommand),
                1 => Some(PmicStateEnum::ExpectData),
                _ => None,
            },
        );
    }
}

impl crate::snapshot::Snap for FirmwareState {
    /// The firmware image itself is included: the console's user settings live
    /// in it and the ARM7 rewrites them, so a snapshot that dropped it would
    /// resume with different calibration than the running game believes in.
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.cmd.snap(v);
        self.idx.snap(v);
        self.addr.snap(v);
        self.wel.snap(v);
        snap_bytes(v, &mut self.data, FIRMWARE_BYTES, "firmware image size mismatch");
        self.wip_reads.snap(v);
    }
}

impl crate::snapshot::Snap for SpiController {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.spicnt.snap(v);
        self.spidata.snap(v);
        self.tsc.snap(v);
        self.pmic.snap(v);
        self.firmware.snap(v);
    }
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
        // Writable bits: Baudrate (0-1), Device Select (8-9), Transfer Size (10),
        // Chipselect Hold (11), IRQ (14), Enable (15). Size and hold are easy to
        // transpose; `write_spidata` documents what each one actually does here.
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
