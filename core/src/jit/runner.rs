//! Drives compiled blocks: decides when one may run, calls it, and reconciles
//! the interpreter-visible state it did not maintain itself.
//!
//! # The equivalence argument
//!
//! A block replaces N iterations of `Arm9Cpu::step`. Everything that loop
//! touches has to end up with the value it would have left:
//!
//! | state | how the block handles it |
//! |---|---|
//! | `gpr[0..15]` | written directly, through the pinned register-file pointer |
//! | `gpr[15]` | a constant per instruction; written once in the epilogue |
//! | `pipeline[0]`, `pipeline[1]` | refilled here from the same words the compiler read |
//! | `pc_modified` | never set: the scanner excludes every instruction that could |
//! | `instrs`, `arm_class_hist` | added here; the counts are known at compile time |
//! | `arm9_exec_pc`, `arm9_exec_lr` | published here, from the block's own last-instruction values |
//! | cycles | returned by the block, summed from the same per-instruction costs |
//!
//! # Where a block is not allowed to run
//!
//! [`Arm9Jit::try_step`] declines — and the interpreter takes over for one
//! instruction — whenever the CPU is halted, has a pending pipeline refill, has
//! an interrupt to service, or has the touch read-watch armed. The last one is
//! not a performance concern: `Arm9Cpu::step` refills the pipeline through
//! `read_word`, which *records* the fetch as a data read when that watch is on,
//! and a block performs no such fetch. Declining keeps the diagnostic exact
//! rather than subtly under-counting.

use std::cell::Cell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::gba::cpu::GbaCpu;
use crate::jit::block::Block;
use crate::jit::compile::{compile_with, DispatchPlan, EmitCfg, JitContext, LinkSlots, MAX_EXIT_SLOTS};
use crate::jit::exec_mem::thunks::BusKind;
use crate::jit::exec_mem::{CodeBuffer, ExecBuffer};
use crate::nds::cpu::{Arm7Cpu, Arm9Cpu, FLAG_I, FLAG_T};
use crate::nds::mmu::NdsMmu;

/// The per-core facts the recompiler is generic over.
///
/// The machinery — scanner, translator, block cache, guards, successor links,
/// the dispatch table — is identical for the two NDS cores. What differs is
/// the ISA level, which bus thunks the generated code calls, which MMU fields
/// gate execution, and which memory is executable. Naming those here lets one
/// [`Jit<C>`] serve both cores as zero-cost monomorphic instantiations — the
/// hot paths compile exactly as they did when they were ARM9-only.
pub trait JitCore: 'static + Sized {
    /// The CPU wrapper this core's blocks run against.
    type Cpu;
    /// ARMv5TE (ARM9) or ARMv4T (ARM7): gates the v5-only encodings and the
    /// interworking word-load PC write. See `EmitCfg`.
    const ARMV5: bool;
    /// Which bus thunk set compiled code calls; see `thunks::BusKind`.
    const BUS: BusKind;
    /// Environment prefix for this core's switches (`_LINK`, `_MIN`, ...).
    const ENV: &'static str;
    /// Must a block provably FIT the remaining slice budget before it may
    /// run?
    ///
    /// The ARM9's slice overshoot feeds `arm9_cycles_run`, which the tick
    /// loop carries as debt, so block-granularity overshoot is repaid there.
    /// The ARM7's slice is derived from the ARM9's actual run and its own
    /// return value is not carried — overshoot is FREE TIME. The interpreter
    /// overshoots by at most one instruction; a block by up to ~50 cycles on
    /// a ~32-cycle slice, which compounded to +395 retired instructions by
    /// the end of boot tick 0 and broke the interleave oracle. With this set,
    /// a block whose worst-case cycles exceed the remaining budget declines
    /// and the interpreter finishes the slice — consuming cycles exactly as
    /// the no-JIT run would, which is what makes the interleave
    /// bit-identical rather than merely close.
    const EXACT_SLICES: bool;
    /// Default for `_LINK`/`_DISPATCH`. Off where `EXACT_SLICES` is set: a
    /// linked chain's emitted budget check stops it only *after* the block
    /// that crossed the line, which is exactly the overshoot exact slices
    /// exist to forbid. Env flags still override for experiments.
    const LINK_DEFAULT: bool;
    /// The shared ARM core inside the wrapper.
    fn gba(cpu: &Self::Cpu) -> &GbaCpu;
    fn gba_mut(cpu: &mut Self::Cpu) -> &mut GbaCpu;
    /// The recompiler slot on the wrapper, for [`step_or_block`].
    fn jit_slot(cpu: &mut Self::Cpu) -> &mut Option<Box<Jit<Self>>>;
    /// Shared view of the same slot, so the hot preconditions can be read
    /// without a `&mut` borrow (and without moving the box).
    fn jit_ref(cpu: &Self::Cpu) -> Option<&Jit<Self>>;
    fn flush_pipeline(cpu: &mut Self::Cpu, mmu: &mut NdsMmu);
    /// One interpreted instruction — the fallback every decline takes.
    fn step(cpu: &mut Self::Cpu, mmu: &mut NdsMmu) -> u32;
    /// Is an enabled, unmasked interrupt pending? Mirrors this core's `step`.
    fn irq_pending(gba: &GbaCpu, mmu: &NdsMmu) -> bool;
    /// Is a diagnostic read-watch armed that blocks must decline under?
    fn watch_armed(mmu: &NdsMmu) -> bool;
    /// Plain aligned word read on this core's bus — fetch semantics, exactly
    /// what a pipeline refill reads.
    fn read_word(mmu: &mut NdsMmu, addr: u32) -> u32;
    fn read_halfword(mmu: &mut NdsMmu, addr: u32) -> u16;
    /// Tracked code page holding `addr`, or `None` (declines the block).
    fn code_page(mmu: &NdsMmu, addr: u32) -> Option<usize>;
    /// This core's link epoch; see `NdsMmu::code_write_epoch`/`_epoch7`.
    /// Split per core so the other core's data stores cannot tear down this
    /// core's links.
    fn code_epoch(mmu: &NdsMmu) -> u64;
    /// This core's bit in `NdsMmu::code_block_pages`.
    const PAGE_MASK: u8;
    /// Publish the exec-pc/lr diagnostics this core's `step` maintains
    /// (a no-op on the ARM7, whose `step` maintains none).
    fn publish_exec(mmu: &mut NdsMmu, pc: u32, lr: u32);
}

/// The NDS ARM9 (ARM946E-S) as a [`JitCore`].
pub struct Arm9Core;

impl JitCore for Arm9Core {
    type Cpu = Arm9Cpu;
    const ARMV5: bool = true;
    const BUS: BusKind = BusKind::Arm9;
    const ENV: &'static str = "EMU_ARM9_JIT";
    const EXACT_SLICES: bool = false;
    const LINK_DEFAULT: bool = true;
    fn gba(cpu: &Arm9Cpu) -> &GbaCpu {
        &cpu.cpu
    }
    fn gba_mut(cpu: &mut Arm9Cpu) -> &mut GbaCpu {
        &mut cpu.cpu
    }
    fn jit_slot(cpu: &mut Arm9Cpu) -> &mut Option<Box<Jit<Self>>> {
        &mut cpu.jit
    }
    fn jit_ref(cpu: &Arm9Cpu) -> Option<&Jit<Self>> {
        cpu.jit.as_deref()
    }
    fn flush_pipeline(cpu: &mut Arm9Cpu, mmu: &mut NdsMmu) {
        cpu.flush_pipeline(mmu);
    }
    fn step(cpu: &mut Arm9Cpu, mmu: &mut NdsMmu) -> u32 {
        cpu.step(mmu)
    }
    fn irq_pending(gba: &GbaCpu, mmu: &NdsMmu) -> bool {
        (mmu.arm9_ime & 1) != 0
            && !gba.registers.get_flag(FLAG_I)
            && (mmu.arm9_ie & mmu.arm9_if) != 0
    }
    fn watch_armed(mmu: &NdsMmu) -> bool {
        mmu.tp_read_watch_on
    }
    fn read_word(mmu: &mut NdsMmu, addr: u32) -> u32 {
        mmu.read_word_arm9(addr)
    }
    fn read_halfword(mmu: &mut NdsMmu, addr: u32) -> u16 {
        mmu.read_halfword_arm9(addr)
    }
    fn code_page(mmu: &NdsMmu, addr: u32) -> Option<usize> {
        mmu.code_page_arm9(addr)
    }
    fn code_epoch(mmu: &NdsMmu) -> u64 {
        mmu.code_write_epoch
    }
    const PAGE_MASK: u8 = crate::nds::mmu::CODE_MASK_ARM9;
    fn publish_exec(mmu: &mut NdsMmu, pc: u32, lr: u32) {
        mmu.arm9_exec_pc = pc;
        mmu.arm9_exec_lr = lr;
    }
}

/// The NDS ARM7 (ARM7TDMI) as a [`JitCore`].
pub struct Arm7Core;

impl JitCore for Arm7Core {
    type Cpu = Arm7Cpu;
    const ARMV5: bool = false;
    const BUS: BusKind = BusKind::Arm7;
    const ENV: &'static str = "EMU_ARM7_JIT";
    const EXACT_SLICES: bool = true;
    const LINK_DEFAULT: bool = false;
    fn gba(cpu: &Arm7Cpu) -> &GbaCpu {
        &cpu.cpu
    }
    fn gba_mut(cpu: &mut Arm7Cpu) -> &mut GbaCpu {
        &mut cpu.cpu
    }
    fn jit_slot(cpu: &mut Arm7Cpu) -> &mut Option<Box<Jit<Self>>> {
        &mut cpu.jit
    }
    fn jit_ref(cpu: &Arm7Cpu) -> Option<&Jit<Self>> {
        cpu.jit.as_deref()
    }
    fn flush_pipeline(cpu: &mut Arm7Cpu, mmu: &mut NdsMmu) {
        cpu.flush_pipeline(mmu);
    }
    fn step(cpu: &mut Arm7Cpu, mmu: &mut NdsMmu) -> u32 {
        cpu.step(mmu)
    }
    fn irq_pending(gba: &GbaCpu, mmu: &NdsMmu) -> bool {
        (mmu.arm7_ime & 1) != 0
            && !gba.registers.get_flag(FLAG_I)
            && (mmu.arm7_ie & mmu.arm7_if) != 0
    }
    fn watch_armed(_mmu: &NdsMmu) -> bool {
        // The touch read-watch records *ARM9* data reads; ARM7 blocks have
        // nothing to observe or under-count.
        false
    }
    fn read_word(mmu: &mut NdsMmu, addr: u32) -> u32 {
        mmu.read_word_arm7(addr)
    }
    fn read_halfword(mmu: &mut NdsMmu, addr: u32) -> u16 {
        mmu.read_halfword_arm7(addr)
    }
    fn code_page(mmu: &NdsMmu, addr: u32) -> Option<usize> {
        mmu.code_page_arm7(addr)
    }
    fn code_epoch(mmu: &NdsMmu) -> u64 {
        mmu.code_write_epoch7
    }
    const PAGE_MASK: u8 = crate::nds::mmu::CODE_MASK_ARM7;
    fn publish_exec(_mmu: &mut NdsMmu, _pc: u32, _lr: u32) {
        // Arm7Cpu::step publishes no exec diagnostics; neither do its blocks.
    }
}

/// What must still be true for a cached block to be usable.
///
/// # Why an address is not a sufficient key
///
/// Two independent things can change under a compiled block.
///
/// * **The bytes.** DS games DMA and decompress code into RAM, so the same
///   address can hold a different program later. Each page the block's guest
///   bytes span is recorded with its version, and the MMU moves that version on
///   any store that lands there.
/// * **The pipeline.** A block's first two instructions come from
///   `pipeline[0]`/`pipeline[1]`, not from memory — the interpreter executes the
///   already-fetched word. Code modified *after* being fetched therefore runs
///   the old word once, and a page version alone cannot distinguish that from
///   the next visit, which runs the new one. The two words are compared
///   directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Guard {
    /// Every distinct page the trace reads from, with the version it was read
    /// at. A trace follows unconditional branches, so its instructions are not
    /// contiguous and the count is not bounded by the body length — a trace
    /// touching more than [`Self::MAX_PAGES`] is simply not cached.
    ///
    /// The page index is a `u16`: main RAM plus ITCM is 1032 pages, so a
    /// `usize` here cost 6 bytes each for nothing, and this structure's size is
    /// not incidental — see [`CachedBlock`].
    pages: [(u16, u32); Guard::MAX_PAGES],
    page_count: u8,
    /// The two words the block was compiled from that came from the pipeline.
    pipeline: [u32; 2],
}

impl Guard {
    /// Distinct code pages a cached trace may span. Four covers a trace that
    /// branches across three regions plus its pipeline tail; beyond that the
    /// guard costs more than the block saves.
    const MAX_PAGES: usize = 4;

    fn still_valid(&self, gba: &GbaCpu, mmu: &NdsMmu) -> bool {
        self.pipeline == gba.pipeline && self.pages_current(mmu)
    }

    /// The page-version half of [`Self::still_valid`], alone.
    ///
    /// Link validation needs it without the pipeline comparison: at link time
    /// the *memory words* at the target stand in for `cpu.pipeline`, because a
    /// linked entry skips the flush the interpreter would have done and memory
    /// is what that flush would have read.
    fn pages_current(&self, mmu: &NdsMmu) -> bool {
        self.pages[..usize::from(self.page_count)]
            .iter()
            .all(|&(page, version)| mmu.code_version(usize::from(page)) == version)
    }
}

/// Shortest run worth entering a compiled block for.
///
/// A block costs a prologue, an epilogue, a cache lookup and a guard check, all
/// of which the interpreter's per-instruction path does not have. Below some
/// length that fixed cost exceeds the interpretation it replaces — and folding
/// direct branches made the question urgent, because a lone branch now forms a
/// **one-instruction** block where it used to be an empty body.
///
/// Re-swept on the player's scene at a 5x request with Thumb and refill
/// servicing on, alternating with the interpreter (3.095), GBA row steady at
/// 9.78-9.99:
///
/// | minimum | NDS @5x |
/// |---|---|
/// | 1 | 2.895 |
/// | **2** | **3.125** |
/// | 3 | 3.070 |
/// | 4 | 2.985 |
///
/// The previous sweep chose 3, but it ran through the address-filter aliasing
/// bug fixed in [`Arm9Jit::hot_slot`], which inflated the cost of every scan
/// and so of every block short enough to be discarded after one. **A tuning
/// constant is only as good as the machinery it was tuned on** — this is the
/// second default that moved once that bug was gone.
///
/// Still readable from the environment (`EMU_ARM9_JIT_MIN`) so the sweep can be
/// repeated rather than trusted.
const DEFAULT_MIN_BLOCK_INSTRS: usize = 2;

/// The default, from the environment when it is set.
///
/// Deliberately *not* a process-wide cached value the translator reads directly:
/// that would be a hidden dependency no test could vary. It seeds a field on
/// [`Arm9Jit`] instead, and tests that want to exercise short blocks construct
/// one with [`Arm9Jit::with_min_block_instrs`].
///
/// **The default depends on linking.** MIN=1 was measured slower and reverted
/// (2.895 vs 3.125 at MIN=2) — an entry-cost fact: a 1-instruction block loses
/// money at ~24 ns/entry and costs ~0 when chained. Re-swept with successor
/// linking on, MIN=1 wins 3/3 alternating pairs (@5x 3.65/3.56/3.58 vs
/// 3.52/3.49/3.45), so the linked default is 1 and the unlinked default stays 2.
fn default_min_block_instrs(env_prefix: &str, link: bool) -> usize {
    std::env::var(format!("{env_prefix}_MIN"))
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(if link { 1 } else { DEFAULT_MIN_BLOCK_INSTRS })
}

/// Hash for block start addresses.
///
/// `HashMap`'s default is SipHash-1-3, which is the right default — it is
/// collision-resistant against hostile keys. These keys are guest program
/// counters chosen by the game, not by an attacker, and the lookup happens on
/// **every instruction the recompiler considers**. A multiply-and-xorshift
/// finalizer replaces it. The xorshift is not optional: block addresses are
/// aligned, so the low bits of a bare multiply by an odd constant carry no
/// entropy and every entry would land in the same buckets.
#[derive(Default)]
struct AddrHasher(u64);

impl Hasher for AddrHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write_u32(&mut self, value: u32) {
        let mut z = u64::from(value).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        z ^= z >> 31;
        self.0 = z;
    }

    /// Never reached for a `u32` key, which hashes through
    /// [`Self::write_u32`], but a `Hasher` must define it.
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
}

/// The block cache's map type.
///
/// **Refuted, do not re-attempt:** replacing this with a 1024-slot
/// direct-mapped table — tag inline with the record, one index and one compare
/// instead of a hash, a control-byte probe and a pointer-chase — measured the
/// ARM9 stage at **18.93 ms against 2.90**, a 6.5x regression.
///
/// A map has no conflict misses. A direct-mapped table does, and here a miss is
/// not a cheap re-probe: it re-runs `translate`, which recompiles the block and
/// appends a fresh copy to the code arena. Two hot blocks landing in one slot
/// evict each other on every entry, so 47,000 entries per frame become 47,000
/// recompiles and megabytes of arena churn. Among 152 blocks in 1024 slots a
/// single unlucky pair is enough. **When a miss is orders of magnitude more
/// expensive than a hit, associativity is a correctness-of-performance
/// property, not a tuning knob.**
type BlockMap = HashMap<u32, CachedBlock, BuildHasherDefault<AddrHasher>>;

/// Why the recompiler stopped, counted per occurrence.
///
/// Coverage has now been raised twice on the strength of the *static*
/// instruction mix — branches (13.5%) and block transfers (7.5%) — and both
/// moved the frame far less than their share predicted. The static mix says how
/// often an encoding is executed; it does not say how often it is the thing
/// *ending a block*, and those are different questions. This answers the second
/// one directly so the next milestone is chosen from evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Unused. Thumb was 91.5% of stand-downs until it was translated; the
    /// variant is kept so the histogram's slot indices stay stable across the
    /// measurements recorded in this module's documentation.
    Thumb,
    /// `may_run` refused, and the finer variants below were not being
    /// collected. Retained so the slot indices stay stable.
    NotRunnable,
    /// The scanner would not put the very first instruction in a body.
    ScannerRejectedFirst,
    /// The scanner produced a body, but the translator could not take all of it,
    /// and the run that remained was shorter than the minimum.
    TranslatorRefused,
    /// A body was produced and translated, but it was shorter than
    /// [`Arm9Jit::min_block_instrs`].
    TooShort,
    /// `may_run` refused: the CPU is halted.
    Halted,
    /// `may_run` refused: a pipeline refill is pending (`pc_modified`).
    PendingRefill,
    /// `may_run` refused: an enabled interrupt is asserted and unmasked.
    PendingIrq,
    /// `may_run` refused: the touch-panel read-watch is armed.
    TouchWatch,
    /// A body was translated, but no [`Guard`] could be built for it: the trace
    /// reaches memory whose changes are not tracked, or spans more than
    /// [`Guard::MAX_PAGES`] pages. The block is discarded rather than cached.
    Unguardable,
}

/// A boolean feature switch read from the environment.
///
/// **Unset and empty both mean `default`.** `is_ok_and(|v| v != "0")` treats
/// `EMU_ARM9_JIT_THUMB=` as *on*, because `""` is `Ok` and is not `"0"`. A
/// shell loop that expanded an absent value that way ran three A/B rows in one
/// configuration and reported them as three — the rows were byte-identical,
/// which is the only reason it was caught. A/B switches decide what gets built,
/// so their parsing is a correctness surface, not a convenience.
pub fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(v) if v.is_empty() => default,
        Ok(v) => v != "0",
        Err(_) => default,
    }
}

/// Why a successor edge could not carry a compiled link.
///
/// A plain slot index would do, but the mapping from "the target was refused"
/// to a histogram position is exactly the kind of cross-module convention that
/// silently rots; naming the three cases makes a wrong slot a type error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainLoss {
    /// `may_run` refused the successor; carries the precondition that failed.
    Refused(StopReason),
    /// The successor's address is already in the decline filter.
    KnownDecline,
    /// The successor was compiled or scanned on this very visit, so no link
    /// could have existed yet. Warm-up only.
    Compiled,
}

impl ChainLoss {
    /// Slot in [`Arm9Jit::chain_lost`]. [`StopReason`] owns 0-9.
    fn slot(self) -> usize {
        match self {
            Self::Refused(reason) => reason as usize,
            Self::KnownDecline => 10,
            Self::Compiled => 11,
        }
    }
}

impl StopReason {
    /// Every reason, in histogram-slot order. Appending only — the measurements
    /// recorded in this module's documentation quote slot indices.
    pub const ALL: [StopReason; 10] = [
        Self::Thumb,
        Self::NotRunnable,
        Self::ScannerRejectedFirst,
        Self::TranslatorRefused,
        Self::TooShort,
        Self::Halted,
        Self::PendingRefill,
        Self::PendingIrq,
        Self::TouchWatch,
        Self::Unguardable,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Thumb => "thumb state",
            Self::NotRunnable => "not runnable (unclassified)",
            Self::ScannerRejectedFirst => "scanner rejected the first instruction",
            Self::TranslatorRefused => "translator cannot encode it",
            Self::TooShort => "run shorter than the minimum",
            Self::Halted => "cpu halted",
            Self::PendingRefill => "pipeline refill pending",
            Self::PendingIrq => "interrupt pending",
            Self::TouchWatch => "touch read-watch armed",
            Self::Unguardable => "no guard could be built (untracked / too many pages)",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Thumb => 0,
            Self::NotRunnable => 1,
            Self::ScannerRejectedFirst => 2,
            Self::TranslatorRefused => 3,
            Self::TooShort => 4,
            Self::Halted => 5,
            Self::PendingRefill => 6,
            Self::PendingIrq => 7,
            Self::TouchWatch => 8,
            Self::Unguardable => 9,
        }
    }
}

/// Size of the direct-mapped "already looked at this address" filter, as a
/// power of two. 2^16 x 4 bytes = 256 KiB.
///
/// # Swept, not guessed
///
/// The filter trades its own probe cost against the scans it avoids, so the
/// optimum is empirical. On the player's scene at a 5x request, Thumb and
/// refill servicing on, alternating with the interpreter (3.085):
///
/// | slots | NDS @5x |
/// |---|---|
/// | 2^13 | 2.985 |
/// | 2^14 | 3.055 |
/// | 2^15 | 3.070 |
/// | **2^16** | **3.095** |
/// | 2^18 | 3.040 |
///
/// It was 2^12, chosen to stay in L2. That reasoning priced the probe and
/// ignored the scans, and the scans are the larger term — though only after
/// the aliasing bug in [`Arm9Jit::hot_slot`] was fixed, without which no size
/// helped.
const DEFAULT_HOT_BITS: u32 = 16;

/// Filter size from the environment, clamped to sane bounds.
///
/// The ceiling is 2^22 slots = 16 MiB: past that the filter is larger than the
/// last-level cache and a probe costs a DRAM round trip, which is more than the
/// scan it is trying to avoid.
fn default_hot_bits(env_prefix: &str) -> u32 {
    std::env::var(format!("{env_prefix}_HOT_BITS"))
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .map_or(DEFAULT_HOT_BITS, |b| b.clamp(8, 22))
}

/// Empty slot marker. Guest instruction addresses are 4-byte aligned, so an
/// all-ones value cannot collide with a real one.
const HOT_EMPTY: u32 = u32::MAX;

/// Tag bit meaning "this address was examined and could not be compiled".
///
/// Free for the taking because addresses are 4-byte aligned.
const HOT_DECLINED: u32 = 1;

/// A compiled block, ready to run again.
///
/// # Why every field here is as small as it is
///
/// This is read on **every block entry** — 3.9 million times per 60 ticks — and
/// roughly eleven interpreted instructions run between consecutive entries,
/// which is more than enough to evict it. Entry cost measured ~70 ns on the real
/// scene against 9.6 ns warm in a loop that never interleaves, and the gap is
/// cache misses.
///
/// It was 192 bytes: three cache lines, including a whole [`Compiled`] whose
/// `Vec<u8>` of machine code is never read again after publication. Now it holds
/// only what an entry needs, in one line.
struct CachedBlock {
    entry: crate::jit::exec_mem::BlockFn,
    guard: Guard,
    /// The pipeline the interpreter would hold when the block ends, read at
    /// translation time.
    ///
    /// Not an optimisation — a correctness requirement. The interpreter fetches
    /// instruction `j` during step `j - 2`, so both of these words are read
    /// *before* the block's last instruction executes. Since a block ends after
    /// any store (see [`crate::jit::block::writes_memory`]), that store is the
    /// last instruction, and re-reading these afterwards would pick up its
    /// effect where the interpreter did not.
    next_pipeline: [u32; 2],
    /// The block ends *before* an instruction the recompiler cannot translate.
    ///
    /// That instruction is then a **guaranteed decline**: the next `try_step`
    /// would re-check the preconditions, miss the cache, re-scan, and reject —
    /// roughly 14 million times per 60 ticks on the player's scene, once per
    /// block, for a result already known at translation time. Interpreting it
    /// here instead skips the whole round trip.
    ///
    /// False when the block ends on a folded branch, on a store, or on the
    /// length cap, because in those cases the next instruction is ordinary and
    /// may well start a compilable block of its own.
    exit_needs_interpreter: bool,
    /// Address of the trace's final instruction, which is what the interpreter
    /// publishes in `NdsMmu::arm9_exec_pc`. Not derivable from the start: a
    /// trace follows branches.
    last_addr: u32,
    /// The alternate exit installed by a followed conditional branch.
    ///
    /// `reconcile` selects between this and the block's own end using
    /// `JitContext::exit_idx`, which only the taken path writes. Without it an
    /// early exit would report the *whole* block's retired count, cycles and
    /// histogram — silently over-counting every taken branch.
    early_exit: Option<crate::jit::compile::EarlyExit>,
    /// Why this block ends, as an index into the probe's exit labels.
    ///
    /// The scanner already computes this; keeping it lets the census be taken
    /// **per entry** instead of per scan. Those are different populations and
    /// the difference is not small: 14,176,748 entries come from 152 hot
    /// blocks, while the per-scan histogram totals 197,024 and is therefore
    /// almost entirely cold blocks the scanner looked at once. Only the hot
    /// distribution says which terminator is costing entries.
    exit_slot: u8,
    /// Thumb state. `execute_thumb` bumps `thumb_instrs` as well as `instrs`,
    /// and never touches `arm_class_hist`.
    ///
    /// The retired count and the per-class histogram used to live here too, as
    /// compile-time constants `reconcile` applied. The block banks both into
    /// [`JitContext`] at run time now — a prefix of a block, and later a chain
    /// of them, has no single constant for either.
    thumb: bool,
    /// Upper bound on one full run's cycles; what the `EXACT_SLICES` entry
    /// gate compares against the remaining slice budget.
    worst_cycles: u32,
    /// Host address a successor link jumps to: this block's code just past the
    /// prologue, where `GPR_BASE`/`CTX`/`CPSR` are assumed live. Zero when the
    /// block was compiled without linking.
    body_entry: u64,
    /// Per-exit link state, indexed like [`crate::jit::compile::CompiledExit::index`].
    /// All-`None` targets when linking is off.
    exits: [ExitLink; MAX_EXIT_SLOTS],
    /// The per-exit successor slots the block's `jmp [slot]`s read. Their
    /// **addresses are baked into this block's code as immediates**, so the
    /// box must live exactly as long as the record — dropping the record is
    /// safe only once nothing can execute the block, which
    /// [`Arm9Jit::flush_links`] before any removal guarantees.
    ///
    /// Empty when linking is off.
    slots: Box<[SlotCell]>,
}

/// One successor slot: where a block exit's `jmp [slot]` goes.
///
/// `AtomicU64` for the store the *generated code* races against in principle —
/// the emulator is single-threaded, so `Relaxed` stores compile to plain moves
/// and the atomicity is a formality that keeps the write defined behaviour.
struct SlotCell {
    cell: AtomicU64,
    /// The exit's own epilogue: where the slot points when no link is written.
    unlinked: u64,
}

impl SlotCell {
    fn new() -> Self {
        Self { cell: AtomicU64::new(0), unlinked: 0 }
    }

    /// Break the link: point the slot back at the exit's own epilogue.
    fn reset(&self) {
        self.cell.store(self.unlinked, Ordering::Relaxed);
    }
}

/// Link state of one block exit.
struct ExitLink {
    /// Guest start address of the successor this exit may link to, when that
    /// is a compile-time constant. The successor's instruction set is the
    /// block's own — nothing inside a block changes state.
    target: Option<u32>,
    /// Does this exit probe the dispatch table on the run-time R15 instead of
    /// a static slot? The runner installs table entries at its chain ends.
    dispatch: bool,
    /// Is a link currently written into the slot? `Cell` because linking and
    /// flushing happen through `&CachedBlock` while the cache is iterated.
    linked: Cell<bool>,
}

impl ExitLink {
    fn none() -> Self {
        Self { target: None, dispatch: false, linked: Cell::new(false) }
    }
}

/// Tag meaning "no entry" in the dispatch table. Misaligned, so no ARM block
/// start — the only values ever written — can collide with it.
const DISPATCH_EMPTY: u32 = u32::MAX;

/// One dispatch-table record, in the exact 16-byte layout the generated probe
/// addresses: the guest start address as the tag, then the block's body-entry
/// pointer at offset 8. `#[repr(C)]` is load-bearing.
#[repr(C)]
struct DispatchSlot {
    tag: AtomicU32,
    _pad: u32,
    body: AtomicU64,
}

const _: () = assert!(std::mem::size_of::<DispatchSlot>() == 16);

impl DispatchSlot {
    fn empty() -> Self {
        Self { tag: AtomicU32::new(DISPATCH_EMPTY), _pad: 0, body: AtomicU64::new(0) }
    }
}

/// Dispatch-table size, as a power of two: 4096 slots x 16 B = 64 KiB. The
/// working set is call/return targets — hundreds on the player's scene — and a
/// collision only costs an eviction, never correctness.
const DISPATCH_BITS: u32 = 12;

/// The table index the generated probe computes for `target` — kept
/// bit-identical with the emitted multiply-shift so the runner writes where
/// the code reads.
fn dispatch_slot(target: u32, mask: u32) -> usize {
    let z = u64::from(target).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    ((z >> 32) as u32 & mask) as usize
}

/// Owns the executable pages holding compiled code, and the cache that makes
/// compiling worthwhile. Generic over the core; see [`JitCore`].
pub struct Jit<C: JitCore> {
    /// Zero-sized: the core choice is a compile-time fact.
    _core: PhantomData<C>,
    /// Kept alive because the code being executed lives in them. Never freed
    /// while the recompiler lives: a block whose page was released would leave a
    /// dangling entry, and code memory is small next to the emulated machine.
    pages: Vec<ExecBuffer>,
    /// Compiled blocks by guest start address. Validity is [`Guard`]'s job; a
    /// hit whose guard has moved is recompiled in place.
    cache: BlockMap,
    /// Direct-mapped filter over addresses already examined.
    ///
    /// # Why this exists
    ///
    /// Measured on the player's scene: running the recompiler with **nothing
    /// compiled at all** still cost 29% (3.12x -> 2.22x). That is the price of
    /// asking "is there a block here?" once per guest instruction — 50 million
    /// times per 60 ticks, three quarters of which end in a decline after a
    /// full scan. Compiled execution was *paying back* part of that tax rather
    /// than being slow itself.
    ///
    /// A slot holds the guest address examined, with [`HOT_DECLINED`] set when
    /// the answer was "cannot compile". A decline therefore costs one load and
    /// one compare instead of a scan and a hash miss.
    ///
    /// A stale entry can only cost time, never correctness: a wrong "declined"
    /// interprets, which is always right, and a wrong "compiled" still goes
    /// through [`Guard`] before the block runs.
    /// Length is always a power of two, so [`Self::hot_slot`] is one mask.
    hot: Box<[u32]>,
    /// Guest instructions executed as compiled code. Diagnostics: a harness
    /// that silently fell back to the interpreter every time would otherwise
    /// look like a passing test.
    pub compiled_instrs: u64,
    /// Times a block could not be built and the interpreter took over.
    pub declined: u64,
    /// Blocks served from the cache, and blocks translated. The ratio is what
    /// says whether caching is doing anything.
    pub cache_hits: u64,
    pub compilations: u64,
    /// The context every block runs against, reused across entries.
    ///
    /// [`JitContext`] is 120 bytes — the `LDM`/`STM` staging buffer alone is 64
    /// — and building one on the stack per block entry meant writing all of it
    /// 3.9 million times per 60 ticks. Only four fields change between entries
    /// and three need clearing; the pointers and the buffer do not.
    ///
    /// **Measured 0%.** Kept because it is strictly less work and no more code,
    /// not because it helped. It is recorded here as the fourth consecutive
    /// sub-noise result on this path: the interpreter's own run-to-run spread is
    /// ~2%, so nothing smaller than about 3% is observable by this method at
    /// all, and micro-optimising the entry path has stopped being measurable.
    ctx: JitContext,
    /// Translate Thumb blocks at all.
    ///
    /// **Off by default, on measurement.** The translator is complete enough to
    /// pass the differential harness (F1-F6, F9, F11-F15), and enabling it took
    /// coverage 26.0% -> 35.2% — and the frame from -3.5% to **-18.5%**.
    ///
    /// The reason is the same one that sank the pipeline-refill experiment, and
    /// Thumb is its worst case: block entries rose 3.94M -> 5.51M for shorter
    /// blocks. A four-instruction Thumb block covers eight bytes of guest code
    /// where ARM covers sixteen, and Thumb interpretation is *cheaper* per
    /// instruction, so each entry buys less while costing the same. 18.9% of
    /// attempts were full scans discarded for being under the minimum.
    ///
    /// **Still off, re-measured after the code arena.** Thumb is a net loss in
    /// every combination tried, ARM9 stage ms/frame:
    ///
    /// | config | arm9 |
    /// |---|---|
    /// | jit alone | 3.253 |
    /// | + thumb | 3.363 |
    /// | refill + cond | **2.960** |
    /// | thumb + refill + cond | 3.010 |
    ///
    /// The pre-arena reading that refill "rescued" Thumb was an artifact: with
    /// entry cost fixed, refill alone is better than refill plus Thumb. Turn on
    /// with `EMU_ARM9_JIT_THUMB=1` only to re-measure.
    thumb: bool,
    /// Refill the pipeline here and compile at the branch target, instead of
    /// standing down and letting the interpreter run one instruction.
    ///
    /// # Why this is back after being reverted
    ///
    /// It was tried, measured **4% slower** for +6.9pp of coverage, and
    /// reverted. That measurement was taken with Thumb translation off, and the
    /// split stop histogram says it was a measurement of the wrong regime:
    ///
    /// | stand-down reason | Thumb off | Thumb on |
    /// |---|---|---|
    /// | pipeline refill pending | 12.2M (30.7%) | **17.1M (64.3%)** |
    /// | cpu halted | 222k (0.6%) | 223k (0.8%) |
    /// | interrupt pending | 71k (0.2%) | 81k (0.3%) |
    /// | touch read-watch armed | 0 | 0 |
    ///
    /// 17.1M of the 63.1M ARM9 instructions retired per 60 ticks are the
    /// instruction *after* a taken control transfer. With Thumb off, servicing
    /// them mostly bought a declined attempt — the flush was paid and no block
    /// came of it. With Thumb on the target is far likelier to compile, and the
    /// arithmetic says folding these in takes coverage 35.2% -> ~62%, against
    /// the ~65% that 5x needs.
    ///
    /// **On by default.** Re-A/B'd after the code-arena fix, ARM9 stage
    /// ms/frame against interpreter 3.420: jit alone 3.253, +refill 3.000.
    /// `EMU_ARM9_JIT_REFILL=0` disables it.
    service_refills: bool,
    /// Let the trace continue down the fall-through of one conditional direct
    /// branch, emitting its taken path as an early exit out of the block.
    ///
    /// Conditional branches end **46.0% of all block entries** — the single
    /// dominant terminator, ahead of stores at 26.3% — and block entries are
    /// 52% of the ARM9 stage, so this is aimed at the largest measured cost.
    /// **On by default**: refill 3.000 -> refill+cond 2.960 (ARM9 ms/frame,
    /// three rounds). `EMU_ARM9_JIT_CONDFOLLOW=0` keeps the previous scanner.
    follow_conditional: bool,
    /// Shortest run worth compiling; see [`DEFAULT_MIN_BLOCK_INSTRS`].
    min_block_instrs: usize,
    /// Did the last [`Self::try_step`] also interpret the instruction the block
    /// stopped before?
    ///
    /// The differential harness needs it: it must advance the reference by the
    /// number of guest instructions the recompiler actually retired, and that is
    /// the block's length plus this.
    last_exit_interpreted: bool,
    /// Whether the stop/exit tallies are maintained.
    ///
    /// Off by default and **not** free: `note_stop` fires on every stand-down —
    /// 25.7 million per 60 ticks on the player's scene, dominated by Thumb — and
    /// leaving it always-on measured **4% slower**. The probe turns it on; the
    /// emulator does not. An instrument that changes what it measures is worse
    /// than no instrument.
    diagnostics: bool,
    /// Declined scans that overwrote a *different* declining address in the
    /// filter. Close to the scan count means the filter is thrashing; close to
    /// zero means the scans are of addresses genuinely seen for the first time.
    hot_evictions: u64,
    /// Occurrences of each [`StopReason`], indexed by `StopReason::index`.
    stops: [u64; 10],
    /// Why each *scanner* exit happened, indexed by `ExitReason as usize`, plus
    /// a final slot for the exits that are not an `ExitReason` at all
    /// (length cap, after a store, after a folded branch).
    exits: [u64; 14],
    /// Why each block ended, counted **once per entry** rather than once per
    /// scan. See [`CachedBlock::exit_slot`] for why the two differ.
    entry_exits: [u64; 14],
    /// The same, restricted to scans that produced an **empty** body.
    ///
    /// Those are [`StopReason::ScannerRejectedFirst`], and unlike every other
    /// stop they name their own fix: the reason the first instruction could not
    /// enter a body is the encoding that has to be translated next. Mixed into
    /// [`Self::exits`] the signal is lost, because that array is dominated by
    /// the ordinary ends of blocks that did compile.
    empty_exits: [u64; 14],
    /// `cpu.instrs` as it stood the moment the previous block returned without
    /// handing its exit instruction to the interpreter.
    ///
    /// An emitted successor link can only replace a block entry when *nothing*
    /// ran between the two blocks, and the retired-instruction counter is the
    /// exact witness: any interpreted instruction in between moves it. The
    /// sentinel [`u64::MAX`] means "no pending edge", and a broken edge needs no
    /// clearing store, because the counter only ever moves forward past it.
    ///
    /// Written only while [`Self::diagnostics`] is on, so the comparison at the
    /// top of [`Self::try_step`] short-circuits on one predictable load in a
    /// shipped build.
    chain_at: u64,
    /// Block exits a compiled successor link could have been attached to, and
    /// the subset whose successor was an already-compiled, guard-valid block.
    ///
    /// Their ratio bounds what emitted block chaining can remove, which is the
    /// question that decides whether to build it: entries are 52% of the ARM9
    /// stage, but only the linkable ones are reachable by chaining.
    ///
    /// **Two known overcounts on the linkable side**, both small and both in the
    /// optimistic direction, so the number is an upper bound by construction: an
    /// `ipc_yield` between the blocks retires no instruction yet must still
    /// break a real chain, and so does a block that leaves the core halted.
    chain_edges: u64,
    chain_linkable: u64,
    /// Why each *unlinkable* edge could not be taken: slots 0-9 are
    /// [`StopReason`], slot 10 is "the target is already in the decline
    /// filter", slot 11 is everything reached through `translate`.
    ///
    /// Split because `chain_edges - chain_linkable` is a single number naming
    /// several unrelated fixes — a Thumb target wants a Thumb translator, a
    /// halted target wants nothing at all, and a declined target wants the
    /// encoding it refused. Sums to exactly `chain_edges - chain_linkable`.
    chain_lost: [u64; 12],
    /// Compile successor-linked exits and write links between blocks
    /// (`EMU_ARM9_JIT_LINK`). The flag decides **code shape at translation
    /// time**, so flipping it via [`Self::set_link_enabled`] discards the
    /// cache — mixed-mode blocks must not link into each other.
    link: bool,
    /// [`NdsMmu::code_write_epoch`] as of the last time links were known
    /// torn down. A mismatch at block entry means some store touched a page
    /// holding compiled code since the last chain, so every link is suspect
    /// and [`Self::flush_links`] runs before anything executes.
    seen_code_epoch: u64,
    /// Is any link currently written? One load instead of a cache walk on
    /// the (common) flush call that has nothing to do.
    links_live: bool,
    /// Census: successor links written, and teardowns that broke at least one.
    links_written: u64,
    link_flushes: u64,
    /// Why each chain ended, diagnostics-gated: 0 = slice budget spent,
    /// 1 = a store raised the stop flag, 2 = the exit's slot is unlinked
    /// (target not compiled, or link refused), 3 = the exit has no static
    /// target at all (MSR/truncation, or a Thumb/uncompiled run-time target),
    /// 4 = a dispatching exit's probe missed. Decides whether the next lever
    /// is longer slices, better linking, or a better dispatcher.
    chain_ends: [u64; 5],
    /// Emit dispatching exits for `BX`/`BLX(reg)`/`LDM {..,pc}`
    /// (`EMU_ARM9_JIT_DISPATCH`). Requires [`Self::link`]; changing it
    /// discards the cache for the same reason.
    dispatch: bool,
    /// The direct-mapped `(start, body entry)` table dispatching exits probe.
    /// Allocated once and **never reallocated** — its address is baked into
    /// every dispatching block as an immediate. Entries are torn down by
    /// [`Self::flush_links`] and [`Self::clear`]; the allocation itself must
    /// outlive both.
    dispatch_table: Box<[DispatchSlot]>,
    /// Census: dispatch-table entries written.
    dispatches_written: u64,
    /// Table slots holding a live entry, so a flush resets exactly those
    /// instead of sweeping all 4096: the ARM7's data stores move its epoch
    /// thousands of times per minute, and the full sweep was ~31M atomic
    /// stores per 60 ticks. May hold duplicates (a reset is idempotent);
    /// falls back to a full sweep if it ever grows past the table itself.
    dispatch_live: Vec<u32>,
    /// Guard revalidations that succeeded; see [`Self::verify`].
    pub revalidations: u64,
    /// Why link/dispatch writes were refused, in [`LinkRefusal::slot`] order:
    /// uncompiled target, instruction-set mismatch, words changed, pages
    /// stale. Diagnostics-gated. This is what splits the "exit not linked"
    /// chain-end bucket into actionable causes.
    link_refusals: [u64; 4],
    /// The identities behind the `Uncompiled` refusals: refused target ->
    /// (refusal count, start of the block whose exit last refused it).
    /// Diagnostics-gated and capped ([`Self::REFUSED_CAP`]) — an aggregate
    /// said "1.13M refusals, 100% uncompiled" and could not say *which
    /// addresses* or *why they never compile*; this can.
    refused_targets: HashMap<u32, (u64, u32), BuildHasherDefault<AddrHasher>>,
    /// Per-address decline census: start -> (filter hits, first instruction
    /// word captured when the address was poisoned). Diagnostics-gated,
    /// capped like [`Self::refused_targets`].
    ///
    /// Exists because the aggregate said "7.7M declined = 92% of retired ARM7
    /// instructions" and nothing named the encodings: the stop histogram
    /// reports the *poisoning* events (a few hundred, at warmup) while the
    /// cost is the *hits*, and the two populations differ by four orders of
    /// magnitude. The captured word is what turns the top row into a named
    /// translator gap without a disassembly pass.
    declined_top: HashMap<u32, (u64, u32), BuildHasherDefault<AddrHasher>>,
    /// The exact `(address, word)` pairs each cached block's translation
    /// consumed, for guard revalidation.
    ///
    /// A page version says "some store landed here", not "the code changed" —
    /// and on the ARM7 the two are almost never the same event: the sound
    /// driver keeps its mixing buffers on the pages its own code occupies, so
    /// versions move every tick under blocks whose bytes are untouched.
    /// Re-reading the words and comparing is ~16 loads against a ~microsecond
    /// retranslation, and a match lets the guard refresh in place. Side map
    /// rather than fields on [`CachedBlock`]: it is touched only on the
    /// stale path, and the hot record stays one cache line.
    verify: HashMap<u32, Box<[(u32, u32)]>, BuildHasherDefault<AddrHasher>>,
    /// Guard-stale recompiles per block start — the thrash ledger.
    ///
    /// A block whose page takes routine **data** stores (the ARM7 sound
    /// driver keeps its mixing buffers on the pages its code lives on) goes
    /// guard-stale on every visit, and each visit re-scans, re-translates and
    /// appends a fresh copy to the code arena: measured 28,887 compilations
    /// per 60 ticks on the ARM7 against 152 on the ARM9. An address that hits
    /// [`Self::STALE_RECOMPILE_CAP`] is quarantined: removed from the cache
    /// and hard-declined, and the link-refusal unstick refuses to clear it —
    /// interpreting it forever costs less than recompiling it forever.
    stale_recompiles: HashMap<u32, u8, BuildHasherDefault<AddrHasher>>,
}

/// The ARM9 recompiler — the name every probe and test knows.
pub type Arm9Jit = Jit<Arm9Core>;
/// The ARM7 recompiler, gated behind `EMU_ARM7_JIT`.
pub type Arm7Jit = Jit<Arm7Core>;

/// What the recompiler did, for the probes that decide whether it is worth it.
///
/// The ratios matter more than the totals: `compiled_instrs / (cache_hits +
/// compilations)` is the mean block length, and `declined` against the rest is
/// how much work is being thrown away.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JitStats {
    pub compiled_instrs: u64,
    pub cache_hits: u64,
    pub compilations: u64,
    pub declined: u64,
}

impl<C: JitCore> Default for Jit<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: JitCore> Jit<C> {
    /// This core's environment switch: `C::ENV` plus `suffix`.
    fn env_name(suffix: &str) -> String {
        format!("{}{}", C::ENV, suffix)
    }

    pub fn new() -> Self {
        Self::with_min_block_instrs(default_min_block_instrs(
            C::ENV,
            env_flag(&Self::env_name("_LINK"), C::LINK_DEFAULT),
        ))
    }

    /// A recompiler that declines runs shorter than `min_block_instrs`.
    ///
    /// Injected rather than read from a global so tests can exercise the short
    /// blocks the measured default rejects.
    pub fn with_min_block_instrs(min_block_instrs: usize) -> Self {
        Self::with_min_block_instrs_and_hot_bits(min_block_instrs, default_hot_bits(C::ENV))
    }

    /// As [`Self::with_min_block_instrs`], with the address filter sized
    /// explicitly. See [`DEFAULT_HOT_BITS`].
    pub fn with_min_block_instrs_and_hot_bits(min_block_instrs: usize, hot_bits: u32) -> Self {
        Self {
            _core: PhantomData,
            pages: Vec::new(),
            cache: BlockMap::default(),
            hot: vec![HOT_EMPTY; 1usize << hot_bits.clamp(8, 22)].into_boxed_slice(),
            ctx: JitContext::new(std::ptr::null_mut(), 0, 0),
            compiled_instrs: 0,
            declined: 0,
            cache_hits: 0,
            compilations: 0,
            min_block_instrs: min_block_instrs.max(1),
            thumb: env_flag(&Self::env_name("_THUMB"), false),
            service_refills: env_flag(&Self::env_name("_REFILL"), true),
            follow_conditional: env_flag(&Self::env_name("_CONDFOLLOW"), true),
            last_exit_interpreted: false,
            diagnostics: false,
            hot_evictions: 0,
            stops: [0; 10],
            exits: [0; 14],
            entry_exits: [0; 14],
            empty_exits: [0; 14],
            chain_at: u64::MAX,
            chain_edges: 0,
            chain_linkable: 0,
            chain_lost: [0; 12],
            // **On by default (when the recompiler itself is enabled), set
            // from measurement**: linking +3.7% @5x / +4.0% @4x over link-off
            // in one binary (3 alternating pairs), and the dispatcher a
            // further +1.9% @5x / +4.0% @4x. `EMU_ARM9_JIT` itself stays off,
            // so shipped behaviour is unchanged.
            link: env_flag(&Self::env_name("_LINK"), C::LINK_DEFAULT),
            seen_code_epoch: 0,
            links_live: false,
            links_written: 0,
            link_flushes: 0,
            chain_ends: [0; 5],
            dispatch: env_flag(&Self::env_name("_LINK"), C::LINK_DEFAULT)
                && env_flag(&Self::env_name("_DISPATCH"), C::LINK_DEFAULT),
            dispatch_table: (0..1usize << DISPATCH_BITS).map(|_| DispatchSlot::empty()).collect(),
            dispatches_written: 0,
            dispatch_live: Vec::new(),
            revalidations: 0,
            link_refusals: [0; 4],
            refused_targets: HashMap::default(),
            declined_top: HashMap::default(),
            verify: HashMap::default(),
            stale_recompiles: HashMap::default(),
        }
    }

    /// Guard-stale recompiles an address is allowed before it is quarantined;
    /// see [`Self::stale_recompiles`]. Small: a genuinely-overwritten block
    /// (a real code reload) re-earns its slot after the next [`Self::clear`],
    /// while a data-store thrasher stops burning arenas within one frame.
    const STALE_RECOMPILE_CAP: u8 = 4;

    /// Distinct refused targets tracked before new ones are dropped. Bounds
    /// diagnostic memory; the hot set is far smaller.
    const REFUSED_CAP: usize = 4096;

    /// Why link/dispatch writes were refused. Diagnostics-gated; see
    /// [`Self::link_refusals`].
    pub fn link_refusal_counts(&self) -> [u64; 4] {
        self.link_refusals
    }

    /// The most-refused uncompiled targets, descending by count. Each row:
    /// `(target, count, exact, declined, from, containing)` — is the target
    /// cached under its own key *now*, is it in the decline filter, which
    /// block's exit refused it last, and a cached block whose
    /// `start..=last_addr` span contains it.
    #[allow(clippy::type_complexity)]
    pub fn refused_target_top(
        &self,
        n: usize,
    ) -> Vec<(u32, u64, bool, bool, u32, Option<u32>)> {
        let mut all: Vec<(u32, (u64, u32))> =
            self.refused_targets.iter().map(|(&t, &v)| (t, v)).collect();
        all.sort_unstable_by(|a, b| b.1 .0.cmp(&a.1 .0));
        all.truncate(n);
        all.into_iter()
            .map(|(t, (c, from))| {
                let containing = self
                    .cache
                    .iter()
                    .find(|(&start, rec)| start < t && t <= rec.last_addr)
                    .map(|(&start, _)| start);
                (t, c, self.cache.contains_key(&t), self.is_known_decline(t), from, containing)
            })
            .collect()
    }

    /// Why chains ended, in [`Self::chain_ends`] order. Diagnostics-gated.
    pub fn chain_end_counts(&self) -> [u64; 5] {
        self.chain_ends
    }

    /// Dispatch-table entries written. See [`Self::dispatches_written`].
    pub fn dispatch_stats(&self) -> u64 {
        self.dispatches_written
    }

    /// Emit dispatching exits, whatever the deployment default is. Implies
    /// linking; discards the cache when the mode changes, because the flag
    /// decides code shape.
    pub fn set_dispatch_enabled(&mut self, on: bool) {
        if self.dispatch != on || (on && !self.link) {
            self.clear();
        }
        if on {
            self.link = true;
        }
        self.dispatch = on;
    }

    /// Successor links written, and teardown passes that broke at least one.
    pub fn link_stats(&self) -> (u64, u64) {
        (self.links_written, self.link_flushes)
    }

    /// Compile successor-linked exits, whatever the deployment default is.
    ///
    /// Discards the cache when the mode changes: the flag decides code shape,
    /// and a linked block jumping into a block compiled without slots — or the
    /// reverse — must be impossible, not merely unlikely.
    pub fn set_link_enabled(&mut self, on: bool) {
        if self.link != on {
            self.clear();
        }
        self.link = on;
    }

    /// Successor edges seen, and the subset a compiled link could have taken.
    /// See [`Self::chain_edges`].
    pub fn chain_stats(&self) -> (u64, u64) {
        (self.chain_edges, self.chain_linkable)
    }

    /// Why the rest of those edges were unlinkable. See [`Self::chain_lost`].
    pub fn chain_loss_counts(&self) -> [u64; 12] {
        self.chain_lost
    }

    /// Attribute an unlinkable successor edge, if this call site is one.
    ///
    /// Takes the retired-instruction count instead of reading it, because the
    /// refusals that matter happen in `Arm9Cpu::step_or_block`, which never
    /// calls [`Self::try_step`] for them. Attributing them from inside
    /// `try_step` accounted for 9,151 of 4,796,468 losses and silently dropped
    /// the rest — the census has to be taken where the decision is made.
    pub fn note_chain_loss(&mut self, loss: ChainLoss, instrs: u64) {
        if self.diagnostics && self.chain_at == instrs {
            self.chain_lost[loss.slot()] += 1;
        }
    }

    /// Occurrences of each [`StopReason`], in `StopReason::ALL` order.
    pub fn stop_counts(&self) -> [u64; 10] {
        self.stops
    }

    /// Scanner exit reasons, indexed by `ExitReason as usize`; slot 9 counts the
    /// exits that are not an `ExitReason` (length cap, store, folded branch).
    pub fn exit_counts(&self) -> [u64; 14] {
        self.exits
    }

    /// Scanner exits for scans that produced an empty body, same indexing.
    /// See [`Self::empty_exits`].
    pub fn empty_exit_counts(&self) -> [u64; 14] {
        self.empty_exits
    }

    /// Block exits weighted by **entries**, same indexing. See
    /// [`Self::entry_exits`].
    pub fn entry_exit_counts(&self) -> [u64; 14] {
        self.entry_exits
    }

    /// Address-filter slots overwritten by a different declining address, and
    /// the filter's size in slots. See [`Self::hot_evictions`].
    pub fn hot_filter_stats(&self) -> (u64, usize) {
        (self.hot_evictions, self.hot.len())
    }

    /// Is stop/exit accounting enabled? See [`Self::diagnostics`].
    pub fn diagnostics_on(&self) -> bool {
        self.diagnostics
    }

    /// Turn stop/exit accounting on. For probes only.
    pub fn set_diagnostics(&mut self, on: bool) {
        self.diagnostics = on;
    }

    /// Record why the recompiler had nothing to run at an address.
    pub fn note_stop(&mut self, reason: StopReason) {
        if self.diagnostics {
            self.stops[reason.index()] += 1;
        }
    }

    /// Translate Thumb blocks, whatever the deployment default is.
    ///
    /// Injected so the Thumb tests exercise the translator rather than the
    /// policy — a feature test that silently becomes a no-op when a default
    /// flips is worse than no test.
    pub fn set_thumb_enabled(&mut self, on: bool) {
        self.thumb = on;
    }

    /// Follow the fall-through of one conditional branch, whatever the
    /// deployment default is. See [`Self::follow_conditional`]. Injected so the
    /// test exercises the translator rather than the policy.
    pub fn set_follow_conditional(&mut self, on: bool) {
        self.follow_conditional = on;
    }

    /// Service pending pipeline refills, whatever the deployment default is.
    /// See [`Self::service_refills`]. Injected for the same reason as
    /// [`Self::set_thumb_enabled`].
    pub fn set_refill_servicing(&mut self, on: bool) {
        self.service_refills = on;
    }

    /// See [`Self::last_exit_interpreted`].
    pub fn last_step_interpreted_exit(&self) -> bool {
        self.last_exit_interpreted
    }

    /// Bytes R15 leads the executing instruction by: two instruction widths.
    pub fn pipeline_lead(cpu: &C::Cpu) -> u32 {
        if C::gba(cpu).registers.get_flag(FLAG_T) {
            4
        } else {
            8
        }
    }

    /// Is this address already known to be untranslatable?
    ///
    /// Callable without moving the recompiler out of its owner, so the decline
    /// path — three quarters of all calls — never pays for the ownership dance
    /// that running a block needs.
    pub fn is_known_decline(&self, start: u32) -> bool {
        self.hot[self.hot_slot(start)] == start | HOT_DECLINED
    }

    /// Try to prove the guard-stale block at `start` is byte-identical to
    /// what was translated, and refresh its guard in place if so.
    ///
    /// Sound only when the **pipeline** half of the guard still matches — a
    /// pipeline mismatch means the entry words the interpreter already
    /// fetched differ from what the block was compiled from, which word
    /// re-reads against *memory* cannot rule on. The fall-through pipeline is
    /// re-read from current memory exactly as a fresh translation would
    /// (including the one-instruction-block rule; see the seed-518 record),
    /// because those are the words the interpreter will fetch *during* the
    /// upcoming run.
    fn try_revalidate(&mut self, cpu: &C::Cpu, mmu: &mut NdsMmu, start: u32) -> bool {
        let Some(words) = self.verify.get(&start) else {
            return false;
        };
        let (thumb, pipeline_ok) = match self.cache.get(&start) {
            Some(rec) if rec.thumb == C::gba(cpu).registers.get_flag(FLAG_T) => {
                (rec.thumb, rec.guard.pipeline == C::gba(cpu).pipeline)
            }
            _ => return false,
        };
        if !pipeline_ok {
            return false;
        }
        for &(addr, word) in words.iter() {
            let current = if thumb {
                u32::from(C::read_halfword(mmu, addr))
            } else {
                C::read_word(mmu, addr)
            };
            if current != word {
                return false;
            }
        }

        let step: u32 = if thumb { 2 } else { 4 };
        let next = words[words.len() - 1].0.wrapping_add(step);
        let mut next_pipeline = Self::refill_words(mmu, next, thumb);
        if words.len() == 1 {
            next_pipeline[0] = C::gba(cpu).pipeline[1];
        }
        let rec = self.cache.get_mut(&start).expect("present above");
        rec.next_pipeline = next_pipeline;
        for entry in rec.guard.pages[..usize::from(rec.guard.page_count)].iter_mut() {
            entry.1 = mmu.code_version(usize::from(entry.0));
        }
        self.revalidations += 1;
        true
    }

    /// Record a guard-stale recompile of `start`; `false` means the address
    /// has hit [`Self::STALE_RECOMPILE_CAP`] and must be quarantined instead.
    /// The ledger is capped: past 4096 distinct thrashers new ones fail open
    /// to the old always-recompile behavior rather than growing unboundedly.
    fn note_stale_recompile(&mut self, start: u32) -> bool {
        if self.stale_recompiles.len() >= Self::REFUSED_CAP
            && !self.stale_recompiles.contains_key(&start)
        {
            return true;
        }
        let n = self.stale_recompiles.entry(start).or_insert(0);
        *n = n.saturating_add(1);
        *n < Self::STALE_RECOMPILE_CAP
    }

    /// Has `start` been quarantined for guard thrash? The link-refusal
    /// unstick consults this so it cannot resurrect a thrasher.
    fn is_quarantined(&self, start: u32) -> bool {
        self.stale_recompiles
            .get(&start)
            .is_some_and(|&n| n >= Self::STALE_RECOMPILE_CAP)
    }

    /// Record a decline that the fast path handled without entering
    /// [`Self::try_step`].
    ///
    /// Separate from [`Self::is_known_decline`] so that predicate stays pure.
    /// Without this the `declined` statistic counts only the calls that reach
    /// `try_step`, which since the filter landed is a small minority — it read
    /// 1.7% where the true figure was over 90%, and the whole point of the
    /// statistic is to decide what to work on next.
    pub fn note_declined(&mut self, start: u32) {
        self.declined += 1;
        self.tally_declined_hit(start);
    }

    /// Diagnostics-gated: create the census row when an address is poisoned,
    /// carrying the entry-pipeline word — the instruction the translator
    /// looked at — so the report names the encoding without a memory pass.
    fn note_poisoned(&mut self, start: u32, word: u32) {
        if self.diagnostics
            && (self.declined_top.len() < Self::REFUSED_CAP
                || self.declined_top.contains_key(&start))
        {
            self.declined_top.entry(start).or_insert((0, word));
        }
    }

    /// Diagnostics-gated per-address hit tally; see [`Self::declined_top`].
    /// Hits only bump rows the poisoning path created, so every row carries
    /// the instruction word captured at poison time.
    fn tally_declined_hit(&mut self, start: u32) {
        if self.diagnostics {
            if let Some(row) = self.declined_top.get_mut(&start) {
                row.0 += 1;
            }
        }
    }

    /// The most-hit permanently-declined addresses, descending:
    /// `(start, hits, first_word)`.
    pub fn declined_top(&self, n: usize) -> Vec<(u32, u64, u32)> {
        let mut all: Vec<(u32, u64, u32)> =
            self.declined_top.iter().map(|(&a, &(h, w))| (a, h, w)).collect();
        all.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        all.truncate(n);
        all
    }

    /// Slot in [`Self::hot`] for a guest address.
    ///
    /// # Why this is a hash and not a shift
    ///
    /// It was `(addr >> 2) & mask`, which is exact for ARM — addresses are
    /// 4-byte aligned, so the discarded bits are always zero. **Thumb
    /// addresses are 2-byte aligned**, so `a` and `a ^ 2` shared a slot, and
    /// adjacent Thumb instructions are the most common pair in the workload:
    /// one starts a compiled block and its neighbour declines, so the two
    /// evicted each other on every visit and the declining one re-scanned
    /// forever.
    ///
    /// Measured: **100% of declined scans overwrote a different address**, and
    /// growing the filter from 4096 slots to 1,048,576 moved that only from
    /// 11.1M scans to 8.4M — capacity was never the problem. Declined scans
    /// were 1.67M with Thumb off and 9.19M with it on, which is the same fact
    /// seen from the other side.
    ///
    /// A multiply-shift spreads both instruction sets over every slot. The
    /// high half is taken because the low bits of a multiply by an odd constant
    /// carry almost no entropy from the high bits of the input.
    fn hot_slot(&self, addr: u32) -> usize {
        let z = u64::from(addr).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        (z >> 32) as usize & (self.hot.len() - 1)
    }

    pub fn stats(&self) -> JitStats {
        JitStats {
            compiled_instrs: self.compiled_instrs,
            cache_hits: self.cache_hits,
            compilations: self.compilations,
            declined: self.declined,
        }
    }

    /// Discard every compiled block.
    ///
    /// For changes no store passes through, so no page version moves: a
    /// savestate restore replaces the whole memory image at once.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.pages.clear();
        self.hot.fill(HOT_EMPTY);
        // A cleared world is a new program (savestate load, mode change):
        // thrash history no longer describes it, and neither do the words.
        self.stale_recompiles.clear();
        self.verify.clear();
        self.declined_top.clear();
        // Every slot died with its record, so no link survives. Sound because
        // nothing can execute a block once the cache no longer names it.
        self.links_live = false;
        // The dispatch table's body pointers aim into the pages just dropped;
        // the allocation stays (its address is baked into code that is also
        // gone), but every entry must die with the blocks it named — body
        // included, since a null body is the probe's "no entry" gate.
        for slot in self.dispatch_table.iter() {
            slot.tag.store(DISPATCH_EMPTY, Ordering::Relaxed);
            slot.body.store(0, Ordering::Relaxed);
        }
        self.dispatch_live.clear();
    }

    /// Break every successor link, pointing each written slot back at its
    /// exit's own epilogue.
    ///
    /// Called before any block runs whenever [`NdsMmu::code_write_epoch`]
    /// moved, and before a cache record is **replaced** — a replaced record
    /// drops the slots other blocks' baked immediates point at, so no link may
    /// survive into it. Links re-form lazily, one map lookup per chain exit.
    fn flush_links(&mut self) {
        if !self.links_live {
            return;
        }
        for rec in self.cache.values() {
            for (slot, exit) in rec.slots.iter().zip(rec.exits.iter()) {
                if exit.linked.get() {
                    slot.reset();
                    exit.linked.set(false);
                }
            }
        }
        // Dispatch entries are links too — same validity, same teardown. The
        // body is zeroed as well: a null body is what the emitted probe
        // treats as "no entry", because no tag sentinel is collision-free
        // against a guest-controlled R15. Only the slots actually written are
        // swept — resetting an already-empty slot is idempotent, so the
        // duplicate indices `dispatch_live` may hold are harmless.
        if self.dispatch_live.len() >= self.dispatch_table.len() {
            for slot in self.dispatch_table.iter() {
                slot.tag.store(DISPATCH_EMPTY, Ordering::Relaxed);
                slot.body.store(0, Ordering::Relaxed);
            }
            self.dispatch_live.clear();
        } else {
            for idx in self.dispatch_live.drain(..) {
                let slot = &self.dispatch_table[idx as usize];
                slot.tag.store(DISPATCH_EMPTY, Ordering::Relaxed);
                slot.body.store(0, Ordering::Relaxed);
            }
        }
        self.links_live = false;
        self.link_flushes += 1;
    }

    /// Run one compiled block — or, with linking, a **chain** of them — or
    /// return `None` if the caller must interpret.
    ///
    /// `max_instrs` caps the body, which the differential harness uses to force
    /// single-instruction blocks: that exercises the prologue, body and epilogue
    /// for every instruction in the corpus, where whole-block execution would
    /// only cover them in aggregate.
    ///
    /// `budget` is the run-loop slice's remaining cycles. A linked chain checks
    /// it at every exit and stands down once it is spent — the same
    /// `while used < budget` comparison `Arm9Cpu::run` makes — so a chain can
    /// overshoot by at most one block, exactly as a single block always could.
    /// Without linking it is unused.
    pub fn try_step(
        &mut self,
        cpu: &mut C::Cpu,
        mmu: &mut NdsMmu,
        max_instrs: usize,
        budget: u32,
    ) -> Option<u32> {
        // Is this entry the immediate successor of the block that ran before
        // it, with nothing interpreted in between? That is exactly the edge an
        // emitted chain would replace, so it is read *before* `may_run` can
        // return: an edge whose destination the recompiler then refuses is a
        // real edge that chaining cannot take. See [`Self::chain_edges`].
        let chained = self.diagnostics && self.chain_at == C::gba(cpu).instrs;

        if !self.may_run(cpu, mmu) {
            if chained {
                // `refusal_detail` is `#[cold]` and agrees with `may_run` by
                // test, so this attributes the loss without a second policy.
                let reason = self.refusal_detail(cpu, mmu).unwrap_or(StopReason::NotRunnable);
                let instrs = C::gba(cpu).instrs;
                self.note_chain_loss(ChainLoss::Refused(reason), instrs);
            }
            self.declined += 1;
            return None;
        }

        // `Arm9Cpu::step` normalises the pipeline before it fetches, and a block
        // reads its first two instructions out of that pipeline, so this has to
        // happen here too — and **before** `start` is derived from R15, which
        // the refill moves. Doing it after produced correct code filed under the
        // wrong address, which cost 4% and nothing else.
        //
        // Safe even if the recompiler goes on to decline: `Arm9Cpu::step` skips
        // its own flush when `pc_modified` is already clear, so the interpreter
        // resumes exactly where it would have.
        if C::gba(cpu).pc_modified {
            C::flush_pipeline(cpu, mmu);
        }

        // Arm the MMU's page-version tracking, which every `Guard` depends on.
        // It lives here, not in the caller, because this is the function whose
        // correctness needs it: `try_step` is public and the differential
        // harness calls it directly, so a caller-side arm would leave those
        // guards permanently valid against code that had changed underneath
        // them. Cheap now that the decline path returns before reaching here.
        mmu.code_watch = true;

        // R15 leads the executing instruction by two instruction widths.
        // R15 leads the executing instruction by **two instruction widths**,
        // which is 8 bytes in ARM and 4 in Thumb. Hardcoding 8 made the
        // recompiler compile from four bytes before the real instruction in
        // Thumb state — it produced working code for the wrong program.
        let start = C::gba(cpu).registers.gpr[15].wrapping_sub(Self::pipeline_lead(cpu));

        // A misaligned start is wild-jump territory — garbage control flow
        // landed the PC off the instruction grid, and the interpreter's
        // degenerate fetch semantics there (rotated/aligned reads of a stale
        // stream) are its own. Real code never executes at these addresses,
        // so the recompiler declines the entire class rather than replicate
        // it. Found by thumb fuzz seed 1918: an ARM-state block at an ODD
        // scratch address diverged on the fetch of its fall-through word.
        let misaligned = start & if C::gba(cpu).registers.get_flag(FLAG_T) { 1 } else { 3 };
        if misaligned != 0 {
            self.declined += 1;
            return None;
        }

        // The cheap filter first: the calls that reach here for an address
        // already known to be untranslatable become a load and a compare.
        //
        // **Refuted, do not re-attempt:** consulting the block cache first so a
        // successful entry skips this probe measured **0%** (cpu 4.183 vs 4.193
        // ms/frame, three alternating rounds). The filter is the largest
        // structure on the entry path — 256 KiB at 2^16 slots — and
        // `step_or_block` probes it once already, so removing the second probe
        // looked free. It is not where the ~24 ns per entry goes.
        let slot = self.hot_slot(start);
        if self.hot[slot] == start | HOT_DECLINED {
            let instrs = C::gba(cpu).instrs;
            self.note_chain_loss(ChainLoss::KnownDecline, instrs);
            self.tally_declined_hit(start);
            self.declined += 1;
            return None;
        }

        // A store since the last chain may have invalidated code some link
        // still points at. Tear every link down *before* anything can execute;
        // they re-form lazily. One load and one compare in the common case.
        if self.link && self.seen_code_epoch != C::code_epoch(mmu) {
            self.flush_links();
            self.seen_code_epoch = C::code_epoch(mmu);
        }

        // One lookup answers both questions: absent, valid, or guard-stale.
        let mut cached = self.cache.get(&start).map(|b| {
            b.thumb == C::gba(cpu).registers.get_flag(FLAG_T)
                && b.guard.still_valid(C::gba(cpu), mmu)
        });
        // A stale guard usually means a *data* store moved a page version
        // under unchanged code. Re-read the words translation consumed; if
        // they match, refresh the guard in place and run the block as a hit.
        if cached == Some(false) && self.try_revalidate(cpu, mmu, start) {
            cached = Some(true);
        }
        // A genuinely-changed block is about to recompile. Count it, and past
        // the cap quarantine the address instead: remove the record (links
        // flushed first — its slot cells die with it) and hard-decline, which
        // the unstick below knows not to clear. See `Self::stale_recompiles`.
        if cached == Some(false) && !self.note_stale_recompile(start) {
            self.flush_links();
            self.cache.remove(&start);
            self.verify.remove(&start);
            self.hot[slot] = start | HOT_DECLINED;
            self.note_poisoned(start, C::gba(cpu).pipeline[0]);
            let instrs = C::gba(cpu).instrs;
            self.note_chain_loss(ChainLoss::KnownDecline, instrs);
            self.declined += 1;
            return None;
        }

        if cached == Some(true) {
            self.cache_hits += 1;
            // Only a guard-valid cached block is linkable: a stale guard has to
            // recompile, which is precisely what a link must not skip.
            if chained {
                self.chain_linkable += 1;
            }
        } else if self.translate(cpu, mmu, start, max_instrs) {
            // A first sight or a guard-stale recompile. Not linkable: a link
            // must not skip the compile, and invalidation has to break it.
            let instrs = C::gba(cpu).instrs;
            self.note_chain_loss(ChainLoss::Compiled, instrs);
            self.hot[slot] = start;
        } else {
            let instrs = C::gba(cpu).instrs;
            self.note_chain_loss(ChainLoss::Compiled, instrs);
            // Did this slot already hold a *different* declining address? That
            // is the difference between "the filter is too small" and "these
            // scans are of addresses never seen before", which want opposite
            // work. Diagnostics-gated: this is the 32-million-per-60-ticks path.
            if self.diagnostics {
                let held = self.hot[slot];
                if held != HOT_EMPTY && held != start | HOT_DECLINED {
                    self.hot_evictions += 1;
                }
            }
            self.hot[slot] = start | HOT_DECLINED;
            self.note_poisoned(start, C::gba(cpu).pipeline[0]);
            self.declined += 1;
            return None;
        }

        // Exact-slice cores must not let a block overshoot the slice the way
        // the interpreter never could — see `JitCore::EXACT_SLICES` for the
        // measured failure. A block that might not fit declines WITHOUT
        // poisoning the filter (the address stays hot; only this entry, at
        // this remaining budget, interprets), so the slice's final
        // instructions consume cycles exactly as the no-JIT run would.
        if C::EXACT_SLICES {
            let worst =
                self.cache.get(&start).map_or(u32::MAX, |b| b.worst_cycles);
            if worst > budget {
                self.declined += 1;
                return None;
            }
        }

        // Copied out so the cache borrow ends before the block runs — a thunk
        // reaches the MMU, not the recompiler, but keeping the borrow alive
        // across the call would be a needless aliasing claim.
        let entry = self.cache.get(&start).expect("translate inserts on success").entry;

        // The block reaches guest memory through thunks that take this back as
        // their first argument. `mmu` is not touched again until the call has
        // returned, so the reborrow inside a thunk is the only live one.
        //
        // Updated in place rather than rebuilt: see `Arm9Jit::ctx`. The register
        // pointer is refreshed every entry because `Arm9Cpu` may have moved
        // since the last one.
        let gba = C::gba_mut(cpu);
        self.ctx.gpr = gba.registers.gpr.as_mut_ptr();
        self.ctx.mmu = (mmu as *mut NdsMmu).cast();
        // For the MSR thunk's bank swap; same pointer contract as `mmu`. The
        // shared `GbaCpu`, not the wrapper, so the thunk is core-agnostic.
        self.ctx.cpu = (gba as *mut GbaCpu).cast();
        self.ctx.cpsr = gba.registers.cpsr;
        self.ctx.exec_lr = gba.registers.gpr[14];
        // The three the block accumulates into, and nothing else: the staging
        // buffer is written before it is read, so it needs no clearing.
        self.ctx.pc_modified = 0;
        // 0 means "ran to the end"; only a taken early-exit branch writes it.
        self.ctx.exit_idx = 0;
        self.ctx.extra_cycles = 0;
        self.ctx.class_hits = [0; 6];
        self.ctx.retired = 0;
        // The chain fields. Without linking no emitted code reads them, and
        // `last_exit = start` makes the post-call lookup below mode-agnostic.
        self.ctx.cycles = 0;
        self.ctx.stop = 0;
        self.ctx.budget = budget;
        self.ctx.last_exit = start;
        self.ctx.code_epoch = C::code_epoch(mmu);

        let cycles = entry(&mut self.ctx as *mut JitContext);
        let ctx = &self.ctx;

        // Which block *ended* the run? Without linking, the one entered — the
        // old epilogue never writes `last_exit` and the init above holds. With
        // linking, whichever block's epilogue the chain finally fell into.
        // Its record must still be cached: links only ever point at cached
        // blocks, and every removal path flushes links first.
        let end_start = ctx.last_exit;
        let (early_exit, exit_slot, next_pipeline, exit_needs_interpreter, last_addr, block_thumb) = {
            let block = self
                .cache
                .get(&end_start)
                .expect("the chain ended in a block that is no longer cached");
            let mut last_addr = block.last_addr;
            let mut next_pipeline = block.next_pipeline;
            if ctx.exit_idx >= crate::jit::compile::READ_IRQ_EXIT_BASE {
                // A side-effecting read requested an IRQ before the trace's
                // ordinary exit. Reads do not write code, so current memory
                // still matches the words prefetched before that instruction.
                // The first instruction's next word is the one exception: it
                // was already prefetched on entry and can intentionally be stale.
                let i = (ctx.exit_idx - crate::jit::compile::READ_IRQ_EXIT_BASE) as usize;
                last_addr = self.verify[&end_start][i].0;
                let step = if block.thumb { 2 } else { 4 };
                next_pipeline = Self::refill_words(mmu, last_addr.wrapping_add(step), block.thumb);
                if i == 0 {
                    next_pipeline[0] = block.guard.pipeline[1];
                }
            }
            (
                block.early_exit,
                usize::from(block.exit_slot),
                next_pipeline,
                block.exit_needs_interpreter,
                last_addr,
                block.thumb,
            )
        };

        Self::reconcile(cpu, mmu, last_addr, next_pipeline, block_thumb, early_exit, ctx);
        // What actually retired. Counted by the block rather than picked from
        // a constant here; see `JitContext::retired`.
        self.compiled_instrs += u64::from(ctx.retired);
        if self.diagnostics {
            self.entry_exits[exit_slot] += 1;
        }

        // The exit that just broke the chain is the next link to write: it had
        // a compiled successor a moment ago (that is why it broke — budget,
        // stop flag, or simply not linked yet), so wire it for the next visit.
        if self.link {
            let exit_idx = usize::from(self.ctx.exit_idx as u8);
            let ended_at_dispatch = self
                .cache
                .get(&end_start)
                .and_then(|rec| rec.exits.get(exit_idx))
                .is_some_and(|e| e.dispatch);
            if self.diagnostics {
                // Attribute to the *binding* break: a structural no-target
                // exit first, then conditions that would have broken a linked
                // chain anyway (stop, budget), and only then the two the
                // machinery could still fix — a missing link, or a dispatch
                // miss.
                let over_budget = self
                    .ctx
                    .cycles
                    .wrapping_add(self.ctx.extra_cycles)
                    >= self.ctx.budget;
                let reason = {
                    let rec = self.cache.get(&end_start).expect("looked up above");
                    match rec.exits.get(exit_idx) {
                        Some(e) if e.target.is_none() && !e.dispatch => 3,
                        _ if self.ctx.stop != 0 => 1,
                        _ if over_budget => 0,
                        Some(e) if e.dispatch => 4,
                        _ => 2,
                    }
                };
                self.chain_ends[reason] += 1;
            }
            let outcome = if ended_at_dispatch {
                // The run-time target the chain could not reach; install it
                // for the next visit. R15 holds it — reconcile just ran.
                let target = C::gba(cpu).registers.gpr[15];
                let r = try_write_dispatch::<C>(
                    &self.cache,
                    &self.dispatch_table,
                    (self.dispatch_table.len() - 1) as u32,
                    target,
                    mmu,
                );
                if let Ok(slot_idx) = r {
                    self.dispatches_written += 1;
                    self.links_live = true;
                    self.dispatch_live.push(slot_idx as u32);
                }
                r.map(|_| ())
            } else {
                let r = try_write_link::<C>(&self.cache, end_start, exit_idx, mmu);
                if r.is_ok() {
                    self.links_written += 1;
                    self.links_live = true;
                }
                r
            };
            if let Err(refusal) = outcome {
                if refusal == LinkRefusal::Uncompiled {
                    let target = if ended_at_dispatch {
                        Some(C::gba(cpu).registers.gpr[15])
                    } else {
                        self.cache
                            .get(&end_start)
                            .and_then(|r| r.exits.get(exit_idx))
                            .and_then(|e| e.target)
                    };
                    if let Some(t) = target {
                        // **The decline filter is sticky, and that starves
                        // linking**: one transient warm-up decline marks the
                        // address forever, `step_or_block` then interprets it
                        // without ever re-scanning, and every chain into it
                        // pays a full entry for eternity. A refused link is
                        // the signal the address is hot again — clear its
                        // slot so the next visit re-attempts the scan. Gated
                        // on guardable code so a permanently uncompilable
                        // region (the exception vectors) cannot churn.
                        if C::code_page(mmu, t).is_some() && !self.is_quarantined(t) {
                            let slot = self.hot_slot(t);
                            if self.hot[slot] == t | HOT_DECLINED {
                                self.hot[slot] = HOT_EMPTY;
                            }
                        }
                        if self.diagnostics
                            && (self.refused_targets.len() < Self::REFUSED_CAP
                                || self.refused_targets.contains_key(&t))
                        {
                            let e = self.refused_targets.entry(t).or_insert((0, end_start));
                            e.0 += 1;
                            e.1 = end_start;
                        }
                    }
                }
                if self.diagnostics {
                    if let Some(slot) = refusal.slot() {
                        self.link_refusals[slot] += 1;
                    }
                }
            }
        }
        let ctx = &self.ctx;

        // Run the instruction the block stopped before, rather than returning
        // and being asked about it. See `CachedBlock::exit_needs_interpreter`.
        // `pc_modified` is necessarily clear here — a block that branched away
        // ends on the branch, not before an untranslatable instruction — so the
        // interpreter picks up exactly where the block left off.
        //
        // On an exact-slice core the append must also honour the run loop's
        // own boundary: the pure interpreter runs that next instruction only
        // if the block's cycles had not already exhausted the slice, and
        // running it unconditionally re-opened a one-instruction overshoot —
        // a few cycles per active slice, absorbed by the halt on quiet ones
        // but visible to the tick-lockstep forensics as a ±1-instruction
        // drift. Skipped, the next slice scans the address and interprets it
        // there, exactly as the no-JIT run would have.
        let exit_needs_interpreter = exit_needs_interpreter
            && ctx.exit_idx == 0
            && ctx.stop == 0
            && (!C::EXACT_SLICES || cycles < budget);
        self.last_exit_interpreted = exit_needs_interpreter;
        // Arm the successor-edge census. An exit that hands the interpreter its
        // own instruction is not linkable, so it deliberately leaves `chain_at`
        // behind — the counter moves past it and the edge is broken.
        if self.diagnostics && !exit_needs_interpreter {
            self.chain_edges += 1;
            self.chain_at = C::gba(cpu).instrs;
        }
        if exit_needs_interpreter {
            return Some(cycles + C::step(cpu, mmu));
        }
        Some(cycles)
    }

    /// Translate the block at `start` and cache it. `false` means nothing could
    /// be built, so the caller must interpret one instruction and try again.
    fn translate(
        &mut self,
        cpu: &C::Cpu,
        mmu: &mut NdsMmu,
        start: u32,
        max_instrs: usize,
    ) -> bool {
        let thumb = C::gba(cpu).registers.get_flag(FLAG_T);
        let block =
            Self::scan_from(cpu, mmu, start, thumb, self.follow_conditional, self.dispatch);
        // Computed unconditionally: it is one match per *compilation* (152 on
        // the player's scene) and it is stored on the block so the per-entry
        // census below can be taken.
        // Slots 0-8 are `ExitReason`; the three non-`Interpret` exits get their
        // own slots because collapsing them hid the answer. Per *entry* they
        // are 72.5% of all block ends — the dominant terminator by a wide
        // margin — and "length cap or store or folded branch" names three
        // unrelated fixes.
        let exit_slot = match block.exit {
            crate::jit::block::BlockExit::Interpret { reason, .. } => reason as usize,
            crate::jit::block::BlockExit::LengthCap => 9,
            crate::jit::block::BlockExit::AfterStore => 10,
            // Split by whether the folded branch goes back to this block's own
            // first instruction. That sub-case is a **loop body**, and it is
            // the one a compiled block could iterate internally: the target is
            // a compile-time constant equal to the entry point, so no successor
            // lookup is needed at all.
            //
            // Sound to iterate in place because such a block provably contains
            // no store — a store ends the block as `AfterStore` instead — so it
            // can neither invalidate its own code nor write IPCSYNC, and this
            // run loop already guarantees no interrupt can arise mid-slice.
            // Dispatching terminators reuse the labels of the exits they used
            // to be: `BX`/`BLX(reg)` was census slot 1, `LDM pc` slot 3, a
            // data-processing PC write slot 2 — each label now means "ended
            // WITH it compiled" rather than "stopped before it".
            crate::jit::block::BlockExit::AfterExchange => {
                let (_, inst) = block.body()[block.body().len() - 1];
                if block.thumb {
                    // Thumb F5 BX/BLX -> the BX label; POP {..,pc} -> LoadsPc.
                    if (inst & 0xFF00) == 0x4700 {
                        1
                    } else {
                        3
                    }
                } else if crate::jit::block::is_register_exchange(inst) {
                    1
                } else if crate::jit::block::is_pc_load_multiple(inst) {
                    3
                } else {
                    2
                }
            }
            // A compiled MSR terminator: census slot 7, the MSR label.
            crate::jit::block::BlockExit::AfterStatus => 7,
            crate::jit::block::BlockExit::AfterBranch => {
                let (addr, inst) = block.body()[block.body().len() - 1];
                // The scanner *pushes* the branch into the body before ending,
                // so these are compiled branches that merely stop the trace —
                // not branches it refused. Three very different cases:
                //
                // * conditional: two live successors, so the trace cannot
                //   follow it. The fall-through is the one the block could
                //   continue into, which is what an early-exit block buys.
                // * unconditional back into the trace: a loop edge.
                // * unconditional to this block's own start: a self-loop.
                if block.thumb || (inst >> 28) != 0xE {
                    13
                } else if crate::jit::block::branch_target(addr, inst) == start {
                    12
                } else {
                    11
                }
            }
        };
        if self.diagnostics {
            self.exits[exit_slot] += 1;
            if block.body().is_empty() {
                self.empty_exits[exit_slot] += 1;
            }
        }
        if block.body().len() < self.min_block_instrs {
            self.note_stop(if block.body().is_empty() {
                StopReason::ScannerRejectedFirst
            } else {
                StopReason::TooShort
            });
            return false;
        }
        let limit = block.body().len().min(max_instrs);
        // With linking, the per-exit successor slots are allocated **before**
        // translation so their addresses can be baked into the code as
        // immediates. The box's heap cells never move — moving the box into
        // the record later moves the pointer, not the cells.
        let slots: Box<[SlotCell]> = if self.link {
            (0..MAX_EXIT_SLOTS).map(|_| SlotCell::new()).collect()
        } else {
            Box::from([])
        };
        let link_plan = self.link.then(|| LinkSlots {
            addrs: std::array::from_fn(|i| &slots[i].cell as *const AtomicU64 as u64),
        });
        let dispatch_plan = self.dispatch.then(|| DispatchPlan {
            table: self.dispatch_table.as_ptr() as u64,
            mask: (self.dispatch_table.len() - 1) as u32,
        });
        let Some(compiled) = compile_with(
            &block.body()[..limit],
            thumb,
            link_plan.as_ref(),
            dispatch_plan.as_ref(),
            EmitCfg { armv5: C::ARMV5, bus: C::BUS },
        ) else {
            self.note_stop(StopReason::TranslatorRefused);
            return false;
        };
        if compiled.instructions < self.min_block_instrs {
            self.note_stop(StopReason::TranslatorRefused);
            return false;
        }
        // Captured *before* the block runs. A block that writes its own page
        // moves that version, so the next visit recompiles — conservative, and
        // exactly what self-modifying code needs.
        // The trace's own fall-through, which is where the pipeline refill reads.
        let next =
            block.body()[compiled.instructions - 1].0.wrapping_add(block.step_bytes());
        let Some(guard) = Self::guard_for(cpu, mmu, &block.body()[..compiled.instructions], next)
        else {
            // Untracked memory, or a trace spanning more than `Guard::MAX_PAGES`
            // pages. Counted: this was the one decline path with no stop reason,
            // so it disappeared into `declined` and could not be told apart from
            // an ordinary refusal. It matters more as traces get longer, since a
            // longer trace touches more pages.
            self.note_stop(StopReason::Unguardable);
            return false; // not memory whose changes can be tracked: do not cache
        };
        // Read now, for the reason on `CachedBlock::next_pipeline`.
        let mut next_pipeline = if block.thumb {
            [
                u32::from(C::read_halfword(mmu, next)),
                u32::from(C::read_halfword(mmu, next.wrapping_add(2))),
            ]
        } else {
            [C::read_word(mmu, next), C::read_word(mmu, next.wrapping_add(4))]
        };
        // **A one-instruction block's fall-through is still inside the entry
        // prefetch window.** The interpreter fetched `start + step` before
        // this block was ever entered — it is `pipeline[1]` right now — and a
        // store in a PREVIOUS block may have rewritten that address since, so
        // reading memory here hands the interpreter a word it never fetched.
        // (Blocks of two or more instructions re-fetch their fall-through
        // *inside* the block, after any prior store, so memory is exact for
        // them — and a block's own store is always its last instruction,
        // fetched past before it runs.) Found by thumb fuzz seed 518, where
        // garbage-ARM self-modification rewrote a 1-instruction block's
        // fall-through between chains.
        if compiled.instructions == 1 {
            next_pipeline[0] = C::gba(cpu).pipeline[1];
        }

        let Some(entry) = self.publish(&compiled.code) else {
            return false;
        };
        // Only when the scanner stopped *before* an instruction, and the
        // translator consumed the whole body it was given — a truncated body
        // ends somewhere the scanner never classified.
        let exit_needs_interpreter = compiled.instructions == block.body().len()
            && matches!(block.exit, crate::jit::block::BlockExit::Interpret { .. });

        let mut slots = slots;
        let mut exits: [ExitLink; MAX_EXIT_SLOTS] =
            std::array::from_fn(|_| ExitLink::none());
        let mut body_entry = 0u64;
        if self.link {
            let base = entry as usize as u64;
            body_entry = base + compiled.body_entry as u64;
            for exit in &compiled.exits {
                let i = usize::from(exit.index);
                // Unlinked, a slot points at its exit's own epilogue, so the
                // `jmp [slot]` is behaviour-preserving until a link is written.
                let unlinked = base + exit.epilogue_offset as u64;
                slots[i].unlinked = unlinked;
                slots[i].cell.store(unlinked, Ordering::Relaxed);
                // An exit the interpreter finishes is never linkable: the next
                // instruction is the one the recompiler could not translate.
                exits[i].target = if i == 0 && exit_needs_interpreter {
                    None
                } else {
                    exit.target
                };
                exits[i].dispatch = exit.dispatch;
            }
            // Stores to these pages must move the link epoch. Never unset —
            // a stale-high bit costs an extra chain break, not correctness.
            for &(page, _) in &guard.pages[..usize::from(guard.page_count)] {
                mmu.mark_code_page(usize::from(page), C::PAGE_MASK);
            }
            // Replacing a record drops the slot cells other blocks' baked
            // immediates point at, and leaves stale links aimed at the old
            // code. No link may survive a replacement.
            if self.cache.contains_key(&start) {
                self.flush_links();
            }
        }

        self.compilations += 1;
        self.cache.insert(
            start,
            CachedBlock {
                entry,
                guard,
                next_pipeline,
                exit_needs_interpreter,
                last_addr: next.wrapping_sub(4),
                early_exit: compiled.early_exit,
                exit_slot: exit_slot as u8,
                thumb: compiled.thumb,
                worst_cycles: compiled.worst_cycles,
                body_entry,
                exits,
                slots,
            },
        );
        // The words this translation consumed, for `try_revalidate`.
        self.verify.insert(
            start,
            block.body()[..compiled.instructions].to_vec().into_boxed_slice(),
        );
        true
    }

    /// Bytes a pipeline refill at `start` would read, in the block's own
    /// instruction set — the link-time stand-in for `Guard`'s pipeline half.
    fn refill_words(mmu: &mut NdsMmu, start: u32, thumb: bool) -> [u32; 2] {
        if thumb {
            [
                u32::from(C::read_halfword(mmu, start)),
                u32::from(C::read_halfword(mmu, start.wrapping_add(2))),
            ]
        } else {
            [C::read_word(mmu, start), C::read_word(mmu, start.wrapping_add(4))]
        }
    }

    /// What must stay true for the block at `start` covering `n` instructions.
    ///
    /// The guarded span runs to `n * 4 + 8`: the body, plus the two words the
    /// pipeline refill reads past it. At most 72 bytes, so it touches one page
    /// or two.
    fn guard_for(
        cpu: &C::Cpu,
        mmu: &NdsMmu,
        body: &[(u32, u32)],
        next: u32,
    ) -> Option<Guard> {
        // Every distinct page the trace reads from, including the two words the
        // pipeline refill takes past the end. A trace follows branches, so these
        // are not contiguous and cannot be derived from a start and a length.
        let mut pages = [(0u16, 0u32); Guard::MAX_PAGES];
        let mut page_count = 0usize;

        let add = |page: usize, pages: &mut [(u16, u32); Guard::MAX_PAGES], count: &mut usize| {
            if pages[..*count].iter().any(|&(p, _)| usize::from(p) == page) {
                return true;
            }
            if *count == Guard::MAX_PAGES {
                return false; // too scattered to guard cheaply; do not cache
            }
            pages[*count] = (page as u16, mmu.code_version(page));
            *count += 1;
            true
        };

        for &(addr, _) in body {
            let page = C::code_page(mmu, addr)?;
            if !add(page, &mut pages, &mut page_count) {
                return None;
            }
        }
        for probe in [next, next.wrapping_add(4)] {
            let page = C::code_page(mmu, probe)?;
            if !add(page, &mut pages, &mut page_count) {
                return None;
            }
        }
        Some(Guard { pages, page_count: page_count as u8, pipeline: C::gba(cpu).pipeline })
    }

    /// The preconditions `Arm9Cpu::step` checks before it fetches, plus the
    /// touch read-watch. See the module documentation for why each one is here.
    pub fn may_run(&self, cpu: &C::Cpu, mmu: &NdsMmu) -> bool {
        // **Thumb first, and it is a method for this reason.** When Thumb is
        // disabled it is the single most common rejection, and refusing it here
        // costs one load. Refusing it later — in `translate`, which is where
        // the gate first went — still pays the address computation, the filter
        // probe, the ownership move and the call, and measured **4% slower**
        // than refusing it here.
        let gba = C::gba(cpu);
        (self.thumb || !gba.registers.get_flag(FLAG_T))
            && !gba.halted
            // A pending pipeline refill; see [`Self::service_refills`].
            && (self.service_refills || !gba.pc_modified)
            && !C::watch_armed(mmu)
            && !C::irq_pending(gba, mmu)
    }

    /// Which [`may_run`](Self::may_run) precondition failed.
    ///
    /// `may_run` answers one bit because it is on the hottest path in the
    /// emulator — 50 million calls per 60 ticks — and returning a richer value
    /// from it risks the 4% that stop-counting already cost once. This is the
    /// diagnostic twin, called only when accounting is on, and
    /// `may_run_agrees_with_refusal_detail` pins the two together so the
    /// duplication cannot drift.
    ///
    /// Order matches `may_run`'s short-circuit order, so a state that fails
    /// several preconditions is attributed to the same one either way.
    #[cold]
    pub fn refusal_detail(&self, cpu: &C::Cpu, mmu: &NdsMmu) -> Option<StopReason> {
        let gba = C::gba(cpu);
        if !self.thumb && gba.registers.get_flag(FLAG_T) {
            Some(StopReason::Thumb)
        } else if gba.halted {
            Some(StopReason::Halted)
        } else if !self.service_refills && gba.pc_modified {
            Some(StopReason::PendingRefill)
        } else if C::watch_armed(mmu) {
            Some(StopReason::TouchWatch)
        } else if C::irq_pending(gba, mmu) {
            Some(StopReason::PendingIrq)
        } else {
            None
        }
    }

    /// Scan from `start`, reading the first two words out of the **pipeline**
    /// rather than out of memory.
    ///
    /// This is not an optimisation. The interpreter executes `pipeline[0]`, which
    /// was fetched before any store the previous instructions made. Code that
    /// rewrites itself one or two instructions ahead therefore runs the *old*
    /// word, and a block that read memory instead would run the new one.
    fn scan_from(
        cpu: &C::Cpu,
        mmu: &mut NdsMmu,
        start: u32,
        thumb: bool,
        follow_conditional: bool,
        dispatch: bool,
    ) -> Block {
        let pipeline = C::gba(cpu).pipeline;
        let step = if thumb { 2 } else { 4 };
        // The pipeline words stand in for the first two *fetches in time*, not
        // for their addresses in perpetuity. A trace that follows a backward
        // branch and runs through `start` again must read **memory** there,
        // exactly as the interpreter re-fetches it — keying the substitution
        // on address alone compiled the stale pipeline words into the revisit,
        // which diverges when a store between the two traversals rewrote them
        // (found by `linked_chains_match_the_interpreter`, seed 224).
        let mut fetches = 0u32;
        crate::jit::block::scan_state_dispatch(start, thumb, follow_conditional, dispatch, |addr| {
            fetches += 1;
            match addr.wrapping_sub(start) {
                0 if fetches == 1 => pipeline[0],
                d if d == step && fetches == 2 => pipeline[1],
                _ if thumb => u32::from(C::read_halfword(mmu, addr)),
                _ => C::read_word(mmu, addr),
            }
        })
    }

    /// Bytes reserved per code arena.
    ///
    /// The player's scene compiles 152 blocks of a few hundred bytes each, so
    /// one 64 KiB arena holds the whole working set on a handful of pages
    /// instead of 152 of them.
    const ARENA_BYTES: usize = 64 * 1024;

    /// Append `code` to the current arena and return its entry point.
    ///
    /// Blocks are packed together rather than given a page each: see
    /// [`ExecBuffer::reopen`] for the iTLB and cache-density reason, and for
    /// why re-opening does not weaken W^X.
    fn publish(&mut self, code: &[u8]) -> Option<crate::jit::exec_mem::BlockFn> {
        // Re-open the arena if there is room, otherwise start a new one. An
        // arena is retired, never resized: a compiled block must not move,
        // because emitted branches are relative to where they landed.
        let mut buf = match self.pages.pop() {
            Some(arena) if arena.spare() >= code.len() => match arena.reopen() {
                Ok(buf) => buf,
                // **Put it back.** The arena still holds every block published
                // into it, and their entry pointers are live in the cache;
                // dropping it here would be a use-after-free on the next entry.
                // Declining to compile is always safe.
                Err((arena, _)) => {
                    self.pages.push(arena);
                    return None;
                }
            },
            Some(arena) => {
                self.pages.push(arena);
                CodeBuffer::with_capacity(Self::ARENA_BYTES.max(code.len())).ok()?
            }
            None => CodeBuffer::with_capacity(Self::ARENA_BYTES.max(code.len())).ok()?,
        };
        let offset = buf.push(code).ok()?;
        let exec = match buf.finalize() {
            Ok(exec) => exec,
            // The arena cannot be made executable again, so every block already
            // published into it is unreachable code behind a non-executable
            // page. Drop the whole cache first: that is what makes it safe to
            // let the region go, because no entry pointer survives.
            Err(_) => {
                self.clear();
                return None;
            }
        };
        let entry = exec.entry(offset).ok()?;
        self.pages.push(exec);
        Some(entry)
    }

    /// Restore the interpreter-visible state the block did not maintain.
    fn reconcile(
        cpu: &mut C::Cpu,
        mmu: &mut NdsMmu,
        last_addr: u32,
        next_pipeline: [u32; 2],
        thumb: bool,
        early_exit: Option<crate::jit::compile::EarlyExit>,
        ctx: &JitContext,
    ) {
        // A block that left through its alternate exit ended at a different
        // address, which is still a compile-time constant captured at the exit
        // point. The retired count and the histogram are **not** — they are
        // accumulated by the block, because a chain of linked blocks has no
        // single constant for either.
        //
        // `== 1` and not `!= 0`: linked epilogues write their exit index, and
        // index 2 — a terminal conditional branch's taken path — is not the
        // alternate exit and must not select its tuple.
        let early = if ctx.exit_idx == 1 { early_exit } else { None };
        let last_addr = early.map_or(last_addr, |e| e.last_addr);
        let n = ctx.retired;

        let gba = C::gba_mut(cpu);
        // The block owns CPSR for its whole run and writes the whole word back,
        // so this restores the mode and T bits unchanged along with NZCV.
        gba.registers.cpsr = ctx.cpsr;

        // A compiled branch sets this when it is taken. R15 already holds the
        // target — the block wrote it over the fall-through address — so all
        // that is left is to tell the next step to refill the pipeline instead
        // of advancing.
        if ctx.pc_modified != 0 {
            gba.pc_modified = true;
        }

        // Read at translation time, *before* the block's last instruction ran —
        // which is where the interpreter reads them too. See
        // `CachedBlock::next_pipeline`.
        gba.pipeline = next_pipeline;

        // Diagnostics the MMU write paths use to attribute a store to the game
        // function that made it. `exec_pc` is the last body instruction's own
        // address; `exec_lr` is R14 as it stood before that instruction ran,
        // which the block captured for exactly this reason. (No-op on cores
        // whose `step` maintains no such diagnostics.)
        C::publish_exec(mmu, last_addr, ctx.exec_lr);

        // Retirement counters. `instrs` is bumped at the top of `execute_arm`,
        // before the condition is even checked, so every instruction counts.
        gba.instrs = gba.instrs.wrapping_add(u64::from(n));
        if thumb {
            // `execute_thumb` counts these separately, and the ratio is what
            // tells a future pass how much of the workload each instruction set
            // actually is.
            gba.thumb_instrs = gba.thumb_instrs.wrapping_add(u64::from(n));
        }
        // `arm_class_hist` is bumped only *after* the condition passes, so the
        // conditional instructions count themselves. The unconditional ones are
        // now banked by the epilogue into the same array, which is what lets an
        // early exit — and, later, a chain — carry the tally of what actually
        // ran rather than of what the block contains.
        for (bucket, &hits) in ctx.class_hits.iter().enumerate() {
            if hits != 0 {
                gba.arm_class_hist[bucket] += u64::from(hits);
            }
        }
    }
}

/// Write a successor link into `from`'s exit `exit`, if it is safe to.
///
/// A free function over the map (not a method) because the caller must bump
/// `links_written`/`links_live` afterwards, and holding `&self.cache` across an
/// `&mut self` counter update is exactly the borrow the compiler forbids.
///
/// The link is written only when the target is **valid right now**:
///
/// * same instruction set — the cache is keyed by address alone;
/// * every guarded page at the version it was compiled from;
/// * the memory words at the target equal the two the target was compiled
///   from. This stands in for `Guard::still_valid`'s pipeline comparison: a
///   linked entry skips the flush the interpreter would do, and memory is what
///   that flush would have read.
///
/// Staying valid afterwards is the epoch's job: any store that could change
/// these facts moves `NdsMmu::code_write_epoch`, which breaks the chain at the
/// next exit and tears all links down before the next one starts.
/// Why a link or dispatch write did not happen. Census-only: naming the
/// refusal is what turns "22.3% of chain ends are unlinked" into a workstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkRefusal {
    /// No exit record, already linked, or no static target: nothing to fix.
    NotApplicable,
    /// The successor never compiled (declined) or was dropped.
    Uncompiled,
    /// The successor is cached for the other instruction set.
    ThumbMismatch,
    /// The memory words at the target no longer match its compiled words.
    WordsChanged,
    /// A guarded page's version moved.
    PagesStale,
}

impl LinkRefusal {
    /// Slot in `Arm9Jit::link_refusals`, or `None` for the uncounted case.
    fn slot(self) -> Option<usize> {
        match self {
            Self::NotApplicable => None,
            Self::Uncompiled => Some(0),
            Self::ThumbMismatch => Some(1),
            Self::WordsChanged => Some(2),
            Self::PagesStale => Some(3),
        }
    }
}

/// Validate `target` as a chain successor and return its body entry.
fn validated_body_entry<C: JitCore>(
    cache: &BlockMap,
    target: u32,
    want_thumb: bool,
    mmu: &mut NdsMmu,
) -> Result<u64, LinkRefusal> {
    let words = Jit::<C>::refill_words(mmu, target, want_thumb);
    let Some(succ) = cache.get(&target) else {
        return Err(LinkRefusal::Uncompiled);
    };
    if succ.thumb != want_thumb || succ.body_entry == 0 {
        return Err(LinkRefusal::ThumbMismatch);
    }
    if succ.guard.pipeline != words {
        return Err(LinkRefusal::WordsChanged);
    }
    if !succ.guard.pages_current(mmu) {
        return Err(LinkRefusal::PagesStale);
    }
    Ok(succ.body_entry)
}

fn try_write_link<C: JitCore>(
    cache: &BlockMap,
    from: u32,
    exit: usize,
    mmu: &mut NdsMmu,
) -> Result<(), LinkRefusal> {
    let Some(rec) = cache.get(&from) else {
        return Err(LinkRefusal::NotApplicable);
    };
    let Some(link) = rec.exits.get(exit) else {
        return Err(LinkRefusal::NotApplicable);
    };
    if link.linked.get() {
        return Err(LinkRefusal::NotApplicable); // the chain broke for another reason
    }
    let Some(target) = link.target else {
        return Err(LinkRefusal::NotApplicable);
    };
    let body = validated_body_entry::<C>(cache, target, rec.thumb, mmu)?;
    rec.slots[exit].cell.store(body, Ordering::Relaxed);
    link.linked.set(true);
    Ok(())
}

/// Install a dispatch-table entry for `target`, if it is safe to — the same
/// validity contract as [`try_write_link`], with the same epoch-based
/// teardown keeping it true afterwards. A colliding entry is simply evicted;
/// a miss never costs more than the unlinked exit always did.
fn try_write_dispatch<C: JitCore>(
    cache: &BlockMap,
    table: &[DispatchSlot],
    mask: u32,
    target: u32,
    mmu: &mut NdsMmu,
) -> Result<usize, LinkRefusal> {
    // Bit 0 partitions the table: an odd target is a Thumb interworking
    // value whose successor is the Thumb block at the aligned address
    // (`flush_pipeline` masks `& !1` before fetching), an even one is ARM.
    // The tag stores the RAW value the probe reads out of R15, so the two
    // populations can never satisfy each other's compare.
    let thumb = target & 1 != 0;
    let key = if thumb { target & !1 } else { target };
    if !thumb && key & 3 != 0 {
        return Err(LinkRefusal::NotApplicable); // misaligned ARM: never cached
    }
    let body = validated_body_entry::<C>(cache, key, thumb, mmu)?;
    let idx = dispatch_slot(target, mask);
    let slot = &table[idx];
    slot.tag.store(target, Ordering::Relaxed);
    slot.body.store(body, Ordering::Relaxed);
    Ok(idx)
}

/// Run one compiled block (or chain) if the core's recompiler has one,
/// otherwise interpret a single instruction. The shared body of
/// `Arm9Cpu::run` and `Arm7Cpu::run`'s per-iteration step.
///
/// The recompiler is moved out of its slot for the call because it needs
/// `&mut C::Cpu`, and moving an `Option<Box<..>>` is two pointer writes
/// against ~24 ns of emulated work. With no recompiler enabled this is one
/// never-taken branch and the interpreter is reached directly.
#[inline]
pub fn step_or_block<C: JitCore>(cpu: &mut C::Cpu, mmu: &mut NdsMmu, budget: u32) -> u32 {
    if C::jit_ref(cpu).is_none() {
        return C::step(cpu, mmu);
    }

    // `may_run` first, and deliberately: it rejects the overwhelming share of
    // stand-downs in about three loads. Consulting the address filter ahead of
    // it meant computing a key and probing a 256 KiB table for cases `may_run`
    // was about to reject anyway.
    if !C::jit_ref(cpu).is_some_and(|jit| jit.may_run(cpu, mmu)) {
        // Which precondition failed, for the stop histogram. Guarded by one
        // predictable load: the tally itself measured 4% on this path.
        if C::jit_ref(cpu).is_some_and(|jit| jit.diagnostics_on()) {
            let reason = C::jit_ref(cpu)
                .and_then(|jit| jit.refusal_detail(cpu, mmu))
                .unwrap_or(StopReason::NotRunnable);
            // Read before `step` moves it: the witness that decides whether
            // this refusal broke a successor edge. See `Jit::note_chain_loss`.
            let instrs = C::gba(cpu).instrs;
            if let Some(jit) = C::jit_slot(cpu).as_mut() {
                jit.note_stop(reason);
                jit.note_chain_loss(ChainLoss::Refused(reason), instrs);
            }
        }
        return C::step(cpu, mmu);
    }

    // The pipeline may still be pending a refill: `may_run` permits that when
    // refill servicing is on. Normalise it here, before R15 is read — the
    // refill moves R15, and deriving the block address from the stale value
    // files correct code under the wrong key.
    if C::gba(cpu).pc_modified {
        C::flush_pipeline(cpu, mmu);
    }

    // R15 leads the executing instruction by two instruction widths.
    let start = C::gba(cpu).registers.gpr[15].wrapping_sub(Jit::<C>::pipeline_lead(cpu));
    let known_decline = C::jit_ref(cpu).is_some_and(|jit| jit.is_known_decline(start));
    if known_decline {
        // Counted here so the `declined` statistic still means "times the
        // recompiler had nothing to run", not "times it was asked twice".
        let instrs = C::gba(cpu).instrs;
        if let Some(jit) = C::jit_slot(cpu).as_mut() {
            jit.note_declined(start);
            jit.note_chain_loss(ChainLoss::KnownDecline, instrs);
        }
        return C::step(cpu, mmu);
    }

    let Some(mut jit) = C::jit_slot(cpu).take() else {
        return C::step(cpu, mmu); // unreachable: checked above
    };
    // **Refuted, do not re-attempt: chaining consecutive blocks here.** See
    // the measured record on the ARM9: a Rust-level chain was 2.1% slower —
    // the ~33 ns a block entry costs is in the cache lookup, the guard, the
    // context setup, the indirect call and `reconcile`, none of which a
    // Rust-level chain touches. Only chaining inside *generated code* removes
    // those, and that is what the linker/dispatcher do.
    let cycles = match jit.try_step(cpu, mmu, crate::jit::block::MAX_BODY_INSTRS, budget) {
        Some(cycles) => cycles,
        None => C::step(cpu, mmu),
    };
    *C::jit_slot(cpu) = Some(jit);
    cycles
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jit::difftest::{
        compare_lockstep, Advance, Arm9Backend, ArmClass, Fixture, Interpreter, Rng, CODE_BASE,
    };

    /// A data read can request an IRQ just as a store can. The instruction
    /// after the last Gamecard word must wait until that IRQ has been serviced.
    #[test]
    fn gamecard_read_irq_stops_before_the_next_instruction() {
        use crate::jit::difftest::Observation;

        for linked in [false, true] {
            for read in [0xE591_0000, 0x1591_0000, 0xE891_0009, 0x1891_0009] {
                // LDR/LDM, unconditional and NE (taken with the fixture's Z clear).
                for prefix in [false, true] {
                    let mut program = Vec::new();
                    if prefix {
                        program.push(0xE284_4001);
                    }
                    program.extend([read, 0xE282_2001, 0xEF00_0000]);
                    let fixture = Fixture::arm(&program, 41);
                    let prepare = || {
                        let (mut cpu, mut mmu) = fixture.instantiate();
                        cpu.cpu.registers.gpr[1] = 0x0410_0010;
                        cpu.cpu.registers.gpr[2] = 0;
                        mmu.arm9_ime = 1;
                        mmu.arm9_ie = 1 << 19;
                        mmu.arm9_if = 0;
                        mmu.write_halfword_arm9(0x0400_01A0, 1 << 14);
                        mmu.write_byte_arm9(0x0400_01A8, 0xB8);
                        mmu.write_word_arm9(0x0400_01A4, 0x8700_0000); // 4-byte chip ID
                        if !prefix {
                            // This word was prefetched before a previous write.
                            // The read's early exit must preserve that old pipeline.
                            mmu.write_word_arm9(CODE_BASE + 4, 0xE282_2002);
                        }
                        (cpu, mmu)
                    };
                    let (mut jc, mut jm) = prepare();
                    let (mut ic, mut im) = prepare();
                    let mut jit = Arm9Jit::with_min_block_instrs(1);
                    jit.set_link_enabled(linked);
                    let actual = jit.try_step(&mut jc, &mut jm, 16, 64).expect("compiled read");
                    let retired = 1 + u64::from(prefix);
                    let expected = (0..retired).map(|_| ic.step(&mut im)).sum();
                    assert_eq!(jc.cpu.registers.gpr[2], 0, "IRQ must precede ADD; {read:#x}, link={linked}");
                    assert_eq!(jm.arm9_if & (1 << 19), 1 << 19);
                    assert_eq!(jit.stats().compiled_instrs, retired, "prefix and read compiled");
                    assert!(
                        Observation::capture(&mut jc, &mut jm, actual)
                            == Observation::capture(&mut ic, &mut im, expected),
                        "read boundary differs; {read:#x}, link={linked}",
                    );
                    assert_eq!(jc.step(&mut jm), 4, "next step services IRQ");
                    assert_eq!(ic.step(&mut im), 4);
                    assert!(
                        Observation::capture(&mut jc, &mut jm, actual + 4)
                            == Observation::capture(&mut ic, &mut im, expected + 4),
                        "IRQ entry differs; {read:#x}, link={linked}",
                    );
                }
            }
        }
    }

    #[test]
    fn gamecard_read_irq_stops_an_already_linked_chain() {
        let fixture = Fixture::arm(
            &[
                0xE584_3000, // STR r3,[r4] ends the first block
                0xE591_0000, // LDR r0,[r1] can interrupt the second block
                0xE282_2001, // ADD r2,r2,#1
                0xEAFF_FFFB, // B back to STR
            ],
            59,
        );
        let (mut cpu, mut mmu) = fixture.instantiate();
        cpu.cpu.registers.gpr[1] = 0x0410_0010;
        cpu.cpu.registers.gpr[4] = CODE_BASE + 0x3000;
        let mut jit = Arm9Jit::with_min_block_instrs(1);
        jit.set_link_enabled(true);
        for _ in 0..6 {
            jit.try_step(&mut cpu, &mut mmu, 16, 64).expect("warm linked loop");
        }
        assert!(jit.link_stats().0 > 0, "the successor links must exist");
        // Trace formation follows B into STR, so the warmed second block
        // loops into itself. Revisit the separate STR entry once to install
        // that entry's edge to the now-compiled read block as well.
        cpu.cpu.registers.gpr[15] = CODE_BASE;
        cpu.cpu.pc_modified = true;
        jit.try_step(&mut cpu, &mut mmu, 16, 64).expect("link the separate STR entry");
        assert!(jit.cache[&CODE_BASE].exits[0].linked.get(), "the exact edge under test is linked");
        cpu.cpu.registers.gpr[15] = CODE_BASE;
        cpu.cpu.pc_modified = true;
        cpu.cpu.registers.gpr[2] = 0;
        mmu.arm9_ime = 1;
        mmu.arm9_ie = 1 << 19;
        mmu.arm9_if = 0;
        mmu.write_halfword_arm9(0x0400_01A0, 1 << 14);
        mmu.write_byte_arm9(0x0400_01A8, 0xB8);
        mmu.write_word_arm9(0x0400_01A4, 0x8700_0000);
        let before = jit.stats().compiled_instrs;
        let cycles = jit.try_step(&mut cpu, &mut mmu, 16, 64).expect("cached chain");
        assert_eq!(jit.stats().compiled_instrs - before, 2, "STR then LDR, across their link");
        assert_eq!(cycles, 6, "cycles include the preceding linked block");
        assert_eq!(cpu.cpu.registers.gpr[2], 0, "ADD waits for IRQ");
        assert_eq!(cpu.step(&mut mmu), 4, "next step enters IRQ");
    }

    /// BX can revisit an ARM block as Thumb with identical prefetched words.
    /// Neither a direct cache hit nor page revalidation may reuse its ARM code.
    #[test]
    fn arm_to_thumb_cache_entries_require_the_same_instruction_set() {
        use crate::jit::difftest::Observation;

        for stale_page in [false, true] {
            let fixture = Fixture::arm(&[0, 0, 0xE12F_FF11], 73);
            let prepare = || {
                let (mut cpu, mmu) = fixture.instantiate();
                cpu.cpu.registers.cpsr = 0x1F; // Z clear: ARM ANDEQ instructions skip
                cpu.cpu.registers.gpr[0] = 0x8000_0000;
                cpu.cpu.registers.gpr[1] = CODE_BASE | 1;
                (cpu, mmu)
            };
            let (mut jc, mut jm) = prepare();
            let (mut ic, mut im) = prepare();
            let mut jit = Arm9Jit::with_min_block_instrs(1);
            jit.set_link_enabled(false);
            jit.set_thumb_enabled(true);
            jit.try_step(&mut jc, &mut jm, 16, 64).expect("ARM prefix compiles");
            for _ in 0..3 {
                ic.step(&mut im);
            }
            assert!(jc.cpu.registers.get_flag(FLAG_T), "BX selected Thumb");
            if stale_page {
                jm.write_word_arm9(CODE_BASE + 0x300, 123);
                im.write_word_arm9(CODE_BASE + 0x300, 123);
            }
            let actual = jit.try_step(&mut jc, &mut jm, 1, 64).expect("Thumb instruction compiles");
            let expected = ic.step(&mut im);
            assert!(
                Observation::capture(&mut jc, &mut jm, actual)
                    == Observation::capture(&mut ic, &mut im, expected),
                "ARM code reused as Thumb; stale_page={stale_page}",
            );
            assert!(jc.cpu.registers.get_flag(1 << 31));
        }
    }

    /// The recompiler as a differential backend: run a **single-instruction**
    /// block where possible, otherwise interpret.
    ///
    /// One instruction per block is the strictest form of the comparison — the
    /// harness sees the block's prologue and epilogue on every instruction, so a
    /// mistake in either shows immediately rather than being masked by a long
    /// body that happens to end in the right state.
    struct JitBackend {
        jit: Arm9Jit,
    }

    impl Arm9Backend for JitBackend {
        fn name(&self) -> &'static str {
            "jit"
        }
        fn step(&mut self, cpu: &mut Arm9Cpu, mmu: &mut NdsMmu) -> Advance {
            let before = self.jit.compiled_instrs;
            // Budget 64, not MAX: with linking enabled (e.g. via the
            // environment) an unbounded budget would let a self-looping
            // one-instruction block chain forever.
            match self.jit.try_step(cpu, mmu, 1, 64) {
                // A `try_step` can cover the block *and* the instruction it
                // stopped before, so report what actually retired rather than
                // assuming one.
                Some(cycles) => {
                    let compiled = (self.jit.compiled_instrs - before) as u32;
                    let interpreted = u32::from(self.jit.last_step_interpreted_exit());
                    Advance { cycles, instructions: compiled + interpreted }
                }
                None => Advance::one(cpu.step(mmu)),
            }
        }
    }

    /// Seeds the **ARM** differential fuzz run, overridable with
    /// `EMU_JIT_FUZZ_SEEDS`.
    ///
    /// It was 40, and 40 was demonstrably too few: a sweep to 4000 found three
    /// real Thumb miscompiles with the whole suite green. The ARM translator is
    /// clean to 4000, so this default is set by what the suite can afford —
    /// 500 costs +0.08s on a 2.29s run for a 12.5x corpus.
    fn fuzz_seeds() -> u64 {
        std::env::var("EMU_JIT_FUZZ_SEEDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n >= 2)
            .unwrap_or(500)
    }

    /// Seeds the **Thumb** fuzz run.
    ///
    /// This was capped at 40 for a long time as a known-defect marker: seed
    /// 518 failed on what looked like a T-bit divergence. Both real causes
    /// are fixed now — the one-instruction-block prefetch hole (seed 518) and
    /// misaligned wild-jump starts (seed 1918) — and the corpus is clean to
    /// 4000. The default matches the ARM corpus for the same suite-cost
    /// reason.
    fn thumb_fuzz_seeds() -> u64 {
        std::env::var("EMU_JIT_FUZZ_SEEDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n >= 2)
            .unwrap_or(500)
    }

    /// **The gate for this milestone.** Compiled code must leave exactly the
    /// state the interpreter leaves, after every instruction, over a corpus that
    /// mixes the supported subset with everything it must decline.
    #[test]
    fn compiled_blocks_match_the_interpreter() {
        let mut executed = 0u64;
        for seed in 1..fuzz_seeds() {
            let mut rng = Rng::new(seed);
            let program =
                crate::jit::difftest::random_program(&ArmClass::ALL, 24, &mut rng);
            let fixture = Fixture::arm(&program, seed);
            let mut jit = JitBackend { jit: Arm9Jit::with_min_block_instrs(1) };

            let result = compare_lockstep(&fixture, &mut Interpreter, &mut jit, 120);
            executed += jit.jit.compiled_instrs;
            result.unwrap_or_else(|d| panic!("seed {seed}: {d}"));
        }
        // A backend that declined everything would pass the comparison above by
        // being the interpreter. Prove the compiled path was actually taken.
        assert!(executed > 0, "no instruction was ever executed as compiled code");
    }

    /// ...and specifically over a corpus of *only* the supported subset, so the
    /// mixed run above cannot pass by declining almost everything.
    #[test]
    fn a_program_of_supported_instructions_is_mostly_compiled() {
        let mut rng = Rng::new(2026);
        let program =
            crate::jit::difftest::random_program(&[ArmClass::DataProcImm], 32, &mut rng);
        let fixture = Fixture::arm(&program, 2026);
        let mut jit = JitBackend { jit: Arm9Jit::with_min_block_instrs(1) };

        compare_lockstep(&fixture, &mut Interpreter, &mut jit, 300)
            .unwrap_or_else(|d| panic!("{d}"));

        let total = jit.jit.compiled_instrs + jit.jit.declined;
        assert!(
            jit.jit.compiled_instrs * 2 > total,
            "only {}/{} instructions were compiled",
            jit.jit.compiled_instrs,
            total
        );
    }

    /// The recompiler with **successor linking on**, run at full block size and
    /// a small chain budget, so chains genuinely form, break on the budget, and
    /// re-enter — the paths a single-instruction backend can never reach.
    struct ChainBackend {
        jit: Arm9Jit,
    }

    impl ChainBackend {
        fn linked() -> Self {
            let mut jit = Arm9Jit::with_min_block_instrs(1);
            jit.set_link_enabled(true);
            Self { jit }
        }

        fn dispatching() -> Self {
            let mut jit = Arm9Jit::with_min_block_instrs(1);
            jit.set_link_enabled(true);
            jit.set_dispatch_enabled(true);
            Self { jit }
        }
    }

    impl Arm9Backend for ChainBackend {
        fn name(&self) -> &'static str {
            "jit+link"
        }
        fn step(&mut self, cpu: &mut Arm9Cpu, mmu: &mut NdsMmu) -> Advance {
            let before = self.jit.compiled_instrs;
            match self.jit.try_step(cpu, mmu, crate::jit::block::MAX_BODY_INSTRS, 64) {
                Some(cycles) => {
                    let compiled = (self.jit.compiled_instrs - before) as u32;
                    let interpreted = u32::from(self.jit.last_step_interpreted_exit());
                    Advance { cycles, instructions: compiled + interpreted }
                }
                None => Advance::one(cpu.step(mmu)),
            }
        }
    }

    /// **The gate for successor linking.** Chains of linked blocks must leave
    /// exactly the state the interpreter leaves — registers, CPSR, memory,
    /// retired count, histogram and the cycle total — over a corpus whose
    /// programs loop, store, and end blocks on conditional branches, with a
    /// budget small enough that chains break and re-form constantly.
    #[test]
    fn linked_chains_match_the_interpreter() {
        let mut linked = 0u64;
        for seed in 1..fuzz_seeds() {
            let mut rng = Rng::new(seed);
            let program = crate::jit::difftest::random_program(&ArmClass::ALL, 24, &mut rng);
            let fixture = Fixture::arm(&program, seed);
            let mut inner = Arm9Jit::with_min_block_instrs(1);
            inner.set_link_enabled(true);
            let mut jit = ChainBackend { jit: inner };

            let result = compare_lockstep(&fixture, &mut Interpreter, &mut jit, 300);
            linked += jit.jit.link_stats().0;
            result.unwrap_or_else(|d| panic!("seed {seed}: {d}"));
        }
        // The comparison would pass with every slot unlinked — that is the
        // whole design. Prove links were actually written and followed.
        assert!(linked > 0, "no successor link was ever written");
    }

    /// **The dispatcher gate.** A register-target call/return loop — `BLX Rm`
    /// out, `BX lr` back — must match the interpreter exactly while chains
    /// flow through the dispatch table in both directions.
    #[test]
    fn dispatched_calls_and_returns_match_the_interpreter() {
        let program = [
            0xE3A0_4402, // MOV r4,#0x02000000
            0xE284_4020, // ADD r4,r4,#0x20        (r4 = &sub)
            0xE280_0001, // ADD r0,r0,#1           <- loop head
            0xE12F_FF34, // BLX r4                 (dispatch out)
            0xE281_1001, // ADD r1,r1,#1           <- return lands here
            0xEAFF_FFFB, // B .-12 (back to the loop head)
            0xE1A0_0000, // NOP (padding)
            0xE1A0_0000, // NOP
            0xE282_2001, // ADD r2,r2,#1           <- sub:
            0xE12F_FF1E, // BX lr                  (dispatch back)
        ];
        let fixture = Fixture::arm(&program, 21);
        let mut jit = ChainBackend::dispatching();
        jit.jit.set_diagnostics(true); // for the per-entry census witness below
        compare_lockstep(&fixture, &mut Interpreter, &mut jit, 400)
            .unwrap_or_else(|d| panic!("{d}"));
        assert!(jit.jit.dispatch_stats() > 0, "no dispatch entry was ever written");
        assert!(
            jit.jit.entry_exit_counts()[1] > 0,
            "no chain ever ended at a compiled BX"
        );
    }

    /// The other run-time branch: a function returning through
    /// `LDMFD sp!,{..,pc}` — the ARMv5 interworking load — inside a call loop.
    #[test]
    fn a_dispatched_ldm_return_matches_the_interpreter() {
        let program = [
            0xE280_0001, // ADD r0,r0,#1           <- loop head
            0xEB00_0002, // BL +2 -> 0x14          (folded by the scanner)
            0xE281_1001, // ADD r1,r1,#1           <- return lands here
            0xEAFF_FFFB, // B .-12 (back to the loop head)
            0xE1A0_0000, // NOP (padding)
            0xE92D_4004, // STMFD sp!,{r2,lr}      <- sub:
            0xE282_2001, // ADD r2,r2,#1
            0xE8BD_8004, // LDMFD sp!,{r2,pc}      (dispatch back)
        ];
        let fixture = Fixture::arm(&program, 22);
        let mut jit = ChainBackend::dispatching();
        compare_lockstep(&fixture, &mut Interpreter, &mut jit, 400)
            .unwrap_or_else(|d| panic!("{d}"));
        assert!(jit.jit.dispatch_stats() > 0, "no dispatch entry was ever written");
    }

    /// The third run-time branch: an ARMv4-style `MOV pc,lr` return, which
    /// must NOT interwork — T stays clear even though the interpreter is on an
    /// ARMv5 core, because data-processing writes to PC never route through
    /// `load_pc`.
    #[test]
    fn a_dispatched_mov_pc_return_matches_the_interpreter() {
        let program = [
            0xE280_0001, // ADD r0,r0,#1           <- loop head
            0xEB00_0002, // BL +2 -> 0x14
            0xE281_1001, // ADD r1,r1,#1           <- return lands here
            0xEAFF_FFFB, // B .-12 (back to the loop head)
            0xE1A0_0000, // NOP (padding)
            0xE282_2001, // ADD r2,r2,#1           <- sub:
            0xE1A0_F00E, // MOV pc,lr              (dispatch back)
        ];
        let fixture = Fixture::arm(&program, 24);
        let mut jit = ChainBackend::dispatching();
        compare_lockstep(&fixture, &mut Interpreter, &mut jit, 400)
            .unwrap_or_else(|d| panic!("{d}"));
        assert!(jit.jit.dispatch_stats() > 0, "no dispatch entry was ever written");
    }

    /// The ARM7 recompiler as a differential backend — the same strictest
    /// single-instruction-block form as [`JitBackend`], over the ARMv4T
    /// instantiation.
    struct Arm7JitBackend {
        jit: Arm7Jit,
    }

    impl crate::jit::difftest::Arm7Backend for Arm7JitBackend {
        fn name(&self) -> &'static str {
            "arm7-jit"
        }
        fn step(
            &mut self,
            cpu: &mut crate::nds::cpu::Arm7Cpu,
            mmu: &mut NdsMmu,
        ) -> Advance {
            let before = self.jit.compiled_instrs;
            match self.jit.try_step(cpu, mmu, 1, 64) {
                Some(cycles) => {
                    let compiled = (self.jit.compiled_instrs - before) as u32;
                    let interpreted = u32::from(self.jit.last_step_interpreted_exit());
                    Advance { cycles, instructions: compiled + interpreted }
                }
                None => Advance::one(cpu.step(mmu)),
            }
        }
    }

    /// ...and the full-block, linked + dispatching form — [`ChainBackend`]'s
    /// ARM7 twin.
    struct Arm7ChainBackend {
        jit: Arm7Jit,
    }

    impl Arm7ChainBackend {
        fn dispatching() -> Self {
            let mut jit = Arm7Jit::with_min_block_instrs(1);
            jit.set_link_enabled(true);
            jit.set_dispatch_enabled(true);
            Self { jit }
        }
    }

    impl crate::jit::difftest::Arm7Backend for Arm7ChainBackend {
        fn name(&self) -> &'static str {
            "arm7-jit+link"
        }
        fn step(
            &mut self,
            cpu: &mut crate::nds::cpu::Arm7Cpu,
            mmu: &mut NdsMmu,
        ) -> Advance {
            let before = self.jit.compiled_instrs;
            match self.jit.try_step(cpu, mmu, crate::jit::block::MAX_BODY_INSTRS, 64) {
                Some(cycles) => {
                    let compiled = (self.jit.compiled_instrs - before) as u32;
                    let interpreted = u32::from(self.jit.last_step_interpreted_exit());
                    Advance { cycles, instructions: compiled + interpreted }
                }
                None => Advance::one(cpu.step(mmu)),
            }
        }
    }

    /// **The ARM7 gate.** The ARMv4T instantiation over the same mixed corpus:
    /// compiled blocks must leave exactly the state `Arm7Cpu::step` leaves.
    #[test]
    fn arm7_compiled_blocks_match_the_interpreter() {
        let mut executed = 0u64;
        for seed in 1..fuzz_seeds() {
            let mut rng = Rng::new(seed);
            let program =
                crate::jit::difftest::random_program(&ArmClass::ALL, 24, &mut rng);
            let fixture = Fixture::arm(&program, seed);
            let mut jit = Arm7JitBackend { jit: Arm7Jit::with_min_block_instrs(1) };
            let result = crate::jit::difftest::compare_lockstep_arm7(
                &fixture,
                &mut crate::jit::difftest::Arm7Interpreter,
                &mut jit,
                120,
            );
            executed += jit.jit.compiled_instrs;
            result.unwrap_or_else(|d| panic!("arm7 seed {seed}: {d}"));
        }
        assert!(executed > 0, "no instruction ever ran as ARM7 compiled code");
    }

    /// ARM7 linked + dispatching chains over the corpus, breaking and
    /// re-forming on a small budget — the paths single blocks never reach.
    #[test]
    fn arm7_linked_chains_match_the_interpreter() {
        let mut linked = 0u64;
        for seed in 1..fuzz_seeds() {
            let mut rng = Rng::new(seed);
            let program =
                crate::jit::difftest::random_program(&ArmClass::ALL, 24, &mut rng);
            let fixture = Fixture::arm(&program, seed);
            let mut jit = Arm7ChainBackend::dispatching();
            let result = crate::jit::difftest::compare_lockstep_arm7(
                &fixture,
                &mut crate::jit::difftest::Arm7Interpreter,
                &mut jit,
                300,
            );
            linked += jit.jit.link_stats().0;
            result.unwrap_or_else(|d| panic!("arm7 seed {seed}: {d}"));
        }
        assert!(linked > 0, "no ARM7 successor link was ever written");
    }

    /// **The ARMv4T interworking pin.** An `LDMFD sp!,{..,pc}` return whose
    /// stacked address carries bit 0: the ARM7 masks the bit and stays in ARM
    /// state (`GbaCpu::load_pc`), so the compiled dispatch terminator must do
    /// the same — this is exactly the semantic the ARM9's interworking
    /// emitter would get wrong, pinning the `EmitCfg::armv5` fork.
    #[test]
    fn arm7_dispatched_ldm_return_masks_bit0_and_stays_arm() {
        let program = [
            0xE280_0001, // ADD r0,r0,#1           <- loop head
            0xEB00_0002, // BL +2 -> 0x14
            0xE281_1001, // ADD r1,r1,#1           <- return lands here
            0xEAFF_FFFB, // B .-12 (back to the loop head)
            0xE1A0_0000, // NOP (padding)
            0xE38E_E001, // ORR lr,lr,#1           <- sub: set bit 0 on the link
            0xE92D_4004, // STMFD sp!,{r2,lr}
            0xE282_2001, // ADD r2,r2,#1
            0xE8BD_8004, // LDMFD sp!,{r2,pc}      (v4: mask bit 0, stay ARM)
        ];
        let fixture = Fixture::arm(&program, 71);
        let mut jit = Arm7ChainBackend::dispatching();
        crate::jit::difftest::compare_lockstep_arm7(
            &fixture,
            &mut crate::jit::difftest::Arm7Interpreter,
            &mut jit,
            400,
        )
        .unwrap_or_else(|d| panic!("{d}"));
        assert!(jit.jit.dispatch_stats() > 0, "no ARM7 dispatch entry was ever written");
    }

    /// The BLX(reg) encoding does not exist on ARMv4T — the translator must
    /// decline it (the interpreter runs it as whatever the PSR space says),
    /// never compile it as a link-and-branch. Identity is the whole assertion.
    #[test]
    fn arm7_blx_reg_encoding_is_declined_not_miscompiled() {
        let program = [
            0xE280_0001, // ADD r0,r0,#1
            0xE12F_FF31, // the ARMv5 BLX r1 pattern — NOT a BLX on ARMv4T
            0xE282_2001, // ADD r2,r2,#1
            0xEAFF_FFFB, // B .-12
        ];
        let fixture = Fixture::arm(&program, 72);
        let mut jit = Arm7ChainBackend::dispatching();
        crate::jit::difftest::compare_lockstep_arm7(
            &fixture,
            &mut crate::jit::difftest::Arm7Interpreter,
            &mut jit,
            200,
        )
        .unwrap_or_else(|d| panic!("{d}"));
    }

    /// A **data** store that moves a code page's version must not force a
    /// retranslation: the words are unchanged, so the guard revalidates in
    /// place and the entry is a cache hit. This is the ARM7's dominant
    /// stale-guard case (mixing buffers share pages with driver code) —
    /// measured 28,887 retranslations per 60 ticks before this existed.
    #[test]
    fn a_data_store_on_a_code_page_revalidates_instead_of_recompiling() {
        let mut mmu = NdsMmu::new();
        // ADD r0,r0,#1 ; ADD r0,r0,#2 ; B .-16 (a self-loop the scanner ends on)
        for (i, w) in [0xE280_0001u32, 0xE280_0002, 0xEAFF_FFFC].iter().enumerate() {
            mmu.write_word_arm9(0x0200_0000 + (i as u32) * 4, *w);
        }
        let mut cpu = Arm9Cpu::new();
        cpu.set_jit_enabled(false); // drive a standalone instance instead
        cpu.cpu.registers.cpsr = 0x1F;
        cpu.cpu.registers.gpr[15] = 0x0200_0000;
        cpu.flush_pipeline(&mut mmu);

        let mut jit = Arm9Jit::with_min_block_instrs(1);
        let enter = |jit: &mut Arm9Jit, cpu: &mut Arm9Cpu, mmu: &mut NdsMmu| {
            cpu.cpu.registers.gpr[15] = 0x0200_0000;
            cpu.cpu.pc_modified = false;
            cpu.flush_pipeline(mmu);
            jit.try_step(cpu, mmu, crate::jit::block::MAX_BODY_INSTRS, 8)
                .expect("the block runs");
        };
        enter(&mut jit, &mut cpu, &mut mmu);
        assert_eq!(jit.stats().compilations, 1, "compiled once");

        // A store elsewhere on the SAME page: version moves, words do not.
        mmu.write_word_arm9(0x0200_0F00, 0xDEAD_BEEF);
        enter(&mut jit, &mut cpu, &mut mmu);
        assert_eq!(jit.stats().compilations, 1, "revalidated, not retranslated");
        assert_eq!(jit.stats().cache_hits, 1, "the revalidated entry is a hit");
    }

    /// ...and a block whose words genuinely keep changing is quarantined: a
    /// bounded number of retranslations, then a permanent decline the
    /// link-unstick cannot clear. Interpreting a self-rewriting hot spot
    /// forever is cheaper than recompiling it forever.
    #[test]
    fn a_repeatedly_rewritten_block_is_quarantined() {
        let mut mmu = NdsMmu::new();
        for (i, w) in [0xE280_0001u32, 0xE280_0002, 0xEAFF_FFFC].iter().enumerate() {
            mmu.write_word_arm9(0x0200_0000 + (i as u32) * 4, *w);
        }
        let mut cpu = Arm9Cpu::new();
        cpu.set_jit_enabled(false);
        cpu.cpu.registers.cpsr = 0x1F;
        let mut jit = Arm9Jit::with_min_block_instrs(1);

        let mut ran_compiled = 0u32;
        for round in 0..12u32 {
            // Rewrite the second instruction each round: ADD r0,r0,#(2+round).
            mmu.write_word_arm9(0x0200_0004, 0xE280_0002 + (round & 0xF));
            cpu.cpu.registers.gpr[15] = 0x0200_0000;
            cpu.cpu.pc_modified = false;
            cpu.flush_pipeline(&mut mmu);
            if jit.try_step(&mut cpu, &mut mmu, crate::jit::block::MAX_BODY_INSTRS, 8).is_some() {
                ran_compiled += 1;
            } else {
                cpu.step(&mut mmu);
            }
        }
        assert!(
            jit.is_known_decline(0x0200_0000),
            "the rewritten address ends up hard-declined"
        );
        assert!(
            jit.stats().compilations <= 6,
            "retranslations are bounded by the quarantine cap (saw {})",
            jit.stats().compilations
        );
        assert!(ran_compiled >= 1, "the block did run as compiled code first");
    }

    /// The ARM7 page-tracking contract: private WRAM is a tracked code
    /// region, stores through **both** windows onto that storage (0x038xxxxx
    /// always; 0x030xxxxx under WRAMCNT mode 0) move its version, main RAM
    /// shares the ARM9's page indices, and shared WRAM proper stays
    /// untracked. A window this misses is a stale ARM7 block that never
    /// invalidates.
    #[test]
    fn arm7_wram_stores_move_their_code_page_version() {
        let mut mmu = NdsMmu::new();
        mmu.code_watch = true;

        let page = mmu.code_page_arm7(0x0380_1000).expect("arm7 wram is tracked");
        let v0 = mmu.code_version(page);
        mmu.write_byte_arm7(0x0380_1000, 0xAA);
        assert!(mmu.code_version(page) > v0, "a 0x038 store must bump the version");

        // WRAMCNT mode 0 aliases the ARM7's shared window onto the SAME
        // storage; a store through the alias must bump the same page.
        mmu.wram_control = 0;
        let v1 = mmu.code_version(page);
        mmu.write_byte_arm7(0x0300_1000, 0xBB);
        assert!(mmu.code_version(page) > v1, "a mode-0 alias store must bump it too");

        // Main RAM maps to the ARM9's own indices, so a cross-core store
        // invalidates the other core's blocks with no extra bookkeeping.
        assert_eq!(
            mmu.code_page_arm7(0x0200_4000),
            mmu.code_page_arm9(0x0200_4000),
            "main RAM pages are shared between the cores"
        );

        // Mode 3 (the boot HLE's pin): the window maps shared-WRAM storage,
        // which is tracked — SoulSilver's whole ARM7 static executes there —
        // and a store through the window bumps the same storage page the
        // mapping reports.
        mmu.wram_control = 3;
        let page3 = mmu
            .code_page_arm7(0x037F_8000)
            .expect("the mode-3 window is tracked shared storage");
        assert_ne!(page3, page, "mode 3 maps shared storage, not arm7 wram");
        let v3 = mmu.code_version(page3);
        mmu.write_byte_arm7(0x037F_8000, 0xCC);
        assert!(mmu.code_version(page3) > v3, "a mode-3 window store must bump it");
    }

    /// A WRAMCNT write remaps what every shared-window address means, so it
    /// must invalidate wholesale — same contract as a TCM remap.
    #[test]
    fn a_wramcnt_change_invalidates_all_code() {
        let mut mmu = NdsMmu::new();
        mmu.code_watch = true;
        mmu.wram_control = 3;
        let page = mmu.code_page_arm7(0x037F_8000).expect("tracked in mode 3");
        let v0 = mmu.code_version(page);
        let e0 = mmu.code_write_epoch7;
        mmu.write_byte_arm9(0x0400_0247, 0); // WRAMCNT: mode 3 -> 0
        assert!(mmu.code_version(page) > v0, "every page version moved");
        assert!(mmu.code_write_epoch7 > e0, "the ARM7 link epoch moved");
        assert_eq!(mmu.wram_control, 0, "the write itself still lands");
    }

    /// **The Thumb dispatch gate.** A `BL` -> `PUSH {lr}` .. `POP {pc}` call
    /// loop: the return pops an ODD interworking value, so the chain re-enters
    /// through the odd-tagged half of the dispatch table.
    #[test]
    fn a_dispatched_thumb_pop_return_matches_the_interpreter() {
        let program = [
            0xF000_3001, // 0x00 ADDS r0,#1 ; 0x02 BL(hi)
            0x3101_F803, // 0x04 BL(lo) -> 0x0C ; 0x06 ADDS r1,#1 (return lands)
            0x46C0_E7FA, // 0x08 B -> 0x00 ; 0x0A NOP
            0x3201_B500, // 0x0C PUSH {lr} ; 0x0E ADDS r2,#1
            0x46C0_BD00, // 0x10 POP {pc} ; 0x12 NOP
        ];
        let fixture = Fixture::thumb(&program, 51);
        let mut jit = ChainBackend::dispatching();
        jit.jit.set_thumb_enabled(true);
        compare_lockstep(&fixture, &mut Interpreter, &mut jit, 400)
            .unwrap_or_else(|d| panic!("{d}"));
        assert!(jit.jit.dispatch_stats() > 0, "no dispatch entry was ever written");
    }

    /// ...and the `BX lr` return variant of the same shape.
    #[test]
    fn a_dispatched_thumb_bx_return_matches_the_interpreter() {
        let program = [
            0xF000_3001, // 0x00 ADDS r0,#1 ; 0x02 BL(hi)
            0xE7FB_F801, // 0x04 BL(lo) -> 0x08 ; 0x06 B -> 0x00
            0x4770_3201, // 0x08 ADDS r2,#1 ; 0x0A BX lr
        ];
        let fixture = Fixture::thumb(&program, 52);
        let mut jit = ChainBackend::dispatching();
        jit.jit.set_thumb_enabled(true);
        compare_lockstep(&fixture, &mut Interpreter, &mut jit, 400)
            .unwrap_or_else(|d| panic!("{d}"));
        assert!(jit.jit.dispatch_stats() > 0, "no dispatch entry was ever written");
    }

    /// The census-named ARM9 tail, exactly as refused: the UMULL/MLA cluster
    /// (`0x020f294c`), a signed long multiply with S, and register-offset
    /// LDR/STR — in a linked, dispatching loop so the encodings run compiled.
    #[test]
    fn census_named_multiplies_and_reg_offset_match_the_interpreter() {
        let program = [
            0xE3A0_1003, // MOV r1,#3
            0xE3A0_2007, // MOV r2,#7
            0xE086_5192, // UMULL r5,r6,r2,r1
            0xE037_5192, // MLAS r7,r2,r1,r5
            0xE0D9_8192, // SMULLS r8,r9,r2,r1
            0xE794_7101, // LDR r7,[r4,r1,LSL #2]
            0xE784_7101, // STR r7,[r4,r1,LSL #2]
            0xEAFF_FFF7, // B back to the loop head
        ];
        let fixture = Fixture::arm(&program, 91);
        let mut jit = ChainBackend::dispatching();
        compare_lockstep(&fixture, &mut Interpreter, &mut jit, 400)
            .unwrap_or_else(|d| panic!("{d}"));
        assert!(jit.jit.stats().compiled_instrs > 0, "nothing ran as compiled code");
    }

    /// The rest of the census-named tail: the CP15 cache-maintenance loop
    /// (`0x020d28a0`, an emulator no-op ×68k refused entries), `MRS SPSR`
    /// (the ITCM IRQ trampoline ×36k), and `CLZ` with both the zero and
    /// non-zero paths (the BIOS vector loop ×20k).
    #[test]
    fn census_named_cp15_spsr_and_clz_match_the_interpreter() {
        let cache_loop = [
            0xE3A0_0000, // MOV r0,#0
            0xE3A0_1080, // MOV r1,#0x80
            0xEE07_CF9A, // MCR p15,0,r12,c7,c10,4 (drain — a no-op here)
            0xEE07_0F3E, // MCR p15,0,r0,c7,c14,1 (clean+invalidate — no-op)
            0xE280_0020, // ADD r0,r0,#0x20
            0xE150_0001, // CMP r0,r1
            0xBAFF_FFFA, // BLT the first MCR
            0xEAFF_FFF8, // B back to the start
        ];
        let spsr_loop = [
            0xE14F_2000, // MRS r2,SPSR
            0xE282_2001, // ADD r2,r2,#1
            0xEAFF_FFFC, // B .-
        ];
        let clz_loop = [
            0xE3A0_1C01, // MOV r1,#0x100
            0xE16F_2F11, // CLZ r2,r1  (= 23)
            0xE3A0_3000, // MOV r3,#0
            0xE16F_4F13, // CLZ r4,r3  (= 32, the bsr-undefined case)
            0xEAFF_FFFA, // B back to the first instruction
        ];
        for (seed, program) in
            [(93u64, &cache_loop[..]), (94, &spsr_loop[..]), (95, &clz_loop[..])]
        {
            let fixture = Fixture::arm(program, seed);
            let mut jit = ChainBackend::dispatching();
            compare_lockstep(&fixture, &mut Interpreter, &mut jit, 300)
                .unwrap_or_else(|d| panic!("seed {seed}: {d}"));
            assert!(jit.jit.stats().compiled_instrs > 0, "seed {seed}: nothing compiled");
        }
    }

    /// **The PSR gate.** MRS/MSR over their own fuzz corpus, mode swaps
    /// included — the banked registers ride in the snapshot comparison.
    #[test]
    fn compiled_psr_transfers_match_the_interpreter() {
        let mut executed = 0u64;
        for seed in 1..300 {
            let mut rng = Rng::new(seed);
            let program = crate::jit::difftest::random_program(&[ArmClass::Psr, ArmClass::DataProcImm], 24, &mut rng);
            let fixture = Fixture::arm(&program, seed);
            let mut jit = JitBackend { jit: Arm9Jit::with_min_block_instrs(1) };
            let result = compare_lockstep(&fixture, &mut Interpreter, &mut jit, 120);
            executed += jit.jit.compiled_instrs;
            result.unwrap_or_else(|d| panic!("psr seed {seed}: {d}"));
        }
        assert!(executed > 0, "no PSR transfer ever ran as compiled code");
    }

    /// The interrupt-critical-section idiom the refused-successor census
    /// named: save CPSR, mask IRQs, work, restore — and a variant that
    /// genuinely changes mode (System -> IRQ -> System), exercising the
    /// thunk's bank swap inside a linked, dispatching chain.
    #[test]
    fn the_census_named_psr_idioms_match_the_interpreter() {
        let critical_section = [
            0xE10F_0000, // MRS r0,cpsr            <- loop head
            0xE380_10C0, // ORR r1,r0,#0xC0        (set I+F)
            0xE121_F001, // MSR cpsr_c,r1          (block ends, compiled)
            0xE282_2001, // ADD r2,r2,#1
            0xE121_F000, // MSR cpsr_c,r0          (restore)
            0xEAFF_FFF9, // B .-20 (back to the MRS)
        ];
        let fixture = Fixture::arm(&critical_section, 41);
        let mut jit = ChainBackend::dispatching();
        compare_lockstep(&fixture, &mut Interpreter, &mut jit, 300)
            .unwrap_or_else(|d| panic!("critical section: {d}"));
        assert!(jit.jit.stats().compiled_instrs > 0, "nothing compiled");

        let mode_swap = [
            0xE3A0_00D2, // MOV r0,#0xD2           <- loop head (IRQ mode, I+F)
            0xE121_F000, // MSR cpsr_c,r0          (swap to IRQ bank)
            0xE283_3001, // ADD r3,r3,#1
            0xE3A0_00DF, // MOV r0,#0xDF           (System, I+F)
            0xE121_F000, // MSR cpsr_c,r0          (swap back)
            0xEAFF_FFF9, // B .-20
        ];
        let fixture = Fixture::arm(&mode_swap, 42);
        let mut jit = ChainBackend::dispatching();
        compare_lockstep(&fixture, &mut Interpreter, &mut jit, 300)
            .unwrap_or_else(|d| panic!("mode swap: {d}"));
        assert!(jit.jit.stats().compiled_instrs > 0, "nothing compiled");
    }

    /// **The halfword gate.** LDRH/LDRSB/LDRSH/STRH over their own fuzz
    /// corpus — the encodings that headed the refused-successor census.
    #[test]
    fn compiled_halfword_transfers_match_the_interpreter() {
        let mut executed = 0u64;
        for seed in 1..300 {
            let mut rng = Rng::new(seed);
            let program =
                crate::jit::difftest::random_program(&[ArmClass::Halfword], 24, &mut rng);
            let fixture = Fixture::arm(&program, seed);
            let mut jit = JitBackend { jit: Arm9Jit::with_min_block_instrs(1) };
            let result = compare_lockstep(&fixture, &mut Interpreter, &mut jit, 120);
            executed += jit.jit.compiled_instrs;
            result.unwrap_or_else(|d| panic!("halfword seed {seed}: {d}"));
        }
        assert!(executed > 0, "no halfword transfer ever ran as compiled code");
    }

    /// The exact encodings the refused-successor census named, register
    /// offsets included (`LDRH r3,[r0,r12]` was the single hottest refused
    /// target). `r12` is pointed at a small offset first so the register form
    /// stays inside the scratch window.
    #[test]
    fn the_census_named_halfword_encodings_match_the_interpreter() {
        let program = [
            0xE3A0_C004, // MOV r12,#4
            0xE190_30BC, // LDRH r3,[r0,r12]      (register offset)
            0xE181_30BC, // STRH r3,[r1,r12]      (register offset store)
            0xE1D1_23BA, // LDRH r2,[r1,#0x3A]
            0xE1D0_21B8, // LDRH r2,[r0,#0x18]
            0xE151_C1B0, // LDRH r12,[r1,-r0]... r0 is a scratch address, so
            //             this leaves the window; replaced below.
            0xE1D0_31BA, // LDRH r3,[r0,#0x1A]
            0xE1F2_40B2, // LDRH r4,[r2,#2]!      (pre-index writeback)
            0xE0D2_50B2, // LDRH r5,[r2],#2       (post-index)
            0xE141_21B4, // STRH r2,[r1,#-0x14]
            0xE1D0_60D1, // LDRSB r6,[r0,#1]
            0xE1D0_70F2, // LDRSH r7,[r0,#2]
            0xEAFF_FFF3, // B back to the LDRH at 0x04
        ];
        let fixture = Fixture::arm(&program, 31);
        let mut jit = ChainBackend::dispatching();
        compare_lockstep(&fixture, &mut Interpreter, &mut jit, 300)
            .unwrap_or_else(|d| panic!("{d}"));
        assert!(
            jit.jit.stats().compiled_instrs > 0,
            "nothing ran as compiled code"
        );
    }

    /// The fourth run-time branch: the `LDR pc,[sp],#4` pop-return, an
    /// **interworking** load on ARMv5 (T from bit 0 of the loaded word), with
    /// the base written back before the PC.
    #[test]
    fn a_dispatched_ldr_pc_return_matches_the_interpreter() {
        let program = [
            0xE280_0001, // ADD r0,r0,#1           <- loop head
            0xEB00_0003, // BL +3 -> 0x18
            0xE281_1001, // ADD r1,r1,#1           <- return lands here
            0xEAFF_FFFB, // B .-12 (back to the loop head)
            0xE1A0_0000, // NOP (padding)
            0xE1A0_0000, // NOP
            0xE52D_E004, // STR lr,[sp,#-4]!       <- sub: push lr
            0xE282_2001, // ADD r2,r2,#1
            0xE49D_F004, // LDR pc,[sp],#4         (pop return; dispatch back)
        ];
        let fixture = Fixture::arm(&program, 25);
        let mut jit = ChainBackend::dispatching();
        compare_lockstep(&fixture, &mut Interpreter, &mut jit, 400)
            .unwrap_or_else(|d| panic!("{d}"));
        assert!(jit.jit.dispatch_stats() > 0, "no dispatch entry was ever written");
    }

    /// A `BX` to a Thumb target (bit 0 set) must set T, leave R15 verbatim,
    /// and end the chain — the dispatcher only serves ARM. The interpreter
    /// then executes the same bytes as Thumb on both sides.
    #[test]
    fn a_thumb_bx_target_ends_the_chain_and_matches_the_interpreter() {
        let program = [
            0xE3A0_3402, // MOV r3,#0x02000000
            0xE283_3041, // ADD r3,r3,#0x41        (odd: Thumb target 0x40|1)
            0xE280_0001, // ADD r0,r0,#1
            0xE12F_FF13, // BX r3
            0xE1A0_0000, // NOP padding to 0x40...
            0xE1A0_0000, 0xE1A0_0000, 0xE1A0_0000, 0xE1A0_0000, 0xE1A0_0000,
            0xE1A0_0000, 0xE1A0_0000, 0xE1A0_0000, 0xE1A0_0000, 0xE1A0_0000,
            0xE1A0_0000,
            // 0x40: executed as THUMB from here on (0x0000 halfwords of the
            // NOP words decode as Thumb MOVS r0,r0 — harmless, and identical
            // on both sides).
            0xE1A0_0000,
        ];
        let fixture = Fixture::arm(&program, 23);
        let mut jit = ChainBackend::dispatching();
        compare_lockstep(&fixture, &mut Interpreter, &mut jit, 60)
            .unwrap_or_else(|d| panic!("{d}"));
    }

    /// The full random corpus, with dispatching exits compiled in. The corpus
    /// generates no BX, so this pins "dispatch mode changes nothing it should
    /// not" across every ordinary path — including the seed-224 class of
    /// self-modifying trace.
    #[test]
    fn dispatched_chains_match_the_interpreter_on_the_corpus() {
        for seed in 1..200 {
            let mut rng = Rng::new(seed);
            let program = crate::jit::difftest::random_program(&ArmClass::ALL, 24, &mut rng);
            let fixture = Fixture::arm(&program, seed);
            let mut jit = ChainBackend::dispatching();
            compare_lockstep(&fixture, &mut Interpreter, &mut jit, 300)
                .unwrap_or_else(|d| panic!("seed {seed}: {d}"));
        }
    }

    /// A linked self-loop must stand down when the slice budget is spent —
    /// and, until then, iterate entirely inside generated code.
    ///
    /// The numbers are exact, which is the point: 4 cycles per iteration
    /// (`ADD` 1 + taken `B` 3) against a budget of 50 stops the chain at the
    /// first exit where `4k >= 50`, i.e. 13 iterations, 52 cycles, 26 retired.
    /// A wrong budget comparison or a lost iteration moves all three.
    #[test]
    fn a_linked_chain_stands_down_when_the_slice_budget_is_spent() {
        let program = [
            0xE280_0001, // ADD r0,r0,#1
            0xEAFF_FFFD, // B .-4 (back to the ADD: a self-loop)
        ];
        let fixture = Fixture::arm(&program, 7);
        let (mut cpu, mut mmu) = fixture.instantiate();
        cpu.cpu.registers.gpr[0] = 0;
        let mut jit = Arm9Jit::with_min_block_instrs(1);
        jit.set_link_enabled(true);

        // First entry: the exit is not linked yet, so exactly one iteration
        // runs — and breaking it is what writes the self-link.
        let first = jit.try_step(&mut cpu, &mut mmu, 16, 50).expect("compiled");
        assert_eq!(first, 4, "one unlinked iteration: ADD(1) + taken B(3)");
        assert_eq!(jit.link_stats().0, 1, "the broken exit wrote the self-link");

        // Second entry: the chain iterates in generated code until the budget
        // comparison at the exit stands it down.
        let before_instrs = cpu.cpu.instrs;
        let chained = jit.try_step(&mut cpu, &mut mmu, 16, 50).expect("linked chain");
        assert_eq!(chained, 52, "13 iterations x 4 cycles, first exit at >= 50");
        assert_eq!(cpu.cpu.instrs - before_instrs, 26, "13 x (ADD + B) retired");
        assert_eq!(cpu.cpu.registers.gpr[0], 1 + 13, "one unlinked + 13 chained ADDs");
    }

    /// An IPCSYNC write inside a linked chain must break it at that store's
    /// block exit: the run loop yields the bus to the ARM7 on `ipc_yield`, and
    /// a chain that sailed past the store would shift the whole interleave.
    ///
    /// Driven through `Arm9Cpu::run` — the real slice loop — against an
    /// interpreter-only twin, slice by slice: `used`, the registers and the
    /// retired count must agree at every yield.
    #[test]
    fn an_ipcsync_store_breaks_the_chain_at_its_block_exit() {
        let program = [
            0xE3A0_1301, // MOV r1,#0x04000000
            0xE281_1E18, // ADD r1,r1,#0x180      (r1 = IPCSYNC)
            0xE280_0001, // ADD r0,r0,#1          <- loop head
            0xE581_0000, // STR r0,[r1]           (sets ipc_yield; ends its block)
            0xE282_2001, // ADD r2,r2,#1
            0xEAFF_FFFB, // B .-12 (back to the loop head)
        ];
        let fixture = Fixture::arm(&program, 11);
        let (mut jc, mut jm) = fixture.instantiate();
        let (mut ic, mut im) = fixture.instantiate();
        jc.set_jit_enabled(true);
        jc.set_jit_link_enabled(true);

        for slice in 0..50 {
            let uj = jc.run(&mut jm, 200);
            let ui = ic.run(&mut im, 200);
            assert_eq!(uj, ui, "slice {slice}: cycles used diverged");
            assert_eq!(
                jc.cpu.registers.gpr, ic.cpu.registers.gpr,
                "slice {slice}: registers diverged"
            );
            assert_eq!(jc.cpu.instrs, ic.cpu.instrs, "slice {slice}: retired diverged");
        }
        let (links, _) = jc.jit_link_stats().expect("recompiler is on");
        assert!(links > 0, "the loop's branch exit never linked");
        assert!(
            jc.jit_stats().expect("on").compiled_instrs > 0,
            "nothing ran as compiled code"
        );
    }

    /// A store that lands on a page holding compiled code must move the link
    /// epoch, break the chain, and tear links down before the next one starts
    /// — under continuous self-page stores the recompiler must simply degrade
    /// to interpreter-equivalent behaviour, never follow a stale link.
    #[test]
    fn a_store_to_a_code_page_tears_links_down() {
        let program = [
            0xE3A0_1402, // MOV r1,#0x02000000
            0xE381_1B02, // ORR r1,r1,#0x800     (same 4 KiB page as the code)
            0xE581_0000, // STR r0,[r1]          <- loop head; dirties the page
            0xE280_0001, // ADD r0,r0,#1
            0xEAFF_FFFB, // B .-12 (back to the STR)
        ];
        let fixture = Fixture::arm(&program, 13);
        let mut inner = Arm9Jit::with_min_block_instrs(1);
        inner.set_link_enabled(true);
        let mut jit = ChainBackend { jit: inner };

        compare_lockstep(&fixture, &mut Interpreter, &mut jit, 250)
            .unwrap_or_else(|d| panic!("{d}"));
    }

    /// **The Thumb gate.** Compiled Thumb must leave exactly the state the
    /// interpreter leaves, over a corpus that mixes the translated formats with
    /// the ones that must be refused — `BX`, an R15 destination, the
    /// register-amount shifts and `MUL`.
    ///
    /// Thumb has no condition field, so unlike ARM every generated instruction
    /// executes; there is no condition-failed path to dilute the corpus.
    #[test]
    fn compiled_thumb_matches_the_interpreter() {
        let mut executed = 0u64;
        for seed in 1..thumb_fuzz_seeds() {
            let mut rng = Rng::new(seed);
            let program = crate::jit::difftest::random_thumb_program(24, &mut rng);
            let fixture = Fixture::thumb(&program, seed);
            let mut inner = Arm9Jit::with_min_block_instrs(1);
            inner.set_thumb_enabled(true); // the translator, not the default
            let mut jit = JitBackend { jit: inner };

            let result = compare_lockstep(&fixture, &mut Interpreter, &mut jit, 120);
            executed += jit.jit.compiled_instrs;
            result.unwrap_or_else(|d| panic!("thumb seed {seed}: {d}"));
        }
        assert!(executed > 0, "no Thumb instruction was ever executed as compiled code");
    }

    /// A hand-written block, checked value by value. The fuzz above proves
    /// equivalence in aggregate; this says what the code is supposed to *do*, so
    /// a failure names the arithmetic rather than a seed.
    #[test]
    fn each_supported_opcode_computes_the_right_value() {
        // r1 = 0x1000 throughout; each instruction writes a different register.
        let program = [
            0xE3A0_1C01, // MOV  r1,#0x100
            0xE281_0001, // ADD  r0,r1,#1     -> 0x101
            0xE241_2002, // SUB  r2,r1,#2     -> 0x0FE
            0xE261_3001, // RSB  r3,r1,#1     -> 1 - 0x100 = 0xFFFFFF01
            0xE201_40FF, // AND  r4,r1,#0xFF  -> 0
            0xE221_50FF, // EOR  r5,r1,#0xFF  -> 0x1FF
            0xE381_60FF, // ORR  r6,r1,#0xFF  -> 0x1FF
            0xE3C1_70FF, // BIC  r7,r1,#0xFF  -> 0x100
            0xE3E0_8000, // MVN  r8,#0        -> 0xFFFFFFFF
            0xE12F_FF1E, // BX   lr  (ends the block: the target is a register)
        ];
        let fixture = Fixture::arm(&program, 1);
        let (mut cpu, mut mmu) = fixture.instantiate();
        let mut jit = Arm9Jit::with_min_block_instrs(1);

        // One block covering the whole run. `BX` has a run-time target, so
        // under the default dispatching configuration it *ends* the block as a
        // compiled terminator rather than stopping the trace before it.
        let cycles = jit.try_step(&mut cpu, &mut mmu, 16, 1).expect("the block compiled");
        assert_eq!(jit.compiled_instrs, 10, "nine data-processing instructions plus the BX");
        assert_eq!(cycles, 9 + 3, "nine at one cycle, the BX at three");

        let r = &cpu.cpu.registers.gpr;
        assert_eq!(r[1], 0x100, "MOV");
        assert_eq!(r[0], 0x101, "ADD");
        assert_eq!(r[2], 0x0FE, "SUB");
        assert_eq!(r[3], 1u32.wrapping_sub(0x100), "RSB");
        assert_eq!(r[4], 0, "AND");
        assert_eq!(r[5], 0x1FF, "EOR");
        assert_eq!(r[6], 0x1FF, "ORR");
        assert_eq!(r[7], 0x100, "BIC");
        assert_eq!(r[8], 0xFFFF_FFFF, "MVN");
        // `try_step` also ran the `BX lr` the trace stopped before, and the
        // fixture seeds LR with `CODE_BASE`, so R15 holds the branch target
        // rather than the fall-through.
        assert_eq!(r[15], CODE_BASE, "the interpreted BX jumped to LR");
    }

    /// **The early-exit gate.** A conditional branch the scanner followed the
    /// fall-through of must leave exactly the state the interpreter leaves —
    /// on both the taken and the not-taken path.
    ///
    /// The existing fuzz test cannot reach this: it compiles one instruction
    /// per block, so a conditional branch is never mid-body. Nothing else in
    /// the suite puts one there either, which is why every test passed with the
    /// path unexercised.
    ///
    /// A wrong retired count or cycle total here is silent — registers still
    /// agree, and only `instrs`, `arm_class_hist` and the cycle return diverge,
    /// which is precisely the class of bug that shifts the ARM9/ARM7 interleave
    /// without changing a single pixel.
    #[test]
    fn a_followed_conditional_branch_matches_the_interpreter() {
        // 0x00 ADDS r1,r1,#1     sets Z when r1 wraps to 0
        // 0x04 BEQ  -> 0x14      taken on that pass only
        // 0x08 ADD  r2,r2,#1     fall-through, inside the same block
        // 0x0C ADD  r3,r3,#1
        // 0x10 B    -> 0x00
        // 0x14 ADD  r4,r4,#1     branch target
        // 0x18 B    -> 0x00
        let program = [
            0xE291_1001u32, // ADDS r1,r1,#1
            0x0A00_0002,    // BEQ +2 -> 0x14
            0xE282_2001,    // ADD r2,r2,#1
            0xE283_3001,    // ADD r3,r3,#1
            0xEAFF_FFFA,    // B -> 0x00
            0xE284_4001,    // ADD r4,r4,#1
            0xEAFF_FFF8,    // B -> 0x00
        ];
        let fixture = Fixture::arm(&program, 1);

        // Start one below the wrap so the branch is taken early, then again
        // every 2^32 — i.e. once here, exercising both paths.
        let prime = |cpu: &mut Arm9Cpu| {
            cpu.cpu.registers.gpr[1] = 0xFFFF_FFFE;
        };

        let (mut a, mut ma) = fixture.instantiate();
        prime(&mut a);
        let (mut b, mut mb) = fixture.instantiate();
        prime(&mut b);

        let mut jit = Arm9Jit::with_min_block_instrs(1);
        jit.set_follow_conditional(true);

        for step in 0..60 {
            let before = jit.compiled_instrs;
            let jit_cycles = match jit.try_step(&mut b, &mut mb, 16, 1) {
                Some(c) => c,
                None => b.step(&mut mb),
            };
            let retired = (jit.compiled_instrs - before).max(1);
            let ref_cycles: u32 = (0..retired).map(|_| a.step(&mut ma)).sum();

            assert_eq!(a.cpu.registers.gpr, b.cpu.registers.gpr, "registers at step {step}");
            assert_eq!(a.cpu.registers.cpsr, b.cpu.registers.cpsr, "cpsr at step {step}");
            assert_eq!(a.cpu.pc_modified, b.cpu.pc_modified, "pc_modified at step {step}");
            // Only meaningful with no refill pending. Refill servicing flushes
            // eagerly inside `try_step`, so when `pc_modified` is set the two
            // sides hold different *stale* words that both are about to
            // overwrite — comparing them tests scheduling, not semantics.
            if !a.cpu.pc_modified {
                assert_eq!(a.cpu.pipeline, b.cpu.pipeline, "pipeline at step {step}");
            }
            assert_eq!(ref_cycles, jit_cycles, "cycles at step {step}");
            assert_eq!(a.cpu.instrs, b.cpu.instrs, "retired count at step {step}");
            assert_eq!(
                a.cpu.arm_class_hist, b.cpu.arm_class_hist,
                "class histogram at step {step}"
            );
        }

        assert!(a.cpu.registers.gpr[4] > 0, "the taken path never ran");
        assert!(a.cpu.registers.gpr[2] > 0, "the fall-through path never ran");
        assert!(jit.compiled_instrs > 0, "nothing was compiled");
    }

    /// `Rn == 15` is the literal-pool idiom. R15 is a compile-time constant
    /// inside a block, so getting it wrong yields a plausible-looking pointer
    /// rather than a crash — hence a test that names the expected address.
    #[test]
    fn r15_as_a_source_reads_as_the_executing_address_plus_eight() {
        // ADD r0,pc,#4 at CODE_BASE: R15 reads CODE_BASE + 8.
        let program = [0xE28F_0004, 0xEA00_0000];
        let fixture = Fixture::arm(&program, 1);
        let (mut cpu, mut mmu) = fixture.instantiate();
        let mut jit = Arm9Jit::with_min_block_instrs(1);

        jit.try_step(&mut cpu, &mut mmu, 16, 1).expect("compiled");
        assert_eq!(cpu.cpu.registers.gpr[0], CODE_BASE + 8 + 4);
    }

    /// Every ARM condition, against every NZCV state.
    ///
    /// The fuzz corpus reaches conditions at random and can never cover all
    /// 14 x 16 combinations; `LS`, `GE`, `LT`, `GT` and `LE` are each several
    /// instructions of bit manipulation, and a wrong mask in one of them shows
    /// up only for particular flag states. This enumerates them.
    #[test]
    fn every_condition_matches_the_interpreter_for_every_flag_state() {
        // `MOV r0,#1` with the condition field cleared, so it can be re-applied.
        const MOV_R0_1: u32 = 0xE3A0_0001 & 0x0FFF_FFFF;

        for cond in 0..14u32 {
            for nzcv in 0..16u32 {
                let program = [(cond << 28) | MOV_R0_1, 0xEA00_0000];
                let fixture = Fixture::arm(&program, 1);
                let cpsr = 0x1F | (nzcv << 28);

                let (mut a, mut ma) = fixture.instantiate();
                a.cpu.registers.cpsr = cpsr;
                a.cpu.registers.gpr[0] = 0;
                let ref_cycles = a.step(&mut ma);

                let (mut b, mut mb) = fixture.instantiate();
                b.cpu.registers.cpsr = cpsr;
                b.cpu.registers.gpr[0] = 0;
                let mut jit = Arm9Jit::with_min_block_instrs(1);
                let jit_cycles = jit.try_step(&mut b, &mut mb, 1, 1).expect("compiled");

                let what = format!("cond {cond:#x}, nzcv {nzcv:#06b}");
                assert_eq!(a.cpu.registers.gpr, b.cpu.registers.gpr, "registers: {what}");
                assert_eq!(a.cpu.registers.cpsr, b.cpu.registers.cpsr, "cpsr: {what}");
                assert_eq!(ref_cycles, jit_cycles, "cycles: {what}");
                assert_eq!(
                    a.cpu.arm_class_hist, b.cpu.arm_class_hist,
                    "the instruction mix histogram only counts a passed condition: {what}"
                );
                assert_eq!(a.cpu.instrs, b.cpu.instrs, "every instruction is retired: {what}");
            }
        }
    }

    /// Flag *production* for every flag-setting opcode, over operand pairs
    /// chosen to straddle the carry and overflow boundaries.
    ///
    /// ARM's carry for a subtraction is the inverse of x86's and a logical
    /// operation preserves V where x86 clears OF. Both asymmetries are invisible
    /// on most operands, so the values here are the ones that expose them.
    #[test]
    fn flag_producing_opcodes_match_the_interpreter() {
        // (mnemonic, instruction with Rn = r1, Rd = r0, immediate = 1)
        let opcodes: [(&str, u32); 12] = [
            ("ANDS", 0xE211_0001),
            ("EORS", 0xE231_0001),
            ("SUBS", 0xE251_0001),
            ("RSBS", 0xE271_0001),
            ("ADDS", 0xE291_0001),
            ("ADCS", 0xE2B1_0001),
            ("SBCS", 0xE2D1_0001),
            ("RSCS", 0xE2F1_0001),
            ("TST", 0xE311_0001),
            ("TEQ", 0xE331_0001),
            ("CMP", 0xE351_0001),
            ("CMN", 0xE371_0001),
        ];
        // 0 and 1 straddle the borrow boundary; 0x7FFFFFFF and 0x80000000 the
        // signed-overflow one; 0xFFFFFFFF the unsigned carry-out one.
        let operands = [0u32, 1, 2, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFF];

        for (name, inst) in opcodes {
            for rn in operands {
                for carry_in in [false, true] {
                    let program = [inst, 0xEA00_0000];
                    let fixture = Fixture::arm(&program, 1);
                    // Start with V set, so "a logical op preserves V" is
                    // falsifiable rather than trivially satisfied.
                    let cpsr = 0x1F | (1 << 28) | if carry_in { 1 << 29 } else { 0 };

                    let (mut a, mut ma) = fixture.instantiate();
                    a.cpu.registers.cpsr = cpsr;
                    a.cpu.registers.gpr[1] = rn;
                    a.step(&mut ma);

                    let (mut b, mut mb) = fixture.instantiate();
                    b.cpu.registers.cpsr = cpsr;
                    b.cpu.registers.gpr[1] = rn;
                    Arm9Jit::with_min_block_instrs(1).try_step(&mut b, &mut mb, 1, 1).expect("compiled");

                    let what = format!("{name} r1={rn:#010x} C={carry_in}");
                    assert_eq!(
                        a.cpu.registers.gpr[0], b.cpu.registers.gpr[0],
                        "result: {what}"
                    );
                    assert_eq!(
                        a.cpu.registers.cpsr >> 28,
                        b.cpu.registers.cpsr >> 28,
                        "NZCV: {what} (interpreter {:#06b} vs jit {:#06b})",
                        a.cpu.registers.cpsr >> 28,
                        b.cpu.registers.cpsr >> 28
                    );
                }
            }
        }
    }

    /// The shifter carry-out of an immediate operand: a rotation of zero leaves
    /// C alone, any other rotation sets it from bit 31 of the rotated value.
    /// Treating "no rotation" as "carry zero" would clobber C on every
    /// `MOVS rd,#imm`, which is the sort of thing that derails a game hours in.
    #[test]
    fn the_immediate_shifter_carry_matches_the_interpreter() {
        // MOVS r0,#0xFF (rot 0), MOVS r0,#0x02000000 (rot 8), MOVS r0,#0x80000000.
        for inst in [0xE3B0_00FFu32, 0xE3B0_0402, 0xE3B0_0102, 0xE3B0_0201] {
            for carry_in in [false, true] {
                let program = [inst, 0xEA00_0000];
                let fixture = Fixture::arm(&program, 1);
                let cpsr = 0x1F | if carry_in { 1 << 29 } else { 0 };

                let (mut a, mut ma) = fixture.instantiate();
                a.cpu.registers.cpsr = cpsr;
                a.step(&mut ma);

                let (mut b, mut mb) = fixture.instantiate();
                b.cpu.registers.cpsr = cpsr;
                Arm9Jit::with_min_block_instrs(1).try_step(&mut b, &mut mb, 1, 1).expect("compiled");

                assert_eq!(
                    a.cpu.registers.cpsr, b.cpu.registers.cpsr,
                    "{inst:#010x} with C={carry_in}"
                );
                assert_eq!(a.cpu.registers.gpr[0], b.cpu.registers.gpr[0], "{inst:#010x}");
            }
        }
    }

    /// **Self-modifying code must produce what the interpreter produces.**
    ///
    /// This is the failure mode the whole invalidation design exists for, and it
    /// is silent: a stale block computes plausible wrong values rather than
    /// crashing. The program overwrites a later instruction and then branches to
    /// it, twice, with a different replacement each time — so a cache that
    /// returned the first translation would visibly disagree.
    #[test]
    fn self_modifying_code_matches_the_interpreter() {
        // 0x00 STR  r3,[r1]      overwrite the instruction at 0x14
        // 0x04 ADD  r3,r3,#1     next pass installs a different one
        // 0x08 ADD  r5,r5,#1     pass counter
        // 0x0C B    +1           -> 0x14, skipping 0x10
        // 0x10 MOV  r4,#0x99     never executed
        // 0x14 <overwritten>     starts as MOV r4,#0
        // 0x18 B    -8           -> 0x00
        let program = [
            0xE581_3000u32, // STR r3,[r1]
            0xE283_3001,    // ADD r3,r3,#1
            0xE285_5001,    // ADD r5,r5,#1
            0xEA00_0000,    // B +0 -> 0x14
            0xE3A0_4099,    // MOV r4,#0x99
            0xE3A0_4000,    // MOV r4,#0 (the target, rewritten each pass)
            0xEAFF_FFF8,    // B -> 0x00
        ];
        let fixture = Fixture::arm(&program, 1);

        let prime = |cpu: &mut Arm9Cpu| {
            cpu.cpu.registers.gpr[1] = CODE_BASE + 0x14; // the instruction to rewrite
            cpu.cpu.registers.gpr[3] = 0xE3A0_4001; // MOV r4,#1 first, then #2, ...
            cpu.cpu.registers.gpr[5] = 0;
        };

        let (mut a, mut ma) = fixture.instantiate();
        prime(&mut a);
        let (mut b, mut mb) = fixture.instantiate();
        prime(&mut b);
        let mut jit = Arm9Jit::with_min_block_instrs(1);

        // Enough passes through the loop that a stale block would show. The
        // recompiler runs whole blocks, so the interpreter is advanced by the
        // number of instructions the block actually retired — comparing one
        // interpreter step against a multi-instruction block would only prove
        // that they are out of step.
        for step in 0..40 {
            let before = jit.compiled_instrs;
            let jit_cycles = match jit.try_step(&mut b, &mut mb, 16, 1) {
                Some(c) => c,
                None => b.step(&mut mb),
            };
            let retired = (jit.compiled_instrs - before).max(1);
            let ref_cycles: u32 = (0..retired).map(|_| a.step(&mut ma)).sum();
            assert_eq!(
                a.cpu.registers.gpr, b.cpu.registers.gpr,
                "registers diverged at step {step} (r4 is the rewritten instruction's effect)"
            );
            assert_eq!(ref_cycles, jit_cycles, "cycles diverged at step {step}");
            assert_eq!(
                ma.read_word_arm9(CODE_BASE + 0x14),
                mb.read_word_arm9(CODE_BASE + 0x14),
                "the rewritten instruction differs at step {step}"
            );
        }
        assert!(a.cpu.registers.gpr[5] >= 2, "the loop ran at least twice");
        assert!(a.cpu.registers.gpr[4] >= 1, "the rewritten instruction executed");
        assert!(jit.compiled_instrs > 0, "the compiled path was used");
    }

    /// A block is translated once and served from the cache afterwards.
    #[test]
    fn a_repeated_block_is_translated_only_once() {
        // MOV r0,#1 ; ADD r0,r0,#1 ; B back to the start.
        let program = [0xE3A0_0001u32, 0xE280_0001, 0xEAFF_FFFC];
        let fixture = Fixture::arm(&program, 1);
        let (mut cpu, mut mmu) = fixture.instantiate();
        let mut jit = Arm9Jit::with_min_block_instrs(1);

        for _ in 0..40 {
            if jit.try_step(&mut cpu, &mut mmu, 16, 1).is_none() {
                cpu.step(&mut mmu);
            }
        }
        // **One** entry point, not two. The second existed only because the
        // interpreter used to run the branch target's first instruction while
        // servicing the pipeline refill, starting the next block one
        // instruction later; with refill servicing on by default the
        // recompiler handles the refill itself and the loop re-enters at its
        // top every pass. Translated once either way — which is what this test
        // is about.
        assert_eq!(jit.compilations, 1, "a block was translated more than once");
        assert!(jit.cache_hits > jit.compilations, "only {} hits", jit.cache_hits);
    }

    /// **A savestate restore must discard every compiled block.**
    ///
    /// This is the one invalidation path that page versions cannot see: a
    /// restore replaces the whole memory image without a single store passing
    /// through the MMU, so no version moves and every cached block survives
    /// code it no longer matches. `Snap for Arm9Cpu` clears the cache when the
    /// visitor is loading, and nothing else in the suite exercised that —
    /// `set_jit_enabled` appears only in probes, so every savestate test ran
    /// with the recompiler off.
    ///
    /// Failure here is silent: the restored game runs stale machine code
    /// against unrelated memory and computes plausible wrong values.
    #[test]
    fn a_savestate_restore_discards_compiled_blocks() {
        use crate::snapshot::{Reader, Snap, Writer};

        let program = [0xE3A0_0001u32, 0xE280_0001, 0xEAFF_FFFC];
        let fixture = Fixture::arm(&program, 1);
        let (mut cpu, mut mmu) = fixture.instantiate();
        cpu.set_jit_enabled(true);

        // Compile something, so there is state a restore could wrongly keep.
        cpu.run(&mut mmu, 64);
        let warm = cpu.jit_stats().expect("the recompiler is enabled");
        assert!(warm.compilations > 0, "nothing was compiled, so nothing is at risk");

        // Running again must serve from the cache — otherwise the assertion
        // below could pass without the restore having done anything.
        cpu.run(&mut mmu, 64);
        let cached = cpu.jit_stats().expect("still enabled");
        assert_eq!(
            cached.compilations, warm.compilations,
            "the cache is not being reused, so this test cannot detect a stale one"
        );

        let mut w = Writer::default();
        cpu.snap(&mut w);
        let bytes = w.out;
        let mut r = Reader::new(&bytes);
        cpu.snap(&mut r);
        r.finish().expect("round trip");

        // The counters are cumulative and `clear` deliberately leaves them
        // alone, so the observable is that the *next* run has to translate
        // again. A surviving cache would serve the same blocks and compile
        // nothing.
        cpu.run(&mut mmu, 64);
        let after = cpu.jit_stats().expect("still enabled");
        assert!(
            after.compilations > cached.compilations,
            "the block cache survived a restore ({} compilations before and \
             after): stale code would run against the restored memory image",
            cached.compilations
        );
    }

    /// A store to the block's own page invalidates it; a store elsewhere does
    /// not. Over-invalidating is merely slow, but under-invalidating is silent
    /// corruption, so both directions are pinned.
    #[test]
    fn invalidation_is_page_granular() {
        let program = [0xE3A0_0001u32, 0xE280_0001, 0xEAFF_FFFC];
        let fixture = Fixture::arm(&program, 1);
        let (mut cpu, mut mmu) = fixture.instantiate();
        let mut jit = Arm9Jit::with_min_block_instrs(1);

        jit.try_step(&mut cpu, &mut mmu, 16, 1).expect("first block");
        assert_eq!(jit.compilations, 1);

        let page = mmu.code_page_arm9(CODE_BASE).expect("main RAM is tracked");
        let before = mmu.code_version(page);

        // A store two pages away must not move this page's version.
        mmu.write_word_arm9(CODE_BASE + 0x2000, 0);
        assert_eq!(mmu.code_version(page), before, "an unrelated page moved this one");

        // A store inside the page must.
        mmu.write_word_arm9(CODE_BASE + 0x40, 0);
        assert_ne!(mmu.code_version(page), before, "a store to the page did not register");
    }

    /// **DMA into a code page must invalidate it.**
    ///
    /// The brief singles this out: DS games DMA and decompress code into RAM,
    /// and a stale block is silent corruption rather than a crash. Today DMA
    /// reaches memory through `write_word_arm9`, which decomposes to the hooked
    /// byte writer, so it is covered *by construction* — but nothing pinned
    /// that, and a future block-copy fast path for DMA would bypass the hook
    /// with every existing test still green.
    #[test]
    fn dma_into_a_code_page_invalidates_it() {
        let mut mmu = NdsMmu::new();
        mmu.code_watch = true;
        let page = mmu.code_page_arm9(0x0200_1000).expect("main RAM is tracked");
        let before = mmu.code_version(page);

        // DMA3, immediate, four words from 0x02000000 into 0x02001000 — the
        // same shape as `test_arm9_immediate_dma_copies_and_clears_enable`.
        for i in 0..4u32 {
            mmu.write_word_arm9(0x0200_0000 + i * 4, 0xE1A0_0000); // NOP
        }
        let source_page = mmu.code_page_arm9(0x0200_0000).expect("tracked");
        let after_seed = mmu.code_version(source_page);

        mmu.write_word_arm9(0x0400_00D4, 0x0200_0000); // SAD
        mmu.write_word_arm9(0x0400_00D8, 0x0200_1000); // DAD
        mmu.write_word_arm9(0x0400_00DC, 0x8400_0004); // enable + 32-bit + 4 words

        assert_eq!(mmu.read_word_arm9(0x0200_1000), 0xE1A0_0000, "the DMA ran");
        assert_ne!(
            mmu.code_version(page),
            before,
            "DMA wrote a tracked code page without moving its version: a block \
             compiled from it would keep running after the code changed"
        );
        // The I/O writes that programmed the DMA must not have disturbed the
        // source page, or the assertion above could pass for the wrong reason.
        assert_eq!(mmu.code_version(source_page), after_seed, "I/O writes moved a code page");
    }

    /// Reconfiguring a TCM changes what an address *means* without moving a
    /// byte, so every cached block keyed on one has to go.
    #[test]
    fn a_tcm_remap_invalidates_everything() {
        let mut mmu = NdsMmu::new();
        mmu.code_watch = true;
        let before: Vec<u32> = mmu.code_versions.clone();

        mmu.set_cp15(crate::nds::cpu::Cp15Registers {
            control: 1 << 18,
            itcm_control: 0x0000_0020,
            dtcm_control: 0,
        });

        assert!(
            mmu.code_versions.iter().zip(&before).all(|(now, was)| now != was),
            "set_cp15 must move every page version"
        );
    }

    /// DTCM is the *data* TCM and is never executed, so it is deliberately not
    /// tracked — tracking it would cost every data store for nothing.
    #[test]
    fn only_executable_memory_is_tracked() {
        let mmu = NdsMmu::new();
        assert!(mmu.code_page_arm9(CODE_BASE).is_some(), "main RAM");
        assert!(mmu.code_page_arm9(0x0400_0000).is_none(), "I/O");
        assert!(mmu.code_page_arm9(0x0800_0000).is_none(), "cartridge");
        // The shared-WRAM window's mapping depends on WRAMCNT, so it is not a
        // stable page identity and is excluded.
        assert!(mmu.code_page_arm9(0x0240_0000).is_none(), "shared WRAM window");
    }

    /// The recompiler must stand down in every state where `Arm9Cpu::step` does
    /// something a block does not model.
    #[test]
    fn the_recompiler_declines_states_it_does_not_model() {
        let program = [0xE281_0001, 0xEA00_0000];
        let fixture = Fixture::arm(&program, 1);

        let halted = |cpu: &mut Arm9Cpu, _: &mut NdsMmu| cpu.cpu.halted = true;
        let refill = |cpu: &mut Arm9Cpu, _: &mut NdsMmu| cpu.cpu.pc_modified = true;
        let watch = |_: &mut Arm9Cpu, mmu: &mut NdsMmu| mmu.tp_read_watch_on = true;
        let irq = |_: &mut Arm9Cpu, mmu: &mut NdsMmu| {
            mmu.arm9_ime = 1;
            mmu.arm9_ie = 1;
            mmu.arm9_if = 1;
        };
        // Thumb is deliberately absent: it is translated now, not declined.
        let cases: [(&str, &dyn Fn(&mut Arm9Cpu, &mut NdsMmu)); 4] = [
            ("halted", &halted),
            ("pending pipeline refill", &refill),
            ("touch read watch armed", &watch),
            ("pending interrupt", &irq),
        ];

        for (what, arm) in cases {
            let (mut cpu, mut mmu) = fixture.instantiate();
            arm(&mut cpu, &mut mmu);
            let mut jit = Arm9Jit::with_min_block_instrs(1);
            jit.set_refill_servicing(false); // the policy, not the environment
            assert!(jit.try_step(&mut cpu, &mut mmu, 16, 1).is_none(), "must decline: {what}");
            assert_eq!(jit.declined, 1);
        }
    }

    /// With refill servicing on, a pending refill is **handled**, not declined —
    /// and the block that results must still match the interpreter exactly.
    ///
    /// The refill is where the recompiler and the interpreter are easiest to
    /// desynchronise: `flush_pipeline` moves R15 as well as the pipeline, so a
    /// block compiled from the pre-flush R15 is correct code at the wrong
    /// address, which no register comparison at a single instruction can see.
    #[test]
    fn servicing_a_refill_matches_the_interpreter() {
        // MOV r0,#1 ; ADD r0,r0,#1 ; ADD r0,r0,#1 ; B back to the start.
        // The backward branch sets `pc_modified` on every pass, so every
        // iteration after the first enters through the refill path.
        let program = [0xE3A0_0001u32, 0xE280_0001, 0xE280_0001, 0xEAFF_FFFB];
        let fixture = Fixture::arm(&program, 1);

        let (mut a, mut ma) = fixture.instantiate();
        let (mut b, mut mb) = fixture.instantiate();
        let mut jit = Arm9Jit::with_min_block_instrs(1);
        jit.set_refill_servicing(true);

        let mut serviced = 0u32;
        for step in 0..60 {
            let refilling = b.cpu.pc_modified;
            let before = jit.compiled_instrs;
            let jit_cycles = match jit.try_step(&mut b, &mut mb, 16, 1) {
                Some(c) => {
                    if refilling {
                        serviced += 1;
                    }
                    c
                }
                None => b.step(&mut mb),
            };
            let retired = (jit.compiled_instrs - before).max(1);
            let ref_cycles: u32 = (0..retired).map(|_| a.step(&mut ma)).sum();

            assert_eq!(
                a.cpu.registers.gpr, b.cpu.registers.gpr,
                "registers diverged at step {step}"
            );
            assert_eq!(a.cpu.registers.cpsr, b.cpu.registers.cpsr, "cpsr at step {step}");
            assert_eq!(a.cpu.pipeline, b.cpu.pipeline, "pipeline at step {step}");
            assert_eq!(a.cpu.pc_modified, b.cpu.pc_modified, "pc_modified at step {step}");
            assert_eq!(ref_cycles, jit_cycles, "cycles diverged at step {step}");
        }
        assert!(serviced > 0, "no refill was ever serviced by the recompiler");
    }

    /// `refusal_detail` duplicates `may_run`'s conditions so the hot path can
    /// stay a single bit. That duplication is only safe while the two agree —
    /// on the answer *and* on which condition wins when several hold.
    #[test]
    fn may_run_agrees_with_refusal_detail() {
        let program = [0xE281_0001, 0xEA00_0000];
        let fixture = Fixture::arm(&program, 1);

        let nothing = |_: &mut Arm9Cpu, _: &mut NdsMmu| {};
        let thumb = |cpu: &mut Arm9Cpu, _: &mut NdsMmu| cpu.cpu.registers.set_flag(FLAG_T, true);
        let halted = |cpu: &mut Arm9Cpu, _: &mut NdsMmu| cpu.cpu.halted = true;
        let refill = |cpu: &mut Arm9Cpu, _: &mut NdsMmu| cpu.cpu.pc_modified = true;
        let watch = |_: &mut Arm9Cpu, mmu: &mut NdsMmu| mmu.tp_read_watch_on = true;
        let irq = |_: &mut Arm9Cpu, mmu: &mut NdsMmu| {
            mmu.arm9_ime = 1;
            mmu.arm9_ie = 1;
            mmu.arm9_if = 1;
        };
        // The last case arms two at once: the classifier must pick the same one
        // `may_run` short-circuits on, or the histogram misattributes.
        let halted_and_watch = |cpu: &mut Arm9Cpu, mmu: &mut NdsMmu| {
            cpu.cpu.halted = true;
            mmu.tp_read_watch_on = true;
        };
        let cases: [(&str, &dyn Fn(&mut Arm9Cpu, &mut NdsMmu), Option<StopReason>); 7] = [
            ("runnable", &nothing, None),
            ("thumb", &thumb, Some(StopReason::Thumb)),
            ("halted", &halted, Some(StopReason::Halted)),
            ("refill", &refill, Some(StopReason::PendingRefill)),
            ("watch", &watch, Some(StopReason::TouchWatch)),
            ("irq", &irq, Some(StopReason::PendingIrq)),
            ("halted+watch", &halted_and_watch, Some(StopReason::Halted)),
        ];

        for (what, arm, expected) in cases {
            let (mut cpu, mut mmu) = fixture.instantiate();
            arm(&mut cpu, &mut mmu);
            let mut jit = Arm9Jit::with_min_block_instrs(1);
            // The policy, not the environment: the deployment defaults must not
            // decide which branches this test exercises.
            jit.set_thumb_enabled(false);
            jit.set_refill_servicing(false);
            let detail = jit.refusal_detail(&cpu, &mmu);
            assert_eq!(detail, expected, "classification: {what}");
            assert_eq!(
                jit.may_run(&cpu, &mmu),
                detail.is_none(),
                "may_run disagrees with refusal_detail: {what}"
            );
        }
    }
}

#[cfg(test)]
mod layout_probe {
    /// Sizes that decide how many cache lines a block entry touches.
    #[test]
    fn report_hot_structure_sizes() {
        eprintln!(
            "CachedBlock {} B | Compiled {} B | Guard {} B | JitContext {} B",
            std::mem::size_of::<super::CachedBlock>(),
            std::mem::size_of::<crate::jit::compile::Compiled>(),
            std::mem::size_of::<super::Guard>(),
            std::mem::size_of::<crate::jit::compile::JitContext>(),
        );
    }
}
