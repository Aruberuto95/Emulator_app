//! Differential harness: run two ARM9 execution backends from identical state
//! and report the first instruction at which they disagree.
//!
//! # Why this exists before any code generation
//!
//! A recompiler fails *quietly*. A wrong carry in one addressing mode does not
//! crash — it corrupts a game's arithmetic thousands of instructions later, and
//! by then the failing instruction is unrecoverable from the symptom. The only
//! instrument that localises that is one which compares the recompiler against
//! the interpreter **after every instruction**, so this is built first and every
//! codegen milestone is gated on it.
//!
//! # What is compared
//!
//! State capture goes through [`crate::snapshot::Snap`], the traversal that
//! already defines "what is CPU state" for savestates. That is deliberate: a
//! hand-written field list would drift the moment someone adds a field to
//! [`GbaCpu`], and the drift would show up as a divergence the harness cannot
//! see. Anything a savestate must preserve, this compares.
//!
//! On top of that it compares
//!
//! * the **cycle count** each backend returns — the NDS run loop schedules the
//!   PPU, APU, timers and the other core off it, so a backend that is
//!   register-correct but cycle-wrong moves every determinism oracle; and
//! * a **memory window**, so a store that goes to the wrong address or writes
//!   the wrong value is caught at the instruction that made it rather than at
//!   the end of the trial.
//!
//! # Granularity
//!
//! [`Arm9Backend::step`] advances by whatever its backend works in and *reports*
//! how far, so the driver can step the reference the same distance. The
//! recompiler is still driven with a one-instruction block limit here, which
//! exercises the emitter's prologue, body and epilogue on every instruction in
//! the corpus rather than only in aggregate — but it may still follow a block
//! with an instruction it already knows it cannot translate, and the comparison
//! has to stay valid when it does.

use crate::gba::cpu::GbaCpu;
use crate::nds::cpu::{Arm7Cpu, Arm9Cpu};
use crate::nds::mmu::NdsMmu;
use crate::snapshot::{Snap, Writer};

/// The CPU-wrapper surface the harness drives. Both NDS cores expose the
/// shared `GbaCpu` and a `Snap` traversal, which is everything capture and
/// lockstep need — the differences (bus, IRQ model) live behind `step`.
pub trait DiffCpu: Snap {
    fn gba(&self) -> &GbaCpu;
    fn gba_mut(&mut self) -> &mut GbaCpu;
}

impl DiffCpu for Arm9Cpu {
    fn gba(&self) -> &GbaCpu {
        &self.cpu
    }
    fn gba_mut(&mut self) -> &mut GbaCpu {
        &mut self.cpu
    }
}

impl DiffCpu for Arm7Cpu {
    fn gba(&self) -> &GbaCpu {
        &self.cpu
    }
    fn gba_mut(&mut self) -> &mut GbaCpu {
        &mut self.cpu
    }
}

/// Where a fixture loads its program. Main RAM, so stores and instruction
/// fetches both take the ordinary path rather than a TCM window.
pub const CODE_BASE: u32 = 0x0200_0000;

/// Start of the scratch area fuzzed loads and stores are pointed at.
///
/// Far enough past [`CODE_BASE`] that a fuzzed store cannot rewrite the program
/// under test — self-modifying code is a real thing the recompiler must handle,
/// but it is a *separate* test, not background noise in every other one.
pub const SCRATCH_BASE: u32 = 0x0200_8000;

/// Bytes of [`SCRATCH_BASE`] hashed after every instruction. One cache line's
/// worth of words per step is cheap enough to run per-instruction over a large
/// corpus, and the fixture seeds every address register inside it.
pub const SCRATCH_BYTES: u32 = 256;

/// Initial stack pointer. Inside the scratch window, so PUSH/POP and the
/// block-transfer forms write somewhere the memory comparison can see.
const STACK_TOP: u32 = SCRATCH_BASE + SCRATCH_BYTES / 2;

// ---------------------------------------------------------------------------
// Backends
// ---------------------------------------------------------------------------

/// One way of advancing an ARM9 by a single guest instruction.
///
/// A generic bound rather than a trait object: the harness is test code, but
/// keeping the same discipline as [`crate::cpu_bus::CpuBus`] means a backend
/// cannot accidentally acquire dynamic-dispatch cost if it is ever promoted
/// into a probe that measures throughput.
pub trait Arm9Backend {
    /// Shown in a divergence report, so the reader can tell which side is which.
    fn name(&self) -> &'static str;

    /// Advance the core and report what that cost.
    ///
    /// A backend is **not** required to retire exactly one instruction: a block
    /// recompiler naturally advances several, and may follow a block with an
    /// interpreted instruction it already knows it cannot translate. The driver
    /// keeps the reference in step using [`Advance::instructions`], so the
    /// comparison stays valid at whatever granularity a backend works in.
    ///
    /// Whatever it covers must have the same observable effect as that many
    /// [`Arm9Cpu::step`] calls, including halt handling, the IRQ poll and the
    /// pipeline update.
    fn step(&mut self, cpu: &mut Arm9Cpu, mmu: &mut NdsMmu) -> Advance;
}

/// What one call to [`Arm9Backend::step`] advanced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Advance {
    pub cycles: u32,
    /// Guest instructions retired. The reference is stepped this many times.
    pub instructions: u32,
}

impl Advance {
    /// One instruction, the interpreter's unit.
    pub fn one(cycles: u32) -> Self {
        Self { cycles, instructions: 1 }
    }
}

/// The reference: the existing interpreter, unmodified.
pub struct Interpreter;

impl Arm9Backend for Interpreter {
    fn name(&self) -> &'static str {
        "interpreter"
    }
    fn step(&mut self, cpu: &mut Arm9Cpu, mmu: &mut NdsMmu) -> Advance {
        Advance::one(cpu.step(mmu))
    }
}

/// One way of advancing an **ARM7** by a guest instruction; the ARM7 twin of
/// [`Arm9Backend`], with the identical contract.
pub trait Arm7Backend {
    fn name(&self) -> &'static str;
    fn step(&mut self, cpu: &mut Arm7Cpu, mmu: &mut NdsMmu) -> Advance;
}

/// The ARM7 reference: `Arm7Cpu::step`, unmodified.
pub struct Arm7Interpreter;

impl Arm7Backend for Arm7Interpreter {
    fn name(&self) -> &'static str {
        "arm7-interpreter"
    }
    fn step(&mut self, cpu: &mut Arm7Cpu, mmu: &mut NdsMmu) -> Advance {
        Advance::one(cpu.step(mmu))
    }
}

// ---------------------------------------------------------------------------
// State capture
// ---------------------------------------------------------------------------

/// Everything the harness compares after one instruction.
///
/// `snapshot` is the [`Snap`] traversal of the whole [`Arm9Cpu`]; the named
/// fields duplicate a few bytes of it on purpose, so a report can say
/// "`r7` differs" instead of "byte 28 of the snapshot differs".
#[derive(Clone, PartialEq, Eq)]
pub struct Observation {
    snapshot: Vec<u8>,
    gpr: [u32; 16],
    cpsr: u32,
    spsr: u32,
    pipeline: [u32; 2],
    pc_modified: bool,
    halted: bool,
    cycles: u32,
    scratch_hash: u64,
}

impl Observation {
    /// Capture `cpu` and the fixture's memory window.
    ///
    /// Takes `&mut` because the snapshot traversal is shared with the save
    /// direction, which needs mutable access to overwrite on load; nothing here
    /// modifies the CPU.
    pub fn capture<Cpu: DiffCpu>(cpu: &mut Cpu, mmu: &mut NdsMmu, cycles: u32) -> Self {
        // Under a pending refill the pipeline is dead state: the next `step`
        // flushes it from R15 before anything reads it, and the two backends
        // legitimately hold *different* stale words there — after a taken
        // early-exit branch the recompiler hands back the block-end pipeline
        // where the interpreter holds the branch's own interleaved fetches.
        // Normalise it to zero (in the named field and in the snapshot alike)
        // so the comparison is of state that can still be observed; whether
        // the refill is pending at all is still compared, and the flush's
        // *outcome* is compared one step later. This is the same rule the
        // early-exit test already applied by hand.
        let stale = cpu.gba().pc_modified;
        let saved = cpu.gba().pipeline;
        if stale {
            cpu.gba_mut().pipeline = [0; 2];
        }
        let mut w = Writer::with_capacity(256);
        cpu.snap(&mut w);
        let observed = Self {
            snapshot: w.out,
            gpr: cpu.gba().registers.gpr,
            cpsr: cpu.gba().registers.cpsr,
            spsr: cpu.gba().registers.spsr,
            pipeline: cpu.gba().pipeline,
            pc_modified: cpu.gba().pc_modified,
            halted: cpu.gba().halted,
            cycles,
            scratch_hash: hash_window(mmu, SCRATCH_BASE, SCRATCH_BYTES),
        };
        if stale {
            cpu.gba_mut().pipeline = saved;
        }
        observed
    }

    /// The first field that differs, named for a report, or `None`.
    ///
    /// Ordered most-specific first so the message points at the register that
    /// actually changed rather than at the snapshot blob that contains it.
    fn first_difference(&self, other: &Self) -> Option<String> {
        for (i, (a, b)) in self.gpr.iter().zip(&other.gpr).enumerate() {
            if a != b {
                return Some(format!("r{i}: {a:#010x} vs {b:#010x}"));
            }
        }
        // `pc_modified` before the pipeline: when only one side has a refill
        // pending, that is the finding — its pipeline was normalised to zero
        // by `capture`, and reporting the zero would point at the symptom.
        let scalars: [(&str, u32, u32); 6] = [
            ("cpsr", self.cpsr, other.cpsr),
            ("spsr", self.spsr, other.spsr),
            ("pc_modified/halted", self.flags_word(), other.flags_word()),
            ("pipeline[0]", self.pipeline[0], other.pipeline[0]),
            ("pipeline[1]", self.pipeline[1], other.pipeline[1]),
            ("cycles", self.cycles, other.cycles),
        ];
        for (name, a, b) in scalars {
            if a != b {
                return Some(format!("{name}: {a:#010x} vs {b:#010x}"));
            }
        }
        if self.scratch_hash != other.scratch_hash {
            return Some(format!(
                "memory {SCRATCH_BASE:#010x}..+{SCRATCH_BYTES:#x}: {:#018x} vs {:#018x}",
                self.scratch_hash, other.scratch_hash
            ));
        }
        if self.snapshot != other.snapshot {
            let at = self
                .snapshot
                .iter()
                .zip(&other.snapshot)
                .position(|(a, b)| a != b)
                .unwrap_or(self.snapshot.len().min(other.snapshot.len()));
            return Some(format!(
                "banked/CP15 state: snapshot byte {at} differs (not covered by a named field)"
            ));
        }
        None
    }

    /// `pc_modified` and `halted` packed so they can share the scalar table.
    fn flags_word(&self) -> u32 {
        u32::from(self.pc_modified) | (u32::from(self.halted) << 1)
    }
}

/// FNV-1a over `len` bytes of ARM9 memory from `base`.
///
/// Word-at-a-time through `read_word_arm9`, which is the plain aligned read
/// every non-side-effecting path uses, so hashing cannot itself perturb state.
fn hash_window(mmu: &mut NdsMmu, base: u32, len: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for off in (0..len).step_by(4) {
        let w = mmu.read_word_arm9(base.wrapping_add(off));
        for byte in w.to_le_bytes() {
            h ^= u64::from(byte);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// A reproducible starting point: a program, and a deterministic register seed.
///
/// Two backends are each given their *own* `Arm9Cpu` and `NdsMmu` built from the
/// same fixture, so neither can perturb the other through shared memory.
pub struct Fixture {
    program: Vec<u32>,
    seed: u64,
    thumb: bool,
}

impl Fixture {
    /// An ARM-state fixture running `program` from [`CODE_BASE`].
    pub fn arm(program: &[u32], seed: u64) -> Self {
        Self { program: program.to_vec(), seed, thumb: false }
    }

    /// A Thumb-state fixture. `program` is still supplied as words; each word
    /// contributes two 16-bit instructions, low halfword first.
    pub fn thumb(program: &[u32], seed: u64) -> Self {
        Self { program: program.to_vec(), seed, thumb: true }
    }

    /// Build a fresh CPU and MMU. Called once per backend, so both start
    /// byte-identical.
    pub fn instantiate(&self) -> (Arm9Cpu, NdsMmu) {
        let mut mmu = NdsMmu::new();
        for (i, &w) in self.program.iter().enumerate() {
            mmu.write_word_arm9(CODE_BASE + (i as u32) * 4, w);
        }

        // Fill the scratch window with a recognisable, non-uniform pattern: an
        // all-zero window would let a load from the wrong address still produce
        // the right value, hiding an addressing-mode bug.
        let mut rng = Rng::new(self.seed ^ 0x5f3f_a91d);
        for off in (0..SCRATCH_BYTES).step_by(4) {
            mmu.write_word_arm9(SCRATCH_BASE + off, rng.next_u32());
        }

        let mut cpu = Arm9Cpu::new();
        // System mode: no banked-register surprises from the seed itself, and
        // it is the mode DS game code runs in.
        cpu.cpu.registers.cpsr = 0x1F | if self.thumb { 0x20 } else { 0 };
        // r0..r12 alternate between a scratch address and a small integer, so
        // both "this register is a pointer" and "this register is data" are
        // covered whichever role an encoding assigns them.
        Self::seed_gprs(&mut cpu.cpu, &mut rng);
        cpu.flush_pipeline(&mut mmu);
        (cpu, mmu)
    }

    /// The ARM7 twin of [`Self::instantiate`]. The program and scratch land in
    /// main RAM — the same storage both cores see at [`CODE_BASE`] — written
    /// through the ARM7 bus so the fixture exercises that decode too.
    pub fn instantiate_arm7(&self) -> (Arm7Cpu, NdsMmu) {
        let mut mmu = NdsMmu::new();
        for (i, &w) in self.program.iter().enumerate() {
            mmu.write_word_arm7(CODE_BASE + (i as u32) * 4, w);
        }
        let mut rng = Rng::new(self.seed ^ 0x5f3f_a91d);
        for off in (0..SCRATCH_BYTES).step_by(4) {
            mmu.write_word_arm7(SCRATCH_BASE + off, rng.next_u32());
        }

        let mut cpu = Arm7Cpu::new();
        cpu.cpu.registers.cpsr = 0x1F | if self.thumb { 0x20 } else { 0 };
        Self::seed_gprs(&mut cpu.cpu, &mut rng);
        cpu.flush_pipeline(&mut mmu);
        (cpu, mmu)
    }

    /// The register seed both cores share; see [`Self::instantiate`] for the
    /// even/odd pointer-vs-data rule.
    fn seed_gprs(gba: &mut GbaCpu, rng: &mut Rng) {
        for r in 0..13usize {
            gba.registers.gpr[r] = if r % 2 == 0 {
                SCRATCH_BASE + (rng.next_u32() % (SCRATCH_BYTES / 2)) / 4 * 4
            } else {
                rng.next_u32() & 0xFFFF
            };
        }
        gba.registers.gpr[13] = STACK_TOP;
        gba.registers.gpr[14] = CODE_BASE;
        gba.registers.gpr[15] = CODE_BASE;
    }
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

/// Where and how two backends disagreed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// Zero-based index of the instruction after which the states differed.
    pub step: usize,
    /// Address of that instruction, as the reference backend saw it.
    pub pc: u32,
    /// The instruction word (or halfword, in Thumb).
    pub instruction: u32,
    /// Which backends, and what differed.
    pub detail: String,
}

impl std::fmt::Display for Divergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "step {} at pc {:#010x} (instr {:#010x}): {}",
            self.step, self.pc, self.instruction, self.detail
        )
    }
}

/// Step `reference` and `candidate` through `fixture` in lockstep, comparing
/// after every instruction. Returns the first disagreement.
///
/// Each backend gets its own machine, so the comparison is of two independent
/// executions rather than of one execution observed twice.
pub fn compare_lockstep<R: Arm9Backend, C: Arm9Backend>(
    fixture: &Fixture,
    reference: &mut R,
    candidate: &mut C,
    steps: usize,
) -> Result<(), Divergence> {
    let ref_name = reference.name();
    let cand_name = candidate.name();
    lockstep(
        || fixture.instantiate(),
        |cpu, mmu| reference.step(cpu, mmu),
        ref_name,
        |cpu, mmu| candidate.step(cpu, mmu),
        cand_name,
        steps,
    )
}

/// [`compare_lockstep`] for the ARM7: the same driver over the ARM7 fixture
/// and backends, so the two cores' recompilers are held to the same gate.
pub fn compare_lockstep_arm7<R: Arm7Backend, C: Arm7Backend>(
    fixture: &Fixture,
    reference: &mut R,
    candidate: &mut C,
    steps: usize,
) -> Result<(), Divergence> {
    let ref_name = reference.name();
    let cand_name = candidate.name();
    lockstep(
        || fixture.instantiate_arm7(),
        |cpu, mmu| reference.step(cpu, mmu),
        ref_name,
        |cpu, mmu| candidate.step(cpu, mmu),
        cand_name,
        steps,
    )
}

/// The core-agnostic lockstep driver both public entry points share.
fn lockstep<Cpu: DiffCpu>(
    instantiate: impl Fn() -> (Cpu, NdsMmu),
    mut reference: impl FnMut(&mut Cpu, &mut NdsMmu) -> Advance,
    ref_name: &'static str,
    mut candidate: impl FnMut(&mut Cpu, &mut NdsMmu) -> Advance,
    cand_name: &'static str,
    steps: usize,
) -> Result<(), Divergence> {
    let (mut ref_cpu, mut ref_mmu) = instantiate();
    let (mut cand_cpu, mut cand_mmu) = instantiate();

    // A fixture that does not start identical would make every later comparison
    // meaningless, so prove it before the first step rather than assuming it.
    let start_ref = Observation::capture(&mut ref_cpu, &mut ref_mmu, 0);
    let start_cand = Observation::capture(&mut cand_cpu, &mut cand_mmu, 0);
    if let Some(detail) = start_ref.first_difference(&start_cand) {
        return Err(Divergence {
            step: 0,
            pc: ref_cpu.gba().registers.gpr[15],
            instruction: 0,
            detail: format!("fixture is not deterministic: {detail}"),
        });
    }

    for step in 0..steps {
        // Captured before the step: after it, the pipeline has already advanced.
        let pc = ref_cpu.gba().registers.gpr[15];
        let instruction = ref_cpu.gba().pipeline[0];

        // The candidate goes first and says how far it went; the reference then
        // covers the same ground. Comparing a multi-instruction block against a
        // single interpreter step would report a divergence on every block
        // rather than on every bug.
        let cand = candidate(&mut cand_cpu, &mut cand_mmu);
        let mut ref_cycles = 0;
        for _ in 0..cand.instructions.max(1) {
            ref_cycles += reference(&mut ref_cpu, &mut ref_mmu).cycles;
        }

        let a = Observation::capture(&mut ref_cpu, &mut ref_mmu, ref_cycles);
        let b = Observation::capture(&mut cand_cpu, &mut cand_mmu, cand.cycles);
        if let Some(detail) = a.first_difference(&b) {
            return Err(Divergence {
                step,
                pc,
                instruction,
                detail: format!("{ref_name} vs {cand_name}: {detail}"),
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------

/// xorshift64*, so a failing trial is reproducible from its seed alone.
///
/// Hand-rolled rather than a dependency: the harness needs *determinism*, not
/// statistical quality, and adding a crate to get thirty bits of arithmetic
/// would be the worse trade in a crate whose only dependency is `cxx`.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // A zero state is a fixed point of xorshift; move off it.
        Self(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Uniform-enough value in `0..n`. `n` must be non-zero.
    pub fn below(&mut self, n: u32) -> u32 {
        self.next_u32() % n
    }
}

/// The ARM encoding classes the recompiler will translate, in the proportions
/// the instruction mix on the SoulSilver overworld actually retires them.
///
/// Generating uniformly over all 2^32 words would spend almost the entire
/// corpus in the coprocessor and undefined spaces, which the recompiler declines
/// anyway. Generating per class is what makes the fuzz reach the addressing
/// modes that carry the bugs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmClass {
    /// Data processing, register operand (22.1% of retired instructions).
    DataProcReg,
    /// Data processing, immediate operand (18.4%).
    DataProcImm,
    /// LDR/STR (16.5%).
    SingleTransfer,
    /// LDM/STM (7.5%).
    BlockTransfer,
    /// B/BL (13.5%).
    Branch,
    /// LDRH/LDRSB/LDRSH/STRH — the extension space that headed the
    /// refused-successor census.
    Halfword,
    /// MRS/MSR on CPSR — the interrupt-critical-section idiom.
    Psr,
    /// MUL/MLA and the long forms — the family the ARM9's refused-target
    /// census named at `0x020f294c`/`0x020d1144` (fixed-point math and the
    /// sound mixer both lean on it).
    Multiply,
}

impl ArmClass {
    /// Every class, for a caller that wants full coverage rather than the mix.
    pub const ALL: [ArmClass; 8] = [
        Self::DataProcReg,
        Self::DataProcImm,
        Self::SingleTransfer,
        Self::BlockTransfer,
        Self::Branch,
        Self::Halfword,
        Self::Psr,
        Self::Multiply,
    ];
}

/// Generate one random instruction of `class`, constrained so it is *executable*
/// in a fixture rather than merely well-formed.
///
/// Three constraints are applied throughout, and each one exists because the
/// unconstrained form makes the trial useless rather than because it is invalid:
///
/// * **R15 is never a destination or a base.** A fuzzed write to PC lands
///   somewhere outside the program and the rest of the trial measures whatever
///   happens to be in RAM. Branches are covered by [`ArmClass::Branch`], which
///   controls its own target.
/// * **The condition field is `AL` most of the time.** A uniformly random
///   condition would fail roughly half the corpus before it reached a handler,
///   so the other conditions are sampled deliberately instead of dominating.
/// * **Transfer offsets stay inside the scratch window**, which the fixture has
///   already pointed every even-numbered register at.
pub fn random_instruction(class: ArmClass, rng: &mut Rng) -> u32 {
    // 3 in 4 unconditional; the rest sample the real condition codes (0..=13,
    // excluding NV, which is the ARMv5 BLX space and not a condition at all).
    let cond = if rng.below(4) != 0 { 0xE } else { rng.below(14) };
    let base = cond << 28;

    // Registers 0..=12: never R13/R14 (the fixture's stack and link) and never
    // R15, so a trial cannot destroy its own frame or control flow.
    let reg = |rng: &mut Rng| rng.below(13);

    match class {
        ArmClass::DataProcImm => {
            let opcode = rng.below(16);
            let s = rng.below(2);
            let rn = reg(rng);
            let rd = reg(rng);
            let rot = rng.below(16);
            let imm = rng.below(256);
            base | (1 << 25) | (opcode << 21) | (s << 20) | (rn << 16) | (rd << 12) | (rot << 8) | imm
        }
        ArmClass::DataProcReg => {
            let opcode = rng.below(16);
            let s = rng.below(2);
            let rn = reg(rng);
            let rd = reg(rng);
            let rm = reg(rng);
            let shift_type = rng.below(4);
            // Immediate shift amount only. Register-controlled shifts read Rs at
            // execute time and are a distinct encoding; they get their own pass
            // once the emitter handles them, and mixing them in here would make
            // a failure ambiguous between the two forms.
            let amount = rng.below(32);
            base | (opcode << 21) | (s << 20) | (rn << 16) | (rd << 12)
                | (amount << 7) | (shift_type << 5) | rm
        }
        ArmClass::SingleTransfer => {
            let load = rng.below(2);
            let byte = rng.below(2);
            let writeback = rng.below(2);
            let pre = rng.below(2);
            let up = rng.below(2);
            // Even registers hold scratch addresses (see `Fixture::instantiate`).
            let rn = rng.below(7) * 2;
            let rd = reg(rng);
            if rng.below(3) == 0 {
                // Register offset through the barrel shifter. `LSR #10..=#13`
                // keeps the shifted value small whatever the seed put in Rm —
                // a scratch pointer or a 16-bit integer both land within a
                // few KiB of the base, deterministic main-RAM addresses on
                // both sides of the comparison.
                let rm = reg(rng);
                let amount = 10 + rng.below(4);
                base | (1 << 26) | (1 << 25) | (pre << 24) | (up << 23) | (byte << 22)
                    | (writeback << 21) | (load << 20) | (rn << 16) | (rd << 12)
                    | (amount << 7) | (1 << 5) | rm
            } else {
                // Offset small enough that base +/- offset stays in the window.
                let imm = rng.below(SCRATCH_BYTES / 4);
                base | (1 << 26) | (pre << 24) | (up << 23) | (byte << 22) | (writeback << 21)
                    | (load << 20) | (rn << 16) | (rd << 12) | imm
            }
        }
        ArmClass::BlockTransfer => {
            let load = rng.below(2);
            let pre = rng.below(2);
            let up = rng.below(2);
            let writeback = rng.below(2);
            let rn = rng.below(7) * 2;
            // Never R15 in the list: that is a control-flow change, and the
            // S bit is never set because the recompiler bails out of those.
            let list = rng.next_u32() & 0x0000_1FFF;
            // An empty list is an edge case with contested semantics; the corpus
            // covers it as a named case instead of at random.
            let list = if list == 0 { 1 } else { list };
            base | (1 << 27) | (pre << 24) | (up << 23) | (writeback << 21) | (load << 20)
                | (rn << 16) | list
        }
        ArmClass::Branch => {
            let link = rng.below(2);
            // Stay within +/- 8 instructions of here, so the branch lands inside
            // a program the fixture actually loaded.
            let delta = (rng.below(16) as i32) - 8;
            let offset = (delta as u32) & 0x00FF_FFFF;
            base | (1 << 27) | (1 << 25) | (link << 24) | offset
        }
        ArmClass::Halfword => {
            let load = rng.below(2);
            // Loads sample all three kinds; the store side of the space is
            // STRH only — L=0 with SH >= 2 is LDRD/STRD, declined by the
            // recompiler and covered as named cases, not random noise.
            let sh = if load == 1 { 1 + rng.below(3) } else { 1 };
            let writeback = rng.below(2);
            let pre = rng.below(2);
            let up = rng.below(2);
            let rn = rng.below(7) * 2;
            let rd = reg(rng);
            // Immediate offsets only: the register form would add a whole
            // scratch *address* to the base and wander out of the window; it
            // gets deterministic coverage in a handwritten test instead.
            let off = rng.below(SCRATCH_BYTES / 4);
            base | (pre << 24) | (up << 23) | (1 << 22) | (writeback << 21) | (load << 20)
                | (rn << 16) | (rd << 12) | ((off & 0xF0) << 4) | 0x90 | (sh << 5) | (off & 0xF)
        }
        ArmClass::Multiply => {
            // MUL/MLA and every long form, S sampled — the S rule (N and Z
            // only, C and V untouched) is exactly what the comparison must
            // hold under. Result registers are unconstrained: multiplies
            // never touch memory, so the window rules do not apply.
            let s = rng.below(2);
            let rd = reg(rng);
            let rn = reg(rng);
            let rs = reg(rng);
            let rm = reg(rng);
            if rng.below(3) == 0 {
                let a = rng.below(2); // MUL / MLA
                base | (a << 21) | (s << 20) | (rd << 16) | (rn << 12) | (rs << 8) | 0x90 | rm
            } else {
                let signed = rng.below(2);
                let a = rng.below(2); // UMULL/UMLAL/SMULL/SMLAL
                base | (1 << 23) | (signed << 22) | (a << 21) | (s << 20) | (rd << 16)
                    | (rn << 12) | (rs << 8) | 0x90 | rm
            }
        }
        ArmClass::Psr => {
            if rng.below(3) == 0 {
                // MRS Rd, CPSR
                base | 0x010F_0000 | (reg(rng) << 12)
            } else {
                // MSR CPSR_fields, op — flags and/or control byte. Random
                // control writes genuinely switch modes and swap banks on
                // both sides, which is exactly what the comparison must hold
                // under. Unconditional: the conditional form stays an
                // interpreter exit and would only dilute the corpus.
                let field = (rng.below(2) << 19) | (rng.below(2) << 16);
                let msr = 0xE000_0000 | 0x0120_F000 | field;
                if rng.below(2) == 1 {
                    msr | (1 << 25) | (rng.below(16) << 8) | rng.below(256)
                } else {
                    msr | reg(rng)
                }
            }
        }
    }
}

/// Generate one random **Thumb** instruction from the formats the recompiler
/// translates.
///
/// Constrained the same way [`random_instruction`] is, and for the same reason:
/// the register fields are three bits so R15 cannot be named at all, which
/// removes the whole class of "the trial wandered out of its own program" that
/// the ARM generator has to guard against by hand.
pub fn random_thumb(rng: &mut Rng) -> u16 {
    let rd = rng.below(8) as u16;
    let rs = rng.below(8) as u16;
    match rng.below(11) {
        // F1 move shifted register. Shift type 0..=2 only; 3 is F2's window.
        0 => {
            let op = rng.below(3) as u16;
            let amount = rng.below(32) as u16;
            (op << 11) | (amount << 6) | (rs << 3) | rd
        }
        // F2 add/subtract, register or 3-bit immediate.
        1 => {
            let flags = rng.below(4) as u16; // I and Op bits
            let rn = rng.below(8) as u16;
            0x1800 | (flags << 9) | (rn << 6) | (rs << 3) | rd
        }
        // F3 move/compare/add/subtract with an 8-bit immediate.
        2 => {
            let op = rng.below(4) as u16;
            let imm = rng.below(256) as u16;
            0x2000 | (op << 11) | (rd << 8) | imm
        }
        // F4 ALU. Every opcode, including the four the translator declines, so
        // the corpus exercises the decline path too.
        3 => {
            let op = rng.below(16) as u16;
            0x4000 | (op << 6) | (rs << 3) | rd
        }
        // F5 hi-register. `Op == 3` is BX and the H1 destination can name R15;
        // both must be refused rather than compiled, so both are generated.
        4 => {
            let op = rng.below(4) as u16;
            let h = rng.below(4) as u16;
            0x4400 | (op << 8) | (h << 6) | (rs << 3) | rd
        }
        // F6 PC-relative load. Reads `R15 & !2`, so it is the case that catches
        // a missing word-alignment.
        5 => 0x4800 | (rd << 8) | rng.below(64) as u16,
        // F9 load/store with a 5-bit offset, word and byte.
        6 => {
            let flags = rng.below(4) as u16; // B and L
            let off5 = rng.below(32) as u16;
            0x6000 | (flags << 11) | (off5 << 6) | (rs << 3) | rd
        }
        // F11 SP-relative load/store. The fixture seeds SP inside the scratch
        // window, so these land somewhere the memory comparison can see.
        7 => {
            let load = rng.below(2) as u16;
            0x9000 | (load << 11) | (rd << 8) | rng.below(32) as u16
        }
        // F14 PUSH/POP, including `POP {pc}` which must be refused.
        8 => {
            let pop = rng.below(2) as u16;
            let r = rng.below(2) as u16;
            0xB400 | (pop << 11) | (r << 8) | rng.below(256) as u16
        }
        // F15 LDMIA/STMIA, including the empty list and the case where the base
        // register is itself in the list.
        9 => {
            let load = rng.below(2) as u16;
            0xC000 | (load << 11) | (rd << 8) | rng.below(256) as u16
        }
        // F12 load address (SP and PC forms) and F13 adjust SP, which are the
        // two formats that move the stack pointer without touching memory.
        _ => {
            if rng.below(2) == 0 {
                0xA000 | ((rng.below(2) as u16) << 11) | (rd << 8) | rng.below(64) as u16
            } else {
                0xB000 | ((rng.below(2) as u16) << 7) | rng.below(16) as u16
            }
        }
    }
}

/// A Thumb program: `len` instructions then a backward branch to the start.
///
/// Two halfwords per word, low first, which is how [`Fixture::thumb`] lays them
/// out in memory.
pub fn random_thumb_program(len: usize, rng: &mut Rng) -> Vec<u32> {
    let mut halves: Vec<u16> = (0..len).map(|_| random_thumb(rng)).collect();
    // F18 unconditional branch. R15 reads as `addr + 4` and the offset is a
    // signed 11-bit *halfword* count, so landing on the start from index `len`
    // needs `-(len + 2)`.
    let back = (-(len as i32 + 2)) as u16 & 0x07FF;
    halves.push(0xE000 | back);
    if halves.len() % 2 == 1 {
        halves.push(0x46C0); // MOV r8,r8 — the canonical Thumb NOP
    }
    halves.chunks_exact(2).map(|p| u32::from(p[0]) | (u32::from(p[1]) << 16)).collect()
}

/// A program of `len` instructions drawn from `classes`, followed by a branch
/// back to the start so a trial can run longer than the program.
pub fn random_program(classes: &[ArmClass], len: usize, rng: &mut Rng) -> Vec<u32> {
    let mut out: Vec<u32> = (0..len)
        .map(|_| {
            let class = classes[rng.below(classes.len() as u32) as usize];
            random_instruction(class, rng)
        })
        .collect();
    // The back-branch sits at index `len`, i.e. `len * 4` past `CODE_BASE`, and
    // R15 reads eight ahead while it executes. So its target is
    // `CODE_BASE + len*4 + 8 + (offset << 2)`, and landing on `CODE_BASE`
    // needs `offset = -(len + 2)`.
    let back = -(len as i32 + 2) as u32 & 0x00FF_FFFF;
    out.push(0xEA00_0000 | back);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A backend that is the interpreter until step `at`, then flips the carry
    /// out of one instruction.
    ///
    /// This is what makes the harness trustworthy. A comparison that cannot fail
    /// proves nothing, so the suite asserts both directions: identical backends
    /// agree, and a backend with one wrong flag is caught at the instruction
    /// that produced it.
    ///
    /// Flips rather than clears: clearing a carry that was already clear is a
    /// no-op, and a mutant that sometimes does not mutate would make this test
    /// pass or fail on the corpus seed rather than on the harness.
    struct CarryMutant {
        step_count: usize,
        at: usize,
    }

    impl Arm9Backend for CarryMutant {
        fn name(&self) -> &'static str {
            "carry-mutant"
        }
        fn step(&mut self, cpu: &mut Arm9Cpu, mmu: &mut NdsMmu) -> Advance {
            let cycles = cpu.step(mmu);
            if self.step_count == self.at {
                cpu.cpu.registers.cpsr ^= 1 << 29; // flip C
            }
            self.step_count += 1;
            Advance::one(cycles)
        }
    }

    /// A backend that returns the right state but the wrong cycle count. The NDS
    /// run loop schedules every peripheral off this number, so it has to be
    /// compared as strictly as a register.
    struct CycleMutant {
        step_count: usize,
        at: usize,
    }

    impl Arm9Backend for CycleMutant {
        fn name(&self) -> &'static str {
            "cycle-mutant"
        }
        fn step(&mut self, cpu: &mut Arm9Cpu, mmu: &mut NdsMmu) -> Advance {
            let cycles = cpu.step(mmu);
            let out = if self.step_count == self.at { cycles + 1 } else { cycles };
            self.step_count += 1;
            Advance::one(out)
        }
    }

    /// A backend that skips one store. Proves the memory window is compared, not
    /// just the registers — the failure mode a register-only harness would miss.
    struct StoreMutant {
        step_count: usize,
        at: usize,
    }

    impl Arm9Backend for StoreMutant {
        fn name(&self) -> &'static str {
            "store-mutant"
        }
        fn step(&mut self, cpu: &mut Arm9Cpu, mmu: &mut NdsMmu) -> Advance {
            if self.step_count == self.at {
                // Corrupt one scratch word without touching any register.
                let old = mmu.read_word_arm9(SCRATCH_BASE);
                mmu.write_word_arm9(SCRATCH_BASE, old ^ 1);
            }
            self.step_count += 1;
            Advance::one(cpu.step(mmu))
        }
    }

    fn mixed_program(seed: u64, len: usize) -> Fixture {
        let mut rng = Rng::new(seed);
        Fixture::arm(&random_program(&ArmClass::ALL, len, &mut rng), seed)
    }

    /// The harness must report no difference between two interpreters. If this
    /// fails, either the fixture is not deterministic or the observation
    /// captures something that is not machine state.
    #[test]
    fn interpreter_agrees_with_itself() {
        for seed in 1..40u64 {
            let fixture = mixed_program(seed, 24);
            compare_lockstep(&fixture, &mut Interpreter, &mut Interpreter, 200)
                .unwrap_or_else(|d| panic!("seed {seed}: interpreter diverged from itself: {d}"));
        }
    }

    /// ...and it must report a difference when there is one. Three mutants, one
    /// per comparison channel: registers, cycles, memory.
    #[test]
    fn harness_detects_a_wrong_flag() {
        // A long-enough program that step 30 is certainly an executed instruction.
        let fixture = mixed_program(7, 24);
        let mut mutant = CarryMutant { step_count: 0, at: 30 };
        let d = compare_lockstep(&fixture, &mut Interpreter, &mut mutant, 200)
            .expect_err("a cleared carry must be caught");
        assert_eq!(d.step, 30, "caught at the instruction that did it, not later");
        assert!(d.detail.contains("cpsr"), "reported as a CPSR difference: {d}");
    }

    #[test]
    fn harness_detects_a_wrong_cycle_count() {
        let fixture = mixed_program(11, 24);
        let mut mutant = CycleMutant { step_count: 0, at: 12 };
        let d = compare_lockstep(&fixture, &mut Interpreter, &mut mutant, 200)
            .expect_err("an off-by-one cycle must be caught");
        assert_eq!(d.step, 12);
        assert!(d.detail.contains("cycles"), "reported as a cycle difference: {d}");
    }

    #[test]
    fn harness_detects_a_memory_difference() {
        let fixture = mixed_program(13, 24);
        let mut mutant = StoreMutant { step_count: 0, at: 5 };
        let d = compare_lockstep(&fixture, &mut Interpreter, &mut mutant, 200)
            .expect_err("a corrupted store must be caught");
        assert_eq!(d.step, 5);
        assert!(d.detail.contains("memory"), "reported as a memory difference: {d}");
    }

    /// The corpus must actually reach every class it claims to. A generator that
    /// silently produced only one encoding would make the fuzz look thorough
    /// while testing one path.
    #[test]
    fn every_class_is_generated_and_decodes_as_itself() {
        let mut rng = Rng::new(99);
        for class in ArmClass::ALL {
            for _ in 0..500 {
                let inst = random_instruction(class, &mut rng);
                let bits = (inst >> 26) & 3;
                let bit25 = (inst >> 25) & 1;
                // The halfword space nests inside class 0: bits 7 and 4 set
                // with a non-zero SH field (SH == 0 is MUL/SWP).
                let halfword =
                    bits == 0 && bit25 == 0 && (inst & 0x90) == 0x90 && (inst >> 5) & 3 != 0;
                // MRS/MSR nest inside class 0's opcode-8..B-with-S-clear window.
                let psr = bits == 0
                    && ((inst & 0x0FFF_0FFF) == 0x010F_0000
                        || (inst & 0x0FF0_FFF0) == 0x0120_F000
                        || (inst & 0x0FF0_F000) == 0x0320_F000);
                // The multiply family: bits 7-4 = 1001 with SH == 0.
                let multiply = bits == 0
                    && ((inst & 0x0FC0_00F0) == 0x0000_0090
                        || (inst & 0x0F80_00F0) == 0x0080_0090);
                let actual = match (bits, bit25) {
                    _ if halfword => ArmClass::Halfword,
                    _ if psr => ArmClass::Psr,
                    _ if multiply => ArmClass::Multiply,
                    (0, 1) => ArmClass::DataProcImm,
                    (0, 0) => ArmClass::DataProcReg,
                    (1, _) => ArmClass::SingleTransfer,
                    (2, 0) => ArmClass::BlockTransfer,
                    (2, 1) => ArmClass::Branch,
                    _ => panic!("{class:?} generated {inst:#010x}, which is class 3"),
                };
                assert_eq!(actual, class, "{inst:#010x} decodes as {actual:?}, not {class:?}");
                assert_ne!((inst >> 28) & 0xF, 0xF, "NV is the BLX space, never a condition");
            }
        }
    }

    /// R15 must never be a destination or a base in the corpus, or a trial
    /// wanders out of its own program and stops testing anything.
    #[test]
    fn corpus_never_targets_r15() {
        let mut rng = Rng::new(1234);
        for class in [ArmClass::DataProcImm, ArmClass::DataProcReg, ArmClass::SingleTransfer] {
            for _ in 0..2000 {
                let inst = random_instruction(class, &mut rng);
                assert_ne!((inst >> 12) & 0xF, 15, "{class:?} produced Rd = r15: {inst:#010x}");
                assert_ne!((inst >> 16) & 0xF, 15, "{class:?} produced Rn = r15: {inst:#010x}");
            }
        }
        for _ in 0..2000 {
            let inst = random_instruction(ArmClass::BlockTransfer, &mut rng);
            assert_eq!(inst & (1 << 15), 0, "LDM/STM list must not contain r15: {inst:#010x}");
            assert_eq!(inst & (1 << 22), 0, "the S bit is out of scope: {inst:#010x}");
        }
    }

    /// The generated back-branch must land on the first instruction, or a trial
    /// only ever executes the program once and the step budget is wasted on
    /// whatever follows it in RAM.
    #[test]
    fn random_program_loops_back_to_its_start() {
        let mut rng = Rng::new(5);
        // Index 0 counts passes; the rest are NOPs (MOV r0,r0) so nothing else
        // can move PC. `MOV r1,r1` would collide with the counter register.
        let len = 6;
        let mut prog = vec![0xE1A0_0000u32; len];
        prog[0] = 0xE282_2001; // ADD r2,r2,#1 (Rn is bits 19-16: r2, not r0)
        prog.push(random_program(&[ArmClass::Branch], len, &mut rng).pop().unwrap());

        let fixture = Fixture::arm(&prog, 5);
        let (mut cpu, mut mmu) = fixture.instantiate();
        cpu.cpu.registers.gpr[2] = 0;

        for _ in 0..=len {
            cpu.step(&mut mmu); // the `len` body instructions, then the branch
        }
        // `write_pc` leaves the *raw* target in R15 and defers the refill to the
        // next step, so this is the branch target itself. Checking it here
        // rather than after the flush is what makes the assertion sensitive to
        // the offset arithmetic instead of to pipeline bookkeeping.
        assert!(cpu.cpu.pc_modified, "the branch requested a pipeline refill");
        assert_eq!(
            cpu.cpu.registers.gpr[15],
            CODE_BASE,
            "the trailing branch returns to the first instruction"
        );

        // ...and the behavioural proof: a second pass re-executes index 0.
        for _ in 0..=len {
            cpu.step(&mut mmu);
        }
        assert_eq!(cpu.cpu.registers.gpr[2], 2, "the loop body ran twice");
    }

    /// `GbaCpu::instrs` is the denominator every throughput probe divides by, so
    /// a backend that retires instructions without bumping it would report a
    /// speedup it did not achieve. Pin the interpreter's behaviour here, so the
    /// recompiler has an oracle to match rather than an assumption.
    #[test]
    fn interpreter_counts_every_retired_instruction() {
        let fixture = Fixture::arm(&[0xE1A0_0000; 8], 3);
        let (mut cpu, mut mmu) = fixture.instantiate();
        let before = cpu.cpu.instrs;
        for _ in 0..8 {
            cpu.step(&mut mmu);
        }
        assert_eq!(cpu.cpu.instrs - before, 8, "one count per retired instruction");
        assert_eq!(GbaCpu::new().instrs, 0, "a fresh core starts at zero");
    }
}
