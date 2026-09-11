//! Nintendo DS cartridge backup chip (AUXSPI bus, 0x040001A0/0x040001A2).
//!
//! This is the *save* chip, not the console's firmware flash. It lives on its
//! own bus with its own framing, which is why it is a separate module from
//! [`crate::nds::spi`]:
//!
//! * The firmware/touchscreen/PMIC bus is 0x040001C0/C2 with a 2-bit device
//!   select and chip-select-hold on **bit 11**; AUXSPI is a single device with
//!   chip-select-hold on **bit 6**. Folding them together would mean branching
//!   every method on which bus, on a bit-numbering difference that has already
//!   produced one hard-won bug (see the comment on `SpiController::write_spicnt`).
//! * `NdsMmu::reset()` recreates `SpiController` on every ROM load and reset.
//!   A backup living inside it would erase the player's save every soft reset.
//!   `backup` is therefore a sibling field that `reset()` deliberately skips,
//!   the same treatment `rom` gets.
//!
//! The command set is FLASH, not EEPROM. That is measured, not assumed: the
//! SoulSilver ARM7 static binary's AUXSPI driver (literal pool at ARM7+0x7C24
//! for 0x040001A0, +0x7C38/+0x7C78/+0x7CC8/+0x7D2C for 0x040001A2) issues
//! `RDSR 0x05`, `WREN 0x06`, `PW 0x0A` and `SE 0xD8`. Page-write and
//! sector-erase exist in no SPI EEPROM command set.
//!
//! The driver reads back and verifies every byte it writes, setting a failure
//! flag on mismatch — so a chip without a real backing store fails every save.
//!
//! ponytail: the parallel-bus `crate::gba::flash::Flash128` is deliberately NOT
//! reused here. It is an AMD/Sanyo device driven by unlock cycles
//! (`0xAA@0x5555` / `0x55@0x2AAA`) with no command byte, no address phase, no
//! status register and no chip select; none of its states map onto SPI framing.
//! Only its on-disk persistence *shape* is copied. Ceiling: two save backends.
//! Upgrade path: a shared `BatteryBacked` trait over `data`/`dirty`/`sav` I/O
//! once a third console needs one.

use crate::snapshot::{snap_bytes, snap_enum};
use std::path::Path;

/// Hard ceiling on any backup allocation, independent of the ROM and of any
/// `.sav` found on disk. NDS headers carry no save-size field (byte 0x14 is
/// *ROM* capacity), so nothing attacker-controlled may size this buffer.
pub const MAX_BACKUP_BYTES: usize = 1024 * 1024;

/// Bytes per program page. `PW`/`PP` addresses wrap within the page.
const PAGE_BYTES: u32 = 256;
/// Bytes per erase sector (`SE`).
const SECTOR_BYTES: u32 = 64 * 1024;

/// RDSR reads that keep reporting an in-flight write after a program or erase.
///
/// A real chip holds WIP for milliseconds and SDK drivers poll for the
/// transition. An instantly-finished write is invisible to a driver that
/// advances on *seeing* the busy state, which stalls it forever — the exact
/// failure already documented for the firmware flash. Two polls is the
/// smallest window that is observable.
const WIP_POLLS: u8 = 2;

/// Which backup device the cartridge carries. All variants share the FLASH
/// command set and differ only in size and JEDEC capacity code.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BackupKind {
    /// No chip wired (e.g. a ROM loaded from memory with no path to persist to).
    #[default]
    None,
    /// 2 Mbit / 256 KiB (ST M45PE20).
    Flash256K,
    /// 4 Mbit / 512 KiB (ST M45PE40) — Pokemon generation 4, incl. `IPGE`.
    Flash512K,
    /// 8 Mbit / 1 MiB (ST M45PE80).
    Flash1M,
}

impl BackupKind {
    pub const fn size_bytes(self) -> usize {
        match self {
            BackupKind::None => 0,
            BackupKind::Flash256K => 256 * 1024,
            BackupKind::Flash512K => 512 * 1024,
            BackupKind::Flash1M => 1024 * 1024,
        }
    }

    /// Third RDID byte (JEDEC capacity code) for the M45PExx family.
    const fn jedec_capacity(self) -> u8 {
        match self {
            BackupKind::None => 0x00,
            BackupKind::Flash256K => 0x12,
            BackupKind::Flash512K => 0x13,
            BackupKind::Flash1M => 0x14,
        }
    }
}

/// Pick the backup device from the 4-byte cartridge gamecode at header 0x0C.
///
/// NDS headers carry no save-type field and SoulSilver's driver never issues
/// RDID (the ARM9 declares the type via `CARD_IdentifyBackup`), so the type
/// cannot be probed — it has to come from a table.
///
/// The default is deliberately the *larger* plausible device: over-sizing is
/// inert (the driver never addresses past the size it believes in, and unused
/// pages stay erased), while under-sizing silently truncates a player's save.
///
/// ponytail: a two-entry table, not a real per-title database. Ceiling: any
/// cartridge whose backup is larger than 512 KiB, or an EEPROM title that would
/// want the other command set. Upgrade path: auto-size from the highest address
/// the driver actually puts on the bus (the W0.1 census already measures it),
/// or bundle a gamecode database.
pub fn backup_kind_for_gamecode(_gamecode: &[u8]) -> BackupKind {
    // Every gamecode currently resolves to the same device, so there is no
    // table to consult — writing one whose arms all return the same value would
    // be dead code that no test could fail. The parameter is kept because the
    // upgrade path is per-title, not global.
    BackupKind::Flash512K
}

/// Census slot names, parallel to [`NdsBackup::cmd_counts`].
pub const CMD_LABELS: [&str; 8] =
    ["READ", "RDSR", "WREN", "WRDI", "PP", "PW", "SE", "other"];

/// Which census slot an opcode belongs to. Kept next to [`CMD_LABELS`] so the
/// two cannot drift.
fn cmd_slot(cmd: u8) -> usize {
    match cmd {
        0x03 => 0, // READ
        0x05 => 1, // RDSR
        0x06 => 2, // WREN
        0x04 => 3, // WRDI
        0x02 => 4, // PP  (page program)
        0x0A => 5, // PW  (page write)
        0xD8 => 6, // SE  (sector erase)
        _ => 7,
    }
}

/// SPI FLASH save chip on the AUXSPI bus.
#[derive(Clone, Default)]
pub struct NdsBackup {
    kind: BackupKind,
    /// Backing store, sized from `kind` only. Erased state is 0xFF.
    data: Vec<u8>,
    /// Set by any mutation; cleared once the contents reach disk.
    dirty: bool,
    /// Command byte of the transaction in flight, `None` between transactions.
    cmd: Option<u8>,
    /// Bytes clocked since the command byte (1 = first address byte).
    idx: u32,
    /// Address accumulated from the 3-byte address phase, then auto-incremented.
    addr: u32,
    /// Write-enable latch: set by WREN (0x06), cleared by WRDI (0x04) and when
    /// a program/erase completes.
    wel: bool,
    /// Remaining RDSR reads that still report WIP set.
    wip_polls: u8,
    /// Legacy snapshot field. PW preserves unaddressed bytes; no erase tracking
    /// is needed, but retaining the byte keeps existing binary states readable.
    page_erased: bool,
    /// Whether the transaction in flight actually performed a program or erase.
    /// The busy window and the write-enable latch both key off this rather than
    /// off the opcode: a program clocked with WEL clear does nothing, and a chip
    /// that reported itself busy for it would be lying.
    launched: bool,
    /// Last byte the chip put on the bus, latched for AUXSPIDATA reads.
    pub last_out: u8,
    /// Commands the driver has actually put on the bus, one slot per opcode this
    /// family uses (see [`CMD_LABELS`] / [`cmd_slot`]). A boot-only run shows
    /// READ and RDSR; an in-game save must additionally show WREN and a program,
    /// which is how "the game saved" is told from "the game only read".
    pub cmd_counts: [u32; 8],
}

impl NdsBackup {
    pub fn new(kind: BackupKind) -> Self {
        let size = kind.size_bytes().min(MAX_BACKUP_BYTES);
        Self {
            kind,
            data: vec![0xFF; size],
            ..Default::default()
        }
    }

    pub fn kind(&self) -> BackupKind {
        self.kind
    }

    pub fn is_present(&self) -> bool {
        !self.data.is_empty()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = self.is_present();
    }

    /// Read-only view of the backing store (savestates, tests, diagnostics).
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Chip-select release. A program or erase that actually ran opens the busy
    /// window here and consumes the write-enable latch; the next data byte
    /// starts a fresh command.
    ///
    /// WEL is cleared here rather than when the busy window expires so that a
    /// driver which never polls RDSR cannot leave the latch set forever and
    /// program again without an intervening WREN.
    pub fn deselect(&mut self) {
        if self.launched {
            self.wip_polls = WIP_POLLS;
            self.wel = false;
        }
        self.cmd = None;
        self.idx = 0;
        self.addr = 0;
        self.page_erased = false;
        self.launched = false;
    }

    /// Clock one byte through the chip and return the byte it drives back.
    pub fn transfer(&mut self, val: u8) -> u8 {
        if self.data.is_empty() {
            // No chip on this cartridge: the bus floats to the pulled-up 0xFF
            // an absent/erased device presents.
            self.last_out = 0xFF;
            return 0xFF;
        }
        let out = match self.cmd {
            // 0x00 is not a command on this family, and SoulSilver's driver
            // really does clock one: 12 of its 36 measured status frames are
            // [00 05 00] rather than [05 00], because it changes baud rate and
            // clocks a settling byte while chip-select is still held. Latching
            // that 0x00 as the opcode would swallow the RDSR behind it and the
            // driver would read a constant 0x00 status — which reads as "never
            // busy", the exact condition that stalls a driver waiting to observe
            // a program complete. Consuming it as a no-op keeps the frame's real
            // command intact.
            None if val == 0x00 => 0x00,
            None => {
                self.cmd = Some(val);
                let slot = cmd_slot(val);
                self.cmd_counts[slot] = self.cmd_counts[slot].saturating_add(1);
                self.idx = 0;
                self.addr = 0;
                self.page_erased = false;
                match val {
                    0x06 => self.wel = true,  // WREN
                    0x04 => self.wel = false, // WRDI
                    _ => {}
                }
                0x00
            }
            // RDSR: bit 0 = WIP, bit 1 = WEL. Repeats while CS is held.
            Some(0x05) => {
                let mut status = 0u8;
                if self.wip_polls > 0 {
                    status |= 0x01;
                    self.wip_polls -= 1;
                }
                if self.wel {
                    status |= 0x02;
                }
                status
            }
            // WRSR: accepts a status byte. Block-protect bits are not modelled.
            // ponytail: writes are never refused by protection. Ceiling: a game
            // that relies on BP to guard a region. Upgrade: honour BP0-BP2 in
            // the program/erase address check.
            Some(0x01) => 0x00,
            // RDID: 3-byte JEDEC identity. SoulSilver never issues this (its
            // type comes from the ARM9), but other titles probe with it.
            Some(0x9F) => {
                self.idx += 1;
                match self.idx {
                    1 => 0x20, // Manufacturer: ST
                    2 => 0x40, // Memory type: M45PExx
                    3 => self.kind.jedec_capacity(),
                    _ => 0x00,
                }
            }
            // READ: 3 address bytes, then a continuous stream that wraps at the
            // end of the device.
            Some(0x03) => {
                self.idx += 1;
                if self.idx <= 3 {
                    self.addr = (self.addr << 8) | val as u32;
                    0x00
                } else {
                    let b = self.read_at(self.addr);
                    self.addr = self.addr.wrapping_add(1);
                    b
                }
            }
            // PW (page write) and PP (page program): 3 address bytes, then data.
            // PW replaces addressed bytes and preserves the rest of the page;
            // the M45PE40 internally merges old bytes before erase/program.
            // PP only clears bits. Both wrap within the 256-byte page.
            Some(cmd @ (0x0A | 0x02)) => {
                self.idx += 1;
                if self.idx <= 3 {
                    self.addr = (self.addr << 8) | val as u32;
                } else if self.wel {
                    self.program_at(self.addr, val, cmd == 0x0A);
                    self.launched = true;
                    let page_base = self.addr - self.addr % PAGE_BYTES;
                    self.addr = page_base + (self.addr + 1) % PAGE_BYTES;
                }
                0x00
            }
            // PE (page erase, 256 B) and SE (sector erase, 64 KiB), executed on
            // the third address byte.
            //
            // ponytail: a real chip launches the erase on chip-select release
            // and rejects the command outright if the byte count since the
            // opcode is not exactly 3. Ceiling: a driver that clocks a trailing
            // byte before releasing would see this model erase where hardware
            // refuses. SoulSilver's address helper passes exactly `addrsize + 1`
            // iterations (ARM7+0x7DE8), so it cannot reach that case. Upgrade
            // path: latch a pending erase and run it in `deselect()` only when
            // `idx == 3` exactly.
            Some(cmd @ (0xDB | 0xD8)) => {
                self.idx += 1;
                if self.idx <= 3 {
                    self.addr = (self.addr << 8) | val as u32;
                    if self.idx == 3 && self.wel {
                        let span = if cmd == 0xDB { PAGE_BYTES } else { SECTOR_BYTES };
                        self.erase_range(self.addr - self.addr % span, span);
                        self.launched = true;
                    }
                }
                0x00
            }
            // Unknown opcode: the chip drives nothing meaningful. Kept silent
            // rather than logged — the W0.1 census already records every byte.
            //
            // ponytail: FAST_READ (0x0B, a READ with a 4-byte address phase),
            // RES (0xAB, electronic signature) and the deep-power-down pair
            // (0xB9/0xAB) are not implemented. Ceiling: a title whose driver
            // probes with them reads 0x00 instead of data. Measured on
            // SoulSilver across a full boot, the only opcodes on the bus were
            // 0x05 and a leading 0x00, so implementing them now would be
            // unevidenced. Upgrade path: add arms here when a census names one.
            Some(_) => 0x00,
        };
        self.last_out = out;
        out
    }

    fn read_at(&self, addr: u32) -> u8 {
        self.data[(addr as usize) % self.data.len()]
    }

    /// Program one byte. FLASH can only clear bits, so `PP` ANDs; `PW` has
    /// internally preserves the rest of the page and stores addressed bytes verbatim.
    fn program_at(&mut self, addr: u32, val: u8, erased: bool) {
        let len = self.data.len();
        let i = (addr as usize) % len;
        let new = if erased { val } else { self.data[i] & val };
        if self.data[i] != new {
            self.data[i] = new;
            self.dirty = true;
        }
    }

    fn erase_range(&mut self, start: u32, len: u32) {
        let size = self.data.len();
        for i in 0..len as usize {
            let idx = (start as usize + i) % size;
            if self.data[idx] != 0xFF {
                self.data[idx] = 0xFF;
                self.dirty = true;
            }
        }
    }

    /// Persist to `<rom>.sav`, atomically. Shares the path rules and the
    /// temp-file protocol with every other battery-backed console.
    pub fn save_to_disk(&mut self, rom_path: &Path, base_dir: &Path) -> Result<(), String> {
        if self.data.is_empty() {
            return Ok(());
        }
        crate::rom::write_battery_file(rom_path, base_dir, &self.data)?;
        self.dirty = false;
        Ok(())
    }

    /// Seed from `<rom>.sav` if one exists. The buffer keeps the length the
    /// chip type dictates — a short file leaves the tail erased and a long one
    /// is ignored past the device end, so neither can resize the device.
    pub fn load_from_disk(&mut self, rom_path: &Path, base_dir: &Path) -> Result<(), String> {
        if self.data.is_empty() {
            return Ok(());
        }
        if crate::rom::read_battery_file(rom_path, base_dir, &mut self.data)? {
            self.dirty = false;
        }
        Ok(())
    }
}

impl crate::snapshot::Snap for NdsBackup {
    /// The chip's contents are part of the state, exactly as cartridge SRAM is
    /// in every other emulator: a savestate taken after an in-game save must
    /// restore that save, not the version currently on disk. `dirty` rides along
    /// so a restore that changed the contents still reaches `.sav`.
    ///
    /// `kind` is snapshotted and validated rather than trusted: it sizes `data`,
    /// so a file naming a different device is refused before anything is
    /// applied.
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        let live = self.kind;
        snap_enum(
            v,
            &mut self.kind,
            |k| match k {
                BackupKind::None => 0,
                BackupKind::Flash256K => 1,
                BackupKind::Flash512K => 2,
                BackupKind::Flash1M => 3,
            },
            |i| match i {
                0 => Some(BackupKind::None),
                1 => Some(BackupKind::Flash256K),
                2 => Some(BackupKind::Flash512K),
                3 => Some(BackupKind::Flash1M),
                _ => None,
            },
        );
        if v.loading() && self.kind != live {
            self.kind = live;
            v.fail("snapshot names a different backup device");
            return;
        }
        let size = self.kind.size_bytes();
        snap_bytes(v, &mut self.data, size, "backup chip size mismatch");
        self.dirty.snap(v);
        self.cmd.snap(v);
        self.idx.snap(v);
        self.addr.snap(v);
        self.wel.snap(v);
        self.wip_polls.snap(v);
        self.page_erased.snap(v);
        self.launched.snap(v);
        self.last_out.snap(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a whole transaction: command byte, then `rest`, then release CS.
    /// Returns the byte the chip drove for each element of `rest`.
    fn txn(chip: &mut NdsBackup, cmd: u8, rest: &[u8]) -> Vec<u8> {
        chip.transfer(cmd);
        let out = rest.iter().map(|&b| chip.transfer(b)).collect();
        chip.deselect();
        out
    }

    fn chip() -> NdsBackup {
        NdsBackup::new(BackupKind::Flash512K)
    }

    #[test]
    fn new_chip_is_erased_and_correctly_sized() {
        let c = chip();
        assert_eq!(c.data().len(), 512 * 1024);
        assert!(c.data().iter().all(|&b| b == 0xFF));
        assert!(!c.is_dirty());
    }

    #[test]
    fn wren_sets_wel_and_wrdi_clears_it() {
        let mut c = chip();
        txn(&mut c, 0x06, &[]);
        assert_eq!(txn(&mut c, 0x05, &[0])[0] & 0x02, 0x02, "WEL should be set");
        txn(&mut c, 0x04, &[]);
        assert_eq!(txn(&mut c, 0x05, &[0])[0] & 0x02, 0x00, "WEL should be clear");
    }

    #[test]
    fn wip_stays_set_for_a_bounded_window_after_a_program_then_clears_wel() {
        let mut c = chip();
        txn(&mut c, 0x06, &[]);
        txn(&mut c, 0x0A, &[0x00, 0x00, 0x00, 0xAB]);
        // The driver must be able to OBSERVE the busy state, then see it clear.
        let s: Vec<u8> = (0..(WIP_POLLS as usize + 1))
            .map(|_| txn(&mut c, 0x05, &[0])[0])
            .collect();
        assert!(s[0] & 0x01 == 0x01, "first poll must report WIP");
        assert!(
            s[WIP_POLLS as usize] & 0x01 == 0x00,
            "WIP must clear within the window: {s:02x?}"
        );
        assert_eq!(
            s[WIP_POLLS as usize] & 0x02,
            0x00,
            "WEL must auto-clear when the write completes"
        );
    }

    #[test]
    fn page_write_stores_verbatim_and_reads_back() {
        let mut c = chip();
        txn(&mut c, 0x06, &[]);
        txn(&mut c, 0x0A, &[0x00, 0x12, 0x34, 0xDE, 0xAD, 0xBE, 0xEF]);
        let got = txn(&mut c, 0x03, &[0x00, 0x12, 0x34, 0, 0, 0, 0]);
        assert_eq!(&got[3..], &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert!(c.is_dirty());
    }

    /// The whole point of the chip: an in-game save must survive closing the app.
    ///
    /// Covers the disk half end to end — program a page through the SPI command
    /// sequence, persist to `<rom>.sav`, then seed a *fresh* chip from that file
    /// and read the bytes back over the bus. Also pins the two properties the
    /// emulator relies on: a successful save clears `dirty` (so `flush_battery`
    /// stops rewriting), and a load clears it too (so merely opening a ROM does
    /// not mark the save as needing a write-back).
    #[test]
    fn contents_survive_a_disk_round_trip() {
        let dir = std::env::temp_dir().join("emu_nds_backup_disk_test");
        let _ = std::fs::create_dir_all(&dir);
        let rom = dir.join("Cartridge.nds");
        let sav = dir.join("Cartridge.sav");
        let _ = std::fs::remove_file(&sav);

        let mut c = chip();
        txn(&mut c, 0x06, &[]);
        txn(&mut c, 0x0A, &[0x01, 0x23, 0x45, 0xC0, 0xFF, 0xEE]);
        assert!(c.is_dirty(), "a program marks the chip dirty");
        c.save_to_disk(&rom, &dir).expect("save to .sav");
        assert!(!c.is_dirty(), "a successful save clears dirty");
        assert!(sav.exists(), "save lands next to the ROM as <stem>.sav");
        assert_eq!(
            std::fs::metadata(&sav).expect("stat").len() as usize,
            BackupKind::Flash512K.size_bytes(),
            "the file is the device's full size, not just the written page"
        );

        // A fresh cartridge insertion: the bytes must come back over the bus.
        let mut restored = chip();
        restored.load_from_disk(&rom, &dir).expect("load .sav");
        assert!(!restored.is_dirty(), "loading is not a modification");
        let got = txn(&mut restored, 0x03, &[0x01, 0x23, 0x45, 0, 0, 0]);
        assert_eq!(&got[3..], &[0xC0, 0xFF, 0xEE], "saved bytes read back after reload");

        // A chip with no device must not create a file (a ROM loaded from memory
        // has nowhere to persist to).
        let mut absent = NdsBackup::new(BackupKind::None);
        let _ = std::fs::remove_file(&sav);
        absent.save_to_disk(&rom, &dir).expect("no-op save");
        assert!(!sav.exists(), "an absent chip writes nothing");

        let _ = std::fs::remove_file(&sav);
    }

    #[test]
    fn page_write_preserves_unaddressed_bytes_including_the_same_page() {
        let mut c = chip();
        // Dirty three bytes: one before the page, one inside it, one after.
        for a in [0x000FFFu32, 0x001080, 0x001100] {
            txn(&mut c, 0x06, &[]);
            let b = a.to_be_bytes();
            txn(&mut c, 0x02, &[b[1], b[2], b[3], 0x00]);
        }
        txn(&mut c, 0x06, &[]);
        txn(&mut c, 0x0A, &[0x00, 0x10, 0x00, 0x55]);
        assert_eq!(c.data()[0x001000], 0x55, "written byte");
        assert_eq!(c.data()[0x001080], 0x00, "partial PW preserves the rest of the page");
        assert_eq!(c.data()[0x000FFF], 0x00, "previous page untouched");
        assert_eq!(c.data()[0x001100], 0x00, "next page untouched");
        // A separate footer write must not erase the payload written earlier.
        txn(&mut c, 0x06, &[]);
        txn(&mut c, 0x0A, &[0x00, 0x10, 0xF0, 0x12, 0x34]);
        assert_eq!(c.data()[0x001000], 0x55, "payload survives a footer update");
        assert_eq!(&c.data()[0x0010F0..0x0010F2], &[0x12, 0x34]);
    }

    #[test]
    fn page_program_only_clears_bits() {
        let mut c = chip();
        txn(&mut c, 0x06, &[]);
        txn(&mut c, 0x02, &[0x00, 0x00, 0x00, 0xF0]);
        txn(&mut c, 0x06, &[]);
        txn(&mut c, 0x02, &[0x00, 0x00, 0x00, 0x3C]);
        assert_eq!(c.data()[0], 0xF0 & 0x3C, "PP must AND, never set bits");
    }

    #[test]
    fn program_addresses_wrap_within_the_page() {
        let mut c = chip();
        txn(&mut c, 0x06, &[]);
        // Start one byte before the page end and clock two bytes.
        txn(&mut c, 0x0A, &[0x00, 0x00, 0xFF, 0x11, 0x22]);
        assert_eq!(c.data()[0x0000FF], 0x11);
        assert_eq!(c.data()[0x000000], 0x22, "second byte wraps to the page base");
        assert_eq!(c.data()[0x000100], 0xFF, "must not spill into the next page");
    }

    #[test]
    fn programs_are_refused_without_write_enable() {
        let mut c = chip();
        txn(&mut c, 0x0A, &[0x00, 0x00, 0x00, 0x42]);
        assert_eq!(c.data()[0], 0xFF, "WEL clear must block the program");
        assert!(!c.is_dirty());
    }

    /// A refused program must not report a busy window. Arming WIP from the
    /// opcode alone would tell the driver an operation it never performed is in
    /// flight, and it would then read back 0xFF and fail its own verify.
    #[test]
    fn a_refused_program_does_not_arm_the_busy_window() {
        let mut c = chip();
        txn(&mut c, 0x0A, &[0x00, 0x00, 0x00, 0x42]); // no WREN first
        assert_eq!(
            txn(&mut c, 0x05, &[0])[0] & 0x01,
            0x00,
            "WIP must be clear after a program that never ran"
        );
    }

    /// WEL is consumed by the operation, not by observing it. A driver that
    /// programs without ever polling RDSR must still need a fresh WREN for its
    /// next write.
    #[test]
    fn write_enable_is_consumed_even_if_the_driver_never_polls_status() {
        let mut c = chip();
        txn(&mut c, 0x06, &[]);
        txn(&mut c, 0x0A, &[0x00, 0x00, 0x00, 0x11]);
        // No RDSR between the two programs.
        txn(&mut c, 0x0A, &[0x00, 0x00, 0x01, 0x22]);
        assert_eq!(c.data()[0], 0x11, "first program had WEL");
        assert_eq!(
            c.data()[1], 0xFF,
            "second program must be refused: WEL was consumed by the first"
        );
    }

    #[test]
    fn sector_erase_clears_exactly_its_64k_sector() {
        let mut c = chip();
        for a in [0x00FFFFu32, 0x010000, 0x01FFFF, 0x020000] {
            txn(&mut c, 0x06, &[]);
            let b = a.to_be_bytes();
            txn(&mut c, 0x02, &[b[1], b[2], b[3], 0x00]);
        }
        txn(&mut c, 0x06, &[]);
        txn(&mut c, 0xD8, &[0x01, 0x80, 0x00]);
        assert_eq!(c.data()[0x00FFFF], 0x00, "below the sector");
        assert_eq!(c.data()[0x010000], 0xFF, "sector start erased");
        assert_eq!(c.data()[0x01FFFF], 0xFF, "sector end erased");
        assert_eq!(c.data()[0x020000], 0x00, "above the sector");
    }

    #[test]
    fn page_erase_clears_exactly_its_256_byte_page() {
        let mut c = chip();
        for a in [0x0002FFu32, 0x000300, 0x0003FF, 0x000400] {
            txn(&mut c, 0x06, &[]);
            let b = a.to_be_bytes();
            txn(&mut c, 0x02, &[b[1], b[2], b[3], 0x00]);
        }
        txn(&mut c, 0x06, &[]);
        txn(&mut c, 0xDB, &[0x00, 0x03, 0x80]);
        assert_eq!(c.data()[0x0002FF], 0x00);
        assert_eq!(c.data()[0x000300], 0xFF);
        assert_eq!(c.data()[0x0003FF], 0xFF);
        assert_eq!(c.data()[0x000400], 0x00);
    }

    #[test]
    fn rdid_reports_the_jedec_identity_for_the_configured_size() {
        let mut c = chip();
        assert_eq!(txn(&mut c, 0x9F, &[0, 0, 0]), vec![0x20, 0x40, 0x13]);
        let mut c1 = NdsBackup::new(BackupKind::Flash1M);
        assert_eq!(txn(&mut c1, 0x9F, &[0, 0, 0]), vec![0x20, 0x40, 0x14]);
    }

    /// The measured SoulSilver status frame comes in two shapes, [05 00] and
    /// [00 05 00]. Both must yield the same status byte: a leading 0x00 is a
    /// bus-settling byte, not a command.
    #[test]
    fn a_leading_zero_byte_does_not_swallow_the_command_behind_it() {
        let mut c = chip();
        txn(&mut c, 0x06, &[]); // WREN so the status has an observable bit set
        let plain = txn(&mut c, 0x05, &[0x00]);
        let prefixed = txn(&mut c, 0x00, &[0x05, 0x00]);
        assert_eq!(plain[0] & 0x02, 0x02, "WEL must be visible in [05 00]");
        assert_eq!(
            prefixed[1], plain[0],
            "[00 05 00] must report the same status as [05 00]"
        );
    }

    #[test]
    fn reads_past_the_end_wrap_instead_of_panicking() {
        let mut c = NdsBackup::new(BackupKind::Flash256K);
        txn(&mut c, 0x06, &[]);
        txn(&mut c, 0x0A, &[0x00, 0x00, 0x00, 0x7E]);
        // Start at the last byte of the device and clock two more.
        let got = txn(&mut c, 0x03, &[0x03, 0xFF, 0xFF, 0, 0]);
        assert_eq!(got[4], 0x7E, "address must wrap to 0");
    }

    #[test]
    fn absent_chip_presents_an_erased_bus_and_never_stores() {
        let mut c = NdsBackup::new(BackupKind::None);
        assert!(!c.is_present());
        assert_eq!(txn(&mut c, 0x9F, &[0, 0, 0]), vec![0xFF, 0xFF, 0xFF]);
        assert!(!c.is_dirty());
    }

    #[test]
    fn soulsilver_gamecode_selects_a_512k_flash() {
        assert_eq!(backup_kind_for_gamecode(b"IPGE"), BackupKind::Flash512K);
        assert_eq!(BackupKind::Flash512K.size_bytes(), 524288);
    }
}
