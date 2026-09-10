// core/src/nds/mmu.rs

use crate::nds::cpu::Cp15Registers;
use crate::nds::spi::SpiController;
use crate::ffi::ButtonState;
use crate::snapshot::{snap_bytes, snap_capped_vec};

#[derive(Clone, Debug, Default)]
pub struct IpcState {
    pub arm9_to_arm7_sync: u8,       // Bits 8-11 of ARM9 IPCSYNC
    pub arm7_to_arm9_sync: u8,       // Bits 8-11 of ARM7 IPCSYNC
    pub arm9_sync_irq_enable: bool,  // Bit 14 of ARM9 IPCSYNC
    pub arm7_sync_irq_enable: bool,  // Bit 14 of ARM7 IPCSYNC

    // Directional FIFOs (max 16 words)
    pub fifo_9to7: Vec<u32>,
    pub fifo_7to9: Vec<u32>,
    pub fifo_control_arm9: u16,      // REG_IPC_FIFO_CNT for ARM9
    pub fifo_control_arm7: u16,      // REG_IPC_FIFO_CNT for ARM7
}

impl IpcState {
    pub fn new() -> Self {
        Self {
            arm9_to_arm7_sync: 0,
            arm7_to_arm9_sync: 0,
            arm9_sync_irq_enable: false,
            arm7_sync_irq_enable: false,
            fifo_9to7: Vec::with_capacity(16),
            fifo_7to9: Vec::with_capacity(16),
            fifo_control_arm9: 0,
            fifo_control_arm7: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct VramBank {
    pub data: Vec<u8>,
    pub control: u8,
}

impl VramBank {
    /// Byte index inside this bank for an address in the ARM7's 0x06000000
    /// window, or `None` when the bank is not mapped there.
    ///
    /// GBATEK: banks C and D reach the ARM7 with MST = 2, and the mapped base is
    /// `0x06000000 + OFS * 0x20000` — OFS is VRAMCNT bit 3. The decode used to
    /// hardcode C to the first 128 KB and D to the second, which is only right
    /// because libnds happens to define `VRAM_C_ARM7` with OFS 0 and
    /// `VRAM_D_ARM7` with OFS 1. A game that maps D at OFS 0 (or C at OFS 1)
    /// read zeros and had its stores dropped.
    pub fn arm7_window_slot(&self, offset: u32) -> Option<usize> {
        if self.control & 0x80 == 0 || self.control & 0x07 != 2 {
            return None;
        }
        let base = u32::from((self.control >> 3) & 1) * 0x20000;
        let rel = offset.wrapping_sub(base);
        if (rel as usize) < self.data.len() && rel < 0x20000 {
            Some(rel as usize)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug)]
pub struct VramManager {
    pub banks: [VramBank; 9], // Banks A-I
    /// Writes that reached no enabled bank, in total and per MST target. Purely
    /// diagnostic (not snapshotted): it answers "did that upload land anywhere?"
    pub dropped_writes: u64,
    pub dropped_by_target: [u64; 8],
}

/// How many `(pc, teximage)` pairs [`NdsMmu::gx_tex_watch`] keeps. Bounded so an
/// armed watch cannot grow without limit inside a long run.
const GX_TEX_WATCH_CAP: usize = 64;

/// ARM7 IF bit for "SPI bus transfer complete" (GBATEK, NDS7 Interrupt Flags:
/// bit 23). Named because all three SPI completion sites previously raised
/// `1 << 8`, which is **DMA 0** — so every SPI byte requested a DMA0 interrupt
/// on the core that runs the sound driver, and the touchscreen is polled once
/// per frame. Bit 23 is NDS7-only, which is why it has no ARM9 counterpart.
const IF_ARM7_SPI: u32 = 1 << 23;

impl VramManager {
    pub fn new() -> Self {
        Self {
            banks: [
                VramBank { data: vec![0; 128 * 1024], control: 0 }, // A: 128KB
                VramBank { data: vec![0; 128 * 1024], control: 0 }, // B: 128KB
                VramBank { data: vec![0; 128 * 1024], control: 0 }, // C: 128KB
                VramBank { data: vec![0; 128 * 1024], control: 0 }, // D: 128KB
                VramBank { data: vec![0; 64 * 1024], control: 0 },  // E: 64KB
                VramBank { data: vec![0; 16 * 1024], control: 0 },  // F: 16KB
                VramBank { data: vec![0; 16 * 1024], control: 0 },  // G: 16KB
                VramBank { data: vec![0; 32 * 1024], control: 0 },  // H: 32KB
                VramBank { data: vec![0; 16 * 1024], control: 0 },  // I: 16KB
            ],
            dropped_writes: 0,
            dropped_by_target: [0; 8]
        }
    }

    pub fn read_lcdc(&self, offset: u32) -> u8 {
        let bank_offsets = [
            (0, 128 * 1024),
            (128 * 1024, 128 * 1024),
            (256 * 1024, 128 * 1024),
            (384 * 1024, 128 * 1024),
            (512 * 1024, 64 * 1024),
            (576 * 1024, 16 * 1024),
            (592 * 1024, 16 * 1024),
            (608 * 1024, 32 * 1024),
            (640 * 1024, 16 * 1024),
        ];

        for i in 0..9 {
            let (start, size) = bank_offsets[i];
            if offset >= start && offset < start + size {
                let bank = &self.banks[i];
                // Enabled (bit 7) and MST == 0 (LCDC)
                if (bank.control & 0x80) != 0 && (bank.control & 0x07) == 0 {
                    return bank.data[(offset - start) as usize];
                }
            }
        }
        0
    }

    pub fn write_lcdc(&mut self, offset: u32, val: u8) {
        let bank_offsets = [
            (0, 128 * 1024),
            (128 * 1024, 128 * 1024),
            (256 * 1024, 128 * 1024),
            (384 * 1024, 128 * 1024),
            (512 * 1024, 64 * 1024),
            (576 * 1024, 16 * 1024),
            (592 * 1024, 16 * 1024),
            (608 * 1024, 32 * 1024),
            (640 * 1024, 16 * 1024),
        ];

        for i in 0..9 {
            let (start, size) = bank_offsets[i];
            if offset >= start && offset < start + size {
                let bank = &mut self.banks[i];
                if (bank.control & 0x80) != 0 && (bank.control & 0x07) == 0 {
                    bank.data[(offset - start) as usize] = val;
                }
            }
        }
    }

    pub fn read_target(&self, target: u8, offset: u32) -> u8 {
        for i in 0..9 {
            let bank = &self.banks[i];
            if (bank.control & 0x80) != 0 && (bank.control & 0x07) == target {
                let bank_offset = ((bank.control >> 3) & 3) as u32 * bank.data.len() as u32;
                if offset >= bank_offset && offset < bank_offset + bank.data.len() as u32 {
                    return bank.data[(offset - bank_offset) as usize];
                }
            }
        }
        0
    }

    pub fn write_target(&mut self, target: u8, offset: u32, val: u8) {
        let mut landed = false;
        for i in 0..9 {
            let bank = &mut self.banks[i];
            if (bank.control & 0x80) != 0 && (bank.control & 0x07) == target {
                let bank_offset = ((bank.control >> 3) & 3) as u32 * bank.data.len() as u32;
                if offset >= bank_offset && offset < bank_offset + bank.data.len() as u32 {
                    bank.data[(offset - bank_offset) as usize] = val;
                    landed = true;
                }
            }
        }
        // Census: a write that matched no enabled bank for its target is lost.
        // Hardware loses it too, so a nonzero count is not automatically a bug —
        // but a *scene whose graphics never appear* while its uploads all land
        // here says the mapping, not the game, is at fault.
        if !landed {
            self.dropped_writes = self.dropped_writes.wrapping_add(1);
            if (target as usize) < self.dropped_by_target.len() {
                self.dropped_by_target[target as usize] =
                    self.dropped_by_target[target as usize].wrapping_add(1);
            }
        }
    }

    /// Resolve an engine-A OBJ VRAM offset (the ARM9 0x06400000 window, 256KB)
    /// to (bank index, offset within bank). Needs per-bank decoding — MST 2
    /// means "OBJ-A" only on banks A/B/E/F/G; on C/D it means "mapped to the
    /// ARM7" and on I it means "OBJ-B", so a flat MST match would collide
    /// across address spaces (an engine-A tile upload would clobber engine B's
    /// bank I at the same window offset — exactly the sprite garble bug).
    /// Windows per GBATEK: A/B at OFS*0x20000; E fixed at 0 (64K);
    /// F/G at OFS.0*0x4000 + OFS.1*0x10000 (16K).
    fn obj_a_slot(&self, offset: u32) -> Option<(usize, usize)> {
        for i in 0..2 {
            let b = &self.banks[i];
            if b.control & 0x80 != 0 && b.control & 7 == 2 {
                let base = ((b.control >> 3) & 3) as u32 * 0x20000;
                if offset >= base && offset < base + 0x20000 {
                    return Some((i, (offset - base) as usize));
                }
            }
        }
        let e = &self.banks[4];
        if e.control & 0x80 != 0 && e.control & 7 == 2 && offset < 0x10000 {
            return Some((4, offset as usize));
        }
        for i in 5..7 {
            let b = &self.banks[i];
            if b.control & 0x80 != 0 && b.control & 7 == 2 {
                let ofs = ((b.control >> 3) & 3) as u32;
                let base = (ofs & 1) * 0x4000 + (ofs >> 1) * 0x10000;
                if offset >= base && offset < base + 0x4000 {
                    return Some((i, (offset - base) as usize));
                }
            }
        }
        None
    }

    /// Resolve an engine-B OBJ VRAM offset (the ARM9 0x06600000 window, 128KB):
    /// bank D with MST 4 (full 128K) or bank I with MST 2 (first 16K).
    fn obj_b_slot(&self, offset: u32) -> Option<(usize, usize)> {
        let d = &self.banks[3];
        if d.control & 0x80 != 0 && d.control & 7 == 4 && offset < 0x20000 {
            return Some((3, offset as usize));
        }
        let i = &self.banks[8];
        if i.control & 0x80 != 0 && i.control & 7 == 2 && offset < 0x4000 {
            return Some((8, offset as usize));
        }
        None
    }

    /// Resolve an engine-A BG VRAM offset (the ARM9 0x06000000 window, 512KB).
    ///
    /// Per-bank decoding for the same reason as [`Self::obj_a_slot`]: MST 1
    /// means engine-A BG on banks A-G but engine-**B** BG on H and I, so a flat
    /// "MST == 1" match pulls engine-B tile data into engine A wherever banks
    /// A-G leave a gap. The offset also has to come from each bank's own GBATEK
    /// window — the generic matcher scaled OFS by the bank's SIZE, which is
    /// right only for A-D and silently misplaces E (which ignores OFS entirely)
    /// and F/G (whose two OFS bits select 16K and 64K steps independently).
    ///
    /// Windows: A-D at OFS*0x20000 (128K each); E fixed at 0 (64K);
    /// F/G at OFS.0*0x4000 + OFS.1*0x10000 (16K each).
    fn bg_a_slot(&self, offset: u32) -> Option<(usize, usize)> {
        for i in 0..4 {
            let b = &self.banks[i];
            if b.control & 0x80 != 0 && b.control & 7 == 1 {
                let base = ((b.control >> 3) & 3) as u32 * 0x20000;
                if offset >= base && offset < base + 0x20000 {
                    return Some((i, (offset - base) as usize));
                }
            }
        }
        let e = &self.banks[4];
        if e.control & 0x80 != 0 && e.control & 7 == 1 && offset < 0x10000 {
            return Some((4, offset as usize));
        }
        for i in 5..7 {
            let b = &self.banks[i];
            if b.control & 0x80 != 0 && b.control & 7 == 1 {
                let ofs = ((b.control >> 3) & 3) as u32;
                let base = (ofs & 1) * 0x4000 + (ofs >> 1) * 0x10000;
                if offset >= base && offset < base + 0x4000 {
                    return Some((i, (offset - base) as usize));
                }
            }
        }
        None
    }

    pub fn read_bg_a(&self, offset: u32) -> u8 {
        self.bg_a_slot(offset).map_or(0, |(i, o)| self.banks[i].data[o])
    }

    pub fn write_bg_a(&mut self, offset: u32, val: u8) {
        if let Some((i, o)) = self.bg_a_slot(offset) {
            self.banks[i].data[o] = val;
        }
    }

    /// Resolve an engine-B BG VRAM offset (the ARM9 0x06200000 window, 128KB).
    ///
    /// Needs per-bank decoding for the same reason as [`Self::obj_a_slot`]:
    /// the MST value alone is ambiguous across banks. Engine-B BG lives on
    /// bank C at MST 4, on bank H at MST 1, and on bank I at MST 1 — but MST 1
    /// on banks A-G means engine-A BG, and MST 4 on bank D means engine-B OBJ.
    /// The generic matcher this replaces resolved engine-B BG as "any bank
    /// whose MST is 4", so it never saw H or I at all (leaving every engine-B
    /// tile fetch reading 0, i.e. a bottom screen collapsed to its backdrop)
    /// and collided with bank D's OBJ window.
    ///
    /// Windows per GBATEK: C fills the whole 128K; H covers the first 32K;
    /// I sits at 0x8000 and is 16K.
    fn bg_b_slot(&self, offset: u32) -> Option<(usize, usize)> {
        let c = &self.banks[2];
        if c.control & 0x80 != 0 && c.control & 7 == 4 && offset < 0x20000 {
            return Some((2, offset as usize));
        }
        let h = &self.banks[7];
        if h.control & 0x80 != 0 && h.control & 7 == 1 && offset < 0x8000 {
            return Some((7, offset as usize));
        }
        let i = &self.banks[8];
        if i.control & 0x80 != 0
            && i.control & 7 == 1
            && (0x8000..0xC000).contains(&offset)
        {
            return Some((8, (offset - 0x8000) as usize));
        }
        None
    }

    pub fn read_bg_b(&self, offset: u32) -> u8 {
        self.bg_b_slot(offset).map_or(0, |(i, o)| self.banks[i].data[o])
    }

    pub fn write_bg_b(&mut self, offset: u32, val: u8) {
        if let Some((i, o)) = self.bg_b_slot(offset) {
            self.banks[i].data[o] = val;
        }
    }

    pub fn read_obj_a(&self, offset: u32) -> u8 {
        self.obj_a_slot(offset).map_or(0, |(i, o)| self.banks[i].data[o])
    }

    pub fn write_obj_a(&mut self, offset: u32, val: u8) {
        if let Some((i, o)) = self.obj_a_slot(offset) {
            self.banks[i].data[o] = val;
        }
    }

    pub fn read_obj_b(&self, offset: u32) -> u8 {
        self.obj_b_slot(offset).map_or(0, |(i, o)| self.banks[i].data[o])
    }

    pub fn write_obj_b(&mut self, offset: u32, val: u8) {
        if let Some((i, o)) = self.obj_b_slot(offset) {
            self.banks[i].data[o] = val;
        }
    }

    /// Texture image slot read (GBATEK): banks A-D with MST 3 map into the
    /// 512KB texture space at slot OFS*0x20000. TEXIMAGE_PARAM addresses
    /// (offset<<3) resolve here at rasterization time.
    pub fn read_tex_image(&self, offset: u32) -> u8 {
        for i in 0..4 {
            let b = &self.banks[i];
            if b.control & 0x80 != 0 && b.control & 7 == 3 {
                let base = ((b.control >> 3) & 3) as u32 * 0x20000;
                if offset >= base && offset < base + 0x20000 {
                    return b.data[(offset - base) as usize];
                }
            }
        }
        0
    }

    /// Texture palette slot read (GBATEK): bank E with MST 3 covers palette
    /// slots 0-3 (64KB at 0); banks F/G with MST 3 cover one 16KB slot at
    /// OFS.0*0x4000 + OFS.1*0x10000.
    pub fn read_tex_pal(&self, offset: u32) -> u8 {
        let e = &self.banks[4];
        if e.control & 0x80 != 0 && e.control & 7 == 3 && offset < 0x10000 {
            return e.data[offset as usize];
        }
        for i in 5..7 {
            let b = &self.banks[i];
            if b.control & 0x80 != 0 && b.control & 7 == 3 {
                let ofs = ((b.control >> 3) & 3) as u32;
                let base = (ofs & 1) * 0x4000 + (ofs >> 1) * 0x10000;
                if offset >= base && offset < base + 0x4000 {
                    return b.data[(offset - base) as usize];
                }
            }
        }
        0
    }
}

/// One ARM9 tightly-coupled-memory window, flattened to the single question the
/// address routing asks: *is this address inside it?*
///
/// A **derived cache**, not state — a pure function of three CP15 values
/// ([`NdsMmu::set_cp15`] is its only writer, and it is rebuilt there rather than
/// invalidated, so there is no stale-flag to get wrong). Snapshots therefore
/// carry the CP15 registers and never this.
///
/// It exists because the previous form asked
/// `itcm_enabled() && in_itcm_range_arm9(addr)` — an enable-bit test, a base
/// mask, a `(control >> 1) & 0x1F` size field and a `checked_shl` — **twice**
/// (ITCM and DTCM) on every byte, halfword and word the ARM9 reads or writes,
/// including every instruction fetch. That is ~14 operations per access to
/// re-answer a question whose inputs change a handful of times per boot.
///
/// A disabled TCM is stored as `size == 0`, so the enable bit needs no separate
/// test: `wrapping_sub(base) < 0` is false for every address.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct TcmWindow {
    base: u32,
    /// Window size in bytes, or 0 when the TCM is disabled.
    size: u32,
}

// Refuted, do not re-attempt without new evidence: returning `&[u8; N]` from
// `contiguous_arm*` so `from_le_bytes` sees a statically-known length, on the
// theory that the `[b[0], b[1], b[2], b[3]]` form costs four bounds checks per
// access. `nds_arm9_synthetic_throughput_probe` prices a stream of `LDR`
// against a stream of `ADD` in one process, and the fast-path load costs
// **+1.4 ns** on top of a ~19 ns instruction — the same run had `main_ram ldr`
// come in *below* `main_ram alu`, i.e. inside the noise. There is nothing to
// win on this path; the cost is the interpreter body.

impl TcmWindow {
    /// TCM virtual region size from a TCM Region Register: `512 << N` bytes,
    /// where N = bits[5:1]. The physical TCM is mirrored within this window (the
    /// read/write paths fold via `% tcm.len()`). SoulSilver sets ITCM N=16
    /// (32 MB, base 0) so its 0x01xxxxxx code accesses hit the mirror.
    fn region_size(control_reg: u32) -> u32 {
        let n = (control_reg >> 1) & 0x1F;
        512u32.checked_shl(n).unwrap_or(u32::MAX)
    }

    /// Derive from the CP15 control register (`enable_bit` = 18 for ITCM, 16 for
    /// DTCM per ARM946) and the matching TCM Region Register.
    fn derive(control: u32, enable_bit: u32, region: u32) -> Self {
        if control & (1 << enable_bit) == 0 {
            return Self::default(); // size 0 => `contains` is always false
        }
        Self {
            base: region & 0xFFFF_F000,
            size: Self::region_size(region),
        }
    }

    /// Unsigned-wrap trick: an address below `base` wraps to a huge value and
    /// fails the compare, so one subtract and one compare cover both bounds.
    #[inline(always)]
    fn contains(self, addr: u32) -> bool {
        addr.wrapping_sub(self.base) < self.size
    }
}

/// Granularity of code-invalidation tracking: 4 KiB, one host page.
const CODE_PAGE_BITS: u32 = 12;
/// [`NdsMmu::code_block_pages`] mask bit: the page holds ARM9 compiled code.
pub const CODE_MASK_ARM9: u8 = 1;
/// [`NdsMmu::code_block_pages`] mask bit: the page holds ARM7 compiled code.
pub const CODE_MASK_ARM7: u8 = 2;
/// Main RAM, in code pages. It comes first in [`NdsMmu::code_versions`].
const CODE_PAGES_MAIN: usize = (4 * 1024 * 1024) >> CODE_PAGE_BITS;
/// ITCM, in code pages, following main RAM.
const CODE_PAGES_ITCM: usize = (32 * 1024) >> CODE_PAGE_BITS;
/// ARM7 private WRAM, in code pages, following ITCM. Tracked because the ARM7
/// recompiler executes from it (games relocate the sound driver there); the
/// storage is also visible through the 0x03000000 window in WRAMCNT mode 0,
/// so both write paths must dirty it. Shared WRAM proper stays untracked —
/// what an address there means depends on WRAMCNT — so blocks in it decline.
const CODE_PAGES_ARM7_WRAM: usize = (64 * 1024) >> CODE_PAGE_BITS;
/// The two 16 KiB HLE BIOS images, following ARM7 WRAM (ARM9 first). Both
/// cores execute their exception vectors from these constantly (the ARM9's
/// `0xffff0028` IRQ exit headed a refused-successor census), and the images
/// are written only at construction and via `set_cp15`'s handler-pointer
/// patch — which already runs `invalidate_all_code` — so the versions are
/// simply always current and the pages are free coverage.
const CODE_PAGES_BIOS9: usize = (16 * 1024) >> CODE_PAGE_BITS;
const CODE_PAGES_BIOS7: usize = (16 * 1024) >> CODE_PAGE_BITS;
/// Shared WRAM storage, following the BIOS images. Tracked because the boot
/// HLE pins WRAMCNT mode 3 and the **entire ARM7 static executes from the
/// 0x037Fxxxx window onto this storage** — the decline census measured the
/// four hottest ARM7 blocks (892k filter hits per 60 ticks) all here.
/// Storage-indexed like main RAM, so the WRAMCNT mode decides only the
/// window→storage mapping in `code_page_arm7`; a mode change remaps what an
/// address means, so the WRAMCNT write invalidates wholesale like `set_cp15`.
const CODE_PAGES_SHARED: usize = (256 * 1024) >> CODE_PAGE_BITS;
/// First shared-WRAM page index.
const CODE_BASE_SHARED: usize = CODE_PAGES_MAIN
    + CODE_PAGES_ITCM
    + CODE_PAGES_ARM7_WRAM
    + CODE_PAGES_BIOS9
    + CODE_PAGES_BIOS7;
/// Total tracked code pages. DTCM is deliberately absent: it is the *data* TCM
/// and is never executed, so tracking it would only cost writes.
const CODE_PAGES: usize = CODE_BASE_SHARED + CODE_PAGES_SHARED;

pub struct NdsMmu {
    pub main_ram: Vec<u8>,         // 4 MB
    pub shared_wram: Vec<u8>,      // 256 KB
    pub arm7_wram: Vec<u8>,        // 64 KB
    pub itcm: Vec<u8>,             // 32 KB
    pub dtcm: Vec<u8>,             // 16 KB
    /// Version counter per 4 KiB page of the memory ARM9 code can execute from.
    ///
    /// Exists for the block recompiler, and it is the mechanism that keeps a
    /// stale compiled block from being *silent* corruption: DS games DMA and
    /// decompress code into RAM, so "the bytes at this address still say what
    /// they said when I compiled them" has to be checkable. A compiled block
    /// records the versions of the pages it spans and is discarded when they
    /// move. See [`Self::code_page_arm9`].
    ///
    /// Indexed by **storage offset**, not guest address — main RAM first, then
    /// ITCM — so address mirroring and CP15 TCM relocation need no special case.
    pub code_versions: Vec<u32>,
    /// Whether stores maintain [`Self::code_versions`].
    ///
    /// Off by default so the write path — the hottest in the MMU — pays one
    /// never-taken, perfectly-predicted branch when no recompiler is running.
    pub code_watch: bool,
    /// Pages of [`Self::code_versions`] that hold at least one **compiled
    /// block**, as a per-core bitmask ([`CODE_MASK_ARM9`] | [`CODE_MASK_ARM7`])
    /// — set by the owning recompiler when it caches one, never cleared
    /// (stale-high bits cost an extra chain break, not correctness).
    ///
    /// Exists so the epochs move only for stores that could invalidate
    /// compiled code, and per core: the ARM7's sound driver stores into the
    /// pages its own code lives on every tick, and a single global epoch let
    /// those stores tear down every **ARM9** link as well — measured as
    /// 17,783 ARM9 flush passes per 60 ticks against ~0 standalone.
    pub code_block_pages: Vec<u8>,
    /// Moves whenever a store (CPU, DMA, cart loader or boot HLE) lands on a
    /// page in [`Self::code_block_pages`], and on [`Self::invalidate_all_code`].
    ///
    /// The recompiler's successor links are only valid while this stands still:
    /// the store thunks compare it against the value captured at block entry to
    /// break a chain, and `Arm9Jit::try_step` compares it against its own
    /// snapshot to tear every link down before the next chain starts. Scheduling
    /// state, not emulated state: absent from the snapshot, like `ipc_yield`.
    pub code_write_epoch: u64,
    /// The ARM7 recompiler's twin of [`Self::code_write_epoch`]: moves for
    /// stores landing on pages whose mask carries [`CODE_MASK_ARM7`]. Split so
    /// each core's links only pay for stores that can invalidate *its* code.
    pub code_write_epoch7: u64,
    pub vram: VramManager,
    pub wram_control: u8,          // WRAMCNT register
    pub ipc: IpcState,
    /// ARM9 CP15 copy the address routing reads. **Private**, because
    /// [`Self::itcm_win`] / [`Self::dtcm_win`] are derived from it and a write
    /// that bypassed [`Self::set_cp15`] would leave them stale — which is a
    /// silent *correctness* fault, not a slow path. Read it with
    /// [`Self::cp15`].
    arm9_cp15: Cp15Registers,
    /// Derived ITCM window; see [`TcmWindow`]. Maintained by [`Self::set_cp15`].
    itcm_win: TcmWindow,
    /// Derived DTCM window; see [`TcmWindow`]. Maintained by [`Self::set_cp15`].
    dtcm_win: TcmWindow,
    /// APU cycles accepted by [`Self::tick_apu`] but not yet rendered.
    ///
    /// Scheduling state, not emulated state, and it is always 0 across a
    /// `tick()` boundary because the run loop calls [`Self::flush_apu`] before
    /// returning — which is why it is absent from the snapshot.
    apu_pending: u32,
    /// A sound register was written, so the deferred window in
    /// [`Self::apu_pending`] must be rendered before the new value takes
    /// effect. Set by [`Self::apu_write_byte`], cleared by
    /// [`Self::flush_apu`]. Transient within a tick, like `apu_pending`.
    apu_mix_dirty: bool,
    /// A core just wrote IPCSYNC; the run loop should hand its partner the bus
    /// at the next instruction boundary instead of finishing the slice.
    ///
    /// The boot handshake is a tight IPCSYNC ping-pong where each side polls
    /// with a short timeout, so a slice wider than that timeout stalls it — the
    /// reason [`crate::emulator`]'s interleave constant was pinned at 64 cycles
    /// and the ~19% a wider slice is worth stayed on the table. Yielding on the
    /// ping keeps the two cores in lock-step *through the handshake* at any
    /// slice width, because the partner runs the moment it is pinged rather
    /// than up to a slice later.
    ///
    /// Consumed by whichever core's `run` is executing, so it never leaks
    /// across a tick boundary. Scheduling state, not emulated state: absent
    /// from the snapshot.
    pub ipc_yield: bool,
    pub spi: SpiController,
    /// Cartridge backup (save) chip on the AUXSPI bus. A sibling of `spi`, not
    /// a member of it: `reset()` recreates `spi` on every ROM load and reset,
    /// and the player's save must survive both. See [`crate::nds::backup`].
    pub backup: crate::nds::backup::NdsBackup,
    pub buttons: ButtonState,

    pub arm9_ie: u32,
    pub arm9_if: u32,
    pub arm9_ime: u32,

    pub arm7_ie: u32,
    pub arm7_if: u32,
    pub arm7_ime: u32,

    pub arm9_io: Vec<u8>,
    pub arm7_io: Vec<u8>,
    pub rom: Vec<u8>,
    /// NDS Gamecard (cartridge) block-transfer cursor. See [`GamecardState`].
    pub gamecard: GamecardState,
    /// ARM9 DMA internal destination cursors. Hardware latches DAD into an
    /// internal register on the enable 0->1 edge and keeps advancing it across
    /// repeat triggers AND across Gamecard blocks — multi-block streams (FS
    /// reads of the FAT/FNT) rely on this. Reloading from DAD per block would
    /// stack every page onto the first 512 bytes.
    pub(crate) dma9_internal_dst: [u32; 4],

    pub palette_ram: [u8; 4096],
    pub oam: [u8; 2048],
    pub has_3d_activity: bool,
    /// ARM9 timer block (TM0..TM3 at 0x04000100-0x0F). The NitroSDK OS tick —
    /// the RTOS alarm/scheduler timebase every game boots on — runs on TM0
    /// (prescaler F/64, IRQ); without these the ARM9 sleeps forever on an alarm.
    pub timers9: NdsTimers,
    /// ARM7 timer block (same register layout on its own bus).
    pub timers7: NdsTimers,
    /// Bit-banged RTC behind ARM7 pin register 0x04000138.
    pub rtc: NdsRtc,
    /// 16-channel sound mixer behind 0x040004xx (ARM7 side). See `nds::apu`.
    pub apu: crate::nds::apu::NdsApu,
    /// Geometry command stream decoder (U27): counts + traces every GX
    /// command arriving via the FIFO window or the direct ports.
    pub gx: crate::nds::gx::GxDecoder,
    /// U27 probe watch: while armed, log ARM7 RAM/WRAM halfword+word writes
    /// whose value matches the injected tap's TSC magic numbers (ADC X 2032 /
    /// Y 3322 / pen-up rail 0xFFF) — locates the shared TP sample struct
    /// without knowing its layout. ponytail: byte writes not watched; the SDK
    /// stores samples as u16 fields.
    pub tp_watch_on: bool,
    pub tp_watch_log: Vec<(u32, u32)>,
    /// U27 probe watch: while armed, log IPC FIFO sends from both cores
    /// (direction, value) — the TP sampler works entirely in ARM7-private
    /// WRAM, so whatever the ARM9 UI learns about the pen must cross PXI.
    pub fifo_log_on: bool,
    pub fifo_log: Vec<(u8, u32)>,
    /// Unconditional tag-7 (TP auto-sampling) delivery counters: requests the
    /// ARM9 sent, requests the ARM7 actually popped, sends dropped on a full
    /// FIFO. Together they tell whether the touch pipeline breaks at
    /// delivery or inside the ARM7's TP module.
    pub tag7_sent: u32,
    pub tag7_rx7: u32,
    pub tag7_dropped_full: u32,
    /// Data field (bits 6-31) of the most recent tag-7 send — the sample
    /// ring address the ARM9 asked the ARM7 to fill this frame.
    pub last_tag7_data: u32,
    /// U27g: while armed, count ARM9 instruction-level reads of the four
    /// touch-state words ([0]=0x021E36B8 sample, [1]=0x021E36BC flags,
    /// [2]=0x027FFFA8, [3]=0x027FFFAC hot page) — compared across a WORKING
    /// menu tap and a DEAD info-screen tap, this says whether the UI even
    /// looks at the delivered data.
    pub tp_read_watch_on: bool,
    pub tp_slot_reads: [u32; 6],
    /// U27l: log every ARM9 write into the TP calibrate-param struct
    /// (0x021E36CC-0x021E36E7) — the probe drains it per frame, so the
    /// writer's values AND timing identify which data source fed
    /// TP_SetCalibrateParam the zeros.
    pub calib_watch_on: bool,
    pub calib_write_log: Vec<(u32, u32)>,
    /// ARM9 hardware maths block (GBATEK "Maths"): divider at 0x04000280
    /// (DIVCNT / NUMER / DENOM / RESULT / REM) and square root at
    /// 0x040002B0. The SDK routes fixed-point division through the divider
    /// — TP_SetCalibrateParam computes 1<<28 / dot there, and the TP dot
    /// factors themselves are divided the same way, so an unimplemented
    /// divider zeroed the whole touch calibration (U28). Results are
    /// computed on read; busy never reads set.
    pub div_cnt: u16,
    pub div_numer: i64,
    pub div_denom: i64,
    pub sqrt_cnt: u16,
    pub sqrt_param: u64,
    /// U27m: while calib_watch_on, log AUXSPI data-port traffic (dir 0=write
    /// with the value, 1=read) and ARM9 reads of the RAM user-settings copy
    /// (first 16 exact addresses) — names the calibration source at boot.
    pub aux_log: Vec<(u8, u8)>,
    pub settings_read_log: Vec<u32>,
    /// W0.1 evidence: unconditional, bounded log of every ARM7 **write** to the
    /// AUXSPI bus — `(reg_low_byte, value)` for 0xA0/0xA1 (AUXSPICNT) and 0xA2
    /// (AUXSPIDATA). `aux_log` above cannot see this traffic: it is ARM9-only
    /// and gated on `calib_watch_on`, while the cartridge backup chip is driven
    /// entirely by the ARM7. Writes alone answer the gating question (is the
    /// save driver reached at all, which opcodes, which addresses) because
    /// every command byte and every address byte is a write; responses are
    /// meaningless until a chip exists to produce them.
    pub aux_bus_log: Vec<(u8, u8)>,
    /// Every byte clocked through AUXSPIDATA as `(written, returned)`. The
    /// write log alone cannot show why a driver gave up: what it acted on is
    /// the byte the chip drove back.
    pub aux_io_log: Vec<(u8, u8)>,
    /// Entries the bounded log refused, so a truncated capture can never be
    /// mistaken for a quiet bus.
    pub aux_bus_dropped: u32,
    /// Which core opened the AUXSPI transaction in flight (`true` = ARM9), or
    /// `None` between transactions. Diagnostic only.
    pub(crate) aux_txn_core: Option<bool>,
    /// ARM7 writes to the gamecard block registers (ROMCTRL 0x040001A4 and the
    /// command buffer), and how many of those set the block-start bit. The ARM7
    /// gamecard path is unwired, so if the game ever starts a block from that
    /// core its data-ready spin cannot exit — but only a nonzero count here
    /// makes that reachable rather than theoretical.
    pub aux7_card_writes: u32,
    pub aux7_block_starts: u32,
    /// ARM9 writes that reached the VRAM (0x06xxxxxx) and OAM (0x07xxxxxx)
    /// windows. Per-frame graphics uploads land here, so a gameplay frame with
    /// zero of either is not animating anything.
    pub vram_writes: u64,
    pub oam_writes: u64,
    /// IRQs actually taken per core. A game whose VBlank handler never runs
    /// performs none of the work it defers there — on this title that includes
    /// flushing queued VRAM transfers, so "no IRQs" and "nothing animates" are
    /// the same observation seen from two places.
    pub arm9_irqs_taken: u64,
    pub arm7_irqs_taken: u64,
    /// Cycles each core spent in the low-power halt (one per halted `step`).
    ///
    /// Evidence, not hardware state: the run loop hands each core a fixed
    /// cycle budget per frame, so "did the core finish its frame work" is only
    /// answerable as *how much of that budget it idled away*. A core that never
    /// halts is saturated — it is still working when the budget runs out, and
    /// whatever it defers (the ARM7's sound driver, the ARM9's VRAM streaming)
    /// slips to the next frame.
    pub arm9_halt_cycles: u64,
    pub arm7_halt_cycles: u64,
    /// Bus cycles the ARM7 was actually given. The ARM9's equivalent is
    /// `Emulator::cpu_cycles`; the ARM7 is slaved to the run loop's slices and
    /// had no counter, so its halt fraction was underivable.
    pub arm7_cycles_run: u64,
    /// Wall-clock nanoseconds spent interpreting both cores, accumulated only
    /// while [`Self::prof_cpu_on`] is set.
    ///
    /// Companion to `NdsPpu::prof_render_ns` and `Gx3d::prof_raster_ns`: with
    /// those three, every part of a tick is attributed instead of landing in an
    /// unexplained remainder.
    pub prof_cpu_ns: u64,
    /// The ARM9's share of [`Self::prof_cpu_ns`], same gate.
    ///
    /// Without this the split between the cores can only be *assumed* — the
    /// obvious assumption being that cost follows instruction count, which puts
    /// the ARM7 at 11.9%. That assumption decides whether an ARM9-only
    /// recompiler can reach the 5x target at all, so it is measured instead:
    /// the ARM7 idles ~80% of its budget, and an idle core retires instructions
    /// at a completely different price.
    pub prof_cpu9_ns: u64,
    /// Gate for [`Self::prof_cpu_ns`], off by default.
    ///
    /// Unlike its two companions, this counter costs two clock reads per
    /// *run-loop slice* rather than one per frame: ~8750 slices/frame at the
    /// 64-cycle interleave, so ~17500 `Instant::now()` calls a frame. That is a
    /// measurement charged to every player, in the one loop where throughput is
    /// the open defect — so the probes that read it turn it on, and nothing
    /// else pays. Not carried in snapshots (see the `Snap` impl): it is a
    /// property of the measuring session, not of the emulated machine.
    pub prof_cpu_on: bool,
    /// Whether the 3D rasterizer should produce pixels at the next
    /// SWAP_BUFFERS. Set by the run loop each slice; see `Gx3d::swap_buffers`.
    ///
    /// Lives here because SWAP_BUFFERS arrives as an ordinary CPU store deep
    /// inside `write_word_arm9`, which has no channel back to the run loop.
    /// Defaults to `true` so every construction path rasterizes: only
    /// fast-forward ever clears it. Not snapshotted — a property of the
    /// measuring session's pacing, not of the emulated machine.
    pub gx_raster_enabled: bool,
    /// VCOUNT at the most recent SWAP_BUFFERS store; see `flush_gx_swap`.
    /// Diagnostic only, not snapshot state.
    pub gx_swap_vcount: u16,
    /// SWAP_BUFFERS stores counted, and how many landed at VCOUNT < 192.
    ///
    /// The second number is the one that matters: it is exactly how many frames
    /// the old single-buffer rasterizer could tear on, because those are the
    /// stores that rewrote the framebuffer while the PPU was scanning it out.
    /// A run where it stays 0 means this game happens to swap inside VBlank and
    /// never tore; anything above 0 is the reported moving seam, counted.
    pub gx_swaps_total: u32,
    pub gx_swaps_in_visible: u32,
    /// Address of the ARM9 instruction currently executing, stamped by
    /// [`crate::nds::cpu::Arm9Cpu::step`].
    ///
    /// This is the missing half of every "who wrote that?" hunt in this
    /// emulator: the write paths live on the MMU, which otherwise has no idea
    /// which instruction reached it, and the alternative is a hand-rolled
    /// stepping loop that duplicates `tick`.
    pub arm9_exec_pc: u32,
    /// ARM9 `lr` at the same moment, so a hit inside a shared helper still names
    /// its caller.
    pub arm9_exec_lr: u32,
    /// DMA transfers (and words) whose destination was the GXFIFO window — the
    /// display lists the SDK sends by DMA rather than CPU copy.
    pub dma_gxfifo_transfers: u64,
    pub dma_gxfifo_words: u64,
    /// Arm the geometry-command watch with a TEXIMAGE_PARAM texture base (the
    /// low 16 bits of the register, i.e. the VRAM offset / 8). Every write that
    /// leaves that texture selected records the ARM9 PC that made it, naming the
    /// game function that draws with it.
    pub gx_tex_watch: Option<u32>,
    /// Watch *every* texture selection instead of one, recording each distinct
    /// `(caller, texture)` pair once. That is a census of the scene's draw call
    /// sites: an object that is never drawn has no entry at all, which separates
    /// "the game skipped it" from "it drew it with something that renders blank".
    pub gx_tex_watch_all: bool,
    /// `(pc, lr, teximage)` per recorded hit.
    pub gx_tex_watch_pcs: Vec<(u32, u32, u32)>,
    /// `(pc, lr)` of the code that committed a zero MTX_SCALE — the command that
    /// collapses an object's transform to a point. Always on: it costs one
    /// integer compare per geometry write and answers the question that matters.
    pub gx_zero_scale_pcs: Vec<(u32, u32)>,
    gx_zero_scale_seen: u32,
    /// Half-open ARM9 address range to trap writes in. Disarmed by default; one
    /// `Option` compare per main-RAM byte write when off.
    ///
    /// The generic form of the bespoke `calib_watch`: "which code wrote this
    /// word?" is the question every one of these investigations ends at.
    pub ram_write_watch: Option<(u32, u32)>,
    /// `(address, value, pc, lr)` per trapped write, bounded.
    pub ram_write_pcs: Vec<(u32, u8, u32, u32)>,
    /// W1 evidence: DMA census indexed `[core][start timing]`, core 0 = ARM9.
    /// `armed` counts enable 0->1 edges, `fired` counts transfers that actually
    /// ran. A timing with `armed > 0 && fired == 0` is a channel the game is
    /// waiting on that can never complete — NitroSDK's `MI_WaitDma` spins on the
    /// enable bit, so that is a hung task rather than a dropped frame.
    pub dma_armed: [[u32; 8]; 2],
    pub dma_fired: [[u32; 8]; 2],
    pub dma_units: [[u64; 8]; 2],
    /// Largest `count` field ever requested per core, to settle whether the
    /// ARM7's 14-bit mask on channels 0-2 is ever actually hit.
    pub dma_max_count: [u32; 2],
    /// PXI FIFO tag 0x0B is the backup-chip command channel: the ARM9 sends an
    /// operation id and the ARM7's card task dispatches it (6 = READ,
    /// 7 = WREN+page-write, 9 = READ+verify, 11 = sector erase, 13 = read
    /// status). Boot issues two 0x23000-byte reads before the first frame, so a
    /// healthy run shows thousands of op-6 requests; a histogram that contains
    /// only status reads localises the failure to the request side rather than
    /// to the chip model.
    pub tag11_9to7: [u32; 16],
    pub tag11_9to7_wide: u32,
    pub tag11_7to9: u32,
    pub tag11_head: Vec<(u8, u32)>,
    /// Pointer to the 0x60-byte shared argument block, taken from the second
    /// word of tag-0x0B command 0.
    pub tag11_argptr: u32,
    pub(crate) tag11_expect_ptr: bool,
    /// Snapshot of that block at each command request: `(op, bytes)`. The ARM7
    /// task refuses any command whose bit is clear in `[0x58]`, and takes the
    /// transfer's source, destination and length from `[0x0C]`, `[0x10]` and
    /// `[0x14]` — so this shows whether a rejected READ was rejected for a
    /// reason we produced.
    pub tag11_args: Vec<(u32, [u8; 0x60])>,
    /// Times a write from one core dropped chip-select while the OTHER core had
    /// a command in flight. AUXSPICNT is one physical register shared by both
    /// cores, and the ARM9 drives it for gamecard ROM streaming (bit 14 IRQ
    /// enable) while the ARM7 drives it for the backup chip — so without slot
    /// ownership arbitration the two tear each other's transactions apart.
    pub aux_cross_deselects: u32,
    /// ARM7 exception-vector region (0x00000000-0x00003FFF). We ship no real
    /// BIOS; this holds a minimal HLE IRQ handler at 0x18 so IRQs dispatch to the
    /// game's handler instead of executing an empty region and derailing.
    pub arm7_bios: Vec<u8>,
    /// ARM9 exception-vector region, same purpose as `arm7_bios` but its handler
    /// reads the game IRQ handler pointer from DTCM+0x3FFC (the ARM9 convention).
    pub arm9_bios: Vec<u8>,
}

/// NDS Real-Time Clock (S-35199 behind the ARM7 pin register 0x04000138).
///
/// Bit-banged serial bus: bit0 = SIO data, bit1 = SCK, bit2 = /CS(select),
/// bits 4-6 = pin directions (bit4: 1 = SIO driven by CPU). A transaction:
/// CS rises -> command byte clocked LSB-first on SCK rising edges -> for
/// reads the CPU flips SIO to input and clocks response bits out. SoulSilver
/// polls date/time at boot to seed its RNG and game clock; with no chip
/// answering, its video-init loop retried forever. The reported moment is a
/// CONSTANT (2024-01-01 Mon 12:00:00) so runs stay deterministic for tests
/// and probes. ponytail: wire std::time in if wall-clock time ever matters.
#[derive(Clone, Debug, Default)]
pub struct NdsRtc {
    prev_pins: u8,
    bit_count: u8,
    shift: u8,
    cmd: Option<u8>,
    response: Vec<u8>,
    out_pos: usize,
}

impl NdsRtc {
    /// Latch a pin write; response bits surface via `read`.
    fn write(&mut self, val: u8) {
        let rising_sck = (val & 0x02) != 0 && (self.prev_pins & 0x02) == 0;
        let cs = (val & 0x04) != 0;
        if !cs {
            // Deselect resets the transaction framing.
            self.bit_count = 0;
            self.shift = 0;
            self.cmd = None;
            self.response.clear();
            self.out_pos = 0;
        } else if rising_sck && (val & 0x10) != 0 {
            // CPU driving SIO: shift the data bit in, LSB first.
            self.shift |= (val & 1) << self.bit_count;
            self.bit_count += 1;
            if self.bit_count == 8 {
                self.consume_byte();
            }
        } else if rising_sck && self.cmd.is_some() {
            // CPU reading SIO: advance to the next response bit.
            self.out_pos += 1;
        }
        self.prev_pins = val;
    }

    /// Pin state as seen by the CPU: written pins, with the data line
    /// replaced by the active response bit while SIO is an input.
    fn read(&self) -> u8 {
        if (self.prev_pins & 0x10) == 0 && self.cmd.is_some() {
            let byte = self.response.get(self.out_pos / 8).copied().unwrap_or(0);
            let bit = (byte >> (self.out_pos % 8)) & 1;
            (self.prev_pins & !1) | bit
        } else {
            self.prev_pins
        }
    }

    fn consume_byte(&mut self) {
        let mut b = self.shift;
        self.bit_count = 0;
        self.shift = 0;
        if self.cmd.is_none() {
            // Command byte 0110_ccc_r; the chip accepts either bit order —
            // normalise when the fixed 0110 pattern isn't in the high nibble.
            if (b >> 4) != 0b0110 {
                b = b.reverse_bits();
            }
            let cmd = (b >> 1) & 7;
            let read = (b & 1) != 0;
            self.cmd = Some(cmd);
            self.out_pos = 0;
            self.response = if read {
                match cmd {
                    // Status 1: 24h mode, no power-loss flag.
                    0 => vec![0x02],
                    // Date+time, BCD: yy mm dd dow hh mi ss.
                    2 => vec![0x24, 0x01, 0x01, 0x01, 0x12, 0x00, 0x00],
                    // Time only: hh mi ss.
                    3 => vec![0x12, 0x00, 0x00],
                    _ => vec![0x00],
                }
            } else {
                Vec::new() // write commands: payload accepted and ignored
            };
        }
        // Payload bytes of write commands are consumed with no effect.
    }
}

/// One CPU's four 16-bit timers (GBATEK "NDS Timers", same model as the GBA's).
///
/// Register layout per channel `ch` at `0x04000100 + ch*4`: CNT_L (+0/+1) reads
/// the live counter and writes the RELOAD latch; CNT_H (+2) is control —
/// bits 0-1 prescaler (F/1, F/64, F/256, F/1024 of the 33.55 MHz bus), bit 2
/// count-up (advance on the previous channel's overflow instead of the clock),
/// bit 6 IRQ on overflow (IF bit 3+ch), bit 7 start. On overflow the counter
/// reloads from the latch. Ticking is batched per emulation slice (whole
/// elapsed-cycle deltas, no per-cycle loop).
#[derive(Clone, Copy, Default)]
pub struct NdsTimers {
    pub reload: [u16; 4],
    pub counter: [u16; 4],
    pub control: [u16; 4],
    /// Prescaler remainder (bus cycles not yet converted into counter ticks).
    acc: [u32; 4],
}

impl NdsTimers {
    /// Byte read within the block (offset 0x0-0xF from 0x04000100).
    fn read_byte(&self, off: u32) -> u8 {
        let ch = (off / 4) as usize;
        match off % 4 {
            0 => self.counter[ch] as u8,
            1 => (self.counter[ch] >> 8) as u8,
            2 => self.control[ch] as u8,
            _ => 0,
        }
    }

    /// Byte write within the block. Writing CNT_L sets the reload latch (the
    /// counter itself only changes on start/overflow); a 0->1 edge of the start
    /// bit loads the counter from the latch and clears the prescaler remainder.
    fn write_byte(&mut self, off: u32, val: u8) {
        let ch = (off / 4) as usize;
        match off % 4 {
            0 => self.reload[ch] = (self.reload[ch] & 0xFF00) | val as u16,
            1 => self.reload[ch] = (self.reload[ch] & 0x00FF) | ((val as u16) << 8),
            2 => {
                let old = self.control[ch];
                let new = val as u16 & 0xC7; // prescaler, count-up, IRQ, start
                if (new & (1 << 7)) != 0 && (old & (1 << 7)) == 0 {
                    self.counter[ch] = self.reload[ch];
                    self.acc[ch] = 0;
                }
                self.control[ch] = new;
            }
            _ => {}
        }
    }

    /// Advance channel `ch` by `ticks` counter increments, chaining overflows
    /// into count-up channels and accumulating overflow IRQs into `iff`.
    fn advance(&mut self, mut ch: usize, mut ticks: u64, iff: &mut u32) {
        while ticks > 0 {
            let until_overflow = 0x1_0000 - self.counter[ch] as u64;
            if ticks < until_overflow {
                self.counter[ch] = (self.counter[ch] as u64 + ticks) as u16;
                return;
            }
            let period = 0x1_0000 - self.reload[ch] as u64; // >= 1
            let overflows = 1 + (ticks - until_overflow) / period;
            self.counter[ch] = (self.reload[ch] as u64 + (ticks - until_overflow) % period) as u16;
            if (self.control[ch] & (1 << 6)) != 0 {
                *iff |= 1 << (3 + ch);
            }
            let next = ch + 1;
            if next < 4
                && (self.control[next] & (1 << 7)) != 0
                && (self.control[next] & (1 << 2)) != 0
            {
                ch = next;
                ticks = overflows;
                continue;
            }
            return;
        }
    }

    /// Feed `cycles` bus cycles to every running clock-driven channel
    /// (count-up channels advance only via `advance`'s cascade).
    fn tick(&mut self, cycles: u32, iff: &mut u32) {
        for ch in 0..4 {
            let c = self.control[ch];
            if (c & (1 << 7)) == 0 || (c & (1 << 2)) != 0 {
                continue;
            }
            let prescale: u32 = match c & 3 {
                0 => 1,
                1 => 64,
                2 => 256,
                _ => 1024,
            };
            let total = self.acc[ch] + cycles;
            self.acc[ch] = total % prescale;
            let ticks = (total / prescale) as u64;
            if ticks > 0 {
                self.advance(ch, ticks, iff);
            }
        }
    }
}

/// Minimal NDS Gamecard (cartridge) block-transfer state for HLE ROM reads.
///
/// Protocol (main/KEY2 mode, which is where the boot ROM leaves the cart once the
/// secure area is past): the CPU writes an 8-byte command to `0x040001A8`, then
/// writes `ROMCTRL` (`0x040001A4`) with bit 31 (block start) set; the block size
/// comes from ROMCTRL bits 26-24. It then streams the block word-by-word from the
/// data port (`0x04100010`), polling ROMCTRL bit 23 (word ready) and bit 31 (busy).
/// We service the transfer synchronously: [`NdsMmu::start_gamecard_block`] latches
/// the source ROM offset and length and raises word-ready; each
/// [`NdsMmu::gamecard_read_data`] returns the next little-endian word and, when the
/// block drains, clears busy (and raises the transfer-complete IRQ if enabled).
#[derive(Default)]
pub struct GamecardState {
    /// Byte offset into `rom` of the next word to deliver (0xB7 main read).
    pub(crate) src: u32,
    /// Bytes remaining in the current block (0 = idle / complete).
    pub(crate) bytes_left: u32,
    /// `None` = stream real ROM data from `src` (command 0xB7). `Some(w)` = deliver
    /// the constant word `w` for every word of the block — used by non-data commands
    /// (chip-ID delivers the ID; dummy/unknown deliver the `0xFFFFFFFF` idle bus) so
    /// the busy poll still completes without leaking ROM bytes.
    fill: Option<u32>,
}

impl NdsMmu {
    pub fn new() -> Self {
        Self {
            main_ram: vec![0; 4 * 1024 * 1024],
            shared_wram: vec![0; 256 * 1024],
            arm7_wram: vec![0; 64 * 1024],
            itcm: vec![0; 32 * 1024],
            dtcm: vec![0; 16 * 1024],
            code_versions: vec![0; CODE_PAGES],
            code_watch: false,
            code_block_pages: vec![0; CODE_PAGES],
            code_write_epoch: 0,
            code_write_epoch7: 0,
            vram: VramManager::new(),
            wram_control: 0,
            ipc: IpcState::new(),
            arm9_cp15: Cp15Registers::default(),
            // Consistent with the default CP15 by construction: no enable bit
            // set means both windows are empty.
            itcm_win: TcmWindow::default(),
            dtcm_win: TcmWindow::default(),
            apu_pending: 0,
            apu_mix_dirty: false,
            ipc_yield: false,
            spi: SpiController::new(),
            backup: crate::nds::backup::NdsBackup::default(),
            buttons: ButtonState {
                up: false,
                down: false,
                left: false,
                right: false,
                a: false,
                b: false,
                start: false,
                select: false,
                l: false,
                r: false,
                x: false,
                y: false,
                nds_touch_x: 0,
                nds_touch_y: 0,
                nds_touch_pressed: false,
            },

            arm9_ie: 0,
            arm9_if: 0,
            arm9_ime: 0,
            timers9: NdsTimers::default(),
            timers7: NdsTimers::default(),
            rtc: NdsRtc::default(),
            apu: crate::nds::apu::NdsApu::new(),
            gx: crate::nds::gx::GxDecoder::new(),
            tp_watch_on: false,
            tp_watch_log: Vec::new(),
            fifo_log_on: false,
            fifo_log: Vec::new(),
            tag7_sent: 0,
            tag7_rx7: 0,
            tag7_dropped_full: 0,
            last_tag7_data: 0,
            tp_read_watch_on: false,
            tp_slot_reads: [0; 6],
            calib_watch_on: false,
            calib_write_log: Vec::new(),
            aux_log: Vec::new(),
            aux_bus_log: Vec::new(),
            aux_io_log: Vec::new(),
            aux_bus_dropped: 0,
            aux_txn_core: None,
            aux_cross_deselects: 0,
            aux7_card_writes: 0,
            aux7_block_starts: 0,
            vram_writes: 0,
            oam_writes: 0,
            arm9_irqs_taken: 0,
            arm7_irqs_taken: 0,
            arm9_halt_cycles: 0,
            arm7_halt_cycles: 0,
            arm7_cycles_run: 0,
            prof_cpu_ns: 0,
            prof_cpu9_ns: 0,
            prof_cpu_on: false,
            gx_raster_enabled: true,
            gx_swap_vcount: 0,
            gx_swaps_total: 0,
            gx_swaps_in_visible: 0,
            arm9_exec_pc: 0,
            arm9_exec_lr: 0,
            dma_gxfifo_transfers: 0,
            dma_gxfifo_words: 0,
            gx_tex_watch: None,
            gx_tex_watch_all: false,
            gx_tex_watch_pcs: Vec::new(),
            gx_zero_scale_pcs: Vec::new(),
            gx_zero_scale_seen: 0,
            ram_write_watch: None,
            ram_write_pcs: Vec::new(),
            dma_armed: [[0; 8]; 2],
            dma_fired: [[0; 8]; 2],
            dma_units: [[0; 8]; 2],
            dma_max_count: [0; 2],
            tag11_9to7: [0; 16],
            tag11_9to7_wide: 0,
            tag11_7to9: 0,
            tag11_head: Vec::new(),
            tag11_argptr: 0,
            tag11_expect_ptr: false,
            tag11_args: Vec::new(),
            settings_read_log: Vec::new(),
            div_cnt: 0,
            div_numer: 0,
            div_denom: 0,
            sqrt_cnt: 0,
            sqrt_param: 0,
            dma9_internal_dst: [0; 4],

            arm7_ie: 0,
            arm7_if: 0,
            arm7_ime: 0,

            arm9_io: vec![0; 1024 * 1024],
            arm7_io: vec![0; 1024 * 1024],
            rom: Vec::new(),
            gamecard: GamecardState::default(),

            palette_ram: [0; 4096],
            oam: [0; 2048],
            has_3d_activity: false,
            arm7_bios: Self::arm7_bios_with_irq_handler(),
            arm9_bios: Self::arm9_bios_with_irq_handler(),
        }
    }

    /// ARM9 exception-vector region with the BIOS IRQ handler at 0x18. Like the
    /// ARM7 handler but reads the game IRQ handler pointer from DTCM+0x3FFC
    /// (0x0B003FFC given our fixed DTCM base) instead of `[0x04000000-4]`.
    /// ponytail: DTCM base is hard-coded to the value boot_load_rom sets; revisit
    /// if a title relocates DTCM before enabling IRQs.
    fn arm9_bios_with_irq_handler() -> Vec<u8> {
        let mut bios = vec![0u8; 0x4000];
        const HANDLER: [u32; 7] = [
            0xE92D_500F, // stmfd sp!, {r0-r3, r12, lr}
            0xE59F_000C, // ldr   r0, [pc, #12]     (r0 = DTCM+0x3FFC literal below)
            0xE28F_E000, // add   lr, pc, #0        (lr = the ldmfd below)
            0xE590_F000, // ldr   pc, [r0]          (pc = [DTCM+0x3FFC] = game handler)
            0xE8BD_500F, // ldmfd sp!, {r0-r3, r12, lr}
            0xE25E_F004, // subs  pc, lr, #4
            0x0B00_3FFC, // literal: DTCM base + 0x3FFC
        ];
        for (i, w) in HANDLER.iter().enumerate() {
            bios[0x18 + i * 4..0x18 + i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        bios
    }

    /// Build the ARM7 exception-vector region with the standard NDS BIOS IRQ
    /// handler installed at the IRQ vector (0x18). Real ARM7 BIOS sequence
    /// (GBATEK): push the caller regs, load the game's IRQ handler pointer from
    /// `[0x03FFFFFC]` (which aliases `[0x0380FFFC]`, the documented slot), call
    /// it, restore, and `subs pc,lr,#4` to return from the interrupt. Executing
    /// this as real code (rather than HLE-ing it in the CPU) keeps the exception
    /// return path handled by the shared interpreter.
    fn arm7_bios_with_irq_handler() -> Vec<u8> {
        let mut bios = vec![0u8; 0x4000];
        const HANDLER: [u32; 6] = [
            0xE92D_500F, // stmfd sp!, {r0-r3, r12, lr}
            0xE3A0_0404, // mov   r0, #0x04000000
            0xE28F_E000, // add   lr, pc, #0        (lr = the ldmfd below)
            0xE510_F004, // ldr   pc, [r0, #-4]     (pc = [0x03FFFFFC] = game handler)
            0xE8BD_500F, // ldmfd sp!, {r0-r3, r12, lr}
            0xE25E_F004, // subs  pc, lr, #4        (return from IRQ, restores CPSR)
        ];
        for (i, w) in HANDLER.iter().enumerate() {
            bios[0x18 + i * 4..0x18 + i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        bios
    }

    pub fn reset(&mut self) {
        self.main_ram.fill(0);
        self.shared_wram.fill(0);
        self.arm7_wram.fill(0);
        self.itcm.fill(0);
        self.dtcm.fill(0);
        self.arm9_io.fill(0);
        self.arm7_io.fill(0);
        self.vram = VramManager::new();
        self.wram_control = 0;
        self.ipc = IpcState::new();
        self.timers9 = NdsTimers::default();
        self.timers7 = NdsTimers::default();
        self.rtc = NdsRtc::default();
        self.apu = crate::nds::apu::NdsApu::new();
        self.dma9_internal_dst = [0; 4];
        self.set_cp15(Cp15Registers::default());
        self.spi = SpiController::new();
        // The geometry engine is state like every peripheral above, and it was
        // the one left out. It carries a command *decoder position* as well as
        // registers, so a soft reset (Ctrl+R, or opening a second .nds from the
        // browser) landing while a MTX_LOAD_4x4 is half-received made the new
        // cartridge's first display-list words get eaten as the old command's
        // missing parameters — every following word then decodes as an opcode
        // and the whole list desyncs. The stale matrix stack, viewport, lights
        // and framebuffer came along too.
        self.gx = crate::nds::gx::GxDecoder::new();
        // `backup` is deliberately NOT reset here. reset() runs on every ROM
        // load and every soft reset (via hle::boot_load_rom), so recreating the
        // save chip would erase the player's save each time — the same reason
        // `rom` is left alone. Only its transaction state needs clearing, and a
        // reset drops chip-select on real hardware.
        self.backup.deselect();
        self.buttons = ButtonState {
            up: false,
            down: false,
            left: false,
            right: false,
            a: false,
            b: false,
            start: false,
            select: false,
            l: false,
            r: false,
            x: false,
            y: false,
            nds_touch_x: 0,
            nds_touch_y: 0,
            nds_touch_pressed: false,
        };
        self.arm9_ie = 0;
        self.arm9_if = 0;
        self.arm9_ime = 0;
        self.arm7_ie = 0;
        self.arm7_if = 0;
        self.arm7_ime = 0;
        self.palette_ram = [0; 4096];
        self.oam = [0; 2048];
        self.has_3d_activity = false;
    }

    // --- TCM Range Check Helpers ---

    /// The ARM9 CP15 registers the address routing reads.
    ///
    /// Write them with [`Self::set_cp15`]; there is no field access, because
    /// the TCM windows are derived from these three values.
    pub fn cp15(&self) -> Cp15Registers {
        self.arm9_cp15
    }

    /// Install a new CP15 register set and re-derive the TCM windows.
    ///
    /// The single writer, so [`Self::itcm_win`] / [`Self::dtcm_win`] cannot go
    /// stale: there is no other way to change their inputs.
    pub fn set_cp15(&mut self, regs: Cp15Registers) {
        self.arm9_cp15 = regs;
        self.itcm_win = TcmWindow::derive(regs.control, 18, regs.itcm_control);
        self.dtcm_win = TcmWindow::derive(regs.control, 16, regs.dtcm_control);
        // Reconfiguring a TCM changes what an *address* means, so every compiled
        // block keyed on one is suspect even though no byte of memory moved.
        self.invalidate_all_code();
    }

    /// Which [`Self::code_versions`] page holds ARM9 address `addr`, or `None`
    /// when it is not memory the recompiler tracks.
    ///
    /// Mirrors `write_byte_arm9`'s decode exactly, and must keep doing so: a
    /// page this reports that the write path bumps differently is a stale block
    /// that never gets invalidated. Returning `None` is always safe — the caller
    /// falls back to the interpreter.
    pub fn code_page_arm9(&self, addr: u32) -> Option<usize> {
        if self.in_itcm_arm9(addr) {
            let off = (addr.wrapping_sub(self.itcm_base()) as usize) % self.itcm.len();
            return Some(CODE_PAGES_MAIN + (off >> CODE_PAGE_BITS));
        }
        if self.in_dtcm_arm9(addr) {
            return None; // data TCM, never executed
        }
        // The BIOS image, at either window `read_byte_arm9` decodes (low
        // 0x00 when ITCM does not shadow it — checked above — and high 0xFF).
        // ROM-backed: only `invalidate_all_code` ever moves these versions.
        match (addr >> 24) & 0xFF {
            0x00 => {
                let off = ((addr & 0x00FF_FFFF) % 0x4000) as usize;
                return Some(CODE_PAGES_MAIN + CODE_PAGES_ITCM + CODE_PAGES_ARM7_WRAM
                    + (off >> CODE_PAGE_BITS));
            }
            0xFF => {
                let off = (addr & 0x3FFF) as usize;
                return Some(CODE_PAGES_MAIN + CODE_PAGES_ITCM + CODE_PAGES_ARM7_WRAM
                    + (off >> CODE_PAGE_BITS));
            }
            _ => {}
        }
        if (addr >> 24) & 0xFF != 0x02 {
            return None; // cartridge, I/O: not tracked, so not cached
        }
        let off = addr & 0x00FF_FFFF;
        if (0x400000..0x480000).contains(&off) {
            return None; // shared-WRAM window, whose mapping depends on WRAMCNT
        }
        Some(((off % (4 * 1024 * 1024)) as usize) >> CODE_PAGE_BITS)
    }

    /// ARM7 twin of [`Self::code_page_arm9`]: which tracked page holds ARM7
    /// address `addr`, or `None` when it is not memory the recompiler tracks.
    ///
    /// Main RAM is the same storage the ARM9 sees, so it maps to the same page
    /// indices — an ARM9 store to shared code invalidates the ARM7's blocks
    /// there and vice versa, with no cross-core bookkeeping. The private 64 KB
    /// WRAM follows ITCM in [`Self::code_versions`]. The shared-WRAM window
    /// (0x03, offset below 0x800000) is untracked because its mapping depends
    /// on WRAMCNT; returning `None` declines the block, which is always safe.
    ///
    /// Mirrors `write_byte_arm7`'s decode exactly, same obligation as the
    /// ARM9 twin: a page this reports that the write path bumps differently
    /// is a stale block that never gets invalidated.
    pub fn code_page_arm7(&self, addr: u32) -> Option<usize> {
        match (addr >> 24) & 0xFF {
            // The ARM7 BIOS image — ROM-backed, mirrored like the read path;
            // only `invalidate_all_code` moves these versions.
            0x00 => {
                let off = ((addr & 0x00FF_FFFF) % 0x4000) as usize;
                Some(CODE_PAGES_MAIN + CODE_PAGES_ITCM + CODE_PAGES_ARM7_WRAM
                    + CODE_PAGES_BIOS9 + (off >> CODE_PAGE_BITS))
            }
            0x02 => {
                let off = (addr & 0x00FF_FFFF) % (4 * 1024 * 1024);
                Some((off as usize) >> CODE_PAGE_BITS)
            }
            0x03 => {
                let offset = addr & 0x00FF_FFFF;
                if offset < 0x80_0000 {
                    // The shared window, mapped to the storage the CURRENT
                    // WRAMCNT mode selects — the same arithmetic as
                    // `read_shared_wram_arm7`, storage-indexed so the dirty
                    // hooks agree. A WRAMCNT write remaps what these
                    // addresses mean, so it invalidates wholesale (below).
                    return Some(match self.wram_control & 3 {
                        3 => CODE_BASE_SHARED
                            + (((offset % (256 * 1024)) as usize) >> CODE_PAGE_BITS),
                        2 => CODE_BASE_SHARED
                            + (((128 * 1024 + offset % (128 * 1024)) as usize)
                                >> CODE_PAGE_BITS),
                        1 => CODE_BASE_SHARED
                            + (((offset % (128 * 1024)) as usize) >> CODE_PAGE_BITS),
                        // Mode 0 aliases the ARM7's own WRAM here.
                        _ => CODE_PAGES_MAIN
                            + CODE_PAGES_ITCM
                            + (((offset % (64 * 1024)) as usize) >> CODE_PAGE_BITS),
                    });
                }
                let off = ((offset - 0x80_0000) % (64 * 1024)) as usize;
                Some(CODE_PAGES_MAIN + CODE_PAGES_ITCM + (off >> CODE_PAGE_BITS))
            }
            _ => None, // I/O, VRAM: not tracked, so not cached
        }
    }

    /// The current version of a page from [`Self::code_page_arm9`].
    pub fn code_version(&self, page: usize) -> u32 {
        self.code_versions.get(page).copied().unwrap_or(0)
    }

    /// Discard every compiled block, by moving every page version.
    ///
    /// For changes that invalidate wholesale rather than per page: a TCM remap,
    /// a savestate restore, a ROM load.
    pub fn invalidate_all_code(&mut self) {
        for v in &mut self.code_versions {
            *v = v.wrapping_add(1);
        }
        // Wholesale invalidation also invalidates every successor link, on
        // both cores.
        self.code_write_epoch = self.code_write_epoch.wrapping_add(1);
        self.code_write_epoch7 = self.code_write_epoch7.wrapping_add(1);
    }

    /// Record that a compiled block of the core named by `mask` spans `page`,
    /// so stores landing there move that core's epoch. See
    /// [`Self::code_block_pages`].
    pub fn mark_code_page(&mut self, page: usize, mask: u8) {
        if let Some(p) = self.code_block_pages.get_mut(page) {
            *p |= mask;
        }
    }

    /// Note that a store landed on `page`.
    ///
    /// The `code_watch` test is what keeps this off the bill when no recompiler
    /// is running; see [`Self::code_watch`].
    #[inline(always)]
    fn dirty_code_page(&mut self, page: usize) {
        if self.code_watch {
            if let Some(v) = self.code_versions.get_mut(page) {
                *v = v.wrapping_add(1);
            }
            // Only pages that hold compiled blocks move a link epoch — and
            // only the owning core's, so the common case (a store to plain
            // data) adds one predictable load and an ARM7 buffer store cannot
            // tear down ARM9 chains.
            let mask = self.code_block_pages.get(page).copied().unwrap_or(0);
            if mask & CODE_MASK_ARM9 != 0 {
                self.code_write_epoch = self.code_write_epoch.wrapping_add(1);
            }
            if mask & CODE_MASK_ARM7 != 0 {
                self.code_write_epoch7 = self.code_write_epoch7.wrapping_add(1);
            }
        }
    }

    /// A store of `len` bytes at main-RAM storage offset `off`.
    #[inline(always)]
    fn dirty_main_ram(&mut self, off: usize, len: usize) {
        if !self.code_watch {
            return;
        }
        let first = off >> CODE_PAGE_BITS;
        let last = (off + len.saturating_sub(1)) >> CODE_PAGE_BITS;
        for page in first..=last {
            self.dirty_code_page(page);
        }
    }

    /// A store of `len` bytes at ITCM storage offset `off`.
    #[inline(always)]
    fn dirty_itcm(&mut self, off: usize, len: usize) {
        if !self.code_watch {
            return;
        }
        let first = CODE_PAGES_MAIN + (off >> CODE_PAGE_BITS);
        let last = CODE_PAGES_MAIN + ((off + len.saturating_sub(1)) >> CODE_PAGE_BITS);
        for page in first..=last {
            self.dirty_code_page(page);
        }
    }

    /// A store of `len` bytes at shared-WRAM **storage** offset `off`.
    /// Storage-indexed, so every WRAMCNT mode's window decode funnels here
    /// with the same arithmetic it used for the store itself.
    #[inline(always)]
    fn dirty_shared_wram(&mut self, off: usize, len: usize) {
        if !self.code_watch {
            return;
        }
        let first = CODE_BASE_SHARED + (off >> CODE_PAGE_BITS);
        let last = CODE_BASE_SHARED + ((off + len.saturating_sub(1)) >> CODE_PAGE_BITS);
        for page in first..=last {
            self.dirty_code_page(page);
        }
    }

    /// A store of `len` bytes at ARM7-WRAM storage offset `off`. Called from
    /// both windows onto that storage: 0x03800000+ always, and the 0x03000000
    /// shared window when WRAMCNT mode 0 aliases it here.
    #[inline(always)]
    fn dirty_arm7_wram(&mut self, off: usize, len: usize) {
        if !self.code_watch {
            return;
        }
        const BASE: usize = CODE_PAGES_MAIN + CODE_PAGES_ITCM;
        let first = BASE + (off >> CODE_PAGE_BITS);
        let last = BASE + ((off + len.saturating_sub(1)) >> CODE_PAGE_BITS);
        for page in first..=last {
            self.dirty_code_page(page);
        }
    }

    /// Does `addr` fall in the enabled ITCM window? **The** hot TCM gate: asked
    /// on every ARM9 byte, halfword and word access, on both the read and the
    /// write path. One subtract and one compare — see [`TcmWindow`] for what
    /// this replaced.
    #[inline(always)]
    pub fn in_itcm_arm9(&self, addr: u32) -> bool {
        self.itcm_win.contains(addr)
    }

    /// DTCM twin of [`Self::in_itcm_arm9`].
    #[inline(always)]
    pub fn in_dtcm_arm9(&self, addr: u32) -> bool {
        self.dtcm_win.contains(addr)
    }

    pub fn itcm_enabled(&self) -> bool {
        // ARM946: ITCM enable is Control Register bit 18 ONLY. The TCM Region
        // Register (itcm_control) holds base+size; its low bits are the size
        // field, NOT an enable — games leave bit 0 clear (e.g. itcm_control=0x20).
        (self.arm9_cp15.control & (1 << 18)) != 0
    }

    pub fn dtcm_enabled(&self) -> bool {
        (self.arm9_cp15.control & (1 << 16)) != 0
    }

    pub fn itcm_base(&self) -> u32 {
        self.arm9_cp15.itcm_control & 0xFFFFF000
    }

    pub fn dtcm_base(&self) -> u32 {
        self.arm9_cp15.dtcm_control & 0xFFFFF000
    }

    /// Keep the synthetic ARM9 IRQ handler's embedded literal in sync with the
    /// live DTCM base. The handler dispatches through the game's registered
    /// pointer at `[DTCM + 0x3FFC]`; that address moves when the game relocates
    /// DTCM via CP15 (SoulSilver: 0x0B000000 boot default → 0x027E0000), so the
    /// literal at word offset 0x30 of the handler must follow it — otherwise the
    /// handler reads a stale address and the IRQ dispatches to garbage. Called
    /// from `execute_cp15_transfer` on every DTCM control write.
    pub fn update_arm9_irq_handler_ptr(&mut self) {
        let ptr = self.dtcm_base().wrapping_add(0x3FFC);
        self.arm9_bios[0x30..0x34].copy_from_slice(&ptr.to_le_bytes());
    }

    /// Is `addr` inside the ITCM *region*, ignoring whether ITCM is enabled?
    ///
    /// Kept as the region-only predicate the tests pin (a region register is
    /// meaningful before the enable bit is set); the address routing wants
    /// enable-and-range together and uses [`Self::in_itcm_arm9`].
    pub fn in_itcm_range_arm9(&self, addr: u32) -> bool {
        TcmWindow {
            base: self.itcm_base(),
            size: TcmWindow::region_size(self.arm9_cp15.itcm_control),
        }
        .contains(addr)
    }

    /// DTCM twin of [`Self::in_itcm_range_arm9`].
    pub fn in_dtcm_range_arm9(&self, addr: u32) -> bool {
        TcmWindow {
            base: self.dtcm_base(),
            size: TcmWindow::region_size(self.arm9_cp15.dtcm_control),
        }
        .contains(addr)
    }

    // --- Shared WRAM Client Decoders ---
    /// WRAMCNT allocation, GBATEK "DS Memory Control - WRAM". The four modes are
    /// monotonic — the ARM9 loses memory as the value rises:
    ///
    /// | mode | ARM9      | ARM7      |
    /// |------|-----------|-----------|
    /// | 0    | all       | none      |
    /// | 1    | 2nd half  | 1st half  |
    /// | 2    | 1st half  | 2nd half  |
    /// | 3    | none      | all       |
    ///
    /// Modes **1 and 3 used to be transposed here** (1 gave the ARM9 nothing and
    /// 3 gave it the upper half), which also meant `nds/hle.rs` writing 3 to mean
    /// "hand the shared WRAM to the ARM7" — GBATEK's mode 3 exactly — silently
    /// got a half-and-half split instead. Mode 2 was already correct. Relabelling
    /// is observably neutral for the current boot: the ARM9's half measures
    /// entirely zero over 900 frames, so ARM9 reads move from "the upper block,
    /// which is all zeros" to "no mapping, reads 0".
    fn read_shared_wram_arm9(&self, offset: u32) -> u8 {
        let mode = self.wram_control & 3;
        match mode {
            0 => self.shared_wram[(offset % (256 * 1024)) as usize], // Entire 256KB to ARM9
            2 => {
                // Block 0 (128KB) to ARM9 at 0x02400000
                if offset < 128 * 1024 {
                    self.shared_wram[offset as usize]
                } else {
                    0
                }
            }
            1 => {
                // Block 1 (128KB) to ARM9 at 0x02440000
                let local_offset = offset.wrapping_sub(256 * 1024);
                if local_offset < 128 * 1024 {
                    self.shared_wram[(128 * 1024 + local_offset) as usize]
                } else {
                    0
                }
            }
            _ => 0, // Mode 3: mapped to ARM7 only
        }
    }

    fn write_shared_wram_arm9(&mut self, offset: u32, val: u8) {
        let mode = self.wram_control & 3;
        match mode {
            0 => {
                let idx = (offset % (256 * 1024)) as usize;
                self.shared_wram[idx] = val;
                self.dirty_shared_wram(idx, 1);
            }
            2 => {
                if offset < 128 * 1024 {
                    self.shared_wram[offset as usize] = val;
                    self.dirty_shared_wram(offset as usize, 1);
                }
            }
            1 => {
                let local_offset = offset.wrapping_sub(256 * 1024);
                if local_offset < 128 * 1024 {
                    let idx = (128 * 1024 + local_offset) as usize;
                    self.shared_wram[idx] = val;
                    self.dirty_shared_wram(idx, 1);
                }
            }
            _ => {}
        }
    }

    /// ponytail: this window diverges from GBATEK three ways at once — the DS
    /// has **32 KB** of shared WRAM and this is a 256 KB `Vec` split into 128 KB
    /// halves; and the ARM9 side is based at 0x02400000 with no 0x03 arm at all.
    /// (The mode table itself was ALSO wrong — modes 1 and 3 transposed — but
    /// that is fixed above; an earlier revision of this comment misdescribed it
    /// as "modes 2 and 3 swapped", which was never true: mode 2 was correct.)
    ///
    /// Measured on SoulSilver's boot before deciding to leave it: the only mode
    /// ever observed is **3** — but that is NOT the cartridge's choice. The boot
    /// HLE pins `wram_control = 3` (`nds/hle.rs`, itself a documented
    /// approximation of the real IPC handshake) and nothing overwrites it during
    /// the intro, so "mode 3 only" says the mode-2 arm is *untested*, not that
    /// the game selected it. The ARM9 writes **zero** bytes
    /// through its half, and the ARM7's writes sit entirely in offsets
    /// `0x18000..=0x1FFFF` — that is `(0x037F8000 - 0x03000000) % 0x20000`, i.e.
    /// the ARM7 keeps its 32 KB of code and stack at 0x037F8000-0x037FFFFF and
    /// nothing else uses the window. Mode 3 gives the two cores disjoint halves
    /// on hardware too, so the swap is unobservable while only mode 3 is used
    /// and only one core writes.
    ///
    /// Ceiling: the 128 KB modulus gives the ARM7 a private 32 KB at 0x037F8000
    /// that a real DS does not have there — hardware would alias that span into
    /// the 16 KB mode 3 allocates and it would overlap itself, so the real ARM7
    /// image must live elsewhere. A title that switches WRAMCNT mid-run would
    /// also see the ARM9's data move between halves. Upgrade path: size the
    /// region to 32 KB, mirror it at its allocated width, follow GBATEK's half
    /// assignment, and re-home the ARM7's 0x037Fxxxx window on its own 64 KB
    /// WRAM — all four together, since the 0x02400000 base is load-bearing for
    /// the WRAMCNT decoders, the fast paths and an existing test. Not attempted
    /// without a symptom: the boot currently depends on this layout.
    fn read_shared_wram_arm7(&self, offset: u32) -> u8 {
        let mode = self.wram_control & 3;
        match mode {
            3 => self.shared_wram[(offset % (256 * 1024)) as usize], // Entire 256KB to ARM7
            2 => {
                let mirrored_offset = offset % (128 * 1024);
                self.shared_wram[(128 * 1024 + mirrored_offset) as usize]
            }
            1 => {
                self.shared_wram[(offset % (128 * 1024)) as usize]
            }
            // Mode 0: shared WRAM is all mapped to the ARM9, so the ARM7's
            // 0x03000000-0x037FFFFF window instead mirrors its own 64KB WRAM
            // (same block seen at 0x03800000). Games relocate ARM7 code here and
            // read it back at 0x037Fxxxx, so this must alias, not read 0.
            _ => self.arm7_wram[(offset % (64 * 1024)) as usize],
        }
    }

    fn write_shared_wram_arm7(&mut self, offset: u32, val: u8) {
        let mode = self.wram_control & 3;
        match mode {
            3 => {
                let idx = (offset % (256 * 1024)) as usize;
                self.shared_wram[idx] = val;
                self.dirty_shared_wram(idx, 1);
            }
            2 => {
                let idx = (128 * 1024 + offset % (128 * 1024)) as usize;
                self.shared_wram[idx] = val;
                self.dirty_shared_wram(idx, 1);
            }
            1 => {
                let idx = (offset % (128 * 1024)) as usize;
                self.shared_wram[idx] = val;
                self.dirty_shared_wram(idx, 1);
            }
            // Mode 0: mirror the ARM7's own WRAM (see read path above). Same
            // storage the tracked 0x03800000 window exposes, so the store must
            // dirty it — an ARM7 block compiled from that window would
            // otherwise survive code rewritten through this alias.
            _ => {
                let idx = (offset % (64 * 1024)) as usize;
                self.arm7_wram[idx] = val;
                self.dirty_arm7_wram(idx, 1);
            }
        }
    }

    /// Advance both CPUs' timer blocks by `cycles` 33.55 MHz bus cycles (the
    /// unit the NDS run loop already budgets per slice). Overflow IRQs land in
    /// the owning core's IF (bits 3-6).
    pub fn tick_nds_timers(&mut self, cycles: u32) {
        let mut if9 = self.arm9_if;
        self.timers9.tick(cycles, &mut if9);
        self.arm9_if = if9;
        let mut if7 = self.arm7_if;
        self.timers7.tick(cycles, &mut if7);
        self.arm7_if = if7;
    }

    // --- Interrupt Triggering ---
    pub fn trigger_interrupt_arm9(&mut self, mask: u32) {
        self.arm9_if |= mask;
    }

    pub fn trigger_interrupt_arm7(&mut self, mask: u32) {
        self.arm7_if |= mask;
    }

    /// Record the executing ARM9 PC when the watched texture is selected.
    ///
    /// Called right after a geometry write is decoded, so `engine.teximage`
    /// already holds whatever that write selected. Bounded by
    /// [`GX_TEX_WATCH_CAP`]; a no-op when the watch is disarmed.
    fn note_gx_tex_watch(&mut self) {
        // Edge-triggered: only when a NEW zero MTX_SCALE was just committed, so
        // the recorded pc/lr is the code that produced those parameters rather
        // than whatever writes the engine next.
        let zeros = self.gx.engine.zero_scale_count;
        if zeros != self.gx_zero_scale_seen {
            self.gx_zero_scale_seen = zeros;
            let entry = (self.arm9_exec_pc, self.arm9_exec_lr);
            if self.gx_zero_scale_pcs.len() < GX_TEX_WATCH_CAP
                && !self.gx_zero_scale_pcs.contains(&entry)
            {
                self.gx_zero_scale_pcs.push(entry);
            }
        }
        if !self.gx_tex_watch_all && self.gx_tex_watch.is_none() {
            return;
        }
        let tex = self.gx.engine.teximage;
        if let Some(want) = self.gx_tex_watch {
            if tex & 0xFFFF != want & 0xFFFF {
                return;
            }
        }
        if self.gx_tex_watch_pcs.len() >= GX_TEX_WATCH_CAP {
            return;
        }
        let (pc, lr) = (self.arm9_exec_pc, self.arm9_exec_lr);
        // In census mode one entry per distinct (caller, texture) is enough; the
        // cap is small, so the linear scan costs nothing.
        if self.gx_tex_watch_all
            && self.gx_tex_watch_pcs.iter().any(|(_, l, t)| *l == lr && *t == tex)
        {
            return;
        }
        self.gx_tex_watch_pcs.push((pc, lr, tex));
    }

    /// GX FIFO IRQ (bit 21, ARM9). The HLE FIFO is always empty, so whenever
    /// the game has selected a nonzero IRQ condition in GXSTAT bits 30-31
    /// (1 = less than half full, 2 = empty) that condition already holds;
    /// latch IF on every selection write and every command push.
    fn maybe_raise_gxfifo_irq(&mut self) {
        if self.arm9_io[0x603] & 0xC0 != 0 {
            self.trigger_interrupt_arm9(1 << 21);
        }
    }

    /// Byte write anywhere in the ARM7 sound block (offset 0x400..0x520).
    /// Channel registers update in place; a 0->1 edge on a SOUNDxCNT start
    /// bit (byte 3 bit 7) keys the channel on. Clearing it stops the channel.
    pub(crate) fn apu_write_byte(&mut self, offset: u32, val: u8) {
        // Every write in this block can change the mix (volume, pan, enable, a
        // key-on edge) or the next channel edge, so it ends the deferral
        // [`Self::tick_apu`] is holding. Setting it here, at the one entry point
        // for the whole sound block, is what keeps the deferral exact rather
        // than "close enough". See [`Self::apu_pending`].
        self.apu_mix_dirty = true;
        match offset {
            0x500 | 0x501 => {
                let shift = (offset - 0x500) * 8;
                let keep = !(0xFFu16 << shift);
                self.apu.soundcnt = (self.apu.soundcnt & keep) | ((val as u16) << shift);
                // OR the census on EVERY byte of the register. Recording only
                // the high byte reported "master volume 0" for a game that
                // writes 0x500 in a separate store — a false lead, since `mix`
                // does apply the volume field.
                self.apu.dbg_soundcnt_seen |= self.apu.soundcnt;
            }
            0x504 => self.apu.soundbias = (self.apu.soundbias & 0xFF00) | val as u16,
            0x505 => self.apu.soundbias = (self.apu.soundbias & 0x00FF) | ((val as u16) << 8),
            o if (0x400..0x500).contains(&o) => {
                let i = ((o - 0x400) / 16) as usize;
                let reg = o & 0xF;
                let ch = &mut self.apu.channels[i];
                match reg {
                    0 => ch.cnt = (ch.cnt & !0xFF) | val as u32,
                    1 => ch.cnt = (ch.cnt & !0xFF00) | ((val as u32) << 8),
                    2 => ch.cnt = (ch.cnt & !0xFF_0000) | ((val as u32) << 16),
                    3 => {
                        let was_started = ch.cnt & (1 << 31) != 0;
                        ch.cnt = (ch.cnt & !0xFF00_0000) | ((val as u32) << 24);
                        if val & 0x80 != 0 {
                            if !was_started {
                                // Census: a channel retriggering many times a
                                // second is what "repetitive" audio sounds like,
                                // and it is invisible in RMS or duplicate-block
                                // checks. Musical key-ons are a few per second.
                                self.apu.key_ons[i] = self.apu.key_ons[i].saturating_add(1);
                                self.apu_key_on(i);
                            }
                        } else {
                            ch.active = false;
                        }
                    }
                    4..=7 => {
                        let sh = (reg - 4) * 8;
                        ch.sad = (ch.sad & !(0xFF << sh)) | ((val as u32) << sh);
                        ch.sad &= 0x07FF_FFFF;
                    }
                    8 => ch.tmr = (ch.tmr & 0xFF00) | val as u16,
                    9 => ch.tmr = (ch.tmr & 0x00FF) | ((val as u16) << 8),
                    0xA => ch.pnt = (ch.pnt & 0xFF00) | val as u16,
                    0xB => ch.pnt = (ch.pnt & 0x00FF) | ((val as u16) << 8),
                    _ => {
                        let sh = (reg - 0xC) * 8;
                        ch.len = (ch.len & !(0xFF << sh)) | ((val as u32) << sh);
                        ch.len &= 0x003F_FFFF;
                    }
                }
            }
            // Sound capture registers: state is stored + logged (U27 echo
            // evidence); no recording into the DAD rings happens yet.
            0x508 | 0x509 => {
                self.apu.cap_cnt[(offset - 0x508) as usize] = val;
                self.apu_log_cap_write(offset, val);
            }
            0x510..=0x513 | 0x518..=0x51B => {
                let u = if offset < 0x518 { 0 } else { 1 };
                let sh = ((offset & 3) * 8) as u32;
                self.apu.cap_dad[u] =
                    (self.apu.cap_dad[u] & !(0xFF << sh)) | ((val as u32) << sh);
                self.apu.cap_dad[u] &= 0x07FF_FFFF;
                self.apu_log_cap_write(offset, val);
            }
            0x514 | 0x515 | 0x51C | 0x51D => {
                let u = if offset < 0x518 { 0 } else { 1 };
                let sh = ((offset & 1) * 8) as u32;
                self.apu.cap_len[u] = (self.apu.cap_len[u] & !(0xFF << sh)) | ((val as u16) << sh);
                self.apu_log_cap_write(offset, val);
            }
            // 0x502-0x503, 0x506-0x507, 0x516-0x517, 0x51E-0x51F: unused.
            _ => {}
        }
    }

    /// Divider outputs per DIVCNT mode (0 = 32/32, 1 = 64/32, 2 = 64/64).
    /// ponytail: division by zero returns quotient 0 + remainder = numerator
    /// and raises the DIVCNT div0 flag — GBATEK's exact ±1/inverted-hi
    /// pattern waits until a game visibly divides by zero.
    fn div_results(&self) -> (i64, i64) {
        let (n, d) = match self.div_cnt & 3 {
            0 => (self.div_numer as i32 as i64, self.div_denom as i32 as i64),
            1 => (self.div_numer, self.div_denom as i32 as i64),
            _ => (self.div_numer, self.div_denom),
        };
        if d == 0 {
            return (0, n);
        }
        (n.wrapping_div(d), n.wrapping_rem(d))
    }

    /// Square-root unit: SQRTCNT bit0 selects 64-bit input; the result is
    /// the 32-bit floor square root.
    fn sqrt_result(&self) -> u32 {
        let v = if self.sqrt_cnt & 1 != 0 {
            self.sqrt_param
        } else {
            self.sqrt_param as u32 as u64
        };
        let mut x = (v as f64).sqrt() as u64;
        while x.checked_mul(x).map_or(true, |s| s > v) {
            x -= 1;
        }
        while (x + 1).checked_mul(x + 1).map_or(false, |s| s <= v) {
            x += 1;
        }
        x as u32
    }

    /// Largest AUXSPI capture kept. A SoulSilver save is ~512 KB written in
    /// 256-byte pages, so a full save would be millions of entries; the cap
    /// keeps the boot-window question answerable without unbounded growth, and
    /// `aux_bus_dropped` records what the cap refused.
    /// A healthy boot clocks over a million bytes through this bus (two
    /// 0x23000-byte reads in 256-byte chunks), so the cap is sized to cover the
    /// identify handshake and the first reads in full; `aux_bus_dropped`
    /// records the rest rather than letting a truncated capture read as a quiet
    /// bus.
    pub(crate) const AUX_BUS_LOG_MAX: usize = 8192;

    /// W0.1 evidence hook: record one AUXSPI register write.
    ///
    /// The stored tag is `register_index | owner_bit | core_bit`, where the
    /// index is 0..=3 for 0x040001A0..0x040001A3, bit 6 carries EXMEMCNT bit 11
    /// (the NDS slot access rights: 1 = ARM7 owns the slot) at the time of the
    /// write, and bit 7 marks an ARM9 write. The index is deliberately NOT the
    /// raw address byte: 0xA0-0xA3 already have bit 7 set, so a core marker in
    /// that bit would be indistinguishable from the address and every entry
    /// would decode identically.
    ///
    /// Pure observation — callers still perform their original store.
    pub(crate) fn note_aux_write(&mut self, arm9: bool, offset: u32, val: u8) {
        let tag = (offset.wrapping_sub(0x0001A0) as u8 & 0x03)
            | if self.slot_owned_by_arm7() { 0x40 } else { 0 }
            | if arm9 { 0x80 } else { 0 };
        if self.aux_bus_log.len() < Self::AUX_BUS_LOG_MAX {
            self.aux_bus_log.push((tag, val));
        } else {
            self.aux_bus_dropped = self.aux_bus_dropped.saturating_add(1);
        }
    }

    /// EXMEMCNT (0x04000204) bit 11: NDS slot access rights, 0 = ARM9, 1 = ARM7.
    /// The register is ARM9-writable, so the ARM9 copy is authoritative.
    pub(crate) fn slot_owned_by_arm7(&self) -> bool {
        self.arm9_io[0x205] & 0x08 != 0
    }

    /// One AUXSPI register read, shared by both cores.
    ///
    /// `offset` is the low 24 bits of the IO address (0x0001A0-0x0001A3).
    /// AUXSPICNT bit 7 (transfer busy) always reads clear: a transfer completes
    /// inside the AUXSPIDATA write below, so the driver's `TST #0x80 / BNE`
    /// spin exits on its first pass and is correct by construction. Reads never
    /// clock the bus — the driver clocks a dummy byte with a write first, then
    /// reads the latched response.
    ///
    /// ponytail: no timed busy window, because the NDS MMU has no cycle
    /// scheduler to hang one on. Ceiling: a game that measures transfer
    /// duration, or that relies on the CPU making progress during a transfer.
    /// Upgrade path: schedule the byte and hold bit 7 for the baud-rate-derived
    /// cycle count.
    fn read_auxspi_byte(&self, arm9: bool, offset: u32) -> u8 {
        let io = if arm9 { &self.arm9_io } else { &self.arm7_io };
        match offset {
            0x0001A0 => io[0x1A0] & !0x80,
            0x0001A1 => io[0x1A1],
            // With no chip on the cartridge the line floats to the pulled-up
            // 0xFF an absent/erased device presents. This must never be a stale
            // IO echo: a game's save-integrity scan reads it as save data.
            0x0001A2 => {
                if self.backup.is_present() {
                    self.backup.last_out
                } else {
                    0xFF
                }
            }
            // AUXSPIDATA is 8 bits wide; the high half of a halfword read is 0.
            _ => 0,
        }
    }

    /// One AUXSPI register write, shared by both cores.
    ///
    /// AUXSPICNT bit 6 is **"deselect after transfer"**, NOT a live chip-select
    /// line: the byte written to AUXSPIDATA is always clocked, and chip-select
    /// drops afterwards if bit 6 was clear. So deselection is driven by the
    /// DATA write, never by the control write — a control write only ends the
    /// transaction by powering the slot down (bit 15).
    ///
    /// Treating bit 6 as a live CS line shredded SoulSilver's frames: its
    /// driver sends the whole command with hold set, clears hold, and only then
    /// clocks the final byte, so one 3-byte transaction became three 1-byte
    /// transactions and every response byte was read as a fresh command.
    ///
    /// Note bit 6 lives in the LOW byte and bit 15 in the high byte — a
    /// different layout from the firmware/TSC bus at 0x040001C0, where
    /// chip-select-hold is bit 11.
    ///
    /// Halfword writes decompose into two byte writes, which is safe here:
    /// 0x1A3 is inert so a 16-bit AUXSPIDATA store clocks exactly one byte, and
    /// a 16-bit AUXSPICNT store passes through the intermediate value 0xA000
    /// (bit 15 still set) before reaching 0x0000, so it deselects exactly once.
    ///
    /// ponytail: EXMEMCNT (0x04000204) bit 11 slot ownership is ignored — both
    /// cores reach the same chip. Measured on SoulSilver: the bit is NEVER set
    /// across a full boot (0/552 ARM7 AUXSPI writes had it), because the HLE
    /// boot skips the firmware that would hand the slot over, so gating on it
    /// would silence the backup driver entirely. Ceiling: a title that hands the
    /// slot between cores and expects the non-owner to read open bus. Upgrade
    /// path: model EXMEMCNT properly at boot, THEN gate on `slot_owned_by_arm7`.
    fn write_auxspi_byte(&mut self, arm9: bool, offset: u32, val: u8) {
        match offset {
            0x0001A0 | 0x0001A1 => {
                let io = if arm9 { &mut self.arm9_io } else { &mut self.arm7_io };
                io[offset as usize] = val;
                if u16::from_le_bytes([io[0x1A0], io[0x1A1]]) & 0x8000 == 0 {
                    self.aux_end_transaction(arm9);
                }
            }
            0x0001A2 => {
                let io = if arm9 { &self.arm9_io } else { &self.arm7_io };
                let cnt = u16::from_le_bytes([io[0x1A0], io[0x1A1]]);
                // The slot must be enabled (bit 15) and in serial/SPI mode
                // (bit 13) for a byte to reach the backup chip at all; in
                // parallel mode 0x040001A2 belongs to the gamecard interface.
                if cnt & 0x8000 == 0 || cnt & 0x2000 == 0 {
                    return;
                }
                self.aux_txn_core = Some(arm9);
                let out = self.backup.transfer(val);
                if self.aux_io_log.len() < Self::AUX_BUS_LOG_MAX {
                    self.aux_io_log.push((val, out));
                }
                if cnt & 0x0040 == 0 {
                    self.aux_end_transaction(arm9);
                }
            }
            _ => {}
        }
    }

    /// Drop chip-select, counting the case where one core ends a transaction
    /// the other core opened (AUXSPICNT is one physical register shared by both
    /// cores; the ARM9 drives it for gamecard ROM streaming while the ARM7
    /// drives it for the backup chip).
    fn aux_end_transaction(&mut self, arm9: bool) {
        if self.aux_txn_core.is_some_and(|c| c != arm9) {
            self.aux_cross_deselects = self.aux_cross_deselects.saturating_add(1);
        }
        self.aux_txn_core = None;
        self.backup.deselect();
    }

    /// U27g read-watch helper (see `tp_slot_reads`).
    pub(crate) fn tp_note_read(&mut self, addr: u32) {
        if (0x027F_FC80..0x027F_FCE8).contains(&addr) && self.settings_read_log.len() < 16 {
            self.settings_read_log.push(addr);
        }
        if addr == 0x0400_01A2 && self.aux_log.len() < 64 {
            self.aux_log.push((1, 0xFF));
        }
        match addr & !3 {
            0x021E_36B8 => self.tp_slot_reads[0] = self.tp_slot_reads[0].wrapping_add(1),
            0x021E_36BC => self.tp_slot_reads[1] = self.tp_slot_reads[1].wrapping_add(1),
            0x027F_FFA8 => self.tp_slot_reads[2] = self.tp_slot_reads[2].wrapping_add(1),
            0x027F_FFAC => self.tp_slot_reads[3] = self.tp_slot_reads[3].wrapping_add(1),
            // The per-request double buffers the tag-7 command ring names —
            // frozen at the idle signature; does anyone actually read them?
            0x021D_D460 => self.tp_slot_reads[4] = self.tp_slot_reads[4].wrapping_add(1),
            0x021D_E640 => self.tp_slot_reads[5] = self.tp_slot_reads[5].wrapping_add(1),
            _ => {}
        }
    }

    fn apu_log_cap_write(&mut self, offset: u32, val: u8) {
        if self.apu.cap_write_log.len() < 256 {
            self.apu.cap_write_log.push((offset, val));
        }
    }

    /// Key a channel on: reset the playback cursor and decoder. ADPCM reads
    /// its 4-byte header at SAD (initial sample + step-table index) here.
    fn apu_key_on(&mut self, i: usize) {
        let mut ch = self.apu.channels[i];
        ch.active = true;
        ch.timer_acc = 0;
        ch.sample = 0;
        ch.adpcm_high = false;
        ch.psg_phase = 0;
        ch.noise_lfsr = 0x7FFF;
        if ch.format() == 2 {
            let hdr = self.read_word_arm7(ch.sad);
            ch.adpcm_val = hdr as u16 as i16 as i32;
            ch.adpcm_idx = (((hdr >> 16) & 0x7F) as i32).min(88);
            ch.adpcm_loop_val = ch.adpcm_val;
            ch.adpcm_loop_idx = ch.adpcm_idx;
            ch.cursor = 4;
        } else {
            ch.cursor = 0;
        }
        self.apu.channels[i] = ch;
    }

    /// Advance every playing channel by `cycles` bus cycles and integrate the
    /// stereo mix into the host-rate output through the shared box resampler.
    /// Called from the NDS run loop with the same cycle unit as the timers.
    ///
    /// The integration is split at channel edges, so the emitted samples do not
    /// depend on how the caller chunks `cycles` (pinned by
    /// `apu_output_is_independent_of_slice_size`).
    /// Accumulate `cycles` of APU time, rendering only once the result can
    /// actually differ.
    ///
    /// The run loop hands this ~64-cycle slices, but the mix is a step function
    /// that changes only at a channel edge (~760 cycles at a 44 kHz source) or
    /// at a sound-register write. Rendering per slice therefore recomputed the
    /// 16-channel mix and the 16-channel edge scan about eleven times per
    /// distinct answer — the cost the ponytail below used to describe.
    ///
    /// Deferring is not the cached-mix design that ponytail proposed, and it is
    /// not an approximation of the *emulated* machine: [`Self::render_apu`]
    /// splits at edges either way (pinned by
    /// `apu_output_is_independent_of_slice_size`, which is precisely the
    /// statement "the caller's chunking does not matter"), and the two events
    /// that *can* change the mix both force a render — an edge, by the
    /// comparison below, and a register write, via [`Self::apu_mix_dirty`].
    ///
    /// It is **not** bit-identical, and the honest bound is worth stating: the
    /// resampler integrates each span in floating point, so accumulating one
    /// 760-cycle span instead of twelve 64-cycle ones rounds differently.
    /// Measured over a 4000-tick cold boot (6.4 M samples,
    /// `nds_boot_handshake_audio_guard`): RMS 2462.4 -> 2462.3 and peak
    /// 31137 -> 31136, i.e. one LSB out of 32767, with the rendered frame hash
    /// unchanged. That is rounding, not a different waveform.
    ///
    /// The deferral must be closed before anything reads the audio buffer;
    /// [`Self::flush_apu`] is that call and the run loop makes it once per tick.
    ///
    /// This matters far more at fast-forward than the ponytail's own estimate
    /// suggested. It judged the cost against a 16.72 ms frame, which is the
    /// budget at 1x; at 5x a tick still gets 16.72 ms but must retire five
    /// frames, so the same work is ~half the entire budget.
    pub fn tick_apu(
        &mut self,
        cycles: u32,
        audio_buffer: &mut [i16],
        audio_offset: usize,
        speed: f32,
    ) {
        self.apu_pending = self.apu_pending.saturating_add(cycles);
        if !self.apu_mix_dirty && self.apu_pending < self.apu_cycles_to_next_edge() {
            return;
        }
        self.flush_apu(audio_buffer, audio_offset, speed);
    }

    /// Render everything [`Self::tick_apu`] has deferred.
    ///
    /// Must be called before the tick's audio buffer is read, or the last
    /// fraction of a frame is missing from it.
    pub fn flush_apu(&mut self, audio_buffer: &mut [i16], audio_offset: usize, speed: f32) {
        let cycles = std::mem::take(&mut self.apu_pending);
        self.apu_mix_dirty = false;
        self.render_apu(cycles, audio_buffer, audio_offset, speed);
    }

    fn render_apu(
        &mut self,
        cycles: u32,
        audio_buffer: &mut [i16],
        audio_offset: usize,
        speed: f32,
    ) {
        let cycles_per_sample = (crate::nds::apu::NDS_CYCLES_PER_SEC * speed as f64)
            / self.apu.resampler.output_hz();
        let mut remaining = cycles;
        while remaining > 0 {
            // The mix is a step function: it changes only when some channel
            // reaches its next source sample. Integrating exactly up to that
            // edge, with the state that held BEFORE it, keeps the edge on its
            // true cycle instead of quantized to whatever slice the caller
            // passed. The run loop feeds ~64-cycle slices — ~9% of one output
            // sample at 48 kHz — and the error varied slice to slice, so it
            // read as correlated grit rather than a constant delay. Same rule
            // the GBA path already follows: render with pre-event state, then
            // apply the event.
            // The "recomputed ~8750 times a frame" ponytail that stood here is
            // resolved by the deferral in `tick_apu`: `remaining` now arrives as
            // a whole edge-to-edge window, so `span` is capped by the edge —
            // which is what the clamp was always meant to express.
            let span = self.apu_cycles_to_next_edge().clamp(1, remaining);
            let (l, r) = self.apu.mix();
            // Evidence only: how much of the available output range the mixer
            // actually uses, and how often it saturates. Counted per rendered
            // span, so the ratios stay meaningful as the split changes — but
            // note `dbg_samples` counts *spans*, so it fell by roughly the
            // deferral factor when `tick_apu` started batching. The ratios
            // (`dbg_clip` / `dbg_samples`) are what that field is read for.
            let amp = l.abs().max(r.abs());
            if amp > self.apu.dbg_peak {
                self.apu.dbg_peak = amp;
            }
            if amp >= 0.999 {
                self.apu.dbg_clip += 1;
            }
            self.apu.dbg_samples += 1;
            self.apu
                .resampler
                .tick(span, l, r, cycles_per_sample, audio_buffer, audio_offset);
            self.apu_advance_channels(span);
            remaining -= span;
        }
    }

    /// Bus cycles until the earliest active channel consumes its next source
    /// sample — i.e. how long the current mix is guaranteed to hold.
    ///
    /// `u32::MAX` when nothing is playing, so a silent APU integrates the whole
    /// slice in a single pass; that is both the common case and the cheapest.
    fn apu_cycles_to_next_edge(&self) -> u32 {
        self.apu
            .channels
            .iter()
            .filter(|ch| ch.active)
            .map(|ch| ch.period_cycles().saturating_sub(ch.timer_acc))
            .min()
            .unwrap_or(u32::MAX)
    }

    /// Advance every playing channel by `cycles` bus cycles, decoding one
    /// source sample per timer overflow.
    fn apu_advance_channels(&mut self, cycles: u32) {
        for i in 0..16 {
            if !self.apu.channels[i].active {
                continue;
            }
            // Scratch copy: sample fetches need `&self` bus reads while the
            // channel mutates; NdsChannel is Copy exactly for this.
            let mut ch = self.apu.channels[i];
            ch.timer_acc += cycles;
            let period = ch.period_cycles();
            while ch.timer_acc >= period {
                ch.timer_acc -= period;
                self.apu_step_channel(i, &mut ch);
                if !ch.active {
                    ch.timer_acc = 0;
                    break;
                }
            }
            self.apu.channels[i] = ch;
        }
    }

    /// Decode the next source sample for one channel (PCM8 / PCM16 / ADPCM,
    /// PSG wave/noise on channels 8-15), handling the loop-point decoder
    /// snapshot and end-of-sample repeat/stop semantics. `i` is the hardware
    /// channel number (PSG generators exist only on 8-13 / 14-15).
    fn apu_step_channel(&self, i: usize, ch: &mut crate::nds::apu::NdsChannel) {
        match ch.format() {
            0 => {
                let b = self.read_byte_arm7(ch.sad.wrapping_add(ch.cursor));
                ch.sample = (b as i8 as i16) << 8;
                ch.cursor += 1;
            }
            1 => {
                let a = ch.sad.wrapping_add(ch.cursor);
                ch.sample =
                    i16::from_le_bytes([self.read_byte_arm7(a), self.read_byte_arm7(a + 1)]);
                ch.cursor += 2;
            }
            2 => {
                let b = self.read_byte_arm7(ch.sad.wrapping_add(ch.cursor));
                let nib = if ch.adpcm_high {
                    ch.cursor += 1;
                    b >> 4
                } else {
                    b & 0xF
                };
                ch.adpcm_high = !ch.adpcm_high;
                ch.adpcm_decode(nib);
                // Arriving at the loop point (cursor just advanced onto it):
                // snapshot the decoder — hardware resumes from THIS state on
                // every wrap, it does not re-read the header.
                if ch.cursor == ch.loop_start_bytes() && !ch.adpcm_high {
                    ch.adpcm_loop_val = ch.adpcm_val;
                    ch.adpcm_loop_idx = ch.adpcm_idx;
                }
            }
            _ => {
                // PSG (format 3): generated, not fetched — no source cursor
                // and no LEN end-of-sample; it runs until software clears the
                // start bit, so the repeat/stop logic below never applies.
                ch.psg_step(i);
                return;
            }
        }
        if ch.cursor >= ch.total_bytes() {
            match ch.repeat_mode() {
                1 | 3 => {
                    ch.cursor = ch.loop_start_bytes();
                    if ch.format() == 2 {
                        ch.adpcm_val = ch.adpcm_loop_val;
                        ch.adpcm_idx = ch.adpcm_loop_idx;
                        ch.adpcm_high = false;
                    }
                }
                _ => {
                    // Manual/one-shot end: stop and clear the start bit so
                    // busy reads back 0.
                    ch.active = false;
                    ch.cnt &= !(1 << 31);
                    if ch.cnt & (1 << 15) == 0 {
                        ch.sample = 0;
                    }
                    // ponytail: CNT bit 15 (Hold) is decoded above and the
                    // final sample kept in `ch.sample`, but `NdsApu::mix`
                    // skips every channel with `active == false`, so a held
                    // level cannot reach the output — the channel drops out
                    // either way. Ceiling: the end-of-sample step hardware
                    // would have avoided. Measured on the SoulSilver overworld
                    // that step never exceeded 8192 of full scale across 4 s
                    // (0 clicks), because the samples end near zero, so this
                    // stays unfixed for want of evidence. Upgrade path: add a
                    // `held` flag to the channel and let `mix` include held
                    // channels at their final sample.
                }
            }
        }
    }

    /// Perform an **immediate-timing** DMA transfer for one channel and report
    /// whether it ran. Returns `false` (no transfer) when the channel is not
    /// enabled (CNT bit 31 clear) or when its start-timing field selects a
    /// non-immediate trigger (VBlank/HBlank/etc.) — those are left armed for a
    /// future event scheduler and, crucially, must never block boot, which drives
    /// its copies with immediate DMA and busy-waits on the enable bit.
    ///
    /// Honors: transfer width (CNT bit 26: 1 = 32-bit word, 0 = 16-bit halfword);
    /// destination address control (bits 22-21) and source address control (bits
    /// 24-23), each 0 = increment, 1 = decrement, 2 = fixed, 3 = increment (dst
    /// reload for repeat — treated as plain increment for a one-shot). `count` is
    /// masked to the channel's width and `0` means the channel maximum. All memory
    /// access routes through the checked `*_arm9`/`*_arm7` accessors, so a rogue
    /// address can never read or write out of bounds (it wraps within the target
    /// region) — the only cost of a pathological `count` is time, bounded by
    /// `count_max`.
    fn run_immediate_dma(
        &mut self,
        sad: u32,
        dad: u32,
        cnt: u32,
        count_mask: u32,
        count_max: u32,
        arm9: bool,
    ) -> bool {
        if cnt & 0x8000_0000 == 0 {
            return false; // channel disabled
        }
        let timing = (cnt >> 27) & 0x7;
        let core = usize::from(!arm9);
        self.dma_armed[core][timing as usize] += 1;
        let requested = cnt & count_mask;
        self.dma_max_count[core] = self.dma_max_count[core].max(requested);
        // Timing 0 = immediate. Timing 7 on the ARM9 = Geometry Command FIFO:
        // games feed 3D display lists to 0x04000400 with it. Our HLE FIFO
        // consumes instantly and always reads below-half-full, so hardware
        // would start this transfer at once and re-trigger until the list is
        // done — equivalent to running it to completion right here. Other
        // timings (VBlank/HBlank/...) stay armed-not-run.
        if timing != 0 && !(arm9 && timing == 7) {
            return false;
        }
        let word = (cnt & 0x0400_0000) != 0; // bit26: 1 = 32-bit
        let dst_ctl = (cnt >> 21) & 0x3;
        let src_ctl = (cnt >> 23) & 0x3;
        let mut count = cnt & count_mask;
        if count == 0 {
            count = count_max;
        }
        let step = if word { 4u32 } else { 2u32 };
        let mut src = sad & 0x0FFF_FFFF;
        let mut dst = dad & 0x0FFF_FFFF;
        for _ in 0..count {
            if word {
                let v = if arm9 { self.read_word_arm9(src) } else { self.read_word_arm7(src) };
                if arm9 { self.write_word_arm9(dst, v) } else { self.write_word_arm7(dst, v) }
            } else {
                let v = if arm9 { self.read_halfword_arm9(src) } else { self.read_halfword_arm7(src) };
                if arm9 { self.write_halfword_arm9(dst, v) } else { self.write_halfword_arm7(dst, v) }
            }
            src = match src_ctl {
                1 => src.wrapping_sub(step),
                2 => src,
                _ => src.wrapping_add(step),
            };
            dst = match dst_ctl {
                1 => dst.wrapping_sub(step),
                2 => dst,
                _ => dst.wrapping_add(step),
            };
        }
        self.dma_fired[core][timing as usize] += 1;
        self.dma_units[core][timing as usize] += count as u64;
        // Display lists reach the geometry engine either by CPU copy or by DMA
        // into the GXFIFO window; the SDK picks per its "DL DMA channel" global.
        // Counting the DMA route separately is the only way to tell "the game
        // never sent that list" from "it sent it down a path we drop".
        if (0x0400_0400..0x0400_0440).contains(&(dad & 0x0FFF_FFFF)) {
            self.dma_gxfifo_transfers = self.dma_gxfifo_transfers.wrapping_add(1);
            self.dma_gxfifo_words = self.dma_gxfifo_words.wrapping_add(u64::from(count));
        }
        true
    }

    /// Start ARM9 DMA channel `ch` (0-3) if it was just enabled with immediate
    /// timing, then clear its enable bit so the game's `tst #0x80000000` completion
    /// poll terminates, and raise the DMA IRQ if requested (CNT bit 30). ARM9 DMA
    /// length is 21 bits; `0` means the maximum (0x200000).
    fn maybe_run_dma_arm9(&mut self, ch: usize) {
        let base = 0xB0 + ch * 0x0C;
        let sad = u32::from_le_bytes(self.arm9_io[base..base + 4].try_into().unwrap());
        let dad = u32::from_le_bytes(self.arm9_io[base + 4..base + 8].try_into().unwrap());
        let cnt = u32::from_le_bytes(self.arm9_io[base + 8..base + 12].try_into().unwrap());
        if self.run_immediate_dma(sad, dad, cnt, 0x001F_FFFF, 0x0020_0000, true) {
            let done = cnt & !0x8000_0000;
            self.arm9_io[base + 8..base + 12].copy_from_slice(&done.to_le_bytes());
            if cnt & 0x4000_0000 != 0 {
                self.trigger_interrupt_arm9(1 << (8 + ch)); // DMA0-3 IRQ = bits 8-11
            }
        }
    }

    /// ARM7 counterpart of [`Self::maybe_run_dma_arm9`]. ARM7 lengths are narrower:
    /// channels 0-2 are 14-bit (max 0x4000), channel 3 is 16-bit (max 0x10000).
    fn maybe_run_dma_arm7(&mut self, ch: usize) {
        let base = 0xB0 + ch * 0x0C;
        let sad = u32::from_le_bytes(self.arm7_io[base..base + 4].try_into().unwrap());
        let dad = u32::from_le_bytes(self.arm7_io[base + 4..base + 8].try_into().unwrap());
        let cnt = u32::from_le_bytes(self.arm7_io[base + 8..base + 12].try_into().unwrap());
        let (mask, max) = if ch == 3 { (0xFFFF, 0x1_0000) } else { (0x3FFF, 0x4000) };
        if self.run_immediate_dma(sad, dad, cnt, mask, max, false) {
            let done = cnt & !0x8000_0000;
            self.arm7_io[base + 8..base + 12].copy_from_slice(&done.to_le_bytes());
            if cnt & 0x4000_0000 != 0 {
                self.trigger_interrupt_arm7(1 << (8 + ch));
            }
        }
    }

    /// NDS-Slot (Gamecard) DMA, start timing 5. Real hardware moves one
    /// `count`-word burst from the cart data port per ready word; our Gamecard
    /// model is synchronous (the whole block is available the moment it
    /// starts), so pump the entire remaining block through the first armed
    /// slot-timing channel as soon as either side arms — block start (ROMCTRL
    /// bit 31) or channel enable. SoulSilver's FS layer switches from PIO
    /// polling to exactly this (DMA1 = 0xAF000001: enable, repeat, 32-bit,
    /// fixed source) after early boot; without it the read never completes and
    /// the game idles forever. Repeat-mode channels stay armed for the next
    /// block; one-shot channels clear enable and raise their DMA IRQ (bit 30).
    /// ponytail: whole-block pump, not per-word pacing — with an instant
    /// gamecard the difference is unobservable.
    fn service_gamecard_dma_arm9(&mut self) {
        if self.gamecard.bytes_left == 0 {
            return;
        }
        // Only main data reads (cmd 0xB7, fill = None) go through the DMA.
        // The card-removal watchdog PIO-polls 4-byte chip-ID blocks BETWEEN
        // read requests; on real hardware the CARD mutex + per-request channel
        // disable keep those away from an armed channel, but our repeat-mode
        // channel stays armed across requests — pumping a chip-ID block here
        // would inject 4 bytes into the stream and shift the dest cursor,
        // scrambling every later page (seen as garbled overlay data).
        if self.gamecard.fill.is_some() {
            return;
        }
        for ch in 0..4usize {
            let base = 0xB0 + ch * 0x0C;
            let cnt = u32::from_le_bytes(self.arm9_io[base + 8..base + 12].try_into().unwrap());
            if cnt & 0x8000_0000 == 0 || (cnt >> 27) & 0x7 != 5 {
                continue;
            }
            if (cnt & 0x0400_0000) == 0 {
                return; // halfword cart DMA: no real game does this; leave armed
            }
            let sad = u32::from_le_bytes(self.arm9_io[base..base + 4].try_into().unwrap())
                & 0x0FFF_FFFF;
            let dst_ctl = (cnt >> 21) & 0x3;
            // Internal cursor (latched on enable), NOT the DAD register: a
            // multi-block FS stream keeps one channel enabled across many
            // ROMCTRL blocks and the destination must keep advancing.
            let mut dst = self.dma9_internal_dst[ch];
            let mut guard = 0x4000u32; // > max block (0x4000 bytes) in words
            while self.gamecard.bytes_left > 0 && guard > 0 {
                guard -= 1;
                // The data port is a side-effecting read (advances the block
                // cursor) — it must go through gamecard_read_data, not the
                // plain bus read.
                let v = if sad == 0x0410_0010 {
                    self.gamecard_read_data()
                } else {
                    self.read_word_arm9(sad)
                };
                self.write_word_arm9(dst, v);
                dst = match dst_ctl {
                    1 => dst.wrapping_sub(4),
                    2 => dst,
                    _ => dst.wrapping_add(4),
                };
            }
            self.dma9_internal_dst[ch] = dst;
            // Count it in the same census as every other DMA. Without this the
            // slot timing reported `armed=51 fired=0`, which reads exactly like
            // the "channel the game waits on forever" case the census exists to
            // detect — while the transfers were in fact running, here.
            self.dma_fired[0][5] += 1;
            self.dma_units[0][5] += u64::from(0x4000u32 - guard);
            if cnt & (1 << 25) == 0 {
                let done = cnt & !0x8000_0000;
                self.arm9_io[base + 8..base + 12].copy_from_slice(&done.to_le_bytes());
                if cnt & 0x4000_0000 != 0 {
                    self.trigger_interrupt_arm9(1 << (8 + ch));
                }
            }
            return; // one channel services the slot
        }
    }

    /// The Gamecard chip ID reported for commands 0x90/0xB8. Byte 0 is the
    /// manufacturer (0xC2 = Macronix, as on retail carts); bits 8-14 encode the ROM
    /// size the same way real cartridges do (`(size>>20)-1` up to 128MB). Games read
    /// this during init and some sanity-check it, so a plausible value matters.
    fn gamecard_chip_id(&self) -> u32 {
        Self::gamecard_chip_id_for_len(self.rom.len())
    }

    /// Chip ID for a ROM of `len` bytes. Shared by the Gamecard command response
    /// above and the HLE boot's firmware boot-info writes (`hle::boot_load_rom`) —
    /// they MUST agree: NitroSDK's card-removal watchdog re-reads the ID over the
    /// Gamecard bus and compares it against the boot-info copy in main RAM,
    /// terminating the game on any mismatch.
    pub(crate) fn gamecard_chip_id_for_len(rom_len: usize) -> u32 {
        let mut id = 0x0000_00C2u32; // Macronix
        let len = rom_len as u32;
        if (0x0010_0000..=0x0800_0000).contains(&len) {
            id |= (len >> 20).wrapping_sub(1) << 8;
        } else if len > 0x0800_0000 {
            id |= 0x100u32.wrapping_sub(len >> 28) << 8;
        }
        id
    }

    /// Begin a Gamecard block transfer. Called on the ROMCTRL block-start write
    /// (bit 31). Decodes the 8-byte command latched at `0x040001A8` and the block
    /// size from ROMCTRL bits 26-24, then arms the cursor and raises word-ready.
    /// A zero-length block completes immediately (busy cleared) — see
    /// [`GamecardState`] for the full protocol. ARM9-owned cart only for now; the
    /// ARM7 path (save/backup access) is not wired yet.
    fn start_gamecard_block(&mut self) {
        let romctrl = u32::from_le_bytes(self.arm9_io[0x1A4..0x1A8].try_into().unwrap());
        let cmd = &self.arm9_io[0x1A8..0x1B0];
        // Block size: field 0 = none, 7 = 4 bytes, else 0x100 << field.
        let block = match (romctrl >> 24) & 0x7 {
            0 => 0,
            7 => 4,
            n => 0x100u32 << n,
        };
        // Decode the command byte. 0xB7 = main data read (address = big-endian
        // cmd[1..5]); 0x90/0xB8 = get chip ID; anything else = idle-bus fill.
        let (src, fill) = match cmd[0] {
            0xB7 => (u32::from_be_bytes([cmd[1], cmd[2], cmd[3], cmd[4]]), None),
            0x90 | 0xB8 => (0, Some(self.gamecard_chip_id())),
            _ => (0, Some(0xFFFF_FFFF)),
        };
        self.gamecard.src = src;
        self.gamecard.bytes_left = block;
        self.gamecard.fill = fill;

        let mut rc = romctrl;
        if block == 0 {
            rc &= !(1 << 31); // no data: transfer already complete
            rc &= !(1 << 23);
        } else {
            rc |= 1 << 23; // data-word status = ready (kept high while data remains)
        }
        self.arm9_io[0x1A4..0x1A8].copy_from_slice(&rc.to_le_bytes());
    }

    /// Deliver the next 32-bit word of the active Gamecard block from `rom`
    /// (little-endian; over-reads and non-data commands yield the `0xFFFFFFFF`
    /// idle-bus value). When the block drains, clears ROMCTRL busy/word-ready and
    /// raises the transfer-complete IRQ if AUXSPICNT bit 14 enabled it. Returns 0
    /// when no transfer is active.
    pub fn gamecard_read_data(&mut self) -> u32 {
        if self.gamecard.bytes_left == 0 {
            return 0;
        }
        let word = match self.gamecard.fill {
            Some(w) => w,
            None => {
                let a = self.gamecard.src as usize;
                u32::from_le_bytes([
                    self.rom.get(a).copied().unwrap_or(0xFF),
                    self.rom.get(a + 1).copied().unwrap_or(0xFF),
                    self.rom.get(a + 2).copied().unwrap_or(0xFF),
                    self.rom.get(a + 3).copied().unwrap_or(0xFF),
                ])
            }
        };
        self.gamecard.src = self.gamecard.src.wrapping_add(4);
        self.gamecard.bytes_left = self.gamecard.bytes_left.saturating_sub(4);

        if self.gamecard.bytes_left == 0 {
            // Block complete: clear busy (bit 31) and word-ready (bit 23).
            let mut rc = u32::from_le_bytes(self.arm9_io[0x1A4..0x1A8].try_into().unwrap());
            rc &= !((1 << 31) | (1 << 23));
            self.arm9_io[0x1A4..0x1A8].copy_from_slice(&rc.to_le_bytes());
            // Transfer-complete IRQ (IF bit 19) if AUXSPICNT (0x040001A0) bit 14 set.
            let auxspicnt = u16::from_le_bytes([self.arm9_io[0x1A0], self.arm9_io[0x1A1]]);
            if auxspicnt & (1 << 14) != 0 {
                self.trigger_interrupt_arm9(1 << 19);
            }
        }
        word
    }

    pub fn set_vcount(&mut self, val: u16) {
        self.arm9_io[6] = val as u8;
        self.arm9_io[7] = (val >> 8) as u8;
        self.arm7_io[6] = val as u8;
        self.arm7_io[7] = (val >> 8) as u8;
    }

    // --- IPC IPCSYNC Registers ---
    pub fn read_ipcsync_arm9(&self) -> u16 {
        let sync = (self.ipc.arm7_to_arm9_sync & 0xF) as u16
            | (((self.ipc.arm9_to_arm7_sync & 0xF) as u16) << 8);
        let irq_bit = if self.ipc.arm9_sync_irq_enable { 1 << 14 } else { 0 };
        sync | irq_bit
    }

    pub fn write_ipcsync_arm9(&mut self, val: u16) {
        self.ipc_yield = true; // see `NdsMmu::ipc_yield`
        self.ipc.arm9_to_arm7_sync = ((val >> 8) & 0xF) as u8;
        self.ipc.arm9_sync_irq_enable = (val & (1 << 14)) != 0;

        // If Send IRQ bit (bit 13 or bit 14? The prompt says: "If ARM7 has enabled interrupts on sync write, trigger ARM7 IRQ").
        // Bit 14 of sync is "Enable IRQ".
        // What triggers the interrupt? Writing to IPCSYNC has a "Send IRQ" trigger (bit 13 or writing it triggers it).
        // Let's check: "Writing a 1 triggers an IPC_SYNC interrupt on the other CPU (if enabled on the destination)."
        // Usually, bit 13 of IPCSYNC is the Send IRQ bit. When written with 1, it triggers partner CPU interrupt.
        if (val & (1 << 13)) != 0 {
            if self.ipc.arm7_sync_irq_enable {
                self.trigger_interrupt_arm7(1 << 16); // IPC Sync IRQ is bit 16
            }
        }
    }

    pub fn read_ipcsync_arm7(&self) -> u16 {
        let sync = (self.ipc.arm9_to_arm7_sync & 0xF) as u16
            | (((self.ipc.arm7_to_arm9_sync & 0xF) as u16) << 8);
        let irq_bit = if self.ipc.arm7_sync_irq_enable { 1 << 14 } else { 0 };
        sync | irq_bit
    }

    pub fn write_ipcsync_arm7(&mut self, val: u16) {
        self.ipc_yield = true; // see `NdsMmu::ipc_yield`
        self.ipc.arm7_to_arm9_sync = ((val >> 8) & 0xF) as u8;
        self.ipc.arm7_sync_irq_enable = (val & (1 << 14)) != 0;

        if (val & (1 << 13)) != 0 {
            if self.ipc.arm9_sync_irq_enable {
                self.trigger_interrupt_arm9(1 << 16);
            }
        }
    }

    // --- IPC FIFOs Registers ---
    pub fn read_ipc_fifo_cnt_arm9(&self) -> u16 {
        let mut cnt = self.ipc.fifo_control_arm9 & 0xC404; // Keep writable bits: 2, 10, 14, 15
        if self.ipc.fifo_9to7.is_empty() { cnt |= 1 << 0; }
        if self.ipc.fifo_9to7.len() >= 16 { cnt |= 1 << 1; }
        if self.ipc.fifo_7to9.is_empty() { cnt |= 1 << 8; }
        if self.ipc.fifo_7to9.len() >= 16 { cnt |= 1 << 9; }
        cnt
    }

    /// IPCFIFOCNT is per-CPU and owns only THIS core's send FIFO.
    ///
    /// Clearing bit 15 used to flush both directions. The receive FIFO is the
    /// *other* core's send FIFO, so that was a cross-core write hardware cannot
    /// perform — and it was reachable by accident, not just by a deliberate
    /// disable: `write_halfword` splits a `STRH` into two byte writes, and the
    /// low-byte read-modify-write composes a value whose bit 15 is whatever the
    /// register happened to hold. A core that had not yet enabled its FIFO
    /// therefore destroyed the words its partner had already queued the moment
    /// it wrote the low half of its own enable. Bit 15 = 0 only gates sends and
    /// receives on hardware; it does not flush anything. The documented flush is
    /// bit 3, and it clears the send FIFO alone — which is what this code
    /// already did on the enabled path, an internal contradiction with the
    /// disabled one.
    pub fn write_ipc_fifo_cnt_arm9(&mut self, val: u16) {
        // Writable bits are updated on EVERY write, including a disable.
        // The old code returned early when bit 15 was clear, so disabling
        // left the send-empty and receive-not-empty IRQ enables (bits 2
        // and 10) at their previous values.
        self.ipc.fifo_control_arm9 = (self.ipc.fifo_control_arm9 & !0x8404) | (val & 0x8404);
        // Bit 3 flushes the send FIFO, and disabling the FIFOs empties it
        // too — but ONLY this core's send FIFO. `fifo_7to9` is the other
        // core's send queue, and clearing it (which the disable path used
        // to do) is a cross-core write no hardware can perform.
        if (val & (1 << 3)) != 0 || (val & (1 << 15)) == 0 {
            self.ipc.fifo_9to7.clear();
        }
        // Bit 14 is the error latch: acknowledged by writing a 1, and also
        // reset when the FIFOs are disabled.
        if (val & (1 << 14)) != 0 || (val & (1 << 15)) == 0 {
            self.ipc.fifo_control_arm9 &= !(1 << 14);
        }
    }

    pub fn read_ipc_fifo_cnt_arm7(&self) -> u16 {
        let mut cnt = self.ipc.fifo_control_arm7 & 0xC404;
        if self.ipc.fifo_7to9.is_empty() { cnt |= 1 << 0; }
        if self.ipc.fifo_7to9.len() >= 16 { cnt |= 1 << 1; }
        if self.ipc.fifo_9to7.is_empty() { cnt |= 1 << 8; }
        if self.ipc.fifo_9to7.len() >= 16 { cnt |= 1 << 9; }
        cnt
    }

    /// ARM7 twin of [`Self::write_ipc_fifo_cnt_arm9`]; same rule, same reason.
    pub fn write_ipc_fifo_cnt_arm7(&mut self, val: u16) {
        // Writable bits are updated on EVERY write, including a disable.
        // The old code returned early when bit 15 was clear, so disabling
        // left the send-empty and receive-not-empty IRQ enables (bits 2
        // and 10) at their previous values.
        self.ipc.fifo_control_arm7 = (self.ipc.fifo_control_arm7 & !0x8404) | (val & 0x8404);
        // Bit 3 flushes the send FIFO, and disabling the FIFOs empties it
        // too — but ONLY this core's send FIFO. `fifo_9to7` is the other
        // core's send queue, and clearing it (which the disable path used
        // to do) is a cross-core write no hardware can perform.
        if (val & (1 << 3)) != 0 || (val & (1 << 15)) == 0 {
            self.ipc.fifo_7to9.clear();
        }
        // Bit 14 is the error latch: acknowledged by writing a 1, and also
        // reset when the FIFOs are disabled.
        if (val & (1 << 14)) != 0 || (val & (1 << 15)) == 0 {
            self.ipc.fifo_control_arm7 &= !(1 << 14);
        }
    }

    pub fn write_ipc_fifo_tx_arm9(&mut self, val: u32) {
        if self.fifo_log_on && self.fifo_log.len() < 512 {
            self.fifo_log.push((9, val));
        }
        if val & 0x1F == 7 {
            self.tag7_sent = self.tag7_sent.wrapping_add(1);
            self.last_tag7_data = val >> 6;
        }
        self.note_tag11(9, val);
        if (self.ipc.fifo_control_arm9 & (1 << 15)) == 0 {
            return;
        }
        if self.ipc.fifo_9to7.len() >= 16 {
            self.ipc.fifo_control_arm9 |= 1 << 14;
            if val & 0x1F == 7 {
                self.tag7_dropped_full = self.tag7_dropped_full.wrapping_add(1);
            }
            if self.fifo_log_on && self.fifo_log.len() < 512 {
                self.fifo_log.push((0xF, val)); // dropped: 9->7 FIFO full
            }
            return;
        }
        let was_empty = self.ipc.fifo_9to7.is_empty();
        self.ipc.fifo_9to7.push(val);

        if was_empty && (self.ipc.fifo_control_arm7 & (1 << 10)) != 0 {
            // ARM9 pushed -> ARM7's receive FIFO became non-empty: raise the ARM7
            // Recv-FIFO-Not-Empty IRQ (bit 18, NOT 17 = Send-Empty).
            self.trigger_interrupt_arm7(1 << 18);
        }
    }

    /// Record one PXI word on the backup command channel (tag 0x0B). `dir` is
    /// the sending core. The payload sits above the 5-bit tag and a status bit,
    /// matching `PXI_SendWordByFifo`.
    fn note_tag11(&mut self, dir: u8, val: u32) {
        if val & 0x1F != 0x0B {
            return;
        }
        let data = val >> 6;
        if dir == 9 {
            if data < 16 {
                self.tag11_9to7[data as usize] += 1;
                // Command 0 is the only two-word message; its payload word
                // carries the shared-argument pointer.
                if data == 0 {
                    self.tag11_expect_ptr = true;
                } else if self.tag11_argptr != 0 && self.tag11_args.len() < 12 {
                    let mut buf = [0u8; 0x60];
                    for (i, b) in buf.iter_mut().enumerate() {
                        *b = self.read_byte_arm9(self.tag11_argptr.wrapping_add(i as u32));
                    }
                    self.tag11_args.push((data, buf));
                }
            } else {
                self.tag11_9to7_wide += 1;
                if self.tag11_expect_ptr {
                    self.tag11_argptr = data;
                    self.tag11_expect_ptr = false;
                }
            }
        } else {
            self.tag11_7to9 = self.tag11_7to9.saturating_add(1);
        }
        if self.tag11_head.len() < 64 {
            self.tag11_head.push((dir, val));
        }
    }

    pub fn read_ipc_fifo_rx_arm9(&mut self) -> u32 {
        if (self.ipc.fifo_control_arm9 & (1 << 15)) == 0 {
            return 0;
        }
        if self.ipc.fifo_7to9.is_empty() {
            self.ipc.fifo_control_arm9 |= 1 << 14;
            return 0;
        }
        let val = self.ipc.fifo_7to9.remove(0);
        if self.fifo_log_on && self.fifo_log.len() < 512 {
            self.fifo_log.push((3, val)); // ARM9 popped from its RX FIFO
        }
        let now_empty = self.ipc.fifo_7to9.is_empty();

        if now_empty && (self.ipc.fifo_control_arm7 & (1 << 2)) != 0 {
            // ARM9 drained the ARM7->ARM9 FIFO: raise the ARM7 Send-FIFO-Empty IRQ
            // (bit 17, NOT 18 = Recv-Not-Empty).
            self.trigger_interrupt_arm7(1 << 17);
        }
        val
    }

    pub fn write_ipc_fifo_tx_arm7(&mut self, val: u32) {
        self.note_tag11(7, val);
        if self.fifo_log_on && self.fifo_log.len() < 512 {
            self.fifo_log.push((7, val));
        }
        if (self.ipc.fifo_control_arm7 & (1 << 15)) == 0 {
            return;
        }
        if self.ipc.fifo_7to9.len() >= 16 {
            self.ipc.fifo_control_arm7 |= 1 << 14;
            return;
        }
        let was_empty = self.ipc.fifo_7to9.is_empty();
        self.ipc.fifo_7to9.push(val);

        if was_empty && (self.ipc.fifo_control_arm9 & (1 << 10)) != 0 {
            // ARM7 pushed -> ARM9's receive FIFO became non-empty: raise the ARM9
            // Recv-FIFO-Not-Empty IRQ (bit 18, NOT 17 = Send-Empty).
            self.trigger_interrupt_arm9(1 << 18);
        }
    }

    pub fn read_ipc_fifo_rx_arm7(&mut self) -> u32 {
        if (self.ipc.fifo_control_arm7 & (1 << 15)) == 0 {
            return 0;
        }
        if self.ipc.fifo_9to7.is_empty() {
            self.ipc.fifo_control_arm7 |= 1 << 14;
            return 0;
        }
        let val = self.ipc.fifo_9to7.remove(0);
        if val & 0x1F == 7 {
            self.tag7_rx7 = self.tag7_rx7.wrapping_add(1);
        }
        if self.fifo_log_on && self.fifo_log.len() < 512 {
            self.fifo_log.push((1, val)); // ARM7 popped from its RX FIFO
        }
        let now_empty = self.ipc.fifo_9to7.is_empty();

        if now_empty && (self.ipc.fifo_control_arm9 & (1 << 2)) != 0 {
            // ARM7 drained the ARM9->ARM7 FIFO: raise the ARM9 Send-FIFO-Empty IRQ
            // (bit 17, NOT 18 = Recv-Not-Empty).
            self.trigger_interrupt_arm9(1 << 17);
        }
        val
    }

    // --- Read/Write CPU-specific Memory Space ---
    pub fn read_byte_arm9(&self, addr: u32) -> u8 {
        if self.in_itcm_arm9(addr) {
            let offset = addr.wrapping_sub(self.itcm_base());
            return self.itcm[(offset as usize) % self.itcm.len()];
        }
        if self.in_dtcm_arm9(addr) {
            let offset = addr.wrapping_sub(self.dtcm_base());
            return self.dtcm[(offset as usize) % self.dtcm.len()];
        }

        match (addr >> 24) & 0xFF {
            0x00 => self.arm9_bios[((addr & 0x00FF_FFFF) % 0x4000) as usize],
            // High exception vectors (CP15 control bit 13). The game relocates its
            // vectors to 0xFFFF0000 and takes IRQs at 0xFFFF0018, so mirror the ARM9
            // BIOS (with its HLE IRQ handler at 0x18) here as well as at 0x00000000.
            0xFF => self.arm9_bios[(addr & 0x3FFF) as usize],
            // ponytail: the ARM9's shared-WRAM window is modelled at 0x02400000
            // rather than at 0x03000000, and there is no 0x03 arm on this core at
            // all -- an ARM9 access there reads 0 and a store is dropped. GBATEK
            // puts shared WRAM at 0x03000000 for the ARM9 and makes
            // 0x02400000-0x0247FFFF an ordinary main-RAM mirror, so both halves
            // of this are wrong against hardware. Ceiling: a game that uses the
            // documented window gets zeros, and one that reaches main RAM through
            // the 0x024xxxxx mirror hits shared WRAM instead. Measured before
            // deciding: over 50 in-game frames of SoulSilver the ARM9 issued
            // **zero** accesses to region 0x03, so this is latent here, and the
            // 0x02400000 base is load-bearing for the WRAMCNT decoders, the
            // read/write fast paths and `test_shared_wram_mappings_all_modes`.
            // Upgrade path: move the window to a 0x03 arm and repoint those
            // three together -- not worth destabilising a working map for a
            // defect no measurement can currently see.
            0x02 => {
                let offset = addr & 0x00FF_FFFF;
                if offset < 4 * 1024 * 1024 {
                    self.main_ram[offset as usize]
                } else if offset >= 0x400000 && offset < 0x480000 {
                    self.read_shared_wram_arm9(offset - 0x400000)
                } else {
                    // Mirrored main ram fallback
                    self.main_ram[(offset % (4 * 1024 * 1024)) as usize]
                }
            }
            0x04 => {
                let offset = addr & 0x00FF_FFFF;
                match offset {
                    o if (0x000100..0x000110).contains(&o) => self.timers9.read_byte(o - 0x100),
                    // AUXSPI: the cartridge backup chip, shared with the ARM7
                    // (which is the core that actually drives it).
                    0x0001A0..=0x0001A3 => self.read_auxspi_byte(true, offset),
                    // ARM9 maths block reads: busy (bit15) never set — the
                    // result is ready the moment the operands land; DIVCNT
                    // bit14 reflects a zero denominator.
                    0x000280 => self.div_cnt as u8,
                    0x000281 => {
                        ((self.div_cnt >> 8) as u8 & 0x3F)
                            | if self.div_denom == 0 { 0x40 } else { 0 }
                    }
                    o if (0x000290..0x000298).contains(&o) => {
                        (self.div_numer >> ((o - 0x290) * 8)) as u8
                    }
                    o if (0x000298..0x0002A0).contains(&o) => {
                        (self.div_denom >> ((o - 0x298) * 8)) as u8
                    }
                    o if (0x0002A0..0x0002A8).contains(&o) => {
                        (self.div_results().0 >> ((o - 0x2A0) * 8)) as u8
                    }
                    o if (0x0002A8..0x0002B0).contains(&o) => {
                        (self.div_results().1 >> ((o - 0x2A8) * 8)) as u8
                    }
                    0x0002B0 => self.sqrt_cnt as u8,
                    0x0002B1 => (self.sqrt_cnt >> 8) as u8 & 0x7F,
                    o if (0x0002B4..0x0002B8).contains(&o) => {
                        (self.sqrt_result() >> ((o - 0x2B4) * 8)) as u8
                    }
                    o if (0x0002B8..0x0002C0).contains(&o) => {
                        (self.sqrt_param >> ((o - 0x2B8) * 8)) as u8
                    }
                    0x000130 => self.get_keyinput() as u8,
                    0x000131 => (self.get_keyinput() >> 8) as u8,
                    0x000180 => self.read_ipcsync_arm9() as u8,
                    0x000181 => (self.read_ipcsync_arm9() >> 8) as u8,
                    0x000184 => self.read_ipc_fifo_cnt_arm9() as u8,
                    0x000185 => (self.read_ipc_fifo_cnt_arm9() >> 8) as u8,
                    0x000240 => self.vram.banks[0].control,
                    0x000241 => self.vram.banks[1].control,
                    0x000242 => self.vram.banks[2].control,
                    0x000243 => self.vram.banks[3].control,
                    0x000244 => self.vram.banks[4].control,
                    0x000245 => self.vram.banks[5].control,
                    0x000246 => self.vram.banks[6].control,
                    0x000247 => self.wram_control,
                    0x000248 => self.vram.banks[7].control,
                    0x000249 => self.vram.banks[8].control,
                    0x000210 => self.arm9_ie as u8,
                    0x000211 => (self.arm9_ie >> 8) as u8,
                    0x000212 => (self.arm9_ie >> 16) as u8,
                    0x000213 => (self.arm9_ie >> 24) as u8,
                    0x000214 => self.arm9_if as u8,
                    0x000215 => (self.arm9_if >> 8) as u8,
                    0x000216 => (self.arm9_if >> 16) as u8,
                    0x000217 => (self.arm9_if >> 24) as u8,
                    // DISPSTAT (0x04000004). NOT 0x204 — that is EXMEMCNT,
                    // and pointing these arms at it made an EXMEMCNT access
                    // read and write DISPSTAT's storage.
                    0x000004 => self.arm9_io[4],
                    0x000005 => self.arm9_io[5],
                    0x000208 => self.arm9_ime as u8,
                    0x000209 => (self.arm9_ime >> 8) as u8,
                    0x00020A => (self.arm9_ime >> 16) as u8,
                    0x00020B => (self.arm9_ime >> 24) as u8,
                    // GXSTAT (3D geometry status) — HLE: GX commands are
                    // consumed instantly, so the FIFO always reads empty
                    // (bit26) and less-than-half-full (bit25), the geometry
                    // engine is never busy (bit27), box/pos/vec tests finish
                    // instantly (bit0), and the matrix stack sits at level 0
                    // with NO error. Bit14 (stack error) MUST read 0: it was
                    // hardwired 1 here and SoulSilver's G3X_Reset polls
                    // "error clear?" after acking with bit15 — it spun on
                    // this byte forever (the pre-title freeze). Only the
                    // FIFO-IRQ mode the game wrote (bits 30-31) reads back.
                    // Bit1 = BOX_TEST result: 1 = box (partly) inside the view
                    // volume. We don't run a real frustum test, so report
                    // "inside" — the overworld box-tests every object (player,
                    // NPCs, furniture) to cull off-screen ones; returning 0
                    // ("all outside") made the game skip drawing ALL of them,
                    // so only the always-drawn map rendered (U34: empty rooms).
                    // ponytail: no real BOX_TEST -> nothing is ever culled;
                    // implement the AABB-vs-frustum test if overdraw matters.
                    // Measured 2026-07-25: the game really does consume this bit.
                    // Answering "outside" instead drops the bedroom from 429 to
                    // 187 triangles per frame, so the objects it gates are being
                    // drawn — the missing overworld character is NOT box-culled.
                    // CLIPMTX_RESULT / VECMTX_RESULT: the geometry engine's
                    // current matrices, read back by games that need the
                    // transform they just built. SoulSilver's overworld reads
                    // the clip matrix and derives an actor's scale from the
                    // magnitude of its rows (VEC_Mag at ARM9 0x020ccf80). While
                    // these read as 0 the scale came out (0,0,0), so MTX_SCALE
                    // collapsed every actor drawn through it to a single point
                    // and the rasterizer then discarded it as back-facing — the
                    // missing overworld characters.
                    //
                    // ponytail: three sibling GX result registers stay
                    // unimplemented and read 0 through the generic IO path —
                    // RAM_COUNT (0x04000604, polygon/vertex RAM usage),
                    // POS_RESULT (0x04000620) and VEC_RESULT (0x04000630).
                    // Ceiling: a game that gates submission on remaining
                    // polygon RAM sees "empty" (permissive, so it submits), and
                    // one that position/vector-tests a point reads zeros.
                    // Measured on SoulSilver: zero POS_TEST/VEC_TEST commands
                    // (0x71/0x72) in a full run, and nothing observed reading
                    // RAM_COUNT. Upgrade path: report `tris.len()` and its
                    // vertex count here, and implement the two test commands
                    // alongside their result registers.
                    o if (0x000640..0x000680).contains(&o) => {
                        let word = self.gx.engine.clipmtx_word(((o - 0x640) / 4) as usize);
                        (word >> (((o - 0x640) % 4) * 8)) as u8
                    }
                    o if (0x000680..0x0006A4).contains(&o) => {
                        let word = self.gx.engine.vecmtx_word(((o - 0x680) / 4) as usize);
                        (word >> (((o - 0x680) % 4) * 8)) as u8
                    }
                    0x000600 => 0x02,
                    0x000601 => 0,
                    0x000602 => 0,
                    0x000603 => 0x06 | (self.arm9_io[0x603] & 0xC0),
                    _ => {
                        if offset < self.arm9_io.len() as u32 {
                            self.arm9_io[offset as usize]
                        } else {
                            0
                        }
                    }
                }
            }
            0x05 => self.palette_ram[(addr & 0xFFF) as usize],
            0x06 => {
                let offset = addr & 0x00FF_FFFF;
                if offset >= 0x800000 && offset <= 0x8A3FFF {
                    self.vram.read_lcdc(offset - 0x800000)
                } else if offset < 0x200000 {
                    self.vram.read_bg_a(offset) // Main BG
                } else if offset >= 0x200000 && offset < 0x400000 {
                    self.vram.read_bg_b(offset - 0x200000) // Sub BG
                } else if offset >= 0x400000 && offset < 0x600000 {
                    self.vram.read_obj_a(offset - 0x400000) // Main OBJ
                } else if offset >= 0x600000 && offset < 0x800000 {
                    self.vram.read_obj_b(offset - 0x600000) // Sub OBJ
                } else {
                    0
                }
            }
            0x07 => self.oam[(addr & 0x7FF) as usize],
            _ => 0,
        }
    }

    pub fn write_byte_arm9(&mut self, addr: u32, val: u8) {
        // U27n: catch whoever writes the user-settings calibration bytes —
        // the boot HLE writes real values, yet the game reads zeros at f36.
        if self.calib_watch_on
            && (0x027F_FCD8..0x027F_FCE4).contains(&addr)
            && self.calib_write_log.len() < 256
        {
            self.calib_write_log.push((addr, val as u32));
        }
        if let Some((lo, hi)) = self.ram_write_watch {
            if (lo..hi).contains(&addr) && self.ram_write_pcs.len() < GX_TEX_WATCH_CAP {
                let (pc, lr) = (self.arm9_exec_pc, self.arm9_exec_lr);
                self.ram_write_pcs.push((addr, val, pc, lr));
            }
        }
        if self.in_itcm_arm9(addr) {
            let offset = addr.wrapping_sub(self.itcm_base());
            let idx = (offset as usize) % self.itcm.len();
            self.itcm[idx] = val;
            self.dirty_itcm(idx, 1);
            return;
        }
        if self.in_dtcm_arm9(addr) {
            let offset = addr.wrapping_sub(self.dtcm_base());
            let idx = (offset as usize) % self.dtcm.len();
            self.dtcm[idx] = val;
            return;
        }

        match (addr >> 24) & 0xFF {
            0x02 => {
                let offset = addr & 0x00FF_FFFF;
                if offset < 4 * 1024 * 1024 {
                    self.main_ram[offset as usize] = val;
                    self.dirty_main_ram(offset as usize, 1);
                } else if offset >= 0x400000 && offset < 0x480000 {
                    self.write_shared_wram_arm9(offset - 0x400000, val);
                } else {
                    let idx = (offset % (4 * 1024 * 1024)) as usize;
                    self.main_ram[idx] = val;
                    self.dirty_main_ram(idx, 1);
                }
            }
            0x04 => {
                let offset = addr & 0x00FF_FFFF;
                match offset {
                    o if (0x000100..0x000110).contains(&o) => {
                        self.timers9.write_byte(o - 0x100, val)
                    }
                    0x000180 => {
                        let cur = self.read_ipcsync_arm9();
                        self.write_ipcsync_arm9((cur & 0xFF00) | val as u16);
                    }
                    0x000181 => {
                        let cur = self.read_ipcsync_arm9();
                        self.write_ipcsync_arm9((cur & 0x00FF) | ((val as u16) << 8));
                    }
                    0x000184 => {
                        // Bit 14 (FIFO error) is acknowledged by writing a 1,
                        // so the read-back half of this RMW must not carry it:
                        // a plain low-byte write would otherwise re-write the
                        // live error bit as a 1 and silently clear an error the
                        // software has not looked at yet.
                        let cur = self.read_ipc_fifo_cnt_arm9() & !(1 << 14);
                        self.write_ipc_fifo_cnt_arm9((cur & 0xFF00) | val as u16);
                    }
                    0x000185 => {
                        let cur = self.read_ipc_fifo_cnt_arm9() & !(1 << 14);
                        self.write_ipc_fifo_cnt_arm9((cur & 0x00FF) | ((val as u16) << 8));
                    }
                    0x000240 => self.vram.banks[0].control = val,
                    0x000241 => self.vram.banks[1].control = val,
                    0x000242 => self.vram.banks[2].control = val,
                    0x000243 => self.vram.banks[3].control = val,
                    0x000244 => self.vram.banks[4].control = val,
                    0x000245 => self.vram.banks[5].control = val,
                    0x000246 => self.vram.banks[6].control = val,
                    0x000247 => {
                        // WRAMCNT remaps what every shared-window address
                        // means; compiled blocks keyed on those addresses are
                        // suspect even though no byte moved. Same rule as the
                        // TCM remap in `set_cp15`.
                        if self.wram_control != val {
                            self.invalidate_all_code();
                        }
                        self.wram_control = val;
                    }
                    0x000248 => self.vram.banks[7].control = val,
                    0x000249 => self.vram.banks[8].control = val,
                    0x000210 => self.arm9_ie = (self.arm9_ie & !0xFF) | val as u32,
                    0x000211 => self.arm9_ie = (self.arm9_ie & !0xFF00) | ((val as u32) << 8),
                    0x000212 => self.arm9_ie = (self.arm9_ie & !0xFF0000) | ((val as u32) << 16),
                    0x000213 => self.arm9_ie = (self.arm9_ie & !0xFF000000) | ((val as u32) << 24),
                    0x000214 => self.arm9_if &= !(val as u32),
                    0x000215 => self.arm9_if &= !((val as u32) << 8),
                    0x000216 => {
                        self.arm9_if &= !((val as u32) << 16);
                        // The GX FIFO IRQ (bit 21) is LEVEL-triggered: while a
                        // nonzero condition is selected it is permanently true
                        // here (FIFO always empty), so an acknowledge re-latches
                        // immediately — GBATEK notes it cannot be cleared while
                        // the condition holds. The SDK silences it by writing
                        // mode 0 to GXSTAT, which read_reg honors.
                        self.maybe_raise_gxfifo_irq();
                    }
                    0x000217 => self.arm9_if &= !((val as u32) << 24),
                    // DISPSTAT (0x04000004), writable bits 3-5 (VBlank/HBlank/
                    // VCount IRQ enables) + bit 7 (LYC bit 8) = 0xB8; bits 0-2
                    // are PPU-owned status. The address was 0x204 = EXMEMCNT, so
                    // every EXMEMCNT write landed here: `strh 0xE880,[0x4000204]`
                    // cleared all three IRQ enables and overwrote the LYC field,
                    // silently killing the ARM9 VBlank interrupt mid-session,
                    // while real DISPSTAT writes fell through to the unmasked
                    // catch-all and could clobber the status bits.
                    0x000004 => self.arm9_io[4] = (self.arm9_io[4] & !0xB8) | (val & 0xB8),
                    0x000005 => self.arm9_io[5] = val,
                    0x000208 => self.arm9_ime = (self.arm9_ime & !0xFF) | val as u32,
                    0x000209 => self.arm9_ime = (self.arm9_ime & !0xFF00) | ((val as u32) << 8),
                    0x00020A => self.arm9_ime = (self.arm9_ime & !0xFF0000) | ((val as u32) << 16),
                    0x00020B => self.arm9_ime = (self.arm9_ime & !0xFF000000) | ((val as u32) << 24),
                    // GX FIFO (0x400-0x43F) + direct command ports through
                    // VEC_TEST (0x5C8): accepted and consumed instantly (no
                    // 3D rasterizer — engine-A BG0 renders transparent).
                    0x000400..=0x0005CB => {
                        self.has_3d_activity = true;
                        if offset < self.arm9_io.len() as u32 {
                            self.arm9_io[offset as usize] = val;
                        }
                        // After any push the FIFO drains back to empty, so a
                        // selected FIFO-IRQ condition immediately re-holds.
                        self.maybe_raise_gxfifo_irq();
                    }
                    // GXSTAT writes: bits 30-31 (FIFO IRQ mode) are the only
                    // stored state; bit15 acks a stack error we never raise.
                    // Selecting a nonzero mode raises the FIFO IRQ at once —
                    // the HLE FIFO is permanently empty, the condition holds.
                    0x000600..=0x000602 => {}
                    0x000603 => {
                        self.arm9_io[0x603] = val & 0xC0;
                        self.maybe_raise_gxfifo_irq();
                    }
                    // DMA channels 0-3 (SAD/DAD/CNT ×4 at 0xB0..0xDF). Store the
                    // byte, then start an immediate DMA when the CNT enable bit
                    // (bit 31 — the high byte of each CNT word) is set. A channel
                    // armed with NDS-slot timing while a Gamecard block is already
                    // open must start draining it right away.
                    0x0000B0..=0x0000DF => {
                        let old = self.arm9_io[offset as usize];
                        self.arm9_io[offset as usize] = val;
                        if (val & 0x80) != 0 {
                            let ch = match offset {
                                0x0000BB => Some(0usize),
                                0x0000C7 => Some(1),
                                0x0000D3 => Some(2),
                                0x0000DF => Some(3),
                                _ => None,
                            };
                            if let Some(ch) = ch {
                                // Hardware starts a DMA only on the enable
                                // 0->1 EDGE. Rewriting CNT of an already-
                                // enabled channel (the SDK's dmaStop clears
                                // the timing bits this way, leaving enable
                                // set) must NOT retrigger: with timing now 0
                                // that spurious "immediate" run read the idle
                                // bus and zeroed the first word of every
                                // Gamecard page SoulSilver streamed.
                                if (old & 0x80) == 0 {
                                    // Latch DAD into the internal dest cursor.
                                    let base = 0xB0 + ch * 0x0C;
                                    self.dma9_internal_dst[ch] = u32::from_le_bytes(
                                        self.arm9_io[base + 4..base + 8].try_into().unwrap(),
                                    ) & 0x0FFF_FFFF;
                                    self.maybe_run_dma_arm9(ch);
                                }
                            }
                            self.service_gamecard_dma_arm9();
                        }
                    }
                    // Gamecard ROMCTRL (0x040001A4). Store the byte; a write to the
                    // high byte (0x1A7) with bit 7 set is the block-start (ROMCTRL
                    // bit 31) — the command at 0x1A8 is already latched by then.
                    // If a DMA channel sits armed with NDS-slot timing, it (not CPU
                    // ARM9 maths block writes: divider + sqrt operands.
                    // Results materialize on read; no busy latency.
                    0x000280 => self.div_cnt = (self.div_cnt & 0xFF00) | val as u16,
                    0x000281 => self.div_cnt = (self.div_cnt & 0x00FF) | ((val as u16) << 8),
                    o if (0x000290..0x000298).contains(&o) => {
                        let sh = (o - 0x290) * 8;
                        self.div_numer =
                            (self.div_numer & !(0xFFi64 << sh)) | ((val as i64) << sh);
                    }
                    o if (0x000298..0x0002A0).contains(&o) => {
                        let sh = (o - 0x298) * 8;
                        self.div_denom =
                            (self.div_denom & !(0xFFi64 << sh)) | ((val as i64) << sh);
                    }
                    0x0002B0 => self.sqrt_cnt = (self.sqrt_cnt & 0xFF00) | val as u16,
                    0x0002B1 => self.sqrt_cnt = (self.sqrt_cnt & 0x00FF) | ((val as u16) << 8),
                    o if (0x0002B8..0x0002C0).contains(&o) => {
                        let sh = (o - 0x2B8) * 8;
                        self.sqrt_param =
                            (self.sqrt_param & !(0xFFu64 << sh)) | ((val as u64) << sh);
                    }
                    // AUXSPI writes: logged while the boot watch is armed
                    // (save-chip commands); stored as before. Also mirrored into
                    // the unconditional W0.1 census with bit 7 of `reg` set so
                    // ARM9 traffic is distinguishable from the ARM7's.
                    0x0001A0..=0x0001A3 => {
                        if offset == 0x0001A2 && self.calib_watch_on && self.aux_log.len() < 64 {
                            self.aux_log.push((0, val));
                        }
                        self.note_aux_write(true, offset, val);
                        self.write_auxspi_byte(true, offset, val);
                    }
                    // PIO) moves the block.
                    0x0001A4..=0x0001A7 => {
                        self.arm9_io[offset as usize] = val;
                        if offset == 0x0001A7 && (val & 0x80) != 0 {
                            self.start_gamecard_block();
                            self.service_gamecard_dma_arm9();
                        }
                    }
                    _ => {
                        if offset < self.arm9_io.len() as u32 {
                            self.arm9_io[offset as usize] = val;
                        }
                    }
                }
            }
            0x05 => {
                self.palette_ram[(addr & 0xFFF) as usize] = val;
            }
            // (main RAM is handled above; the watch is applied there)
            0x06 => {
                let offset = addr & 0x00FF_FFFF;
                // Census: a frame that uploads no VRAM at all cannot be
                // animating anything (character cells, sprite tiles). Counted
                // here, at the entry point, so "never attempted" is
                // distinguishable from "attempted and dropped by the bank
                // mapping below".
                self.vram_writes = self.vram_writes.wrapping_add(1);
                if offset >= 0x800000 && offset <= 0x8A3FFF {
                    self.vram.write_lcdc(offset - 0x800000, val);
                } else if offset < 0x200000 {
                    self.vram.write_bg_a(offset, val);
                } else if offset >= 0x200000 && offset < 0x400000 {
                    self.vram.write_bg_b(offset - 0x200000, val);
                } else if offset >= 0x400000 && offset < 0x600000 {
                    // Per-bank OBJ windows: a flat MST-2 write here also
                    // landed in bank I (engine B's OBJ bank) at the same
                    // offset, garbling engine B's sprites with engine A data.
                    self.vram.write_obj_a(offset - 0x400000, val);
                } else if offset >= 0x600000 && offset < 0x800000 {
                    self.vram.write_obj_b(offset - 0x600000, val);
                }
            }
            0x07 => {
                self.oam_writes = self.oam_writes.wrapping_add(1);
                self.oam[(addr & 0x7FF) as usize] = val;
            }
            _ => {}
        }
    }

    pub fn read_byte_arm7(&self, addr: u32) -> u8 {
        match (addr >> 24) & 0xFF {
            0x00 => self.arm7_bios[((addr & 0x00FF_FFFF) % 0x4000) as usize],
            0x02 => {
                let offset = addr & 0x00FF_FFFF;
                self.main_ram[(offset % (4 * 1024 * 1024)) as usize]
            }
            0x03 => {
                let offset = addr & 0x00FF_FFFF;
                if offset < 0x800000 { // Shared WRAM region
                    self.read_shared_wram_arm7(offset)
                } else {
                    self.arm7_wram[((offset - 0x800000) % 65536) as usize]
                }
            }
            0x04 => {
                let offset = addr & 0x00FF_FFFF;
                match offset {
                    o if (0x000100..0x000110).contains(&o) => self.timers7.read_byte(o - 0x100),
                    0x000138 => self.rtc.read(),
                    // Sound registers: live APU state. SOUNDxCNT bit 31 reads
                    // the real per-channel busy (one-shots clear it when LEN
                    // runs out, loops stay busy); SAD/TMR/PNT/LEN are
                    // write-only on hardware and read 0.
                    o if (0x000400..0x000520).contains(&o) => self.apu.read_reg(o),
                    0x000130 => self.get_keyinput() as u8,
                    0x000131 => (self.get_keyinput() >> 8) as u8,
                    0x000136 => self.get_extkeyin() as u8,
                    0x000137 => (self.get_extkeyin() >> 8) as u8,
                    0x000180 => self.read_ipcsync_arm7() as u8,
                    0x000181 => (self.read_ipcsync_arm7() >> 8) as u8,
                    0x000184 => self.read_ipc_fifo_cnt_arm7() as u8,
                    0x000185 => (self.read_ipc_fifo_cnt_arm7() >> 8) as u8,
                    0x0001A0..=0x0001A3 => self.read_auxspi_byte(false, offset),
                    0x0001C0 => self.spi.read_spicnt() as u8,
                    0x0001C1 => (self.spi.read_spicnt() >> 8) as u8,
                    0x0001C2 => self.spi.read_spidata() as u8,
                    0x0001C3 => (self.spi.read_spidata() >> 8) as u8,
                    0x000241 => self.wram_control,
                    0x000210 => self.arm7_ie as u8,
                    0x000211 => (self.arm7_ie >> 8) as u8,
                    0x000212 => (self.arm7_ie >> 16) as u8,
                    0x000213 => (self.arm7_ie >> 24) as u8,
                    0x000214 => self.arm7_if as u8,
                    0x000215 => (self.arm7_if >> 8) as u8,
                    0x000216 => (self.arm7_if >> 16) as u8,
                    0x000217 => (self.arm7_if >> 24) as u8,
                    // DISPSTAT (0x04000004); see the ARM9 copy.
                    0x000004 => self.arm7_io[4],
                    0x000005 => self.arm7_io[5],
                    0x000208 => self.arm7_ime as u8,
                    0x000209 => (self.arm7_ime >> 8) as u8,
                    0x00020A => (self.arm7_ime >> 16) as u8,
                    0x00020B => (self.arm7_ime >> 24) as u8,
                    _ => {
                        if offset < self.arm7_io.len() as u32 {
                            self.arm7_io[offset as usize]
                        } else {
                            0
                        }
                    }
                }
            }
            0x06 => {
                let offset = addr & 0x00FF_FFFF;
                // VRAM banks C and D mapped to the ARM7; each carries its own
                // OFS, so ask both rather than assuming C is low and D is high.
                for bank in [&self.vram.banks[2], &self.vram.banks[3]] {
                    if let Some(slot) = bank.arm7_window_slot(offset) {
                        return bank.data[slot];
                    }
                }
                0
            }
            _ => 0,
        }
    }

    pub fn write_byte_arm7(&mut self, addr: u32, val: u8) {
        // U27o: the settings-copy wiper hunt — ARM7 side.
        if self.calib_watch_on
            && (0x027F_FCD8..0x027F_FCE4).contains(&addr)
            && self.calib_write_log.len() < 256
        {
            self.calib_write_log.push((addr | 0x8000_0000, val as u32)); // bit31 = ARM7
        }
        match (addr >> 24) & 0xFF {
            0x02 => {
                let offset = addr & 0x00FF_FFFF;
                let idx = (offset % (4 * 1024 * 1024)) as usize;
                self.main_ram[idx] = val;
                // The ARM7 shares main RAM with the ARM9, so its stores can
                // land on ARM9 code just as the ARM9's own can.
                self.dirty_main_ram(idx, 1);
            }
            0x03 => {
                let offset = addr & 0x00FF_FFFF;
                if offset < 0x800000 {
                    self.write_shared_wram_arm7(offset, val);
                } else {
                    let idx = ((offset - 0x800000) % 65536) as usize;
                    self.arm7_wram[idx] = val;
                    // The ARM7 recompiler executes from this WRAM; see
                    // `code_page_arm7`.
                    self.dirty_arm7_wram(idx, 1);
                }
            }
            0x04 => {
                let offset = addr & 0x00FF_FFFF;
                match offset {
                    o if (0x000100..0x000110).contains(&o) => {
                        self.timers7.write_byte(o - 0x100, val)
                    }
                    0x000138 => self.rtc.write(val),
                    0x0001C0 => {
                        let cur = self.spi.read_spicnt();
                        self.spi.write_spicnt((cur & 0xFF00) | val as u16);
                    }
                    0x0001C1 => {
                        let cur = self.spi.read_spicnt();
                        self.spi.write_spicnt((cur & 0x00FF) | ((val as u16) << 8));
                    }
                    0x0001C2 => {
                        self.spi.write_spidata(val as u16);
                        if (self.spi.spicnt & 0x4000) != 0 && (self.spi.spicnt & 0x8000) != 0 {
                            self.trigger_interrupt_arm7(IF_ARM7_SPI);
                        }
                    }
                    0x0001C3 => {
                        let cur = self.spi.read_spidata();
                        self.spi.write_spidata((cur & 0x00FF) | ((val as u16) << 8));
                        if (self.spi.spicnt & 0x4000) != 0 && (self.spi.spicnt & 0x8000) != 0 {
                            self.trigger_interrupt_arm7(IF_ARM7_SPI);
                        }
                    }
                    0x000180 => {
                        let cur = self.read_ipcsync_arm7();
                        self.write_ipcsync_arm7((cur & 0xFF00) | val as u16);
                    }
                    0x000181 => {
                        let cur = self.read_ipcsync_arm7();
                        self.write_ipcsync_arm7((cur & 0x00FF) | ((val as u16) << 8));
                    }
                    0x000184 => {
                        // Bit 14 (FIFO error) is acknowledged by writing a 1,
                        // so the read-back half of this RMW must not carry it:
                        // a plain low-byte write would otherwise re-write the
                        // live error bit as a 1 and silently clear an error the
                        // software has not looked at yet.
                        let cur = self.read_ipc_fifo_cnt_arm7() & !(1 << 14);
                        self.write_ipc_fifo_cnt_arm7((cur & 0xFF00) | val as u16);
                    }
                    0x000185 => {
                        let cur = self.read_ipc_fifo_cnt_arm7() & !(1 << 14);
                        self.write_ipc_fifo_cnt_arm7((cur & 0x00FF) | ((val as u16) << 8));
                    }
                    0x000210 => self.arm7_ie = (self.arm7_ie & !0xFF) | val as u32,
                    0x000211 => self.arm7_ie = (self.arm7_ie & !0xFF00) | ((val as u32) << 8),
                    0x000212 => self.arm7_ie = (self.arm7_ie & !0xFF0000) | ((val as u32) << 16),
                    0x000213 => self.arm7_ie = (self.arm7_ie & !0xFF000000) | ((val as u32) << 24),
                    0x000214 => self.arm7_if &= !(val as u32),
                    0x000215 => self.arm7_if &= !((val as u32) << 8),
                    0x000216 => self.arm7_if &= !((val as u32) << 16),
                    0x000217 => self.arm7_if &= !((val as u32) << 24),
                    // DISPSTAT (0x04000004); see the ARM9 copy.
                    0x000004 => self.arm7_io[4] = (self.arm7_io[4] & !0xB8) | (val & 0xB8),
                    0x000005 => self.arm7_io[5] = val,
                    0x000208 => self.arm7_ime = (self.arm7_ime & !0xFF) | val as u32,
                    0x000209 => self.arm7_ime = (self.arm7_ime & !0xFF00) | ((val as u32) << 8),
                    0x00020A => self.arm7_ime = (self.arm7_ime & !0xFF0000) | ((val as u32) << 16),
                    0x00020B => self.arm7_ime = (self.arm7_ime & !0xFF000000) | ((val as u32) << 24),
                    // DMA channels 0-3 — immediate start on the CNT enable-bit write
                    // (see the ARM9 side and `run_immediate_dma`).
                    0x0000B0..=0x0000DF => {
                        let old = self.arm7_io[offset as usize];
                        self.arm7_io[offset as usize] = val;
                        // Same edge rule as the ARM9 path: only a 0->1 enable
                        // transition starts a DMA.
                        if (val & 0x80) != 0 && (old & 0x80) == 0 {
                            match offset {
                                0x0000BB => self.maybe_run_dma_arm7(0),
                                0x0000C7 => self.maybe_run_dma_arm7(1),
                                0x0000D3 => self.maybe_run_dma_arm7(2),
                                0x0000DF => self.maybe_run_dma_arm7(3),
                                _ => {}
                            }
                        }
                    }
                    // Sound registers: routed to the APU (a start-bit 0->1
                    // edge on SOUNDxCNT byte 3 keys the channel on).
                    o if (0x000400..0x000520).contains(&o) => self.apu_write_byte(o, val),
                    // AUXSPI (cartridge backup chip bus). The ARM7 owns this
                    // bus; see `crate::nds::backup` for the device model.
                    0x0001A0..=0x0001A3 => {
                        self.note_aux_write(false, offset, val);
                        self.write_auxspi_byte(false, offset, val);
                    }
                    // Gamecard block registers. The ARM7 gamecard path is
                    // unwired, which would hang a data-ready spin on that core.
                    //
                    // ponytail: measured on SoulSilver across a full boot, the
                    // ARM7 issues ZERO writes here and starts ZERO blocks, so
                    // the unwired path is unreachable for this title and wiring
                    // `start_gamecard_block` (as the ARM9 arm does) would be an
                    // unevidenced change to ROM streaming. Ceiling: a title that
                    // reads the card from the ARM7 would stall. Upgrade path:
                    // mirror the ARM9 arm once these counters go nonzero.
                    0x0001A4..=0x0001AF => {
                        self.aux7_card_writes = self.aux7_card_writes.saturating_add(1);
                        if offset == 0x0001A7 && (val & 0x80) != 0 {
                            self.aux7_block_starts = self.aux7_block_starts.saturating_add(1);
                        }
                        self.arm7_io[offset as usize] = val;
                    }
                    _ => {
                        if offset < self.arm7_io.len() as u32 {
                            self.arm7_io[offset as usize] = val;
                        }
                    }
                }
            }
            0x06 => {
                let offset = addr & 0x00FF_FFFF;
                // Mirror of the read decode above; see `arm7_window_slot`.
                for i in [2usize, 3] {
                    if let Some(slot) = self.vram.banks[i].arm7_window_slot(offset) {
                        self.vram.banks[i].data[slot] = val;
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    /// `n` bytes of plain RAM starting at `addr`, when the ARM9 decode lands
    /// somewhere that is a contiguous byte array with no side effects.
    ///
    /// This exists purely for throughput, and it is the hottest decision in the
    /// emulator. `read_word_arm9` is the instruction-fetch path (`Arm9Bus::
    /// read_word` at every pipeline fill) as well as every LDR, and it was built
    /// out of four `read_byte_arm9` calls — so a single 32-bit access re-ran the
    /// ITCM test, the DTCM test, the `addr >> 24` dispatch and the region range
    /// arithmetic four times over. At the measured ~400k instructions a frame
    /// that is well over a million redundant decodes per frame, against a 16.72 ms
    /// budget the overworld already misses on half its ticks.
    ///
    /// Only the three regions that are genuinely contiguous byte arrays qualify,
    /// and only when the whole access fits inside one without wrapping the
    /// mirror; **everything else falls through to the byte path**, so no I/O
    /// register, VRAM bank, palette or open-bus behaviour can be bypassed. The
    /// guards are written to mirror `read_byte_arm9`'s own branches one for one:
    /// if that function's decode ever changes, this must change with it, which is
    /// why the two sit next to each other and why the region arithmetic is
    /// repeated rather than shared with a helper that could drift.
    #[inline]
    pub(crate) fn contiguous_arm9(&self, addr: u32, n: u32) -> Option<&[u8]> {
        if self.in_itcm_arm9(addr) {
            let off = (addr.wrapping_sub(self.itcm_base()) as usize) % self.itcm.len();
            return self.itcm.get(off..off + n as usize);
        }
        if self.in_dtcm_arm9(addr) {
            let off = (addr.wrapping_sub(self.dtcm_base()) as usize) % self.dtcm.len();
            return self.dtcm.get(off..off + n as usize);
        }
        if (addr >> 24) & 0xFF == 0x02 {
            // Only the first branch of the 0x02 arm: below 4 MB is plain main
            // RAM. The 0x400000..0x480000 window routes to shared WRAM and
            // anything above mirrors, so both are left to the byte path.
            let off = addr & 0x00FF_FFFF;
            if off.saturating_add(n) <= 4 * 1024 * 1024 {
                return self.main_ram.get(off as usize..(off + n) as usize);
            }
        }
        None
    }

    /// Split fast/cold so the *check* inlines into the caller and the decode
    /// does not.
    ///
    /// `contiguous_arm9` is a handful of compares, but it used to live inside a
    /// function large enough that LLVM never inlined the whole thing into
    /// `Arm9Bus::read_*` — so every access paid a call even when the fast path
    /// hit, which is nearly always. `inline(always)` on the test plus
    /// `inline(never)` on the byte-path tail keeps the hot path branch-only
    /// without duplicating the decode at every call site.
    #[inline(always)]
    pub fn read_halfword_arm9(&self, addr: u32) -> u16 {
        if let Some(b) = self.contiguous_arm9(addr, 2) {
            return u16::from_le_bytes([b[0], b[1]]);
        }
        self.read_halfword_arm9_cold(addr)
    }

    /// Byte-path tail of [`Self::read_halfword_arm9`]; see it for why this is
    /// a separate, deliberately un-inlined function.
    #[inline(never)]
    fn read_halfword_arm9_cold(&self, addr: u32) -> u16 {
        let b0 = self.read_byte_arm9(addr) as u16;
        let b1 = self.read_byte_arm9(addr.wrapping_add(1)) as u16;
        b0 | (b1 << 8)
    }

    pub fn write_halfword_arm9(&mut self, addr: u32, val: u16) {
        if self.calib_watch_on
            && (0x021E_36CC..0x021E_36E8).contains(&addr)
            && self.calib_write_log.len() < 256
        {
            self.calib_write_log.push((addr, val as u32));
        }
        if let Some(dst) = self.contiguous_arm9_mut(addr, 2) {
            dst.copy_from_slice(&val.to_le_bytes());
            return;
        }
        self.write_byte_arm9(addr, val as u8);
        self.write_byte_arm9(addr.wrapping_add(1), (val >> 8) as u8);
    }

    /// The instruction-fetch path. Split fast/cold for the reason given on
    /// [`Self::read_halfword_arm9`].
    #[inline(always)]
    pub fn read_word_arm9(&self, addr: u32) -> u32 {
        if let Some(b) = self.contiguous_arm9(addr, 4) {
            return u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
        self.read_word_arm9_cold(addr)
    }

    /// Byte-path tail of [`Self::read_word_arm9`].
    #[inline(never)]
    fn read_word_arm9_cold(&self, addr: u32) -> u32 {
        let b0 = self.read_byte_arm9(addr) as u32;
        let b1 = self.read_byte_arm9(addr.wrapping_add(1)) as u32;
        let b2 = self.read_byte_arm9(addr.wrapping_add(2)) as u32;
        let b3 = self.read_byte_arm9(addr.wrapping_add(3)) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    /// Run a SWAP_BUFFERS the decoder deferred (U30): rasterizing needs the
    /// texture VRAM slots, which can't be borrowed while `gx` is being fed —
    /// the flag decouples the two borrows.
    fn flush_gx_swap(&mut self) {
        if self.gx.engine.swap_pending {
            // Scanline the SWAP_BUFFERS store landed on. This is the number that
            // says whether the single-buffer rasterizer could tear: anything
            // below 192 is inside the visible period, so the old code rewrote
            // the buffer the PPU was scanning out of and left a seam there. Kept
            // as evidence for why `Gx3d::fb_back` exists.
            self.gx_swap_vcount = u16::from(self.arm9_io[6]) | (u16::from(self.arm9_io[7]) << 8);
            self.gx_swaps_total = self.gx_swaps_total.wrapping_add(1);
            if self.gx_swap_vcount < 192 {
                self.gx_swaps_in_visible = self.gx_swaps_in_visible.wrapping_add(1);
            }
            let NdsMmu { gx, vram, gx_raster_enabled, .. } = self;
            gx.engine.swap_buffers(vram, *gx_raster_enabled);
        }
    }

    pub fn write_word_arm9(&mut self, addr: u32, val: u32) {
        if self.calib_watch_on
            && (0x021E_36CC..0x021E_36E8).contains(&addr)
            && self.calib_write_log.len() < 256
        {
            self.calib_write_log.push((addr, val));
        }
        // Geometry command stream tap: CPU STRs and DMA start-timing-7
        // transfers both deliver display-list words here. 0x400-0x43F is the
        // packed GXFIFO window; 0x440-0x5C8 are the one-command-per-port
        // registers. ponytail: halfword/byte GX writes are not decoded — the
        // SDK feeds the engine exclusively in words.
        // A GX word is fully handled here, ONCE, instead of being handed on to
        // four `write_byte_arm9` calls. That fall-through re-entered the
        // 0x400..=0x5CB byte arm four more times per word, and on the order of
        // 10^4 display-list words a frame it re-set `has_3d_activity`, re-ran
        // `maybe_raise_gxfifo_irq()` and stored the same four bytes into
        // `arm9_io` — five times over. The two side effects that matter are kept
        // (they are what `test_gx_words_reach_decoder_via_fifo_and_ports` and
        // `test_gxstat_hle_read_never_reports_error_or_busy` pin); only the
        // repetition and the byte store go. The store was dead weight: the GX
        // command ports are write-only on hardware, GXSTAT has its own read arm
        // at 0x600, and no read path consults that range.
        if (0x0400_0400..0x0400_0440).contains(&addr) {
            self.gx.push_fifo_word(val);
            self.note_gx_tex_watch();
            self.flush_gx_swap();
            self.has_3d_activity = true;
            // After any push the FIFO drains back to empty, so a selected
            // FIFO-IRQ condition immediately re-holds.
            self.maybe_raise_gxfifo_irq();
            return;
        } else if (0x0400_0440..=0x0400_05C8).contains(&addr) {
            self.gx.push_port_word(((addr - 0x0400_0400) >> 2) as u8, val);
            self.note_gx_tex_watch();
            self.flush_gx_swap();
            self.has_3d_activity = true;
            self.maybe_raise_gxfifo_irq();
            return;
        } else if addr == 0x0400_0350 {
            // CLEAR_COLOR: bits0-14 RGB, bits16-20 alpha — alpha 0 means the
            // 3D layer clears transparent and the 2D backdrop shows through.
            // ponytail: clear polygon-ID/fog bits ignored, word writes only
            // (the SDK's G3X_SetClearColor writes the full word).
            self.gx.engine.clear_px = if (val >> 16) & 0x1F != 0 {
                (val as u16 & 0x7FFF) | 0x8000
            } else {
                0
            };
        }
        if self.write_word_auxspicnt(true, addr, val) {
            return;
        }
        if let Some(dst) = self.contiguous_arm9_mut(addr, 4) {
            dst.copy_from_slice(&val.to_le_bytes());
            return;
        }
        self.write_byte_arm9(addr, val as u8);
        self.write_byte_arm9(addr.wrapping_add(1), (val >> 8) as u8);
        self.write_byte_arm9(addr.wrapping_add(2), (val >> 16) as u8);
        self.write_byte_arm9(addr.wrapping_add(3), (val >> 24) as u8);
    }

    /// AUXSPICNT is a 16-bit register whose neighbour, AUXSPIDATA, clocks the
    /// save chip on every byte written to it. A 32-bit store to 0x040001A0
    /// therefore must NOT be decomposed into four byte writes: the third would
    /// clock a spurious byte into the chip. Returns whether the write was
    /// handled here.
    fn write_word_auxspicnt(&mut self, arm9: bool, addr: u32, val: u32) -> bool {
        if addr != 0x0400_01A0 {
            return false;
        }
        for (i, b) in val.to_le_bytes().iter().take(2).enumerate() {
            let off = 0x0001A0 + i as u32;
            self.note_aux_write(arm9, off, *b);
            self.write_auxspi_byte(arm9, off, *b);
        }
        true
    }

    /// ARM7 twin of [`Self::contiguous_arm9`] — same contract, same reason, and
    /// the same rule that it must mirror `read_byte_arm7`'s decode branch for
    /// branch. The ARM7 has no TCM, and its 0x02 arm always folds through the
    /// 4 MB mirror rather than routing a sub-window elsewhere, so the two
    /// qualifying regions are main RAM and the private 64 KB WRAM. Shared WRAM
    /// (0x03 below 0x800000) is deliberately left out: what it maps to depends on
    /// WRAMCNT, so it is the byte path's business.
    #[inline]
    pub(crate) fn contiguous_arm7(&self, addr: u32, n: u32) -> Option<&[u8]> {
        match (addr >> 24) & 0xFF {
            0x02 => {
                let off = (addr & 0x00FF_FFFF) % (4 * 1024 * 1024);
                self.main_ram.get(off as usize..(off + n) as usize)
            }
            0x03 => {
                let offset = addr & 0x00FF_FFFF;
                if offset < 0x800000 {
                    return None; // shared WRAM: mapping depends on WRAMCNT
                }
                let off = (offset - 0x800000) % 65536;
                self.arm7_wram.get(off as usize..(off + n) as usize)
            }
            _ => None,
        }
    }

    /// Is any of the write-path diagnostics armed?
    ///
    /// The `*_mut` fast paths below store into the backing array directly, which
    /// means `write_byte_arm9`/`write_byte_arm7` — and the three watches that
    /// live inside them — never run. Rather than replicate the watch logic in
    /// two more places (where it would silently drift), the fast path simply
    /// declines whenever a watch is armed and lets the byte path see every byte.
    ///
    /// All three are off in every non-diagnostic run (`false`/`None` at
    /// construction, armed only from the probes in `emulator.rs`), so this is one
    /// perfectly-predicted branch on the hot path and full fidelity on the cold
    /// one.
    #[inline]
    fn write_watch_armed(&self) -> bool {
        self.calib_watch_on || self.tp_watch_on || self.ram_write_watch.is_some()
    }


    /// Write-side twin of [`Self::contiguous_arm9`].
    ///
    /// Same regions, same mirror rules, same "must mirror the byte path's decode
    /// branch for branch" obligation. It is only ever reached *after* the callers'
    /// special-cased ranges have returned (the GX command window, CLEAR_COLOR,
    /// AUXSPICNT), so those keep their existing precedence over the TCMs.
    #[inline]
    pub(crate) fn contiguous_arm9_mut(&mut self, addr: u32, n: u32) -> Option<&mut [u8]> {
        if self.write_watch_armed() {
            return None;
        }
        // The caller may write any byte of what it is handed, so the pages are
        // marked here rather than per store. Marking before the range check
        // rather than after means a rejected request can over-invalidate, which
        // costs a recompile and never costs correctness.
        if self.in_itcm_arm9(addr) {
            let off = (addr.wrapping_sub(self.itcm_base()) as usize) % self.itcm.len();
            self.dirty_itcm(off, n as usize);
            return self.itcm.get_mut(off..off + n as usize);
        }
        if self.in_dtcm_arm9(addr) {
            let off = (addr.wrapping_sub(self.dtcm_base()) as usize) % self.dtcm.len();
            return self.dtcm.get_mut(off..off + n as usize);
        }
        if (addr >> 24) & 0xFF == 0x02 {
            let off = addr & 0x00FF_FFFF;
            if off.saturating_add(n) <= 4 * 1024 * 1024 {
                self.dirty_main_ram(off as usize, n as usize);
                return self.main_ram.get_mut(off as usize..(off + n) as usize);
            }
        }
        None
    }

    /// Write-side twin of [`Self::contiguous_arm7`]; see
    /// [`Self::contiguous_arm9_mut`] for the watch rule.
    #[inline]
    pub(crate) fn contiguous_arm7_mut(&mut self, addr: u32, n: u32) -> Option<&mut [u8]> {
        if self.write_watch_armed() {
            return None;
        }
        match (addr >> 24) & 0xFF {
            0x02 => {
                let off = (addr & 0x00FF_FFFF) % (4 * 1024 * 1024);
                // Main RAM is shared, so an ARM7 block copy can land on ARM9
                // code; see the ARM9 twin for why this precedes the range check.
                self.dirty_main_ram(off as usize, n as usize);
                self.main_ram.get_mut(off as usize..(off + n) as usize)
            }
            0x03 => {
                let offset = addr & 0x00FF_FFFF;
                if offset < 0x800000 {
                    return None; // shared WRAM: mapping depends on WRAMCNT
                }
                let off = (offset - 0x800000) % 65536;
                // ARM7 code executes from this WRAM; see `code_page_arm7` and
                // the ARM9 twin for why marking precedes the range check.
                self.dirty_arm7_wram(off as usize, n as usize);
                self.arm7_wram.get_mut(off as usize..(off + n) as usize)
            }
            _ => None,
        }
    }

    /// ARM7 twin of [`Self::read_halfword_arm9`], split fast/cold for the same
    /// reason. The SPICNT/SPIDATA special cases live in the cold half: they are
    /// I/O, which `contiguous_arm7` never claims, so moving them costs the fast
    /// path nothing and keeps their precedence over the byte path unchanged.
    #[inline(always)]
    pub fn read_halfword_arm7(&self, addr: u32) -> u16 {
        if let Some(b) = self.contiguous_arm7(addr, 2) {
            return u16::from_le_bytes([b[0], b[1]]);
        }
        self.read_halfword_arm7_cold(addr)
    }

    /// I/O and byte-path tail of [`Self::read_halfword_arm7`].
    #[inline(never)]
    fn read_halfword_arm7_cold(&self, addr: u32) -> u16 {
        if addr == 0x040001C0 {
            return self.spi.read_spicnt();
        }
        if addr == 0x040001C2 {
            return self.spi.read_spidata();
        }
        let b0 = self.read_byte_arm7(addr) as u16;
        let b1 = self.read_byte_arm7(addr.wrapping_add(1)) as u16;
        b0 | (b1 << 8)
    }

    pub fn write_halfword_arm7(&mut self, addr: u32, val: u16) {
        if self.tp_watch_on
            && matches!(addr >> 24, 0x02 | 0x03)
            && matches!(val, 2032 | 3322 | 0x0FFF)
            && self.tp_watch_log.len() < 512
        {
            self.tp_watch_log.push((addr, val as u32));
        }
        if addr == 0x040001C0 {
            self.spi.write_spicnt(val);
            return;
        }
        if addr == 0x040001C2 {
            self.spi.write_spidata(val);
            if (self.spi.spicnt & 0x4000) != 0 && (self.spi.spicnt & 0x8000) != 0 {
                self.trigger_interrupt_arm7(IF_ARM7_SPI);
            }
            return;
        }
        if let Some(dst) = self.contiguous_arm7_mut(addr, 2) {
            dst.copy_from_slice(&val.to_le_bytes());
            return;
        }
        self.write_byte_arm7(addr, val as u8);
        self.write_byte_arm7(addr.wrapping_add(1), (val >> 8) as u8);
    }

    /// The ARM7 instruction-fetch path. Split fast/cold; see
    /// [`Self::read_halfword_arm9`].
    #[inline(always)]
    pub fn read_word_arm7(&self, addr: u32) -> u32 {
        if let Some(b) = self.contiguous_arm7(addr, 4) {
            return u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
        self.read_word_arm7_cold(addr)
    }

    /// Byte-path tail of [`Self::read_word_arm7`].
    #[inline(never)]
    fn read_word_arm7_cold(&self, addr: u32) -> u32 {
        let b0 = self.read_byte_arm7(addr) as u32;
        let b1 = self.read_byte_arm7(addr.wrapping_add(1)) as u32;
        let b2 = self.read_byte_arm7(addr.wrapping_add(2)) as u32;
        let b3 = self.read_byte_arm7(addr.wrapping_add(3)) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    pub fn write_word_arm7(&mut self, addr: u32, val: u32) {
        if self.tp_watch_on
            && matches!(addr >> 24, 0x02 | 0x03)
            && (matches!(val as u16, 2032 | 3322 | 0x0FFF)
                || matches!((val >> 16) as u16, 2032 | 3322 | 0x0FFF))
            && self.tp_watch_log.len() < 512
        {
            self.tp_watch_log.push((addr, val));
        }
        if self.write_word_auxspicnt(false, addr, val) {
            return;
        }
        if let Some(dst) = self.contiguous_arm7_mut(addr, 4) {
            dst.copy_from_slice(&val.to_le_bytes());
            return;
        }
        self.write_byte_arm7(addr, val as u8);
        self.write_byte_arm7(addr.wrapping_add(1), (val >> 8) as u8);
        self.write_byte_arm7(addr.wrapping_add(2), (val >> 16) as u8);
        self.write_byte_arm7(addr.wrapping_add(3), (val >> 24) as u8);
    }

    pub fn get_keyinput(&self) -> u16 {
        let mut keyinput = 0xFC00u16; // Bits 10-15 are always 1
        if !self.buttons.a { keyinput |= 0x0001; }
        if !self.buttons.b { keyinput |= 0x0002; }
        if !self.buttons.select { keyinput |= 0x0004; }
        if !self.buttons.start { keyinput |= 0x0008; }
        if !self.buttons.right { keyinput |= 0x0010; }
        if !self.buttons.left { keyinput |= 0x0020; }
        if !self.buttons.up { keyinput |= 0x0040; }
        if !self.buttons.down { keyinput |= 0x0080; }
        if !self.buttons.r { keyinput |= 0x0100; }
        if !self.buttons.l { keyinput |= 0x0200; }
        keyinput
    }

    /// EXTKEYIN (0x04000136, ARM7): bit0 X, bit1 Y, bit6 pen-down, bit7 hinge
    /// — all active-LOW except that hinge reads 1 when the lid is CLOSED.
    /// The hinge bit must be 0 (lid open): with it set, SoulSilver decided the
    /// DS was shut at boot, entered sleep mode (screens off, WFI + spin
    /// waiting for the lid-open PM interrupt) and never initialised video.
    pub fn get_extkeyin(&self) -> u16 {
        let mut extkeyin = 0xFF7Fu16; // unused bits high, hinge (bit 7) = open
        if self.buttons.x { extkeyin &= !0x0001; }
        if self.buttons.y { extkeyin &= !0x0002; }
        if self.buttons.nds_touch_pressed { extkeyin &= !0x0040; }
        extkeyin
    }
}

#[cfg(test)]
mod spi_tsc_tests {
    use super::*;

    #[test]
    fn test_spi_tsc_keypad_simulation() {
        let mut mmu = NdsMmu::new();

        mmu.buttons.a = true;
        mmu.buttons.x = true;
        mmu.buttons.nds_touch_pressed = true;

        let keyinput = mmu.read_halfword_arm7(0x04000130);
        let extkeyin = mmu.read_halfword_arm7(0x04000136);

        assert_eq!(keyinput & 0x0001, 0); // A pressed -> bit 0 is 0
        assert_ne!(keyinput & 0x0002, 0); // B not pressed -> bit 1 is 1
        assert_eq!(extkeyin & 0x0001, 0); // X pressed -> bit 0 is 0
        assert_eq!(extkeyin & 0x0040, 0); // Touch pressed -> pen-down bit 6 is 0
        assert_eq!(extkeyin & 0x0080, 0); // Hinge bit 7 is 0 = lid OPEN

        mmu.spi.tsc.touch_x = 100;
        mmu.spi.tsc.touch_y = 120;
        mmu.spi.tsc.touch_pressed = true;

        // SPI enable + CS Hold (bit 11 on NDS7 — bit 10 is the bugged
        // "transfer size") + Device 2.
        mmu.write_halfword_arm7(0x040001C0, 0x8200 | 0x0800);

        mmu.write_byte_arm7(0x040001C2, 0x90 | (5 << 4)); // Start + Channel 5 (X-coordinate)
        let response1 = mmu.read_byte_arm7(0x040001C2);
        assert_eq!(response1, 0);

        mmu.write_byte_arm7(0x040001C2, 0);
        let response2 = mmu.read_byte_arm7(0x040001C2);

        mmu.write_halfword_arm7(0x040001C0, 0x8200); // CS Hold released
        mmu.write_byte_arm7(0x040001C2, 0);
        let response3 = mmu.read_byte_arm7(0x040001C2);

        let raw_x = ((response2 as u16) << 5) | ((response3 as u16) >> 3);
        // Derived from the calibration transform, not an independent constant:
        // 15 ADC counts per pixel with a 128-count offset (see
        // `hle::build_user_settings`). The invariant itself is pinned by
        // `touch_transform_round_trips_without_saturating`.
        assert_eq!(raw_x, 100 * 15 + 128);
    }

    #[test]
    fn test_spi_8bit_and_halfword_access() {
        let mut mmu = NdsMmu::new();
        mmu.spi.tsc.touch_x = 100;
        mmu.spi.tsc.touch_y = 120;
        mmu.spi.tsc.touch_pressed = true;

        // Enable SPI, CS Hold (bit 11), Device 2 (TSC): 0x8200 | 0x0800 = 0x8A00
        mmu.write_halfword_arm7(0x040001C0, 0x8A00);
        assert_eq!(mmu.read_halfword_arm7(0x040001C0), 0x8A00);

        // Control byte: Start (0x80) | Channel 5 (0x50) | 8-bit mode (0x08) = 0xD8
        mmu.write_halfword_arm7(0x040001C2, 0xD8);
        assert_eq!(mmu.read_halfword_arm7(0x040001C2), 0); // Returns 0 on control byte write

        // Second transfer to read the 8-bit response
        mmu.write_halfword_arm7(0x040001C2, 0x00);
        let resp = mmu.read_halfword_arm7(0x040001C2);
        assert_eq!(resp, ((100 * 15 + 128) >> 4) & 0xFF); // 8-bit read of the same value
    }
}


// ---------------------------------------------------------------------------
// Snapshot support (see `crate::snapshot`).
// ---------------------------------------------------------------------------

impl crate::snapshot::Snap for VramBank {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        let size = self.data.len();
        snap_bytes(v, &mut self.data, size, "vram bank size mismatch");
        self.control.snap(v);
    }
}

impl crate::snapshot::Snap for VramManager {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.banks.snap(v);
    }
}

impl crate::snapshot::Snap for IpcState {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.arm9_to_arm7_sync.snap(v);
        self.arm7_to_arm9_sync.snap(v);
        self.arm9_sync_irq_enable.snap(v);
        self.arm7_sync_irq_enable.snap(v);
        // Hardware FIFO depth is 16 words per direction.
        snap_capped_vec(v, &mut self.fifo_9to7, 16);
        snap_capped_vec(v, &mut self.fifo_7to9, 16);
        self.fifo_control_arm9.snap(v);
        self.fifo_control_arm7.snap(v);
    }
}

impl crate::snapshot::Snap for GamecardState {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.src.snap(v);
        self.bytes_left.snap(v);
        self.fill.snap(v);
    }
}

impl crate::snapshot::Snap for NdsTimers {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.reload.snap(v);
        self.counter.snap(v);
        self.control.snap(v);
        self.acc.snap(v);
    }
}

impl crate::snapshot::Snap for NdsRtc {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.prev_pins.snap(v);
        self.bit_count.snap(v);
        self.shift.snap(v);
        self.cmd.snap(v);
        // Longest RTC response is the 7-byte date+time; 16 is slack.
        snap_capped_vec(v, &mut self.response, 16);
        self.out_pos.snap(v);
    }
}

impl crate::snapshot::Snap for NdsMmu {
    /// Everything a resumed NDS needs, and nothing it does not:
    ///
    /// * `rom`, `arm7_bios`, `arm9_bios` are read-only images already in memory,
    ///   and the snapshot header pins the cartridge identity, so re-storing
    ///   135 MB per slot would buy nothing.
    /// * `buttons` is re-injected by the frontend every frame before `tick`.
    /// * every `*_log`, `*_watch_on`, `tag11_*` and `dma_*` counter is probe
    ///   instrumentation rather than hardware state.
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        snap_bytes(v, &mut self.main_ram, 4 * 1024 * 1024, "main_ram size mismatch");
        snap_bytes(v, &mut self.shared_wram, 256 * 1024, "shared_wram size mismatch");
        snap_bytes(v, &mut self.arm7_wram, 64 * 1024, "arm7_wram size mismatch");
        snap_bytes(v, &mut self.itcm, 32 * 1024, "itcm size mismatch");
        snap_bytes(v, &mut self.dtcm, 16 * 1024, "dtcm size mismatch");
        self.vram.snap(v);
        self.wram_control.snap(v);
        self.ipc.snap(v);
        self.arm9_cp15.snap(v);
        // Re-derive: on restore `snap` has just overwritten the CP15 inputs, and
        // the windows are not in the snapshot (they are a cache). Harmless on
        // the save direction, where it recomputes identical values.
        self.set_cp15(self.arm9_cp15);
        self.spi.snap(v);
        self.backup.snap(v);
        self.arm9_ie.snap(v);
        self.arm9_if.snap(v);
        self.arm9_ime.snap(v);
        self.arm7_ie.snap(v);
        self.arm7_if.snap(v);
        self.arm7_ime.snap(v);
        let io9 = self.arm9_io.len();
        snap_bytes(v, &mut self.arm9_io, io9, "arm9_io size mismatch");
        let io7 = self.arm7_io.len();
        snap_bytes(v, &mut self.arm7_io, io7, "arm7_io size mismatch");
        self.gamecard.snap(v);
        self.dma9_internal_dst.snap(v);
        self.palette_ram.snap(v);
        self.oam.snap(v);
        self.has_3d_activity.snap(v);
        self.timers9.snap(v);
        self.timers7.snap(v);
        self.rtc.snap(v);
        self.apu.snap(v);
        self.gx.snap(v);
        self.div_cnt.snap(v);
        self.div_numer.snap(v);
        self.div_denom.snap(v);
        self.sqrt_cnt.snap(v);
        self.sqrt_param.snap(v);
        // Which core owns the AUXSPI transaction in flight, if any: dropping it
        // would let the other core's next byte join a foreign frame.
        self.aux_txn_core.snap(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nds::cpu::{Arm9Cpu, Arm7Cpu};
    use crate::nds::hle;

    /// How the caller chunks a slice must not change the audio.
    ///
    /// The run loop feeds `tick_apu` ~64-cycle slices, but nothing about the
    /// emulated machine depends on that number, so one long call and many short
    /// ones have to render the same samples. Before the integration was split
    /// at channel edges, every source sample was applied retroactively over the
    /// whole slice that contained it, which displaced edges by up to a full
    /// slice and made the output a function of the scheduler's granularity.
    #[test]
    fn apu_output_is_independent_of_slice_size() {
        const SAD: u32 = 0x0200_0000;
        const TOTAL: u32 = 40_960;

        // One PCM8 channel looping a square pattern out of main RAM, at a
        // 256-cycle source period so many edges fall inside a coarse slice.
        let build = || {
            let mut mmu = NdsMmu::new();
            for k in 0..64u32 {
                mmu.write_byte_arm7(SAD + k, if (k / 4) % 2 == 0 { 0x60 } else { 0xA0 });
            }
            mmu.apu.soundcnt = 0x807F; // master enable + full master volume
            let ch = &mut mmu.apu.channels[0];
            // PCM8, volume 127, divider /1, centre pan, repeat mode 1 (loop).
            ch.cnt = (1 << 27) | 0x0040_007F;
            ch.sad = SAD;
            ch.tmr = 0xFF80; // period = 2 * 0x80 = 256 bus cycles
            ch.len = 16; // 16 words = the 64 bytes written above
            ch.active = true;
            mmu
        };

        let render = |slice: u32| -> Vec<i16> {
            let mut mmu = build();
            let mut buf = vec![0i16; 8192];
            let mut done = 0;
            while done < TOTAL {
                let n = slice.min(TOTAL - done);
                mmu.tick_apu(n, &mut buf, 0, 1.0);
                done += n;
            }
            // `tick_apu` defers whatever cannot yet change the mix, so the tail
            // of the last window is still pending — exactly as it is inside a
            // real tick until the run loop flushes. Closing it here is part of
            // the contract under test, not a workaround for it.
            mmu.flush_apu(&mut buf, 0, 1.0);
            buf.truncate(mmu.apu.resampler.sample_count * 2);
            buf
        };

        let coarse = render(TOTAL);
        let fine = render(64);
        assert!(!coarse.is_empty(), "the channel must actually produce audio");
        assert!(
            coarse.iter().any(|&s| s != 0),
            "the channel must produce signal, not silence"
        );
        assert_eq!(
            coarse.len(),
            fine.len(),
            "the same cycle count must emit the same number of samples"
        );
        // Splitting differs, so the f64 accumulator sums differ in the last
        // bits; anything beyond one LSB is displaced edges, not rounding.
        let worst = coarse
            .iter()
            .zip(&fine)
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
            .max()
            .unwrap_or(0);
        assert!(worst <= 1, "slice size changed the output by {worst} LSB");
    }

    #[test]
    fn test_cp15_mcr_propagation_and_tcm_mapping() {
        let mut mmu = NdsMmu::new();
        let mut arm9 = Arm9Cpu::new();

        // Put unique values in main ram, ITCM and DTCM
        mmu.main_ram[0] = 0xAA;
        mmu.itcm[0] = 0xBB;
        mmu.dtcm[0] = 0xCC;

        // By default, TCM is disabled, so reading from virtual addresses 0x02000000 should hit main ram
        assert_eq!(mmu.read_byte_arm9(0x02000000), 0xAA);

        // Set the TCM Region Registers: base + size only (NOT enable — on ARM946
        // the enable is the Control Register bit). DTCM base 0x0B000000 size 16KB
        // (N=5); ITCM base 0x01000000 size 32KB (N=6).
        let mut dtcm_val = 0x0B000000 | (5 << 1);
        arm9.execute_cp15_transfer(&mut mmu, true, 9, 1, 0, &mut dtcm_val);
        assert_eq!(mmu.cp15().dtcm_control, 0x0B000000 | (5 << 1));
        assert_eq!(arm9.cp15.dtcm_control, 0x0B000000 | (5 << 1));
        assert_eq!(mmu.dtcm_base(), 0x0B000000);

        let mut itcm_val = 0x01000000 | (6 << 1);
        arm9.execute_cp15_transfer(&mut mmu, true, 9, 1, 1, &mut itcm_val);
        assert_eq!(mmu.cp15().itcm_control, 0x01000000 | (6 << 1));
        assert_eq!(mmu.itcm_base(), 0x01000000);

        // Still disabled: Control Register enable bits (16/18) not set yet.
        assert!(!mmu.dtcm_enabled());
        assert!(!mmu.itcm_enabled());

        // Enable both via Control Register (c1,c0,0): bit 16 DTCM, bit 18 ITCM.
        let mut ctrl_val = (1 << 16) | (1 << 18);
        arm9.execute_cp15_transfer(&mut mmu, true, 1, 0, 0, &mut ctrl_val);
        assert!(mmu.dtcm_enabled());
        assert!(mmu.itcm_enabled());

        // Now the TCMs map: 0x0B000000 -> DTCM (0xCC), 0x01000000 -> ITCM (0xBB),
        // 0x02000000 -> main RAM (0xAA, outside both windows).
        assert_eq!(mmu.read_byte_arm9(0x0B000000), 0xCC);
        assert_eq!(mmu.read_byte_arm9(0x01000000), 0xBB);
        assert_eq!(mmu.read_byte_arm9(0x02000000), 0xAA);

        // Disable both via the Control Register.
        let mut disable_ctrl = 0;
        arm9.execute_cp15_transfer(&mut mmu, true, 1, 0, 0, &mut disable_ctrl);
        assert!(!mmu.dtcm_enabled());
        assert!(!mmu.itcm_enabled());
    }

    #[test]
    fn test_shared_wram_mappings_all_modes() {
        let mut mmu = NdsMmu::new();

        // Set up distinct patterns in shared_wram blocks
        // Block 0 (first 128KB)
        mmu.shared_wram[0] = 0x11;
        mmu.shared_wram[128 * 1024 - 1] = 0x22;
        // Block 1 (second 128KB)
        mmu.shared_wram[128 * 1024] = 0x33;
        mmu.shared_wram[256 * 1024 - 1] = 0x44;

        // --- Mode 0: 256KB to ARM9 ---
        mmu.write_byte_arm9(0x04000247, 0); // Set WRAMCNT to 0
        assert_eq!(mmu.wram_control, 0);

        // ARM9 reads entire 256KB
        assert_eq!(mmu.read_byte_arm9(0x02400000), 0x11);
        assert_eq!(mmu.read_byte_arm9(0x02400000 + 128 * 1024 - 1), 0x22);
        assert_eq!(mmu.read_byte_arm9(0x02400000 + 128 * 1024), 0x33);
        assert_eq!(mmu.read_byte_arm9(0x02400000 + 256 * 1024 - 1), 0x44);

        // ARM9 mirroring to 0x02440000 (offset 256KB)
        assert_eq!(mmu.read_byte_arm9(0x02440000), 0x11);
        assert_eq!(mmu.read_byte_arm9(0x02440000 + 128 * 1024 - 1), 0x22);

        // ARM7 gets nothing (returns 0)
        assert_eq!(mmu.read_byte_arm7(0x03000000), 0);
        assert_eq!(mmu.read_byte_arm7(0x03020000), 0);

        // --- Mode 1: Block 1 to ARM9, Block 0 to ARM7 ---
        //
        // This section and the Mode 3 one below used to be the other way round,
        // which is the transposition the decoders had: GBATEK's table is
        // monotonic (0 = ARM9 all, 1 = ARM9 upper half, 2 = ARM9 lower half,
        // 3 = ARM7 all), so 1 is a SPLIT and 3 is all-to-ARM7 — not the reverse.
        // The test encoded the defect, so the test was corrected, not the fix.
        mmu.write_byte_arm9(0x04000247, 1); // Set WRAMCNT to 1
        assert_eq!(mmu.wram_control, 1);

        // ARM9 reads Block 1 at 0x02440000
        assert_eq!(mmu.read_byte_arm9(0x02440000), 0x33);
        assert_eq!(mmu.read_byte_arm9(0x02440000 + 128 * 1024 - 1), 0x44);
        // ARM9 reads at <256KB return 0
        assert_eq!(mmu.read_byte_arm9(0x02400000), 0);
        assert_eq!(mmu.read_byte_arm9(0x02400000 + 128 * 1024 - 1), 0);

        // ARM7 reads Block 0 (mirrored every 128KB)
        assert_eq!(mmu.read_byte_arm7(0x03000000), 0x11);
        assert_eq!(mmu.read_byte_arm7(0x03000000 + 128 * 1024 - 1), 0x22);
        assert_eq!(mmu.read_byte_arm7(0x03000000 + 128 * 1024), 0x11); // Mirror

        // --- Mode 2: Block 0 to ARM9, Block 1 to ARM7 ---
        mmu.write_byte_arm9(0x04000247, 2);
        assert_eq!(mmu.wram_control, 2);

        // ARM9 reads Block 0 at 0x02400000
        assert_eq!(mmu.read_byte_arm9(0x02400000), 0x11);
        assert_eq!(mmu.read_byte_arm9(0x02400000 + 128 * 1024 - 1), 0x22);
        // ARM9 reads at >=128KB return 0
        assert_eq!(mmu.read_byte_arm9(0x02400000 + 128 * 1024), 0);

        // ARM7 reads Block 1 (mirrored every 128KB)
        assert_eq!(mmu.read_byte_arm7(0x03000000), 0x33);
        assert_eq!(mmu.read_byte_arm7(0x03000000 + 128 * 1024 - 1), 0x44);
        assert_eq!(mmu.read_byte_arm7(0x03000000 + 128 * 1024), 0x33); // Mirror

        // --- Mode 3: 256KB to ARM7 (see the Mode 1 note above) ---
        mmu.write_byte_arm9(0x04000247, 3);
        assert_eq!(mmu.wram_control, 3);

        // ARM9 gets nothing
        assert_eq!(mmu.read_byte_arm9(0x02400000), 0);
        assert_eq!(mmu.read_byte_arm9(0x02440000), 0);

        // ARM7 reads entire 256KB at 0x03000000
        assert_eq!(mmu.read_byte_arm7(0x03000000), 0x11);
        assert_eq!(mmu.read_byte_arm7(0x03000000 + 128 * 1024 - 1), 0x22);
        assert_eq!(mmu.read_byte_arm7(0x03000000 + 128 * 1024), 0x33);
        assert_eq!(mmu.read_byte_arm7(0x03000000 + 256 * 1024 - 1), 0x44);

        // ARM7 mirrors every 256KB up to 0x037C0000
        assert_eq!(mmu.read_byte_arm7(0x03000000 + 256 * 1024), 0x11);

        // --- ARM7 Write Protection to WRAMCNT ---
        // Writing to 0x04000241 from ARM7 should be ignored
        mmu.write_byte_arm7(0x04000241, 0); // Try to set Mode 0
        assert_eq!(mmu.wram_control, 3); // Remains Mode 3
    }

    #[test]
    fn test_ipc_fifo_stress_and_disable() {
        let mut mmu = NdsMmu::new();

        // 1. Enable FIFOs on both sides
        mmu.write_ipc_fifo_cnt_arm9(0x8000);
        mmu.write_ipc_fifo_cnt_arm7(0x8000);

        // Verify empty flags are set
        assert_eq!(mmu.read_ipc_fifo_cnt_arm9() & 1, 1);       // ARM9 TX empty
        assert_eq!(mmu.read_ipc_fifo_cnt_arm9() & 0x0100, 0x0100); // ARM9 RX empty
        assert_eq!(mmu.read_ipc_fifo_cnt_arm7() & 1, 1);       // ARM7 TX empty
        assert_eq!(mmu.read_ipc_fifo_cnt_arm7() & 0x0100, 0x0100); // ARM7 RX empty

        // 2. Fill FIFO 9->7 to capacity (16 words)
        for i in 0..16 {
            mmu.write_ipc_fifo_tx_arm9(100 + i);
        }
        // Verify full flags
        assert_eq!(mmu.read_ipc_fifo_cnt_arm9() & 2, 2);       // ARM9 TX full
        assert_eq!(mmu.read_ipc_fifo_cnt_arm7() & 0x0200, 0x0200);       // ARM7 RX full

        // Verify writing 17th word triggers error flag
        mmu.write_ipc_fifo_tx_arm9(999);
        assert_eq!(mmu.ipc.fifo_9to7.len(), 16); // Remains 16
        assert_eq!(mmu.read_ipc_fifo_cnt_arm9() & (1 << 14), 1 << 14); // Error flag set on ARM9

        // Clear error flag by writing 1 to bit 14
        mmu.write_ipc_fifo_cnt_arm9(0x8000 | (1 << 14));
        assert_eq!(mmu.read_ipc_fifo_cnt_arm9() & (1 << 14), 0); // Error flag cleared

        // 3. Read all 16 words from ARM7
        for i in 0..16 {
            assert_eq!(mmu.read_ipc_fifo_rx_arm7(), 100 + i);
        }
        // Verify empty flags are set again
        assert_eq!(mmu.read_ipc_fifo_cnt_arm9() & 1, 1);
        assert_eq!(mmu.read_ipc_fifo_cnt_arm7() & 1, 1);

        // Verify reading from empty FIFO triggers error flag
        assert_eq!(mmu.read_ipc_fifo_rx_arm7(), 0);
        assert_eq!(mmu.read_ipc_fifo_cnt_arm7() & (1 << 14), 1 << 14); // Error flag set on ARM7

        // 4. Test FIFO Disable clears FIFOs and error flag
        mmu.write_ipc_fifo_tx_arm9(50);
        assert_eq!(mmu.ipc.fifo_9to7.len(), 1);
        mmu.write_ipc_fifo_cnt_arm9(0); // Disable
        assert_eq!(mmu.ipc.fifo_9to7.len(), 0); // Cleared
        assert_eq!(mmu.read_ipc_fifo_cnt_arm9() & (1 << 15), 0); // Disabled
        assert_eq!(mmu.read_ipc_fifo_cnt_arm9() & (1 << 14), 0); // Error cleared

        // 5. Test Disable behavior on other writable bits (2 and 10)
        // Enable and set bits 2 and 10 to 1
        mmu.write_ipc_fifo_cnt_arm9(0x8000 | (1 << 2) | (1 << 10));
        assert_eq!(mmu.read_ipc_fifo_cnt_arm9() & 0x8404, 0x8404);

        // Try to disable AND clear bits 2 and 10 by writing 0
        mmu.write_ipc_fifo_cnt_arm9(0);
        
        // Assert the correct behavior where bits 2 and 10 are cleared.
        // NOTE: In the current implementation, this assertion will fail because the early return
        // when (val & (1 << 15)) == 0 prevents the code from updating the control register with val.
        let cnt_after_disable = mmu.read_ipc_fifo_cnt_arm9();
        assert_eq!(cnt_after_disable & (1 << 2), 0);
        assert_eq!(cnt_after_disable & (1 << 10), 0);
    }

    /// An armed NDS-slot DMA channel (start timing 5) must drain a Gamecard
    /// block into its destination the moment the block starts. SoulSilver's FS
    /// switches from PIO polling to exactly this (DMA1=0xAF000001) after early
    /// boot — without it the read never completes and the game idles forever.
    #[test]
    fn test_gamecard_slot_dma_drains_block() {
        let mut mmu = NdsMmu::new();
        mmu.rom = (0..0x2000u32).map(|i| i as u8).collect();
        // DMA1: SAD = cart data port (fixed), DAD = main RAM, enable + repeat +
        // 32-bit + NDS-slot timing, count 1 (per-word burst on real HW).
        mmu.write_word_arm9(0x0400_00BC, 0x0410_0010);
        mmu.write_word_arm9(0x0400_00C0, 0x0200_1000);
        mmu.write_word_arm9(0x0400_00C4, 0xAF00_0001);
        // Gamecard command: 0xB7 main read at ROM offset 0x200 (big-endian).
        mmu.write_byte_arm9(0x0400_01A8, 0xB7);
        mmu.write_byte_arm9(0x0400_01A9, 0x00);
        mmu.write_byte_arm9(0x0400_01AA, 0x00);
        mmu.write_byte_arm9(0x0400_01AB, 0x02);
        mmu.write_byte_arm9(0x0400_01AC, 0x00);
        // ROMCTRL: block start, size field 1 = 512 bytes. The drain is
        // deferred to the next run-loop slice (the SDK stores a delivery
        // sentinel right after this write) — emulate the slice boundary.
        mmu.write_word_arm9(0x0400_01A4, 0x8000_0000 | (1 << 24));
        assert_eq!(mmu.gamecard.bytes_left, 0, "DMA drained the whole block");
        assert_eq!(
            mmu.read_word_arm9(0x0200_1000),
            0x0302_0100,
            "first word landed at the destination"
        );
        assert_eq!(
            mmu.read_word_arm9(0x0200_1000 + 508),
            0xFFFE_FDFC,
            "last word of the 512-byte block landed"
        );
        let romctrl = mmu.read_word_arm9(0x0400_01A4);
        assert_eq!(romctrl & (1 << 31), 0, "busy cleared on completion");
        let cnt = mmu.read_word_arm9(0x0400_00C4);
        assert_eq!(cnt & (1 << 31), 1 << 31, "repeat-mode channel stays armed");

        // Second block under the SAME enable: the internal dest cursor must
        // keep advancing (hardware latches DAD only on the enable 0->1 edge).
        // FS multi-block streams (FAT/FNT reads) depend on page N+1 landing
        // right after page N, not on top of it.
        mmu.write_byte_arm9(0x0400_01AB, 0x04); // cmd addr -> ROM 0x400
        mmu.write_word_arm9(0x0400_01A4, 0x8000_0000 | (1 << 24));
        assert_eq!(
            mmu.read_word_arm9(0x0200_1200),
            0x0302_0100,
            "second block continued at the advanced cursor (rom[0x400..] = 00 01 02 03)"
        );
    }

    /// TM0 at F/1 with reload 0xFFF0: counts bus cycles, overflows after 0x10,
    /// reloads the latch and raises IF bit 3. The NitroSDK OS tick — the RTOS
    /// alarm timebase SoulSilver sleeps on during boot — is exactly this IRQ.
    #[test]
    fn test_nds_timer_overflow_reload_and_irq() {
        let mut mmu = NdsMmu::new();
        mmu.write_byte_arm9(0x0400_0100, 0xF0); // reload lo
        mmu.write_byte_arm9(0x0400_0101, 0xFF); // reload hi
        mmu.write_byte_arm9(0x0400_0102, 0xC0); // start + IRQ, prescaler F/1
        assert_eq!(
            mmu.read_halfword_arm9(0x0400_0100),
            0xFFF0,
            "start loads the counter from the reload latch"
        );
        mmu.tick_nds_timers(0x0F);
        assert_eq!(mmu.read_halfword_arm9(0x0400_0100), 0xFFFF, "counts at F/1");
        assert_eq!(mmu.arm9_if & (1 << 3), 0, "no IRQ before overflow");
        mmu.tick_nds_timers(1);
        assert_eq!(
            mmu.read_halfword_arm9(0x0400_0100),
            0xFFF0,
            "overflow reloads the latch"
        );
        assert_eq!(mmu.arm9_if & (1 << 3), 1 << 3, "overflow raises IF bit 3");
    }

    /// Count-up cascade: a TM0 overflow advances TM1 (count-up mode) instead of
    /// the prescaler clock; TM1's own overflow raises ITS IRQ (IF bit 4).
    #[test]
    fn test_nds_timer_count_up_cascade() {
        let mut mmu = NdsMmu::new();
        // TM0: reload 0xFFFF (period 1), start, IRQ off, F/1.
        mmu.write_byte_arm9(0x0400_0100, 0xFF);
        mmu.write_byte_arm9(0x0400_0101, 0xFF);
        mmu.write_byte_arm9(0x0400_0102, 0x80);
        // TM1: reload 0xFFFF, count-up + start + IRQ.
        mmu.write_byte_arm9(0x0400_0104, 0xFF);
        mmu.write_byte_arm9(0x0400_0105, 0xFF);
        mmu.write_byte_arm9(0x0400_0106, 0xC4);
        mmu.tick_nds_timers(1); // TM0 overflow -> TM1 +1 -> TM1 overflow
        assert_eq!(mmu.arm9_if & (1 << 4), 1 << 4, "cascaded TM1 overflow raises IF bit 4");
        assert_eq!(mmu.arm9_if & (1 << 3), 0, "TM0 IRQ disabled: bit 3 stays clear");
    }

    /// The ARM7 timer block is independent state whose IRQs land in ARM7 IF.
    #[test]
    fn test_nds_timer_arm7_block_is_independent() {
        let mut mmu = NdsMmu::new();
        mmu.write_byte_arm7(0x0400_0100, 0xFF);
        mmu.write_byte_arm7(0x0400_0101, 0xFF);
        mmu.write_byte_arm7(0x0400_0102, 0xC0);
        mmu.tick_nds_timers(64); // several overflows batched into one delta
        assert_eq!(mmu.arm7_if & (1 << 3), 1 << 3, "ARM7 timer IRQ lands in arm7_if");
        assert_eq!(mmu.arm9_if, 0, "ARM9 IF untouched");
    }

    /// The HLE boot must leave the gamecard chip ID in the firmware boot-info
    /// slots, agreeing exactly with the Gamecard bus response — NitroSDK's
    /// card-removal watchdog compares the two and OS_Terminates on mismatch
    /// (SoulSilver died this way: slots were 0, fresh read was 0xC2-based).
    #[test]
    fn test_boot_writes_gamecard_chip_id_boot_info() {
        let mut mmu = NdsMmu::new();
        let mut arm9 = Arm9Cpu::new();
        let mut arm7 = Arm7Cpu::new();
        let rom = vec![0u8; 0x200]; // minimal parseable header, empty binaries
        hle::boot_load_rom(&mut mmu, &mut arm9, &mut arm7, &rom).unwrap();
        let id = NdsMmu::gamecard_chip_id_for_len(rom.len());
        assert_eq!(id & 0xFF, 0xC2, "Macronix manufacturer byte");
        for slot in [0x027F_F800u32, 0x027F_F804, 0x027F_FC00, 0x027F_FC04, 0x027F_FC10] {
            assert_eq!(mmu.read_word_arm9(slot), id, "boot-info chip ID at {slot:#010x}");
        }
        assert_eq!(mmu.read_halfword_arm9(0x027F_FC40), 1, "boot indicator = cartridge boot");
    }

    #[test]
    fn test_nds_cartridge_load_and_boot() {
        let mut mmu = NdsMmu::new();
        let mut arm9 = Arm9Cpu::new();
        let mut arm7 = Arm7Cpu::new();

        // Construct a mock ROM image
        let mut rom = vec![0u8; 0x1000];
        
        // Game title "MOCK GAME"
        rom[0..9].copy_from_slice(b"MOCK GAME");
        // Game code "MCKG"
        rom[0x0C..0x10].copy_from_slice(b"MCKG");

        // ARM9 binary: rom offset = 0x200, ram address = 0x02000800, size = 0x200
        let arm9_rom_offset: u32 = 0x200;
        let arm9_ram_addr: u32 = 0x02000800;
        let arm9_size: u32 = 0x200;
        rom[0x20..0x24].copy_from_slice(&arm9_rom_offset.to_le_bytes());
        rom[0x24..0x28].copy_from_slice(&arm9_ram_addr.to_le_bytes()); // entry point
        rom[0x28..0x2C].copy_from_slice(&arm9_ram_addr.to_le_bytes()); // ram address
        rom[0x2C..0x30].copy_from_slice(&arm9_size.to_le_bytes());

        // Fill ARM9 binary with some dummy instructions
        let instr1: u32 = 0xEA000000;
        let instr2: u32 = 0xE1A00000;
        rom[0x200..0x204].copy_from_slice(&instr1.to_le_bytes());
        rom[0x204..0x208].copy_from_slice(&instr2.to_le_bytes());

        // ARM7 binary: rom offset = 0x400, ram address = 0x03800000, size = 0x100
        let arm7_rom_offset: u32 = 0x400;
        let arm7_ram_addr: u32 = 0x03800000;
        let arm7_size: u32 = 0x100;
        rom[0x30..0x34].copy_from_slice(&arm7_rom_offset.to_le_bytes());
        rom[0x34..0x38].copy_from_slice(&arm7_ram_addr.to_le_bytes()); // entry point
        rom[0x38..0x3C].copy_from_slice(&arm7_ram_addr.to_le_bytes()); // ram address
        rom[0x3C..0x40].copy_from_slice(&arm7_size.to_le_bytes());

        // Fill ARM7 binary
        let arm7_instr: u32 = 0xE12FFF11; // BX R1
        rom[0x400..0x404].copy_from_slice(&arm7_instr.to_le_bytes());

        // Perform HLE boot load
        let res = hle::boot_load_rom(&mut mmu, &mut arm9, &mut arm7, &rom);
        assert!(res.is_ok());

        // Verify memory copying
        assert_eq!(mmu.read_word_arm9(0x02000800), instr1);
        assert_eq!(mmu.read_word_arm9(0x02000804), instr2);
        assert_eq!(mmu.read_word_arm7(0x03800000), arm7_instr);

        // Header copy at 0x027FFE00
        assert_eq!(&mmu.main_ram[0x3FFE00..0x3FFE09], b"MOCK GAME");

        // Boot indicator flags set
        assert_eq!(mmu.read_byte_arm9(0x027FFFC0), 0x66);
        assert_eq!(mmu.read_byte_arm7(0x027FFFC4), 0x66);

        // Verify CPU registers and pipeline priming
        assert_eq!(arm9.cpu.registers.cpsr, 0x1F);
        assert_eq!(arm9.cpu.registers.gpr[13], 0x03002F00); // SP
        assert_eq!(arm9.cpu.registers.gpr[15], arm9_ram_addr + 8); // PC
        assert_eq!(arm9.cpu.pipeline[0], instr1);
        assert_eq!(arm9.cpu.pipeline[1], instr2);

        assert_eq!(arm7.cpu.registers.cpsr, 0x1F);
        assert_eq!(arm7.cpu.registers.gpr[13], 0x0380FFFC); // SP
        assert_eq!(arm7.cpu.registers.gpr[15], arm7_ram_addr + 8); // PC
        assert_eq!(arm7.cpu.pipeline[0], arm7_instr);
    }

    #[test]
    fn test_nds_cartridge_load_and_boot_with_autoload() {
        let mut mmu = NdsMmu::new();
        let mut arm9 = Arm9Cpu::new();
        let mut arm7 = Arm7Cpu::new();

        // Construct a mock ROM image
        let mut rom = vec![0u8; 0x1000];
        
        rom[0..9].copy_from_slice(b"MOCK GAME");
        rom[0x0C..0x10].copy_from_slice(b"MCKG");

        // ARM9 binary: rom offset = 0x200, ram address = 0x02000800, size = 0x200
        let arm9_rom_offset: u32 = 0x200;
        let arm9_ram_addr: u32 = 0x02000800;
        let arm9_size: u32 = 0x200;
        rom[0x20..0x24].copy_from_slice(&arm9_rom_offset.to_le_bytes());
        rom[0x24..0x28].copy_from_slice(&arm9_ram_addr.to_le_bytes()); // entry point
        rom[0x28..0x2C].copy_from_slice(&arm9_ram_addr.to_le_bytes()); // ram address
        rom[0x2C..0x30].copy_from_slice(&arm9_size.to_le_bytes());

        // Set autoload info address to 0x02000900 (offset 0x100 in ARM9 binary)
        let arm9_autoload_info: u32 = 0x02000900;
        rom[0x74..0x78].copy_from_slice(&arm9_autoload_info.to_le_bytes());

        // In ARM9 binary at offset 0x100 (which is 0x200 + 0x100 = 0x300 in rom)
        // Autoload Entry 1: dest=0x02300000, size=16, bss_size=8
        let dest_addr: u32 = 0x02300000;
        let size: u32 = 16;
        let bss_size: u32 = 8;
        rom[0x300..0x304].copy_from_slice(&dest_addr.to_le_bytes());
        rom[0x304..0x308].copy_from_slice(&size.to_le_bytes());
        rom[0x308..0x30C].copy_from_slice(&bss_size.to_le_bytes());

        // Autoload Entry 2: dest=0 (terminator)
        rom[0x30C..0x310].copy_from_slice(&0u32.to_le_bytes());

        // Autoload data is placed immediately after ARM9 binary in ROM:
        // current_rom_src = arm9_rom_offset + arm9_size = 0x200 + 0x200 = 0x400
        let autoload_data = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        rom[0x400..0x410].copy_from_slice(&autoload_data);

        // ARM7 binary starts after the autoload data. Let's put it at 0x500.
        // ARM7 binary: rom offset = 0x500, ram address = 0x03800000, size = 0x100
        let arm7_rom_offset: u32 = 0x500;
        let arm7_ram_addr: u32 = 0x03800000;
        let arm7_size: u32 = 0x100;
        rom[0x30..0x34].copy_from_slice(&arm7_rom_offset.to_le_bytes());
        rom[0x34..0x38].copy_from_slice(&arm7_ram_addr.to_le_bytes()); // entry point
        rom[0x38..0x3C].copy_from_slice(&arm7_ram_addr.to_le_bytes()); // ram address
        rom[0x3C..0x40].copy_from_slice(&arm7_size.to_le_bytes());

        // Fill ARM7 binary
        let arm7_instr: u32 = 0xE12FFF11;
        rom[0x500..0x504].copy_from_slice(&arm7_instr.to_le_bytes());

        // Perform HLE boot load
        let res = hle::boot_load_rom(&mut mmu, &mut arm9, &mut arm7, &rom);
        assert!(res.is_ok());

        // Verify autoload data was copied to dest_addr
        for i in 0..16 {
            assert_eq!(mmu.read_byte_arm9(dest_addr + i as u32), autoload_data[i]);
        }
        // Verify BSS section was cleared to 0
        for i in 0..8 {
            assert_eq!(mmu.read_byte_arm9(dest_addr + 16 + i as u32), 0);
        }
    }

    /// The contiguous fast path must be invisible: every 16- and 32-bit ARM9
    /// read has to equal the byte-by-byte composition it replaced, at every
    /// address, including the ones the fast path must decline.
    ///
    /// This is the test that makes the optimisation safe to keep. It compares
    /// against `read_byte_arm9` rather than against literals, so it stays honest
    /// if the decode itself is ever changed — the property is "same answer",
    /// not "this particular answer".
    #[test]
    fn word_reads_match_the_byte_path_everywhere() {
        let mut mmu = NdsMmu::new();
        for (i, b) in mmu.main_ram.iter_mut().enumerate() {
            *b = (i as u8) ^ 0x5A;
        }
        for (i, b) in mmu.itcm.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(3) ^ 0x11;
        }
        for (i, b) in mmu.dtcm.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7) ^ 0x22;
        }
        // ITCM at 0x01000000 (32 KB), DTCM at 0x0B000000 (16 KB), both enabled.
        let mut cp15 = mmu.cp15();
        cp15.itcm_control = 0x0100_0000 | (6 << 1);
        cp15.dtcm_control = 0x0B00_0000 | (5 << 1);
        cp15.control |= (1 << 18) | (1 << 16);
        mmu.set_cp15(cp15);

        let main_top = 0x0200_0000 + 4 * 1024 * 1024;
        let addrs = [
            0x0200_0000,      // main RAM, aligned, fast path
            0x0200_0001,      // main RAM, unaligned, fast path
            0x0201_2345,      // main RAM, deep inside
            main_top - 4,     // last word wholly inside main RAM
            main_top - 2,     // straddles the top: fast path must decline
            main_top - 1,
            0x0240_0000,      // the shared-WRAM window inside the 0x02 arm
            0x0248_0000,      // above it: the mirroring fallback
            0x0400_0004,      // I/O (DISPSTAT) — never fast-pathed
            0x0500_0000,      // palette
            0x0100_0000,      // ITCM base
            0x0100_7FFE,      // ITCM, straddles the end of the 32 KB window
            0x0B00_0000,      // DTCM base
            0x0B00_3FFE,      // DTCM, straddles the end of the 16 KB window
            0x0000_0000,      // BIOS
            0xFFFF_0018,      // high vectors
        ];
        for &a in &addrs {
            let want_w = u32::from(mmu.read_byte_arm9(a))
                | (u32::from(mmu.read_byte_arm9(a.wrapping_add(1))) << 8)
                | (u32::from(mmu.read_byte_arm9(a.wrapping_add(2))) << 16)
                | (u32::from(mmu.read_byte_arm9(a.wrapping_add(3))) << 24);
            assert_eq!(mmu.read_word_arm9(a), want_w, "word read at {a:#010x}");
            let want_h = u16::from(mmu.read_byte_arm9(a))
                | (u16::from(mmu.read_byte_arm9(a.wrapping_add(1))) << 8);
            assert_eq!(mmu.read_halfword_arm9(a), want_h, "halfword read at {a:#010x}");
        }

        // With the TCMs disabled the same addresses must decode as ordinary
        // memory — the fast path must consult the enable bits, not just the range.
        let mut cp15 = mmu.cp15();
        cp15.control &= !((1 << 18) | (1 << 16));
        mmu.set_cp15(cp15);
        for &a in &[0x0100_0000u32, 0x0B00_0000] {
            let want = u32::from(mmu.read_byte_arm9(a))
                | (u32::from(mmu.read_byte_arm9(a.wrapping_add(1))) << 8)
                | (u32::from(mmu.read_byte_arm9(a.wrapping_add(2))) << 16)
                | (u32::from(mmu.read_byte_arm9(a.wrapping_add(3))) << 24);
            assert_eq!(mmu.read_word_arm9(a), want, "TCM disabled at {a:#010x}");
        }
    }

    /// The write-side fast path must be invisible in exactly the same sense:
    /// every 16- and 32-bit store must leave memory where the byte-by-byte
    /// decomposition left it, at every address, including the ones the fast path
    /// must decline (the mirror straddles, the shared-WRAM window, I/O).
    ///
    /// Two emulators are stepped in lockstep rather than comparing against
    /// literals, so this stays true if the decode itself is ever changed. The
    /// readback goes through `read_byte_*`, whose own agreement with the wide
    /// accessors is pinned by the two read tests above.
    #[test]
    fn word_writes_match_the_byte_path_everywhere() {
        // Same map on both sides, including the TCMs, so the store has somewhere
        // to land in every arm the fast path can take.
        let configure = || {
            let mut mmu = NdsMmu::new();
            let mut cp15 = mmu.cp15();
            cp15.itcm_control = 0x0100_0000 | (6 << 1);
            cp15.dtcm_control = 0x0B00_0000 | (5 << 1);
            cp15.control |= (1 << 18) | (1 << 16);
            mmu.set_cp15(cp15);
            mmu
        };

        let main_top: u32 = 0x0200_0000 + 4 * 1024 * 1024;
        let arm9_addrs = [
            0x0200_0000,  // main RAM, fast path
            0x0201_2344,  // main RAM, deep inside
            main_top - 4, // last word wholly inside main RAM
            main_top - 2, // straddles the top: fast path must decline
            0x0240_0000,  // shared-WRAM window inside the 0x02 arm
            0x0248_0000,  // above it: the mirroring fallback
            0x0100_0000,  // ITCM base
            0x0100_7FFE,  // ITCM, straddles the end of the window
            0x0B00_0000,  // DTCM base
            0x0B00_3FFE,  // DTCM, straddles the end of the window
            0x0500_0000,  // palette — not a fast-path region
            0x0600_0000,  // VRAM — bank routing, never fast-pathed
        ];
        for &a in &arm9_addrs {
            let (mut fast, mut slow) = (configure(), configure());
            fast.write_word_arm9(a, 0x1234_5678);
            slow.write_byte_arm9(a, 0x78);
            slow.write_byte_arm9(a.wrapping_add(1), 0x56);
            slow.write_byte_arm9(a.wrapping_add(2), 0x34);
            slow.write_byte_arm9(a.wrapping_add(3), 0x12);
            for i in 0..4u32 {
                let at = a.wrapping_add(i);
                assert_eq!(
                    fast.read_byte_arm9(at),
                    slow.read_byte_arm9(at),
                    "arm9 word store at {a:#010x}, byte {i}"
                );
            }

            let (mut fast, mut slow) = (configure(), configure());
            fast.write_halfword_arm9(a, 0xBEEF);
            slow.write_byte_arm9(a, 0xEF);
            slow.write_byte_arm9(a.wrapping_add(1), 0xBE);
            for i in 0..2u32 {
                let at = a.wrapping_add(i);
                assert_eq!(
                    fast.read_byte_arm9(at),
                    slow.read_byte_arm9(at),
                    "arm9 halfword store at {a:#010x}, byte {i}"
                );
            }
        }

        let arm7_addrs = [
            0x0200_0000,
            0x0203_FFFC,
            0x0380_0000,  // private WRAM base
            0x0380_FFFE,  // private WRAM, straddles the 64 KB end
            0x0300_0000,  // shared WRAM: WRAMCNT-dependent, must decline
            0x0400_0004,  // I/O
        ];
        for &a in &arm7_addrs {
            let (mut fast, mut slow) = (NdsMmu::new(), NdsMmu::new());
            fast.write_word_arm7(a, 0x1234_5678);
            slow.write_byte_arm7(a, 0x78);
            slow.write_byte_arm7(a.wrapping_add(1), 0x56);
            slow.write_byte_arm7(a.wrapping_add(2), 0x34);
            slow.write_byte_arm7(a.wrapping_add(3), 0x12);
            for i in 0..4u32 {
                let at = a.wrapping_add(i);
                assert_eq!(
                    fast.read_byte_arm7(at),
                    slow.read_byte_arm7(at),
                    "arm7 word store at {a:#010x}, byte {i}"
                );
            }

            let (mut fast, mut slow) = (NdsMmu::new(), NdsMmu::new());
            fast.write_halfword_arm7(a, 0xBEEF);
            slow.write_byte_arm7(a, 0xEF);
            slow.write_byte_arm7(a.wrapping_add(1), 0xBE);
            for i in 0..2u32 {
                let at = a.wrapping_add(i);
                assert_eq!(
                    fast.read_byte_arm7(at),
                    slow.read_byte_arm7(at),
                    "arm7 halfword store at {a:#010x}, byte {i}"
                );
            }
        }

        // An armed watch must force the byte path, or the diagnostic silently
        // stops seeing the stores it exists to record.
        let mut watched = configure();
        watched.ram_write_watch = Some((0x0200_0000, 0x0200_0010));
        assert!(
            watched.contiguous_arm9_mut(0x0200_0000, 4).is_none(),
            "an armed ram_write_watch must disable the write fast path"
        );
    }

    /// ARM7 half of the same property. The ARM7 decode differs (no TCM, the
    /// 0x02 arm always mirrors, and 0x03 splits between shared and private
    /// WRAM), so it needs its own address list rather than sharing the ARM9 one.
    #[test]
    fn arm7_word_reads_match_the_byte_path_everywhere() {
        let mut mmu = NdsMmu::new();
        for (i, b) in mmu.main_ram.iter_mut().enumerate() {
            *b = (i as u8) ^ 0x3C;
        }
        for (i, b) in mmu.arm7_wram.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(5) ^ 0x44;
        }
        for (i, b) in mmu.shared_wram.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(9) ^ 0x66;
        }
        let addrs = [
            0x0200_0000u32,          // main RAM
            0x0200_0003,             // unaligned
            0x0240_0000,             // above 4 MB: folds through the mirror
            0x023F_FFFE,             // straddles the top of main RAM
            0x0300_0000,             // shared WRAM — never fast-pathed
            0x0380_0000,             // private ARM7 WRAM base
            0x0380_FFFE,             // straddles the end of the 64 KB window
            0x0400_0004,             // I/O
            0x0400_01C0,             // SPICNT: has a halfword special case
            0x0000_0000,             // BIOS
        ];
        for &a in &addrs {
            let want_w = u32::from(mmu.read_byte_arm7(a))
                | (u32::from(mmu.read_byte_arm7(a.wrapping_add(1))) << 8)
                | (u32::from(mmu.read_byte_arm7(a.wrapping_add(2))) << 16)
                | (u32::from(mmu.read_byte_arm7(a.wrapping_add(3))) << 24);
            assert_eq!(mmu.read_word_arm7(a), want_w, "word read at {a:#010x}");
        }
        // Halfword reads, minus the two SPI addresses whose documented contract
        // is to bypass the byte path entirely.
        for &a in &addrs {
            if a == 0x0400_01C0 {
                continue;
            }
            let want_h = u16::from(mmu.read_byte_arm7(a))
                | (u16::from(mmu.read_byte_arm7(a.wrapping_add(1))) << 8);
            assert_eq!(mmu.read_halfword_arm7(a), want_h, "halfword read at {a:#010x}");
        }
        // The WRAMCNT-dependent shared window must still reach its decoder for
        // every mode, which is exactly why the fast path declines it.
        for mode in 0..4u8 {
            mmu.wram_control = mode;
            let a = 0x0300_1000;
            let want = u32::from(mmu.read_byte_arm7(a))
                | (u32::from(mmu.read_byte_arm7(a + 1)) << 8)
                | (u32::from(mmu.read_byte_arm7(a + 2)) << 16)
                | (u32::from(mmu.read_byte_arm7(a + 3)) << 24);
            assert_eq!(mmu.read_word_arm7(a), want, "shared WRAM mode {mode}");
        }
    }

    #[test]
    fn test_tcm_range_check_overflow_guards() {
        let mut mmu = NdsMmu::new();
        // ITCM base 0xFFFF0000 (very high; window wraps the address space), size
        // 32KB (N=6). DTCM base 0x00001000 (low), size 16KB (N=5). Base+size in
        // the region registers; the range check folds via wrapping_sub.
        let mut cp15 = mmu.cp15();
        cp15.itcm_control = 0xFFFF0000 | (6 << 1);
        cp15.dtcm_control = 0x00001000 | (5 << 1);
        mmu.set_cp15(cp15);

        // ITCM = 32KB window from base, no panic on wrap.
        assert!(mmu.in_itcm_range_arm9(0xFFFF0000)); // offset 0
        assert!(mmu.in_itcm_range_arm9(0xFFFF_0000u32.wrapping_add(0x7FFF))); // last in
        assert!(!mmu.in_itcm_range_arm9(0xFFFF_0000u32.wrapping_add(0x8000))); // first out
        assert!(!mmu.in_itcm_range_arm9(0xFEFF_0000)); // far below base wraps -> out

        // DTCM = 16KB window at 0x00001000.
        assert!(mmu.in_dtcm_range_arm9(0x00001000)); // offset 0
        assert!(mmu.in_dtcm_range_arm9(0x00001000 + 0x3FFF)); // last in
        assert!(!mmu.in_dtcm_range_arm9(0x00001000 + 0x4000)); // first out
        assert!(!mmu.in_dtcm_range_arm9(0x00000FFF)); // underflow wraps -> out
    }

    /// Immediate-timing DMA must copy the words and then clear the enable bit,
    /// otherwise the boot code's `ldr r0,[DMAxCNT]; tst r0,#0x80000000; bne`
    /// completion poll spins forever (the SoulSilver ARM9 boot hang).
    /// WRAMCNT's four modes are monotonic (GBATEK "DS Memory Control - WRAM"):
    /// the ARM9 loses memory as the value rises, 0 = ARM9 all, 1 = ARM9 upper
    /// half, 2 = ARM9 lower half, 3 = ARM9 none. Modes 1 and 3 were transposed,
    /// so `hle.rs` writing 3 to hand the window to the ARM7 got a split instead.
    /// Asserted through the public accessors rather than on the backing `Vec`,
    /// so it stays true if the storage is ever resized to the DS's real 32 KB.
    #[test]
    fn wramcnt_modes_follow_the_gbatek_allocation_table() {
        let probe = |mode: u8| -> (bool, bool) {
            let mut mmu = NdsMmu::new();
            // Write through each core's own window, then read it back through
            // the same window: "mapped" means a store survives a load.
            mmu.wram_control = mode;
            mmu.write_byte_arm9(0x0240_0000, 0xA9);
            mmu.write_byte_arm7(0x0300_0000, 0xA7);
            (
                mmu.read_byte_arm9(0x0240_0000) == 0xA9,
                mmu.read_byte_arm7(0x0300_0000) == 0xA7,
            )
        };
        // Mode 0: all to the ARM9. Mode 3: all to the ARM7.
        assert_eq!(probe(0).0, true, "mode 0 must map the window to the ARM9");
        assert_eq!(probe(3).0, false, "mode 3 gives the ARM9 nothing");
        assert_eq!(probe(3).1, true, "mode 3 must map the whole window to the ARM7");
        // Mode 2 gives the ARM9 the LOWER half, which is the half its window
        // base addresses; mode 1 gives it the upper half, reached at +0x40000.
        assert_eq!(probe(2).0, true, "mode 2 maps the ARM9's lower half at its base");
        assert_eq!(probe(1).0, false, "mode 1 puts the ARM9's half at +0x40000, not its base");
        let mut mmu = NdsMmu::new();
        mmu.wram_control = 1;
        mmu.write_byte_arm9(0x0244_0000, 0x5A);
        assert_eq!(mmu.read_byte_arm9(0x0244_0000), 0x5A, "mode 1 = ARM9 upper half");
        // Both split modes must still leave the ARM7 mapped.
        assert!(probe(1).1 && probe(2).1, "the split modes map a half to the ARM7");
    }

    /// A completed SPI transfer requests IF bit 23 (NDS7 "SPI bus"), never
    /// bit 8 (DMA 0). All three completion sites used to raise bit 8, so every
    /// SPI byte asked the ARM7 — the core running the sound driver — to service
    /// a DMA0 interrupt that no DMA had produced. The touchscreen is polled once
    /// per frame, so this fired continuously.
    #[test]
    fn spi_transfer_completion_requests_the_spi_irq_not_dma0() {
        let mut mmu = NdsMmu::new();
        mmu.arm7_if = 0;
        // SPICNT: bus enable (15) + IRQ on completion (14).
        mmu.write_byte_arm7(0x0400_01C0, 0x00);
        mmu.write_byte_arm7(0x0400_01C1, 0xC0);
        mmu.write_byte_arm7(0x0400_01C2, 0x00); // write SPIDATA -> transfer

        assert_ne!(mmu.arm7_if & (1 << 23), 0, "SPI completion must raise IF bit 23");
        assert_eq!(mmu.arm7_if & (1 << 8), 0, "bit 8 is DMA 0, not SPI");

        // With the IRQ disabled (bit 14 clear) a transfer must request nothing.
        mmu.arm7_if = 0;
        mmu.write_byte_arm7(0x0400_01C1, 0x80); // bus enabled, IRQ off
        mmu.write_byte_arm7(0x0400_01C2, 0x00);
        assert_eq!(mmu.arm7_if, 0, "SPICNT bit 14 clear must not request an IRQ");
    }

    #[test]
    fn test_arm9_immediate_dma_copies_and_clears_enable() {
        let mut mmu = NdsMmu::new();
        // Known pattern at the source in Main RAM.
        for i in 0..4u32 {
            mmu.write_word_arm9(0x0200_0000 + i * 4, 0x1000_0000 + i);
        }
        // DMA3: SAD=0x02000000, DAD=0x02001000, then CNT = enable|32-bit|count 4.
        mmu.write_word_arm9(0x0400_00D4, 0x0200_0000); // SAD
        mmu.write_word_arm9(0x0400_00D8, 0x0200_1000); // DAD
        mmu.write_word_arm9(0x0400_00DC, 0x8400_0004); // enable(31)+word(26)+len 4, immediate

        for i in 0..4u32 {
            assert_eq!(
                mmu.read_word_arm9(0x0200_1000 + i * 4),
                0x1000_0000 + i,
                "DMA copied word {i}"
            );
        }
        assert_eq!(
            mmu.read_word_arm9(0x0400_00DC) & 0x8000_0000,
            0,
            "enable bit cleared on completion so the busy-wait exits"
        );
    }

    /// A non-immediate timing (here VBlank, start field = 1) must NOT run at
    /// enable time — it stays armed for the event scheduler. Guards against the
    /// DMA engine eagerly firing timed transfers during boot.
    #[test]
    fn test_arm9_non_immediate_dma_does_not_run_yet() {
        let mut mmu = NdsMmu::new();
        mmu.write_word_arm9(0x0200_0000, 0xDEAD_BEEF);
        mmu.write_word_arm9(0x0400_00D4, 0x0200_0000); // SAD
        mmu.write_word_arm9(0x0400_00D8, 0x0200_1000); // DAD
        // CNT: enable(31) + VBlank timing (bits 27-29 = 001) + word + len 1.
        mmu.write_word_arm9(0x0400_00DC, 0x8400_0001 | (1 << 27));
        assert_eq!(mmu.read_word_arm9(0x0200_1000), 0, "timed DMA must not copy at enable time");
    }

    /// Gamecard 0xB7 main-read: the block-start write must stream the requested ROM
    /// bytes out of the data port and clear ROMCTRL busy when drained — otherwise
    /// the boot code's `ldr [ROMCTRL]; tst #0x80000000; bne` spins forever (the
    /// SoulSilver cartridge hang).
    #[test]
    fn test_gamecard_b7_block_read_streams_and_clears_busy() {
        let mut mmu = NdsMmu::new();
        mmu.rom = vec![0u8; 0x2000];
        for i in 0..16u32 {
            mmu.rom[0x1000 + i as usize] = (0xA0 + i) as u8; // recognizable pattern
        }
        // Command 0xB7 + big-endian address 0x00001000.
        mmu.write_byte_arm9(0x0400_01A8, 0xB7);
        mmu.write_byte_arm9(0x0400_01A9, 0x00);
        mmu.write_byte_arm9(0x0400_01AA, 0x00);
        mmu.write_byte_arm9(0x0400_01AB, 0x10);
        mmu.write_byte_arm9(0x0400_01AC, 0x00);
        // ROMCTRL: block-size field 1 (=512B) + bit31 (block start).
        mmu.write_word_arm9(0x0400_01A4, 0x8000_0000 | (1 << 24));

        let rc = mmu.read_word_arm9(0x0400_01A4);
        assert_ne!(rc & (1 << 31), 0, "busy set after block start");
        assert_ne!(rc & (1 << 23), 0, "word-ready set while data remains");

        assert_eq!(mmu.gamecard_read_data(), u32::from_le_bytes([0xA0, 0xA1, 0xA2, 0xA3]));
        assert_eq!(mmu.gamecard_read_data(), u32::from_le_bytes([0xA4, 0xA5, 0xA6, 0xA7]));
        // Drain the rest of the 512-byte block (128 words total, 2 already read).
        for _ in 0..126 {
            mmu.gamecard_read_data();
        }
        assert_eq!(
            mmu.read_word_arm9(0x0400_01A4) & (1 << 31),
            0,
            "busy cleared once the block is fully drained"
        );
        assert_eq!(mmu.gamecard_read_data(), 0, "no data outside an active transfer");
    }

    /// A zero-length block (ROMCTRL block-size field 0) must complete immediately —
    /// busy cleared at start — so a "send command, no data" poll doesn't hang.
    #[test]
    fn test_gamecard_zero_length_block_completes_immediately() {
        let mut mmu = NdsMmu::new();
        mmu.rom = vec![0u8; 0x100];
        mmu.write_byte_arm9(0x0400_01A8, 0xB7);
        mmu.write_word_arm9(0x0400_01A4, 0x8000_0000); // start, block-size field 0
        assert_eq!(
            mmu.read_word_arm9(0x0400_01A4) & (1 << 31),
            0,
            "zero-length block clears busy at start"
        );
    }

    /// Command 0xB8 (get chip ID) must return a plausible Macronix ID encoding the
    /// ROM size, not ROM bytes or the idle-bus fill — games sanity-check it at init.
    #[test]
    fn test_gamecard_chip_id_command() {
        let mut mmu = NdsMmu::new();
        mmu.rom = vec![0u8; 0x0800_0000]; // 128 MB -> (0x80)-1 = 0x7F in bits 8-14
        mmu.write_byte_arm9(0x0400_01A8, 0xB8);
        mmu.write_word_arm9(0x0400_01A4, 0x8000_0000 | (7 << 24)); // start, 4-byte block
        assert_eq!(mmu.gamecard_read_data(), 0x0000_7FC2, "Macronix ID + 128MB size code");
        assert_eq!(
            mmu.read_word_arm9(0x0400_01A4) & (1 << 31),
            0,
            "busy cleared after the 4-byte chip-ID block"
        );
    }

    #[test]
    fn test_hle_autoload_and_ipc_robustness() {
        // --- 1. Autoload Copy Correctness & TCM ---
        let mut mmu = NdsMmu::new();
        let mut arm9 = Arm9Cpu::new();
        let mut arm7 = Arm7Cpu::new();

        // Construct mock ROM: size 0x2000
        let mut rom = vec![0u8; 0x2000];
        rom[0..9].copy_from_slice(b"MOCK GAME");
        rom[0x0C..0x10].copy_from_slice(b"MCKG");

        // ARM9 binary: offset = 0x200, ram = 0x02000800, size = 0x400
        let arm9_rom_offset: u32 = 0x200;
        let arm9_ram_addr: u32 = 0x02000800;
        let arm9_size: u32 = 0x400;
        rom[0x20..0x24].copy_from_slice(&arm9_rom_offset.to_le_bytes());
        rom[0x24..0x28].copy_from_slice(&arm9_ram_addr.to_le_bytes());
        rom[0x28..0x2C].copy_from_slice(&arm9_ram_addr.to_le_bytes());
        rom[0x2C..0x30].copy_from_slice(&arm9_size.to_le_bytes());

        // Autoload info address = 0x02000B00 (offset 0x300 in ARM9 binary)
        let arm9_autoload_info: u32 = 0x02000B00;
        rom[0x74..0x78].copy_from_slice(&arm9_autoload_info.to_le_bytes());

        // Prepare autoload entries at 0x200 + 0x300 = 0x500 in ROM
        // Entry 1: dest = 0x02100000 (Main RAM), size = 0x10, bss_size = 0x10
        let dest_1: u32 = 0x02100000;
        let size_1: u32 = 0x10;
        let bss_1: u32 = 0x10;
        rom[0x500..0x504].copy_from_slice(&dest_1.to_le_bytes());
        rom[0x504..0x508].copy_from_slice(&size_1.to_le_bytes());
        rom[0x508..0x50C].copy_from_slice(&bss_1.to_le_bytes());

        // Entry 2: dest = 0x01002000 (ITCM), size = 0x20, bss_size = 0x20
        let dest_2: u32 = 0x01002000;
        let size_2: u32 = 0x20;
        let bss_2: u32 = 0x20;
        rom[0x50C..0x510].copy_from_slice(&dest_2.to_le_bytes());
        rom[0x510..0x514].copy_from_slice(&size_2.to_le_bytes());
        rom[0x514..0x518].copy_from_slice(&bss_2.to_le_bytes());

        // Entry 3: dest = 0x0B001000 (DTCM), size = 0x30, bss_size = 0x30
        let dest_3: u32 = 0x0B001000;
        let size_3: u32 = 0x30;
        let bss_3: u32 = 0x30;
        rom[0x518..0x51C].copy_from_slice(&dest_3.to_le_bytes());
        rom[0x51C..0x520].copy_from_slice(&size_3.to_le_bytes());
        rom[0x520..0x524].copy_from_slice(&bss_3.to_le_bytes());

        // Entry 4: dest = 0 (Terminator)
        rom[0x524..0x528].copy_from_slice(&0u32.to_le_bytes());

        // Fill autoload data in ROM: starts at arm9_rom_offset + arm9_size = 0x600
        // Section 1: size_1 (16 bytes) starting at 0x600
        let data_1 = [0xA1; 16];
        rom[0x600..0x610].copy_from_slice(&data_1);

        // Section 2: size_2 (32 bytes) starting at 0x610
        let data_2 = [0xB2; 32];
        rom[0x610..0x630].copy_from_slice(&data_2);

        // Section 3: size_3 (48 bytes) starting at 0x630
        let data_3 = [0xC3; 48];
        rom[0x630..0x660].copy_from_slice(&data_3);

        // ARM7 binary: offset = 0x700, ram = 0x03800000, size = 0x100
        let arm7_rom_offset: u32 = 0x700;
        let arm7_ram_addr: u32 = 0x03800000;
        let arm7_size: u32 = 0x100;
        rom[0x30..0x34].copy_from_slice(&arm7_rom_offset.to_le_bytes());
        rom[0x34..0x38].copy_from_slice(&arm7_ram_addr.to_le_bytes());
        rom[0x38..0x3C].copy_from_slice(&arm7_ram_addr.to_le_bytes());
        rom[0x3C..0x40].copy_from_slice(&arm7_size.to_le_bytes());

        // Perform boot load
        let res = hle::boot_load_rom(&mut mmu, &mut arm9, &mut arm7, &rom);
        assert!(res.is_ok());

        // Verify Main RAM autoload copy
        for i in 0..16 {
            assert_eq!(mmu.read_byte_arm9(dest_1 + i), 0xA1);
        }
        for i in 0..16 {
            assert_eq!(mmu.read_byte_arm9(dest_1 + 16 + i), 0);
        }

        // Verify ITCM autoload copy
        for i in 0..32 {
            assert_eq!(mmu.read_byte_arm9(dest_2 + i), 0xB2);
        }
        for i in 0..32 {
            assert_eq!(mmu.read_byte_arm9(dest_2 + 32 + i), 0);
        }

        // Verify DTCM autoload copy
        for i in 0..48 {
            assert_eq!(mmu.read_byte_arm9(dest_3 + i), 0xC3);
        }
        for i in 0..48 {
            assert_eq!(mmu.read_byte_arm9(dest_3 + 48 + i), 0);
        }

        // --- 2. Autoload boundary cases & protection ---
        // Test BSS size >= 8MB is not zeroed (it would timeout or panic)
        mmu.reset();
        // Setup autoload table entry with bss_size = 8MB
        let dest_large: u32 = 0x02100000;
        let size_large: u32 = 4;
        let bss_large: u32 = 8 * 1024 * 1024;
        rom[0x500..0x504].copy_from_slice(&dest_large.to_le_bytes());
        rom[0x504..0x508].copy_from_slice(&size_large.to_le_bytes());
        rom[0x508..0x50C].copy_from_slice(&bss_large.to_le_bytes());
        rom[0x50C..0x510].copy_from_slice(&0u32.to_le_bytes()); // terminator

        let data_large = [0xD4; 4];
        rom[0x600..0x604].copy_from_slice(&data_large);

        let res = hle::boot_load_rom(&mut mmu, &mut arm9, &mut arm7, &rom);
        // A BSS of 8MB must be SKIPPED: boot completing (rather than looping 8M
        // times / running off the end of Main RAM) is what verifies the guard.
        assert!(res.is_ok());
        assert_eq!(mmu.read_byte_arm9(dest_large), 0xD4);
        // (A "marker survives" check can't work here: boot_load_rom resets Main
        // RAM to 0 before autoload, so any pre-seeded byte is cleared by the
        // reset, not by the BSS loop — the skip is verified by boot completing.)

        // Test BSS size = 8MB - 1 IS zeroed
        mmu.reset();
        let bss_almost_large: u32 = 8 * 1024 * 1024 - 1;
        rom[0x508..0x50C].copy_from_slice(&bss_almost_large.to_le_bytes());
        let res = hle::boot_load_rom(&mut mmu, &mut arm9, &mut arm7, &rom);
        assert!(res.is_ok());

        // Test malformed autoload_info underflow
        mmu.reset();
        let bad_autoload_info = arm9_ram_addr - 4; // underflow checked_sub
        rom[0x74..0x78].copy_from_slice(&bad_autoload_info.to_le_bytes());
        let res = hle::boot_load_rom(&mut mmu, &mut arm9, &mut arm7, &rom);
        assert!(res.is_ok()); // Should skip autoload and boot successfully

        // Test malformed autoload_info pointing out of binary bounds
        mmu.reset();
        let bad_autoload_info = arm9_ram_addr + arm9_size + 10;
        rom[0x74..0x78].copy_from_slice(&bad_autoload_info.to_le_bytes());
        let res = hle::boot_load_rom(&mut mmu, &mut arm9, &mut arm7, &rom);
        assert!(res.is_ok()); // Should break safely and boot successfully

        // --- 3. IPC FIFO flag status transitions and interrupts ---
        mmu.reset();
        // Enable FIFOs on both sides
        mmu.write_ipc_fifo_cnt_arm9(0x8000);
        mmu.write_ipc_fifo_cnt_arm7(0x8000);

        // Check initial state
        let cnt9 = mmu.read_ipc_fifo_cnt_arm9();
        assert!((cnt9 & 1) != 0);       // Send FIFO empty
        assert!((cnt9 & 0x0100) != 0);  // Recv FIFO empty

        // Set Recv interrupt enable on ARM7
        mmu.write_ipc_fifo_cnt_arm7(0x8000 | (1 << 10));

        // Write 1 word to empty TX from ARM9
        mmu.write_ipc_fifo_tx_arm9(0x12345678);
        assert_eq!(mmu.read_ipc_fifo_cnt_arm9() & 1, 0); // Not empty anymore
        // ARM7's receive FIFO became non-empty -> Recv-FIFO-Not-Empty IRQ = bit 18.
        assert_ne!(mmu.arm7_if & (1 << 18), 0);
        assert_eq!(mmu.arm7_if & (1 << 17), 0, "must not raise Send-Empty (bit 17)");

        // Set Send Empty interrupt enable on ARM9 (bit 2)
        mmu.write_ipc_fifo_cnt_arm9(0x8000 | (1 << 2));

        // Read the word from ARM7 (this makes the FIFO empty)
        let val = mmu.read_ipc_fifo_rx_arm7();
        assert_eq!(val, 0x12345678);
        assert!((mmu.read_ipc_fifo_cnt_arm9() & 1) != 0); // Empty again
        // ARM9's send FIFO drained -> Send-FIFO-Empty IRQ = bit 17.
        assert_ne!(mmu.arm9_if & (1 << 17), 0);
        assert_eq!(mmu.arm9_if & (1 << 18), 0, "must not raise Recv-Not-Empty (bit 18)");

        // Fill FIFO 9->7 to capacity (16 words)
        for i in 0..16 {
            mmu.write_ipc_fifo_tx_arm9(i);
        }
        let cnt9 = mmu.read_ipc_fifo_cnt_arm9();
        assert!((cnt9 & 2) != 0); // Send FIFO full
        let cnt7 = mmu.read_ipc_fifo_cnt_arm7();
        assert!((cnt7 & 0x0200) != 0); // Recv FIFO full on ARM7

        // Write 17th word, check error flag (bit 14)
        mmu.write_ipc_fifo_tx_arm9(99);
        assert!((mmu.read_ipc_fifo_cnt_arm9() & (1 << 14)) != 0);

        // Clear error flag by writing 1 to bit 14
        mmu.write_ipc_fifo_cnt_arm9(0x8000 | (1 << 14));
        assert_eq!(mmu.read_ipc_fifo_cnt_arm9() & (1 << 14), 0);

        // Try reading empty FIFO on ARM7, check error flag
        mmu.ipc.fifo_9to7.clear();
        mmu.read_ipc_fifo_rx_arm7();
        assert!((mmu.read_ipc_fifo_cnt_arm7() & (1 << 14)) != 0);

        // Clear it
        mmu.write_ipc_fifo_cnt_arm7(0x8000 | (1 << 14));
        assert_eq!(mmu.read_ipc_fifo_cnt_arm7() & (1 << 14), 0);

        // Test disable behavior (writing 0) clears FIFOs and bits 2/10
        mmu.write_ipc_fifo_cnt_arm9(0x8000 | (1 << 2) | (1 << 10));
        mmu.write_ipc_fifo_tx_arm9(42);
        assert_eq!(mmu.ipc.fifo_9to7.len(), 1);

        mmu.write_ipc_fifo_cnt_arm9(0); // Disable
        assert_eq!(mmu.ipc.fifo_9to7.len(), 0);
        let disabled_cnt = mmu.read_ipc_fifo_cnt_arm9();
        assert_eq!(disabled_cnt & (1 << 15), 0);
        assert_eq!(disabled_cnt & (1 << 2), 0);
        assert_eq!(disabled_cnt & (1 << 10), 0);

        // --- 4. IPCSYNC interrupt triggering ---
        mmu.reset();
        // ARM7 enables Sync interrupt
        mmu.write_ipcsync_arm7(1 << 14); // Set bit 14
        // ARM9 sends Sync interrupt (write 1 to bit 13)
        mmu.write_ipcsync_arm9(1 << 13);
        // ARM7 should have triggered IPC Sync IRQ (bit 16)
        assert_ne!(mmu.arm7_if & (1 << 16), 0);

        // Symmetric check: ARM9 enables Sync interrupt, ARM7 sends
        mmu.reset();
        mmu.write_ipcsync_arm9(1 << 14);
        mmu.write_ipcsync_arm7(1 << 13);
        assert_ne!(mmu.arm9_if & (1 << 16), 0);
    }

    /// GXSTAT HLE: SoulSilver's G3X_Reset does `LDR;ORR #0x8000;STR` then
    /// polls bit14 (stack error) until clear — the readback must never echo
    /// written flag bits (a hardwired 0xC0 in byte1 froze the game pre-title).
    /// Reads report: FIFO empty + less-than-half, engine idle, stack level 0,
    /// no error; only the FIFO-IRQ mode (bits 30-31) persists from writes.
    #[test]
    fn test_gxstat_hle_read_never_reports_error_or_busy() {
        let mut mmu = NdsMmu::new();
        mmu.write_word_arm9(0x0400_0600, 0x0000_C000); // the game's error-ack write
        // bit1 = BOX_TEST result = 1 (inside view); no error/busy bits.
        assert_eq!(
            mmu.read_word_arm9(0x0400_0600),
            0x0600_0002,
            "box-test-inside only; no error/busy; FIFO reads empty + <half"
        );
        // Selecting FIFO IRQ mode 1 (<half): mode reads back, IRQ latches at
        // once (the HLE FIFO is permanently empty, the condition holds).
        mmu.write_byte_arm9(0x0400_0603, 0x40);
        assert_eq!(mmu.read_word_arm9(0x0400_0600), 0x4600_0002);
        assert_ne!(mmu.arm9_if & (1 << 21), 0, "GX FIFO IRQ must latch on mode select");
        // Every GX command push re-raises it (FIFO drains instantly).
        mmu.arm9_if = 0;
        mmu.write_word_arm9(0x0400_0400, 0x1234_5678);
        assert_ne!(mmu.arm9_if & (1 << 21), 0, "GX FIFO IRQ must latch on push");
        assert!(mmu.has_3d_activity);
    }

    /// APU: a PCM8 one-shot channel plays its LEN and stops — busy (bit 31 of
    /// SOUNDxCNT byte 3) reads 1 while playing, 0 after — and the mixed output
    /// reaching the resampler is audibly non-silent for a +/-full-scale square.
    #[test]
    fn test_apu_pcm8_one_shot_mixes_then_clears_busy() {
        let mut mmu = NdsMmu::new();
        // 32-byte +/-127 square at 0x02000000 (period 8 source samples).
        for i in 0..32u32 {
            mmu.write_byte_arm7(0x0200_0000 + i, if (i / 4) % 2 == 0 { 0x7F } else { 0x81 });
        }
        mmu.apu_write_byte(0x500, 0x7F); // master volume
        mmu.apu_write_byte(0x501, 0x80); // master enable
        let base = 0x400; // channel 0
        for (off, b) in [(4u32, 0x00u8), (5, 0x00), (6, 0x00), (7, 0x02)] {
            mmu.apu_write_byte(base + off, b); // SAD = 0x02000000
        }
        mmu.apu_write_byte(base + 8, 0x00); // TMR = 0xFD00 -> 1536 cycles/sample
        mmu.apu_write_byte(base + 9, 0xFD);
        mmu.apu_write_byte(base + 0xA, 0);
        mmu.apu_write_byte(base + 0xB, 0); // PNT = 0
        mmu.apu_write_byte(base + 0xC, 8); // LEN = 8 words = 32 samples
        mmu.apu_write_byte(base + 0, 0x7F); // vol 127
        mmu.apu_write_byte(base + 1, 0x00); // div /1
        mmu.apu_write_byte(base + 2, 0x40); // pan center
        mmu.apu_write_byte(base + 3, 0x90); // start + repeat mode 2 (one-shot)
        assert_ne!(mmu.read_byte_arm7(0x0400_0403) & 0x80, 0, "busy while playing");

        let mut buf = vec![0i16; 4096];
        // Drive with the run loop's 64-cycle cadence; 32 samples * 1536 cycles
        // = ~49k cycles, run ~80k so the one-shot completes.
        for _ in 0..1250 {
            mmu.tick_apu(64, &mut buf, 0, 1.0);
        }
        // A one-shot self-stops partway through, and a silent APU has no next
        // edge — so the tail sits deferred until a flush, exactly as it does
        // inside a real tick. The run loop flushes once per frame; do the same
        // before reading `sample_count`.
        mmu.flush_apu(&mut buf, 0, 1.0);
        let n = mmu.apu.resampler.sample_count;
        assert!(n >= 80, "~110 output samples expected for 80k cycles, got {n}");
        assert!(
            buf[..n * 2].iter().any(|&s| s.abs() > 100),
            "square wave must reach the output buffer non-silently"
        );
        assert!(!mmu.apu.channels[0].active, "one-shot must self-stop");
        assert_eq!(
            mmu.read_byte_arm7(0x0400_0403) & 0x80,
            0,
            "busy must clear when LEN is consumed"
        );
    }

    /// APU: looping channels never clear busy (the BGM stream player polls
    /// this), and an ADPCM key-on latches the header sample + step index.
    #[test]
    fn test_apu_loop_stays_busy_and_adpcm_keyon_reads_header() {
        let mut mmu = NdsMmu::new();
        mmu.apu_write_byte(0x500, 0x7F);
        mmu.apu_write_byte(0x501, 0x80);
        // ADPCM header at 0x02000100: initial sample 0x1234, index 5.
        mmu.write_word_arm7(0x0200_0100, (5 << 16) | 0x1234);
        let base = 0x400 + 16; // channel 1
        for (off, b) in [(4u32, 0x00u8), (5, 0x01), (6, 0x00), (7, 0x02)] {
            mmu.apu_write_byte(base + off, b); // SAD = 0x02000100
        }
        mmu.apu_write_byte(base + 8, 0x00);
        mmu.apu_write_byte(base + 9, 0xFD);
        mmu.apu_write_byte(base + 0xA, 1);
        mmu.apu_write_byte(base + 0xB, 0); // PNT = 1 (the header word)
        mmu.apu_write_byte(base + 0xC, 2); // LEN = 2 words
        mmu.apu_write_byte(base + 3, 0xC8); // start + ADPCM (fmt 2) + loop (rep 1)
        let ch = &mmu.apu.channels[1];
        assert_eq!(ch.adpcm_val, 0x1234, "header sample latched on key-on");
        assert_eq!(ch.adpcm_idx, 5, "header step index latched on key-on");
        assert_eq!(ch.cursor, 4, "ADPCM data starts after the header word");

        let mut buf = vec![0i16; 4096];
        for _ in 0..2000 {
            mmu.tick_apu(64, &mut buf, 0, 1.0); // 128k cycles >> (PNT+LEN)*4 samples
        }
        assert!(mmu.apu.channels[1].active, "looping channel stays busy");
        assert_ne!(mmu.read_byte_arm7(0x0400_0413) & 0x80, 0);
        // Stopping via the start bit keys the channel off.
        mmu.apu_write_byte(base + 3, 0x48);
        assert!(!mmu.apu.channels[1].active);
        assert_eq!(mmu.read_byte_arm7(0x0400_0413) & 0x80, 0);
    }

    /// APU ADPCM loop continuity: crossing PNT snapshots the decoder, and the
    /// wrap restores EXACTLY that state (value, index, nibble phase, cursor) —
    /// any jump here is an audible click in looped BGM instruments.
    #[test]
    fn test_apu_adpcm_loop_wrap_resumes_from_snapshot() {
        let mut mmu = NdsMmu::new();
        mmu.apu_write_byte(0x500, 0x7F);
        mmu.apu_write_byte(0x501, 0x80);
        // Header @0x02000200 (sample 100, index 4); pre-loop data = 8x nibble
        // 0x7 (drives value+index up hard), loop body = 8x nibble 0x2.
        mmu.write_word_arm7(0x0200_0200, (4 << 16) | 100);
        for i in 0..4u32 {
            mmu.write_byte_arm7(0x0200_0204 + i, 0x77);
            mmu.write_byte_arm7(0x0200_0208 + i, 0x22);
        }
        let base = 0x400 + 2 * 16; // channel 2
        for (off, b) in [(4u32, 0x00u8), (5, 0x02), (6, 0x00), (7, 0x02)] {
            mmu.apu_write_byte(base + off, b); // SAD = 0x02000200
        }
        mmu.apu_write_byte(base + 8, 0x00); // TMR = 0xFF00 -> 512 cycles/sample
        mmu.apu_write_byte(base + 9, 0xFF);
        mmu.apu_write_byte(base + 0xA, 2); // PNT = 2 words (header + 4 data bytes)
        mmu.apu_write_byte(base + 0xB, 0);
        mmu.apu_write_byte(base + 0xC, 1); // LEN = 1 word -> total 12 bytes
        mmu.apu_write_byte(base + 0, 0x7F);
        mmu.apu_write_byte(base + 2, 0x40);
        mmu.apu_write_byte(base + 3, 0xC8); // start + ADPCM + loop

        let mut buf = vec![0i16; 4096];
        mmu.tick_apu(8 * 512, &mut buf, 0, 1.0); // decode the 8 pre-loop nibbles
        let ch = mmu.apu.channels[2];
        assert_eq!(ch.cursor, 8, "at the loop point after the pre-loop bytes");
        assert!(!ch.adpcm_high);
        assert_eq!(
            (ch.adpcm_loop_val, ch.adpcm_loop_idx),
            (ch.adpcm_val, ch.adpcm_idx),
            "snapshot taken exactly when first crossing PNT"
        );
        let snap = (ch.cursor, ch.adpcm_val, ch.adpcm_idx, ch.adpcm_high);
        assert!(ch.adpcm_val > 1000, "pre-loop nibbles moved the decoder");

        // Mid-loop the state must diverge from the snapshot (proves the wrap
        // assertion below isn't comparing a decoder that never moved)...
        mmu.tick_apu(4 * 512, &mut buf, 0, 1.0);
        assert_ne!(mmu.apu.channels[2].adpcm_val, snap.1);
        // ...and after the full loop body the wrap restores the snapshot.
        mmu.tick_apu(4 * 512, &mut buf, 0, 1.0);
        let ch = mmu.apu.channels[2];
        assert!(ch.active);
        assert_eq!(
            (ch.cursor, ch.adpcm_val, ch.adpcm_idx, ch.adpcm_high),
            snap,
            "loop wrap must resume from the PNT snapshot with no state jump"
        );
    }

    /// User-settings trailer per GBATEK: 0x70 = update counter, 0x72 = CRC16
    /// (poly 0xA001, init 0xFFFF) over bytes 0x00-0x6F, version byte = 5.
    /// The SDK validates this before trusting the block; an invalid trailer
    /// silently discarded the TP calibration, so TP_GetCalibratedPoint
    /// returned x=y=0 and every UI hit-test failed (U27 root cause).
    #[test]
    fn test_user_settings_trailer_is_counter_then_crc() {
        let s = hle::build_user_settings();
        assert_eq!(s[0x00], 5, "settings version must be 5");
        assert_eq!(
            u16::from_le_bytes([s[0x70], s[0x71]]),
            0,
            "0x70 holds the update counter, not a CRC"
        );
        let mut crc = 0xFFFFu16;
        for &b in &s[0..0x70] {
            crc ^= b as u16;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
            }
        }
        assert_eq!(u16::from_le_bytes([s[0x72], s[0x73]]), crc);
    }

    /// Touch: a synthetic tap at pixel (128,96) answered by the TSC over SPI
    /// must map back to (128,96)±1 through the SAME calibration block
    /// build_user_settings() hands the game (adc1/px1 + adc2/px2 pairs) —
    /// this is the whole ADC->pixel chain the ARM9 runs on real input.
    #[test]
    fn test_tsc_adc_roundtrips_through_user_calibration() {
        let mut mmu = NdsMmu::new();
        mmu.spi.tsc.touch_x = 128;
        mmu.spi.tsc.touch_y = 96;
        mmu.spi.tsc.touch_pressed = true;

        // One 12-bit conversion: control byte, then two data bytes (CS held).
        let mut convert = |ctrl: u8| -> u16 {
            mmu.write_halfword_arm7(0x040001C0, 0x8A00); // enable + hold + TSC
            mmu.write_byte_arm7(0x040001C2, ctrl);
            mmu.write_byte_arm7(0x040001C2, 0);
            let hi = mmu.read_byte_arm7(0x040001C2) as u16;
            mmu.write_byte_arm7(0x040001C2, 0);
            let lo = mmu.read_byte_arm7(0x040001C2) as u16;
            (hi << 5) | (lo >> 3)
        };
        let adc_x = convert(0xD0); // channel 5 = X
        let adc_y = convert(0x90); // channel 1 = Y

        // Calibration pairs exactly as the firmware user settings publish them.
        let s = hle::build_user_settings();
        let rd16 = |o: usize| u16::from_le_bytes([s[o], s[o + 1]]) as i32;
        let (adc_x1, adc_y1) = (rd16(0x58), rd16(0x5A));
        let (px1, py1) = (s[0x5C] as i32, s[0x5D] as i32);
        let (adc_x2, adc_y2) = (rd16(0x5E), rd16(0x60));
        let (px2, py2) = (s[0x62] as i32, s[0x63] as i32);
        let px = (adc_x as i32 - adc_x1) * (px2 - px1) / (adc_x2 - adc_x1) + px1;
        let py = (adc_y as i32 - adc_y1) * (py2 - py1) / (adc_y2 - adc_y1) + py1;
        assert!((px - 128).abs() <= 1, "X: adc={adc_x} -> px={px}, want 128±1");
        assert!((py - 96).abs() <= 1, "Y: adc={adc_y} -> px={py}, want 96±1");
    }

    /// AUXSPI wiring, driven exactly the way SoulSilver's ARM7 backup driver
    /// does it: select with AUXSPICNT = 0xA040 (enable | serial | CS-hold),
    /// clock command bytes through AUXSPIDATA, spin on the busy bit, read the
    /// response back, then deselect with 0. Before this wiring both AUXSPI
    /// registers fell through to a byte echo on the ARM7 and no chip existed.
    #[test]
    fn test_auxspi_backup_round_trip_via_arm7_driver_sequence() {
        let mut mmu = NdsMmu::new();
        mmu.backup = crate::nds::backup::NdsBackup::new(
            crate::nds::backup::BackupKind::Flash512K,
        );

        // The driver's own select value, low byte then high byte.
        let select = |m: &mut NdsMmu| {
            m.write_byte_arm7(0x0400_01A0, 0x40);
            m.write_byte_arm7(0x0400_01A1, 0xA0);
        };
        let deselect = |m: &mut NdsMmu| {
            m.write_byte_arm7(0x0400_01A0, 0x00);
            m.write_byte_arm7(0x0400_01A1, 0x00);
        };
        // Write a byte, then spin on AUXSPICNT bit 7 the way the driver does.
        let clock = |m: &mut NdsMmu, b: u8| -> u8 {
            m.write_byte_arm7(0x0400_01A2, b);
            let mut guard = 0;
            while m.read_halfword_arm7(0x0400_01A0) & 0x80 != 0 {
                guard += 1;
                assert!(guard < 16, "busy bit never cleared — driver would hang");
            }
            m.read_halfword_arm7(0x0400_01A2) as u8
        };

        select(&mut mmu);
        clock(&mut mmu, 0x06); // WREN
        deselect(&mut mmu);

        select(&mut mmu);
        clock(&mut mmu, 0x05); // RDSR
        assert_eq!(clock(&mut mmu, 0x00) & 0x02, 0x02, "WREN must set WEL");
        deselect(&mut mmu);

        select(&mut mmu);
        for b in [0x0A, 0x00, 0x20, 0x00, 0xC0, 0xFF, 0xEE] {
            clock(&mut mmu, b); // PW at 0x002000
        }
        deselect(&mut mmu);

        select(&mut mmu);
        for b in [0x03, 0x00, 0x20, 0x00] {
            clock(&mut mmu, b); // READ from 0x002000
        }
        let got = [clock(&mut mmu, 0), clock(&mut mmu, 0), clock(&mut mmu, 0)];
        deselect(&mut mmu);

        // The driver verifies every byte it writes; a mismatch here is exactly
        // the failure that made every in-game save retry forever.
        assert_eq!(got, [0xC0, 0xFF, 0xEE]);
        assert!(mmu.backup.is_dirty(), "a programmed chip must want persisting");
        assert!(!mmu.aux_bus_log.is_empty(), "census must see ARM7 traffic");
    }

    /// A 16-bit store to AUXSPIDATA must clock exactly ONE byte. Halfword
    /// writes decompose into two byte writes, so an inert high half is what
    /// keeps `STRH` (which is what the driver emits) from double-clocking.
    #[test]
    fn test_auxspi_halfword_write_clocks_exactly_one_byte() {
        let mut mmu = NdsMmu::new();
        mmu.backup = crate::nds::backup::NdsBackup::new(
            crate::nds::backup::BackupKind::Flash256K,
        );
        mmu.write_halfword_arm7(0x0400_01A0, 0xA040);
        mmu.write_halfword_arm7(0x0400_01A2, 0x009F); // RDID
        // Three response bytes: if the high half had clocked too, the identity
        // would be shifted by one and the first read would return 0x40.
        assert_eq!(mmu.read_halfword_arm7(0x0400_01A2) as u8, 0x00);
        mmu.write_halfword_arm7(0x0400_01A2, 0x0000);
        assert_eq!(mmu.read_halfword_arm7(0x0400_01A2) as u8, 0x20);
        mmu.write_halfword_arm7(0x0400_01A2, 0x0000);
        assert_eq!(mmu.read_halfword_arm7(0x0400_01A2) as u8, 0x40);
        mmu.write_halfword_arm7(0x0400_01A2, 0x0000);
        assert_eq!(mmu.read_halfword_arm7(0x0400_01A2) as u8, 0x12);
    }

    /// AUXSPICNT bit 6 means "deselect AFTER this transfer", not "chip-select is
    /// currently asserted". SoulSilver's driver clears it BEFORE clocking the
    /// last byte of a frame, so treating it as a live chip-select line split
    /// every multi-byte transaction into single-byte ones and made each response
    /// byte look like a new command opcode.
    ///
    /// This replays that exact control/data interleave, captured from the game.
    #[test]
    fn test_auxspi_hold_bit_deselects_after_the_transfer_not_on_the_control_write() {
        let mut mmu = NdsMmu::new();
        mmu.backup = crate::nds::backup::NdsBackup::new(
            crate::nds::backup::BackupKind::Flash512K,
        );
        // Frame: [RDID, dummy, dummy] with hold cleared before the final byte,
        // exactly the shape the driver uses.
        mmu.write_halfword_arm7(0x0400_01A0, 0xA042);
        mmu.write_halfword_arm7(0x0400_01A2, 0x009F);
        mmu.write_halfword_arm7(0x0400_01A0, 0xA040);
        mmu.write_halfword_arm7(0x0400_01A2, 0x0000);
        assert_eq!(
            mmu.read_halfword_arm7(0x0400_01A2) as u8,
            0x20,
            "second byte of the frame must be the manufacturer id"
        );
        // Hold cleared — the NEXT byte still transfers, and only then does CS drop.
        mmu.write_halfword_arm7(0x0400_01A0, 0xA000);
        mmu.write_halfword_arm7(0x0400_01A2, 0x0000);
        assert_eq!(
            mmu.read_halfword_arm7(0x0400_01A2) as u8,
            0x40,
            "third byte must continue the same frame, not start a new command"
        );

        // And the frame really did end: a further byte begins a new transaction,
        // so it is read as an (unknown) opcode rather than the third id byte.
        mmu.write_halfword_arm7(0x0400_01A0, 0xA040);
        mmu.write_halfword_arm7(0x0400_01A2, 0x0000);
        assert_eq!(mmu.read_halfword_arm7(0x0400_01A2) as u8, 0x00);
        assert_eq!(mmu.aux_cross_deselects, 0);
    }

    /// A control write must not end a transaction on its own; only powering the
    /// slot down (bit 15) does. This pins the halfword-decomposition path, where
    /// 0xA040 -> 0x0000 passes through the intermediate value 0xA000.
    #[test]
    fn test_auxspi_slot_disable_ends_the_transaction_exactly_once() {
        let mut mmu = NdsMmu::new();
        mmu.backup = crate::nds::backup::NdsBackup::new(
            crate::nds::backup::BackupKind::Flash512K,
        );
        mmu.write_halfword_arm7(0x0400_01A0, 0xA040);
        mmu.write_halfword_arm7(0x0400_01A2, 0x009F);
        // Re-programming baud mid-frame must not disturb the transaction.
        mmu.write_halfword_arm7(0x0400_01A0, 0xA042);
        mmu.write_halfword_arm7(0x0400_01A2, 0x0000);
        assert_eq!(mmu.read_halfword_arm7(0x0400_01A2) as u8, 0x20);
        mmu.write_halfword_arm7(0x0400_01A0, 0x0000);
        mmu.write_halfword_arm7(0x0400_01A0, 0xA040);
        mmu.write_halfword_arm7(0x0400_01A2, 0x0000);
        assert_eq!(
            mmu.read_halfword_arm7(0x0400_01A2) as u8,
            0x00,
            "slot disable must have ended the frame"
        );
    }

    /// Engine-A BG must use each bank's own GBATEK window. The generic matcher
    /// scaled OFS by the bank's size, which is right only for A-D: bank E
    /// ignores OFS entirely, and F/G use two independent OFS bits selecting
    /// 16K and 64K steps. It also matched MST 1 on banks H/I, which is
    /// engine-*B* BG, pulling the other engine's tiles into engine A.
    #[test]
    fn test_bg_a_uses_per_bank_windows_and_excludes_engine_b_banks() {
        let mut v = VramManager::new();
        // F at MST 1, OFS 2 -> base 0x10000 (not 2 * bank size = 0x8000).
        v.banks[5].control = 0x80 | 1 | (2 << 3);
        v.write_bg_a(0x10000, 0xF1);
        assert_eq!(v.banks[5].data[0], 0xF1, "F/G OFS.1 selects a 64K step");
        assert_eq!(v.read_bg_a(0x10000), 0xF1);
        assert_eq!(v.read_bg_a(0x8000), 0, "nothing is mapped at the old base");

        // E at MST 1 ignores OFS entirely and sits at 0 for its whole 64K.
        let mut v = VramManager::new();
        v.banks[4].control = 0x80 | 1 | (3 << 3);
        v.write_bg_a(0x0004, 0xEE);
        assert_eq!(v.banks[4].data[4], 0xEE, "bank E ignores OFS");

        // H at MST 1 is engine-B BG and must NOT answer engine-A reads.
        let mut v = VramManager::new();
        v.banks[7].control = 0x81;
        v.banks[7].data[0] = 0xBB;
        assert_eq!(v.read_bg_a(0x0000), 0, "H at MST 1 belongs to engine B");
        assert_eq!(v.read_bg_b(0x0000), 0xBB);
    }

    /// Engine-B BG must reach banks H and I, which carry it at MST 1 — a value
    /// that means engine-A BG on banks A-G, so the generic matcher this
    /// replaced could never see them and every engine-B tile fetch read 0.
    #[test]
    fn test_bg_b_resolves_banks_c_h_and_i_without_collision() {
        let mut v = VramManager::new();
        // C = engine-B BG (MST 4), H = engine-B BG (MST 1), I = engine-B BG
        // (MST 1, second window), D = engine-B OBJ (MST 4) — D must NOT answer.
        v.banks[2].control = 0x84;
        v.banks[7].control = 0x81;
        v.banks[8].control = 0x81;
        v.banks[3].control = 0x84;

        v.write_bg_b(0x0000, 0xC1); // bank C
        v.write_bg_b(0x0100, 0xC2);
        assert_eq!(v.read_bg_b(0x0000), 0xC1);
        assert_eq!(v.read_bg_b(0x0100), 0xC2);
        assert_eq!(v.banks[2].data[0], 0xC1, "must land in bank C");
        assert_eq!(v.banks[3].data[0], 0x00, "engine-B OBJ bank D untouched");

        // With C disabled, H owns the first 32K and I the 16K at 0x8000.
        v.banks[2].control = 0;
        v.write_bg_b(0x0004, 0xAA);
        v.write_bg_b(0x8004, 0xBB);
        assert_eq!(v.read_bg_b(0x0004), 0xAA);
        assert_eq!(v.read_bg_b(0x8004), 0xBB);
        assert_eq!(v.banks[7].data[4], 0xAA, "bank H at window base");
        assert_eq!(v.banks[8].data[4], 0xBB, "bank I offset by 0x8000");
        // Outside every mapped window reads 0 rather than aliasing a bank.
        assert_eq!(v.read_bg_b(0x1F000), 0);
    }

    /// A 32-bit store to AUXSPICNT must not reach AUXSPIDATA. Word writes
    /// decompose byte-wise, and 0x040001A2 is the very next byte — so a naive
    /// decomposition clocks a spurious byte into the save chip.
    #[test]
    fn test_auxspi_word_store_to_cnt_does_not_clock_the_chip() {
        let mut mmu = NdsMmu::new();
        mmu.backup = crate::nds::backup::NdsBackup::new(
            crate::nds::backup::BackupKind::Flash512K,
        );
        mmu.write_word_arm7(0x0400_01A0, 0x0000_A040);
        // Nothing clocked yet, so this byte must be read as the opcode.
        mmu.write_halfword_arm7(0x0400_01A2, 0x009F);
        mmu.write_halfword_arm7(0x0400_01A2, 0x0000);
        assert_eq!(
            mmu.read_halfword_arm7(0x0400_01A2) as u8,
            0x20,
            "RDID must be the first byte of the frame"
        );
    }

    /// The chip is on the slot only while the slot is enabled AND in serial
    /// mode; in parallel mode 0x040001A2 belongs to the gamecard interface.
    #[test]
    fn test_auxspi_data_is_ignored_when_the_slot_is_not_in_serial_mode() {
        let mut mmu = NdsMmu::new();
        mmu.backup = crate::nds::backup::NdsBackup::new(
            crate::nds::backup::BackupKind::Flash512K,
        );
        // Enabled (bit 15) + hold (bit 6) but NOT serial (bit 13).
        mmu.write_halfword_arm7(0x0400_01A0, 0x8040);
        mmu.write_halfword_arm7(0x0400_01A2, 0x009F);
        mmu.write_halfword_arm7(0x0400_01A2, 0x0000);
        assert_eq!(
            mmu.read_halfword_arm7(0x0400_01A2) as u8,
            0x00,
            "parallel-mode writes must not reach the backup chip"
        );
        // Now with serial mode set, the same sequence does reach it.
        mmu.write_halfword_arm7(0x0400_01A0, 0xA040);
        mmu.write_halfword_arm7(0x0400_01A2, 0x009F);
        mmu.write_halfword_arm7(0x0400_01A2, 0x0000);
        assert_eq!(mmu.read_halfword_arm7(0x0400_01A2) as u8, 0x20);
    }

    /// The AUXSPI census tag must distinguish both the register and the core.
    /// Encoding the core in bit 7 of the raw address byte silently failed:
    /// 0xA0-0xA3 already have bit 7 set, so every entry decoded as an ARM9
    /// write to one register and the whole census read as an idle bus.
    #[test]
    fn test_aux_census_tags_register_and_core_distinctly() {
        let mut mmu = NdsMmu::new();
        mmu.write_byte_arm7(0x0400_01A0, 0x40);
        mmu.write_byte_arm7(0x0400_01A1, 0xA0);
        mmu.write_byte_arm7(0x0400_01A2, 0x05);
        mmu.write_byte_arm9(0x0400_01A2, 0x06);
        assert_eq!(
            mmu.aux_bus_log,
            vec![(0x00, 0x40), (0x01, 0xA0), (0x02, 0x05), (0x82, 0x06)]
        );
        // Every tag must survive the (index, core) split the reader performs.
        for (tag, _) in &mmu.aux_bus_log {
            assert!(tag & 0x7F <= 3, "register index must be 0..=3, got {tag:#04x}");
        }
    }

    /// A soft reset must not wipe the player's save. `reset()` recreates the
    /// SPI controller and every RAM, and runs on every ROM load.
    #[test]
    fn test_reset_preserves_backup_contents() {
        let mut mmu = NdsMmu::new();
        mmu.backup = crate::nds::backup::NdsBackup::new(
            crate::nds::backup::BackupKind::Flash256K,
        );
        mmu.write_halfword_arm7(0x0400_01A0, 0xA040);
        mmu.write_byte_arm7(0x0400_01A2, 0x06); // WREN
        mmu.write_halfword_arm7(0x0400_01A0, 0x0000);
        mmu.write_halfword_arm7(0x0400_01A0, 0xA040);
        for b in [0x0A, 0x00, 0x00, 0x00, 0x5A] {
            mmu.write_byte_arm7(0x0400_01A2, b);
        }
        mmu.write_halfword_arm7(0x0400_01A0, 0x0000);
        assert_eq!(mmu.backup.data()[0], 0x5A);

        mmu.reset();
        assert_eq!(mmu.backup.data()[0], 0x5A, "reset must not erase the save");
        assert_eq!(mmu.backup.data().len(), 256 * 1024);
    }

    #[test]
    fn test_sndcap_registers_store_and_read_back() {
        let mut mmu = NdsMmu::new();
        mmu.write_byte_arm7(0x0400_0508, 0x8F); // cap0 CNT: armed + source bits
        mmu.write_byte_arm7(0x0400_0510, 0x00); // cap0 DAD = 0x02064000
        mmu.write_byte_arm7(0x0400_0511, 0x40);
        mmu.write_byte_arm7(0x0400_0512, 0x06);
        mmu.write_byte_arm7(0x0400_0513, 0x02);
        mmu.write_byte_arm7(0x0400_0514, 0x80); // cap0 LEN = 0x80 words
        mmu.write_byte_arm7(0x0400_0515, 0x00);
        mmu.write_byte_arm7(0x0400_0509, 0x80); // cap1 CNT armed
        mmu.write_byte_arm7(0x0400_051C, 0x40); // cap1 LEN
        assert_eq!(mmu.apu.cap_cnt, [0x8F, 0x80]);
        assert_eq!(mmu.apu.cap_dad[0], 0x0206_4000);
        assert_eq!(mmu.apu.cap_len, [0x80, 0x40]);
        // CNT and DAD read back; LEN is write-only on hardware.
        assert_eq!(mmu.read_byte_arm7(0x0400_0508), 0x8F);
        assert_eq!(mmu.read_byte_arm7(0x0400_0512), 0x06);
        assert_eq!(mmu.read_byte_arm7(0x0400_0514), 0);
        assert_eq!(mmu.apu.cap_write_log.len(), 9);
    }

    /// ARM9 hardware divider/sqrt (0x04000280 block): the SDK computes every
    /// fixed-point division here — TP_SetCalibrateParam derives 1<<28/dot
    /// and the calibration dot factors through it, so a missing divider
    /// zeroed the touch calibration (U28).
    #[test]
    fn test_arm9_hardware_divider_and_sqrt() {
        let mut mmu = NdsMmu::new();
        // Mode 0 (32/32) signed: -100 / 7 = -14 rem -2.
        mmu.write_halfword_arm9(0x0400_0280, 0);
        mmu.write_word_arm9(0x0400_0290, (-100i32) as u32);
        mmu.write_word_arm9(0x0400_0298, 7);
        let q = mmu.read_word_arm9(0x0400_02A0) as i32;
        let q_hi = mmu.read_word_arm9(0x0400_02A4) as i32;
        let r = mmu.read_word_arm9(0x0400_02A8) as i32;
        assert_eq!(q, -14);
        assert_eq!(q_hi, -1, "quotient sign-extends across the 64-bit result");
        assert_eq!(r, -2);
        assert_eq!(
            mmu.read_halfword_arm9(0x0400_0280) & 0xC000,
            0,
            "busy clear, no div0"
        );
        // Mode 1 (64/32): the TP reciprocal 1<<28 / 1152.
        mmu.write_halfword_arm9(0x0400_0280, 1);
        mmu.write_word_arm9(0x0400_0290, 1 << 28);
        mmu.write_word_arm9(0x0400_0294, 0);
        mmu.write_word_arm9(0x0400_0298, 1152);
        assert_eq!(mmu.read_word_arm9(0x0400_02A0), (1u32 << 28) / 1152);
        // Zero denominator: div0 flag (bit14), quotient 0, remainder = numer.
        mmu.write_word_arm9(0x0400_0298, 0);
        assert_ne!(mmu.read_halfword_arm9(0x0400_0280) & 0x4000, 0);
        assert_eq!(mmu.read_word_arm9(0x0400_02A0), 0);
        // Sqrt, 32-bit mode: sqrt(1<<28) = 1<<14.
        mmu.write_halfword_arm9(0x0400_02B0, 0);
        mmu.write_word_arm9(0x0400_02B8, 1 << 28);
        assert_eq!(mmu.read_word_arm9(0x0400_02B4), 1 << 14);
        // Sqrt, 64-bit mode: floor(sqrt(2^40 + 3)) = 2^20.
        mmu.write_halfword_arm9(0x0400_02B0, 1);
        mmu.write_word_arm9(0x0400_02B8, 3);
        mmu.write_word_arm9(0x0400_02BC, 1 << 8);
        assert_eq!(mmu.read_word_arm9(0x0400_02B4), 1 << 20);
    }

    #[test]
    fn test_auxspidata_reads_as_erased_chip() {
        // A bare MMU carries no cartridge, so no backup chip is wired (the
        // device is chosen from the gamecode at ROM load). The data port must
        // read as an ERASED chip (0xFF) on BOTH cores, never as stale IO-echo
        // bytes — a game's save-integrity scan reads whatever is there.
        let mmu = NdsMmu::new();
        assert!(!mmu.backup.is_present());
        assert_eq!(mmu.read_byte_arm9(0x0400_01A2), 0xFF);
        assert_eq!(mmu.read_byte_arm7(0x0400_01A2), 0xFF);
    }

    #[test]
    fn test_gx_words_reach_decoder_via_fifo_and_ports() {
        let mut mmu = NdsMmu::new();
        // Direct port write: 0x04000440 = MTX_MODE (1 param).
        mmu.write_word_arm9(0x0400_0440, 1);
        assert_eq!(mmu.gx.histo[0x10], 1);
        // Packed word through the FIFO window: MTX_IDENTITY (no params).
        mmu.write_word_arm9(0x0400_0400, 0x0000_0015);
        assert_eq!(mmu.gx.histo[0x15], 1);
        assert_eq!(mmu.gx.trace.len(), 2);
        // The byte-level GX side effects (activity flag) are untouched.
        assert!(mmu.has_3d_activity);
    }

    #[test]
    fn test_clear_color_write_maps_alpha_to_opacity() {
        let mut mmu = NdsMmu::new();
        // Opaque clear: alpha 31 + BGR555 blue.
        mmu.write_word_arm9(0x0400_0350, (31 << 16) | 0x7C00);
        assert_eq!(mmu.gx.engine.clear_px, 0x8000 | 0x7C00);
        // Alpha 0 clears transparent (2D backdrop shows).
        mmu.write_word_arm9(0x0400_0350, 0x7C00);
        assert_eq!(mmu.gx.engine.clear_px, 0);
    }

    #[test]
    fn test_apu_psg_wave_mixes_nonzero() {
        let mut mmu = NdsMmu::new();
        mmu.apu_write_byte(0x500, 0x7F);
        mmu.apu_write_byte(0x501, 0x80);
        let base = 0x400 + 8 * 16; // channel 8 = first PSG-capable
        mmu.apu_write_byte(base + 8, 0x00); // TMR = 0xFD00 -> ~21.9 kHz clock
        mmu.apu_write_byte(base + 9, 0xFD);
        mmu.apu_write_byte(base + 0, 0x7F); // vol 127
        mmu.apu_write_byte(base + 2, 0x40); // pan center
        mmu.apu_write_byte(base + 3, 0xE3); // start + fmt 3 + duty 3 (square)
        let mut buf = vec![0i16; 4096];
        for _ in 0..1250 {
            mmu.tick_apu(64, &mut buf, 0, 1.0);
        }
        // A one-shot self-stops partway through, and a silent APU has no next
        // edge — so the tail sits deferred until a flush, exactly as it does
        // inside a real tick. The run loop flushes once per frame; do the same
        // before reading `sample_count`.
        mmu.flush_apu(&mut buf, 0, 1.0);
        let n = mmu.apu.resampler.sample_count;
        assert!(
            buf[..n * 2].iter().any(|&s| s.abs() > 100),
            "PSG square must reach the output buffer non-silently"
        );
        assert!(mmu.apu.channels[8].active, "PSG never self-stops");
        assert_ne!(mmu.read_byte_arm7(0x0400_0483) & 0x80, 0, "busy reads 1");
        mmu.apu_write_byte(base + 3, 0x63);
        assert!(!mmu.apu.channels[8].active, "start-bit clear stops PSG");
    }

    /// OBJ VRAM windows are per-engine: with bank B = OBJ-A and bank I =
    /// OBJ-B (both MST 2 — SoulSilver's button-guide mapping), an engine-A
    /// tile upload must land in bank B ONLY. The flat MST-2 write used to
    /// also hit bank I at the same offset, garbling engine B's sprites with
    /// engine A tile data (the green trash around the menu "Touch" button).
    #[test]
    fn test_obj_vram_windows_do_not_cross_engines() {
        let mut mmu = NdsMmu::new();
        mmu.write_byte_arm9(0x04000241, 0x82); // VRAMCNT_B: enable, MST 2 -> OBJ-A
        mmu.write_byte_arm9(0x04000249, 0x82); // VRAMCNT_I: enable, MST 2 -> OBJ-B
        mmu.write_byte_arm9(0x0640_2000, 0xAA); // engine A OBJ tile byte
        mmu.write_byte_arm9(0x0660_2000, 0xBB); // engine B OBJ tile byte
        assert_eq!(mmu.vram.banks[1].data[0x2000], 0xAA, "OBJ-A write lands in bank B");
        assert_eq!(mmu.vram.banks[1].data.iter().filter(|&&b| b == 0xBB).count(), 0,
            "engine-B write must not leak into bank B");
        assert_eq!(mmu.vram.banks[8].data[0x2000], 0xBB, "OBJ-B write lands in bank I");
        assert_eq!(mmu.vram.banks[8].data.iter().filter(|&&b| b == 0xAA).count(), 0,
            "engine-A write must not clobber bank I (the sprite-garble bug)");
        assert_eq!(mmu.read_byte_arm9(0x0640_2000), 0xAA);
        assert_eq!(mmu.read_byte_arm9(0x0660_2000), 0xBB);
        assert_eq!(mmu.vram.read_obj_a(0x2000), 0xAA);
        assert_eq!(mmu.vram.read_obj_b(0x2000), 0xBB);
    }
}
