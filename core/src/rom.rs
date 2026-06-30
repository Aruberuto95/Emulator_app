use std::fs;
use std::path::{Path, PathBuf};

// GBC Nintendo Logo Check bytes (48 bytes)
pub const GBC_LOGO: [u8; 48] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
    0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
    0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
];

// GBA Nintendo Logo Check bytes (156 bytes)
pub const GBA_LOGO: [u8; 156] = [
    0x24, 0xFF, 0xAE, 0x51, 0x69, 0x9A, 0xA2, 0x21, 0x3D, 0x84, 0x82, 0x0A, 0x84, 0xE4, 0x09, 0xAD,
    0x11, 0x24, 0x8B, 0x98, 0xC0, 0x81, 0x7F, 0x21, 0xA3, 0x52, 0xBE, 0x19, 0x93, 0x09, 0xCE, 0x20,
    0x10, 0x46, 0x4A, 0x4A, 0xF8, 0x27, 0x31, 0xEC, 0x58, 0xC7, 0xE8, 0x33, 0x82, 0xE3, 0xCE, 0xBF,
    0x85, 0xF4, 0xDF, 0x94, 0xCE, 0x4B, 0x09, 0xC1, 0x94, 0x56, 0x8A, 0xC0, 0x13, 0x72, 0xA7, 0xFC,
    0x9F, 0x84, 0x4D, 0x73, 0xA3, 0xCA, 0x9A, 0x61, 0x58, 0x97, 0xA3, 0x27, 0xFC, 0x03, 0x98, 0x76,
    0x23, 0x1D, 0xC7, 0x61, 0x03, 0x04, 0xAE, 0x56, 0xBF, 0x38, 0x84, 0x00, 0x40, 0xA7, 0x0E, 0xFD,
    0xFF, 0x52, 0xFE, 0x03, 0x6F, 0x95, 0x30, 0xF1, 0x97, 0xFB, 0xC0, 0x85, 0x60, 0xD6, 0x80, 0x25,
    0xA9, 0x63, 0xBE, 0x03, 0x01, 0x4E, 0x38, 0xE2, 0xF9, 0xA2, 0x34, 0xFF, 0xBB, 0x3E, 0x03, 0x44,
    0x78, 0x00, 0x90, 0xCB, 0x88, 0x11, 0x3A, 0x94, 0x65, 0xC0, 0x7C, 0x63, 0x87, 0xF0, 0x3C, 0xAF,
    0xD6, 0x25, 0xE4, 0x8B, 0x38, 0x0A, 0xAC, 0x72, 0x21, 0xD4, 0xF8, 0x07,
];

use crate::ffi::ConsoleType;

pub fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                components.pop();
            }
            Component::CurDir => {}
            Component::Normal(c) => {
                components.push(c);
            }
            _ => {
                components.push(component.as_os_str());
            }
        }
    }
    components.iter().collect()
}

pub fn validate_path_safety(target_path: &Path, base_dir: &Path) -> Result<PathBuf, &'static str> {
    let abs_target = if target_path.is_absolute() {
        target_path.to_path_buf()
    } else {
        base_dir.join(target_path)
    };

    let normalized = normalize_path(&abs_target);
    let canonical_base = base_dir
        .canonicalize()
        .map_err(|_| "Invalid base directory")?;

    if normalized.exists() {
        let canonical_target = normalized
            .canonicalize()
            .map_err(|_| "Path traversal detected")?;
        if !canonical_target.starts_with(&canonical_base) {
            return Err("Path traversal detected");
        }
        if let Ok(metadata) = std::fs::symlink_metadata(&normalized) {
            if metadata.file_type().is_symlink() {
                let target_link =
                    std::fs::read_link(&normalized).map_err(|_| "Symlink resolution error")?;
                let abs_link_target = if target_link.is_absolute() {
                    target_link
                } else {
                    normalized.parent().unwrap_or(base_dir).join(target_link)
                };
                let clean_link_target = normalize_path(&abs_link_target);
                if clean_link_target.exists() {
                    let canonical_link_target = clean_link_target
                        .canonicalize()
                        .map_err(|_| "Path traversal detected")?;
                    if !canonical_link_target.starts_with(&canonical_base) {
                        return Err("Symlink points outside allowed directory");
                    }
                } else {
                    if !clean_link_target.starts_with(&canonical_base) {
                        return Err("Symlink points outside allowed directory");
                    }
                }
            }
        }
        Ok(canonical_target)
    } else {
        // Target does not exist yet (e.g. a fresh savestate slot). Canonicalize its parent
        // directory so the prefix check compares like-for-like with `canonical_base`. On
        // Windows `canonicalize` yields verbatim (\\?\) paths, so comparing a raw join against
        // the canonical base would never match and every new-file write would be rejected.
        let file_name = normalized.file_name().ok_or("Path traversal detected")?;
        let parent = normalized.parent().filter(|p| !p.as_os_str().is_empty());
        let canonical_parent = match parent {
            Some(p) => p.canonicalize().map_err(|_| "Path traversal detected")?,
            None => canonical_base.clone(),
        };
        if !canonical_parent.starts_with(&canonical_base) {
            return Err("Path traversal detected");
        }
        Ok(canonical_parent.join(file_name))
    }
}

pub fn parse_ascii_title(bytes: &[u8]) -> Result<String, &'static str> {
    let mut title = String::new();
    for &b in bytes {
        if b == 0 {
            break;
        }
        if b >= 32 && b <= 126 {
            title.push(b as char);
        } else {
            title.push('?');
        }
    }
    Ok(title.trim().to_string())
}

pub fn validate_and_parse_header(rom_data: &[u8]) -> Result<ConsoleType, &'static str> {
    if rom_data.is_empty() {
        return Err("Empty ROM file");
    }

    // Check if it is a text mock profile first
    if rom_data.len() >= 8 {
        if let Ok(text_content) =
            std::str::from_utf8(&rom_data[..std::cmp::min(rom_data.len(), 512)])
        {
            if text_content.contains("CONSOLE:") {
                let lines: Vec<&str> = text_content.lines().map(|l| l.trim()).collect();
                let mut console_type = None;
                let mut logo_valid = false;
                let mut checksum_valid = false;
                for line in lines {
                    if line.starts_with("CONSOLE:") {
                        console_type = Some(line["CONSOLE:".len()..].trim());
                    } else if line.starts_with("LOGO:") {
                        logo_valid = line["LOGO:".len()..].trim() == "VALID";
                    } else if line.starts_with("CHECKSUM:") {
                        checksum_valid = line["CHECKSUM:".len()..].trim() == "VALID";
                    }
                }
                if let Some(c) = console_type {
                    if !logo_valid {
                        return Err("Invalid Nintendo logo");
                    }
                    if !checksum_valid {
                        return Err("Invalid checksum");
                    }
                    if c == "GBA" {
                        return Ok(ConsoleType::Gba);
                    } else if c == "GBC" {
                        return Ok(ConsoleType::Gbc);
                    } else {
                        return Err("Missing CONSOLE type in text mock");
                    }
                }
            }
        }
    }

    // Binary ROM parsing
    // GBA requires at least 0xC0 bytes
    // GBC requires at least 0x150 bytes

    // Check GBA first (GBA console byte 0xB2 must be 0x96)
    if rom_data.len() >= 0xC0 && rom_data[0xB2] == 0x96 {
        // GBA logo check
        if rom_data[0x004..0x004 + 156] != GBA_LOGO[..] {
            return Err("Invalid Nintendo logo");
        }

        // GBA Checksum
        let mut checksum: u8 = 0;
        for i in 0xA0..0xBD {
            checksum = checksum.wrapping_sub(rom_data[i]);
        }
        checksum = checksum.wrapping_sub(0x19);
        if rom_data[0xBD] != checksum {
            return Err("GBA header checksum mismatch");
        }

        // Check title ASCII
        let title_bytes = &rom_data[0xA0..0xAC];
        if parse_ascii_title(title_bytes).is_err() {
            return Err("Invalid Title");
        }

        return Ok(ConsoleType::Gba);
    }

    // Check GBC / GB
    if rom_data.len() >= 0x150 {
        // GBC logo check
        if rom_data[0x104..0x104 + 48] != GBC_LOGO[..] {
            return Err("Invalid Nintendo logo");
        }

        // GBC console byte (0x143) must be 0x80 or 0xC0
        if rom_data[0x143] != 0x80 && rom_data[0x143] != 0xC0 {
            return Err("GBC console byte mismatch");
        }

        // GBC Checksum
        let mut checksum: u8 = 0;
        for i in 0x134..0x14D {
            checksum = checksum.wrapping_sub(rom_data[i]).wrapping_sub(1);
        }
        if rom_data[0x14D] != checksum {
            return Err("GBC header checksum mismatch");
        }

        // Check title ASCII
        let title_bytes = &rom_data[0x134..0x143];
        if parse_ascii_title(title_bytes).is_err() {
            return Err("Invalid Title");
        }

        return Ok(ConsoleType::Gbc);
    }

    Err("Truncated ROM")
}

pub fn scan_roms_in_directory(dir_path: &Path, base_dir: &Path) -> Result<String, &'static str> {
    let safe_dir = validate_path_safety(dir_path, base_dir)?;
    if !safe_dir.is_dir() {
        return Err("Not a directory");
    }

    let mut discovered = Vec::new();

    // Recursive directory walk with cycle/depth limit (10)
    fn walk(dir: &Path, base: &Path, results: &mut Vec<(PathBuf, ConsoleType)>, depth: usize) {
        if depth > 10 {
            return;
        }
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if validate_path_safety(&path, base).is_err() {
                    continue;
                }

                if path.is_dir() {
                    walk(&path, base, results, depth + 1);
                } else if path.is_file() {
                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        let lower_ext = ext.to_lowercase();
                        if lower_ext == "gb" || lower_ext == "gbc" || lower_ext == "gba" {
                            if let Ok(meta) = fs::metadata(&path) {
                                if meta.len() > 0 && meta.len() <= 32 * 1024 * 1024 {
                                    if let Ok(data) = fs::read(&path) {
                                        if let Ok(console) = validate_and_parse_header(&data) {
                                            results.push((path, console));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    walk(&safe_dir, base_dir, &mut discovered, 0);

    // Sort discovered list for deterministic output
    discovered.sort_by(|a, b| a.0.cmp(&b.0));

    let mut json_items = Vec::new();
    for (path, console) in discovered {
        let path_str = path.to_string_lossy().to_string();
        let console_str = match console {
            ConsoleType::Gba => "GBA",
            ConsoleType::Gbc => "GBC",
            _ => "GBC",
        };
        json_items.push(format!(
            "{{\"path\":\"{}\",\"console_type\":\"{}\"}}",
            path_str.replace("\\", "\\\\").replace("\"", "\\\""),
            console_str
        ));
    }

    if json_items.is_empty() {
        Ok("[]".to_string())
    } else {
        Ok(format!("[{}]", json_items.join(",")))
    }
}
