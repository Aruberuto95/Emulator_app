// core/src/nds/sound_tables.rs
//! The three sound tables the NDS ARM7 BIOS publishes through SWI 1Ah/1Bh/1Ch
//! (GBATEK "BIOS Misc Functions": `GetSineTable`, `GetPitchTable`,
//! `GetVolumeTable`).
//!
//! The NitroSDK sound driver does not compute pitch or volume itself. For every
//! note it asks the BIOS for a table entry and feeds the result to SOUNDxTMR and
//! SOUNDxCNT. While these SWIs were unimplemented the handler fell through
//! without touching `r0`, so the driver got **its own index back** — measured on
//! SoulSilver's opening at 18973 calls to each of 1Bh and 1Ch over 900 frames.
//!
//! That is audible, and specifically it is the reported "echo". `SND_CalcTimer`
//! computes `timer * (PitchTable[i] + 0x10000) >> 16`, and the table spans
//! exactly one octave in 1/64-semitone steps (768 = 12 * 64), so a correct entry
//! runs `0..=0xFF8A` — a x1.0 .. x2.0 multiplier. The raw index only runs
//! `0..=0x2FF`, i.e. x1.0 .. x1.0117: **every interval inside an octave
//! collapsed to at most 20 cents.** Octaves still came out right, because those
//! come from the driver's own shift rather than from the table, which is why the
//! defect read as "the same note again and again" rather than as detuning. The
//! measurement that named it: one chime played five voices of a single sample at
//! 21876 / 21962 / 22048 Hz — 13.6 cents apart — plus an exact-2x pair an octave
//! below.
//!
//! Ranges here are GBATEK's, and the tables are synthesized from the relations
//! those ranges imply rather than dumped from a BIOS image (this emulator HLEs
//! the BIOS and never loads one). Each `ponytail:` below states how far the
//! synthesis is known to sit from the hardware bytes.

use std::sync::LazyLock;

/// `GetSineTable` entry count (GBATEK: index 0..3Fh).
const SINE_LEN: usize = 0x40;
/// `GetPitchTable` entry count (GBATEK: index 0..2FFh).
const PITCH_LEN: usize = 0x300;
/// `GetVolumeTable` entry count (GBATEK: index 0..2D3h).
const VOLUME_LEN: usize = 0x2D4;

/// Quarter-turn sine, `SIN(0 .. 88.6 degrees)` — 64 steps of 90/64 degrees.
///
/// ponytail: GBATEK describes the scale as `*8000h` but documents the largest
/// entry as `7FF5h`, which is what `*7FFFh` truncated produces (`*8000h` gives
/// `7FF6h`). The documented output range is the tighter constraint, so it is the
/// one reproduced here. Ceiling: at most 1 LSB from the hardware table, and
/// nothing observed calls this SWI at all (the SoulSilver census records 1Bh and
/// 1Ch only). Upgrade path: copy the real 64 entries if a BIOS dump is ever
/// available to this project.
static SINE: LazyLock<[u16; SINE_LEN]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let angle = std::f64::consts::FRAC_PI_2 * i as f64 / SINE_LEN as f64;
        (angle.sin() * 32767.0) as u16
    })
});

/// Pitch multiplier fraction: `PitchTable[i] + 0x10000 = 0x10000 * 2^(i/768)`.
///
/// ponytail: synthesized from that relation, which the documented range pins —
/// the last entry comes out `FF8Eh` against GBATEK's `FF8Ah`, i.e. 4/65536 or
/// about 0.1 cent, from the fixed-point rounding of the real BIOS table.
/// Ceiling: inaudible, and three orders of magnitude closer than the index the
/// driver was getting. Upgrade path: same as above.
static PITCH: LazyLock<[u16; PITCH_LEN]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let ratio = (i as f64 / PITCH_LEN as f64).exp2();
        (65536.0 * ratio - 65536.0).round() as u16
    })
});

/// Decibel-to-linear channel volume: index `0..=723` maps to the SOUNDxCNT
/// volume field `0..=127`.
///
/// ponytail: the curve is NOT documented, only its endpoints (`00h..7Fh` over
/// 724 entries). Reproduced as 1/10 dB steps — `127 * 10^((i-723)/200)` — which
/// hits both documented endpoints exactly (index 723 -> 127, index 0 -> 0) and
/// is monotonic throughout.
///
/// Three independent things corroborate the 1/10 dB reading, so it is more than
/// a guess:
/// * The driver pairs this table with SOUNDxCNT's volume divider, measured over
///   40 key-ons as /2 x15, /4 x22, /16 x3, /1 never. Those dividers are exactly
///   -6, -12 and -24 dB — the values a 1/10 dB unit puts at the round indices
///   -60, -120 and -240 below full scale.
/// * At 1/10 dB, index 663 (i.e. -6 dB) evaluates to 63.7, which is half of the
///   127 full-scale field. The law reproduces the divider's own first step.
/// * End to end the mixer then peaks at 23362/32767 = 0.71 of full scale with
///   zero clipped samples over the intro, which is a musically sane level; the
///   masked raw index it replaced (`0..=723 & 0x7F`) made every note's level
///   arbitrary.
///
/// Ceiling: a note's LOUDNESS may still differ from hardware in the middle of
/// the curve, where nothing above constrains it. Its pitch and timing do not.
/// Upgrade path: replace with the real 724 bytes if a BIOS dump ever becomes
/// available to this project.
static VOLUME: LazyLock<[u8; VOLUME_LEN]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let db_tenths = (i as f64) - (VOLUME_LEN - 1) as f64;
        (127.0 * 10f64.powf(db_tenths / 200.0)) as u8
    })
});

/// SWI 1Ah `GetSineTable`. Out-of-range indices are clamped: hardware would read
/// whatever BIOS bytes follow the table, which is not something to reproduce.
pub fn sine(index: u32) -> u16 {
    SINE[(index as usize).min(SINE_LEN - 1)]
}

/// SWI 1Bh `GetPitchTable`. See [`sine`] for the out-of-range rule.
pub fn pitch(index: u32) -> u16 {
    PITCH[(index as usize).min(PITCH_LEN - 1)]
}

/// SWI 1Ch `GetVolumeTable`. See [`sine`] for the out-of-range rule.
pub fn volume(index: u32) -> u8 {
    VOLUME[(index as usize).min(VOLUME_LEN - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GBATEK's documented output ranges are the specification these tables are
    /// synthesized against, so they are what the tests pin.
    #[test]
    fn tables_match_the_documented_ranges() {
        assert_eq!(sine(0), 0, "SIN(0) = 0");
        assert_eq!(sine(SINE_LEN as u32 - 1), 0x7FF5, "GBATEK: entries 0000h..7FF5h");

        assert_eq!(pitch(0), 0, "a x1.0 multiplier is a zero fraction");
        // GBATEK documents FF8Ah; the synthesized table lands 4 LSB above it.
        assert!(
            (0xFF8A..=0xFF8E).contains(&pitch(PITCH_LEN as u32 - 1)),
            "GBATEK: entries 0000h..FF8Ah, got {:#06x}",
            pitch(PITCH_LEN as u32 - 1)
        );

        assert_eq!(volume(0), 0, "GBATEK: entries 00h..7Fh");
        assert_eq!(volume(VOLUME_LEN as u32 - 1), 0x7F);
    }

    /// The property that makes them usable as tables at all: both are monotonic,
    /// so a higher index is never a lower pitch or a quieter volume.
    #[test]
    fn tables_are_monotonic_and_clamped() {
        for i in 1..PITCH_LEN as u32 {
            assert!(pitch(i) >= pitch(i - 1), "pitch dips at {i}");
        }
        for i in 1..VOLUME_LEN as u32 {
            assert!(volume(i) >= volume(i - 1), "volume dips at {i}");
        }
        // Out of range must saturate, never panic: `index` reaches these from a
        // guest register and a Rust panic aborts the process across the FFI.
        assert_eq!(pitch(u32::MAX), pitch(PITCH_LEN as u32 - 1));
        assert_eq!(volume(u32::MAX), volume(VOLUME_LEN as u32 - 1));
        assert_eq!(sine(u32::MAX), sine(SINE_LEN as u32 - 1));
    }

    /// The relation the driver actually consumes: `PitchTable[i] + 0x10000` is
    /// `0x10000 * 2^(i/768)`, so half the table is a x1.5 multiplier (a fifth)
    /// and a full octave is x2. This is the property whose absence collapsed
    /// every interval to under 20 cents.
    #[test]
    fn pitch_table_is_one_octave_of_equal_temperament() {
        let mult = |i: u32| (f64::from(pitch(i)) + 65536.0) / 65536.0;
        // 12 semitones * 64 steps: one semitone is 64 entries.
        assert!((mult(64) - 2f64.powf(1.0 / 12.0)).abs() < 1e-4, "one semitone");
        // Equal temperament, not just intonation: the tempered fifth is
        // 2^(7/12) = 1.49831, two cents flat of the 3:2 ratio. Asserting 1.5
        // here fails against a correct table.
        assert!((mult(448) - 2f64.powf(7.0 / 12.0)).abs() < 1e-4, "seven semitones");
        // The table is an octave EXCLUSIVE of its endpoint: the last entry is
        // 2^(767/768), one step short of x2, because the doubling itself is the
        // driver's own shift. Asserting x2.0 here fails against a correct table.
        let last = mult(PITCH_LEN as u32 - 1);
        assert!(
            (last - 2f64.powf(767.0 / 768.0)).abs() < 1e-4 && last < 2.0,
            "the table spans one octave minus one step, got {last}"
        );
        // The defect, stated as a test: the raw index spans 1.17% of an octave.
        let index_as_multiplier = (f64::from(PITCH_LEN as u16 - 1) + 65536.0) / 65536.0;
        assert!(
            index_as_multiplier < 1.012,
            "returning the index instead of the entry must be a <20-cent spread"
        );
    }
}
