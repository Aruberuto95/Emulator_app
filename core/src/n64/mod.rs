//! N64 cartridge boundary. The engine only receives a bounded, normalized image.
pub const MAX_ROM_BYTES: usize = 64 * 1024 * 1024;

/// Scheduler state belongs to the host wrapper, not to the reusable N64 machine.
/// Restoring the tick counter also restores the position in scripted input.
#[derive(Default)]
pub(crate) struct HostState {
    pub version: u32,
    pub ticks: u32,
    pub rendered_frames: u32,
    pub frame_credit: f64,
}
impl crate::snapshot::Snap for HostState {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.version.snap(v);
        self.ticks.snap(v);
        self.rendered_frames.snap(v);
        self.frame_credit.snap(v);
        if v.loading()
            && (self.version != 1
                || !self.frame_credit.is_finite()
                || !(0.0..1.0).contains(&self.frame_credit))
        {
            v.fail("invalid N64 scheduler state");
        }
    }
}

pub(crate) fn read_bounded(path: &std::path::Path, limit: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut data = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(limit as u64 + 1).read_to_end(&mut data))
        .map_err(|e| e.to_string())?;
    if data.len() > limit {
        return Err("N64 file exceeds size limit".into());
    }
    Ok(data)
}
pub(crate) fn load_battery(
    engine: &mut n64_engine::Engine,
    rom: &std::path::Path,
    base: &std::path::Path,
) -> Result<(), String> {
    let path = crate::rom::battery_path(rom, base)?;
    if path.exists() {
        engine.load_battery(&read_bounded(&path, n64_engine::MAX_BATTERY_BYTES)?)?;
    }
    Ok(())
}

pub fn is_header(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some([0x80, 0x37, 0x12, 0x40] | [0x37, 0x80, 0x40, 0x12] | [0x40, 0x12, 0x37, 0x80])
    )
}

pub fn normalize(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if !is_header(bytes) || !(4096..=MAX_ROM_BYTES).contains(&bytes.len()) || bytes.len() % 4 != 0 {
        return Err("Invalid N64 ROM: header, size or word alignment".into());
    }
    let mut rom = bytes.to_vec();
    match bytes[0] {
        0x37 => {
            for word in rom.chunks_exact_mut(2) {
                word.swap(0, 1);
            }
        }
        0x40 => {
            for word in rom.chunks_exact_mut(4) {
                word.reverse();
            }
        }
        _ => {}
    }
    Ok(rom)
}

pub fn input_from_buttons(b: crate::ffi::ButtonState) -> crate::ffi::N64Input {
    use crate::ffi::N64Button;
    let mut buttons = 0u16;
    for (pressed, mask) in [
        (b.a, N64Button::A),
        (b.b, N64Button::B),
        (b.select, N64Button::Z),
        (b.start, N64Button::Start),
        (b.l, N64Button::L),
        (b.r, N64Button::R),
        (b.x, N64Button::CUp),
        (b.y, N64Button::CDown),
    ] {
        if pressed {
            buttons |= mask.repr;
        }
    }
    crate::ffi::N64Input {
        buttons,
        stick_x: (i8::from(b.right) - i8::from(b.left)) * 80,
        stick_y: (i8::from(b.up) - i8::from(b.down)) * 80,
        connected: true,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_input_preserves_buttons_and_cancels_opposite_directions() {
        let mut b = crate::ffi::ButtonState::default();
        b.a = true;
        b.select = true;
        b.start = true;
        b.x = true;
        b.left = true;
        b.right = true;
        b.up = true;
        let input = super::input_from_buttons(b);
        assert_eq!(input.buttons, 0xb008);
        assert_eq!((input.stick_x, input.stick_y), (0, 80));
        assert!(input.connected);
    }
    #[test]
    fn endian_variants_normalize_to_identical_roms() {
        let mut z64 = vec![0u8; 4096];
        z64[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        z64[63] = 0x19;
        for size in [2, 4] {
            let mut swapped = z64.clone();
            for word in swapped.chunks_exact_mut(size) {
                word.reverse();
            }
            assert_eq!(super::normalize(&swapped).unwrap(), z64);
        }
        assert!(super::normalize(&z64[..4095]).is_err());
        assert!(super::normalize(&[0x80, 0x37, 0x12, 0x40]).is_err());
        assert!(super::normalize(&[0; 4096]).is_err());
    }
}
