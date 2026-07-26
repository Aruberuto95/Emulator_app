// core/src/nds/hle.rs

use crate::nds::mmu::NdsMmu;
use crate::nds::cpu::{Arm9Cpu, Arm7Cpu};

pub struct NdsHeader {
    #[allow(dead_code)]
    pub game_title: String,
    #[allow(dead_code)]
    pub game_code: [u8; 4],
    pub arm9_rom_offset: u32,
    pub arm9_entry_address: u32,
    pub arm9_ram_address: u32,
    pub arm9_size: u32,
    pub arm7_rom_offset: u32,
    pub arm7_entry_address: u32,
    pub arm7_ram_address: u32,
    pub arm7_size: u32,
    pub arm9_autoload_info: u32,
    pub arm7_autoload_info: u32,
}

impl NdsHeader {
    pub fn parse(rom_data: &[u8]) -> Result<Self, &'static str> {
        if rom_data.len() < 0x200 {
            return Err("NDS ROM truncated");
        }

        let mut title = String::new();
        for &b in &rom_data[0..12] {
            if b == 0 {
                break;
            }
            if b >= 32 && b <= 126 {
                title.push(b as char);
            } else {
                title.push('?');
            }
        }

        let mut game_code = [0; 4];
        game_code.copy_from_slice(&rom_data[0x0C..0x10]);

        Ok(Self {
            game_title: title.trim().to_string(),
            game_code,
            arm9_rom_offset: u32::from_le_bytes(rom_data[0x20..0x24].try_into().unwrap()),
            arm9_entry_address: u32::from_le_bytes(rom_data[0x24..0x28].try_into().unwrap()),
            arm9_ram_address: u32::from_le_bytes(rom_data[0x28..0x2C].try_into().unwrap()),
            arm9_size: u32::from_le_bytes(rom_data[0x2C..0x30].try_into().unwrap()),
            arm7_rom_offset: u32::from_le_bytes(rom_data[0x30..0x34].try_into().unwrap()),
            arm7_entry_address: u32::from_le_bytes(rom_data[0x34..0x38].try_into().unwrap()),
            arm7_ram_address: u32::from_le_bytes(rom_data[0x38..0x3C].try_into().unwrap()),
            arm7_size: u32::from_le_bytes(rom_data[0x3C..0x40].try_into().unwrap()),
            arm9_autoload_info: u32::from_le_bytes(rom_data[0x074..0x078].try_into().unwrap()),
            arm7_autoload_info: u32::from_le_bytes(rom_data[0x078..0x07C].try_into().unwrap()),
        })
    }
}

pub fn boot_load_rom(mmu: &mut NdsMmu, arm9: &mut Arm9Cpu, arm7: &mut Arm7Cpu, rom_data: &[u8]) -> Result<(), &'static str> {
    mmu.reset();

    // Reset the CPUs BEFORE configuring the TCMs. `Arm9Cpu::reset` clears CP15,
    // so when it ran at the END of boot (as it used to) it wiped this setup and
    // disabled the ITCM/DTCM the autoload below populates — leaving TCM-resident
    // code invisible to the running ARM9. Doing it here makes the config persist.
    arm9.reset(mmu);
    arm7.reset();
    // Enable ITCM (control bit 18) + DTCM (bit 16). The TCM region registers carry
    // base + size (N in bits[5:1], region = 512<<N): ITCM base 0x01000000 / 32KB
    // (N=6), DTCM base 0x0B000000 / 16KB (N=5). Games reconfigure these via CP15
    // during boot (SoulSilver: ITCM base 0/32MB, DTCM base 0x027E0000, high vecs).
    arm9.cp15.control |= (1 << 18) | (1 << 16);
    arm9.cp15.itcm_control = 0x01000000 | (6 << 1);
    arm9.cp15.dtcm_control = 0x0B000000 | (5 << 1);
    mmu.arm9_cp15 = arm9.cp15; // address routing reads mmu's copy — keep in sync

    // HLE the WRAM allocation the ARM9/BIOS would establish during the boot IPC
    // handshake: map the shared WRAM to the ARM7 (WRAMCNT=3). The ARM7's early
    // boot relocation copies its secondary code to 0x037Fxxxx expecting SHARED
    // WRAM there (distinct from its own 64KB WRAM at 0x0380xxxx); with WRAMCNT=0
    // those alias in the ARM7 WRAM mirror and the copy self-overwrites. Our ARM9
    // can't set this in time (still decompressing; the handshake hasn't
    // completed), so we establish the mapping up front.
    // ponytail: static WRAMCNT is an HLE approximation of the real dynamic
    // handshake; revisit if a title reconfigures WRAM and depends on the ARM9 view.
    mmu.wram_control = 3;

    let header = NdsHeader::parse(rom_data)?;

    // 1. Copy ARM9 binary
    let arm9_start = header.arm9_rom_offset as usize;
    let arm9_end = arm9_start + header.arm9_size as usize;
    if arm9_end > rom_data.len() {
        return Err("ARM9 binary offset/size out of ROM bounds");
    }
    let arm9_bytes = &rom_data[arm9_start..arm9_end];
    for (i, &b) in arm9_bytes.iter().enumerate() {
        mmu.write_byte_arm9(header.arm9_ram_address.wrapping_add(i as u32), b);
    }

    // 2. Copy ARM7 binary
    let arm7_start = header.arm7_rom_offset as usize;
    let arm7_end = arm7_start + header.arm7_size as usize;
    if arm7_end > rom_data.len() {
        return Err("ARM7 binary offset/size out of ROM bounds");
    }
    let arm7_bytes = &rom_data[arm7_start..arm7_end];
    for (i, &b) in arm7_bytes.iter().enumerate() {
        mmu.write_byte_arm7(header.arm7_ram_address.wrapping_add(i as u32), b);
    }

    // 3. Copy ROM Header to 0x027FFE00 (256 bytes or 0x160 bytes)
    let header_len = std::cmp::min(rom_data.len(), 0x160);
    let header_slice = &rom_data[0..header_len];
    for (i, &b) in header_slice.iter().enumerate() {
        // 0x027FFE00 maps to top of Main RAM (0x3FFE00)
        mmu.write_byte_arm9(0x027F_FE00 + i as u32, b);
    }

    // 4. Parse ARM9 autoload table and copy TCM sections
    if header.arm9_autoload_info != 0 {
        if let Some(offset_in_binary) = header.arm9_autoload_info.checked_sub(header.arm9_ram_address) {
            let mut entry_idx = 0;
            let mut current_rom_src = header.arm9_rom_offset + header.arm9_size;
            // Total .bss bytes this table may zero. The per-entry guard below
            // bounds one entry at 8 MB but nothing bounded the table: a crafted
            // ROM (every field is cartridge data, and the only other gate is the
            // 128 MB file-size limit) can repeat a 12-byte entry {dest, size 0,
            // bss 0x7FFFFF} for the whole ARM9 image, giving millions of entries
            // x 8 MB of byte-at-a-time writes — the load never returns and the
            // app hangs before it draws a frame. Real DS binaries have a handful
            // of autoload sections totalling well under one main-RAM's worth, so
            // 8 MB across the whole table is generous and cannot reject a real
            // cartridge.
            const BSS_TOTAL_CAP: u64 = 8 * 1024 * 1024;
            let mut bss_total: u64 = 0;
            loop {
                let entry_offset = offset_in_binary as usize + entry_idx * 12;
                if entry_offset + 12 > arm9_bytes.len() {
                    break;
                }

                let dest_addr = u32::from_le_bytes(arm9_bytes[entry_offset..entry_offset + 4].try_into().unwrap());
                let size = u32::from_le_bytes(arm9_bytes[entry_offset + 4..entry_offset + 8].try_into().unwrap());
                let bss_size = u32::from_le_bytes(arm9_bytes[entry_offset + 8..entry_offset + 12].try_into().unwrap());

                if dest_addr == 0 {
                    break;
                }

                if size > 0 {
                    if let Some(end_src) = current_rom_src.checked_add(size) {
                        if end_src as usize <= rom_data.len() {
                            let section_bytes = &rom_data[current_rom_src as usize..end_src as usize];
                            for (i, &b) in section_bytes.iter().enumerate() {
                                mmu.write_byte_arm9(dest_addr.wrapping_add(i as u32), b);
                            }
                            current_rom_src = end_src;
                        }
                    }
                }

                // `>=`, not `>`: the per-entry guard this replaces admitted
                // `bss_size < 8 MB`, and `test_hle_autoload_and_ipc_robustness`
                // pins both sides of that boundary. Main RAM is 4 MB and mirrors,
                // so an exactly-8 MB fill wraps it twice and erases the section
                // that was just copied — which is what the test caught.
                bss_total = bss_total.saturating_add(u64::from(bss_size));
                if bss_total >= BSS_TOTAL_CAP {
                    break;
                }
                for i in 0..bss_size {
                    mmu.write_byte_arm9(dest_addr.wrapping_add(size).wrapping_add(i), 0);
                }

                entry_idx += 1;
            }
        }
    }

    // 5. Parse ARM7 autoload table
    if header.arm7_autoload_info != 0 {
        if let Some(offset_in_binary) = header.arm7_autoload_info.checked_sub(header.arm7_ram_address) {
            let mut entry_idx = 0;
            let mut current_rom_src = header.arm7_rom_offset + header.arm7_size;
            loop {
                let entry_offset = offset_in_binary as usize + entry_idx * 12;
                if entry_offset + 12 > arm7_bytes.len() {
                    break;
                }

                let dest_addr = u32::from_le_bytes(arm7_bytes[entry_offset..entry_offset + 4].try_into().unwrap());
                let size = u32::from_le_bytes(arm7_bytes[entry_offset + 4..entry_offset + 8].try_into().unwrap());
                let bss_size = u32::from_le_bytes(arm7_bytes[entry_offset + 8..entry_offset + 12].try_into().unwrap());

                if dest_addr == 0 {
                    break;
                }

                if size > 0 {
                    if let Some(end_src) = current_rom_src.checked_add(size) {
                        if end_src as usize <= rom_data.len() {
                            let section_bytes = &rom_data[current_rom_src as usize..end_src as usize];
                            for (i, &b) in section_bytes.iter().enumerate() {
                                mmu.write_byte_arm7(dest_addr.wrapping_add(i as u32), b);
                            }
                            current_rom_src = end_src;
                        }
                    }
                }

                if bss_size < 8 * 1024 * 1024 {
                    for i in 0..bss_size {
                        mmu.write_byte_arm7(dest_addr.wrapping_add(size).wrapping_add(i), 0);
                    }
                }

                entry_idx += 1;
            }
        }
    }

    // 6. Initialize CPU entry state. The CPUs were already reset above (before
    // the TCM setup); resetting again here would wipe CP15 and disable the TCMs.
    // ARM9: System Mode, SP=0x03002F00, PC=entry_point
    arm9.cpu.registers.cpsr = 0x1F; // System Mode (T bit clear, ARM state)
    arm9.cpu.registers.gpr[13] = 0x03002F00; // SP
    arm9.cpu.registers.gpr[15] = header.arm9_entry_address;
    arm9.flush_pipeline(mmu);

    // ARM7: System Mode, SP=0x0380FFFC, PC=entry_point
    arm7.cpu.registers.cpsr = 0x1F; // System Mode (T bit clear, ARM state)
    arm7.cpu.registers.gpr[13] = 0x0380FFFC; // SP
    arm7.cpu.registers.gpr[15] = header.arm7_entry_address;
    arm7.flush_pipeline(mmu);

    // Write boot indicator flags (GBATEK boot success signals)
    mmu.write_byte_arm9(0x027FFFC0, 0x66);
    mmu.write_byte_arm7(0x027FFFC4, 0x66);
    mmu.write_byte_arm9(0x027FFFC8, 0); // Lid open
    mmu.write_byte_arm9(0x027FFFCC, 0); // Boot indicator
    write_user_settings(mmu);

    // Firmware boot-info leftovers (GBATEK "boot info"): the real BIOS/firmware
    // reads the gamecard chip ID during boot and stores it in main RAM at
    // 0x027FF800/0x027FF804 (+ the 0x027FFC00 copies). NitroSDK's card-removal
    // watchdog re-reads the ID over the Gamecard bus (cmd 0xB8 -> 0x00007FC2 for
    // this ROM) and compares it against these slots; with them unset (0),
    // SoulSilver concluded the cartridge was pulled out mid-boot, notified the
    // ARM7 (PXI cmd 14) and OS_Terminate'd into a permanent WFI idle hang.
    // 0x027FFC10 is the slot SoulSilver's CARD lib actually references (literal
    // in its check fn); the others are the documented firmware locations.
    let chip_id = NdsMmu::gamecard_chip_id_for_len(rom_data.len());
    for slot in [0x027F_F800u32, 0x027F_F804, 0x027F_FC00, 0x027F_FC04, 0x027F_FC10] {
        mmu.write_word_arm9(slot, chip_id);
    }
    // Boot indicator halfword: 1 = normal cartridge boot.
    mmu.write_halfword_arm9(0x027F_FC40, 0x0001);

    Ok(())
}

/// Build the 512-byte firmware user-settings page (language, touch
/// calibration, the two CRC16s). Shared by the boot HLE (which copies the
/// first 0x74 bytes to main RAM at 0x027FFC80) and the SPI firmware-flash
/// image (games locate this page via the firmware header and read/verify it
/// over SPI directly).
pub(crate) fn build_user_settings() -> Vec<u8> {
    let mut settings = vec![0u8; 512];
    settings[0x00] = 0x05; // version — GBATEK: must be 5
    // 0x02 favourite colour, 0x03 birthday month, 0x04 birthday day. Language
    // is NOT here -- it lives in bits 0-2 of 0x64 -- so writing it to 0x04 both
    // left the console reporting language 0 (Japanese) and set the birthday to
    // the 1st of month 0.
    settings[0x02] = 0; // favourite colour
    settings[0x03] = 1; // birthday month = January
    settings[0x04] = 1; // birthday day = 1
    settings[0x64] = 1; // language = English (bits 0-2)

    // Touch calibration. Both endpoints AND the slope matter: the panel is
    // 256x192 and a TSC2046 conversion is 12 bits, so a transform steeper than
    // 4095/255 = 16.06 counts/px in X or 4095/191 = 21.4 in Y cannot express
    // the whole surface. The previous pair implied 18 and 22.5, which needed
    // 4608 and 4320 counts: `SpiController` clamped the overflow, so every tap
    // in the leftmost 16 columns decoded to the same x, the rightmost 13 to
    // another, and 6 top / 4 bottom rows likewise -- a button in the corner
    // could not be pressed at all. 15 and 20 counts/px fit with room to spare
    // (x: 128..3953, y: 128..3948) and keep both endpoints on whole pixels.
    //
    // `SpiController::calculate_result` MUST stay the exact inverse of this;
    // the two are pinned together by `touch_transform_round_trips`.
    let adc_x1 = 608u16;
    let adc_y1 = 608u16;
    let screen_x1 = 32u8;
    let screen_y1 = 24u8;
    let adc_x2 = 3488u16;
    let adc_y2 = 3488u16;
    let screen_x2 = 224u8;
    let screen_y2 = 168u8;

    settings[0x58] = adc_x1 as u8;
    settings[0x59] = (adc_x1 >> 8) as u8;
    settings[0x5A] = adc_y1 as u8;
    settings[0x5B] = (adc_y1 >> 8) as u8;
    settings[0x5C] = screen_x1;
    settings[0x5D] = screen_y1;
    settings[0x5E] = adc_x2 as u8;
    settings[0x5F] = (adc_x2 >> 8) as u8;
    settings[0x60] = adc_y2 as u8;
    settings[0x61] = (adc_y2 >> 8) as u8;
    settings[0x62] = screen_x2;
    settings[0x63] = screen_y2;

    // GBATEK trailer layout: 0x70 = 16-bit UPDATE COUNTER, 0x72 = CRC16
    // (poly 0xA001, init 0xFFFF) over bytes 0x00-0x6F. The old code stored a
    // CRC in the counter slot and a CRC over 0x00-0x71 at 0x72, so every
    // settings copy failed the SDK's validation: the game silently fell back
    // to defaults, TP_SetCalibrateParam never got real values, and
    // TP_GetCalibratedPoint returned x=y=0 for every pen sample — touch
    // FLAGS flowed (new_down/held pulsed in the input struct) but every UI
    // hit-test failed. U27 root cause of the dead UI touch buttons.
    settings[0x70] = 0; // update counter
    settings[0x71] = 0;
    let crc = crc16(&settings[0..0x70]);
    settings[0x72] = crc as u8;
    settings[0x73] = (crc >> 8) as u8;
    settings
}

fn write_user_settings(mmu: &mut NdsMmu) {
    let settings = build_user_settings();
    // GBATEK: the firmware boot copies the user settings to main RAM at
    // 0x027FFC80 (NOT 0x027FC000 — that was a transposition bug and games read
    // language/owner data from the documented address). Only the settings block
    // itself (0x70 bytes + the two CRC16s at 0x70/0x72) is copied: writing the
    // full 512-byte NVRAM page would run past 0x027FFE00 and clobber the ROM
    // header copy placed there during boot.
    for (i, &b) in settings[..0x74].iter().enumerate() {
        mmu.write_byte_arm9(0x027F_FC80 + i as u32, b);
    }
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &byte in data {
        crc ^= byte as u16;
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::crc16;

    // Vector canónico de CRC-16/MODBUS (poly 0xA001, init 0xFFFF, reflejado): el
    // check estándar de "123456789" es 0x4B37 — el mismo CRC que usa el firmware NDS
    // para sus user settings. Con el poly viejo (0x1021) el valor era distinto.
    #[test]
    fn crc16_modbus_canonical_vector() {
        assert_eq!(crc16(b"123456789"), 0x4B37);
    }
}
