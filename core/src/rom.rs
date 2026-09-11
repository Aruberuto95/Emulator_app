use std::fs;
use std::io::Read;
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

pub(crate) fn extension_console(path: &Path) -> Option<ConsoleType> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "gb" | "gbc" => Some(ConsoleType::Gbc),
        "gba" => Some(ConsoleType::Gba),
        "nds" => Some(ConsoleType::Nds),
        "z64" | "v64" | "n64" => Some(ConsoleType::N64),
        _ => None,
    }
}
pub(crate) fn rom_limit(console: ConsoleType) -> usize {
    match console { ConsoleType::Nds => 128*1024*1024, ConsoleType::N64 => crate::n64::MAX_ROM_BYTES, _ => 32*1024*1024 }
}
pub fn validate_and_parse_header(rom_data: &[u8]) -> Result<ConsoleType, &'static str> {
    if rom_data.is_empty() {
        return Err("Empty ROM file");
    }

    if crate::n64::is_header(rom_data) {
        return if rom_data.len() >= 64 { Ok(ConsoleType::N64) } else { Err("Truncated N64 header") };
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
                    } else if c == "NDS" {
                        return Ok(ConsoleType::Nds);
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

    // Check NDS (NDS requires at least 0x200 bytes, GBA logo at 0xC0)
    if rom_data.len() >= 0x200 && rom_data[0x0C0..0x0C0 + 156] == GBA_LOGO[..] {
        // ponytail: el header CRC-16 (0x15E) no es un gate de carga â€” ni el hardware
        // real ni melonDS/DeSmuME rechazan por Ã©l. La detecciÃ³n NDS es el match del
        // logo Nintendo (0x0C0). Integridad de descarga = hash del archivo completo
        // (otra feature, si alguna vez se pide).
        let title_bytes = &rom_data[0x000..0x00C];
        if parse_ascii_title(title_bytes).is_err() {
            return Err("Invalid Title");
        }
        return Ok(ConsoleType::Nds);
    }

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

pub fn scan_entries(dir_path: &Path, base_dir: &Path) -> Result<Vec<crate::ffi::RomEntry>, &'static str> {
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
                        let _ = ext;
                        if let Some(console) = extension_console(&path) {
                            if let Ok(meta) = fs::metadata(&path) {
                                let max_size = rom_limit(console) as u64;
                                if meta.len() > 0 && meta.len() <= max_size {
                                    if let Ok(data) = read_rom_header(&path) {
                                        if let Ok(console) = validate_and_parse_header(&data) {
                                            if meta.len() > rom_limit(console) as u64 ||
                                                (console == ConsoleType::N64 && (meta.len()<4096 || meta.len()%4!=0)) {
                                                continue;
                                            }
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

    Ok(discovered.into_iter().map(|(path,console_type)| crate::ffi::RomEntry {
        path: path.to_string_lossy().into_owned(), console_type
    }).collect())
}

pub fn scan_roms_in_directory(dir_path: &Path, base_dir: &Path) -> Result<String, &'static str> {
    let entries=scan_entries(dir_path,base_dir)?;
    let values:Vec<_>=entries.into_iter().map(|entry| {
        let console=match entry.console_type {
            ConsoleType::Gbc=>"GBC",ConsoleType::Gba=>"GBA",ConsoleType::Nds=>"NDS",ConsoleType::N64=>"N64",_=>"UNKNOWN"
        };
        serde_json::json!({"path":entry.path,"console_type":console})
    }).collect();
    serde_json::to_string(&values).map_err(|_|"ROM list serialization failed")
}

/// Detection only needs the largest supported header (NDS, 512 bytes).
/// Keep the file-size gate in the caller; a library scan must not load ROM bodies.
fn read_rom_header(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut header = Vec::with_capacity(0x200);
    fs::File::open(path)?.take(0x200).read_to_end(&mut header)?;
    Ok(header)
}

/// Resolve `<rom>.sav` for a cartridge, validated against `base_dir`.
///
/// Shared by every battery-backed console so the path rules are stated once.
/// Two guards beyond `validate_path_safety`:
/// * the derived path must not equal the ROM path â€” `with_extension("sav")` is
///   the identity for a ROM that is itself named `*.sav`, and writing the save
///   would then destroy the user's ROM;
/// * the caller gets the validated path only, so no caller can invent a sibling
///   path (such as a temp file) that skipped the gate.
pub(crate) fn battery_path(rom_path: &Path, base_dir: &Path) -> Result<PathBuf, String> {
    let save_path = rom_path.with_extension("sav");
    let safe_rom_path = validate_path_safety(rom_path, base_dir)
        .map_err(|e| format!("ROM path safety error: {}", e))?;
    let safe_save_path = validate_path_safety(&save_path, base_dir)
        .map_err(|e| format!("Save path safety error: {}", e))?;
    // Resolve both names before comparing: Windows treats game.SAV and
    // game.sav as the same file, and a save symlink can alias the ROM too.
    if safe_save_path == safe_rom_path {
        return Err("Save path would overwrite the ROM".to_string());
    }
    Ok(safe_save_path)
}

/// Write a battery save atomically: full contents to a temp file, then rename.
///
/// The temp path is validated in its own right. Deriving it from an
/// already-validated save path is not sufficient â€” a pre-existing symlink at
/// `<rom>.tmp` would be followed by the write and place attacker-chosen content
/// outside `base_dir`, which is the same class of hole `validate_path_safety`
/// exists to close.
pub(crate) fn write_battery_file(
    rom_path: &Path,
    base_dir: &Path,
    data: &[u8],
) -> Result<(), String> {
    let safe_save_path = battery_path(rom_path, base_dir)?;
    let tmp_path = validate_path_safety(&safe_save_path.with_extension("tmp"), base_dir)
        .map_err(|e| format!("Save temp path safety error: {}", e))?;
    let safe_rom_path = validate_path_safety(rom_path, base_dir)
        .map_err(|e| format!("ROM path safety error: {}", e))?;
    if tmp_path == safe_rom_path {
        return Err("Save temp path would overwrite the ROM".to_string());
    }

    // ponytail: MOCK_DISK_FULL is a live test hook in release builds. Ceiling:
    // an inherited environment variable silently disables saving for every
    // console. Upgrade path: gate it behind a cargo feature once the pytest
    // suite that sets it drives the real binary rather than the Python mock.
    if std::env::var("MOCK_DISK_FULL").unwrap_or_default() == "1" {
        return Err("SAVE_STATE_ERROR Disk full".to_string());
    }

    fs::write(&tmp_path, data).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        format!("Failed to write temporary save: {}", e)
    })?;
    fs::rename(&tmp_path, &safe_save_path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        format!("Failed to finalize save file: {}", e)
    })
}

/// Read a battery save into `dest`, copying at most `dest.len()` bytes and
/// never allocating more than that regardless of the file's size.
///
/// Returns whether a save file existed. The destination length is fixed by the
/// emulated chip, so a truncated file leaves the tail untouched and an oversized
/// one is ignored past the device end â€” the file can never resize the device,
/// and a hostile multi-gigabyte `.sav` cannot force a matching allocation.
pub(crate) fn read_battery_file(
    rom_path: &Path,
    base_dir: &Path,
    dest: &mut [u8],
) -> Result<bool, String> {
    use std::io::Read;
    let safe_save_path = battery_path(rom_path, base_dir)?;
    if !safe_save_path.exists() {
        return Ok(false);
    }
    let file = fs::File::open(&safe_save_path).map_err(|e| format!("Failed to read save: {}", e))?;
    let mut read = 0usize;
    let mut limited = file.take(dest.len() as u64);
    loop {
        match limited.read(&mut dest[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(e) => return Err(format!("Failed to read save: {}", e)),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{validate_and_parse_header, ConsoleType, GBA_LOGO};

    #[test]
    fn maintenance_rom_scan_reads_only_headers_and_keeps_detection_results() {
        use std::io::Write;
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("emu_maintenance_scan_{}_{nonce}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let mut header = vec![0u8; 0x200];
        header[0xC0..0xC0 + GBA_LOGO.len()].copy_from_slice(&GBA_LOGO);
        let nds = dir.join("large.nds");
        let mut file = std::fs::File::create(&nds).unwrap();
        file.write_all(&header).unwrap();
        file.set_len(4 * 1024 * 1024).unwrap();
        drop(file);
        assert_eq!(super::read_rom_header(&nds).unwrap(), header);

        let gba = dir.join("valid.gba");
        let mut gba_header = vec![0u8; 0xC0];
        gba_header[4..4 + GBA_LOGO.len()].copy_from_slice(&GBA_LOGO);
        gba_header[0xB2] = 0x96;
        gba_header[0xBD] = gba_header[0xA0..0xBD].iter().fold(0u8, |sum, &byte| sum.wrapping_sub(byte)).wrapping_sub(0x19);
        std::fs::write(&gba, &gba_header).unwrap();
        assert_eq!(super::read_rom_header(&gba).unwrap(), gba_header);
        let truncated = dir.join("truncated.gbc");
        std::fs::write(&truncated, [0u8; 20]).unwrap();
        assert_eq!(super::read_rom_header(&truncated).unwrap().len(), 20);
        let invalid = dir.join("invalid.nds");
        std::fs::write(&invalid, [0u8; 512]).unwrap();

        let scanned = super::scan_roms_in_directory(&dir, &dir).unwrap();
        assert!(scanned.contains("large.nds") && scanned.contains("\"console_type\":\"NDS\""));
        assert!(scanned.contains("valid.gba") && scanned.contains("\"console_type\":\"GBA\""));
        assert!(!scanned.contains("truncated") && !scanned.contains("invalid"));
        for path in [nds, gba, truncated, invalid] { std::fs::remove_file(path).unwrap(); }
        std::fs::remove_dir(dir).unwrap();
    }

    // Header NDS sintÃ©tico mÃ­nimo (0x200 bytes): logo Nintendo vÃ¡lido en 0x0C0 pero
    // bytes de checksum BASURA en 0x15C..0x160. Antes del fix el gate de CRC lo
    // rechazaba con "NDS header checksum mismatch"; ahora debe detectarse como NDS
    // porque el checksum del header ya no es un gate de carga (solo logo + tÃ­tulo).
    #[test]
    fn nds_loads_with_bogus_header_checksum() {
        let mut rom = vec![0u8; 0x200];
        rom[0x000..0x004].copy_from_slice(b"TEST"); // tÃ­tulo ASCII vÃ¡lido
        rom[0x0C0..0x0C0 + 156].copy_from_slice(&GBA_LOGO); // logo => detecciÃ³n NDS
        rom[0x15C..0x160].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // checksums basura
        assert_eq!(validate_and_parse_header(&rom), Ok(ConsoleType::Nds));
    }
}
