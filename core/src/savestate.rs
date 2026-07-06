use crate::emulator::Emulator;
use std::path::Path;

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn from_hex(hex: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut chars = hex.chars();
    while let (Some(c1), Some(c2)) = (chars.next(), chars.next()) {
        if let (Some(d1), Some(d2)) = (c1.to_digit(16), c2.to_digit(16)) {
            bytes.push(((d1 << 4) | d2) as u8);
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
        "n64_rom_loaded", "n64_rdram", "expansion_pak", "stick_x", "stick_y",
        // N64 CPU / CP0 / CP1 / TLB keys:
        "n64_cpu_gpr", "n64_cpu_pc", "n64_cpu_hi", "n64_cpu_lo",
        "n64_cpu_fgr", "n64_cpu_fcr31", "n64_cpu_fcr0",
        "n64_cpu_in_delay_slot", "n64_cpu_delay_slot_pc", "n64_cpu_nullify_delay_slot",
        "n64_cp0_index", "n64_cp0_random", "n64_cp0_entry_lo0", "n64_cp0_entry_lo1", "n64_cp0_context",
        "n64_cp0_page_mask", "n64_cp0_wired", "n64_cp0_bad_vaddr", "n64_cp0_count", "n64_cp0_entry_hi",
        "n64_cp0_compare", "n64_cp0_status", "n64_cp0_cause", "n64_cp0_epc", "n64_cp0_prid", "n64_cp0_config",
        "n64_cp0_error_epc", "n64_cpu_tlb",
        // N64 RSP keys:
        "n64_rsp_gpr", "n64_rsp_pc", "n64_rsp_vpr", "n64_rsp_acc", "n64_rsp_vco", "n64_rsp_vcc", "n64_rsp_vce",
        "n64_rsp_status", "n64_rsp_semaphore", "n64_rsp_halted", "n64_rsp_broke", "n64_rsp_single_step",
        "n64_rsp_intr_on_break", "n64_rsp_signals", "n64_rsp_dma_busy", "n64_rsp_dma_full", "n64_rsp_in_delay_slot",
        "n64_rsp_delay_slot_pc", "n64_rsp_sp_mem_addr", "n64_rsp_sp_dram_addr", "n64_rsp_sp_rd_len", "n64_rsp_sp_wr_len",
        // N64 RDP keys:
        "n64_rdp_tmem", "n64_rdp_color_image_addr", "n64_rdp_color_image_format", "n64_rdp_color_image_size",
        "n64_rdp_color_image_width", "n64_rdp_depth_image_addr", "n64_rdp_texture_image_addr",
        "n64_rdp_texture_image_format", "n64_rdp_texture_image_size", "n64_rdp_texture_image_width",
        "n64_rdp_tiles", "n64_rdp_scissor_xh", "n64_rdp_scissor_yh", "n64_rdp_scissor_xl", "n64_rdp_scissor_yl",
        "n64_rdp_fill_color", "n64_rdp_blend_color", "n64_rdp_fog_color", "n64_rdp_prim_color", "n64_rdp_env_color",
        "n64_rdp_cycle_type",
        // N64 DMEM/IMEM & MMIO keys:
        "n64_dmem", "n64_imem",
        "n64_rdram_regs", "n64_sp_regs", "n64_sp_pc_regs", "n64_dpc_regs", "n64_dps_regs", "n64_mi_regs", "n64_vi_regs",
        "n64_ai_regs", "n64_pi_regs", "n64_si_regs", "n64_ri_regs", "n64_pif_ram",
        // N64 Other MMU states:
        "n64_mmu_vi_cycles", "n64_mmu_vi_intr_triggered", "n64_mmu_ai_dma_address", "n64_mmu_ai_dma_length",
        "n64_mmu_ai_dma_count", "n64_mmu_ai_dma_len_remaining", "n64_mmu_ai_dma_byte_accumulator",
        "n64_mmu_ai_programmed_addr", "n64_mmu_ai_control", "n64_mmu_ai_dacrate", "n64_mmu_ai_bitrate",
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
        "gbc_apu_ch1_sweep_shift", "gbc_apu_ch1_sweep_direction", "gbc_apu_ch1_sweep_frequency",
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

fn allocate_aligned_local<T: Default + Clone>(size: usize, align: usize) -> (Vec<T>, usize) {
    let mut vec = vec![T::default(); size + align];
    let ptr = vec.as_ptr() as usize;
    let offset = if ptr % align == 0 {
        0
    } else {
        (align - (ptr % align)) / std::mem::size_of::<T>()
    };
    (vec, offset)
}

impl Emulator {
    pub fn save_state(&self, slot: &str, base_dir: &str) -> String {
        if slot.contains("..") || slot.contains('/') || slot.contains('\\') {
            return "SAVE_STATE_ERROR Path traversal detected".to_string();
        }
        let base = Path::new(base_dir);
        let rom_name = if !self.rom_path.as_os_str().is_empty() {
            self.rom_path.file_stem().and_then(|s| s.to_str())
        } else {
            None
        };
        let (filename, tmp_filename) = if let Some(name) = rom_name {
            (format!("{}_savestate_{}.sav", name, slot), format!("{}_savestate_{}.tmp", name, slot))
        } else {
            (format!("savestate_{}.sav", slot), format!("savestate_{}.tmp", slot))
        };

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
            crate::ffi::ConsoleType::Nintendo64 => "N64",
        };

        let mut state_json = format!(
            "{{\n  \"console_type\": \"{}\",\n  \"playback_state\": \"{}\",\n  \"ticks\": {},\n  \"player_x\": {},\n  \"player_y\": {},\n  \"buttons\": {{\n    \"up\": {},\n    \"down\": {},\n    \"left\": {},\n    \"right\": {},\n    \"a\": {},\n    \"b\": {},\n    \"start\": {},\n    \"select\": {},\n    \"l\": {},\n    \"r\": {},\n    \"z\": {},\n    \"c_up\": {},\n    \"c_down\": {},\n    \"c_left\": {},\n    \"c_right\": {}\n  }},\n  \"speed\": {},\n  \"frame_skip\": {},\n  \"cpu_cycles\": {},\n  \"rendered_frames\": {}",
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
            self.buttons.z,
            self.buttons.c_up,
            self.buttons.c_down,
            self.buttons.c_left,
            self.buttons.c_right,
            self.speed,
            self.frame_skip,
            self.cpu_cycles,
            self.rendered_frames
        );

        if self.console_type == crate::ffi::ConsoleType::Gbc && self.rom_loaded {
            state_json.push_str(&format!(
                ",\n  \"gbc_rom_loaded\": true,\n  \"gbc_cpu_pc\": {},\n  \"gbc_cpu_sp\": {},\n  \"gbc_cpu_a\": {},\n  \"gbc_cpu_f\": {},\n  \"gbc_cpu_b\": {},\n  \"gbc_cpu_c\": {},\n  \"gbc_cpu_d\": {},\n  \"gbc_cpu_e\": {},\n  \"gbc_cpu_h\": {},\n  \"gbc_cpu_l\": {},\n  \"gbc_cpu_ime\": {},\n  \"gbc_cpu_halted\": {},\n  \"gbc_cpu_double_speed\": {},\n  \"gbc_cpu_ei_delay\": {},\n  \"gbc_cpu_stop_mode\": {},\n  \"gbc_cpu_stop_cycles_left\": {},\n  \"gbc_cpu_div_counter\": {},\n  \"gbc_mmu_ie\": {},\n  \"gbc_mmu_bcps\": {},\n  \"gbc_mmu_ocps\": {},\n  \"gbc_mmu_mbc_rom_bank\": {},\n  \"gbc_mmu_mbc_ram_bank_or_rtc_reg\": {},\n  \"gbc_mmu_mbc_ram_rtc_enabled\": {},\n  \"gbc_mmu_mbc_latch_state\": {},\n  \"gbc_mmu_mbc_rtc_seconds\": {},\n  \"gbc_mmu_mbc_rtc_minutes\": {},\n  \"gbc_mmu_mbc_rtc_hours\": {},\n  \"gbc_mmu_mbc_rtc_days\": {},\n  \"gbc_mmu_mbc_rtc_halt\": {},\n  \"gbc_mmu_mbc_rtc_day_overflow\": {},\n  \"gbc_mmu_mbc_rtc_cycle_accumulator\": {},\n  \"gbc_mmu_vram\": \"{}\",\n  \"gbc_mmu_wram\": \"{}\",\n  \"gbc_mmu_oam\": \"{}\",\n  \"gbc_mmu_io\": \"{}\",\n  \"gbc_mmu_hram\": \"{}\",\n  \"gbc_mmu_bg_palette_ram\": \"{}\",\n  \"gbc_mmu_obj_palette_ram\": \"{}\",\n  \"gbc_mmu_mbc_ram\": \"{}\",\n  \"gbc_mmu_mbc_rtc_latched_seconds\": {},\n  \"gbc_mmu_mbc_rtc_latched_minutes\": {},\n  \"gbc_mmu_mbc_rtc_latched_hours\": {},\n  \"gbc_mmu_mbc_rtc_latched_days_low\": {},\n  \"gbc_mmu_mbc_rtc_latched_days_high\": {},\n  \"gbc_ppu_cycle_accumulator\": {},\n  \"gbc_apu_frame_seq_timer\": {},\n  \"gbc_apu_frame_seq_step\": {},\n  \"gbc_apu_ch1_enabled\": {},\n  \"gbc_apu_ch1_duty\": {},\n  \"gbc_apu_ch1_duty_pointer\": {},\n  \"gbc_apu_ch1_length_enabled\": {},\n  \"gbc_apu_ch1_length_counter\": {},\n  \"gbc_apu_ch1_period\": {},\n  \"gbc_apu_ch1_period_timer\": {},\n  \"gbc_apu_ch1_volume\": {},\n  \"gbc_apu_ch1_env_enabled\": {},\n  \"gbc_apu_ch1_env_period\": {},\n  \"gbc_apu_ch1_env_timer\": {},\n  \"gbc_apu_ch1_env_direction\": {},\n  \"gbc_apu_ch1_env_initial_volume\": {},\n  \"gbc_apu_ch1_sweep_enabled\": {},\n  \"gbc_apu_ch1_sweep_period\": {},\n  \"gbc_apu_ch1_sweep_timer\": {},\n  \"gbc_apu_ch1_sweep_shift\": {},\n  \"gbc_apu_ch1_sweep_direction\": {},\n  \"gbc_apu_ch1_shadow_frequency\": {},\n  \"gbc_apu_ch2_enabled\": {},\n  \"gbc_apu_ch2_duty\": {},\n  \"gbc_apu_ch2_duty_pointer\": {},\n  \"gbc_apu_ch2_length_enabled\": {},\n  \"gbc_apu_ch2_length_counter\": {},\n  \"gbc_apu_ch2_period\": {},\n  \"gbc_apu_ch2_period_timer\": {},\n  \"gbc_apu_ch2_volume\": {},\n  \"gbc_apu_ch2_env_enabled\": {},\n  \"gbc_apu_ch2_env_period\": {},\n  \"gbc_apu_ch2_env_timer\": {},\n  \"gbc_apu_ch2_env_direction\": {},\n  \"gbc_apu_ch2_env_initial_volume\": {},\n  \"gbc_apu_ch3_enabled\": {},\n  \"gbc_apu_ch3_dac_enabled\": {},\n  \"gbc_apu_ch3_length_enabled\": {},\n  \"gbc_apu_ch3_length_counter\": {},\n  \"gbc_apu_ch3_period\": {},\n  \"gbc_apu_ch3_period_timer\": {},\n  \"gbc_apu_ch3_volume_shift\": {},\n  \"gbc_apu_ch3_wave_ram\": \"{}\",\n  \"gbc_apu_ch3_sample_pointer\": {},\n  \"gbc_apu_ch3_sample_rate\": {}, \n  \"gbc_apu_ch4_enabled\": {},\n  \"gbc_apu_ch4_length_enabled\": {},\n  \"gbc_apu_ch4_length_counter\": {},\n  \"gbc_apu_ch4_volume\": {},\n  \"gbc_apu_ch4_env_enabled\": {},\n  \"gbc_apu_ch4_env_period\": {},\n  \"gbc_apu_ch4_env_timer\": {},\n  \"gbc_apu_ch4_env_direction\": {},\n  \"gbc_apu_ch4_env_initial_volume\": {},\n  \"gbc_apu_ch4_lfsr\": {},\n  \"gbc_apu_ch4_divisor\": {},\n  \"gbc_apu_ch4_shift_clock\": {},\n  \"gbc_apu_ch4_width_7bit\": {},\n  \"gbc_apu_ch4_period_timer\": {}",
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
                0, 
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
                ",\n  \"gba_rom_loaded\": true,\n  \"gba_cpu_r0\": {},\n  \"gba_cpu_r1\": {},\n  \"gba_cpu_r2\": {},\n  \"gba_cpu_r3\": {},\n  \"gba_cpu_r4\": {},\n  \"gba_cpu_r5\": {},\n  \"gba_cpu_r6\": {},\n  \"gba_cpu_r7\": {},\n  \"gba_cpu_r8\": {},\n  \"gba_cpu_r9\": {},\n  \"gba_cpu_r10\": {},\n  \"gba_cpu_r11\": {},\n  \"gba_cpu_r12\": {},\n  \"gba_cpu_r13\": {},\n  \"gba_cpu_r14\": {},\n  \"gba_cpu_r15\": {},\n  \"gba_cpu_cpsr\": {},\n  \"gba_cpu_spsr\": {},\n  \"gba_cpu_halted\": {},\n  \"gba_mmu_waitcnt\": {},\n  \"gba_mmu_ie\": {},\n  \"gba_mmu_if\": {},\n  \"gba_mmu_ime\": {},\n  \"gba_flash_bank\": {},\n  \"gba_flash_state\": {},\n  \"gba_mmu_ewram\": \"{}\",\n  \"gba_mmu_iwram\": \"{}\",\n  \"gba_mmu_palette_ram\": \"{}\",\n  \"gba_mmu_vram\": \"{}\",\n  \"gba_mmu_oam\": \"{}\",\n  \"gba_mmu_io\": \"{}\",\n  \"gba_flash_data\": \"{}\",\n  \"gba_cpu_r8_usr\": {:?},\n  \"gba_cpu_r8_fiq\": {:?},\n  \"gba_cpu_r13_usr\": {},\n  \"gba_cpu_r14_usr\": {},\n  \"gba_cpu_r13_svc\": {},\n  \"gba_cpu_r14_svc\": {},\n  \"gba_cpu_spsr_svc\": {},\n  \"gba_cpu_r13_irq\": {},\n  \"gba_cpu_r14_irq\": {},\n  \"gba_cpu_spsr_irq\": {},\n  \"gba_cpu_r13_abt\": {},\n  \"gba_cpu_r14_abt\": {},\n  \"gba_cpu_spsr_abt\": {},\n  \"gba_cpu_r13_und\": {},\n  \"gba_cpu_r14_und\": {},\n  \"gba_cpu_spsr_und\": {},\n  \"gba_cpu_r13_fiq\": {},\n  \"gba_cpu_r14_fiq\": {},\n  \"gba_cpu_spsr_fiq\": {},\n  \"gba_cpu_pipeline\": {:?},\n  \"gba_apu_fifo_a_buffer\": \"{}\",\n  \"gba_apu_fifo_a_write_ptr\": {},\n  \"gba_apu_fifo_a_read_ptr\": {},\n  \"gba_apu_fifo_a_count\": {},\n  \"gba_apu_fifo_b_buffer\": \"{}\",\n  \"gba_apu_fifo_b_write_ptr\": {},\n  \"gba_apu_fifo_b_read_ptr\": {},\n  \"gba_apu_fifo_b_count\": {},\n  \"gba_apu_dma_request_a\": {},\n  \"gba_apu_dma_request_b\": {},\n  \"gba_apu_current_sample_a\": {},\n  \"gba_apu_current_sample_b\": {},\n  \"gba_dma_ch0_sad\": {},\n  \"gba_dma_ch0_dad\": {},\n  \"gba_dma_ch0_count\": {},\n  \"gba_dma_ch0_control\": {},\n  \"gba_dma_ch0_cur_src\": {},\n  \"gba_dma_ch0_cur_dest\": {},\n  \"gba_dma_ch0_cur_count\": {},\n  \"gba_dma_ch0_active\": {},\n  \"gba_dma_ch1_sad\": {},\n  \"gba_dma_ch1_dad\": {},\n  \"gba_dma_ch1_count\": {},\n  \"gba_dma_ch1_control\": {},\n  \"gba_dma_ch1_cur_src\": {},\n  \"gba_dma_ch1_cur_dest\": {},\n  \"gba_dma_ch1_cur_count\": {},\n  \"gba_dma_ch1_active\": {},\n  \"gba_dma_ch2_sad\": {},\n  \"gba_dma_ch2_dad\": {},\n  \"gba_dma_ch2_count\": {},\n  \"gba_dma_ch2_control\": {},\n  \"gba_dma_ch2_cur_src\": {},\n  \"gba_dma_ch2_cur_dest\": {},\n  \"gba_dma_ch2_cur_count\": {},\n  \"gba_dma_ch2_active\": {},\n  \"gba_dma_ch3_sad\": {},\n  \"gba_dma_ch3_dad\": {},\n  \"gba_dma_ch3_count\": {},\n  \"gba_dma_ch3_control\": {},\n  \"gba_dma_ch3_cur_src\": {},\n  \"gba_dma_ch3_cur_dest\": {},\n  \"gba_dma_ch3_cur_count\": {},\n  \"gba_dma_ch3_active\": {},\n  \"gba_timer_ch0_counter\": {},\n  \"gba_timer_ch0_reload\": {},\n  \"gba_timer_ch0_control\": {},\n  \"gba_timer_ch0_cycle_accumulator\": {},\n  \"gba_timer_ch0_overflowed\": {},\n  \"gba_timer_ch1_counter\": {},\n  \"gba_timer_ch1_reload\": {},\n  \"gba_timer_ch1_control\": {},\n  \"gba_timer_ch1_cycle_accumulator\": {},\n  \"gba_timer_ch1_overflowed\": {},\n  \"gba_timer_ch2_counter\": {},\n  \"gba_timer_ch2_reload\": {},\n  \"gba_timer_ch2_control\": {},\n  \"gba_timer_ch2_cycle_accumulator\": {},\n  \"gba_timer_ch2_overflowed\": {},\n  \"gba_timer_ch3_counter\": {},\n  \"gba_timer_ch3_reload\": {},\n  \"gba_timer_ch3_control\": {},\n  \"gba_timer_ch3_cycle_accumulator\": {},\n  \"gba_timer_ch3_overflowed\": {}",
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
                self.gba_cpu.halt,
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

        if self.console_type == crate::ffi::ConsoleType::Nintendo64 && self.rom_loaded {
            state_json.push_str(",\n  \"n64_rom_loaded\": true");
            let hex_rdram = to_hex(&self.n64_mmu.rdram.data[..crate::n64::rdram::Rdram::SIZE]);
            state_json.push_str(&format!(",\n  \"n64_rdram\": \"{}\"", hex_rdram));
            state_json.push_str(&format!(
                ",\n  \"expansion_pak\": {},\n  \"stick_x\": {},\n  \"stick_y\": {}",
                self.expansion_pak,
                self.buttons.stick_x,
                self.buttons.stick_y
            ));

            // CPU registers
            let mut cpu_gpr_bytes = [0u8; 32 * 8];
            for i in 0..32 {
                cpu_gpr_bytes[i*8..(i+1)*8].copy_from_slice(&self.n64_cpu.regs.gpr[i].to_be_bytes());
            }
            state_json.push_str(&format!(",\n  \"n64_cpu_gpr\": \"{}\"", to_hex(&cpu_gpr_bytes)));
            state_json.push_str(&format!(
                ",\n  \"n64_cpu_pc\": {},\n  \"n64_cpu_hi\": {},\n  \"n64_cpu_lo\": {}",
                self.n64_cpu.regs.pc, self.n64_cpu.regs.hi, self.n64_cpu.regs.lo
            ));

            // FPU registers (CP1)
            let mut fgr_bytes = [0u8; 32 * 8];
            for i in 0..32 {
                fgr_bytes[i*8..(i+1)*8].copy_from_slice(&self.n64_cpu.cp1.fgr[i].to_be_bytes());
            }
            state_json.push_str(&format!(",\n  \"n64_cpu_fgr\": \"{}\"", to_hex(&fgr_bytes)));
            state_json.push_str(&format!(
                ",\n  \"n64_cpu_fcr31\": {},\n  \"n64_cpu_fcr0\": {}",
                self.n64_cpu.cp1.fcr31, self.n64_cpu.cp1.fcr0
            ));

            // CPU delay slot state
            state_json.push_str(&format!(
                ",\n  \"n64_cpu_in_delay_slot\": {},\n  \"n64_cpu_delay_slot_pc\": {},\n  \"n64_cpu_nullify_delay_slot\": {}",
                self.n64_cpu.in_delay_slot, self.n64_cpu.delay_slot_pc, self.n64_cpu.nullify_delay_slot
            ));

            // CP0 registers
            state_json.push_str(&format!(
                ",\n  \"n64_cp0_index\": {},\n  \"n64_cp0_random\": {},\n  \"n64_cp0_entry_lo0\": {},\n  \"n64_cp0_entry_lo1\": {},\n  \"n64_cp0_context\": {},\n  \"n64_cp0_page_mask\": {},\n  \"n64_cp0_wired\": {},\n  \"n64_cp0_bad_vaddr\": {},\n  \"n64_cp0_count\": {},\n  \"n64_cp0_entry_hi\": {},\n  \"n64_cp0_compare\": {},\n  \"n64_cp0_status\": {},\n  \"n64_cp0_cause\": {},\n  \"n64_cp0_epc\": {},\n  \"n64_cp0_prid\": {},\n  \"n64_cp0_config\": {},\n  \"n64_cp0_error_epc\": {}",
                self.n64_cpu.cp0.index, self.n64_cpu.cp0.random, self.n64_cpu.cp0.entry_lo0, self.n64_cpu.cp0.entry_lo1,
                self.n64_cpu.cp0.context, self.n64_cpu.cp0.page_mask, self.n64_cpu.cp0.wired, self.n64_cpu.cp0.bad_vaddr,
                self.n64_cpu.cp0.count, self.n64_cpu.cp0.entry_hi, self.n64_cpu.cp0.compare, self.n64_cpu.cp0.status,
                self.n64_cpu.cp0.cause, self.n64_cpu.cp0.epc, self.n64_cpu.cp0.prid, self.n64_cpu.cp0.config,
                self.n64_cpu.cp0.error_epc
            ));

            // CP0 TLB
            let mut tlb_bytes = [0u8; 32 * 28];
            for i in 0..32 {
                let entry = &self.n64_cpu.tlb[i];
                tlb_bytes[i*28..i*28+4].copy_from_slice(&entry.page_mask.to_be_bytes());
                tlb_bytes[i*28+4..i*28+12].copy_from_slice(&entry.entry_hi.to_be_bytes());
                tlb_bytes[i*28+12..i*28+20].copy_from_slice(&entry.entry_lo0.to_be_bytes());
                tlb_bytes[i*28+20..i*28+28].copy_from_slice(&entry.entry_lo1.to_be_bytes());
            }
            state_json.push_str(&format!(",\n  \"n64_cpu_tlb\": \"{}\"", to_hex(&tlb_bytes)));

            // RSP registers
            let mut rsp_gpr_bytes = [0u8; 32 * 4];
            for i in 0..32 {
                rsp_gpr_bytes[i*4..(i+1)*4].copy_from_slice(&self.n64_mmu.rsp.gpr[i].to_be_bytes());
            }
            state_json.push_str(&format!(",\n  \"n64_rsp_gpr\": \"{}\"", to_hex(&rsp_gpr_bytes)));
            state_json.push_str(&format!(",\n  \"n64_rsp_pc\": {}", self.n64_mmu.rsp.pc));

            let mut vpr_bytes = [0u8; 32 * 8 * 2];
            for i in 0..32 {
                for j in 0..8 {
                    let offset = (i * 8 + j) * 2;
                    vpr_bytes[offset..offset+2].copy_from_slice(&self.n64_mmu.rsp.vpr[i][j].to_be_bytes());
                }
            }
            state_json.push_str(&format!(",\n  \"n64_rsp_vpr\": \"{}\"", to_hex(&vpr_bytes)));

            let mut acc_bytes = [0u8; 8 * 8];
            for i in 0..8 {
                acc_bytes[i*8..(i+1)*8].copy_from_slice(&self.n64_mmu.rsp.acc[i].to_be_bytes());
            }
            state_json.push_str(&format!(",\n  \"n64_rsp_acc\": \"{}\"", to_hex(&acc_bytes)));

            state_json.push_str(&format!(
                ",\n  \"n64_rsp_vco\": {},\n  \"n64_rsp_vcc\": {},\n  \"n64_rsp_vce\": {},\n  \"n64_rsp_status\": {},\n  \"n64_rsp_semaphore\": {},\n  \"n64_rsp_halted\": {},\n  \"n64_rsp_broke\": {},\n  \"n64_rsp_single_step\": {},\n  \"n64_rsp_intr_on_break\": {}",
                self.n64_mmu.rsp.vco, self.n64_mmu.rsp.vcc, self.n64_mmu.rsp.vce, self.n64_mmu.rsp.status,
                self.n64_mmu.rsp.semaphore.get(), self.n64_mmu.rsp.halted, self.n64_mmu.rsp.broke,
                self.n64_mmu.rsp.single_step, self.n64_mmu.rsp.intr_on_break
            ));

            let mut signals_val = 0u8;
            for i in 0..8 {
                if self.n64_mmu.rsp.signals[i] {
                    signals_val |= 1 << i;
                }
            }
            state_json.push_str(&format!(
                ",\n  \"n64_rsp_signals\": {},\n  \"n64_rsp_dma_busy\": {},\n  \"n64_rsp_dma_full\": {},\n  \"n64_rsp_in_delay_slot\": {},\n  \"n64_rsp_delay_slot_pc\": {}",
                signals_val, self.n64_mmu.rsp.dma_busy, self.n64_mmu.rsp.dma_full,
                self.n64_mmu.rsp.in_delay_slot, self.n64_mmu.rsp.delay_slot_pc
            ));

            state_json.push_str(&format!(
                ",\n  \"n64_rsp_sp_mem_addr\": {},\n  \"n64_rsp_sp_dram_addr\": {},\n  \"n64_rsp_sp_rd_len\": {},\n  \"n64_rsp_sp_wr_len\": {}",
                self.n64_mmu.rsp.sp_mem_addr, self.n64_mmu.rsp.sp_dram_addr, self.n64_mmu.rsp.sp_rd_len, self.n64_mmu.rsp.sp_wr_len
            ));

            // RDP registers
            state_json.push_str(&format!(",\n  \"n64_rdp_tmem\": \"{}\"", to_hex(&self.n64_mmu.rdp.tmem)));
            state_json.push_str(&format!(
                ",\n  \"n64_rdp_color_image_addr\": {},\n  \"n64_rdp_color_image_format\": {},\n  \"n64_rdp_color_image_size\": {},\n  \"n64_rdp_color_image_width\": {},\n  \"n64_rdp_depth_image_addr\": {},\n  \"n64_rdp_texture_image_addr\": {},\n  \"n64_rdp_texture_image_format\": {},\n  \"n64_rdp_texture_image_size\": {},\n  \"n64_rdp_texture_image_width\": {}",
                self.n64_mmu.rdp.color_image_addr, self.n64_mmu.rdp.color_image_format, self.n64_mmu.rdp.color_image_size,
                self.n64_mmu.rdp.color_image_width, self.n64_mmu.rdp.depth_image_addr, self.n64_mmu.rdp.texture_image_addr,
                self.n64_mmu.rdp.texture_image_format, self.n64_mmu.rdp.texture_image_size, self.n64_mmu.rdp.texture_image_width
            ));

            let mut tiles_bytes = [0u8; 8 * 12];
            for i in 0..8 {
                let tile = &self.n64_mmu.rdp.tiles[i];
                tiles_bytes[i*12] = tile.format;
                tiles_bytes[i*12+1] = tile.size;
                tiles_bytes[i*12+2..i*12+4].copy_from_slice(&tile.tmem_addr.to_be_bytes());
                tiles_bytes[i*12+4..i*12+6].copy_from_slice(&tile.line_width.to_be_bytes());
                tiles_bytes[i*12+6] = if tile.clamp_s { 1 } else { 0 };
                tiles_bytes[i*12+7] = if tile.clamp_t { 1 } else { 0 };
                tiles_bytes[i*12+8] = tile.mask_s;
                tiles_bytes[i*12+9] = tile.mask_t;
                tiles_bytes[i*12+10] = tile.shift_s;
                tiles_bytes[i*12+11] = tile.shift_t;
            }
            state_json.push_str(&format!(",\n  \"n64_rdp_tiles\": \"{}\"", to_hex(&tiles_bytes)));

            state_json.push_str(&format!(
                ",\n  \"n64_rdp_scissor_xh\": {},\n  \"n64_rdp_scissor_yh\": {},\n  \"n64_rdp_scissor_xl\": {},\n  \"n64_rdp_scissor_yl\": {},\n  \"n64_rdp_fill_color\": {},\n  \"n64_rdp_blend_color\": {},\n  \"n64_rdp_fog_color\": {},\n  \"n64_rdp_prim_color\": {},\n  \"n64_rdp_env_color\": {},\n  \"n64_rdp_cycle_type\": {}",
                self.n64_mmu.rdp.scissor_xh, self.n64_mmu.rdp.scissor_yh, self.n64_mmu.rdp.scissor_xl, self.n64_mmu.rdp.scissor_yl,
                self.n64_mmu.rdp.fill_color, self.n64_mmu.rdp.blend_color, self.n64_mmu.rdp.fog_color,
                self.n64_mmu.rdp.prim_color, self.n64_mmu.rdp.env_color, self.n64_mmu.rdp.cycle_type
            ));

            // DMEM and IMEM
            state_json.push_str(&format!(",\n  \"n64_dmem\": \"{}\"", to_hex(&self.n64_mmu.sp_dmem)));
            state_json.push_str(&format!(",\n  \"n64_imem\": \"{}\"", to_hex(&self.n64_mmu.sp_imem)));

            // MMIO registers
            state_json.push_str(&format!(",\n  \"n64_rdram_regs\": \"{}\"", to_hex(&self.n64_mmu.rdram_regs)));
            state_json.push_str(&format!(",\n  \"n64_sp_regs\": \"{}\"", to_hex(&self.n64_mmu.sp_regs)));
            state_json.push_str(&format!(",\n  \"n64_sp_pc_regs\": \"{}\"", to_hex(&self.n64_mmu.sp_pc_regs)));
            state_json.push_str(&format!(",\n  \"n64_dpc_regs\": \"{}\"", to_hex(&self.n64_mmu.dpc_regs)));
            state_json.push_str(&format!(",\n  \"n64_dps_regs\": \"{}\"", to_hex(&self.n64_mmu.dps_regs)));
            state_json.push_str(&format!(",\n  \"n64_mi_regs\": \"{}\"", to_hex(&self.n64_mmu.mi_regs)));
            state_json.push_str(&format!(",\n  \"n64_vi_regs\": \"{}\"", to_hex(&self.n64_mmu.vi_regs)));
            state_json.push_str(&format!(",\n  \"n64_ai_regs\": \"{}\"", to_hex(&self.n64_mmu.ai_regs)));
            state_json.push_str(&format!(",\n  \"n64_pi_regs\": \"{}\"", to_hex(&self.n64_mmu.pi_regs)));
            state_json.push_str(&format!(",\n  \"n64_si_regs\": \"{}\"", to_hex(&self.n64_mmu.si_regs)));
            state_json.push_str(&format!(",\n  \"n64_ri_regs\": \"{}\"", to_hex(&self.n64_mmu.ri_regs)));
            state_json.push_str(&format!(",\n  \"n64_pif_ram\": \"{}\"", to_hex(&self.n64_mmu.pif_ram)));

            // Other MMU timing/DMA queue states
            state_json.push_str(&format!(
                ",\n  \"n64_mmu_vi_cycles\": {},\n  \"n64_mmu_vi_intr_triggered\": {}",
                self.n64_mmu.vi_cycles, self.n64_mmu.vi_intr_triggered
            ));

            let mut ai_dma_addr_bytes = [0u8; 8];
            ai_dma_addr_bytes[0..4].copy_from_slice(&self.n64_mmu.ai_dma_address[0].to_be_bytes());
            ai_dma_addr_bytes[4..8].copy_from_slice(&self.n64_mmu.ai_dma_address[1].to_be_bytes());
            state_json.push_str(&format!(",\n  \"n64_mmu_ai_dma_address\": \"{}\"", to_hex(&ai_dma_addr_bytes)));

            let mut ai_dma_len_bytes = [0u8; 8];
            ai_dma_len_bytes[0..4].copy_from_slice(&self.n64_mmu.ai_dma_length[0].to_be_bytes());
            ai_dma_len_bytes[4..8].copy_from_slice(&self.n64_mmu.ai_dma_length[1].to_be_bytes());
            state_json.push_str(&format!(",\n  \"n64_mmu_ai_dma_length\": \"{}\"", to_hex(&ai_dma_len_bytes)));

            state_json.push_str(&format!(
                ",\n  \"n64_mmu_ai_dma_count\": {},\n  \"n64_mmu_ai_dma_len_remaining\": {},\n  \"n64_mmu_ai_dma_byte_accumulator\": \"{}\",\n  \"n64_mmu_ai_programmed_addr\": {},\n  \"n64_mmu_ai_control\": {},\n  \"n64_mmu_ai_dacrate\": {},\n  \"n64_mmu_ai_bitrate\": {}",
                self.n64_mmu.ai_dma_count, self.n64_mmu.ai_dma_len_remaining, self.n64_mmu.ai_dma_byte_accumulator,
                self.n64_mmu.ai_programmed_addr, self.n64_mmu.ai_control, self.n64_mmu.ai_dacrate, self.n64_mmu.ai_bitrate
            ));
        }

        for (key, val) in &self.extra_fields {
            state_json.push_str(&format!(",\n  \"{}\": {}", key, val));
        }
        state_json.push_str("\n}");

        if let Err(e) = atomic_save(&safe_tmp, &safe_sav, &state_json) {
            let _ = std::fs::remove_file(&safe_tmp);
            return format!("SAVE_STATE_ERROR {}", e);
        }

        if rom_name.is_some() {
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
        let rom_name = if !self.rom_path.as_os_str().is_empty() {
            self.rom_path.file_stem().and_then(|s| s.to_str())
        } else {
            None
        };
        let filename = if let Some(name) = rom_name {
            format!("{}_savestate_{}.sav", name, slot)
        } else {
            format!("savestate_{}.sav", slot)
        };
        let mut sav_path = base.join(&filename);
        if rom_name.is_some() && !sav_path.exists() {
            sav_path = base.join(format!("savestate_{}.sav", slot));
        }
        let safe_sav = match crate::rom::validate_path_safety(&sav_path, base) {
            Ok(p) => p,
            Err(_) => return "LOAD_STATE_ERROR Path traversal detected".to_string(),
        };

        if !safe_sav.exists() {
            return "LOAD_STATE_ERROR File not found".to_string();
        }

        let metadata = match std::fs::metadata(&safe_sav) {
            Ok(m) => m,
            Err(e) => return format!("LOAD_STATE_ERROR {}", e),
        };
        if metadata.len() > 32 * 1024 * 1024 {
            return "LOAD_STATE_ERROR State file too large".to_string();
        }

        let content = match std::fs::read_to_string(&safe_sav) {
            Ok(c) => c,
            Err(e) => return format!("LOAD_STATE_ERROR {}", e),
        };

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
        let z = get_json_bool(&content, "z").unwrap_or(false);
        let c_up = get_json_bool(&content, "c_up").unwrap_or(false);
        let c_down = get_json_bool(&content, "c_down").unwrap_or(false);
        let c_left = get_json_bool(&content, "c_left").unwrap_or(false);
        let c_right = get_json_bool(&content, "c_right").unwrap_or(false);

        let console_type = match console_type_str.as_str() {
            "GBC" => crate::ffi::ConsoleType::Gbc,
            "GBA" => crate::ffi::ConsoleType::Gba,
            "N64" | "Nintendo64" => crate::ffi::ConsoleType::Nintendo64,
            _ => return "LOAD_STATE_ERROR Invalid console type".to_string(),
        };

        let is_playing = match playback_state_str.as_str() {
            "play" => true,
            "pause" => false,
            _ => return "LOAD_STATE_ERROR Invalid playback state".to_string(),
        };

        let ticks = match ticks_str.parse::<u32>() {
            Ok(t) => t,
            Err(_) => return "LOAD_STATE_ERROR Invalid ticks".to_string(),
        };

        let player_x = match player_x_str.parse::<u32>() {
            Ok(x) => x,
            Err(_) => return "LOAD_STATE_ERROR Invalid player_x".to_string(),
        };

        let player_y = match player_y_str.parse::<u32>() {
            Ok(y) => y,
            Err(_) => return "LOAD_STATE_ERROR Invalid player_y".to_string(),
        };

        let speed = match speed_str.parse::<f32>() {
            Ok(s) => s,
            Err(_) => return "LOAD_STATE_ERROR Invalid speed".to_string(),
        };

        let frame_skip = match frame_skip_str.parse::<u32>() {
            Ok(fs) => fs,
            Err(_) => return "LOAD_STATE_ERROR Invalid frame_skip".to_string(),
        };

        let cpu_cycles = match cpu_cycles_str.parse::<u64>() {
            Ok(cc) => cc,
            Err(_) => return "LOAD_STATE_ERROR Invalid cpu_cycles".to_string(),
        };

        let rendered_frames = match rendered_frames_str.parse::<u32>() {
            Ok(rf) => rf,
            Err(_) => return "LOAD_STATE_ERROR Invalid rendered_frames".to_string(),
        };

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
            stick_x: 0,
            stick_y: 0,
            z,
            c_up,
            c_down,
            c_left,
            c_right,
        };

        if self.console_type == crate::ffi::ConsoleType::Gba {
            self.width = 240;
            self.height = 160;
            if let Some(true) = get_json_bool(&content, "gba_rom_loaded") {
                self.rom_loaded = true;
                if let Some(n) = get_json_number(&content, "gba_cpu_r0") { self.gba_cpu.registers.gpr[0] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r1") { self.gba_cpu.registers.gpr[1] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r2") { self.gba_cpu.registers.gpr[2] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r3") { self.gba_cpu.registers.gpr[3] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r4") { self.gba_cpu.registers.gpr[4] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r5") { self.gba_cpu.registers.gpr[5] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r6") { self.gba_cpu.registers.gpr[6] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r7") { self.gba_cpu.registers.gpr[7] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r8") { self.gba_cpu.registers.gpr[8] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r9") { self.gba_cpu.registers.gpr[9] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r10") { self.gba_cpu.registers.gpr[10] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r11") { self.gba_cpu.registers.gpr[11] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r12") { self.gba_cpu.registers.gpr[12] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r13") { self.gba_cpu.registers.gpr[13] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r14") { self.gba_cpu.registers.gpr[14] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r15") { self.gba_cpu.registers.gpr[15] = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_cpsr") { self.gba_cpu.registers.cpsr = n.parse().unwrap_or(0x1F); }
                if let Some(n) = get_json_number(&content, "gba_cpu_spsr") { self.gba_cpu.registers.spsr = n.parse().unwrap_or(0); }
                if let Some(b) = get_json_bool(&content, "gba_cpu_halted") { self.gba_cpu.halt = b; }

                if let Some(n) = get_json_number(&content, "gba_mmu_waitcnt") { self.gba_mmu.waitcnt = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_mmu_ie") { self.gba_mmu.ie = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_mmu_if") { self.gba_mmu.r_if = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_mmu_ime") { self.gba_mmu.ime = n.parse().unwrap_or(0); }

                if let Some(n) = get_json_number(&content, "gba_flash_bank") { self.gba_mmu.flash.bank = n.parse().unwrap_or(0); }
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

                if let Some(hex) = get_json_string(&content, "gba_mmu_ewram") { self.gba_mmu.ewram = from_hex(&hex); }
                if let Some(hex) = get_json_string(&content, "gba_mmu_iwram") { self.gba_mmu.iwram = from_hex(&hex); }
                if let Some(hex) = get_json_string(&content, "gba_mmu_palette_ram") {
                    let bytes = from_hex(&hex);
                    if bytes.len() == 1024 { self.gba_mmu.palette_ram.copy_from_slice(&bytes); }
                }
                if let Some(hex) = get_json_string(&content, "gba_mmu_vram") { self.gba_mmu.vram = from_hex(&hex); }
                if let Some(hex) = get_json_string(&content, "gba_mmu_oam") {
                    let bytes = from_hex(&hex);
                    if bytes.len() == 1024 { self.gba_mmu.oam.copy_from_slice(&bytes); }
                }
                if let Some(hex) = get_json_string(&content, "gba_mmu_io") {
                    let bytes = from_hex(&hex);
                    if bytes.len() == 1024 { self.gba_mmu.io.copy_from_slice(&bytes); }
                }
                if let Some(hex) = get_json_string(&content, "gba_flash_data") { self.gba_mmu.flash.data = from_hex(&hex); }

                if let Some(arr) = get_json_array_of_numbers(&content, "gba_cpu_r8_usr") {
                    if arr.len() == 5 { self.gba_cpu.registers.r8_usr.copy_from_slice(&arr); }
                }
                if let Some(arr) = get_json_array_of_numbers(&content, "gba_cpu_r8_fiq") {
                    if arr.len() == 5 { self.gba_cpu.registers.r8_fiq.copy_from_slice(&arr); }
                }
                if let Some(n) = get_json_number(&content, "gba_cpu_r13_usr") { self.gba_cpu.registers.r13_usr = n.parse().unwrap_or(0x03007F00); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r14_usr") { self.gba_cpu.registers.r14_usr = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r13_svc") { self.gba_cpu.registers.r13_svc = n.parse().unwrap_or(0x03007FE0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r14_svc") { self.gba_cpu.registers.r14_svc = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_spsr_svc") { self.gba_cpu.registers.spsr_svc = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r13_irq") { self.gba_cpu.registers.r13_irq = n.parse().unwrap_or(0x03007FA0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r14_irq") { self.gba_cpu.registers.r14_irq = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_spsr_irq") { self.gba_cpu.registers.spsr_irq = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r13_abt") { self.gba_cpu.registers.r13_abt = n.parse().unwrap_or(0x03007FA0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r14_abt") { self.gba_cpu.registers.r14_abt = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_spsr_abt") { self.gba_cpu.registers.spsr_abt = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r13_und") { self.gba_cpu.registers.r13_und = n.parse().unwrap_or(0x03007FA0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r14_und") { self.gba_cpu.registers.r14_und = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_spsr_und") { self.gba_cpu.registers.spsr_und = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r13_fiq") { self.gba_cpu.registers.r13_fiq = n.parse().unwrap_or(0x03007FA0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_r14_fiq") { self.gba_cpu.registers.r14_fiq = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_cpu_spsr_fiq") { self.gba_cpu.registers.spsr_fiq = n.parse().unwrap_or(0); }

                if let Some(arr) = get_json_array_of_numbers(&content, "gba_cpu_pipeline") {
                    if arr.len() == 2 { self.gba_cpu.pipeline.copy_from_slice(&arr); }
                }

                if let Some(hex) = get_json_string(&content, "gba_apu_fifo_a_buffer") {
                    let bytes = from_hex(&hex);
                    for (idx, &b) in bytes.iter().enumerate() {
                        if idx < 32 { self.gba_mmu.apu.fifo_a.buffer[idx] = b as i8; }
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

                if let Some(hex) = get_json_string(&content, "gba_apu_fifo_b_buffer") {
                    let bytes = from_hex(&hex);
                    for (idx, &b) in bytes.iter().enumerate() {
                        if idx < 32 { self.gba_mmu.apu.fifo_b.buffer[idx] = b as i8; }
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

                if let Some(b) = get_json_bool(&content, "gba_apu_dma_request_a") { self.gba_mmu.apu.dma_request_a = b; }
                if let Some(b) = get_json_bool(&content, "gba_apu_dma_request_b") { self.gba_mmu.apu.dma_request_b = b; }
                if let Some(n) = get_json_number(&content, "gba_apu_current_sample_a") { self.gba_mmu.apu.current_sample_a = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "gba_apu_current_sample_b") { self.gba_mmu.apu.current_sample_b = n.parse().unwrap_or(0); }
                self.gba_mmu.apu.prev_sample_a = self.gba_mmu.apu.current_sample_a;
                self.gba_mmu.apu.prev_sample_b = self.gba_mmu.apu.current_sample_b;

                for ch in 0..4 {
                    let prefix = format!("gba_dma_ch{}", ch);
                    if let Some(n) = get_json_number(&content, &format!("{}_sad", prefix)) { self.gba_mmu.dma.channels[ch].sad = n.parse().unwrap_or(0); }
                    if let Some(n) = get_json_number(&content, &format!("{}_dad", prefix)) { self.gba_mmu.dma.channels[ch].dad = n.parse().unwrap_or(0); }
                    if let Some(n) = get_json_number(&content, &format!("{}_count", prefix)) { self.gba_mmu.dma.channels[ch].count = n.parse().unwrap_or(0); }
                    if let Some(n) = get_json_number(&content, &format!("{}_control", prefix)) { self.gba_mmu.dma.channels[ch].control = n.parse().unwrap_or(0); }
                    if let Some(n) = get_json_number(&content, &format!("{}_cur_src", prefix)) { self.gba_mmu.dma.channels[ch].cur_src = n.parse().unwrap_or(0); }
                    if let Some(n) = get_json_number(&content, &format!("{}_cur_dest", prefix)) { self.gba_mmu.dma.channels[ch].cur_dest = n.parse().unwrap_or(0); }
                    if let Some(n) = get_json_number(&content, &format!("{}_cur_count", prefix)) { self.gba_mmu.dma.channels[ch].cur_count = n.parse().unwrap_or(0); }
                    if let Some(b) = get_json_bool(&content, &format!("{}_active", prefix)) { self.gba_mmu.dma.channels[ch].active = b; }
                }

                for tmr in 0..4 {
                    let prefix = format!("gba_timer_ch{}", tmr);
                    if let Some(n) = get_json_number(&content, &format!("{}_counter", prefix)) { self.gba_mmu.timers[tmr].counter = n.parse().unwrap_or(0); }
                    if let Some(n) = get_json_number(&content, &format!("{}_reload", prefix)) { self.gba_mmu.timers[tmr].reload = n.parse().unwrap_or(0); }
                    if let Some(n) = get_json_number(&content, &format!("{}_control", prefix)) { self.gba_mmu.timers[tmr].control = n.parse().unwrap_or(0); }
                    if let Some(n) = get_json_number(&content, &format!("{}_cycle_accumulator", prefix)) { self.gba_mmu.timers[tmr].cycle_accumulator = n.parse().unwrap_or(0); }
                    if let Some(b) = get_json_bool(&content, &format!("{}_overflowed", prefix)) { self.gba_mmu.timers[tmr].overflowed = b; }
                }
            } else {
                self.rom_loaded = false;
            }
        } else if self.console_type == crate::ffi::ConsoleType::Nintendo64 {
            self.width = 320;
            self.height = 240;
            if let Some(true) = get_json_bool(&content, "n64_rom_loaded") {
                self.rom_loaded = true;
                let expansion_pak = get_json_bool(&content, "expansion_pak").unwrap_or(false);
                self.expansion_pak = expansion_pak;
                if expansion_pak {
                    self.width = 640;
                    self.height = 480;
                }
                if let Some(n) = get_json_number(&content, "stick_x") {
                    self.buttons.stick_x = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "stick_y") {
                    self.buttons.stick_y = n.parse().unwrap_or(0);
                }

                // Re-allocate video buffers
                let (raw_video, video_offset) = allocate_aligned_local::<u16>((self.width * self.height) as usize, 16);
                let (front_video, front_offset) = allocate_aligned_local::<u16>((self.width * self.height) as usize, 16);
                self.raw_video_buffer = raw_video;
                self.video_offset = video_offset;
                self.front_video_buffer = front_video;
                self.front_offset = front_offset;

                if let Some(hex) = get_json_string(&content, "n64_rdram") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), crate::n64::rdram::Rdram::SIZE);
                    self.n64_mmu.rdram.data[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }

                // CPU general registers
                if let Some(hex) = get_json_string(&content, "n64_cpu_gpr") {
                    let bytes = from_hex(&hex);
                    for i in 0..32 {
                        if i * 8 + 8 <= bytes.len() {
                            self.n64_cpu.regs.gpr[i] = u64::from_be_bytes(bytes[i*8..i*8+8].try_into().unwrap());
                        }
                    }
                }
                if let Some(n) = get_json_number(&content, "n64_cpu_pc") {
                    self.n64_cpu.regs.pc = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "n64_cpu_hi") {
                    self.n64_cpu.regs.hi = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "n64_cpu_lo") {
                    self.n64_cpu.regs.lo = n.parse().unwrap_or(0);
                }

                // FPU registers (CP1)
                if let Some(hex) = get_json_string(&content, "n64_cpu_fgr") {
                    let bytes = from_hex(&hex);
                    for i in 0..32 {
                        if i * 8 + 8 <= bytes.len() {
                            self.n64_cpu.cp1.fgr[i] = u64::from_be_bytes(bytes[i*8..i*8+8].try_into().unwrap());
                        }
                    }
                }
                if let Some(n) = get_json_number(&content, "n64_cpu_fcr31") {
                    self.n64_cpu.cp1.fcr31 = n.parse().unwrap_or(0);
                }
                if let Some(n) = get_json_number(&content, "n64_cpu_fcr0") {
                    self.n64_cpu.cp1.fcr0 = n.parse().unwrap_or(0);
                }

                // CPU delay slot state
                if let Some(b) = get_json_bool(&content, "n64_cpu_in_delay_slot") {
                    self.n64_cpu.in_delay_slot = b;
                }
                if let Some(n) = get_json_number(&content, "n64_cpu_delay_slot_pc") {
                    self.n64_cpu.delay_slot_pc = n.parse().unwrap_or(0);
                }
                if let Some(b) = get_json_bool(&content, "n64_cpu_nullify_delay_slot") {
                    self.n64_cpu.nullify_delay_slot = b;
                }

                // CP0 registers
                if let Some(n) = get_json_number(&content, "n64_cp0_index") { self.n64_cpu.cp0.index = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_random") { self.n64_cpu.cp0.random = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_entry_lo0") { self.n64_cpu.cp0.entry_lo0 = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_entry_lo1") { self.n64_cpu.cp0.entry_lo1 = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_context") { self.n64_cpu.cp0.context = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_page_mask") { self.n64_cpu.cp0.page_mask = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_wired") { self.n64_cpu.cp0.wired = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_bad_vaddr") { self.n64_cpu.cp0.bad_vaddr = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_count") { self.n64_cpu.cp0.count = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_entry_hi") { self.n64_cpu.cp0.entry_hi = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_compare") { self.n64_cpu.cp0.compare = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_status") { self.n64_cpu.cp0.status = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_cause") { self.n64_cpu.cp0.cause = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_epc") { self.n64_cpu.cp0.epc = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_prid") { self.n64_cpu.cp0.prid = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_config") { self.n64_cpu.cp0.config = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_cp0_error_epc") { self.n64_cpu.cp0.error_epc = n.parse().unwrap_or(0); }

                // CP0 TLB
                if let Some(hex) = get_json_string(&content, "n64_cpu_tlb") {
                    let bytes = from_hex(&hex);
                    for i in 0..32 {
                        if i * 28 + 28 <= bytes.len() {
                            self.n64_cpu.tlb[i].page_mask = u32::from_be_bytes(bytes[i*28..i*28+4].try_into().unwrap());
                            self.n64_cpu.tlb[i].entry_hi = u64::from_be_bytes(bytes[i*28+4..i*28+12].try_into().unwrap());
                            self.n64_cpu.tlb[i].entry_lo0 = u64::from_be_bytes(bytes[i*28+12..i*28+20].try_into().unwrap());
                            self.n64_cpu.tlb[i].entry_lo1 = u64::from_be_bytes(bytes[i*28+20..i*28+28].try_into().unwrap());
                        }
                    }
                }

                // RSP registers
                if let Some(hex) = get_json_string(&content, "n64_rsp_gpr") {
                    let bytes = from_hex(&hex);
                    for i in 0..32 {
                        if i * 4 + 4 <= bytes.len() {
                            self.n64_mmu.rsp.gpr[i] = u32::from_be_bytes(bytes[i*4..i*4+4].try_into().unwrap());
                        }
                    }
                }
                if let Some(n) = get_json_number(&content, "n64_rsp_pc") {
                    self.n64_mmu.rsp.pc = n.parse().unwrap_or(0);
                }
                if let Some(hex) = get_json_string(&content, "n64_rsp_vpr") {
                    let bytes = from_hex(&hex);
                    for i in 0..32 {
                        for j in 0..8 {
                            let offset = (i * 8 + j) * 2;
                            if offset + 2 <= bytes.len() {
                                self.n64_mmu.rsp.vpr[i][j] = i16::from_be_bytes(bytes[offset..offset+2].try_into().unwrap());
                            }
                        }
                    }
                }
                if let Some(hex) = get_json_string(&content, "n64_rsp_acc") {
                    let bytes = from_hex(&hex);
                    for i in 0..8 {
                        if i * 8 + 8 <= bytes.len() {
                            self.n64_mmu.rsp.acc[i] = i64::from_be_bytes(bytes[i*8..i*8+8].try_into().unwrap());
                        }
                    }
                }
                if let Some(n) = get_json_number(&content, "n64_rsp_vco") { self.n64_mmu.rsp.vco = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rsp_vcc") { self.n64_mmu.rsp.vcc = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rsp_vce") { self.n64_mmu.rsp.vce = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rsp_status") { self.n64_mmu.rsp.status = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rsp_semaphore") { self.n64_mmu.rsp.semaphore.set(n.parse().unwrap_or(0)); }
                if let Some(b) = get_json_bool(&content, "n64_rsp_halted") { self.n64_mmu.rsp.halted = b; }
                if let Some(b) = get_json_bool(&content, "n64_rsp_broke") { self.n64_mmu.rsp.broke = b; }
                if let Some(b) = get_json_bool(&content, "n64_rsp_single_step") { self.n64_mmu.rsp.single_step = b; }
                if let Some(b) = get_json_bool(&content, "n64_rsp_intr_on_break") { self.n64_mmu.rsp.intr_on_break = b; }

                if let Some(n) = get_json_number(&content, "n64_rsp_signals") {
                    let signals_val = n.parse().unwrap_or(0u8);
                    for i in 0..8 {
                        self.n64_mmu.rsp.signals[i] = (signals_val & (1 << i)) != 0;
                    }
                }
                if let Some(b) = get_json_bool(&content, "n64_rsp_dma_busy") { self.n64_mmu.rsp.dma_busy = b; }
                if let Some(b) = get_json_bool(&content, "n64_rsp_dma_full") { self.n64_mmu.rsp.dma_full = b; }
                if let Some(b) = get_json_bool(&content, "n64_rsp_in_delay_slot") { self.n64_mmu.rsp.in_delay_slot = b; }
                if let Some(n) = get_json_number(&content, "n64_rsp_delay_slot_pc") { self.n64_mmu.rsp.delay_slot_pc = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rsp_sp_mem_addr") { self.n64_mmu.rsp.sp_mem_addr = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rsp_sp_dram_addr") { self.n64_mmu.rsp.sp_dram_addr = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rsp_sp_rd_len") { self.n64_mmu.rsp.sp_rd_len = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rsp_sp_wr_len") { self.n64_mmu.rsp.sp_wr_len = n.parse().unwrap_or(0); }

                // RDP registers
                if let Some(hex) = get_json_string(&content, "n64_rdp_tmem") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.rdp.tmem.len());
                    self.n64_mmu.rdp.tmem[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }
                if let Some(n) = get_json_number(&content, "n64_rdp_color_image_addr") { self.n64_mmu.rdp.color_image_addr = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rdp_color_image_format") { self.n64_mmu.rdp.color_image_format = n.parse().unwrap_or(0) as u8; }
                if let Some(n) = get_json_number(&content, "n64_rdp_color_image_size") { self.n64_mmu.rdp.color_image_size = n.parse().unwrap_or(0) as u8; }
                if let Some(n) = get_json_number(&content, "n64_rdp_color_image_width") { self.n64_mmu.rdp.color_image_width = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rdp_depth_image_addr") { self.n64_mmu.rdp.depth_image_addr = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rdp_texture_image_addr") { self.n64_mmu.rdp.texture_image_addr = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rdp_texture_image_format") { self.n64_mmu.rdp.texture_image_format = n.parse().unwrap_or(0) as u8; }
                if let Some(n) = get_json_number(&content, "n64_rdp_texture_image_size") { self.n64_mmu.rdp.texture_image_size = n.parse().unwrap_or(0) as u8; }
                if let Some(n) = get_json_number(&content, "n64_rdp_texture_image_width") { self.n64_mmu.rdp.texture_image_width = n.parse().unwrap_or(0); }

                if let Some(hex) = get_json_string(&content, "n64_rdp_tiles") {
                    let bytes = from_hex(&hex);
                    for i in 0..8 {
                        if i * 12 + 12 <= bytes.len() {
                            let tile = &mut self.n64_mmu.rdp.tiles[i];
                            tile.format = bytes[i*12];
                            tile.size = bytes[i*12+1];
                            tile.tmem_addr = u16::from_be_bytes(bytes[i*12+2..i*12+4].try_into().unwrap());
                            tile.line_width = u16::from_be_bytes(bytes[i*12+4..i*12+6].try_into().unwrap());
                            tile.clamp_s = bytes[i*12+6] != 0;
                            tile.clamp_t = bytes[i*12+7] != 0;
                            tile.mask_s = bytes[i*12+8];
                            tile.mask_t = bytes[i*12+9];
                            tile.shift_s = bytes[i*12+10];
                            tile.shift_t = bytes[i*12+11];
                        }
                    }
                }

                if let Some(n) = get_json_number(&content, "n64_rdp_scissor_xh") { self.n64_mmu.rdp.scissor_xh = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rdp_scissor_yh") { self.n64_mmu.rdp.scissor_yh = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rdp_scissor_xl") { self.n64_mmu.rdp.scissor_xl = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rdp_scissor_yl") { self.n64_mmu.rdp.scissor_yl = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rdp_fill_color") { self.n64_mmu.rdp.fill_color = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rdp_blend_color") { self.n64_mmu.rdp.blend_color = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rdp_fog_color") { self.n64_mmu.rdp.fog_color = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rdp_prim_color") { self.n64_mmu.rdp.prim_color = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rdp_env_color") { self.n64_mmu.rdp.env_color = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_rdp_cycle_type") { self.n64_mmu.rdp.cycle_type = n.parse().unwrap_or(0) as u8; }

                // DMEM and IMEM
                if let Some(hex) = get_json_string(&content, "n64_dmem") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.sp_dmem.len());
                    self.n64_mmu.sp_dmem[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }
                if let Some(hex) = get_json_string(&content, "n64_imem") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.sp_imem.len());
                    self.n64_mmu.sp_imem[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }

                // MMIO registers
                if let Some(hex) = get_json_string(&content, "n64_rdram_regs") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.rdram_regs.len());
                    self.n64_mmu.rdram_regs[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }
                if let Some(hex) = get_json_string(&content, "n64_sp_regs") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.sp_regs.len());
                    self.n64_mmu.sp_regs[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }
                if let Some(hex) = get_json_string(&content, "n64_sp_pc_regs") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.sp_pc_regs.len());
                    self.n64_mmu.sp_pc_regs[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }
                if let Some(hex) = get_json_string(&content, "n64_dpc_regs") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.dpc_regs.len());
                    self.n64_mmu.dpc_regs[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }
                if let Some(hex) = get_json_string(&content, "n64_dps_regs") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.dps_regs.len());
                    self.n64_mmu.dps_regs[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }
                if let Some(hex) = get_json_string(&content, "n64_mi_regs") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.mi_regs.len());
                    self.n64_mmu.mi_regs[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }
                if let Some(hex) = get_json_string(&content, "n64_vi_regs") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.vi_regs.len());
                    self.n64_mmu.vi_regs[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }
                if let Some(hex) = get_json_string(&content, "n64_ai_regs") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.ai_regs.len());
                    self.n64_mmu.ai_regs[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }
                if let Some(hex) = get_json_string(&content, "n64_pi_regs") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.pi_regs.len());
                    self.n64_mmu.pi_regs[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }
                if let Some(hex) = get_json_string(&content, "n64_si_regs") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.si_regs.len());
                    self.n64_mmu.si_regs[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }
                if let Some(hex) = get_json_string(&content, "n64_ri_regs") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.ri_regs.len());
                    self.n64_mmu.ri_regs[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }
                if let Some(hex) = get_json_string(&content, "n64_pif_ram") {
                    let bytes = from_hex(&hex);
                    let copy_len = std::cmp::min(bytes.len(), self.n64_mmu.pif_ram.len());
                    self.n64_mmu.pif_ram[..copy_len].copy_from_slice(&bytes[..copy_len]);
                }

                // Other MMU timing/DMA queue states
                if let Some(n) = get_json_number(&content, "n64_mmu_vi_cycles") { self.n64_mmu.vi_cycles = n.parse().unwrap_or(0); }
                if let Some(b) = get_json_bool(&content, "n64_mmu_vi_intr_triggered") { self.n64_mmu.vi_intr_triggered = b; }

                if let Some(hex) = get_json_string(&content, "n64_mmu_ai_dma_address") {
                    let bytes = from_hex(&hex);
                    if bytes.len() == 8 {
                        self.n64_mmu.ai_dma_address[0] = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
                        self.n64_mmu.ai_dma_address[1] = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
                    }
                }
                if let Some(hex) = get_json_string(&content, "n64_mmu_ai_dma_length") {
                    let bytes = from_hex(&hex);
                    if bytes.len() == 8 {
                        self.n64_mmu.ai_dma_length[0] = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
                        self.n64_mmu.ai_dma_length[1] = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
                    }
                }
                if let Some(n) = get_json_number(&content, "n64_mmu_ai_dma_count") { self.n64_mmu.ai_dma_count = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_mmu_ai_dma_len_remaining") { self.n64_mmu.ai_dma_len_remaining = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_mmu_ai_dma_byte_accumulator") { self.n64_mmu.ai_dma_byte_accumulator = n.parse().unwrap_or(0.0); }
                if let Some(n) = get_json_number(&content, "n64_mmu_ai_programmed_addr") { self.n64_mmu.ai_programmed_addr = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_mmu_ai_control") { self.n64_mmu.ai_control = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_mmu_ai_dacrate") { self.n64_mmu.ai_dacrate = n.parse().unwrap_or(0); }
                if let Some(n) = get_json_number(&content, "n64_mmu_ai_bitrate") { self.n64_mmu.ai_bitrate = n.parse().unwrap_or(0); }
            } else {
                self.rom_loaded = false;
            }
        } else {
            self.rom_loaded = false;
        }

        let expected_size = (self.width * self.height) as usize;
        if self.raw_video_buffer.len() != expected_size + self.video_offset {
            let (raw_video, video_offset) = allocate_aligned_local::<u16>(expected_size, 16);
            let (front_video, front_offset) = allocate_aligned_local::<u16>(expected_size, 16);
            self.raw_video_buffer = raw_video;
            self.video_offset = video_offset;
            self.front_video_buffer = front_video;
            self.front_offset = front_offset;
        }

        self.player_x = std::cmp::min(self.player_x, self.width.saturating_sub(1));
        self.player_y = std::cmp::min(self.player_y, self.height.saturating_sub(1));

        "LOAD_STATE_OK".to_string()
    }
}
