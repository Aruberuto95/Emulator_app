//! Versioned binary snapshot codec for console state too large for the JSON
//! savestate container in [`crate::savestate`].
//!
//! # Why a second container
//!
//! The JSON container stores byte arrays as hex (2 ASCII chars per byte, one
//! `format!` per byte) and `load_state` refuses files over 2 MB. A Nintendo DS
//! state is ~5.3 MB of raw memory before encoding, so it fits neither the size
//! limit nor the encoding cost. This module is the binary sidecar the NDS branch
//! uses; GBC/GBA states keep their JSON format untouched.
//!
//! # One field list, two directions
//!
//! Save and load are the *same* traversal, driven by a [`Visitor`]: [`Writer`]
//! appends the bytes a field currently holds, [`Reader`] overwrites the field
//! from the stream. A type therefore declares its fields exactly once, in
//! [`Snap::snap`], and the two directions cannot drift apart — the failure mode
//! of hand-written save/load pairs, where a field added to one is forgotten in
//! the other and the state silently restores stale data.
//!
//! Each type implements [`Snap`] in its own module so it can reach its private
//! fields; the codec never needs them to be public.
//!
//! # Trusting the input
//!
//! A snapshot file is untrusted input: it can be truncated, corrupted, or
//! hand-crafted. The reader therefore
//!
//! * bounds every read against the buffer and latches a failure on overrun,
//! * never allocates from a length in the file — fixed buffers are sized from
//!   the emulator's own constants, and variable-length ones are capped
//!   ([`snap_capped_vec`]),
//! * verifies magic, version and a content hash before any field is applied.
//!
//! A failed load must leave the emulator on the pre-load state; callers get an
//! `Err` and apply the snapshot to a scratch instance, never in place.

/// File magic. Bumping it invalidates every existing snapshot, so change the
/// version instead.
pub const MAGIC: [u8; 8] = *b"EMUSNAP\0";

/// Container version. Bump on any change to a field list: an older payload
/// would otherwise be read with the new layout and silently mis-restore.
///
/// * 1 — initial NDS layout.
/// * 2 — dropped the resampler's output rate (host device property, not machine
///   state; carrying it desynced the core from the device's real rate).
/// * 3 — NDS CPU clock credits and pending 3D frame/geometry state. Version 2
///   loads with zero credits and no unpublished 3D work.
/// * 4 — full ROM fingerprint in the container header; v2/v3 still load.
pub const VERSION: u32 = 4;

/// Hard ceiling on a snapshot file, applied before it is read into memory. The
/// NDS payload is ~5.3 MB; 64 MB leaves room for future consoles while keeping
/// a corrupt or hostile length from exhausting memory.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Why a snapshot could not be decoded. Every variant is safe to show a player.
#[derive(Debug, PartialEq, Eq)]
pub enum SnapError {
    /// Not a snapshot file at all.
    Magic,
    /// Written by a different container version.
    Version(u32),
    /// Stream ended early, or a field rejected its value.
    Malformed(&'static str),
    /// Content hash did not match the header.
    Hash,
    /// Snapshot belongs to a different cartridge.
    WrongRom,
}

impl std::fmt::Display for SnapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapError::Magic => write!(f, "not a snapshot file"),
            SnapError::Version(v) => write!(f, "unsupported snapshot version {v}"),
            SnapError::Malformed(why) => write!(f, "malformed snapshot ({why})"),
            SnapError::Hash => write!(f, "snapshot failed its integrity check"),
            SnapError::WrongRom => write!(f, "snapshot belongs to a different ROM"),
        }
    }
}

/// One direction of a snapshot traversal. See the module docs.
pub trait Visitor {
    /// Layout being traversed. Writers always produce the current version.
    fn version(&self) -> u32 { VERSION }

    /// Save: append the current contents of `bytes`. Load: overwrite `bytes`
    /// from the stream. A no-op once the stream has failed.
    fn visit_raw(&mut self, bytes: &mut [u8]);

    /// True while restoring, so a field can validate what it just read.
    fn loading(&self) -> bool;

    /// Reject the stream. Later visits become no-ops and the load fails.
    fn fail(&mut self, why: &'static str);

    /// The first rejection, if any.
    fn error(&self) -> Option<&'static str>;
}

/// A type that can be snapshotted. Implement it next to the type so the field
/// list can reach private fields.
pub trait Snap {
    /// Visit every field that is part of the saved state, in a fixed order.
    fn snap(&mut self, v: &mut dyn Visitor);
}

/// Save direction: grows a byte vector.
#[derive(Default)]
pub struct Writer {
    pub out: Vec<u8>,
}

impl Writer {
    pub fn with_capacity(n: usize) -> Self {
        Self { out: Vec::with_capacity(n) }
    }
}

impl Visitor for Writer {
    fn visit_raw(&mut self, bytes: &mut [u8]) {
        self.out.extend_from_slice(bytes);
    }
    fn loading(&self) -> bool {
        false
    }
    /// Writing cannot fail: the state being saved is by definition valid, and
    /// swallowing it here keeps `Snap` impls free of direction checks.
    fn fail(&mut self, _why: &'static str) {}
    fn error(&self) -> Option<&'static str> {
        None
    }
}

/// Load direction: consumes a byte slice.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    err: Option<&'static str>,
    version: u32,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self::with_version(buf, VERSION)
    }

    /// Use the version from an already validated snapshot header.
    pub fn with_version(buf: &'a [u8], version: u32) -> Self {
        Self { buf, pos: 0, err: None, version }
    }

    /// Bytes consumed so far.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Consume the reader, failing if anything went wrong or if the payload had
    /// trailing bytes. Leftover bytes mean the writer and reader disagree about
    /// the layout, which is exactly the drift this codec exists to prevent.
    pub fn finish(self) -> Result<(), SnapError> {
        if let Some(why) = self.err {
            return Err(SnapError::Malformed(why));
        }
        if self.pos != self.buf.len() {
            return Err(SnapError::Malformed("trailing bytes"));
        }
        Ok(())
    }
}

impl Visitor for Reader<'_> {
    fn version(&self) -> u32 { self.version }

    fn visit_raw(&mut self, bytes: &mut [u8]) {
        if self.err.is_some() {
            return;
        }
        match self.buf.get(self.pos..self.pos + bytes.len()) {
            Some(src) => {
                bytes.copy_from_slice(src);
                self.pos += bytes.len();
            }
            None => self.err = Some("stream ended early"),
        }
    }
    fn loading(&self) -> bool {
        true
    }
    fn fail(&mut self, why: &'static str) {
        if self.err.is_none() {
            self.err = Some(why);
        }
    }
    fn error(&self) -> Option<&'static str> {
        self.err
    }
}

/// Fixed-width little-endian primitives. `to_le_bytes`/`from_le_bytes` make the
/// on-disk layout host-endian-independent.
macro_rules! snap_le_primitive {
    ($($t:ty),* $(,)?) => {
        $(impl Snap for $t {
            fn snap(&mut self, v: &mut dyn Visitor) {
                let mut b = self.to_le_bytes();
                v.visit_raw(&mut b);
                *self = <$t>::from_le_bytes(b);
            }
        })*
    };
}
snap_le_primitive!(u8, u16, u32, u64, i8, i16, i32, i64, f32, f64);

impl Snap for bool {
    fn snap(&mut self, v: &mut dyn Visitor) {
        let mut b = u8::from(*self);
        b.snap(v);
        *self = b != 0;
    }
}

/// Stored as `u32` so a snapshot is portable across pointer widths.
impl Snap for usize {
    fn snap(&mut self, v: &mut dyn Visitor) {
        let mut n = u32::try_from(*self).unwrap_or(u32::MAX);
        n.snap(v);
        *self = n as usize;
    }
}

impl<T: Snap, const N: usize> Snap for [T; N] {
    fn snap(&mut self, v: &mut dyn Visitor) {
        for e in self.iter_mut() {
            e.snap(v);
        }
    }
}

impl<T: Snap + Default> Snap for Option<T> {
    fn snap(&mut self, v: &mut dyn Visitor) {
        let mut present = self.is_some();
        present.snap(v);
        if v.loading() && present && self.is_none() {
            *self = Some(T::default());
        }
        match self.as_mut() {
            Some(inner) if present => inner.snap(v),
            // Absent on disk: drop whatever the field held, and visit a scratch
            // value so both directions consume the same number of bytes.
            _ => {
                let mut scratch = T::default();
                scratch.snap(v);
                if v.loading() {
                    *self = None;
                }
            }
        }
    }
}

impl<A: Snap, B: Snap> Snap for (A, B) {
    fn snap(&mut self, v: &mut dyn Visitor) {
        self.0.snap(v);
        self.1.snap(v);
    }
}

impl<A: Snap, B: Snap, C: Snap> Snap for (A, B, C) {
    fn snap(&mut self, v: &mut dyn Visitor) {
        self.0.snap(v);
        self.1.snap(v);
        self.2.snap(v);
    }
}

impl<A: Snap, B: Snap, C: Snap, D: Snap> Snap for (A, B, C, D) {
    fn snap(&mut self, v: &mut dyn Visitor) {
        self.0.snap(v);
        self.1.snap(v);
        self.2.snap(v);
        self.3.snap(v);
    }
}

/// Snapshot a byte buffer whose length is fixed by the emulator (a memory
/// region). The length comes from `len`, never from the file, so a hostile
/// snapshot cannot drive an allocation. A single `visit_raw` moves the whole
/// region — the per-element path would cost one virtual call per byte.
///
/// The length is still *recorded* and checked: a file whose region is a
/// different size than this build expects would otherwise slide every later
/// field by that difference, restoring plausible-looking garbage instead of
/// failing.
/// `label` names the region in the failure message: "which buffer changed size"
/// is the only question worth asking when this fires.
pub fn snap_bytes(v: &mut dyn Visitor, data: &mut Vec<u8>, len: usize, label: &'static str) {
    let mut stored = len;
    stored.snap(v);
    if v.loading() && stored != len {
        v.fail(label);
        return;
    }
    if data.len() != len {
        data.resize(len, 0);
    }
    v.visit_raw(&mut data[..]);
}

/// Snapshot a fixed-length buffer of non-byte elements (framebuffers, depth
/// buffers). Same contract as [`snap_bytes`]: `len` is the emulator's, not the
/// file's.
pub fn snap_fixed_vec<T: Snap + Clone + Default>(
    v: &mut dyn Visitor,
    data: &mut Vec<T>,
    len: usize,
) {
    if data.len() != len {
        data.resize(len, T::default());
    }
    for e in data.iter_mut() {
        e.snap(v);
    }
}

/// Snapshot a variable-length vector, rejecting any length above `cap`.
///
/// Used for queues (IPC FIFOs, device response buffers) whose depth is part of
/// the state. The cap is the hardware's own limit, so a file claiming more is
/// corrupt by definition and is refused before the allocation happens.
pub fn snap_capped_vec<T: Snap + Clone + Default>(
    v: &mut dyn Visitor,
    data: &mut Vec<T>,
    cap: usize,
) {
    let mut n = data.len();
    n.snap(v);
    if v.loading() {
        if n > cap {
            v.fail("vector length over cap");
            return;
        }
        data.clear();
        data.resize(n, T::default());
    }
    for e in data.iter_mut() {
        e.snap(v);
    }
}

/// Snapshot a C-like enum through its discriminant.
///
/// `to_index` must be the inverse of `from_index`; an out-of-range index in the
/// file rejects the stream instead of silently landing on a default state,
/// because a device state machine resumed in the wrong phase corrupts saves.
pub fn snap_enum<T: Copy>(
    v: &mut dyn Visitor,
    slot: &mut T,
    to_index: impl Fn(T) -> u8,
    from_index: impl Fn(u8) -> Option<T>,
) {
    let mut idx = to_index(*slot);
    idx.snap(v);
    if v.loading() {
        match from_index(idx) {
            Some(val) => *slot = val,
            None => v.fail("enum discriminant out of range"),
        }
    }
}

/// FNV-1a over the payload: an integrity check against truncation and bit rot,
/// not an authenticity check — a snapshot file is only as trustworthy as the
/// directory it sits in.
pub fn content_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Header written ahead of every payload: `MAGIC`, version, console tag, ROM
/// identity, payload length and content hash.
pub struct Header {
    pub console: u8,
    /// Cartridge gamecode (NDS header 0x0C) or 0 when the ROM has none.
    pub gamecode: [u8; 4],
    pub rom_len: u64,
    pub rom_hash: u64,
}

/// Bytes a serialized [`Header`] occupies, plus the payload length and hash
/// fields that follow it.
const HEADER_BYTES: usize = 8 + 4 + 1 + 4 + 8 + 4 + 8;

impl Header {
    /// Serialize `self` + `payload` into a complete snapshot file image.
    pub fn wrap(&self, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_BYTES + 8 + payload.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.push(self.console);
        out.extend_from_slice(&self.gamecode);
        out.extend_from_slice(&self.rom_len.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&content_hash(payload).to_le_bytes());
        out.extend_from_slice(&self.rom_hash.to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// Validate a file image and split off its payload.
    ///
    /// `expect` is the identity of the currently loaded cartridge; a mismatch is
    /// refused so a state cannot be restored over the wrong game.
    pub fn unwrap_payload<'a>(file: &'a [u8], expect: &Header) -> Result<&'a [u8], SnapError> {
        if file.len() < HEADER_BYTES || file[..8] != MAGIC {
            return Err(SnapError::Magic);
        }
        let u32_at = |o: usize| u32::from_le_bytes([file[o], file[o + 1], file[o + 2], file[o + 3]]);
        let version = u32_at(8);
        if !(2..=VERSION).contains(&version) {
            return Err(SnapError::Version(version));
        }
        let console = file[12];
        let gamecode: [u8; 4] = file[13..17].try_into().expect("4 bytes");
        let rom_len = u64::from_le_bytes(file[17..25].try_into().expect("8 bytes"));
        if console != expect.console || gamecode != expect.gamecode || rom_len != expect.rom_len {
            return Err(SnapError::WrongRom);
        }
        let payload_len = u32_at(25) as usize;
        let hash = u64::from_le_bytes(file[29..37].try_into().expect("8 bytes"));
        let start = if version >= 4 {
            let fingerprint = file.get(HEADER_BYTES..HEADER_BYTES + 8)
                .ok_or(SnapError::Malformed("missing ROM fingerprint"))?;
            if u64::from_le_bytes(fingerprint.try_into().unwrap()) != expect.rom_hash {
                return Err(SnapError::WrongRom);
            }
            HEADER_BYTES + 8
        } else { HEADER_BYTES };
        let payload = file
            .get(start..start + payload_len)
            .ok_or(SnapError::Malformed("payload shorter than its header claims"))?;
        if file.len() != start + payload_len {
            return Err(SnapError::Malformed("trailing bytes after payload"));
        }
        if content_hash(payload) != hash {
            return Err(SnapError::Hash);
        }
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A struct whose field list is declared once and walked in both
    /// directions: the property the whole codec rests on.
    #[derive(Default, PartialEq, Debug, Clone)]
    struct Toy {
        a: u32,
        b: bool,
        c: [u16; 3],
        d: Vec<u8>,
        e: Option<u32>,
        f: Vec<u32>,
        g: f32,
        h: usize,
    }

    impl Snap for Toy {
        fn snap(&mut self, v: &mut dyn Visitor) {
            self.a.snap(v);
            self.b.snap(v);
            self.c.snap(v);
            snap_bytes(v, &mut self.d, 4, "toy d size mismatch");
            self.e.snap(v);
            snap_capped_vec(v, &mut self.f, 8);
            self.g.snap(v);
            self.h.snap(v);
        }
    }

    fn sample() -> Toy {
        Toy {
            a: 0xDEAD_BEEF,
            b: true,
            c: [1, 2, 0xFFFF],
            d: vec![9, 8, 7, 6],
            e: Some(42),
            f: vec![5, 6, 7],
            g: -0.25,
            h: 1234,
        }
    }

    fn save(t: &mut Toy) -> Vec<u8> {
        let mut w = Writer::default();
        t.snap(&mut w);
        w.out
    }

    #[test]
    fn round_trip_restores_every_field() {
        let bytes = save(&mut sample());
        let mut got = Toy::default();
        let mut r = Reader::new(&bytes);
        got.snap(&mut r);
        r.finish().expect("clean stream");
        assert_eq!(got, sample());
    }

    #[test]
    fn absent_option_clears_a_present_field() {
        let mut none = Toy { e: None, ..sample() };
        let bytes = save(&mut none);
        let mut got = sample(); // starts with e = Some(42)
        let mut r = Reader::new(&bytes);
        got.snap(&mut r);
        r.finish().expect("clean stream");
        assert_eq!(got.e, None, "a None on disk must clear the live value");
        assert_eq!(got, none);
    }

    #[test]
    fn truncated_stream_fails_instead_of_restoring_partial_state() {
        let bytes = save(&mut sample());
        let mut got = Toy::default();
        let mut r = Reader::new(&bytes[..bytes.len() - 3]);
        got.snap(&mut r);
        assert_eq!(r.finish(), Err(SnapError::Malformed("stream ended early")));
    }

    #[test]
    fn trailing_bytes_fail_so_layout_drift_cannot_pass_silently() {
        let mut bytes = save(&mut sample());
        bytes.push(0);
        let mut got = Toy::default();
        let mut r = Reader::new(&bytes);
        got.snap(&mut r);
        assert_eq!(r.finish(), Err(SnapError::Malformed("trailing bytes")));
    }

    /// A length field is the one number in the file that could drive an
    /// allocation, so it must be rejected against the hardware cap.
    #[test]
    fn oversized_vector_length_is_refused() {
        let mut bytes = save(&mut sample());
        // Rewrite the capped vector's length (u32 at the fixed offset after
        // a=4, b=1, c=6, d=4+4, e=1+4 bytes) to 9, one past the cap of 8.
        let off = 4 + 1 + 6 + 8 + 5;
        bytes[off..off + 4].copy_from_slice(&9u32.to_le_bytes());
        let mut got = Toy::default();
        let mut r = Reader::new(&bytes);
        got.snap(&mut r);
        assert_eq!(r.finish(), Err(SnapError::Malformed("vector length over cap")));
        assert!(got.f.is_empty(), "no allocation from a rejected length");
    }

    /// A memory region that changed size between builds must fail loudly, not
    /// slide every later field by the difference.
    #[test]
    fn region_size_mismatch_is_refused() {
        let mut w = Writer::default();
        let mut wrong = vec![1u8; 8];
        snap_bytes(&mut w, &mut wrong, 8, "toy region size mismatch");
        let mut r = Reader::new(&w.out);
        let mut here = vec![0u8; 4];
        snap_bytes(&mut r, &mut here, 4, "toy region size mismatch");
        assert_eq!(r.error(), Some("toy region size mismatch"));
        assert_eq!(here, vec![0, 0, 0, 0], "nothing applied from a rejected region");
    }

    #[test]
    fn header_rejects_magic_version_rom_and_tamper() {
        let id = Header { console: 2, gamecode: *b"IPGE", rom_len: 128, rom_hash: 42 };
        let file = id.wrap(&[1, 2, 3, 4]);
        assert_eq!(Header::unwrap_payload(&file, &id).unwrap(), &[1, 2, 3, 4]);

        assert_eq!(Header::unwrap_payload(b"short", &id), Err(SnapError::Magic));
        let mut bad_magic = file.clone();
        bad_magic[0] = b'X';
        assert_eq!(Header::unwrap_payload(&bad_magic, &id), Err(SnapError::Magic));

        let mut bad_version = file.clone();
        bad_version[8..12].copy_from_slice(&999u32.to_le_bytes());
        assert_eq!(Header::unwrap_payload(&bad_version, &id), Err(SnapError::Version(999)));

        let other_rom = Header { console: 2, gamecode: *b"ADAE", rom_len: 128, rom_hash: 42 };
        assert_eq!(Header::unwrap_payload(&file, &other_rom), Err(SnapError::WrongRom));

        let mut tampered = file.clone();
        *tampered.last_mut().unwrap() ^= 0xFF;
        assert_eq!(Header::unwrap_payload(&tampered, &id), Err(SnapError::Hash));

        let mut truncated = file.clone();
        truncated.pop();
        assert_eq!(
            Header::unwrap_payload(&truncated, &id),
            Err(SnapError::Malformed("payload shorter than its header claims"))
        );
    }

    #[test]
    fn enum_discriminant_out_of_range_is_refused() {
        #[derive(Copy, Clone, PartialEq, Debug)]
        enum Phase {
            Idle,
            Busy,
        }
        let to = |p: Phase| match p {
            Phase::Idle => 0,
            Phase::Busy => 1,
        };
        let from = |i: u8| match i {
            0 => Some(Phase::Idle),
            1 => Some(Phase::Busy),
            _ => None,
        };
        let mut r = Reader::new(&[7]);
        let mut slot = Phase::Idle;
        snap_enum(&mut r, &mut slot, to, from);
        assert_eq!(slot, Phase::Idle, "rejected value must not be applied");
        assert_eq!(r.error(), Some("enum discriminant out of range"));
    }
}
