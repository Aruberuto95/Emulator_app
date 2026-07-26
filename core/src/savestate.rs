use crate::emulator::Emulator;
use std::path::Path;

/// Reply for a *legacy* NDS state: the placeholder this module used to write,
/// which stored `"nds_rom_loaded": true` and nothing else while reporting
/// success, so a slot looked occupied but restored no CPU, memory, VRAM, PPU,
/// APU or GX state at all. Such a file must still be refused — accepting it
/// would mark a ROM as loaded and leave the machine mid-air.
///
/// Real NDS states are the binary container in [`crate::snapshot`]; they are
/// told apart by its magic, so this only ever fires for the old JSON files.
const NDS_LEGACY_PLACEHOLDER: &str =
    "LOAD_STATE_ERROR This slot holds an obsolete NDS placeholder state; save again to replace it";

#[cfg(test)]
mod state_validation_tests {
    use crate::emulator::Emulator;

    /// [`from_hex`] drops any pair that is not valid hex, so a corrupted blob
    /// decodes **shorter**. Installing that would shrink a memory region, and
    /// the MMUs index those `Vec`s directly (`self.vram[offset]`), so the next
    /// access would panic out of bounds — which aborts the process across the
    /// cxx FFI boundary. Length must match exactly or nothing is applied.
    #[test]
    fn restore_region_rejects_any_length_change() {
        let mut region = vec![0xAAu8; 8];

        // Exact length: applied.
        super::restore_region(&mut region, "0102030405060708");
        assert_eq!(region, vec![1, 2, 3, 4, 5, 6, 7, 8]);

        // One corrupted character drops that pair, decoding to 7 bytes. This is
        // the realistic case: no attacker, just a damaged file.
        super::restore_region(&mut region, "01020304050607zz");
        assert_eq!(region.len(), 8, "a short blob must not shrink the region");
        assert_eq!(
            region,
            vec![1, 2, 3, 4, 5, 6, 7, 8],
            "a rejected blob must not be partially applied either"
        );

        // Longer than the region, and empty, are refused the same way.
        super::restore_region(&mut region, "0102030405060708090A");
        assert_eq!(region.len(), 8);
        super::restore_region(&mut region, "");
        assert_eq!(region, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    /// `load_state` writes `speed` straight into the field, so it is the one
    /// path into it that skips `Emulator::set_speed`. That matters because a
    /// savestate is a *file*: `speed` scales every console's
    /// `cycles_per_sample`, and `BoxResampler::tick` emits one output sample
    /// per `cycles_per_sample` cycles — a near-zero value read back from disk
    /// wedges `tick` rather than merely playing slowly. A state written by a
    /// build that accepted such a speed is a legitimately-checksummed file, so
    /// integrity checking upstream does not cover this.
    #[test]
    fn load_state_rejects_out_of_range_speed() {
        let dir = std::env::temp_dir().join("emu_savestate_speed_validation");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let base = dir.to_str().expect("temp path is UTF-8");
        let path = dir.join("savestate_speedtest.sav");

        let mut emu = Emulator::new();
        emu.set_speed(2.0);
        assert_eq!(emu.save_state("speedtest", base), "SAVE_STATE_OK");
        assert_eq!(emu.load_state("speedtest", base), "LOAD_STATE_OK");
        let good = std::fs::read_to_string(&path).expect("state file readable");

        // Rewrite just the speed value, leaving the rest of the file intact.
        let marker = "\"speed\": ";
        let start = good.find(marker).expect("state carries a speed field") + marker.len();
        let end = start + good[start..].find(',').expect("speed field ends in a comma");

        // 3.6e-6 is the smallest speed that still leaves the GBA a non-zero
        // cycle budget, i.e. the worst case for the resampler loop.
        for bad in ["0.0000036", "0", "-1", "1000", "NaN", "inf"] {
            let tampered = format!("{}{bad}{}", &good[..start], &good[end..]);
            std::fs::write(&path, &tampered).expect("write tampered state");
            assert_eq!(
                emu.load_state("speedtest", base),
                "LOAD_STATE_ERROR Invalid speed",
                "speed {bad} was accepted"
            );
        }
        assert_eq!(emu.get_speed(), 2.0, "a refused state must not move the speed");

        // The bounds themselves still load, so this rejects only what it must.
        for ok in [Emulator::MIN_SPEED, 1.0, Emulator::MAX_SPEED] {
            let patched = format!("{}{ok}{}", &good[..start], &good[end..]);
            std::fs::write(&path, &patched).expect("write patched state");
            assert_eq!(emu.load_state("speedtest", base), "LOAD_STATE_OK", "speed {ok} refused");
            assert_eq!(emu.get_speed(), ok);
        }

        let _ = std::fs::remove_file(&path);
    }
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Replace a variable-length memory region from a hex blob, but only when the
/// decoded length matches the length the region already has.
///
/// [`from_hex`] silently drops any pair that is not valid hex, so a single
/// corrupted character inside a 32 KB blob decodes to a **shorter** `Vec`.
/// Installed unchecked that shrinks the region, and the next `self.vram[offset]`
/// in the MMU — which indexes directly, having no reason to expect a short
/// buffer — panics out of bounds. A Rust panic aborts the process across the cxx
/// FFI boundary, and these JSON states carry no hash, so ordinary file
/// corruption reaches it.
///
/// The fixed-size fields in this module already guard with `if bytes.len() == N`;
/// this gives the `Vec`-backed ones the same protection. Comparing against the
/// live length instead of a literal keeps it correct for the regions whose size
/// depends on the cartridge (flash, MBC RAM) and cannot drift from the
/// allocation.
fn restore_region(region: &mut Vec<u8>, hex: &str) {
    let bytes = from_hex(hex);
    if bytes.len() == region.len() {
        *region = bytes;
    }
}

fn from_hex(hex: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut chars = hex.chars();
    while let (Some(c1), Some(c2)) = (chars.next(), chars.next()) {
        if let Some(val) = u8::from_str_radix(&format!("{}{}", c1, c2), 16).ok() {
            bytes.push(val);
        }
    }
    bytes
}

fn get_json_bool(json: &str, key: &str) -> Option<bool> {
    let pattern = format!("\"{}\"", key);
    if let Some(pos) = json.find(&pattern) {
        if let Some(colon) = json[pos..].find(':') {
            let sub = &json[pos + colon..];
            let next_true = sub.find("true");
            let next_false = sub.find("false");
            match (next_true, next_false) {
                (Some(t), Some(f)) => {
                    if t < f {
                        Some(true)
                    } else {
                        Some(false)
                    }
                }
                (Some(_), None) => Some(true),
                (None, Some(_)) => Some(false),
                _ => None,
            }
        } else {
            None
        }
    } else {
        None
    }
}

fn get_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    if let Some(pos) = json.find(&pattern) {
        if let Some(colon) = json[pos..].find(':') {
            let sub = &json[pos + colon..];
            if let Some(first_quote) = sub.find('"') {
                let rest = &sub[first_quote + 1..];
                if let Some(second_quote) = rest.find('"') {
                    return Some(rest[..second_quote].to_string());
                }
            }
        }
    }
    None
}

fn get_json_number(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    if let Some(pos) = json.find(&pattern) {
        if let Some(colon) = json[pos..].find(':') {
            let sub = &json[pos + colon..];
            let mut num_str = String::new();
            let mut started = false;
            for c in sub.chars() {
                if c.is_numeric() || c == '.' || c == '-' || c == 'e' || c == 'E' || c == '+' {
                    num_str.push(c);
                    started = true;
                } else if started {
                    break;
                }
            }
            if !num_str.is_empty() {
                return Some(num_str);
            }
        }
    }
    None
}

fn get_json_array_of_numbers(json: &str, key: &str) -> Option<Vec<u32>> {
    let pattern = format!("\"{}\"", key);
    if let Some(pos) = json.find(&pattern) {
        if let Some(colon) = json[pos..].find(':') {
            let sub = &json[pos + colon..];
            if let Some(start_bracket) = sub.find('[') {
                if let Some(end_bracket) = sub[start_bracket..].find(']') {
                    let array_str = &sub[start_bracket + 1..start_bracket + end_bracket];
                    let mut vals = Vec::new();
                    for part in array_str.split(',') {
                        let trimmed = part.trim();
                        if let Ok(val) = trimmed.parse::<u32>() {
                            vals.push(val);
                        }
                    }
                    return Some(vals);
                }
            }
        }
    }
    None
}

fn atomic_save(tmp_path: &Path, sav_path: &Path, content: &str) -> std::io::Result<()> {
    use std::fs::File;
    use std::io::Write;
    let mut file = File::create(tmp_path)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(tmp_path, sav_path)?;
    Ok(())
}

pub(crate) fn parse_extra_fields(json: &str) -> Vec<(String, String)> {
    let mut extra = Vec::new();
    let known_keys = [
        "console_type", "playback_state", "ticks", "player_x", "player_y",
        "buttons", "speed", "frame_skip", "cpu_cycles", "rendered_frames",
        "up", "down", "left", "right", "a", "b", "start", "select", "l", "r",
        // GBC keys:
        "gbc_rom_loaded", "gbc_cpu_pc", "gbc_cpu_sp", "gbc_cpu_a", "gbc_cpu_f",
        "gbc_cpu_b", "gbc_cpu_c", "gbc_cpu_d", "gbc_cpu_e", "gbc_cpu_h", "gbc_cpu_l",
        "gbc_cpu_ime", "gbc_cpu_halted", "gbc_cpu_double_speed", "gbc_cpu_ei_delay",
        "gbc_cpu_stop_mode", "gbc_cpu_stop_cycles_left", "gbc_cpu_div_counter",
        "gbc_mmu_ie", "gbc_mmu_bcps", "gbc_mmu_ocps", "gbc_mmu_mbc_rom_bank",
        "gbc_mmu_mbc_ram_bank_or_rtc_reg", "gbc_mmu_mbc_ram_rtc_enabled",
        "gbc_mmu_mbc_latch_state", "gbc_mmu_mbc_rtc_seconds", "gbc_mmu_mbc_rtc_minutes",
        "gbc_mmu_mbc_rtc_hours", "gbc_mmu_mbc_rtc_days", "gbc_mmu_mbc_rtc_halt",
        "gbc_mmu_mbc_rtc_day_overflow", "gbc_mmu_mbc_rtc_cycle_accumulator",
        "gbc_mmu_vram", "gbc_mmu_wram", "gbc_mmu_oam", "gbc_mmu_io", "gbc_mmu_hram",
        "gbc_mmu_bg_palette_ram", "gbc_mmu_obj_palette_ram", "gbc_mmu_mbc_ram",
        "gbc_mmu_mbc_rtc_latched_seconds", "gbc_mmu_mbc_rtc_latched_minutes",
        "gbc_mmu_mbc_rtc_latched_hours", "gbc_mmu_mbc_rtc_latched_days_low",
        "gbc_mmu_mbc_rtc_latched_days_high", "gbc_ppu_cycle_accumulator",
        "gbc_apu_frame_seq_timer", "gbc_apu_frame_seq_step", "gbc_apu_ch1_enabled",
        "gbc_apu_ch1_duty", "gbc_apu_ch1_duty_pointer", "gbc_apu_ch1_length_enabled",
        "gbc_apu_ch1_length_counter", "gbc_apu_ch1_period", "gbc_apu_ch1_period_timer",
        "gbc_apu_ch1_volume", "gbc_apu_ch1_env_enabled", "gbc_apu_ch1_env_period",
        "gbc_apu_ch1_env_timer", "gbc_apu_ch1_env_direction", "gbc_apu_ch1_env_initial_volume",
        "gbc_apu_ch1_sweep_enabled", "gbc_apu_ch1_sweep_period", "gbc_apu_ch1_sweep_timer",
        "gbc_apu_ch1_sweep_shift", "gbc_apu_ch1_sweep_direction", "gbc_apu_ch1_shadow_frequency",
        "gbc_apu_ch2_enabled", "gbc_apu_ch2_duty", "gbc_apu_ch2_duty_pointer",
        "gbc_apu_ch2_length_enabled", "gbc_apu_ch2_length_counter", "gbc_apu_ch2_period",
        "gbc_apu_ch2_period_timer", "gbc_apu_ch2_volume", "gbc_apu_ch2_env_enabled",
        "gbc_apu_ch2_env_period", "gbc_apu_ch2_env_timer", "gbc_apu_ch2_env_direction",
        "gbc_apu_ch2_env_initial_volume", "gbc_apu_ch3_enabled", "gbc_apu_ch3_dac_enabled",
        "gbc_apu_ch3_length_enabled", "gbc_apu_ch3_length_counter", "gbc_apu_ch3_period",
        "gbc_apu_ch3_period_timer", "gbc_apu_ch3_volume_shift", "gbc_apu_ch3_wave_ram",
        "gbc_apu_ch3_sample_pointer", "gbc_apu_ch4_enabled", "gbc_apu_ch4_length_enabled",
        "gbc_apu_ch4_length_counter", "gbc_apu_ch4_volume", "gbc_apu_ch4_env_enabled",
        "gbc_apu_ch4_env_period", "gbc_apu_ch4_env_timer", "gbc_apu_ch4_env_direction",
        "gbc_apu_ch4_env_initial_volume", "gbc_apu_ch4_lfsr", "gbc_apu_ch4_divisor",
        "gbc_apu_ch4_shift_clock", "gbc_apu_ch4_width_7bit", "gbc_apu_ch4_period_timer",
        // GBA keys:
        "gba_rom_loaded", "gba_cpu_r0", "gba_cpu_r1", "gba_cpu_r2", "gba_cpu_r3",
        "gba_cpu_r4", "gba_cpu_r5", "gba_cpu_r6", "gba_cpu_r7", "gba_cpu_r8",
        "gba_cpu_r9", "gba_cpu_r10", "gba_cpu_r11", "gba_cpu_r12", "gba_cpu_r13",
        "gba_cpu_r14", "gba_cpu_r15", "gba_cpu_cpsr", "gba_cpu_spsr", "gba_cpu_halted",
        "gba_mmu_waitcnt", "gba_mmu_ie", "gba_mmu_if", "gba_mmu_ime", "gba_flash_bank",
        "gba_flash_state", "gba_mmu_ewram", "gba_mmu_iwram", "gba_mmu_palette_ram",
        "gba_mmu_vram", "gba_mmu_oam", "gba_mmu_io", "gba_flash_data", "gba_cpu_r8_usr",
        "gba_cpu_r8_fiq", "gba_cpu_r13_usr", "gba_cpu_r14_usr", "gba_cpu_r13_svc",
        "gba_cpu_r14_svc", "gba_cpu_spsr_svc", "gba_cpu_r13_irq", "gba_cpu_r14_irq",
        "gba_cpu_spsr_irq", "gba_cpu_r13_abt", "gba_cpu_r14_abt", "gba_cpu_spsr_abt",
        "gba_cpu_r13_und", "gba_cpu_r14_und", "gba_cpu_spsr_und", "gba_cpu_r13_fiq",
        "gba_cpu_r14_fiq", "gba_cpu_spsr_fiq", "gba_cpu_pipeline", "gba_apu_fifo_a_buffer",
        "gba_apu_fifo_a_write_ptr", "gba_apu_fifo_a_read_ptr", "gba_apu_fifo_a_count",
        "gba_apu_fifo_b_buffer", "gba_apu_fifo_b_write_ptr", "gba_apu_fifo_b_read_ptr",
        "gba_apu_fifo_b_count", "gba_apu_dma_request_a", "gba_apu_dma_request_b",
        "gba_apu_current_sample_a", "gba_apu_current_sample_b",
        "gba_apu_soundcnt_h", "gba_apu_soundcnt_x", "gba_apu_soundbias",
        "gba_apu_nr50", "gba_apu_nr51",
        // GBA DMA channels:
        "gba_dma_ch0_sad", "gba_dma_ch0_dad", "gba_dma_ch0_count", "gba_dma_ch0_control",
        "gba_dma_ch0_cur_src", "gba_dma_ch0_cur_dest", "gba_dma_ch0_cur_count", "gba_dma_ch0_active",
        "gba_dma_ch1_sad", "gba_dma_ch1_dad", "gba_dma_ch1_count", "gba_dma_ch1_control",
        "gba_dma_ch1_cur_src", "gba_dma_ch1_cur_dest", "gba_dma_ch1_cur_count", "gba_dma_ch1_active",
        "gba_dma_ch2_sad", "gba_dma_ch2_dad", "gba_dma_ch2_count", "gba_dma_ch2_control",
        "gba_dma_ch2_cur_src", "gba_dma_ch2_cur_dest", "gba_dma_ch2_cur_count", "gba_dma_ch2_active",
        "gba_dma_ch3_sad", "gba_dma_ch3_dad", "gba_dma_ch3_count", "gba_dma_ch3_control",
        "gba_dma_ch3_cur_src", "gba_dma_ch3_cur_dest", "gba_dma_ch3_cur_count", "gba_dma_ch3_active",
        // GBA timers:
        "gba_timer_ch0_counter", "gba_timer_ch0_reload", "gba_timer_ch0_control", "gba_timer_ch0_cycle_accumulator", "gba_timer_ch0_overflowed",
        "gba_timer_ch1_counter", "gba_timer_ch1_reload", "gba_timer_ch1_control", "gba_timer_ch1_cycle_accumulator", "gba_timer_ch1_overflowed",
        "gba_timer_ch2_counter", "gba_timer_ch2_reload", "gba_timer_ch2_control", "gba_timer_ch2_cycle_accumulator", "gba_timer_ch2_overflowed",
        "gba_timer_ch3_counter", "gba_timer_ch3_reload", "gba_timer_ch3_control", "gba_timer_ch3_cycle_accumulator", "gba_timer_ch3_overflowed",
    ];

    let chars: Vec<char> = json.chars().collect();
    let mut i = 0;

    // Skip to '{'
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    if i >= chars.len() || chars[i] != '{' {
        return extra;
    }
    i += 1; // skip '{'

    loop {
        // Skip whitespace
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        if chars[i] == '}' {
            break;
        }

        // Expect key starts with '"'
        if chars[i] != '"' {
            i += 1;
            continue;
        }
        i += 1; // skip '"'
        
        let mut key = String::new();
        while i < chars.len() {
            if chars[i] == '"' {
                break;
            }
            if chars[i] == '\\' && i + 1 < chars.len() {
                key.push(chars[i+1]);
                i += 2;
            } else {
                key.push(chars[i]);
                i += 1;
            }
        }
        if i >= chars.len() {
            break;
        }
        i += 1; // skip '"'

        // Skip to ':'
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() || chars[i] != ':' {
            break;
        }
        i += 1; // skip ':'

        // Skip to start of value
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }

        let val_start = i;
        let val_end;

        // Parse value based on type
        if chars[i] == '"' {
            // String value
            i += 1;
            while i < chars.len() {
                if chars[i] == '"' {
                    i += 1;
                    break;
                }
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            val_end = i;
        } else if chars[i] == '{' {
            // Nested object
            let mut depth = 1;
            i += 1;
            while i < chars.len() && depth > 0 {
                if chars[i] == '"' {
                    i += 1;
                    while i < chars.len() {
                        if chars[i] == '"' {
                            i += 1;
                            break;
                        }
                        if chars[i] == '\\' && i + 1 < chars.len() {
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                } else {
                    if chars[i] == '{' {
                        depth += 1;
                    } else if chars[i] == '}' {
                        depth -= 1;
                    }
                    i += 1;
                }
            }
            val_end = i;
        } else if chars[i] == '[' {
            // Array value
            let mut depth = 1;
            i += 1;
            while i < chars.len() && depth > 0 {
                if chars[i] == '"' {
                    i += 1;
                    while i < chars.len() {
                        if chars[i] == '"' {
                            i += 1;
                            break;
                        }
                        if chars[i] == '\\' && i + 1 < chars.len() {
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                } else {
                    if chars[i] == '[' {
                        depth += 1;
                    } else if chars[i] == ']' {
                        depth -= 1;
                    }
                    i += 1;
                }
            }
            val_end = i;
        } else {
            // Primitive (number, boolean, null)
            while i < chars.len() && chars[i] != ',' && chars[i] != '}' && !chars[i].is_whitespace() {
                i += 1;
            }
            val_end = i;
        }

        let val_str: String = chars[val_start..val_end].iter().collect();
        let val_trimmed = val_str.trim().to_string();

        if !known_keys.contains(&key.as_str()) {
            extra.push((key, val_trimmed));
        }

        // Skip to next item (comma or closing brace)
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i < chars.len() && chars[i] == ',' {
            i += 1; // skip comma
        }
    }

    extra
}

impl Emulator {
    /// `<rom_stem>_savestate_<slot>` — the slot basename shared by the JSON and
    /// the binary container, with the no-ROM fallback. Single source of truth:
    /// the frontend's save menu lists slots by this name, so save, load and menu
    /// must all agree on it.
    fn slot_basename(&self, slot: &str) -> String {
        match self.rom_path.file_stem().and_then(|s| s.to_str()) {
            Some(name) if !self.rom_path.as_os_str().is_empty() => {
                format!("{name}_savestate_{slot}")
            }
            _ => format!("savestate_{slot}"),
        }
    }

    /// Identity of the cartridge currently loaded, written into every binary
    /// snapshot and required to match on load.
    fn snapshot_header(&self) -> crate::snapshot::Header {
        let console = match self.console_type {
            crate::ffi::ConsoleType::Gbc => 0,
            crate::ffi::ConsoleType::Gba => 1,
            crate::ffi::ConsoleType::Nds => 2,
            _ => u8::MAX,
        };
        let rom = &self.nds_mmu.rom;
        let gamecode = rom
            .get(0x0C..0x10)
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
            .unwrap_or([0; 4]);
        crate::snapshot::Header { console, gamecode, rom_len: rom.len() as u64 }
    }

    /// Write the NDS machine to `slot` as a binary snapshot.
    ///
    /// Uses the same filename as the JSON container (the save menu keys off it);
    /// the container magic distinguishes them on load. Written to a temporary
    /// file and renamed, so a crash mid-write cannot destroy the state already
    /// in the slot.
    fn save_nds_snapshot(&mut self, slot: &str, base_dir: &str) -> String {
        let base = Path::new(base_dir);
        let name = self.slot_basename(slot);
        let (sav, tmp) = (base.join(format!("{name}.sav")), base.join(format!("{name}.tmp")));
        let (safe_sav, safe_tmp) = match (
            crate::rom::validate_path_safety(&sav, base),
            crate::rom::validate_path_safety(&tmp, base),
        ) {
            (Ok(a), Ok(b)) => (a, b),
            _ => return "SAVE_STATE_ERROR Path traversal detected".to_string(),
        };
        if std::env::var("MOCK_DISK_FULL").unwrap_or_default() == "1" {
            return "SAVE_STATE_ERROR Disk full".to_string();
        }

        let mut writer = crate::snapshot::Writer::with_capacity(6 * 1024 * 1024);
        self.snap_nds(&mut writer);
        let file = self.snapshot_header().wrap(&writer.out);
        if let Err(e) = std::fs::write(&safe_tmp, &file) {
            return format!("SAVE_STATE_ERROR {e}");
        }
        if let Err(e) = std::fs::rename(&safe_tmp, &safe_sav) {
            let _ = std::fs::remove_file(&safe_tmp);
            return format!("SAVE_STATE_ERROR {e}");
        }
        "SAVE_STATE_OK".to_string()
    }

    /// Restore the NDS machine from `slot`.
    ///
    /// The payload's hash is verified before any field is applied, so a
    /// truncated or corrupted file is rejected while the running machine is
    /// still untouched. A hash-valid payload written by a *different container
    /// version* is refused by the version check for the same reason: partial
    /// application is the one failure this cannot undo.
    fn load_nds_snapshot(&mut self, safe_sav: &Path) -> String {
        match std::fs::metadata(safe_sav) {
            Ok(m) if m.len() > crate::snapshot::MAX_FILE_BYTES => {
                return "LOAD_STATE_ERROR State file too large".to_string()
            }
            Ok(_) => {}
            Err(e) => return format!("LOAD_STATE_ERROR {e}"),
        }
        let file = match std::fs::read(safe_sav) {
            Ok(f) => f,
            Err(e) => return format!("LOAD_STATE_ERROR {e}"),
        };
        let expect = self.snapshot_header();
        let payload = match crate::snapshot::Header::unwrap_payload(&file, &expect) {
            Ok(p) => p.to_vec(),
            Err(e) => return format!("LOAD_STATE_ERROR {e}"),
        };
        let mut reader = crate::snapshot::Reader::new(&payload);
        self.snap_nds(&mut reader);
        match reader.finish() {
            Ok(()) => "LOAD_STATE_OK".to_string(),
            Err(e) => format!("LOAD_STATE_ERROR {e}"),
        }
    }

    /// True when `file` starts with the binary snapshot magic. Lets `load_state`
    /// route a slot to the right container without parsing it twice.
    fn is_binary_snapshot(path: &Path) -> bool {
        let mut magic = [0u8; crate::snapshot::MAGIC.len()];
        match std::fs::File::open(path) {
            Ok(mut f) => {
                use std::io::Read;
                f.read_exact(&mut magic).is_ok() && magic == crate::snapshot::MAGIC
            }
            Err(_) => false,
        }
    }

    pub fn save_state(&mut self, slot: &str, base_dir: &str) -> String {
        if slot.contains("..") || slot.contains('/') || slot.contains('\\') {
            return "SAVE_STATE_ERROR Path traversal detected".to_string();
        }
        if self.console_type == crate::ffi::ConsoleType::Nds && self.rom_loaded {
            return self.save_nds_snapshot(slot, base_dir);
        }
        let base = Path::new(base_dir);
        let base_name = self.slot_basename(slot);
        let (filename, tmp_filename) =
            (format!("{base_name}.sav"), format!("{base_name}.tmp"));

        let sav_path = base.join(&filename);
        let tmp_path = base.join(&tmp_filename);

        let safe_sav = match crate::rom::validate_path_safety(&sav_path, base) {
            Ok(p) => p,
            Err(_) => return "SAVE_STATE_ERROR Path traversal detected".to_string(),
        };
        let safe_tmp = match crate::rom::validate_path_safety(&tmp_path, base) {
            Ok(p) => p,
            Err(_) => return "SAVE_STATE_ERROR Path traversal detected".to_string(),
        };

        if std::env::var("MOCK_DISK_FULL").unwrap_or_default() == "1" {
            return "SAVE_STATE_ERROR Disk full".to_string();
        }

        let console_str = match self.console_type {
            crate::ffi::ConsoleType::Gbc => "GBC",
            crate::ffi::ConsoleType::Gba => "GBA",
            crate::ffi::ConsoleType::Nds => "NDS",
            _ => "UNKNOWN", // cxx enum: repr is u8, needs a catch-all
        };

        let mut state_json = format!(
            "{{\n  \"console_type\": \"{}\",\n  \"playback_state\": \"{}\",\n  \"ticks\": {},\n  \"player_x\": {},\n  \"player_y\": {},\n  \"buttons\": {{\n    \"up\": {},\n    \"down\": {},\n    \"left\": {},\n    \"right\": {},\n    \"a\": {},\n    \"b\": {},\n    \"start\": {},\n    \"select\": {},\n    \"l\": {},\n    \"r\": {},\n    \"x\": {},\n    \"y\": {},\n    \"nds_touch_x\": {},\n    \"nds_touch_y\": {},\n    \"nds_touch_pressed\": {}\n  }},\n  \"speed\": {},\n  \"frame_skip\": {},\n  \"cpu_cycles\": {},\n  \"rendered_frames\": {}",
            console_str,
            if self.is_playing { "play" } else { "pause" },
            self.ticks,
            self.player_x,
            self.player_y,
            self.buttons.up,
            self.buttons.down,
            self.buttons.left,
            self.buttons.right,
            self.buttons.a,
            self.buttons.b,
            self.buttons.start,
            self.buttons.select,
            self.buttons.l,
            self.buttons.r,
            self.buttons.x,
            self.buttons.y,
            self.buttons.nds_touch_x,
            self.buttons.nds_touch_y,
            self.buttons.nds_touch_pressed,
            self.speed,
            self.frame_skip,
            self.cpu_cycles,
            self.rendered_frames
        );

        if self.console_type == crate::ffi::ConsoleType::Gbc && self.rom_loaded {
            state_json.push_str(&format!(
                ",\n  \"gbc_rom_loaded\": true,\n  \"gbc_cpu_pc\": {},\n  \"gbc_cpu_sp\": {},\n  \"gbc_cpu_a\": {},\n  \"gbc_cpu_f\": {},\n  \"gbc_cpu_b\": {},\n  \"gbc_cpu_c\": {},\n  \"gbc_cpu_d\": {},\n  \"gbc_cpu_e\": {},\n  \"gbc_cpu_h\": {},\n  \"gbc_cpu_l\": {},\n  \"gbc_cpu_ime\": {},\n  \"gbc_cpu_halted\": {},\n  \"gbc_cpu_double_speed\": {},\n  \"gbc_cpu_ei_delay\": {},\n  \"gbc_cpu_stop_mode\": {},\n  \"gbc_cpu_stop_cycles_left\": {},\n  \"gbc_cpu_div_counter\": {},\n  \"gbc_mmu_ie\": {},\n  \"gbc_mmu_bcps\": {},\n  \"gbc_mmu_ocps\": {},\n  \"gbc_mmu_mbc_rom_bank\": {},\n  \"gbc_mmu_mbc_ram_bank_or_rtc_reg\": {},\n  \"gbc_mmu_mbc_ram_rtc_enabled\": {},\n  \"gbc_mmu_mbc_latch_state\": {},\n  \"gbc_mmu_mbc_rtc_seconds\": {},\n  \"gbc_mmu_mbc_rtc_minutes\": {},\n  \"gbc_mmu_mbc_rtc_hours\": {},\n  \"gbc_mmu_mbc_rtc_days\": {},\n  \"gbc_mmu_mbc_rtc_halt\": {},\n  \"gbc_mmu_mbc_rtc_day_overflow\": {},\n  \"gbc_mmu_mbc_rtc_cycle_accumulator\": {},\n  \"gbc_mmu_vram\": \"{}\",\n  \"gbc_mmu_wram\": \"{}\",\n  \"gbc_mmu_oam\": \"{}\",\n  \"gbc_mmu_io\": \"{}\",\n  \"gbc_mmu_hram\": \"{}\",\n  \"gbc_mmu_bg_palette_ram\": \"{}\",\n  \"gbc_mmu_obj_palette_ram\": \"{}\",\n  \"gbc_mmu_mbc_ram\": \"{}\",\n  \"gbc_mmu_mbc_rtc_latched_seconds\": {},\n  \"gbc_mmu_mbc_rtc_latched_minutes\": {},\n  \"gbc_mmu_mbc_rtc_latched_hours\": {},\n  \"gbc_mmu_mbc_rtc_latched_days_low\": {},\n  \"gbc_mmu_mbc_rtc_latched_days_high\": {},\n  \"gbc_ppu_cycle_accumulator\": {},\n  \"gbc_apu_frame_seq_timer\": {},\n  \"gbc_apu_frame_seq_step\": {},\n  \"gbc_apu_ch1_enabled\": {},\n  \"gbc_apu_ch1_duty\": {},\n  \"gbc_apu_ch1_duty_pointer\": {},\n  \"gbc_apu_ch1_length_enabled\": {},\n  \"gbc_apu_ch1_length_counter\": {},\n  \"gbc_apu_ch1_period\": {},\n  \"gbc_apu_ch1_period_timer\": {},\n  \"gbc_apu_ch1_volume\": {},\n  \"gbc_apu_ch1_env_enabled\": {},\n  \"gbc_apu_ch1_env_period\": {},\n  \"gbc_apu_ch1_env_timer\": {},\n  \"gbc_apu_ch1_env_direction\": {},\n  \"gbc_apu_ch1_env_initial_volume\": {},\n  \"gbc_apu_ch1_sweep_enabled\": {},\n  \"gbc_apu_ch1_sweep_period\": {},\n  \"gbc_apu_ch1_sweep_timer\": {},\n  \"gbc_apu_ch1_sweep_shift\": {},\n  \"gbc_apu_ch1_sweep_direction\": {},\n  \"gbc_apu_ch1_shadow_frequency\": {},\n  \"gbc_apu_ch2_enabled\": {},\n  \"gbc_apu_ch2_duty\": {},\n  \"gbc_apu_ch2_duty_pointer\": {},\n  \"gbc_apu_ch2_length_enabled\": {},\n  \"gbc_apu_ch2_length_counter\": {},\n  \"gbc_apu_ch2_period\": {},\n  \"gbc_apu_ch2_period_timer\": {},\n  \"gbc_apu_ch2_volume\": {},\n  \"gbc_apu_ch2_env_enabled\": {},\n  \"gbc_apu_ch2_env_period\": {},\n  \"gbc_apu_ch2_env_timer\": {},\n  \"gbc_apu_ch2_env_direction\": {},\n  \"gbc_apu_ch2_env_initial_volume\": {},\n  \"gbc_apu_ch3_enabled\": {},\n  \"gbc_apu_ch3_dac_enabled\": {},\n  \"gbc_apu_ch3_length_enabled\": {},\n  \"gbc_apu_ch3_length_counter\": {},\n  \"gbc_apu_ch3_period\": {},\n  \"gbc_apu_ch3_period_timer\": {},\n  \"gbc_apu_ch3_volume_shift\": {},\n  \"gbc_apu_ch3_wave_ram\": \"{}\",\n  \"gbc_apu_ch3_sample_pointer\": {},\n  \"gbc_apu_ch4_enabled\": {},\n  \"gbc_apu_ch4_length_enabled\": {},\n  \"gbc_apu_ch4_length_counter\": {},\n  \"gbc_apu_ch4_volume\": {},\n  \"gbc_apu_ch4_env_enabled\": {},\n  \"gbc_apu_ch4_env_period\": {},\n  \"gbc_apu_ch4_env_timer\": {},\n  \"gbc_apu_ch4_env_direction\": {},\n  \"gbc_apu_ch4_env_initial_volume\": {},\n  \"gbc_apu_ch4_lfsr\": {},\n  \"gbc_apu_ch4_divisor\": {},\n  \"gbc_apu_ch4_shift_clock\": {},\n  \"gbc_apu_ch4_width_7bit\": {},\n  \"gbc_apu_ch4_period_timer\": {}",
                self.gbc_cpu.registers.pc,
                self.gbc_cpu.registers.sp,
                self.gbc_cpu.registers.a,
                self.gbc_cpu.registers.f,
                self.gbc_cpu.registers.b,
                self.gbc_cpu.registers.c,
                self.gbc_cpu.registers.d,
                self.gbc_cpu.registers.e,
                self.gbc_cpu.registers.h,
                self.gbc_cpu.registers.l,
                self.gbc_cpu.ime,
                self.gbc_cpu.halted,
                self.gbc_cpu.double_speed,
                self.gbc_cpu.ei_delay,
                self.gbc_cpu.stop_mode,
                self.gbc_cpu.stop_cycles_left,
                self.gbc_cpu.div_counter,
                self.gbc_mmu.ie,
                self.gbc_mmu.bcps,
                self.gbc_mmu.ocps,
                self.gbc_mmu.mbc.rom_bank,
                self.gbc_mmu.mbc.ram_bank_or_rtc_reg,
                self.gbc_mmu.mbc.ram_rtc_enabled,
                self.gbc_mmu.mbc.latch_state,
                self.gbc_mmu.mbc.rtc.seconds,
                self.gbc_mmu.mbc.rtc.minutes,
                self.gbc_mmu.mbc.rtc.hours,
                self.gbc_mmu.mbc.rtc.days,
                self.gbc_mmu.mbc.rtc.halt,
                self.gbc_mmu.mbc.rtc.day_overflow,
                self.gbc_mmu.mbc.rtc.cycle_accumulator,
                to_hex(&self.gbc_mmu.vram),
                to_hex(&self.gbc_mmu.wram),
                to_hex(&self.gbc_mmu.oam),
                to_hex(&self.gbc_mmu.io),
                to_hex(&self.gbc_mmu.hram),
                to_hex(&self.gbc_mmu.bg_palette_ram),
                to_hex(&self.gbc_mmu.obj_palette_ram),
                to_hex(&self.gbc_mmu.mbc.ram),
                self.gbc_mmu.mbc.rtc.latched_seconds,
                self.gbc_mmu.mbc.rtc.latched_minutes,
                self.gbc_mmu.mbc.rtc.latched_hours,
                self.gbc_mmu.mbc.rtc.latched_days_low,
                self.gbc_mmu.mbc.rtc.latched_days_high,
                self.gbc_ppu.cycle_accumulator,
                self.gbc_mmu.apu.frame_seq_timer,
                self.gbc_mmu.apu.frame_seq_step,
                self.gbc_mmu.apu.ch1.enabled,
                self.gbc_mmu.apu.ch1.duty,
                self.gbc_mmu.apu.ch1.duty_pointer,
                self.gbc_mmu.apu.ch1.length_enabled,
                self.gbc_mmu.apu.ch1.length_counter,
                self.gbc_mmu.apu.ch1.period,
                self.gbc_mmu.apu.ch1.period_timer,
                self.gbc_mmu.apu.ch1.volume,
                self.gbc_mmu.apu.ch1.env_enabled,
                self.gbc_mmu.apu.ch1.env_period,
                self.gbc_mmu.apu.ch1.env_timer,
                self.gbc_mmu.apu.ch1.env_direction,
                self.gbc_mmu.apu.ch1.env_initial_volume,
                self.gbc_mmu.apu.ch1.sweep_enabled,
                self.gbc_mmu.apu.ch1.sweep_period,
                self.gbc_mmu.apu.ch1.sweep_timer,
                self.gbc_mmu.apu.ch1.sweep_shift,
                self.gbc_mmu.apu.ch1.sweep_direction,
                self.gbc_mmu.apu.ch1.shadow_frequency,
                self.gbc_mmu.apu.ch2.enabled,
                self.gbc_mmu.apu.ch2.duty,
                self.gbc_mmu.apu.ch2.duty_pointer,
                self.gbc_mmu.apu.ch2.length_enabled,
                self.gbc_mmu.apu.ch2.length_counter,
                self.gbc_mmu.apu.ch2.period,
                self.gbc_mmu.apu.ch2.period_timer,
                self.gbc_mmu.apu.ch2.volume,
                self.gbc_mmu.apu.ch2.env_enabled,
                self.gbc_mmu.apu.ch2.env_period,
                self.gbc_mmu.apu.ch2.env_timer,
                self.gbc_mmu.apu.ch2.env_direction,
                self.gbc_mmu.apu.ch2.env_initial_volume,
                self.gbc_mmu.apu.ch3.enabled,
                self.gbc_mmu.apu.ch3.dac_enabled,
                self.gbc_mmu.apu.ch3.length_enabled,
                self.gbc_mmu.apu.ch3.length_counter,
                self.gbc_mmu.apu.ch3.period,
                self.gbc_mmu.apu.ch3.period_timer,
                self.gbc_mmu.apu.ch3.volume_shift,
                to_hex(&self.gbc_mmu.apu.ch3.wave_ram),
                self.gbc_mmu.apu.ch3.sample_pointer,
                self.gbc_mmu.apu.ch4.enabled,
                self.gbc_mmu.apu.ch4.length_enabled,
                self.gbc_mmu.apu.ch4.length_counter,
                self.gbc_mmu.apu.ch4.volume,
                self.gbc_mmu.apu.ch4.env_enabled,
                self.gbc_mmu.apu.ch4.env_period,
                self.gbc_mmu.apu.ch4.env_timer,
                self.gbc_mmu.apu.ch4.env_direction,
                self.gbc_mmu.apu.ch4.env_initial_volume,
                self.gbc_mmu.apu.ch4.lfsr,
                self.gbc_mmu.apu.ch4.divisor,
                self.gbc_mmu.apu.ch4.shift_clock,
                self.gbc_mmu.apu.ch4.width_7bit,
                self.gbc_mmu.apu.ch4.period_timer,
            ));
        }

        if self.console_type == crate::ffi::ConsoleType::Gba && self.rom_loaded {
            state_json.push_str(&format!(
                ",\n  \"gba_rom_loaded\": true,\n  \"gba_cpu_r0\": {},\n  \"gba_cpu_r1\": {},\n  \"gba_cpu_r2\": {},\n  \"gba_cpu_r3\": {},\n  \"gba_cpu_r4\": {},\n  \"gba_cpu_r5\": {},\n  \"gba_cpu_r6\": {},\n  \"gba_cpu_r7\": {},\n  \"gba_cpu_r8\": {},\n  \"gba_cpu_r9\": {},\n  \"gba_cpu_r10\": {},\n  \"gba_cpu_r11\": {},\n  \"gba_cpu_r12\": {},\n  \"gba_cpu_r13\": {},\n  \"gba_cpu_r14\": {},\n  \"gba_cpu_r15\": {},\n  \"gba_cpu_cpsr\": {},\n  \"gba_cpu_spsr\": {},\n  \"gba_cpu_halted\": {},\n  \"gba_mmu_waitcnt\": {},\n  \"gba_mmu_ie\": {},\n  \"gba_mmu_if\": {},\n  \"gba_mmu_ime\": {},\n  \"gba_flash_bank\": {},\n  \"gba_flash_state\": {},\n  \"gba_mmu_ewram\": \"{}\",\n  \"gba_mmu_iwram\": \"{}\",\n  \"gba_mmu_palette_ram\": \"{}\",\n  \"gba_mmu_vram\": \"{}\",\n  \"gba_mmu_oam\": \"{}\",\n  \"gba_mmu_io\": \"{}\",\n  \"gba_flash_data\": \"{}\",\n  \"gba_cpu_r8_usr\": {:?},\n  \"gba_cpu_r8_fiq\": {:?},\n  \"gba_cpu_r13_usr\": {},\n  \"gba_cpu_r14_usr\": {},\n  \"gba_cpu_r13_svc\": {},\n  \"gba_cpu_r14_svc\": {},\n  \"gba_cpu_spsr_svc\": {},\n  \"gba_cpu_r13_irq\": {},\n  \"gba_cpu_r14_irq\": {},\n  \"gba_cpu_spsr_irq\": {},\n  \"gba_cpu_r13_abt\": {},\n  \"gba_cpu_r14_abt\": {},\n  \"gba_cpu_spsr_abt\": {},\n  \"gba_cpu_r13_und\": {},\n  \"gba_cpu_r14_und\": {},\n  \"gba_cpu_spsr_und\": {},\n  \"gba_cpu_r13_fiq\": {},\n  \"gba_cpu_r14_fiq\": {},\n  \"gba_cpu_spsr_fiq\": {},\n  \"gba_cpu_pipeline\": {:?},\n  \"gba_apu_fifo_a_buffer\": \"{}\",\n  \"gba_apu_fifo_a_write_ptr\": {},\n  \"gba_apu_fifo_a_read_ptr\": {},\n  \"gba_apu_fifo_a_count\": {},\n  \"gba_apu_fifo_b_buffer\": \"{}\",\n  \"gba_apu_fifo_b_write_ptr\": {},\n  \"gba_apu_fifo_b_read_ptr\": {},\n  \"gba_apu_fifo_b_count\": {},\n  \"gba_apu_dma_request_a\": {},\n  \"gba_apu_dma_request_b\": {},\n  \"gba_apu_current_sample_a\": {},\n  \"gba_apu_current_sample_b\": {},\n  \"gba_apu_soundcnt_h\": {},\n  \"gba_apu_soundcnt_x\": {},\n  \"gba_apu_soundbias\": {},\n  \"gba_apu_nr50\": {},\n  \"gba_apu_nr51\": {},\n  \"gba_dma_ch0_sad\": {},\n  \"gba_dma_ch0_dad\": {},\n  \"gba_dma_ch0_count\": {},\n  \"gba_dma_ch0_control\": {},\n  \"gba_dma_ch0_cur_src\": {},\n  \"gba_dma_ch0_cur_dest\": {},\n  \"gba_dma_ch0_cur_count\": {},\n  \"gba_dma_ch0_active\": {},\n  \"gba_dma_ch1_sad\": {},\n  \"gba_dma_ch1_dad\": {},\n  \"gba_dma_ch1_count\": {},\n  \"gba_dma_ch1_control\": {},\n  \"gba_dma_ch1_cur_src\": {},\n  \"gba_dma_ch1_cur_dest\": {},\n  \"gba_dma_ch1_cur_count\": {},\n  \"gba_dma_ch1_active\": {},\n  \"gba_dma_ch2_sad\": {},\n  \"gba_dma_ch2_dad\": {},\n  \"gba_dma_ch2_count\": {},\n  \"gba_dma_ch2_control\": {},\n  \"gba_dma_ch2_cur_src\": {},\n  \"gba_dma_ch2_cur_dest\": {},\n  \"gba_dma_ch2_cur_count\": {},\n  \"gba_dma_ch2_active\": {},\n  \"gba_dma_ch3_sad\": {},\n  \"gba_dma_ch3_dad\": {},\n  \"gba_dma_ch3_count\": {},\n  \"gba_dma_ch3_control\": {},\n  \"gba_dma_ch3_cur_src\": {},\n  \"gba_dma_ch3_cur_dest\": {},\n  \"gba_dma_ch3_cur_count\": {},\n  \"gba_dma_ch3_active\": {},\n  \"gba_timer_ch0_counter\": {},\n  \"gba_timer_ch0_reload\": {},\n  \"gba_timer_ch0_control\": {},\n  \"gba_timer_ch0_cycle_accumulator\": {},\n  \"gba_timer_ch0_overflowed\": {},\n  \"gba_timer_ch1_counter\": {},\n  \"gba_timer_ch1_reload\": {},\n  \"gba_timer_ch1_control\": {},\n  \"gba_timer_ch1_cycle_accumulator\": {},\n  \"gba_timer_ch1_overflowed\": {},\n  \"gba_timer_ch2_counter\": {},\n  \"gba_timer_ch2_reload\": {},\n  \"gba_timer_ch2_control\": {},\n  \"gba_timer_ch2_cycle_accumulator\": {},\n  \"gba_timer_ch2_overflowed\": {},\n  \"gba_timer_ch3_counter\": {},\n  \"gba_timer_ch3_reload\": {},\n  \"gba_timer_ch3_control\": {},\n  \"gba_timer_ch3_cycle_accumulator\": {},\n  \"gba_timer_ch3_overflowed\": {}",
                self.gba_cpu.registers.gpr[0],
                self.gba_cpu.registers.gpr[1],
                self.gba_cpu.registers.gpr[2],
                self.gba_cpu.registers.gpr[3],
                self.gba_cpu.registers.gpr[4],
                self.gba_cpu.registers.gpr[5],
                self.gba_cpu.registers.gpr[6],
                self.gba_cpu.registers.gpr[7],
                self.gba_cpu.registers.gpr[8],
                self.gba_cpu.registers.gpr[9],
                self.gba_cpu.registers.gpr[10],
                self.gba_cpu.registers.gpr[11],
                self.gba_cpu.registers.gpr[12],
                self.gba_cpu.registers.gpr[13],
                self.gba_cpu.registers.gpr[14],
                self.gba_cpu.registers.gpr[15],
                self.gba_cpu.registers.cpsr,
                self.gba_cpu.registers.spsr,
                self.gba_cpu.halted,
                self.gba_mmu.waitcnt,
                self.gba_mmu.ie,
                self.gba_mmu.r_if,
                self.gba_mmu.ime,
                self.gba_mmu.flash.bank,
                self.gba_mmu.flash.state as u32,
                to_hex(&self.gba_mmu.ewram),
                to_hex(&self.gba_mmu.iwram),
                to_hex(&self.gba_mmu.palette_ram),
                to_hex(&self.gba_mmu.vram),
                to_hex(&self.gba_mmu.oam),
                to_hex(&self.gba_mmu.io),
                to_hex(&self.gba_mmu.flash.data),
                self.gba_cpu.registers.r8_usr,
                self.gba_cpu.registers.r8_fiq,
                self.gba_cpu.registers.r13_usr,
                self.gba_cpu.registers.r14_usr,
                self.gba_cpu.registers.r13_svc,
                self.gba_cpu.registers.r14_svc,
                self.gba_cpu.registers.spsr_svc,
                self.gba_cpu.registers.r13_irq,
                self.gba_cpu.registers.r14_irq,
                self.gba_cpu.registers.spsr_irq,
                self.gba_cpu.registers.r13_abt,
                self.gba_cpu.registers.r14_abt,
                self.gba_cpu.registers.spsr_abt,
                self.gba_cpu.registers.r13_und,
                self.gba_cpu.registers.r14_und,
                self.gba_cpu.registers.spsr_und,
                self.gba_cpu.registers.r13_fiq,
                self.gba_cpu.registers.r14_fiq,
                self.gba_cpu.registers.spsr_fiq,
                self.gba_cpu.pipeline,
                to_hex(&self.gba_mmu.apu.fifo_a.buffer.iter().map(|&x| x as u8).collect::<Vec<u8>>()),
                self.gba_mmu.apu.fifo_a.write_ptr,
                self.gba_mmu.apu.fifo_a.read_ptr,
                self.gba_mmu.apu.fifo_a.count,
                to_hex(&self.gba_mmu.apu.fifo_b.buffer.iter().map(|&x| x as u8).collect::<Vec<u8>>()),
                self.gba_mmu.apu.fifo_b.write_ptr,
                self.gba_mmu.apu.fifo_b.read_ptr,
                self.gba_mmu.apu.fifo_b.count,
                self.gba_mmu.apu.dma_request_a,
                self.gba_mmu.apu.dma_request_b,
                self.gba_mmu.apu.current_sample_a,
                self.gba_mmu.apu.current_sample_b,
                self.gba_mmu.apu.soundcnt_h,
                self.gba_mmu.apu.soundcnt_x,
                self.gba_mmu.apu.soundbias,
                self.gba_mmu.apu.nr50,
                self.gba_mmu.apu.nr51,
                self.gba_mmu.dma.channels[0].sad,
                self.gba_mmu.dma.channels[0].dad,
                self.gba_mmu.dma.channels[0].count,
                self.gba_mmu.dma.channels[0].control,
                self.gba_mmu.dma.channels[0].cur_src,
                self.gba_mmu.dma.channels[0].cur_dest,
                self.gba_mmu.dma.channels[0].cur_count,
                self.gba_mmu.dma.channels[0].active,
                self.gba_mmu.dma.channels[1].sad,
                self.gba_mmu.dma.channels[1].dad,
                self.gba_mmu.dma.channels[1].count,
                self.gba_mmu.dma.channels[1].control,
                self.gba_mmu.dma.channels[1].cur_src,
                self.gba_mmu.dma.channels[1].cur_dest,
                self.gba_mmu.dma.channels[1].cur_count,
                self.gba_mmu.dma.channels[1].active,
                self.gba_mmu.dma.channels[2].sad,
                self.gba_mmu.dma.channels[2].dad,
                self.gba_mmu.dma.channels[2].count,
                self.gba_mmu.dma.channels[2].control,
                self.gba_mmu.dma.channels[2].cur_src,
                self.gba_mmu.dma.channels[2].cur_dest,
                self.gba_mmu.dma.channels[2].cur_count,
                self.gba_mmu.dma.channels[2].active,
                self.gba_mmu.dma.channels[3].sad,
                self.gba_mmu.dma.channels[3].dad,
                self.gba_mmu.dma.channels[3].count,
                self.gba_mmu.dma.channels[3].control,
                self.gba_mmu.dma.channels[3].cur_src,
                self.gba_mmu.dma.channels[3].cur_dest,
                self.gba_mmu.dma.channels[3].cur_count,
                self.gba_mmu.dma.channels[3].active,
                self.gba_mmu.timers[0].counter,
                self.gba_mmu.timers[0].reload,
                self.gba_mmu.timers[0].control,
                self.gba_mmu.timers[0].cycle_accumulator,
                self.gba_mmu.timers[0].overflowed,
                self.gba_mmu.timers[1].counter,
                self.gba_mmu.timers[1].reload,
                self.gba_mmu.timers[1].control,
                self.gba_mmu.timers[1].cycle_accumulator,
                self.gba_mmu.timers[1].overflowed,
                self.gba_mmu.timers[2].counter,
                self.gba_mmu.timers[2].reload,
                self.gba_mmu.timers[2].control,
                self.gba_mmu.timers[2].cycle_accumulator,
                self.gba_mmu.timers[2].overflowed,
                self.gba_mmu.timers[3].counter,
                self.gba_mmu.timers[3].reload,
                self.gba_mmu.timers[3].control,
                self.gba_mmu.timers[3].cycle_accumulator,
                self.gba_mmu.timers[3].overflowed,
            ));
        }

        // No NDS arm here: `save_state` routes a loaded NDS ROM to the binary
        // container (`save_nds_snapshot`) before reaching the JSON writer.

        for (key, val) in &self.extra_fields {
            state_json.push_str(&format!(",\n  \"{}\": {}", key, val));
        }
        state_json.push_str("\n}");

        if let Err(e) = atomic_save(&safe_tmp, &safe_sav, &state_json) {
            let _ = std::fs::remove_file(&safe_tmp);
            return format!("SAVE_STATE_ERROR {}", e);
        }

        // A ROM-specific slot also gets a copy under the ROM-less name, so a
        // later session that has not resolved the ROM path can still load it.
        if base_name != format!("savestate_{slot}") {
            let fallback_filename = format!("savestate_{}.sav", slot);
            let fallback_tmp_filename = format!("savestate_{}.tmp", slot);
            let fallback_sav_path = base.join(&fallback_filename);
            let fallback_tmp_path = base.join(&fallback_tmp_filename);
            if let (Ok(fsav), Ok(ftmp)) = (
                crate::rom::validate_path_safety(&fallback_sav_path, base),
                crate::rom::validate_path_safety(&fallback_tmp_path, base)
            ) {
                if let Err(_) = atomic_save(&ftmp, &fsav, &state_json) {
                    let _ = std::fs::remove_file(&ftmp);
                }
            }
        }

        "SAVE_STATE_OK".to_string()
    }

    pub fn load_state(&mut self, slot: &str, base_dir: &str) -> String {
        self.extra_fields.clear();
        if slot.contains("..") || slot.contains('/') || slot.contains('\\') {
            return "LOAD_STATE_ERROR Path traversal detected".to_string();
        }
        let base = Path::new(base_dir);
        let base_name = self.slot_basename(slot);
        // Fall back to the ROM-less filename, so a state written before a ROM
        // path was known is still reachable. A no-op when they are the same.
        let mut sav_path = base.join(format!("{base_name}.sav"));
        if !sav_path.exists() {
            sav_path = base.join(format!("savestate_{slot}.sav"));
        }
        let safe_sav = match crate::rom::validate_path_safety(&sav_path, base) {
            Ok(p) => p,
            Err(_) => return "LOAD_STATE_ERROR Path traversal detected".to_string(),
        };

        if !safe_sav.exists() {
            return "LOAD_STATE_ERROR File not found".to_string();
        }

        // Binary container (NDS): routed before the JSON reader below, which
        // would otherwise try to read megabytes of non-UTF-8 bytes as text.
        if Self::is_binary_snapshot(&safe_sav) {
            if self.console_type != crate::ffi::ConsoleType::Nds || !self.rom_loaded {
                return "LOAD_STATE_ERROR Slot holds an NDS state; load that ROM first"
                    .to_string();
            }
            return self.load_nds_snapshot(&safe_sav);
        }

        let content = match std::fs::read_to_string(&safe_sav) {
            Ok(c) => c,
            Err(e) => return format!("LOAD_STATE_ERROR {}", e),
        };

        if content.len() > 2 * 1024 * 1024 {
            return "LOAD_STATE_ERROR State file too large".to_string();
        }

        self.extra_fields = parse_extra_fields(&content);

        let console_type_str = match get_json_string(&content, "console_type") {
            Some(s) => s,
            None => return "LOAD_STATE_ERROR Missing console_type".to_string(),
        };
        let playback_state_str = match get_json_string(&content, "playback_state") {
            Some(s) => s,
            None => return "LOAD_STATE_ERROR Missing playback_state".to_string(),
        };
        let ticks_str = match get_json_number(&content, "ticks") {
            Some(n) => n,
            None => return "LOAD_STATE_ERROR Missing ticks".to_string(),
        };
        let player_x_str = match get_json_number(&content, "player_x") {
            Some(n) => n,
            None => return "LOAD_STATE_ERROR Missing player_x".to_string(),
        };
        let player_y_str = match get_json_number(&content, "player_y") {
            Some(n) => n,
            None => return "LOAD_STATE_ERROR Missing player_y".to_string(),
        };
        let speed_str = match get_json_number(&content, "speed") {
            Some(n) => n,
            None => return "LOAD_STATE_ERROR Missing speed".to_string(),
        };
        let frame_skip_str = match get_json_number(&content, "frame_skip") {
            Some(n) => n,
            None => return "LOAD_STATE_ERROR Missing frame_skip".to_string(),
        };
        let cpu_cycles_str = match get_json_number(&content, "cpu_cycles") {
            Some(n) => n,
            None => return "LOAD_STATE_ERROR Missing cpu_cycles".to_string(),
        };
        let rendered_frames_str = match get_json_number(&content, "rendered_frames") {
            Some(n) => n,
            None => return "LOAD_STATE_ERROR Missing rendered_frames".to_string(),
        };

        let up = get_json_bool(&content, "up").unwrap_or(false);
        let down = get_json_bool(&content, "down").unwrap_or(false);
        let left = get_json_bool(&content, "left").unwrap_or(false);
        let right = get_json_bool(&content, "right").unwrap_or(false);
        let a = get_json_bool(&content, "a").unwrap_or(false);
        let b = get_json_bool(&content, "b").unwrap_or(false);
        let start = get_json_bool(&content, "start").unwrap_or(false);
        let select = get_json_bool(&content, "select").unwrap_or(false);
        let l = get_json_bool(&content, "l").unwrap_or(false);
        let r = get_json_bool(&content, "r").unwrap_or(false);
        let x = get_json_bool(&content, "x").unwrap_or(false);
        let y = get_json_bool(&content, "y").unwrap_or(false);
        let nds_touch_x = get_json_number(&content, "nds_touch_x")
            .and_then(|n| n.parse::<u16>().ok())
            .unwrap_or(0);
        let nds_touch_y = get_json_number(&content, "nds_touch_y")
            .and_then(|n| n.parse::<u16>().ok())
            .unwrap_or(0);
        let nds_touch_pressed = get_json_bool(&content, "nds_touch_pressed").unwrap_or(false);

        let console_type = match console_type_str.as_str() {
            "GBC" => crate::ffi::ConsoleType::Gbc,
            "GBA" => crate::ffi::ConsoleType::Gba,
            "NDS" => crate::ffi::ConsoleType::Nds,
            _ => return "LOAD_STATE_ERROR Invalid console type".to_string(),
        };
        // Refuse on the FILE's console type too, not just the emulator's. A
        // state written before NDS savestates were withdrawn carries
        // "nds_rom_loaded": true and nothing else, and would otherwise be
        // accepted here — marking a ROM as loaded while restoring no CPU,
        // memory or video state at all.
        if console_type == crate::ffi::ConsoleType::Nds {
            return NDS_LEGACY_PLACEHOLDER.to_string();
        }
        // A state for a different console must not switch the emulator out from
        // under a loaded cartridge: `flush_battery` keys off `console_type`, so
        // the outgoing cartridge's unsaved battery data would be stranded.
        if self.rom_loaded && console_type != self.console_type {
            return "LOAD_STATE_ERROR State is for a different console".to_string();
        }

        let is_playing = match playback_state_str.as_str() {
            "play" => true,
            "pause" => false,
            _ => return "LOAD_STATE_ERROR Invalid playback state".to_string(),
        };

        let ticks = match ticks_str.parse::<u32>() {
            Ok(t) => t,
            Err(_) => return "LOAD_STATE_ERROR Invalid ticks".to_string(),
        };

        let player_x = match player_x_str.parse::<u8>() {
            Ok(x) => x,
            Err(_) => return "LOAD_STATE_ERROR Invalid player_x".to_string(),
        };

        let player_y = match player_y_str.parse::<u8>() {
            Ok(y) => y,
            Err(_) => return "LOAD_STATE_ERROR Invalid player_y".to_string(),
        };

        // Range-checked here, not just parsed: the assignment below writes the
        // field directly, so it is the one path into `speed` that does not go
        // through `Emulator::set_speed`. A savestate is a file, and `speed`
        // scales every console's `cycles_per_sample` — a near-zero value makes
        // `BoxResampler::tick` emit unboundedly many samples for one slice, so
        // an out-of-range number here is a wedged `tick`, not a wrong speed.
        let speed = match speed_str.parse::<f32>() {
            Ok(s) if s.is_finite() && (Self::MIN_SPEED..=Self::MAX_SPEED).contains(&s) => s,
            _ => return "LOAD_STATE_ERROR Invalid speed".to_string(),
        };

        // Range-checked for the same reason as `speed` above: this assignment
        // bypasses `set_frame_skip`, and `tick` divides by `frame_skip + 1`, so
        // a value near `u32::MAX` read off disk is a panic, not a slow redraw.
        let frame_skip = match frame_skip_str.parse::<u32>() {
            Ok(fs) if fs <= Self::MAX_FRAME_SKIP => fs,
            _ => return "LOAD_STATE_ERROR Invalid frame_skip".to_string(),
        };

        let cpu_cycles = match cpu_cycles_str.parse::<u64>() {
            Ok(cc) => cc,
            Err(_) => return "LOAD_STATE_ERROR Invalid cpu_cycles".to_string(),
        };

        let rendered_frames = match rendered_frames_str.parse::<u32>() {
            Ok(rf) => rf,
            Err(_) => return "LOAD_STATE_ERROR Invalid rendered_frames".to_string(),
        };

        // ponytail: this JSON path has no integrity check, unlike the NDS binary
        // snapshot (`load_nds_snapshot` verifies the payload hash before any
        // field is applied). Ceiling: a corrupted or hand-edited `.sav` is
        // applied field by field, so a half-written file leaves the machine
        // holding a mix of two states. The field-level range checks above are
        // what stop that from panicking, which is the part that mattered, and
        // these files live in the user's own config directory — the remaining
        // exposure is corruption, not an attacker. Upgrade path: write the same
        // trailing hash the binary container uses and verify it before the first
        // assignment below; needs a version bump, as existing slots carry none.
        self.console_type = console_type;
        self.is_playing = is_playing;
        self.ticks = ticks;
        self.player_x = player_x;
        self.player_y = player_y;
        self.speed = speed;
        self.frame_skip = frame_skip;
        self.cpu_cycles = cpu_cycles;
        self.rendered_frames = rendered_frames;
        self.buttons = crate::ffi::ButtonState {
            up,
            down,
            left,
            right,
            a,
            b,
            start,
            select,
            l,
            r,
            x,
            y,
            nds_touch_x,
            nds_touch_y,
            nds_touch_pressed,
        };

        if self.console_type == crate::ffi::ConsoleType::Gba {
            self.width = 240;
            self.height = 160;
            if let Some(true) = get_json_bool(&content, "gba_rom_loaded") {
                self.rom_loaded = true;
                // Parse CPU registers
                if let Some(n) = get_json_number(&content, "gba_cpu_r0") {
                    self.gba_cpu.registers.gpr[0] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r1") {
                    self.gba_cpu.registers.gpr[1] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r2") {
                    self.gba_cpu.registers.gpr[2] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r3") {
                    self.gba_cpu.registers.gpr[3] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r4") {
                    self.gba_cpu.registers.gpr[4] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r5") {
                    self.gba_cpu.registers.gpr[5] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r6") {
                    self.gba_cpu.registers.gpr[6] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r7") {
                    self.gba_cpu.registers.gpr[7] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r8") {
                    self.gba_cpu.registers.gpr[8] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r9") {
                    self.gba_cpu.registers.gpr[9] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r10") {
                    self.gba_cpu.registers.gpr[10] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r11") {
                    self.gba_cpu.registers.gpr[11] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r12") {
                    self.gba_cpu.registers.gpr[12] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r13") {
                    self.gba_cpu.registers.gpr[13] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r14") {
                    self.gba_cpu.registers.gpr[14] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r15") {
                    self.gba_cpu.registers.gpr[15] = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_cpsr") {
                    self.gba_cpu.registers.cpsr = n.parse().unwrap_or(0x1F);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_spsr") {
                    self.gba_cpu.registers.spsr = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gba_cpu_halted") {
                    self.gba_cpu.halted = b;
                }

                // Parse MMU simple fields
                if let Some(n) = get_json_number(&content, "gba_mmu_waitcnt") {
                    self.gba_mmu.waitcnt = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_mmu_ie") {
                    self.gba_mmu.ie = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_mmu_if") {
                    self.gba_mmu.r_if = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_mmu_ime") {
                    self.gba_mmu.ime = n.parse().unwrap_or(0);
                }

                // Parse Flash
                if let Some(n) = get_json_number(&content, "gba_flash_bank") {
                    self.gba_mmu.flash.bank = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_flash_state") {
                    let st_val = n.parse().unwrap_or(0);
                    self.gba_mmu.flash.state = match st_val {
                        1 => crate::gba::flash::FlashState::CommandSeq1,
                        2 => crate::gba::flash::FlashState::CommandSeq2,
                        3 => crate::gba::flash::FlashState::Identify,
                        4 => crate::gba::flash::FlashState::Write,
                        5 => crate::gba::flash::FlashState::BankSelect,
                        6 => crate::gba::flash::FlashState::EraseSeq,
                        7 => crate::gba::flash::FlashState::EraseSeq1,
                        8 => crate::gba::flash::FlashState::EraseSeq2,
                        _ => crate::gba::flash::FlashState::Ready,
                    };
                }

                // Hex arrays
                if let Some(hex) = get_json_string(&content, "gba_mmu_ewram") {
                    restore_region(&mut self.gba_mmu.ewram, &hex);
                }
                if let Some(hex) = get_json_string(&content, "gba_mmu_iwram") {
                    restore_region(&mut self.gba_mmu.iwram, &hex);
                }
                if let Some(hex) = get_json_string(&content, "gba_mmu_palette_ram") {
                    let bytes = from_hex(&hex);
                    if bytes.len() == 1024 {
                        self.gba_mmu.palette_ram.copy_from_slice(&bytes);
                    }
                }
                if let Some(hex) = get_json_string(&content, "gba_mmu_vram") {
                    restore_region(&mut self.gba_mmu.vram, &hex);
                }
                if let Some(hex) = get_json_string(&content, "gba_mmu_oam") {
                    let bytes = from_hex(&hex);
                    if bytes.len() == 1024 {
                        self.gba_mmu.oam.copy_from_slice(&bytes);
                    }
                }
                if let Some(hex) = get_json_string(&content, "gba_mmu_io") {
                    let bytes = from_hex(&hex);
                    if bytes.len() == 1024 {
                        self.gba_mmu.io.copy_from_slice(&bytes);
                    }
                }
                if let Some(hex) = get_json_string(&content, "gba_flash_data") {
                    restore_region(&mut self.gba_mmu.flash.data, &hex);
                }

                // Banked registers
                if let Some(arr) = get_json_array_of_numbers(&content, "gba_cpu_r8_usr") {
                    if arr.len() == 5 {
                        self.gba_cpu.registers.r8_usr.copy_from_slice(&arr);
                    }
                }
                if let Some(arr) = get_json_array_of_numbers(&content, "gba_cpu_r8_fiq") {
                    if arr.len() == 5 {
                        self.gba_cpu.registers.r8_fiq.copy_from_slice(&arr);
                    }
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r13_usr") {
                    self.gba_cpu.registers.r13_usr = n.parse().unwrap_or(0x03007F00);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r14_usr") {
                    self.gba_cpu.registers.r14_usr = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r13_svc") {
                    self.gba_cpu.registers.r13_svc = n.parse().unwrap_or(0x03007FE0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r14_svc") {
                    self.gba_cpu.registers.r14_svc = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_spsr_svc") {
                    self.gba_cpu.registers.spsr_svc = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r13_irq") {
                    self.gba_cpu.registers.r13_irq = n.parse().unwrap_or(0x03007FA0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r14_irq") {
                    self.gba_cpu.registers.r14_irq = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_spsr_irq") {
                    self.gba_cpu.registers.spsr_irq = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r13_abt") {
                    self.gba_cpu.registers.r13_abt = n.parse().unwrap_or(0x03007FA0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r14_abt") {
                    self.gba_cpu.registers.r14_abt = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_spsr_abt") {
                    self.gba_cpu.registers.spsr_abt = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r13_und") {
                    self.gba_cpu.registers.r13_und = n.parse().unwrap_or(0x03007FA0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r14_und") {
                    self.gba_cpu.registers.r14_und = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_spsr_und") {
                    self.gba_cpu.registers.spsr_und = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r13_fiq") {
                    self.gba_cpu.registers.r13_fiq = n.parse().unwrap_or(0x03007FA0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r14_fiq") {
                    self.gba_cpu.registers.r14_fiq = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_spsr_fiq") {
                    self.gba_cpu.registers.spsr_fiq = n.parse().unwrap_or(0);
                }

                // Pipeline
                if let Some(arr) = get_json_array_of_numbers(&content, "gba_cpu_pipeline") {
                    if arr.len() == 2 {
                        self.gba_cpu.pipeline.copy_from_slice(&arr);
                    }
                }

                // APU FIFO A
                if let Some(hex) = get_json_string(&content, "gba_apu_fifo_a_buffer") {
                    let bytes = from_hex(&hex);
                    for (idx, &b) in bytes.iter().enumerate() {
                        if idx < 32 {
                            self.gba_mmu.apu.fifo_a.buffer[idx] = b as i8;
                        }
                    }
                }
                if let Some(n) = get_json_number(&content, "gba_apu_fifo_a_write_ptr") {
                    self.gba_mmu.apu.fifo_a.write_ptr = n.parse().unwrap_or(0);
                    self.gba_mmu.apu.fifo_a.write_ptr = self.gba_mmu.apu.fifo_a.write_ptr.min(31);
                }
                if let Some(n) = get_json_number(&content, "gba_apu_fifo_a_read_ptr") {
                    self.gba_mmu.apu.fifo_a.read_ptr = n.parse().unwrap_or(0);
                    self.gba_mmu.apu.fifo_a.read_ptr = self.gba_mmu.apu.fifo_a.read_ptr.min(31);
                }
                if let Some(n) = get_json_number(&content, "gba_apu_fifo_a_count") {
                    self.gba_mmu.apu.fifo_a.count = n.parse().unwrap_or(0);
                    self.gba_mmu.apu.fifo_a.count = self.gba_mmu.apu.fifo_a.count.min(32);
                }

                // APU FIFO B
                if let Some(hex) = get_json_string(&content, "gba_apu_fifo_b_buffer") {
                    let bytes = from_hex(&hex);
                    for (idx, &b) in bytes.iter().enumerate() {
                        if idx < 32 {
                            self.gba_mmu.apu.fifo_b.buffer[idx] = b as i8;
                        }
                    }
                }
                if let Some(n) = get_json_number(&content, "gba_apu_fifo_b_write_ptr") {
                    self.gba_mmu.apu.fifo_b.write_ptr = n.parse().unwrap_or(0);
                    self.gba_mmu.apu.fifo_b.write_ptr = self.gba_mmu.apu.fifo_b.write_ptr.min(31);
                }
                if let Some(n) = get_json_number(&content, "gba_apu_fifo_b_read_ptr") {
                    self.gba_mmu.apu.fifo_b.read_ptr = n.parse().unwrap_or(0);
                    self.gba_mmu.apu.fifo_b.read_ptr = self.gba_mmu.apu.fifo_b.read_ptr.min(31);
                }
                if let Some(n) = get_json_number(&content, "gba_apu_fifo_b_count") {
                    self.gba_mmu.apu.fifo_b.count = n.parse().unwrap_or(0);
                    self.gba_mmu.apu.fifo_b.count = self.gba_mmu.apu.fifo_b.count.min(32);
                }

                // APU Simple
                if let Some(b) = get_json_bool(&content, "gba_apu_dma_request_a") {
                    self.gba_mmu.apu.dma_request_a = b;
                }
                if let Some(b) = get_json_bool(&content, "gba_apu_dma_request_b") {
                    self.gba_mmu.apu.dma_request_b = b;
                }
                if let Some(n) = get_json_number(&content, "gba_apu_current_sample_a") {
                    self.gba_mmu.apu.current_sample_a = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_apu_current_sample_b") {
                    self.gba_mmu.apu.current_sample_b = n.parse().unwrap_or(0);
                }
                // The mixer registers. Without these a loaded state played in
                // TOTAL SILENCE: `load_state` performs no reset, so the APU is
                // whatever `reset_on_rom_load` built (`GbaApu::new()`, which
                // leaves soundcnt_h = nr50 = nr51 = 0 -- DirectSound disabled and
                // master volume zero), and nothing rewrites them until the game
                // next touches its sound driver. The FIFOs and DMA request flags
                // beside them were already carried, which is what made the gap
                // easy to miss: the samples were saved, the enables were not.
                //
                // ponytail: the four PSG channels are still not carried. Ceiling:
                // a state restored mid-note resumes that note only once the game
                // rewrites NRx0-NRx4, which for GBA titles is a small part of the
                // mix (DirectSound carries the music). Upgrade path: serialize
                // them the way the GBC path already does for its own channels.
                if let Some(n) = get_json_number(&content, "gba_apu_soundcnt_h") {
                    self.gba_mmu.apu.soundcnt_h = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_apu_soundcnt_x") {
                    self.gba_mmu.apu.soundcnt_x = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_apu_soundbias") {
                    self.gba_mmu.apu.soundbias = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_apu_nr50") {
                    self.gba_mmu.apu.nr50 = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gba_apu_nr51") {
                    self.gba_mmu.apu.nr51 = n.parse().unwrap_or(0);
                }
                // The DS interp state is transient (not serialized), but prev must be
                // re-synced to the loaded latch: a ramp from another session's stale
                // prev would be an audible tick on the first post-load FIFO period.
                // The counters/periods need no sync — the interp frac clamp bounds them.
                self.gba_mmu.apu.prev_sample_a = self.gba_mmu.apu.current_sample_a;
                self.gba_mmu.apu.prev_sample_b = self.gba_mmu.apu.current_sample_b;

                // DMAs
                for ch in 0..4 {
                    let prefix = format!("gba_dma_ch{}", ch);
                    if let Some(n) = get_json_number(&content, &format!("{}_sad", prefix)) {
                        self.gba_mmu.dma.channels[ch].sad = n.parse().unwrap_or(0);
                    }
                    if let Some(n) = get_json_number(&content, &format!("{}_dad", prefix)) {
                        self.gba_mmu.dma.channels[ch].dad = n.parse().unwrap_or(0);
                    }
                    if let Some(n) = get_json_number(&content, &format!("{}_count", prefix)) {
                        self.gba_mmu.dma.channels[ch].count = n.parse().unwrap_or(0);
                    }
                    if let Some(n) = get_json_number(&content, &format!("{}_control", prefix)) {
                        self.gba_mmu.dma.channels[ch].control = n.parse().unwrap_or(0);
                    }
                    if let Some(n) = get_json_number(&content, &format!("{}_cur_src", prefix)) {
                        self.gba_mmu.dma.channels[ch].cur_src = n.parse().unwrap_or(0);
                    }
                    if let Some(n) = get_json_number(&content, &format!("{}_cur_dest", prefix)) {
                        self.gba_mmu.dma.channels[ch].cur_dest = n.parse().unwrap_or(0);
                    }
                    if let Some(n) = get_json_number(&content, &format!("{}_cur_count", prefix)) {
                        self.gba_mmu.dma.channels[ch].cur_count = n.parse().unwrap_or(0);
                    }
                    if let Some(b) = get_json_bool(&content, &format!("{}_active", prefix)) {
                        self.gba_mmu.dma.channels[ch].active = b;
                    }
                }

                // Timers
                for tmr in 0..4 {
                    let prefix = format!("gba_timer_ch{}", tmr);
                    if let Some(n) = get_json_number(&content, &format!("{}_counter", prefix)) {
                        self.gba_mmu.timers[tmr].counter = n.parse().unwrap_or(0);
                    }
                    if let Some(n) = get_json_number(&content, &format!("{}_reload", prefix)) {
                        self.gba_mmu.timers[tmr].reload = n.parse().unwrap_or(0);
                    }
                    if let Some(n) = get_json_number(&content, &format!("{}_control", prefix)) {
                        self.gba_mmu.timers[tmr].control = n.parse().unwrap_or(0);
                    }
                    if let Some(n) =
                        get_json_number(&content, &format!("{}_cycle_accumulator", prefix))
                    {
                        self.gba_mmu.timers[tmr].cycle_accumulator = n.parse().unwrap_or(0);
                    }
                    if let Some(b) = get_json_bool(&content, &format!("{}_overflowed", prefix)) {
                        self.gba_mmu.timers[tmr].overflowed = b;
                    }
                }
            } else {
                self.rom_loaded = false;
            }
        } else if self.console_type == crate::ffi::ConsoleType::Nds {
            // Only reachable for a legacy JSON placeholder, which is refused
            // above; a real NDS state took the binary path and never gets here.
            // Kept as a guard so a future caller cannot reach the old behaviour
            // of trusting "nds_rom_loaded".
            self.width = 256;
            self.height = 384;
            self.rom_loaded = false;
        } else {
            self.width = 160;
            self.height = 144;

            if let Some(true) = get_json_bool(&content, "gbc_rom_loaded") {
                self.rom_loaded = true;
                // Parse CPU registers
                if let Some(n) = get_json_number(&content, "gbc_cpu_pc") {
                    self.gbc_cpu.registers.pc = n.parse().unwrap_or(0x100);
                }
                if let Some(n) = get_json_number(&content, "gbc_cpu_sp") {
                    self.gbc_cpu.registers.sp = n.parse().unwrap_or(0xFFFE);
                }
                if let Some(n) = get_json_number(&content, "gbc_cpu_a") {
                    self.gbc_cpu.registers.a = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_cpu_f") {
                    self.gbc_cpu.registers.f = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_cpu_b") {
                    self.gbc_cpu.registers.b = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_cpu_c") {
                    self.gbc_cpu.registers.c = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_cpu_d") {
                    self.gbc_cpu.registers.d = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_cpu_e") {
                    self.gbc_cpu.registers.e = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_cpu_h") {
                    self.gbc_cpu.registers.h = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_cpu_l") {
                    self.gbc_cpu.registers.l = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gbc_cpu_ime") {
                    self.gbc_cpu.ime = b;
                }
                if let Some(b) = get_json_bool(&content, "gbc_cpu_halted") {
                    self.gbc_cpu.halted = b;
                }
                if let Some(b) = get_json_bool(&content, "gbc_cpu_double_speed") {
                    self.gbc_cpu.double_speed = b;
                }
                if let Some(b) = get_json_bool(&content, "gbc_cpu_ei_delay") {
                    self.gbc_cpu.ei_delay = b;
                }
                if let Some(b) = get_json_bool(&content, "gbc_cpu_stop_mode") {
                    self.gbc_cpu.stop_mode = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_cpu_stop_cycles_left") {
                    self.gbc_cpu.stop_cycles_left = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_cpu_div_counter") {
                    self.gbc_cpu.div_counter = n.parse().unwrap_or(0);
                }

                // Parse MMU simple fields
                if let Some(n) = get_json_number(&content, "gbc_mmu_ie") {
                    self.gbc_mmu.ie = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_mmu_bcps") {
                    self.gbc_mmu.bcps = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_mmu_ocps") {
                    self.gbc_mmu.ocps = n.parse().unwrap_or(0);
                }

                // Parse MBC
                if let Some(n) = get_json_number(&content, "gbc_mmu_mbc_rom_bank") {
                    self.gbc_mmu.mbc.rom_bank = n.parse().unwrap_or(1);
                }
                if let Some(n) = get_json_number(&content, "gbc_mmu_mbc_ram_bank_or_rtc_reg") {
                    self.gbc_mmu.mbc.ram_bank_or_rtc_reg = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gbc_mmu_mbc_ram_rtc_enabled") {
                    self.gbc_mmu.mbc.ram_rtc_enabled = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_mmu_mbc_latch_state") {
                    self.gbc_mmu.mbc.latch_state = n.parse().unwrap_or(0xFF);
                }

                // Parse RTC
                if let Some(n) = get_json_number(&content, "gbc_mmu_mbc_rtc_seconds") {
                    self.gbc_mmu.mbc.rtc.seconds = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_mmu_mbc_rtc_minutes") {
                    self.gbc_mmu.mbc.rtc.minutes = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_mmu_mbc_rtc_hours") {
                    self.gbc_mmu.mbc.rtc.hours = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_mmu_mbc_rtc_days") {
                    self.gbc_mmu.mbc.rtc.days = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gbc_mmu_mbc_rtc_halt") {
                    self.gbc_mmu.mbc.rtc.halt = b;
                }
                if let Some(b) = get_json_bool(&content, "gbc_mmu_mbc_rtc_day_overflow") {
                    self.gbc_mmu.mbc.rtc.day_overflow = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_mmu_mbc_rtc_cycle_accumulator") {
                    self.gbc_mmu.mbc.rtc.cycle_accumulator = n.parse().unwrap_or(0);
                }

                // Hex arrays
                if let Some(hex) = get_json_string(&content, "gbc_mmu_vram") {
                    restore_region(&mut self.gbc_mmu.vram, &hex);
                }
                if let Some(hex) = get_json_string(&content, "gbc_mmu_wram") {
                    restore_region(&mut self.gbc_mmu.wram, &hex);
                }
                if let Some(hex) = get_json_string(&content, "gbc_mmu_oam") {
                    let bytes = from_hex(&hex);
                    if bytes.len() == 160 {
                        self.gbc_mmu.oam.copy_from_slice(&bytes);
                    }
                }
                if let Some(hex) = get_json_string(&content, "gbc_mmu_io") {
                    let bytes = from_hex(&hex);
                    if bytes.len() == 128 {
                        self.gbc_mmu.io.copy_from_slice(&bytes);
                    }
                }
                if let Some(hex) = get_json_string(&content, "gbc_mmu_hram") {
                    let bytes = from_hex(&hex);
                    if bytes.len() == 127 {
                        self.gbc_mmu.hram.copy_from_slice(&bytes);
                    }
                }
                if let Some(hex) = get_json_string(&content, "gbc_mmu_bg_palette_ram") {
                    let bytes = from_hex(&hex);
                    if bytes.len() == 64 {
                        self.gbc_mmu.bg_palette_ram.copy_from_slice(&bytes);
                    }
                }
                if let Some(hex) = get_json_string(&content, "gbc_mmu_obj_palette_ram") {
                    let bytes = from_hex(&hex);
                    if bytes.len() == 64 {
                        self.gbc_mmu.obj_palette_ram.copy_from_slice(&bytes);
                    }
                }

                // Restore missing GBC states
                if let Some(hex) = get_json_string(&content, "gbc_mmu_mbc_ram") {
                    restore_region(&mut self.gbc_mmu.mbc.ram, &hex);
                }
                if let Some(n) = get_json_number(&content, "gbc_mmu_mbc_rtc_latched_seconds") {
                    self.gbc_mmu.mbc.rtc.latched_seconds = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_mmu_mbc_rtc_latched_minutes") {
                    self.gbc_mmu.mbc.rtc.latched_minutes = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_mmu_mbc_rtc_latched_hours") {
                    self.gbc_mmu.mbc.rtc.latched_hours = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_mmu_mbc_rtc_latched_days_low") {
                    self.gbc_mmu.mbc.rtc.latched_days_low = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_mmu_mbc_rtc_latched_days_high") {
                    self.gbc_mmu.mbc.rtc.latched_days_high = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_ppu_cycle_accumulator") {
                    self.gbc_ppu.cycle_accumulator = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_frame_seq_timer") {
                    self.gbc_mmu.apu.frame_seq_timer = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_frame_seq_step") {
                    self.gbc_mmu.apu.frame_seq_step = n.parse().unwrap_or(0);
                }

                // APU Ch1
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch1_enabled") {
                    self.gbc_mmu.apu.ch1.enabled = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch1_duty") {
                    self.gbc_mmu.apu.ch1.duty = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch1_duty_pointer") {
                    self.gbc_mmu.apu.ch1.duty_pointer = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch1_length_enabled") {
                    self.gbc_mmu.apu.ch1.length_enabled = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch1_length_counter") {
                    self.gbc_mmu.apu.ch1.length_counter = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch1_period") {
                    self.gbc_mmu.apu.ch1.period = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch1_period_timer") {
                    self.gbc_mmu.apu.ch1.period_timer = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch1_volume") {
                    self.gbc_mmu.apu.ch1.volume = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch1_env_enabled") {
                    self.gbc_mmu.apu.ch1.env_enabled = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch1_env_period") {
                    self.gbc_mmu.apu.ch1.env_period = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch1_env_timer") {
                    self.gbc_mmu.apu.ch1.env_timer = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch1_env_direction") {
                    self.gbc_mmu.apu.ch1.env_direction = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch1_env_initial_volume") {
                    self.gbc_mmu.apu.ch1.env_initial_volume = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch1_sweep_enabled") {
                    self.gbc_mmu.apu.ch1.sweep_enabled = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch1_sweep_period") {
                    self.gbc_mmu.apu.ch1.sweep_period = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch1_sweep_timer") {
                    self.gbc_mmu.apu.ch1.sweep_timer = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch1_sweep_shift") {
                    self.gbc_mmu.apu.ch1.sweep_shift = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch1_sweep_direction") {
                    self.gbc_mmu.apu.ch1.sweep_direction = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch1_shadow_frequency") {
                    self.gbc_mmu.apu.ch1.shadow_frequency = n.parse().unwrap_or(0);
                }

                // APU Ch2
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch2_enabled") {
                    self.gbc_mmu.apu.ch2.enabled = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch2_duty") {
                    self.gbc_mmu.apu.ch2.duty = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch2_duty_pointer") {
                    self.gbc_mmu.apu.ch2.duty_pointer = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch2_length_enabled") {
                    self.gbc_mmu.apu.ch2.length_enabled = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch2_length_counter") {
                    self.gbc_mmu.apu.ch2.length_counter = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch2_period") {
                    self.gbc_mmu.apu.ch2.period = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch2_period_timer") {
                    self.gbc_mmu.apu.ch2.period_timer = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch2_volume") {
                    self.gbc_mmu.apu.ch2.volume = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch2_env_enabled") {
                    self.gbc_mmu.apu.ch2.env_enabled = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch2_env_period") {
                    self.gbc_mmu.apu.ch2.env_period = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch2_env_timer") {
                    self.gbc_mmu.apu.ch2.env_timer = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch2_env_direction") {
                    self.gbc_mmu.apu.ch2.env_direction = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch2_env_initial_volume") {
                    self.gbc_mmu.apu.ch2.env_initial_volume = n.parse().unwrap_or(0);
                }

                // APU Ch3
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch3_enabled") {
                    self.gbc_mmu.apu.ch3.enabled = b;
                }
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch3_dac_enabled") {
                    self.gbc_mmu.apu.ch3.dac_enabled = b;
                }
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch3_length_enabled") {
                    self.gbc_mmu.apu.ch3.length_enabled = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch3_length_counter") {
                    self.gbc_mmu.apu.ch3.length_counter = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch3_period") {
                    self.gbc_mmu.apu.ch3.period = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch3_period_timer") {
                    self.gbc_mmu.apu.ch3.period_timer = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch3_volume_shift") {
                    self.gbc_mmu.apu.ch3.volume_shift = n.parse().unwrap_or(0);
                }
                if let Some(hex) = get_json_string(&content, "gbc_apu_ch3_wave_ram") {
                    let bytes = from_hex(&hex);
                    for (idx, &b) in bytes.iter().enumerate() {
                        if idx < 16 {
                            self.gbc_mmu.apu.ch3.wave_ram[idx] = b;
                        }
                    }
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch3_sample_pointer") {
                    self.gbc_mmu.apu.ch3.sample_pointer = n.parse().unwrap_or(0);
                }

                // APU Ch4
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch4_enabled") {
                    self.gbc_mmu.apu.ch4.enabled = b;
                }
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch4_length_enabled") {
                    self.gbc_mmu.apu.ch4.length_enabled = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch4_length_counter") {
                    self.gbc_mmu.apu.ch4.length_counter = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch4_volume") {
                    self.gbc_mmu.apu.ch4.volume = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch4_env_enabled") {
                    self.gbc_mmu.apu.ch4.env_enabled = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch4_env_period") {
                    self.gbc_mmu.apu.ch4.env_period = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch4_env_timer") {
                    self.gbc_mmu.apu.ch4.env_timer = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch4_env_direction") {
                    self.gbc_mmu.apu.ch4.env_direction = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch4_env_initial_volume") {
                    self.gbc_mmu.apu.ch4.env_initial_volume = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch4_lfsr") {
                    self.gbc_mmu.apu.ch4.lfsr = n.parse().unwrap_or(0x7FFF);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch4_divisor") {
                    self.gbc_mmu.apu.ch4.divisor = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch4_shift_clock") {
                    self.gbc_mmu.apu.ch4.shift_clock = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "gbc_apu_ch4_width_7bit") {
                    self.gbc_mmu.apu.ch4.width_7bit = b;
                }
                if let Some(n) = get_json_number(&content, "gbc_apu_ch4_period_timer") {
                    self.gbc_mmu.apu.ch4.period_timer = n.parse().unwrap_or(0);
                }
            } else {
                self.rom_loaded = false;
            }
        }

        "LOAD_STATE_OK".to_string()
    }
}
