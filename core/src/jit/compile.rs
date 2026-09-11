//! ARM -> x86-64 translation.
//!
//! # What is translated
//!
//! Data processing with an **immediate** operand, in full: every opcode, every
//! condition, with or without the `S` bit. Destination R15 is excluded, which
//! the block scanner has already guaranteed.
//!
//! That subset is 18.4% of retired instructions on the SoulSilver overworld and
//! it is the one where the second operand costs nothing: `op2` is
//! `imm.rotate_right(rot * 2)` with both fields in the instruction word, so the
//! operand *and* the shifter carry-out are compile-time constants. Register
//! operands need the barrel shifter and are the next milestone.
//!
//! # Flags
//!
//! CPSR is pinned in a host register for the life of a block, in ARM's own bit
//! positions, and written back once. Three things follow:
//!
//! * a **condition** is a `test` against a mask plus a branch;
//! * `ADC`/`SBC`/`RSC` can read the guest carry back into the host's with `bt`,
//!   so they are no longer excluded;
//! * flag *production* is `lahf` + `seto` and a pack, which is why the working
//!   value lives in `rdx` rather than `rax` — `lahf` writes `ah`.
//!
//! Two asymmetries between the architectures are handled explicitly and are the
//! classic sources of a wrong-flag bug:
//!
//! * **ARM's carry for a subtraction is the inverse of x86's.** x86 `sub` sets
//!   CF on borrow; ARM clears C on borrow. Every subtracting form emits `cmc`.
//! * **A logical operation preserves V on ARM** and clears OF on x86, so the
//!   logical path never touches the V bit rather than copying OF into it.
//!
//! # The pipeline contract
//!
//! While the instruction at `A` executes, the interpreter exposes `R15` as
//! `A + 8`. Inside a straight-line block every address is known, so **R15 is a
//! constant**: it is never maintained, only written once in the epilogue. That
//! is the single largest structural saving in the design, and it is why
//! `Rn == 15` becomes an immediate rather than a register read.

use std::ffi::c_void;

use crate::jit::exec_mem::thunks;
use crate::jit::x64::{AluOp, Cond, Emitter, Mem, Reg, ShiftOp};

/// Guest register file base (`*mut u32`, 16 slots). Callee-saved under Win64.
const GPR_BASE: Reg = Reg::Rbx;
/// [`JitContext`] pointer. Callee-saved.
const CTX: Reg = Reg::R12;
/// Guest CPSR for the life of the block. Callee-saved.
const CPSR: Reg = Reg::Rsi;
/// The working value. Volatile, and deliberately not `rax`: `lahf` writes `ah`.
const VALUE: Reg = Reg::Rdx;
/// Operand 2, once a shifted register has been materialised. Volatile.
const OP2: Reg = Reg::Rcx;
/// The barrel-shifter carry-out as a 0/1, captured before the shift runs.
const CARRY: Reg = Reg::R10;
/// Flag extraction. Volatile.
const FLAGS: Reg = Reg::Rax;
/// Flag packing scratch. Volatile.
const PACK: Reg = Reg::R8;
const PACK2: Reg = Reg::R9;

/// ARM CPSR flag masks.
const N: u32 = 1 << 31;
const Z: u32 = 1 << 30;
const C: u32 = 1 << 29;
const V: u32 = 1 << 28;
const NZCV: u32 = N | Z | C | V;

/// The state a compiled block reaches through, laid out for machine code.
///
/// `#[repr(C)]` is load-bearing: the emitted instructions address these fields
/// by numeric offset, so the layout must be the declared one.
/// [`tests::context_offsets_match_the_declared_layout`] checks the constants
/// against the real type rather than trusting the arithmetic.
///
/// It holds a *pointer* to the guest registers rather than a copy: `[u32; 16]`
/// has a guaranteed layout, so `cpu.registers.gpr.as_mut_ptr()` is a valid base
/// however `CpuRegisters` itself is laid out, and 64 bytes of copying per block
/// would be pure overhead.
#[repr(C)]
pub struct JitContext {
    /// Base of the guest register file: `&mut cpu.registers.gpr`.
    pub gpr: *mut u32,
    /// Guest CPSR, in and out.
    pub cpsr: u32,
    /// R14 as of the last body instruction, which is what the interpreter
    /// publishes in `NdsMmu::arm9_exec_lr` before executing it.
    pub exec_lr: u32,
    /// Opaque `&mut NdsMmu` for the memory thunks. Null until they exist.
    pub mmu: *mut c_void,
    /// Non-zero when a compiled branch was taken, which is what
    /// `GbaCpu::pc_modified` means: R15 was written, so the next step must
    /// refill the pipeline rather than advance.
    ///
    /// Set by the block instead of inferred by the caller because a
    /// *conditional* branch may or may not have been taken, and only the block
    /// knows.
    pub pc_modified: u32,
    /// Conditional instructions whose condition **passed**, per
    /// `GbaCpu::arm_class_hist` bucket, counted by the block itself.
    ///
    /// The interpreter bumps `arm_class_hist` only after `check_condition`
    /// succeeds, and whether a condition succeeds is not known at compile time,
    /// so a conditional instruction increments its bucket here and an
    /// unconditional one is counted statically. Getting it wrong would not break
    /// emulation — it would corrupt the instruction-mix evidence that decides
    /// what to optimise next.
    pub class_hits: [u32; 6],
    /// Cycles beyond one-per-instruction contributed by *conditional*
    /// instructions whose condition passed.
    ///
    /// `execute_arm` returns **1 cycle for a failed condition**, before it ever
    /// reaches the handler — so a conditional `LDR` costs 3 when it runs and 1
    /// when it does not. That is invisible for data processing, which costs 1
    /// either way, and wrong by two cycles for every skipped transfer. The
    /// differential harness caught it as a cycle mismatch on `LDRCS`.
    pub extra_cycles: u32,
    /// Staging buffer for `LDM`/`STM`.
    ///
    /// The block transfer thunks move a whole register list in one bus decode
    /// (see `CpuBus::read_words`), so the words need somewhere to live that both
    /// generated code and Rust can address. Sixteen is the longest list ARM can
    /// name, and the thunks clamp to it.
    pub words: [u32; Self::WORDS],
    /// Which exit the block left through: `0` for "ran to the end", `1` for the
    /// alternate exit a followed conditional branch installs, `2` for its
    /// terminal taken path, and `READ_IRQ_EXIT_BASE + instruction_index` for
    /// an interrupt requested by a non-terminal word read.
    ///
    /// Appended rather than inserted so every other offset above is unchanged —
    /// those are baked into emitted code and asserted in `context_layout`.
    pub exit_idx: u32,
    /// Guest instructions retired, accumulated **by the block at run time**.
    ///
    /// Previously `reconcile` applied a compile-time constant chosen from the
    /// block and its alternate-exit tuple. That works only while exactly one
    /// block runs per entry: a chain of linked blocks has no single constant,
    /// and the count decides how far the ARM9 advances against the ARM7, so
    /// getting it wrong leaves every register correct and silently shifts the
    /// interleave. Accumulating here is the primitive block linking needs.
    pub retired: u32,
    /// Opaque `&mut Arm9Cpu` for the MSR thunk, which must swap register
    /// banks on a mode change — state only the CPU owns. Same pointer
    /// contract as [`Self::mmu`]. Null until `try_step` arms it.
    pub cpu: *mut c_void,
    /// Statically-known cycles, accumulated **by the block at run time** when
    /// successor linking is compiled in.
    ///
    /// Without linking each block *returns* its own compile-time constant plus
    /// [`Self::extra_cycles`]; a chain has no single constant, so each linked
    /// block banks its share here and the epilogue returns the sum.
    pub cycles: u32,
    /// Non-zero when a chain must not continue past the next exit.
    ///
    /// Set by memory/status thunks — never by Rust between blocks — when an
    /// instruction did something the run loop or recompiler must observe at the
    /// next instruction boundary: an IPCSYNC write (`NdsMmu::ipc_yield`), a
    /// store that landed on a page holding compiled code, an unmasked pending
    /// interrupt, or an armed touch read-watch. Routing these through a flag
    /// keeps `NdsMmu`'s layout out of generated code.
    pub stop: u32,
    /// Cycle budget for the current run-loop slice. A linked exit compares
    /// `cycles + extra_cycles` against it and stands down once the slice is
    /// spent — the same comparison `Arm9Cpu::run`'s `while used < budget` makes.
    pub budget: u32,
    /// Start address of the block that **ended** the chain, written by every
    /// linked epilogue. `reconcile` looks that block's record up for the
    /// last-address and pipeline hand-off; without linking it is simply the
    /// entered block's own start.
    pub last_exit: u32,
    /// [`crate::nds::mmu::NdsMmu::code_write_epoch`] as of block entry. The
    /// store thunks compare the live value against this to detect a store that
    /// touched compiled code mid-chain.
    pub code_epoch: u64,
}

impl JitContext {
    /// Length of [`Self::words`], and the only definition of it.
    ///
    /// The `LDM`/`STM` thunks clamp their run length to
    /// `exec_mem::thunks::MAX_BLOCK_REGS` before building a slice over this
    /// buffer, so the two must agree — if this array shrank, the clamp would
    /// silently permit an out-of-bounds host write from a miscompiled register
    /// list. The `const` assertion below makes that a build error instead of a
    /// convention held across two modules.
    pub const WORDS: usize = 16;

    /// A context that reaches `gpr` and carries `cpsr`, with no MMU.
    pub fn new(gpr: *mut u32, cpsr: u32, exec_lr: u32) -> Self {
        Self::with_mmu(gpr, cpsr, exec_lr, std::ptr::null_mut())
    }

    /// The same, with the MMU a block needs in order to reach guest memory.
    pub fn with_mmu(gpr: *mut u32, cpsr: u32, exec_lr: u32, mmu: *mut c_void) -> Self {
        Self {
            gpr,
            cpsr,
            exec_lr,
            mmu,
            pc_modified: 0,
            class_hits: [0; 6],
            extra_cycles: 0,
            words: [0; Self::WORDS],
            exit_idx: 0,
            retired: 0,
            cpu: std::ptr::null_mut(),
            cycles: 0,
            stop: 0,
            budget: 0,
            last_exit: 0,
            code_epoch: 0,
        }
    }
}

/// Byte offset of [`JitContext::gpr`].
pub const CTX_GPR: i32 = 0;
/// Byte offset of [`JitContext::cpsr`].
pub const CTX_CPSR: i32 = 8;
/// Byte offset of [`JitContext::exec_lr`].
pub const CTX_EXEC_LR: i32 = 12;
/// Byte offset of [`JitContext::mmu`].
pub const CTX_MMU: i32 = 16;
/// Byte offset of [`JitContext::pc_modified`].
pub const CTX_PC_MODIFIED: i32 = 24;
/// Byte offset of [`JitContext::class_hits`]; bucket `n` is at `+ 4 * n`.
pub const CTX_CLASS_HITS: i32 = 28;
/// Byte offset of [`JitContext::extra_cycles`].
pub const CTX_EXTRA_CYCLES: i32 = 52;
/// Byte offset of [`JitContext::words`]; word `n` is at `+ 4 * n`.
pub const CTX_WORDS: i32 = 56;
/// See [`JitContext::exit_idx`]. Appended after the 64-byte staging buffer.
pub const CTX_EXIT_IDX: i32 = 120;
/// Offset of [`JitContext::retired`].
pub const CTX_RETIRED: i32 = 124;
/// Offset of [`JitContext::cpu`].
pub const CTX_CPU: i32 = 128;
/// Offset of [`JitContext::cycles`].
pub const CTX_CYCLES: i32 = 136;
/// Offset of [`JitContext::stop`].
pub const CTX_STOP: i32 = 140;
/// Offset of [`JitContext::budget`].
pub const CTX_BUDGET: i32 = 144;
/// Offset of [`JitContext::last_exit`].
pub const CTX_LAST_EXIT: i32 = 148;
/// Offset of [`JitContext::code_epoch`].
pub const CTX_CODE_EPOCH: i32 = 152;

/// Read IRQ exits carry the trace instruction index without allocating a
/// successor slot. The runner reconstructs their pipeline only when taken.
pub(crate) const READ_IRQ_EXIT_BASE: u32 = 3;

/// Which core a block is compiled for: selects the bus thunk set and the
/// ARMv4T/ARMv5TE semantic forks. Everything about it is baked at translation
/// time — the emitted code carries the chosen thunk addresses as immediates
/// and the chosen PC-write shape as instructions — so no run-time choice
/// exists to cost anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmitCfg {
    /// ARMv5TE (NDS ARM9). `false` = ARMv4T (NDS ARM7): a word load into PC
    /// forces bit 0 low and stays in state (`GbaCpu::load_pc`), and the
    /// BLX(reg) / Thumb BLX(reg) encodings do not exist.
    pub armv5: bool,
    /// Which core's bus the emitted thunk calls reach guest memory through.
    pub bus: thunks::BusKind,
}

impl EmitCfg {
    /// The NDS ARM9 (ARM946E-S, ARMv5TE).
    pub const ARM9: Self = Self { armv5: true, bus: thunks::BusKind::Arm9 };
    /// The NDS ARM7 (ARM7TDMI, ARMv4T).
    pub const ARM7: Self = Self { armv5: false, bus: thunks::BusKind::Arm7 };

    /// The thunk address a generated `call` targets for `access`.
    fn thunk(self, access: thunks::Access) -> usize {
        thunks::address(access, self.bus)
    }
}

/// `arm_class_hist` bucket for an instruction, matching the indices
/// `GbaCpu::execute_arm` uses: 0 = DP register, 1 = DP immediate,
/// 2 = single transfer, 3 = block transfer, 4 = branch, 5 = coprocessor/SWI.
fn hist_slot(bucket: usize) -> Mem {
    Mem::new(CTX, CTX_CLASS_HITS + (bucket as i32) * 4)
}

/// Byte offset of guest register `r` in the register file.
fn gpr_slot(r: u32) -> Mem {
    Mem::new(GPR_BASE, (r * 4) as i32)
}

/// A block that was successfully translated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    /// Machine code, ready for an [`crate::jit::exec_mem::CodeBuffer`].
    pub code: Vec<u8>,
    /// Guest instructions the code covers. May be fewer than the scanned body:
    /// the scanner decides what is *straight-line*, this decides what is
    /// *translatable*, and the second is the narrower question.
    pub instructions: usize,
    /// Cycles charged unconditionally: one per instruction, plus the extra cost
    /// of each *unconditional* instruction whose class costs more than one.
    ///
    /// A **sum**, not a count — data processing is one cycle and a single
    /// transfer is three. Conditional instructions contribute only their first
    /// cycle here and add the rest at run time through
    /// [`JitContext::extra_cycles`], because a failed condition costs 1 cycle
    /// whatever the class.
    pub static_cycles: u32,
    /// Upper bound on what one full run of the block can charge:
    /// [`Self::static_cycles`] plus every conditional instruction's taken-path
    /// extra. What a runner that must not overshoot a slice budget compares
    /// against before entering; see `JitCore::EXACT_SLICES`.
    pub worst_cycles: u32,
    /// Unconditional retirements per `arm_class_hist` bucket. The interpreter
    /// counts those unconditionally; conditional ones count themselves at run
    /// time through [`JitContext::class_hits`].
    ///
    /// Compacting this into a `(bucket, count)` list, to avoid sweeping six
    /// buckets on a path taken 14 million times per 60 ticks, **measured 0%**
    /// (2.73x -> 2.69x/2.67x, inside the within-session noise) and was reverted.
    pub unconditional_hist: [u64; 6],
    /// Thumb state. `execute_thumb` bumps `instrs` and **`thumb_instrs`** and
    /// never touches `arm_class_hist`, so a Thumb block must not contribute to
    /// the ARM histogram — the instruction-mix evidence would otherwise report
    /// Thumb work as ARM work.
    pub thumb: bool,
    /// The alternate exit installed by a followed conditional branch, if any.
    pub early_exit: Option<EarlyExit>,
    /// Code offset just past the prologue. A successor link jumps **here**:
    /// `GPR_BASE`, `CTX` and `CPSR` are already live across a link, so the
    /// pushes and reloads must be skipped — only the block that finally exits
    /// runs a pop/ret sequence, which is what keeps the stack balanced.
    pub body_entry: usize,
    /// Every exit the block can leave through, in emission order. Empty unless
    /// compiled with [`LinkSlots`].
    pub exits: Vec<CompiledExit>,
}

/// Highest number of distinct exits one block can have when linking is
/// compiled in: the main exit, the followed conditional branch's taken path,
/// and the taken path of a *terminal* conditional branch.
pub const MAX_EXIT_SLOTS: usize = 3;

/// Addresses of the per-exit successor slots, allocated by the runner **before**
/// translation so they can be baked into the code as immediates.
///
/// Each is the address of a stable `AtomicU64` holding where that exit's
/// `jmp [slot]` goes. Unlinked, the runner points it at the exit's own
/// epilogue, so behaviour is identical until a link is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkSlots {
    /// Indexed by [`CompiledExit::index`].
    pub addrs: [u64; MAX_EXIT_SLOTS],
}

/// One exit of a block compiled with successor linking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompiledExit {
    /// Which slot this exit jumps through: 0 = the block's main exit, 1 = the
    /// followed conditional branch's taken path, 2 = a terminal conditional
    /// branch's taken path. Also what the epilogue writes into
    /// [`JitContext::exit_idx`].
    pub index: u8,
    /// Guest address execution continues at, when it is a compile-time
    /// constant the runner may link to. `None` for an exit whose successor
    /// must be interpreted (a truncated translation), and for a dispatching
    /// exit, whose successor is a run-time value.
    pub target: Option<u32>,
    /// Did this exit's path write R15 (a taken branch)? The epilogue re-asserts
    /// `pc_modified` from this, because the linked path clears it — following a
    /// link *is* the pipeline flush a taken branch owes.
    pub sets_pc: bool,
    /// Code offset of this exit's own epilogue: the unlinked slot value.
    pub epilogue_offset: usize,
    /// Does this exit continue the chain through the **dispatch table** on the
    /// run-time R15 instead of a static slot? The runner installs table
    /// entries at such an exit's chain ends.
    pub dispatch: bool,
}

/// Where the indirect-dispatch exit probes: a direct-mapped table of
/// `(guest start, body entry)` pairs the runner maintains. Baked into the
/// code as immediates, so the table allocation must be stable for the life of
/// the recompiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchPlan {
    /// Base address of the `[DispatchSlot]` allocation (16-byte records:
    /// `u32` tag, pad, `u64` body pointer).
    pub table: u64,
    /// Slot-index mask; table length is `mask + 1` slots.
    pub mask: u32,
}

/// What `reconcile` needs when a block leaves through its alternate exit.
/// Every field is a compile-time constant at the exit point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EarlyExit {
    /// Cycles charged statically up to that point.
    pub cycles: u32,
    /// The branch's own address, which is what `arm9_exec_pc` publishes.
    ///
    /// The only field left: the retired count and the histogram used to be
    /// compile-time constants here too, and are now banked by the epilogue
    /// into [`JitContext`] so that a prefix of a block — and, later, a chain of
    /// them — reports what actually ran.
    pub last_addr: u32,
}

/// Translate as many leading instructions of `body` as are supported.
///
/// Returns `None` when the first instruction is untranslatable, so the caller
/// interprets one and tries again — which is what guarantees forward progress
/// however narrow the supported subset is.
///
/// `start` is the guest address of `body[0]`.
pub fn compile(body: &[(u32, u32)], thumb: bool) -> Option<Compiled> {
    compile_with(body, thumb, None, None, EmitCfg::ARM9)
}

/// [`compile`], optionally emitting **successor-linked** exits.
///
/// With `link` present, every exit banks its cycle/retired/histogram share into
/// the context, checks the chain-break conditions and jumps through its slot in
/// `link` instead of returning; the slot initially points at the exit's own
/// epilogue, so behaviour is identical until the runner writes a link. Without
/// `link` the emission is byte-identical to what it always was, which is what
/// makes `EMU_ARM9_JIT_LINK` an honest single-binary A/B.
pub fn compile_with(
    body: &[(u32, u32)],
    thumb: bool,
    link: Option<&LinkSlots>,
    dispatch: Option<&DispatchPlan>,
    cfg: EmitCfg,
) -> Option<Compiled> {
    let allow_exchange = dispatch.is_some();
    let decoded: Vec<Instr> = if thumb {
        // Manual loop rather than `map_while`: the F19 `BL` pair decodes as a
        // unit (two body entries -> `TBl` + `TBlPad`, keeping addresses 1:1),
        // which per-element iteration cannot express.
        let mut out = Vec::with_capacity(body.len());
        let mut i = 0;
        while i < body.len() {
            let inst = body[i].1 as u16;
            let w = u32::from(inst);
            if (w & 0xF800) == 0xE000 || ((w & 0xF000) == 0xD000 && (w >> 8) & 0xF <= 0xD) {
                let cond = if (w & 0xF800) == 0xE000 {
                    ArmCond::ALWAYS
                } else {
                    ArmCond((w >> 8) & 0xF)
                };
                let delta = crate::jit::block::thumb_branch_target(body[i].0, inst)
                    .wrapping_sub(body[i].0.wrapping_add(4)) as i32;
                out.push(Instr::TBranch(ThumbBranch { cond, delta }));
                i += 1;
                continue;
            }
            if (w & 0xF800) == 0xF000
                && i + 1 < body.len()
                && (body[i + 1].1 & 0xF800) == 0xF800
            {
                let target =
                    crate::jit::block::thumb_bl_target(body[i].0, inst, body[i + 1].1 as u16);
                out.push(Instr::TBl(ThumbBl { target }));
                out.push(Instr::TBlPad(ThumbBl { target }));
                i += 2;
                continue;
            }
            // The dispatching terminators, only in dispatch mode (the scanner
            // only forms them there too).
            if allow_exchange && (w & 0xFF00) == 0x4700 {
                let link = (w & 0x80) != 0;
                if link && !cfg.armv5 {
                    break; // Thumb BLX(reg) is ARMv5T-only; truncate = decline
                }
                out.push(Instr::TBx { link, rs: (w >> 3) & 0xF });
                i += 1;
                continue;
            }
            if allow_exchange && (w & 0xFF00) == 0xBD00 {
                out.push(Instr::TPop { list: (w & 0xFF) as u8 });
                i += 1;
                continue;
            }
            match decode_thumb(inst) {
                Some(d) => out.push(d),
                None => break,
            }
            i += 1;
        }
        out
    } else {
        body.iter()
            .map_while(|&(_, inst)| decode_dispatch(inst, allow_exchange, cfg.armv5))
            .collect()
    };
    if decoded.is_empty() {
        return None;
    }
    // Addresses come from the body, not from `start` plus an index: a trace
    // follows unconditional branches, so execution order is not address order.
    let addrs: Vec<u32> = body[..decoded.len()].iter().map(|&(a, _)| a).collect();
    let start = addrs[0];

    let mut e = Emitter::new();
    emit_prologue(&mut e);
    // Where a successor link lands: past the prologue, with `GPR_BASE`, `CTX`
    // and `CPSR` already live from the block that jumped here.
    let body_entry = e.len();

    // R15 as the interpreter would leave it if the last instruction does not
    // branch: past that instruction, still leading by two instruction widths.
    // R15 leads the executing instruction by two instruction widths in both
    // states — which is 8 bytes in ARM and 4 in Thumb.
    let step = if thumb { 2 } else { 4 };
    let lead = step * 2;
    let final_pc = addrs[decoded.len() - 1].wrapping_add(step).wrapping_add(lead);

    let mut unconditional_hist = [0u64; 6];
    let mut early_exit: Option<EarlyExit> = None;
    let mut exits: Vec<CompiledExit> = Vec::new();
    let mut cycles = 0u32;
    // The conditional instructions' taken-path extras, summed — the run-time
    // ceiling on top of the static cycles. See `Compiled::worst_cycles`.
    let mut worst_extra = 0u32;
    for (i, op) in decoded.iter().enumerate() {
        let terminal = i + 1 == decoded.len();
        let read_irq_exit = cfg.armv5 && op.reads_word_memory() && !terminal;
        if terminal || read_irq_exit {
            // The interpreter publishes R14 before executing an instruction, so
            // the value the *last* one saw is what must survive. Captured here
            // rather than in the epilogue because that instruction may itself
            // write R14, and the pre-write value is the one published.
            e.mov_rm(FLAGS, gpr_slot(14));
            e.mov_mr(Mem::new(CTX, CTX_EXEC_LR), FLAGS);
            // R15 is written *before* the last instruction rather than in the
            // epilogue, so a taken branch simply overwrites it and the
            // not-taken path needs no fix-up. Safe for every other instruction
            // because the scanner excludes anything that writes R15, and R15 as
            // a *source* is materialised as a constant, never read from here.
            if terminal {
                e.mov_mi(gpr_slot(15), final_pc);
            }
        }

        // R15 reads as the executing address plus eight.
        let pc = addrs[i].wrapping_add(lead);
        // Only the final instruction may redirect execution. A branch anywhere
        // else in a trace was *followed* by the scanner, so the instruction
        // after it already is its target.
        if op.cond() == ArmCond::ALWAYS {
            cycles += op.cycles();
            unconditional_hist[op.hist_bucket()] += 1;
            emit_instr(&mut e, op, pc, terminal, cfg);
        } else {
            // One cycle is charged whether or not the condition passes; the rest
            // is charged inside the taken path. See `JitContext::extra_cycles`.
            cycles += 1;
            worst_extra += op.cycles().saturating_sub(1);
            let skips = emit_condition_fails(&mut e, op.cond());
            // `execute_thumb` never touches `arm_class_hist`; bumping it here
            // would report Thumb work as ARM work.
            if !thumb {
                e.inc_m(hist_slot(op.hist_bucket()));
            }
            if op.cycles() > 1 {
                e.alu_mi(
                    AluOp::Add,
                    Mem::new(CTX, CTX_EXTRA_CYCLES),
                    op.cycles() - 1,
                );
            }
            // **A non-terminal conditional branch is an early exit.** The
            // scanner followed its fall-through, so the taken path leaves the
            // block here: write R15 and `pc_modified` exactly as a terminal
            // branch does, record which exit was taken, and return with the
            // cycles accumulated *so far* rather than the block's full total.
            //
            // Everything the caller needs is a compile-time constant at this
            // point, which is why one `mov` suffices: `reconcile` looks the
            // retired count, the histogram and the last address up from the
            // block's alternate-exit tuple.
            // The taken target of a conditional direct branch, when this
            // instruction is one — ARM and Thumb spell it differently but
            // both are compile-time constants.
            let taken_target = match op {
                Instr::Branch(b) => Some(pc.wrapping_add((b.offset << 2) as u32)),
                Instr::TBranch(t) => Some((pc as i32).wrapping_add(t.delta) as u32),
                _ => None,
            };
            let early = !terminal && taken_target.is_some();
            if early {
                emit_branch_taken_exit(&mut e, op, pc);
                early_exit = Some(EarlyExit { cycles, last_addr: addrs[i] });
                let hist = hist_for(thumb, &unconditional_hist);
                match link {
                    Some(slots) => exits.push(emit_linked_exit(
                        &mut e,
                        slots,
                        ExitShape {
                            index: 1,
                            target: taken_target,
                            sets_pc: true,
                            cycles,
                            retired: i + 1,
                            start,
                        },
                        &hist,
                    )),
                    None => emit_epilogue(&mut e, cycles, i + 1, &hist),
                }
            } else {
                emit_instr(&mut e, op, pc, terminal, cfg);
                // A **terminal conditional branch** has two live successors,
                // both compile-time constants. With linking, the taken path
                // leaves through its own slot right here — one shared slot
                // would link whichever successor happened to run last — and
                // the fall-through joins the main exit after the patch below.
                if let (true, Some(slots), Some(target)) = (terminal, link, taken_target) {
                    exits.push(emit_linked_exit(
                        &mut e,
                        slots,
                        ExitShape {
                            index: 2,
                            target: Some(target),
                            sets_pc: true,
                            cycles,
                            retired: decoded.len(),
                            start,
                        },
                        &hist_for(thumb, &unconditional_hist),
                    ));
                }
            }
            for site in skips {
                e.patch_rel32(site);
            }
        }
        if read_irq_exit {
            // A read of the last Gamecard word can raise an IRQ in the middle
            // of a trace. Keep normal loads in the trace; take this cold exit
            // only after the complete instruction, including LDM writeback.
            e.alu_mi(AluOp::Cmp, Mem::new(CTX, CTX_STOP), 0);
            let no_irq = e.jcc_placeholder(Cond::E);
            e.mov_mi(gpr_slot(15), pc.wrapping_add(step));
            let index = (READ_IRQ_EXIT_BASE + i as u32) as u8;
            let hist = hist_for(thumb, &unconditional_hist);
            match link {
                Some(slots) => {
                    // No target means this exit returns directly; it has no
                    // slot and can never bypass the interpreter's IRQ poll.
                    emit_linked_exit(
                        &mut e,
                        slots,
                        ExitShape {
                            index,
                            target: None,
                            sets_pc: false,
                            cycles,
                            retired: i + 1,
                            start,
                        },
                        &hist,
                    );
                }
                None => {
                    e.mov_mi(Mem::new(CTX, CTX_EXIT_IDX), u32::from(index));
                    emit_epilogue(&mut e, cycles, i + 1, &hist);
                }
            }
            e.patch_rel32(no_irq);
        }
    }

    let hist = hist_for(thumb, &unconditional_hist);
    match link {
        None => emit_epilogue(&mut e, cycles, decoded.len(), &hist),
        Some(slots) => {
            let last = &decoded[decoded.len() - 1];
            let last_addr = addrs[decoded.len() - 1];
            let shape = |target, sets_pc| ExitShape {
                index: 0,
                target,
                sets_pc,
                cycles,
                retired: decoded.len(),
                start,
            };
            if last.is_runtime_branch() {
                // BX/BLX(reg) or LDM {..,pc}: R15 holds a run-time value, so
                // the exit probes the dispatch table on it instead of a slot.
                let plan =
                    dispatch.expect("run-time branches decode only with a dispatch plan");
                exits.push(emit_dispatch_exit(&mut e, plan, shape(None, true), &hist));
            } else {
                // Where execution continues when the block runs to its end. An
                // unconditional terminal branch redirected R15; anything else
                // falls through — but only a fully-translated body has a
                // fall-through the runner may link (a truncated one stops
                // before an instruction the interpreter must run).
                let (target, sets_pc) = match last {
                    Instr::Branch(b) if last.cond() == ArmCond::ALWAYS => (
                        Some(last_addr.wrapping_add(lead).wrapping_add((b.offset << 2) as u32)),
                        true,
                    ),
                    Instr::TBranch(t) if t.cond == ArmCond::ALWAYS => (
                        Some(
                            (last_addr.wrapping_add(lead) as i32).wrapping_add(t.delta) as u32,
                        ),
                        true,
                    ),
                    Instr::TBlPad(bl) => (Some(bl.target), true),
                    _ => {
                        let full = decoded.len() == body.len();
                        (full.then(|| last_addr.wrapping_add(step)), false)
                    }
                };
                exits.push(emit_linked_exit(&mut e, slots, shape(target, sets_pc), &hist));
            }
        }
    }

    Some(Compiled {
        code: e.into_vec(),
        instructions: decoded.len(),
        static_cycles: cycles,
        worst_cycles: cycles + worst_extra,
        unconditional_hist: hist,
        thumb,
        early_exit,
        body_entry,
        exits,
    })
}

fn emit_instr(e: &mut Emitter, op: &Instr, pc: u32, terminal: bool, cfg: EmitCfg) {
    match op {
        Instr::DataProc(d) => emit_data_proc_imm(e, *d, pc),
        Instr::Transfer(t) => emit_transfer(e, *t, pc, cfg),
        Instr::Half(h) => emit_half(e, *h, pc, cfg),
        Instr::Mul(m) => emit_mul(e, *m),
        Instr::Clz(c) => emit_clz(e, *c),
        // Architecturally nothing: one cycle, already charged statically.
        Instr::CopNop => {}
        Instr::Branch(b) => emit_branch(e, *b, pc, terminal),
        Instr::Block(b) => emit_block_transfer(e, *b, pc, cfg),
        Instr::Exchange(x) => {
            // The scanner only forms these as the last instruction of a body.
            debug_assert!(terminal, "BX/BLX(reg) mid-body");
            emit_exchange(e, *x, pc);
        }
        // One host mov: CPSR lives in a pinned register for the block's life.
        // The SPSR is state only the CPU owns, read through a thunk.
        Instr::Mrs(m) => {
            if m.spsr {
                emit_thunk_call(e, cfg.thunk(thunks::Access::MrsSpsr), true);
                e.mov_mr(gpr_slot(m.rd), Reg::Rax);
            } else {
                e.mov_mr(gpr_slot(m.rd), CPSR);
            }
        }
        Instr::Msr(m) => {
            debug_assert!(terminal, "MSR mid-body");
            emit_msr(e, *m, pc, cfg);
        }
        // A followed Thumb branch is fully absorbed into the trace (the next
        // body entry IS its target); only a terminal one redirects R15.
        Instr::TBranch(t) => {
            if terminal {
                e.mov_mi(gpr_slot(15), (pc as i32).wrapping_add(t.delta) as u32);
                e.mov_mi(Mem::new(CTX, CTX_PC_MODIFIED), 1);
            }
        }
        // The F19 pair's net link value; the interpreter's intermediate
        // LR (half one's PC-relative scratch) is unobservable inside a block.
        Instr::TBl(_) => e.mov_mi(gpr_slot(14), pc | 1),
        Instr::TBlPad(bl) => {
            if terminal {
                e.mov_mi(gpr_slot(15), bl.target);
                e.mov_mi(Mem::new(CTX, CTX_PC_MODIFIED), 1);
            }
        }
        Instr::TBx { link, rs } => {
            debug_assert!(terminal, "thumb BX mid-body");
            // The target register is read BEFORE the link — `BLX lr` must
            // branch to the old LR (`thumb_hi_reg` captures `b` up front).
            if *rs == 15 {
                e.mov_ri(VALUE, pc);
            } else {
                e.mov_rm(VALUE, gpr_slot(*rs));
            }
            if *link {
                e.mov_mi(gpr_slot(14), pc.wrapping_sub(2) | 1);
            }
            emit_pc_write_interworking(e, VALUE);
        }
        Instr::TPop { list } => {
            debug_assert!(terminal, "thumb POP pc mid-body");
            emit_thumb_pop_pc(e, *list, cfg);
        }
    }
}

/// Emit `POP {list, pc}`, mirroring `thumb_push_pop` exactly: one rotating
/// word read per register (the same bus path the interpreter takes, so an
/// unaligned SP behaves identically), SP written back **before** the PC, and
/// the popped value written through the ARMv5 interworking rule. SP itself
/// cannot appear in `list` (it is not encodable), so re-reading the base
/// from the register file after each thunk call is exact.
fn emit_thumb_pop_pc(e: &mut Emitter, list: u8, cfg: EmitCfg) {
    let regs: Vec<u32> = (0..8u32).filter(|r| (list >> r) & 1 != 0).collect();
    for (k, &r) in regs.iter().enumerate() {
        e.mov_rm(VALUE, gpr_slot(13));
        if k > 0 {
            e.alu_ri(AluOp::Add, VALUE, (k as u32) * 4);
        }
        emit_thunk_call(e, cfg.thunk(thunks::Access::ReadWord), true);
        e.mov_mr(gpr_slot(r), Reg::Rax);
    }
    let n = regs.len() as u32;
    e.mov_rm(VALUE, gpr_slot(13));
    if n > 0 {
        e.alu_ri(AluOp::Add, VALUE, n * 4);
    }
    emit_thunk_call(e, cfg.thunk(thunks::Access::ReadWord), true);
    // SP before PC, the interpreter's order.
    e.mov_rm(CARRY, gpr_slot(13));
    e.alu_ri(AluOp::Add, CARRY, (n + 1) * 4);
    e.mov_mr(gpr_slot(13), CARRY);
    emit_pc_write_load(e, Reg::Rax, cfg);
}

/// Emit `MSR CPSR_fields, op` as a thunk call: `(ctx, operand, mask, cpsr)`
/// in, the committed CPSR out. The thunk performs the field merge, the
/// T-preservation, the bank swap a mode change requires, and raises
/// `JitContext::stop` when the write unmasked a pending interrupt — the block
/// ends here, so that flag is honoured before the next guest instruction.
fn emit_msr(e: &mut Emitter, m: Msr, pc: u32, cfg: EmitCfg) {
    match m.operand {
        MsrOperand::Imm(v) => e.mov_ri(VALUE, v),
        MsrOperand::Reg(15) => e.mov_ri(VALUE, pc),
        MsrOperand::Reg(rm) => e.mov_rm(VALUE, gpr_slot(rm)),
    }
    e.mov_ri(Reg::R8, m.mask);
    e.mov_rr(Reg::R9, CPSR);
    emit_thunk_call(e, cfg.thunk(thunks::Access::MsrCpsr), true);
    e.mov_rr(CPSR, Reg::Rax);
}

/// ARM's T flag in CPSR: bit 5.
const T_BIT: u32 = 1 << 5;

/// Emit MUL/MLA or a long multiply, mirroring `arm_multiply` /
/// `arm_multiply_long` exactly: operands read before any write, RdLo stored
/// before RdHi, S sets N and Z only.
///
/// One `imul r64` serves every form: 32-bit operands zero- or sign-extended
/// into 64 bits produce the exact 64-bit product, whose low half is the
/// 32-bit result.
fn emit_mul(e: &mut Emitter, m: Multiply) {
    e.mov_rm(VALUE, gpr_slot(m.rm)); // 32-bit loads zero-extend to 64
    e.mov_rm(OP2, gpr_slot(m.rs));
    match m.long {
        Some(signed) => {
            if signed {
                e.movsxd_rr(VALUE, VALUE);
                e.movsxd_rr(OP2, OP2);
            }
            e.imul_rr_64(VALUE, OP2);
            if m.accumulate {
                // The existing RdHi:RdLo pair, as one 64-bit accumulator.
                e.mov_rm(PACK, gpr_slot(m.rn));
                e.mov_rm(PACK2, gpr_slot(m.rd));
                e.shift_ri_64(ShiftOp::Shl, PACK2, 32);
                e.alu_rr_64(AluOp::Or, PACK, PACK2);
                e.alu_rr_64(AluOp::Add, VALUE, PACK);
            }
            e.mov_mr(gpr_slot(m.rn), VALUE); // RdLo first, the interpreter's order
            e.mov_rr_64(PACK, VALUE);
            e.shift_ri_64(ShiftOp::Shr, PACK, 32);
            e.mov_mr(gpr_slot(m.rd), PACK);
            if m.set_flags {
                // N = bit 63, Z = the whole 64-bit result — exactly
                // `arm_multiply_long`'s S rule.
                e.test_rr_64(VALUE, VALUE);
                emit_nz_flags(e);
            }
        }
        None => {
            e.imul_rr_64(VALUE, OP2); // low 32 bits == the ARM result
            if m.accumulate {
                e.mov_rm(OP2, gpr_slot(m.rn));
                e.alu_rr(AluOp::Add, VALUE, OP2);
            }
            e.mov_mr(gpr_slot(m.rd), VALUE);
            if m.set_flags {
                // Sign-extend so bit 63 mirrors bit 31: one 64-bit `test`
                // then yields the 32-bit N and Z.
                e.movsxd_rr(VALUE, VALUE);
                e.test_rr_64(VALUE, VALUE);
                emit_nz_flags(e);
            }
        }
    }
}

/// Emit `CLZ Rd, Rm`: `31 - bsr`, with the zero case (`CLZ(0) == 32`)
/// branched around because `bsr` leaves its destination undefined there.
/// No flags — `execute_arm_reg_class`'s CLZ writes the register and nothing
/// else.
fn emit_clz(e: &mut Emitter, c: Clz) {
    e.mov_rm(VALUE, gpr_slot(c.rm));
    e.mov_ri(PACK, 32); // the CLZ(0) answer
    e.bsr_rr(OP2, VALUE); // ZF says the source was zero
    let zero = e.jcc_placeholder(Cond::E);
    e.mov_ri(PACK, 31);
    e.alu_rr(AluOp::Sub, PACK, OP2);
    e.patch_rel32(zero);
    e.mov_mr(gpr_slot(c.rd), PACK);
}

/// Merge N and Z from the flags the last operation left into CPSR, leaving C
/// **and** V alone — `set_nz`, the multiply family's S rule.
fn emit_nz_flags(e: &mut Emitter) {
    e.lahf(); // ah: bit 7 = SF, bit 6 = ZF -> eax bits 15, 14
    e.mov_rr(PACK, FLAGS);
    e.alu_ri(AluOp::And, PACK, 0x0000_C000);
    e.shift_ri(ShiftOp::Shl, PACK, 16); // -> bits 31, 30
    e.alu_ri(AluOp::And, CPSR, !(N | Z));
    e.alu_rr(AluOp::Or, CPSR, PACK);
}

/// The ARMv5 interworking PC write (`GbaCpu::write_pc` with
/// `thumb_from_bit0`): T := bit 0 of `value`, R15 := `value` **verbatim** —
/// the interpreter does not mask it — and the pipeline is owed a refill.
///
/// Clobbers [`PACK`].
fn emit_pc_write_interworking(e: &mut Emitter, value: Reg) {
    e.mov_rr(PACK, value);
    e.alu_ri(AluOp::And, PACK, 1);
    e.shift_ri(ShiftOp::Shl, PACK, 5); // bit 0 -> CPSR's T position
    e.alu_ri(AluOp::And, CPSR, !T_BIT);
    e.alu_rr(AluOp::Or, CPSR, PACK);
    e.mov_mr(gpr_slot(15), value);
    e.mov_mi(Mem::new(CTX, CTX_PC_MODIFIED), 1);
}

/// The PC write a **word load** into R15 performs (`GbaCpu::load_pc`): on
/// ARMv5T it is the interworking write above; on ARMv4T there is no
/// interworking — bit 0 is forced low and the state is untouched. Data
/// processing writes to PC never route here on either architecture.
///
/// Clobbers `value` on the ARMv4T path and [`PACK`] on the ARMv5T one.
fn emit_pc_write_load(e: &mut Emitter, value: Reg, cfg: EmitCfg) {
    if cfg.armv5 {
        emit_pc_write_interworking(e, value);
    } else {
        e.alu_ri(AluOp::And, value, !1);
        e.mov_mr(gpr_slot(15), value);
        e.mov_mi(Mem::new(CTX, CTX_PC_MODIFIED), 1);
    }
}

/// Emit `BX Rm` / `BLX Rm`. Matches `execute_arm_reg_class`: the target is
/// read **before** `BLX` links, so `BLX lr` branches to the old LR; the link
/// value is `R15 - 4`, a compile-time constant inside a block.
fn emit_exchange(e: &mut Emitter, x: Exchange, pc: u32) {
    if x.rm == 15 {
        e.mov_ri(VALUE, pc);
    } else {
        e.mov_rm(VALUE, gpr_slot(x.rm));
    }
    if x.link {
        e.mov_mi(gpr_slot(14), pc.wrapping_sub(4));
    }
    emit_pc_write_interworking(e, VALUE);
}

/// Emit `LDM`/`STM`.
///
/// # Shape
///
/// The register list is constant, so the transfer size, every slot offset and
/// the cycle count are baked in; only the base is a run-time value. The words
/// move through [`JitContext::words`] so the thunk can do **one** bus decode for
/// the whole list, which is the same reason the interpreter grew
/// `CpuBus::read_words`.
///
/// # The three rules that are easy to get wrong
///
/// * the lowest-numbered register always takes the **lowest address**, whatever
///   the direction bit says;
/// * `up == pre` means the first access is offset by a word (the `IB`/`DB`
///   forms), which is why the address fix-up is a comparison and not a constant;
/// * an `STM` that stores its own base **after** the first slot stores the
///   *written-back* value, not the register — the interpreter's `first` rule.
fn emit_block_transfer(e: &mut Emitter, b: BlockTransfer, pc: u32, cfg: EmitCfg) {
    let count = b.count();
    let span = b.span();

    // `final_base` is what writeback leaves, and for the descending forms it is
    // also the transfer's start address.
    let base_to_final = |e: &mut Emitter, reg: Reg| {
        if b.up {
            e.alu_ri(AluOp::Add, reg, span);
        } else {
            e.alu_ri(AluOp::Sub, reg, span);
        }
    };

    // An STM that has to store the written-back base needs it before the gather.
    let stores_written_back_base = !b.load
        && b.writeback
        && (b.list >> b.rn) & 1 != 0
        && (b.list & ((1u16 << b.rn) - 1)) != 0;
    if stores_written_back_base {
        e.mov_rm(CARRY, gpr_slot(b.rn));
        base_to_final(e, CARRY);
    }

    if !b.load {
        // Gather the values first: a store cannot change a register, so this is
        // equivalent to interleaving and keeps the `first` rule intact.
        for (slot, r) in (0..16u32).filter(|r| (b.list >> r) & 1 != 0).enumerate() {
            let dst = Mem::new(CTX, CTX_WORDS + (slot as i32) * 4);
            if r == 15 {
                e.mov_mi(dst, pc.wrapping_add(4)); // STM stores PC + 12
            } else if r == b.rn && stores_written_back_base && slot > 0 {
                e.mov_mr(dst, CARRY);
            } else {
                e.mov_rm(FLAGS, gpr_slot(r));
                e.mov_mr(dst, FLAGS);
            }
        }
    }

    // Address, into the thunk's second argument.
    e.mov_rm(VALUE, gpr_slot(b.rn));
    if !b.up {
        e.alu_ri(AluOp::Sub, VALUE, span);
    }
    if b.up == b.pre {
        e.alu_ri(AluOp::Add, VALUE, 4);
    }

    e.lea_64(Reg::R8, Mem::new(CTX, CTX_WORDS));
    e.mov_ri(Reg::R9, count);
    emit_thunk_call(
        e,
        cfg.thunk(if b.load { thunks::Access::ReadWords } else { thunks::Access::WriteWords }),
        true,
    );

    if b.load {
        // Distribute, in list order — R15, when the dispatch scanner admitted
        // it, is bit 15 and therefore last, exactly the order the interpreter
        // loads it in. The PC write is `GbaCpu::load_pc`: interworking on
        // ARMv5, bit 0 forced low on ARMv4T — see `emit_pc_write_load`.
        for (slot, r) in (0..16u32).filter(|r| (b.list >> r) & 1 != 0).enumerate() {
            e.mov_rm(FLAGS, Mem::new(CTX, CTX_WORDS + (slot as i32) * 4));
            if r == 15 {
                emit_pc_write_load(e, FLAGS, cfg);
            } else {
                e.mov_mr(gpr_slot(r), FLAGS);
            }
        }
    }

    if b.writes_base() {
        // Recomputed from the register file: a load that would have clobbered
        // the base skips writeback entirely, so this reads the original value.
        e.mov_rm(FLAGS, gpr_slot(b.rn));
        base_to_final(e, FLAGS);
        e.mov_mr(gpr_slot(b.rn), FLAGS);
    }
}

/// Emit a direct `B`/`BL`.
///
/// Everything is a constant: `pc` is the executing address plus eight, so the
/// target is `pc + (offset << 2)` and `BL`'s link value is `pc - 4`. Three
/// stores at most, against a full interpreter dispatch that this replaces.
///
/// R15 has already been written with the fall-through address by the caller
/// (see `compile`), so a taken branch simply overwrites it — no conditional
/// fix-up is needed for the not-taken path.
/// The taken path of a conditional branch the scanner followed the
/// fall-through of: architecturally identical to a terminal branch, plus the
/// exit marker `reconcile` reads.
///
/// `CTX_EXIT_IDX` is 0 when a block runs to its end, so only this path writes
/// it and the common case pays nothing.
fn emit_branch_taken_exit(e: &mut Emitter, op: &Instr, pc: u32) {
    let target = match op {
        Instr::Branch(b) => {
            if b.link {
                e.mov_mi(gpr_slot(14), pc.wrapping_sub(4));
            }
            pc.wrapping_add((b.offset << 2) as u32)
        }
        Instr::TBranch(t) => (pc as i32).wrapping_add(t.delta) as u32,
        _ => return, // caller checked; nothing else can reach here
    };
    e.mov_mi(gpr_slot(15), target);
    e.mov_mi(Mem::new(CTX, CTX_PC_MODIFIED), 1);
    e.mov_mi(Mem::new(CTX, CTX_EXIT_IDX), 1);
}

fn emit_branch(e: &mut Emitter, b: DirectBranch, pc: u32, terminal: bool) {
    // `BL` links wherever it sits: the link register is architectural state the
    // trace cannot skip.
    if b.link {
        e.mov_mi(gpr_slot(14), pc.wrapping_sub(4));
    }
    if !terminal {
        // Followed by the scanner. The instruction after this one *is* the
        // target, so redirecting R15 would be writing a value that the next
        // instruction's own constant immediately supersedes — and setting
        // `pc_modified` would make the caller refill the pipeline in the middle
        // of a trace. Only the link write is architecturally visible here.
        return;
    }
    e.mov_mi(gpr_slot(15), pc.wrapping_add((b.offset << 2) as u32));
    // `pc_modified`: the next step must refill the pipeline rather than advance
    // R15. Only the block knows whether a conditional branch was taken.
    e.mov_mi(Mem::new(CTX, CTX_PC_MODIFIED), 1);
}

/// Save the callee-saved registers this block pins, and load them.
///
/// **Three** pushes, which is not arbitrary. Win64 guarantees `rsp % 16 == 8` at
/// function entry, so three 8-byte pushes bring it back to `0` — the alignment a
/// `call` requires. The memory-thunk milestone therefore only has to add the
/// 32-byte shadow space, not repair the alignment. A fourth pinned register
/// would break this.
/// The histogram a block may contribute, which is none of it in Thumb state.
///
/// One definition, used by both epilogue sites and by [`Compiled`], so the
/// Thumb rule cannot be applied in one place and forgotten in the other.
fn hist_for(thumb: bool, hist: &[u64; 6]) -> [u64; 6] {
    if thumb {
        [0; 6]
    } else {
        *hist
    }
}

fn emit_prologue(e: &mut Emitter) {
    e.push_64(GPR_BASE);
    e.push_64(CTX);
    e.push_64(CPSR);
    e.mov_rr_64(CTX, Reg::Rcx); // first Win64 integer argument
    e.mov_rm_64(GPR_BASE, Mem::new(CTX, CTX_GPR));
    e.mov_rm(CPSR, Mem::new(CTX, CTX_CPSR));
}

/// Close a block: publish CPSR, bank the run-time tallies, return cycles.
///
/// `retired` and `hist` are accumulated into the context rather than returned,
/// because a linked chain of blocks has no single compile-time constant for
/// either. `hist` is already empty for a Thumb block — `execute_thumb` never
/// touches `arm_class_hist`, so contributing to it would report Thumb work as
/// ARM work in the very evidence that decides what to optimise next.
fn emit_epilogue(e: &mut Emitter, static_cycles: u32, retired: usize, hist: &[u64; 6]) {
    e.mov_mr(Mem::new(CTX, CTX_CPSR), CPSR);
    e.alu_mi(AluOp::Add, Mem::new(CTX, CTX_RETIRED), retired as u32);
    for (bucket, &count) in hist.iter().enumerate() {
        if count != 0 {
            let slot = CTX_CLASS_HITS + 4 * bucket as i32;
            e.alu_mi(AluOp::Add, Mem::new(CTX, slot), count as u32);
        }
    }
    // Win64 integer return value: the statically known cycles plus whatever the
    // conditional instructions that actually ran contributed.
    e.mov_ri(Reg::Rax, static_cycles);
    e.alu_rm(AluOp::Add, Reg::Rax, Mem::new(CTX, CTX_EXTRA_CYCLES));
    e.pop_64(CPSR);
    e.pop_64(CTX);
    e.pop_64(GPR_BASE);
    e.ret();
}

/// Everything one linked exit needs baked in. All compile-time constants.
struct ExitShape {
    index: u8,
    target: Option<u32>,
    sets_pc: bool,
    /// Statically-known cycles accumulated up to this exit.
    cycles: u32,
    /// Instructions retired when leaving through this exit.
    retired: usize,
    /// The block's own start address, for [`JitContext::last_exit`].
    start: u32,
}

/// Close one exit of a **linked** block: bank the run-time tallies, then either
/// continue the chain through this exit's slot or fall into a full epilogue.
///
/// Shape, in emission order:
///
/// 1. bank `cycles`, `retired` and the histogram share for the path that ran;
/// 2. (linkable exits only) load `cycles + extra_cycles`, stand down to the
///    epilogue when the slice budget is spent or a store thunk raised
///    [`JitContext::stop`] — the checks come **before** the jump, so a chain
///    can never cross an IPCSYNC write, a code-page store, or the budget;
/// 3. clear `pc_modified` — following a link *is* the pipeline flush a taken
///    branch owes — and `jmp [slot]`;
/// 4. the epilogue (also the slot's unlinked target): re-assert `pc_modified`
///    for this exit's own path, record which block and exit ended the chain,
///    publish CPSR, return the banked cycle total.
///
/// The epilogue re-asserts `pc_modified` because an **unlinked** slot points
/// here after step 3 already cleared it; a per-exit constant restores the
/// truth either way.
fn emit_linked_exit(
    e: &mut Emitter,
    slots: &LinkSlots,
    shape: ExitShape,
    hist: &[u64; 6],
) -> CompiledExit {
    e.alu_mi(AluOp::Add, Mem::new(CTX, CTX_CYCLES), shape.cycles);
    e.alu_mi(AluOp::Add, Mem::new(CTX, CTX_RETIRED), shape.retired as u32);
    for (bucket, &count) in hist.iter().enumerate() {
        if count != 0 {
            let slot = CTX_CLASS_HITS + 4 * bucket as i32;
            e.alu_mi(AluOp::Add, Mem::new(CTX, slot), count as u32);
        }
    }
    if shape.target.is_some() {
        // Total cycles this chain has consumed, `while used < budget`'s term.
        e.mov_rm(FLAGS, Mem::new(CTX, CTX_CYCLES));
        e.alu_rm(AluOp::Add, FLAGS, Mem::new(CTX, CTX_EXTRA_CYCLES));
        e.alu_rm(AluOp::Cmp, FLAGS, Mem::new(CTX, CTX_BUDGET));
        let budget_spent = e.jcc_placeholder(Cond::Ae);
        e.alu_mi(AluOp::Cmp, Mem::new(CTX, CTX_STOP), 0);
        let store_stopped = e.jcc_placeholder(Cond::Ne);
        e.mov_mi(Mem::new(CTX, CTX_PC_MODIFIED), 0);
        e.mov_ri_64(OP2, slots.addrs[usize::from(shape.index)]);
        e.jmp_m(Mem::new(OP2, 0));
        e.patch_rel32(budget_spent);
        e.patch_rel32(store_stopped);
    }
    let epilogue_offset = emit_exit_epilogue(e, &shape);
    CompiledExit {
        index: shape.index,
        target: shape.target,
        sets_pc: shape.sets_pc,
        epilogue_offset,
        dispatch: false,
    }
}

/// The shared per-exit epilogue: re-assert this exit's own truth, publish
/// CPSR, return the banked cycle total. Returns its code offset — the
/// unlinked value for this exit's slot.
fn emit_exit_epilogue(e: &mut Emitter, shape: &ExitShape) -> usize {
    let epilogue_offset = e.len();
    e.mov_mi(Mem::new(CTX, CTX_PC_MODIFIED), u32::from(shape.sets_pc));
    e.mov_mi(Mem::new(CTX, CTX_EXIT_IDX), u32::from(shape.index));
    e.mov_mi(Mem::new(CTX, CTX_LAST_EXIT), shape.start);
    e.mov_mr(Mem::new(CTX, CTX_CPSR), CPSR);
    e.mov_rm(Reg::Rax, Mem::new(CTX, CTX_CYCLES));
    e.alu_rm(AluOp::Add, Reg::Rax, Mem::new(CTX, CTX_EXTRA_CYCLES));
    e.pop_64(CPSR);
    e.pop_64(CTX);
    e.pop_64(GPR_BASE);
    e.ret();
    epilogue_offset
}

/// Close a **dispatching** exit: bank, check the chain-break conditions, then
/// probe the dispatch table on the run-time R15 and continue the chain on a
/// hit. The probe is three checks deep — Thumb bit, tag compare — and any
/// failure falls into the ordinary epilogue, so a miss costs what an unlinked
/// exit always cost.
///
/// The hash is the same multiply-shift `Arm9Jit::hot_slot` uses, for the same
/// reason: block starts are aligned, so low bits carry no entropy.
fn emit_dispatch_exit(
    e: &mut Emitter,
    plan: &DispatchPlan,
    shape: ExitShape,
    hist: &[u64; 6],
) -> CompiledExit {
    e.alu_mi(AluOp::Add, Mem::new(CTX, CTX_CYCLES), shape.cycles);
    e.alu_mi(AluOp::Add, Mem::new(CTX, CTX_RETIRED), shape.retired as u32);
    for (bucket, &count) in hist.iter().enumerate() {
        if count != 0 {
            let slot = CTX_CLASS_HITS + 4 * bucket as i32;
            e.alu_mi(AluOp::Add, Mem::new(CTX, slot), count as u32);
        }
    }
    // Chain-break conditions, exactly as a linked exit checks them.
    e.mov_rm(FLAGS, Mem::new(CTX, CTX_CYCLES));
    e.alu_rm(AluOp::Add, FLAGS, Mem::new(CTX, CTX_EXTRA_CYCLES));
    e.alu_rm(AluOp::Cmp, FLAGS, Mem::new(CTX, CTX_BUDGET));
    let budget_spent = e.jcc_placeholder(Cond::Ae);
    e.alu_mi(AluOp::Cmp, Mem::new(CTX, CTX_STOP), 0);
    let store_stopped = e.jcc_placeholder(Cond::Ne);
    // The run-time target, RAW. Bit 0 partitions the table: Thumb bodies are
    // tagged with their odd interworking value, ARM bodies with the aligned
    // one, so an odd target can only ever hit a Thumb entry and vice versa —
    // which is also what makes a cross-ISA chain hop legal (CPSR, with T
    // already updated by the interworking write, rides in the pinned
    // register).
    e.mov_rm(FLAGS, gpr_slot(15));
    // slot = ((target * K) >> 32) & mask, scaled by the 16-byte record.
    e.mov_rr(OP2, FLAGS);
    e.mov_ri_64(PACK, 0x9E37_79B9_7F4A_7C15);
    e.imul_rr_64(OP2, PACK);
    e.shift_ri_64(ShiftOp::Shr, OP2, 32);
    e.alu_ri(AluOp::And, OP2, plan.mask);
    e.shift_ri(ShiftOp::Shl, OP2, 4);
    e.mov_ri_64(PACK, plan.table);
    e.alu_rr_64(AluOp::Add, PACK, OP2);
    e.alu_rm(AluOp::Cmp, FLAGS, Mem::new(PACK, 0));
    let tag_miss = e.jcc_placeholder(Cond::Ne);
    // **The body pointer, not the tag, is the true gate.** No tag sentinel is
    // safe when the guest controls R15 — `MVN r4,#0; BLX r4` produces a
    // target of 0xFFFFFFFF, which matched the old EMPTY marker on a slot
    // whose body was null and jumped the host to address zero (thumb fuzz
    // seed 1006, STATUS_ACCESS_VIOLATION). Teardown zeroes bodies with tags,
    // so a null body covers empty, torn-down, and stale-sentinel slots alike.
    e.mov_rm_64(OP2, Mem::new(PACK, 8));
    e.test_rr_64(OP2, OP2);
    let empty_body = e.jcc_placeholder(Cond::E);
    // Following the dispatch IS the pipeline flush the exchange owes.
    e.mov_mi(Mem::new(CTX, CTX_PC_MODIFIED), 0);
    e.jmp_r(OP2);
    e.patch_rel32(budget_spent);
    e.patch_rel32(store_stopped);
    e.patch_rel32(tag_miss);
    e.patch_rel32(empty_body);
    let epilogue_offset = emit_exit_epilogue(e, &shape);
    CompiledExit {
        index: shape.index,
        target: shape.target,
        sets_pc: shape.sets_pc,
        epilogue_offset,
        dispatch: true,
    }
}

// ---------------------------------------------------------------------------
// Conditions
// ---------------------------------------------------------------------------

/// An ARM condition code, with `AL` named so the common case is readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArmCond(u32);

impl ArmCond {
    pub const ALWAYS: Self = Self(0xE);
}

/// Emit a test that **jumps away when the ARM condition fails**, returning the
/// patch sites to point past the instruction body.
///
/// Producing "jump on failure" rather than "jump on success" keeps the taken
/// path — the common one, since most conditions pass — as straight-line code.
///
/// The single-flag conditions are one `test` and one branch. `GE`/`LT`/`GT`/`LE`
/// need `N == V`, which is computed by shifting V up to N's position and
/// XOR-ing: bit 31 of the result is `N ^ V`, so `sign set` means they differ.
fn emit_condition_fails(e: &mut Emitter, cond: ArmCond) -> Vec<usize> {
    // Test one flag: the AND sets ZF when the flag is clear.
    let one = |e: &mut Emitter, mask: u32, fail_when_set: bool| -> Vec<usize> {
        e.mov_rr(FLAGS, CPSR);
        e.alu_ri(AluOp::And, FLAGS, mask);
        vec![e.jcc_placeholder(if fail_when_set { Cond::Ne } else { Cond::E })]
    };

    // bit 31 of PACK becomes `N ^ V`.
    let n_xor_v = |e: &mut Emitter| {
        e.mov_rr(PACK, CPSR);
        e.shift_ri(crate::jit::x64::ShiftOp::Shl, PACK, 3); // V (bit 28) -> bit 31
        e.alu_rr(AluOp::Xor, PACK, CPSR);
    };

    match cond.0 {
        0x0 => one(e, Z, false),  // EQ: fail when Z clear
        0x1 => one(e, Z, true),   // NE: fail when Z set
        0x2 => one(e, C, false),  // CS
        0x3 => one(e, C, true),   // CC
        0x4 => one(e, N, false),  // MI
        0x5 => one(e, N, true),   // PL
        0x6 => one(e, V, false),  // VS
        0x7 => one(e, V, true),   // VC
        0x8 => {
            // HI: C set and Z clear. Two independent failures.
            let mut sites = one(e, C, false);
            sites.extend(one(e, Z, true));
            sites
        }
        0x9 => {
            // LS: C clear or Z set. Fails only when C is set and Z is clear, so
            // build `!C | Z` in one value: shifting `!C` (bit 29) up by one
            // lands it on Z's own position, bit 30.
            e.mov_rr(PACK, CPSR);
            e.not_r(PACK);
            e.shift_ri(crate::jit::x64::ShiftOp::Shl, PACK, 1);
            e.alu_rr(AluOp::Or, PACK, CPSR);
            e.alu_ri(AluOp::And, PACK, Z);
            vec![e.jcc_placeholder(Cond::E)]
        }
        0xA => {
            // GE: N == V, i.e. fail when they differ.
            n_xor_v(e);
            e.alu_ri(AluOp::And, PACK, N);
            vec![e.jcc_placeholder(Cond::Ne)]
        }
        0xB => {
            // LT: N != V.
            n_xor_v(e);
            e.alu_ri(AluOp::And, PACK, N);
            vec![e.jcc_placeholder(Cond::E)]
        }
        0xC => {
            // GT: Z clear and N == V.
            let mut sites = one(e, Z, true);
            n_xor_v(e);
            e.alu_ri(AluOp::And, PACK, N);
            sites.push(e.jcc_placeholder(Cond::Ne));
            sites
        }
        0xD => {
            // LE: Z set or N != V. Combine into one value so a single branch
            // decides: `(N ^ V) | (Z << 1)` has bit 31 set exactly when it passes.
            n_xor_v(e);
            e.mov_rr(PACK2, CPSR);
            e.shift_ri(crate::jit::x64::ShiftOp::Shl, PACK2, 1); // Z -> bit 31
            e.alu_rr(AluOp::Or, PACK, PACK2);
            e.alu_ri(AluOp::And, PACK, N);
            vec![e.jcc_placeholder(Cond::E)]
        }
        other => unreachable!("condition {other:#x} reached the emitter"),
    }
}

// ---------------------------------------------------------------------------
// Data processing
// ---------------------------------------------------------------------------

/// A constant-amount barrel shift applied to `Rm`.
///
/// Every variant records where the *shifter carry-out* comes from, because ARM
/// takes it from a specific bit of the **original** value while the x86 shift
/// leaves CF holding the same thing only by coincidence — and the ALU operation
/// that follows overwrites CF anyway. Capturing it from the named bit before the
/// shift runs is the only form that is correct in all cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shift {
    /// `LSL #0`: the register itself, carry unchanged. By far the most common
    /// form — `MOV rd,rm`, `ADD rd,rn,rm` — so it costs no shift at all.
    Pass,
    /// A shift by 1..=31, and the bit of the original value that becomes C.
    By { op: ShiftOp, amount: u8, carry_bit: u8 },
    /// `LSR #32`, which the encoding writes as `LSR #0`: the result is zero.
    Lsr32,
    /// `ASR #32`, written as `ASR #0`: the result is the sign bit replicated.
    Asr32,
    /// `RRX`, written as `ROR #0`: a 33-bit rotate right through the carry.
    Rrx,
}

impl Shift {
    /// The bit of the original `Rm` that becomes the shifter carry, or `None`
    /// when the shift leaves C alone.
    fn carry_bit(self) -> Option<u8> {
        match self {
            Self::Pass => None,
            Self::By { carry_bit, .. } => Some(carry_bit),
            Self::Lsr32 | Self::Asr32 => Some(31),
            Self::Rrx => Some(0),
        }
    }

    /// Decode the shift-type and amount fields. The `#0` encoding means a
    /// different thing for each type, which is the trap this centralises.
    fn decode(shift_type: u32, amount: u32) -> Self {
        match (shift_type, amount) {
            (0, 0) => Self::Pass,
            (0, n) => Self::By { op: ShiftOp::Shl, amount: n as u8, carry_bit: (32 - n) as u8 },
            (1, 0) => Self::Lsr32,
            (1, n) => Self::By { op: ShiftOp::Shr, amount: n as u8, carry_bit: (n - 1) as u8 },
            (2, 0) => Self::Asr32,
            (2, n) => Self::By { op: ShiftOp::Sar, amount: n as u8, carry_bit: (n - 1) as u8 },
            (_, 0) => Self::Rrx,
            (_, n) => Self::By { op: ShiftOp::Ror, amount: n as u8, carry_bit: (n - 1) as u8 },
        }
    }
}

/// How operand 2 is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operand2 {
    /// A rotated immediate: the value *and* the shifter carry are constants.
    Immediate { value: u32, carry: Option<bool> },
    /// `Rm`, shifted by a constant amount.
    Register { rm: u32, shift: Shift },
}

/// Which flags a data-processing instruction publishes.
///
/// ARM has one rule per opcode class; Thumb has three, and they do not line up
/// with ARM's — a Thumb logical operation leaves **C** alone where the ARM form
/// takes it from the barrel shifter, and several Thumb formats write no flags
/// at all. Modelling that as a mode rather than a `bool` is what lets both
/// instruction sets share one emitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlagMode {
    /// Nothing published. Thumb F5 `ADD`/`MOV`, and any ARM form with S clear.
    None,
    /// N and Z only; C and V preserved. Thumb F3 `MOV` and the F4 logical ops.
    NzOnly,
    /// N, Z and C-from-the-barrel-shifter; V preserved. ARM logical with S set,
    /// and Thumb F1.
    Logical,
    /// N, Z, C and V from the arithmetic. Both instruction sets.
    Arithmetic,
}

impl FlagMode {
    fn writes_anything(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// The decoded form of a supported data-processing instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DataProcImm {
    cond: ArmCond,
    opcode: u32,
    rn: u32,
    rd: u32,
    flags: FlagMode,
    operand: Operand2,
    /// Word-align R15 before using it as operand 1.
    ///
    /// Thumb's `ADD Rd, PC, #off` reads `R15 & !2`; see
    /// [`SingleTransfer::align_base`]. Only meaningful when `rn == 15`.
    align_rn: bool,
}

impl DataProcImm {
    /// True for the opcodes that discard their result (`TST`/`TEQ`/`CMP`/`CMN`).
    fn discards_result(&self) -> bool {
        (0x8..=0xB).contains(&self.opcode)
    }
    /// True for the opcodes whose flags come from the barrel shifter rather than
    /// from an arithmetic carry: `AND EOR TST TEQ ORR MOV BIC MVN`.
    fn is_logical(&self) -> bool {
        matches!(self.opcode, 0x0 | 0x1 | 0x8 | 0x9 | 0xC | 0xD | 0xE | 0xF)
    }

    /// Does the shifter carry-out need capturing before the ALU overwrites CF?
    /// Only [`FlagMode::Logical`] consumes it.
    fn needs_shifter_carry(&self) -> bool {
        self.flags == FlagMode::Logical
    }
}

/// The word/byte space's offset: a 12-bit immediate, or a barrel-shifted
/// register (bit 25 set). The shift amount is always a constant in this
/// space — bit 4 set is the UNDEFINED-instruction window and is declined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransferOffset {
    Imm(u32),
    /// `Rm` with a constant shift, materialised by [`emit_operand2`] with the
    /// carry left alone — an address computation publishes no flags.
    Reg { rm: u32, shift: Shift },
}

/// `LDR`/`LDRB`/`STR`/`STRB`, immediate or register offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SingleTransfer {
    cond: ArmCond,
    load: bool,
    byte: bool,
    /// P: add the offset before the access rather than after it.
    pre: bool,
    /// U: add rather than subtract.
    up: bool,
    /// W. A post-indexed form always writes back, whatever this says.
    writeback: bool,
    rn: u32,
    rd: u32,
    offset: TransferOffset,
    /// Word-align the base before adding the offset.
    ///
    /// Thumb's PC-relative forms read `R15 & !2`, because R15 in Thumb state is
    /// only halfword-aligned and these access words. Missing the mask puts the
    /// access two bytes out — on the right page, in the right region, reading
    /// the wrong word.
    align_base: bool,
}

impl SingleTransfer {
    /// Does this form write the base register?
    ///
    /// The rule differs by direction, and the difference is real: a load that
    /// would write its own destination skips the writeback entirely, because
    /// the loaded value wins.
    fn writes_base(&self) -> bool {
        let wb = self.writeback || !self.pre;
        if self.load {
            wb && self.rd != self.rn
        } else {
            wb
        }
    }
}

/// What a halfword-space load moves. The store side of the space is `STRH`
/// only — `LDRD`/`STRD` (ARMv5E, L=0 with SH >= 2) move a register pair and
/// are declined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HalfKind {
    /// `LDRH`/`STRH`: zero-extended halfword.
    Half,
    /// `LDRSB`: sign-extended byte.
    SignedByte,
    /// `LDRSH`: sign-extended halfword.
    SignedHalf,
}

/// The halfword space's offset: an 8-bit immediate split across two nibbles,
/// or a plain register — no barrel shift exists in this encoding, which is
/// why the register form is translatable here and not in the word space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HalfOffset {
    Imm(u32),
    Reg(u32),
}

/// `LDRH`/`LDRSB`/`LDRSH`/`STRH` — the extension-space transfers
/// (`arm_halfword_transfer`): 3 cycles, writeback-before-destination on
/// loads, store-then-writeback on `STRH`, `Rd == Rn` loads skip writeback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HalfTransfer {
    cond: ArmCond,
    load: bool,
    kind: HalfKind,
    pre: bool,
    up: bool,
    writeback: bool,
    rn: u32,
    rd: u32,
    offset: HalfOffset,
}

impl HalfTransfer {
    /// Same direction-dependent rule as [`SingleTransfer::writes_base`].
    fn writes_base(&self) -> bool {
        let wb = self.writeback || !self.pre;
        if self.load {
            wb && self.rd != self.rn
        } else {
            wb
        }
    }
}

/// MUL/MLA and the long forms — `arm_multiply` (flat 4 cycles) and
/// `arm_multiply_long` (flat 5): S sets N and Z only, C and V untouched on
/// the ARM7TDMI and this interpreter alike. Any R15 field declines: the
/// interpreter would read the block's synthetic R15 slot or write the PC
/// without a refill, and real code never multiplies the program counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Multiply {
    cond: ArmCond,
    accumulate: bool,
    set_flags: bool,
    /// `None` = 32-bit MUL/MLA (`rd` result, `rn` accumulator);
    /// `Some(signed)` = long form (`rd` = RdHi, `rn` = RdLo).
    long: Option<bool>,
    rd: u32,
    rn: u32,
    rs: u32,
    rm: u32,
}

/// A direct `B`/`BL`. Always the last instruction of its block.
///
/// The offset is kept as encoded rather than resolved at decode time because
/// the target depends on the instruction's own address, which only the emitter
/// knows — and keeping `decode` address-independent is what lets a block be
/// cached by address alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectBranch {
    cond: ArmCond,
    link: bool,
    /// Sign-extended 24-bit word offset, as encoded.
    offset: i32,
}

/// `LDM`/`STM` without the S bit.
///
/// The register list is fixed by the encoding, so the transfer size, the
/// addresses and the cycle count are all compile-time constants — only the base
/// value is a run-time quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockTransfer {
    cond: ArmCond,
    load: bool,
    /// P: the first access is offset by a word when `up == pre`.
    pre: bool,
    /// U: ascending addresses.
    up: bool,
    writeback: bool,
    rn: u32,
    /// Bit `r` set means register `r` takes part. Never contains R15 for a load
    /// (the scanner excludes that), and never empty.
    list: u16,
    /// Cycles the interpreter charges.
    ///
    /// Not derivable from the register count: `arm_block_transfer` returns
    /// `count + 2`, but Thumb's `thumb_push_pop` and `thumb_block` both return
    /// **3 flat**. Charging ARM's formula for a Thumb `POP {r0-r7}` would
    /// over-report by six cycles and drag the whole peripheral schedule.
    cycle_cost: u32,
}

impl BlockTransfer {
    fn count(&self) -> u32 {
        u32::from(self.list.count_ones() as u16)
    }

    /// Does this load distribute into R15? Only reachable when the dispatch
    /// scanner ended the block with it; the interworking write is
    /// [`emit_pc_write_interworking`].
    fn loads_pc(&self) -> bool {
        self.load && (self.list & 0x8000) != 0
    }

    /// Bytes the transfer spans.
    fn span(&self) -> u32 {
        self.count() * 4
    }

    /// Does the base register get written back?
    ///
    /// A load that reloads its own base does not: the loaded value wins, which
    /// is the same rule single transfers follow for `Rd == Rn`.
    fn writes_base(&self) -> bool {
        self.writeback && !(self.load && (self.list >> self.rn) & 1 != 0)
    }
}

/// An unconditional `BX Rm` / `BLX Rm` — the run-time branch a dispatching
/// block ends with. The interpreter's semantics
/// (`GbaCpu::execute_arm_reg_class`): T := bit 0 of the target, R15 := the
/// target **verbatim**, `pc_modified`, 3 cycles; `BLX` first links
/// `LR = R15 - 4`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Exchange {
    link: bool,
    rm: u32,
}

/// `MRS Rd, CPSR` — one host `mov` out of the pinned CPSR register — or
/// `MRS Rd, SPSR`, a thunk read of the banked field the CPU owns (the ITCM
/// IRQ trampoline's idiom, ×36k refused chain targets before it compiled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mrs {
    cond: ArmCond,
    rd: u32,
    spsr: bool,
}

/// ARMv5 `CLZ Rd, Rm`: `Rm.leading_zeros()`, one cycle, no flags. Host
/// `bsr` plus a branched zero case (`bsr`'s destination is undefined there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Clz {
    cond: ArmCond,
    rd: u32,
    rm: u32,
}

/// `MSR CPSR_fields, op` — committed through a thunk that owns the parts a
/// block cannot: the register-bank swap a mode change requires, and raising
/// the chain-break flag when the write unmasks a pending interrupt. Always a
/// block terminator, so the flag is checked before the next guest
/// instruction, exactly as the interpreter's boundary would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Msr {
    /// The field mask `arm_psr_transfer` derives from bits 19/16.
    mask: u32,
    operand: MsrOperand,
    /// The immediate encoding retires in `arm_class_hist` bucket 1, the
    /// register one in bucket 0 — the interpreter bumps before dispatching.
    imm_form: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MsrOperand {
    Imm(u32),
    Reg(u32),
}

/// Thumb F16 (conditional) / F18 (unconditional) branch. `delta` is the
/// halfword-scaled displacement, already resolved: `target = pc + delta`,
/// where `pc = addr + 4`. Thumb condition codes are ARM's, verbatim
/// (`GbaCpu::check_condition` mirrors the ARM table), so the shared
/// condition emitter applies unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ThumbBranch {
    cond: ArmCond,
    delta: i32,
}

/// Thumb F19 `BL` **pair**. Both halves are in the body (the scanner pushes
/// them atomically), the pair's net effect is two constants — the target and
/// `LR = (first_half_addr + 4) | 1` — and the target is followable exactly
/// like ARM's `BL`. The first half emits the link; the second half is
/// [`Instr::TBlPad`], keeping the body/decoded arrays aligned 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ThumbBl {
    target: u32,
}

/// One translatable ARM instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Instr {
    DataProc(DataProcImm),
    Transfer(SingleTransfer),
    Half(HalfTransfer),
    Mul(Multiply),
    Clz(Clz),
    /// An unconditional CP15 transfer `execute_cp15_transfer` has no arm for
    /// — the cache-maintenance ops, an architectural no-op on this emulator
    /// (unmatched MCR does nothing; unmatched MRC writes Rd's own value
    /// back). One cycle. `Arm9Cpu::step`'s intercept skips the retirement
    /// counters for these where the ARM7's coprocessor path bumps them; the
    /// block counts uniformly, a documented diagnostic-only skew on the ARM9.
    CopNop,
    Branch(DirectBranch),
    Block(BlockTransfer),
    Exchange(Exchange),
    Mrs(Mrs),
    Msr(Msr),
    TBranch(ThumbBranch),
    TBl(ThumbBl),
    /// The `BL` pair's second half: a placeholder that keeps addresses 1:1
    /// with the body. Charges the second half's 3 cycles, and — when the pair
    /// is terminal — performs the PC redirect.
    TBlPad(ThumbBl),
    /// Thumb F5 `BX`/`BLX` (`thumb_hi_reg` op 3): interworking, R15 takes the
    /// register VERBATIM, T from bit 0; `BLX` (ARMv5, H1) links
    /// `LR = (pc - 2) | 1` **after** the target register is read — `BLX lr`
    /// must branch to the old LR. One cycle. A dispatching terminator.
    TBx { link: bool, rs: u32 },
    /// Thumb F14 `POP {list, pc}` (`thumb_push_pop`): per-register rotating
    /// word reads, SP written back before the PC, `load_pc` interworking on
    /// the popped value. Three cycles flat. A dispatching terminator.
    TPop { list: u8 },
}

impl Instr {
    /// Word reads are the only load paths that can reach the side-effecting
    /// Gamecard port. Byte/halfword reads leave IRQ state unchanged.
    fn reads_word_memory(&self) -> bool {
        match self {
            Self::Transfer(t) => t.load && !t.byte,
            Self::Block(b) => b.load,
            Self::TPop { .. } => true,
            _ => false,
        }
    }

    fn cond(&self) -> ArmCond {
        match self {
            Self::DataProc(d) => d.cond,
            Self::Transfer(t) => t.cond,
            Self::Half(h) => h.cond,
            Self::Mul(m) => m.cond,
            Self::Clz(c) => c.cond,
            Self::Branch(b) => b.cond,
            Self::Block(b) => b.cond,
            Self::Mrs(m) => m.cond,
            Self::TBranch(t) => t.cond,
            // The scanner only ever forms the unconditional encodings — and
            // `CopNop` decodes only when unconditional by construction.
            Self::CopNop
            | Self::Exchange(_)
            | Self::Msr(_)
            | Self::TBl(_)
            | Self::TBlPad(_)
            | Self::TBx { .. }
            | Self::TPop { .. } => ArmCond::ALWAYS,
        }
    }

    /// Cycles the interpreter charges. `arm_data_processing` returns 1,
    /// `arm_single_transfer` 3, and a taken branch 3 — so a block's total is a
    /// sum, and a conditional instruction's is only known at run time.
    fn cycles(&self) -> u32 {
        match self {
            // `arm_psr_transfer` returns 1 for both directions; the F19 `BL`
            // first half and `thumb_hi_reg` (BX included) are 1 too. CLZ and
            // the CP15/coprocessor no-op each consume a single cycle.
            Self::DataProc(_)
            | Self::Mrs(_)
            | Self::Msr(_)
            | Self::Clz(_)
            | Self::CopNop
            | Self::TBl(_)
            | Self::TBx { .. } => 1,
            Self::Transfer(_)
            | Self::Half(_)
            | Self::Branch(_)
            | Self::Exchange(_)
            | Self::TBranch(_)
            | Self::TBlPad(_)
            | Self::TPop { .. } => 3,
            // `arm_multiply` returns 4 flat; `arm_multiply_long` 5 flat.
            Self::Mul(m) => {
                if m.long.is_some() {
                    5
                } else {
                    4
                }
            }
            Self::Block(b) => b.cycle_cost,
        }
    }

    /// The `arm_class_hist` bucket `GbaCpu::execute_arm` would have used.
    fn hist_bucket(&self) -> usize {
        match self {
            Self::DataProc(d) => match d.operand {
                Operand2::Register { .. } => 0,
                Operand2::Immediate { .. } => 1,
            },
            Self::Transfer(_) => 2,
            Self::Branch(_) => 4,
            Self::Block(_) => 3,
            // Class 0 with a register operand: bucket 0, bumped before the
            // handler in `execute_arm`. The halfword space, the multiply
            // family and CLZ live there too.
            Self::Exchange(_) | Self::Half(_) | Self::Mrs(_) | Self::Mul(_) | Self::Clz(_) => 0,
            // The coprocessor bucket — what the ARM7's class-3 path bumps.
            Self::CopNop => 5,
            Self::Msr(m) => usize::from(m.imm_form),
            // Thumb retirements never touch `arm_class_hist`; the value is
            // unused (`hist_for` zeroes Thumb histograms and the conditional
            // bump is skipped for Thumb bodies).
            Self::TBranch(_) | Self::TBl(_) | Self::TBlPad(_) | Self::TBx { .. }
            | Self::TPop { .. } => 4,
        }
    }

    /// Does this instruction leave R15 holding a run-time value the dispatch
    /// exit should probe on?
    fn is_runtime_branch(&self) -> bool {
        match self {
            Self::Exchange(_) => true,
            Self::Block(b) => b.loads_pc(),
            Self::DataProc(d) => d.rd == 15,
            Self::Transfer(t) => t.load && t.rd == 15,
            Self::TBx { .. } | Self::TPop { .. } => true,
            Self::Half(_)
            | Self::Mul(_)
            | Self::Clz(_)
            | Self::CopNop
            | Self::Branch(_)
            | Self::Mrs(_)
            | Self::Msr(_)
            | Self::TBranch(_)
            | Self::TBl(_)
            | Self::TBlPad(_) => false,
        }
    }
}

/// Recognise a supported **Thumb** instruction, or `None`.
///
/// # Why this produces ARM's decoded form
///
/// Thumb's data-processing formats are ARM's with the fields moved and the
/// flag rules changed: `ADD r0,r1,r2` is the same operation whichever
/// instruction set encodes it. Mapping onto [`DataProcImm`] means the emitter,
/// the flag packer and the barrel shifter are shared rather than duplicated —
/// and [`FlagMode`] carries the one thing that genuinely differs.
///
/// Two Thumb-only simplifications fall out: there is **no condition field**, so
/// every instruction is unconditional, and the register fields are three bits,
/// so R15 cannot be named except in the hi-register format.
fn decode_thumb(inst: u16) -> Option<Instr> {
    let i = u32::from(inst);
    let always = ArmCond::ALWAYS;
    let reg = |rm: u32| Operand2::Register { rm, shift: Shift::Pass };
    let imm = |value: u32| Operand2::Immediate { value, carry: None };

    // F2 add/subtract. Checked first: its mask is a sub-window of F1's.
    if (i & 0xF800) == 0x1800 {
        let is_imm = (i & 0x0400) != 0;
        let sub = (i & 0x0200) != 0;
        let rn_off = (i >> 6) & 7;
        return Some(Instr::DataProc(DataProcImm {
            cond: always,
            opcode: if sub { 0x2 } else { 0x4 },
            rn: (i >> 3) & 7,
            rd: i & 7,
            flags: FlagMode::Arithmetic,
            operand: if is_imm { imm(rn_off) } else { reg(rn_off) },
            align_rn: false,
        }));
    }

    // F1 move shifted register. `LSR #0` and `ASR #0` mean #32, which
    // `Shift::decode` already models.
    if (i & 0xE000) == 0x0000 {
        let shift = Shift::decode((i >> 11) & 3, (i >> 6) & 0x1F);
        return Some(Instr::DataProc(DataProcImm {
            cond: always,
            opcode: 0xD, // MOV
            rn: 0,       // unused
            rd: i & 7,
            flags: FlagMode::Logical,
            operand: Operand2::Register { rm: (i >> 3) & 7, shift },
            align_rn: false,
        }));
    }

    // F3 move/compare/add/subtract immediate.
    if (i & 0xE000) == 0x2000 {
        let rd = (i >> 8) & 7;
        let value = i & 0xFF;
        let (opcode, flags) = match (i >> 11) & 3 {
            // `MOV` publishes N and Z only — not the carry an ARM `MOVS` would.
            0 => (0xD, FlagMode::NzOnly),
            1 => (0xA, FlagMode::Arithmetic), // CMP
            2 => (0x4, FlagMode::Arithmetic), // ADD
            _ => (0x2, FlagMode::Arithmetic), // SUB
        };
        return Some(Instr::DataProc(DataProcImm {
            cond: always,
            opcode,
            rn: rd,
            rd,
            flags,
            operand: imm(value),
            align_rn: false,
        }));
    }

    // F4 ALU operations, register to register.
    if (i & 0xFC00) == 0x4000 {
        let rs = (i >> 3) & 7;
        let rd = i & 7;
        let (opcode, flags, operand) = match (i >> 6) & 0xF {
            0x0 => (0x0, FlagMode::NzOnly, reg(rs)),    // AND
            0x1 => (0x1, FlagMode::NzOnly, reg(rs)),    // EOR
            0x5 => (0x5, FlagMode::Arithmetic, reg(rs)), // ADC
            0x6 => (0x6, FlagMode::Arithmetic, reg(rs)), // SBC
            0x8 => (0x8, FlagMode::NzOnly, reg(rs)),    // TST
            // NEG is `0 - Rs`, which is ARM's RSB with a zero immediate.
            0x9 => (0x3, FlagMode::Arithmetic, imm(0)),
            0xA => (0xA, FlagMode::Arithmetic, reg(rs)), // CMP
            0xB => (0xB, FlagMode::Arithmetic, reg(rs)), // CMN
            0xC => (0xC, FlagMode::NzOnly, reg(rs)),    // ORR
            0xE => (0xE, FlagMode::NzOnly, reg(rs)),    // BIC
            0xF => (0xF, FlagMode::NzOnly, reg(rs)),    // MVN
            // 0x2/0x3/0x4/0x7 shift by a *register* amount, and 0xD is MUL.
            // Neither has an ARM data-processing equivalent here; both are
            // later work.
            _ => return None,
        };
        let rn = if (i >> 6) & 0xF == 0x9 { rs } else { rd };
        return Some(Instr::DataProc(DataProcImm {
            cond: always,
            opcode,
            rn,
            rd,
            flags,
            operand,
            align_rn: false,
        }));
    }

    // F6 PC-relative load. The address is entirely compile-time: R15 in Thumb
    // is the instruction's address plus four, word-aligned by the `& !2`.
    if (i & 0xF800) == 0x4800 {
        return Some(Instr::Transfer(SingleTransfer {
            cond: always,
            load: true,
            byte: false,
            pre: true,
            up: true,
            writeback: false,
            rn: 15,
            rd: (i >> 8) & 7,
            offset: TransferOffset::Imm((i & 0xFF) << 2),
            align_base: true,
        }));
    }

    // F9 load/store with a 5-bit immediate offset. The offset scales by the
    // access width: words shift left by two, bytes do not.
    if (i & 0xE000) == 0x6000 {
        let byte = (i & 0x1000) != 0;
        let off5 = (i >> 6) & 0x1F;
        return Some(Instr::Transfer(SingleTransfer {
            cond: always,
            load: (i & 0x0800) != 0,
            byte,
            pre: true,
            up: true,
            writeback: false,
            rn: (i >> 3) & 7,
            rd: i & 7,
            offset: TransferOffset::Imm(if byte { off5 } else { off5 << 2 }),
            align_base: false,
        }));
    }

    // F11 SP-relative load/store.
    if (i & 0xF000) == 0x9000 {
        return Some(Instr::Transfer(SingleTransfer {
            cond: always,
            load: (i & 0x0800) != 0,
            byte: false,
            pre: true,
            up: true,
            writeback: false,
            rn: 13,
            rd: (i >> 8) & 7,
            offset: TransferOffset::Imm((i & 0xFF) << 2),
            align_base: false,
        }));
    }

    // F12 load address: `Rd = (SP or word-aligned PC) + off`. No flags.
    if (i & 0xF000) == 0xA000 {
        let rd = (i >> 8) & 7;
        let off = (i & 0xFF) << 2;
        let use_sp = (i & 0x0800) != 0;
        return Some(Instr::DataProc(DataProcImm {
            cond: always,
            opcode: 0x4, // ADD
            // The PC form is a constant, so it becomes `MOV rd, #const` at emit
            // time via the R15 operand-1 path, with the alignment folded in.
            rn: if use_sp { 13 } else { 15 },
            rd,
            flags: FlagMode::None,
            operand: imm(off),
            // The PC form reads `R15 & !2`.
            align_rn: !use_sp,
        }));
    }

    // F13 adjust stack pointer. Bit 7 selects subtract.
    if (i & 0xFF00) == 0xB000 {
        let off = (i & 0x7F) << 2;
        return Some(Instr::DataProc(DataProcImm {
            cond: always,
            opcode: if (i & 0x80) != 0 { 0x2 } else { 0x4 },
            rn: 13,
            rd: 13,
            flags: FlagMode::None,
            operand: imm(off),
            align_rn: false,
        }));
    }

    // **F14 PUSH/POP and F15 LDMIA/STMIA are declined.**
    //
    // The interpreter transfers these register by register through
    // `CpuBus::read_word`/`write_word`, which mask `addr & !3` *and*, on the
    // read side, rotate the loaded word by `(addr & 3) * 8`. The ARM emitter
    // this would otherwise reuse goes through `read_words`/`write_words`, which
    // take the address as given — matching the interpreter's ARM path, not its
    // Thumb one.
    //
    // The differential harness found both halves at 4000 seeds: `thumb seed
    // 104` (memory written at unaligned offsets) and, after masking alone,
    // `thumb seed 90` (`r0: 0x500fe8bd vs 0xe8bd500f` — the missing rotate).
    // Emitting a per-word rotate to serve a base alignment real code never
    // produces is not worth the encoding; the interpreter keeps them.
    // F5 hi-register operations. `BX`/`BLX` and any R15 destination are
    // excluded by the scanner; rejecting them here too keeps a disagreement
    // from becoming a miscompile.
    if (i & 0xFC00) == 0x4400 {
        let rd = (i & 7) | ((i >> 4) & 8);
        let rs = ((i >> 3) & 7) | ((i >> 3) & 8);
        if rd == 15 {
            return None;
        }
        let (opcode, flags) = match (i >> 8) & 3 {
            0 => (0x4, FlagMode::None),       // ADD, no flags
            1 => (0xA, FlagMode::Arithmetic), // CMP, flags only
            2 => (0xD, FlagMode::None),       // MOV, no flags
            _ => return None,                 // BX/BLX
        };
        return Some(Instr::DataProc(DataProcImm {
            cond: always,
            opcode,
            rn: rd,
            rd,
            flags,
            operand: reg(rs),
            align_rn: false,
        }));
    }

    None
}

/// Recognise the supported subset, or `None`.
///
/// # How the register form is separated from everything that shares its space
///
/// Class 0 with bit 25 clear also holds `BX`, `BLX(reg)`, `CLZ`, `MUL`, `MULL`,
/// `SWP`, the halfword and doubleword transfers, and the ARMv5TE DSP families.
/// Two tests exclude all of them:
///
/// * **bit 4 clear** — every one of those encodings sets it, because bit 4 is
///   what distinguishes a register-specified shift amount from an immediate one
///   and they all reuse that slot. It also excludes register-specified shifts
///   themselves, which need run-time branching on the amount and are a later
///   milestone.
/// * **opcode 8..=B with S clear rejected** — that is the MSR/MRS window, and
///   the DSP signed-multiply family (bit 7 set, bit 4 clear, which the first
///   test does *not* catch) lies entirely inside it.
fn decode(inst: u32) -> Option<Instr> {
    decode_dispatch(inst, false, true)
}

/// [`decode`], optionally accepting the run-time branches a dispatching block
/// ends with. Defensive on both axes: without `dispatch` the subset is exactly
/// what it always was, and with it only the **unconditional** encodings the
/// scanner forms are accepted, so a scanner/translator disagreement declines
/// instead of miscompiling.
///
/// `armv5` gates the encodings that exist only on the ARM9: BLX(reg) decodes
/// as something else entirely on an ARMv4T core, so compiling it there would
/// be a miscompile, not a missing feature.
fn decode_dispatch(inst: u32, dispatch: bool, armv5: bool) -> Option<Instr> {
    let cond = inst >> 28;
    if cond == 0xF {
        return None; // the ARMv5 NV space is BLX, not a condition
    }
    if dispatch && crate::jit::block::is_register_exchange(inst) {
        let link = (inst & 0x0000_0020) != 0;
        if link && !armv5 {
            return None; // BLX(reg) is ARMv5-only; on ARMv4T this is PSR space
        }
        return Some(Instr::Exchange(Exchange { link, rm: inst & 0xF }));
    }
    if dispatch && crate::jit::block::is_pc_data_proc(inst) {
        // `MOV pc,lr` and friends: an ordinary data-processing emission whose
        // destination slot happens to be R15, plus the refill it owes. One
        // cycle and NO interworking, matching `arm_data_processing`.
        return decode_data_proc_any_rd(inst, ArmCond(cond)).map(Instr::DataProc);
    }
    if dispatch && crate::jit::block::is_pc_single_load(inst) {
        // `LDR pc, [..]`: an ordinary word load whose destination is R15 —
        // which on ARMv5 **interworks** (T from bit 0 of the loaded word),
        // unlike the data-processing write above. Three cycles.
        return decode_transfer_any_rd(inst, ArmCond(cond)).map(Instr::Transfer);
    }
    match (inst >> 26) & 3 {
        // The halfword/signed extension space: class 0, bit 25 clear, bits 7
        // and 4 set, SH non-zero (SH == 0 is MUL/SWP). Checked before data
        // processing, whose bit-4 test would otherwise reject it.
        0 if (inst & 0x0200_0090) == 0x0000_0090 && (inst >> 5) & 3 != 0 => {
            decode_halfword(inst, ArmCond(cond)).map(Instr::Half)
        }
        // The multiply family: MUL/MLA (bits 27-22 zero) and the long forms
        // (bits 27-23 = 00001), both with bits 7-4 = 1001. SWP shares SH == 0
        // but has bit 24 set, which both masks exclude.
        0 if (inst & 0x0FC0_00F0) == 0x0000_0090
            || (inst & 0x0F80_00F0) == 0x0080_0090 =>
        {
            decode_multiply(inst, ArmCond(cond)).map(Instr::Mul)
        }
        // ARMv5 `CLZ Rd, Rm` (an R15 field declines; ARMv4 executes this
        // encoding as something else entirely, so the gate is correctness).
        0 if armv5 && (inst & 0x0FFF_0FF0) == 0x016F_0F10 => {
            let rd = (inst >> 12) & 0xF;
            let rm = inst & 0xF;
            (rd != 15 && rm != 15)
                .then_some(Instr::Clz(Clz { cond: ArmCond(cond), rd, rm }))
        }
        // `MRS Rd, CPSR/SPSR` (an R15 destination declines).
        0 if (inst & 0x0FBF_0FFF) == 0x010F_0000 => {
            let rd = (inst >> 12) & 0xF;
            (rd != 15).then_some(Instr::Mrs(Mrs {
                cond: ArmCond(cond),
                rd,
                spsr: (inst & 0x0040_0000) != 0,
            }))
        }
        // `MSR CPSR_fields, op`, both encodings — only the unconditional
        // form the scanner terminates blocks with.
        0 if cond == 0xE
            && ((inst & 0x0FF0_FFF0) == 0x0120_F000
                || (inst & 0x0FF0_F000) == 0x0320_F000) =>
        {
            let imm_form = (inst & 0x0200_0000) != 0;
            let operand = if imm_form {
                let imm = inst & 0xFF;
                let rot = ((inst >> 8) & 0xF) * 2;
                MsrOperand::Imm(imm.rotate_right(rot))
            } else {
                MsrOperand::Reg(inst & 0xF)
            };
            let mut mask = 0u32;
            if (inst & 0x0008_0000) != 0 {
                mask |= 0xFF00_0000;
            }
            if (inst & 0x0001_0000) != 0 {
                mask |= 0x0000_00FF;
            }
            Some(Instr::Msr(Msr { mask, operand, imm_form }))
        }
        0 => decode_data_proc(inst, ArmCond(cond)).map(Instr::DataProc),
        1 => decode_transfer(inst, ArmCond(cond)).map(Instr::Transfer),
        2 if (inst & 0x0200_0000) == 0 => {
            decode_block_dispatch(inst, ArmCond(cond), dispatch).map(Instr::Block)
        }
        // The CP15 no-op subset (unconditional only — `Arm9Cpu::step`'s
        // intercept ignores the condition field, so the conditional forms
        // stay interpreter business). Double-gated with the scanner's
        // `is_cp15_nop`, so a disagreement declines instead of miscompiling.
        3 if cond == 0xE
            && ((inst >> 24) & 0xF) == 0xE
            && ((inst >> 8) & 0xF) == 0xF
            && (inst & 0x10) != 0
            && crate::jit::block::is_cp15_nop(inst) =>
        {
            Some(Instr::CopNop)
        }
        2 if crate::jit::block::is_direct_branch(inst) => {
            // Sign-extend the 24-bit word offset.
            let mut offset = (inst & 0x00FF_FFFF) as i32;
            if (offset & 0x0080_0000) != 0 {
                offset |= !0x00FF_FFFF;
            }
            Some(Instr::Branch(DirectBranch {
                cond: ArmCond(cond),
                link: (inst & 0x0100_0000) != 0,
                offset,
            }))
        }
        _ => None, // LDM/STM and the coprocessor space: later milestones
    }
}

/// The halfword/signed extension space (`arm_halfword_transfer`).
///
/// Declined: `LDRD`/`STRD` (L clear with SH >= 2 — they move a register
/// pair), any R15 destination, and a writeback into an R15 base. The caller
/// has already matched the encoding shape.
fn decode_halfword(inst: u32, cond: ArmCond) -> Option<HalfTransfer> {
    let load = (inst & 0x0010_0000) != 0;
    let kind = match (inst >> 5) & 3 {
        1 => HalfKind::Half,
        2 if load => HalfKind::SignedByte,
        3 if load => HalfKind::SignedHalf,
        _ => return None, // LDRD/STRD
    };
    let rd = (inst >> 12) & 0xF;
    if rd == 15 {
        return None; // a control-flow change this space does not dispatch
    }
    let rn = (inst >> 16) & 0xF;
    let offset = if (inst & 0x0040_0000) != 0 {
        HalfOffset::Imm(((inst >> 4) & 0xF0) | (inst & 0xF))
    } else {
        HalfOffset::Reg(inst & 0xF)
    };
    let t = HalfTransfer {
        cond,
        load,
        kind,
        pre: (inst & 0x0100_0000) != 0,
        up: (inst & 0x0080_0000) != 0,
        writeback: (inst & 0x0020_0000) != 0,
        rn,
        rd,
        offset,
    };
    if rn == 15 && t.writes_base() {
        return None;
    }
    Some(t)
}

/// The multiply family; see [`Multiply`] for the semantics translated.
fn decode_multiply(inst: u32, cond: ArmCond) -> Option<Multiply> {
    let long = if (inst & 0x0FC0_00F0) == 0x0000_0090 {
        None
    } else if (inst & 0x0F80_00F0) == 0x0080_0090 {
        Some((inst & 0x0040_0000) != 0)
    } else {
        return None;
    };
    let m = Multiply {
        cond,
        accumulate: (inst & 0x0020_0000) != 0,
        set_flags: (inst & 0x0010_0000) != 0,
        long,
        rd: (inst >> 16) & 0xF,
        rn: (inst >> 12) & 0xF,
        rs: (inst >> 8) & 0xF,
        rm: inst & 0xF,
    };
    if m.rd == 15 || m.rn == 15 || m.rs == 15 || m.rm == 15 {
        return None;
    }
    Some(m)
}

/// `LDM`/`STM` without the S bit.
fn decode_block(inst: u32, cond: ArmCond) -> Option<BlockTransfer> {
    decode_block_dispatch(inst, cond, false)
}

/// [`decode_block`], optionally accepting the **unconditional** `LDM {..,pc}`
/// a dispatching block ends with.
fn decode_block_dispatch(inst: u32, cond: ArmCond, dispatch: bool) -> Option<BlockTransfer> {
    // S selects the user-mode bank or an exception return; out of scope, and
    // already excluded by the scanner.
    if (inst & 0x0040_0000) != 0 {
        return None;
    }
    let list = (inst & 0xFFFF) as u16;
    if list == 0 {
        // `arm_block_transfer` treats an empty list as a 1-cycle no-op. Rare and
        // contested; declining costs nothing and avoids encoding a special case.
        return None;
    }
    let load = (inst & 0x0010_0000) != 0;
    let rn = (inst >> 16) & 0xF;
    // A load into R15 is a control-flow change: translatable only as a
    // dispatching terminator, and only in the unconditional form the scanner
    // produces. A writeback into R15 stays excluded outright.
    let loads_pc = load && (list & 0x8000) != 0;
    if (loads_pc && !(dispatch && cond == ArmCond::ALWAYS)) || rn == 15 {
        return None;
    }
    Some(BlockTransfer {
        cond,
        load,
        pre: (inst & 0x0100_0000) != 0,
        up: (inst & 0x0080_0000) != 0,
        writeback: (inst & 0x0020_0000) != 0,
        rn,
        list,
        cycle_cost: u32::from(list.count_ones()) + 2,
    })
}

/// `LDR`/`STR` with an immediate offset.
fn decode_transfer(inst: u32, cond: ArmCond) -> Option<SingleTransfer> {
    let t = decode_transfer_any_rd(inst, cond)?;
    // `LDR pc` is a control-flow change: translatable only as a dispatching
    // terminator. Rejecting it here as well keeps a scanner/translator
    // disagreement from turning into a miscompile rather than a declined block.
    if t.load && t.rd == 15 {
        return None;
    }
    Some(t)
}

/// [`decode_transfer`] without the `LDR pc` rejection — the dispatch path's
/// entry point, which has already vetted the encoding through
/// `is_pc_single_load`.
fn decode_transfer_any_rd(inst: u32, cond: ArmCond) -> Option<SingleTransfer> {
    let offset = if (inst & 0x0200_0000) != 0 {
        // Register offset: `Rm` through the same constant-amount barrel
        // shifter data processing uses. Bit 4 set is the UNDEFINED window,
        // and an R15 offset would read the block's synthetic R15 slot.
        if (inst & 0x10) != 0 || (inst & 0xF) == 15 {
            return None;
        }
        TransferOffset::Reg {
            rm: inst & 0xF,
            shift: Shift::decode((inst >> 5) & 3, (inst >> 7) & 0x1F),
        }
    } else {
        TransferOffset::Imm(inst & 0xFFF)
    };
    let rd = (inst >> 12) & 0xF;
    let rn = (inst >> 16) & 0xF;
    let t = SingleTransfer {
        cond,
        load: (inst & 0x0010_0000) != 0,
        byte: (inst & 0x0040_0000) != 0,
        pre: (inst & 0x0100_0000) != 0,
        up: (inst & 0x0080_0000) != 0,
        writeback: (inst & 0x0020_0000) != 0,
        rn,
        rd,
        offset,
        align_base: false,
    };
    // A writeback into R15 moves the program counter without going through
    // `write_pc` and stays excluded everywhere.
    if rn == 15 && t.writes_base() {
        return None;
    }
    Some(t)
}

fn decode_data_proc(inst: u32, cond: ArmCond) -> Option<DataProcImm> {
    let d = decode_data_proc_any_rd(inst, cond)?;
    // The scanner already excludes this outside dispatch mode; rejecting
    // rather than assuming keeps a disagreement between the two from becoming
    // a miscompile.
    if d.rd == 15 {
        return None;
    }
    Some(d)
}

/// [`decode_data_proc`] without the R15-destination rejection — the dispatch
/// path's entry point, which has already vetted the encoding through
/// `is_pc_data_proc`.
fn decode_data_proc_any_rd(inst: u32, cond: ArmCond) -> Option<DataProcImm> {
    let immediate_form = (inst & 0x0200_0000) != 0;
    if !immediate_form && (inst & 0x10) != 0 {
        return None; // register-specified shift, or one of the special encodings
    }

    let opcode = (inst >> 21) & 0xF;
    let set_flags = (inst & 0x0010_0000) != 0;
    // The MSR/MRS window, which also contains the DSP signed multiplies. With S
    // clear the interpreter either performs a PSR transfer or, when bits 7-4 are
    // non-zero, executes a result-discarding no-op; declining covers both.
    if !set_flags && (0x8..=0xB).contains(&opcode) {
        return None;
    }
    let rd = (inst >> 12) & 0xF;

    let operand = if immediate_form {
        let imm = inst & 0xFF;
        let rot = ((inst >> 8) & 0xF) * 2;
        if rot == 0 {
            Operand2::Immediate { value: imm, carry: None }
        } else {
            let v = imm.rotate_right(rot);
            Operand2::Immediate { value: v, carry: Some((v >> 31) & 1 != 0) }
        }
    } else {
        Operand2::Register {
            rm: inst & 0xF,
            shift: Shift::decode((inst >> 5) & 3, (inst >> 7) & 0x1F),
        }
    };

    let flags = if !set_flags {
        FlagMode::None
    } else if matches!(opcode, 0x0 | 0x1 | 0x8 | 0x9 | 0xC | 0xD | 0xE | 0xF) {
        FlagMode::Logical
    } else {
        FlagMode::Arithmetic
    };
    Some(DataProcImm { cond, opcode, rn: (inst >> 16) & 0xF, rd, flags, operand, align_rn: false })
}

// ---------------------------------------------------------------------------
// Loads and stores
// ---------------------------------------------------------------------------

/// Emit a call to one of the memory thunks.
///
/// Win64: `rcx` = MMU (byte/halfword loads) or `JitContext` (word loads,
/// stores and status operations), `rdx` = address, `r8` = value. Context-taking
/// thunks can publish boundary conditions into `JitContext::stop` as well as
/// reach memory. The 32-byte shadow space
/// is the callee's to use and the caller's to provide; the stack is already
/// 16-byte aligned because the prologue pushed exactly three registers, so
/// subtracting 32 keeps it aligned.
///
/// Every volatile register is destroyed by this — which is safe only because
/// guest registers live in memory behind `rbx` and CPSR is in `rsi`, both
/// callee-saved.
fn emit_thunk_call(e: &mut Emitter, thunk: usize, takes_context: bool) {
    if takes_context {
        e.mov_rr_64(Reg::Rcx, CTX);
    } else {
        e.mov_rm_64(Reg::Rcx, Mem::new(CTX, CTX_MMU));
    }
    e.alu_ri_64(AluOp::Sub, Reg::Rsp, 32);
    e.mov_ri_64(Reg::Rax, thunk as u64);
    e.call_r(Reg::Rax);
    e.alu_ri_64(AluOp::Add, Reg::Rsp, 32);
}

fn emit_transfer(e: &mut Emitter, t: SingleTransfer, pc: u32, cfg: EmitCfg) {
    // `Rn == 15` is a constant inside a block; it is only ever a source here,
    // because `decode_transfer` rejects a writeback into it.
    let load_base = |e: &mut Emitter, dst: Reg| {
        if t.rn == 15 {
            e.mov_ri(dst, if t.align_base { pc & !2 } else { pc });
        } else {
            e.mov_rm(dst, gpr_slot(t.rn));
        }
    };
    let apply_offset = |e: &mut Emitter, dst: Reg| {
        let dir = if t.up { AluOp::Add } else { AluOp::Sub };
        match t.offset {
            TransferOffset::Imm(0) => {}
            TransferOffset::Imm(v) => e.alu_ri(dir, dst, v),
            TransferOffset::Reg { rm, shift } => {
                // The DP shifter with the carry left alone — an address
                // computation publishes no flags. Recomputing it for the
                // writeback is exact: the thunk cannot touch guest
                // registers, and a load writes `rd` only after writeback.
                let (src, _) =
                    emit_operand2(e, Operand2::Register { rm, shift }, pc, false);
                alu(e, dir, dst, src);
            }
        }
    };

    // Address, into the thunk's second argument. A post-indexed form accesses
    // the *unmodified* base and applies the offset only to the writeback.
    load_base(e, VALUE);
    if t.pre {
        apply_offset(e, VALUE);
    }

    if !t.load {
        // `STR pc` stores R15 + 4, i.e. the instruction's address plus twelve.
        if t.rd == 15 {
            e.mov_ri(Reg::R8, pc.wrapping_add(4));
        } else {
            e.mov_rm(Reg::R8, gpr_slot(t.rd));
        }
    }

    emit_thunk_call(
        e,
        cfg.thunk(match (t.load, t.byte) {
            (true, false) => thunks::Access::ReadWord,
            (true, true) => thunks::Access::ReadByte,
            (false, false) => thunks::Access::WriteWord,
            (false, true) => thunks::Access::WriteByte,
        }),
        !t.load || !t.byte,
    );

    // Writeback, then the destination — the interpreter's order, and it matters
    // when the two are the same register. The base is recomputed rather than
    // preserved across the call: the thunk cannot touch the guest register file,
    // so re-reading it is exact, and every register that could have held it is
    // volatile.
    if t.writes_base() {
        load_base(e, CARRY);
        apply_offset(e, CARRY);
        e.mov_mr(gpr_slot(t.rn), CARRY);
    }
    if t.load {
        if t.rd == 15 {
            // `LDR pc`: `load_pc` — interworking on ARMv5, bit 0 forced low
            // on ARMv4T. Only reachable as a dispatching terminator.
            emit_pc_write_load(e, Reg::Rax, cfg);
        } else {
            e.mov_mr(gpr_slot(t.rd), Reg::Rax); // Win64 integer return value
        }
    }
}

/// Emit `LDRH`/`LDRSB`/`LDRSH`/`STRH`, mirroring [`emit_transfer`]'s shape:
/// address into the thunk argument, the call, writeback recomputed from the
/// register file (the thunk cannot touch it), then the destination — the
/// interpreter's order, which `Rd == Rn` makes observable. The register
/// offset needs no barrel shifter in this space, so it is a plain re-read;
/// sign extension happens host-side on the thunk's zero-extended return.
fn emit_half(e: &mut Emitter, t: HalfTransfer, pc: u32, cfg: EmitCfg) {
    let load_base = |e: &mut Emitter, dst: Reg| {
        if t.rn == 15 {
            e.mov_ri(dst, pc);
        } else {
            e.mov_rm(dst, gpr_slot(t.rn));
        }
    };
    let apply_offset = |e: &mut Emitter, dst: Reg| {
        let op = if t.up { AluOp::Add } else { AluOp::Sub };
        match t.offset {
            HalfOffset::Imm(0) => {}
            HalfOffset::Imm(v) => e.alu_ri(op, dst, v),
            HalfOffset::Reg(rm) => {
                if rm == 15 {
                    e.mov_ri(PACK, pc);
                } else {
                    e.mov_rm(PACK, gpr_slot(rm));
                }
                e.alu_rr(op, dst, PACK);
            }
        }
    };

    load_base(e, VALUE);
    if t.pre {
        apply_offset(e, VALUE);
    }
    if !t.load {
        // `STRH`: the original Rd value — writeback has not happened yet.
        // R15 cannot appear; `decode_halfword` rejects it.
        e.mov_rm(Reg::R8, gpr_slot(t.rd));
    }
    emit_thunk_call(
        e,
        cfg.thunk(match (t.load, t.kind) {
            (true, HalfKind::SignedByte) => thunks::Access::ReadByte,
            (true, _) => thunks::Access::ReadHalfword,
            (false, _) => thunks::Access::WriteHalfword,
        }),
        !t.load,
    );
    if t.writes_base() {
        load_base(e, CARRY);
        apply_offset(e, CARRY);
        e.mov_mr(gpr_slot(t.rn), CARRY);
    }
    if t.load {
        match t.kind {
            HalfKind::Half => {}
            HalfKind::SignedByte => e.movsx_rr8(Reg::Rax, Reg::Rax),
            HalfKind::SignedHalf => e.movsx_rr16(Reg::Rax, Reg::Rax),
        }
        e.mov_mr(gpr_slot(t.rd), Reg::Rax);
    }
}

/// An x86 source operand: an immediate, or a register already holding the value.
#[derive(Debug, Clone, Copy)]
enum Src {
    Imm(u32),
    Reg(Reg),
}

/// Apply an ALU operation with either source form.
fn alu(e: &mut Emitter, op: AluOp, dst: Reg, src: Src) {
    match src {
        Src::Imm(v) => e.alu_ri(op, dst, v),
        Src::Reg(r) => e.alu_rr(op, dst, r),
    }
}

/// Where the shifter carry-out comes from, for a logical operation that has to
/// publish it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShifterCarry {
    /// C is left as it was: `LSL #0`, and a rotation of zero on an immediate.
    Unchanged,
    Constant(bool),
    /// A 0 or 1 in [`CARRY`], captured before the shift ran.
    InRegister,
}

/// Materialise operand 2, and say where its carry-out came from.
///
/// `need_carry` is `set_flags && is_logical` — the only case that consumes the
/// shifter carry. When it is false the returned [`ShifterCarry::Unchanged`] is
/// never read, which is why not capturing it is safe rather than merely cheap.
fn emit_operand2(
    e: &mut Emitter,
    operand: Operand2,
    pc: u32,
    need_carry: bool,
) -> (Src, ShifterCarry) {
    match operand {
        Operand2::Immediate { value, carry } => (
            Src::Imm(value),
            match carry {
                None => ShifterCarry::Unchanged,
                Some(b) => ShifterCarry::Constant(b),
            },
        ),
        Operand2::Register { rm, shift } => {
            // R15 is a constant inside a block, so it is loaded as one and then
            // shifted normally — folding the shift too would be a second code
            // path for no measurable gain.
            if rm == 15 {
                e.mov_ri(OP2, pc);
            } else {
                e.mov_rm(OP2, gpr_slot(rm));
            }

            // Captured from the **original** value, before the shift runs and
            // before the ALU operation overwrites CF.
            let carry = match (need_carry, shift.carry_bit()) {
                (true, Some(bit)) => {
                    e.alu_rr(AluOp::Xor, CARRY, CARRY); // zero the upper bytes
                    e.bt_ri(OP2, bit);
                    e.setcc(Cond::B, CARRY); // CF -> the low byte
                    ShifterCarry::InRegister
                }
                _ => ShifterCarry::Unchanged,
            };

            match shift {
                Shift::Pass => {}
                Shift::By { op, amount, .. } => e.shift_ri(op, OP2, amount),
                Shift::Lsr32 => e.mov_ri(OP2, 0),
                Shift::Asr32 => e.shift_ri(ShiftOp::Sar, OP2, 31),
                Shift::Rrx => {
                    // A 33-bit rotate: the guest carry becomes the new top bit.
                    e.bt_ri(CPSR, 29);
                    e.shift_ri(ShiftOp::Rcr, OP2, 1);
                }
            }
            (Src::Reg(OP2), carry)
        }
    }
}

fn emit_data_proc_imm(e: &mut Emitter, op: DataProcImm, pc: u32) {
    // MOV and MVN ignore Rn. With an immediate operand their result is a
    // constant, and so are N and Z, so nothing has to run at all.
    if let Operand2::Immediate { value, carry } = op.operand {
        if matches!(op.opcode, 0xD | 0xF) {
            let result = if op.opcode == 0xD { value } else { !value };
            e.mov_mi(gpr_slot(op.rd), result);
            if op.rd == 15 {
                // A data-processing write to PC owes a refill; T is untouched.
                e.mov_mi(Mem::new(CTX, CTX_PC_MODIFIED), 1);
            }
            match op.flags {
                FlagMode::None => {}
                // The result is constant, so N and Z are too.
                FlagMode::NzOnly => emit_constant_logical_flags(e, result, None),
                FlagMode::Logical => emit_constant_logical_flags(e, result, carry),
                FlagMode::Arithmetic => unreachable!("MOV/MVN never publish V"),
            }
            return;
        }
    }

    let need_carry = op.needs_shifter_carry();
    let (src, shifter_carry) = emit_operand2(e, op.operand, pc, need_carry);

    match op.opcode {
        // MOV / MVN with a register operand: operand 1 is not read.
        0xD | 0xF => {
            match src {
                Src::Imm(v) => e.mov_ri(VALUE, v),
                Src::Reg(r) => e.mov_rr(VALUE, r),
            }
            if op.opcode == 0xF {
                e.not_r(VALUE); // `not` does not set flags on x86
            }
            if op.flags.writes_anything() {
                // Neither `mov` nor `not` publishes N/Z, so ask for them.
                e.test_rr(VALUE, VALUE);
            }
        }
        // RSB / RSC: operand 2 is the *left* operand.
        0x3 | 0x7 => {
            match src {
                Src::Imm(v) => e.mov_ri(VALUE, v),
                Src::Reg(r) => e.mov_rr(VALUE, r),
            }
            if op.opcode == 0x7 {
                emit_load_guest_carry_inverted(e);
                if op.rn == 15 {
                    e.alu_ri(AluOp::Sbb, VALUE, pc);
                } else {
                    e.alu_rm(AluOp::Sbb, VALUE, gpr_slot(op.rn));
                }
            } else if op.rn == 15 {
                e.alu_ri(AluOp::Sub, VALUE, if op.align_rn { pc & !2 } else { pc });
            } else {
                e.alu_rm(AluOp::Sub, VALUE, gpr_slot(op.rn));
            }
        }
        _ => {
            // Operand 1. `Rn == 15` is the literal-pool idiom `ADD rd,pc,#off`.
            if op.rn == 15 {
                e.mov_ri(VALUE, if op.align_rn { pc & !2 } else { pc });
            } else {
                e.mov_rm(VALUE, gpr_slot(op.rn));
            }
            match op.opcode {
                0x0 | 0x8 => alu(e, AluOp::And, VALUE, src), // AND, TST
                0x1 | 0x9 => alu(e, AluOp::Xor, VALUE, src), // EOR, TEQ
                0x2 | 0xA => alu(e, AluOp::Sub, VALUE, src), // SUB, CMP
                0x4 | 0xB => alu(e, AluOp::Add, VALUE, src), // ADD, CMN
                0x5 => {
                    emit_load_guest_carry(e);
                    alu(e, AluOp::Adc, VALUE, src); // ADC
                }
                0x6 => {
                    emit_load_guest_carry_inverted(e);
                    alu(e, AluOp::Sbb, VALUE, src); // SBC
                }
                0xC => alu(e, AluOp::Or, VALUE, src), // ORR
                // BIC is `op1 & !op2`. An immediate complement folds into the
                // constant; a register one costs a `not`, which leaves flags
                // alone and so does not disturb what the `and` publishes.
                0xE => match src {
                    Src::Imm(v) => e.alu_ri(AluOp::And, VALUE, !v),
                    Src::Reg(r) => {
                        e.not_r(r);
                        e.alu_rr(AluOp::And, VALUE, r);
                    }
                },
                other => unreachable!("opcode {other:#x} passed `decode` but has no emitter"),
            }
        }
    }

    // The store does not disturb flags, so it can precede the flag extraction.
    if !op.discards_result() {
        e.mov_mr(gpr_slot(op.rd), VALUE);
        if op.rd == 15 {
            // A data-processing write to PC owes a refill; T is untouched
            // (`arm_data_processing` -> `write_pc(result, false)`).
            e.mov_mi(Mem::new(CTX, CTX_PC_MODIFIED), 1);
        }
    }
    match op.flags {
        FlagMode::None => {}
        // N and Z from what the last operation published; C and V untouched.
        FlagMode::NzOnly => emit_logical_flags(e, ShifterCarry::Unchanged),
        FlagMode::Logical => emit_logical_flags(e, shifter_carry),
        FlagMode::Arithmetic => emit_arithmetic_flags(e, subtracts(op.opcode)),
    }
}

/// Does this opcode compute its result by subtracting, so that x86's CF is the
/// inverse of ARM's C?
fn subtracts(opcode: u32) -> bool {
    matches!(opcode, 0x2 | 0x3 | 0x6 | 0x7 | 0xA)
}

/// `bt cpsr, 29` — put the guest carry into the host CF for `adc`.
fn emit_load_guest_carry(e: &mut Emitter) {
    e.bt_ri(CPSR, 29);
}

/// The same, inverted, for `sbb`: ARM's `SBC` is `op1 + !op2 + C`, which is
/// `op1 - op2 - (1 - C)`, and x86's `sbb` subtracts `CF`. So CF must be `!C`.
fn emit_load_guest_carry_inverted(e: &mut Emitter) {
    e.bt_ri(CPSR, 29);
    e.cmc();
}

/// Merge a compile-time-constant N/Z/C into CPSR, leaving V alone.
fn emit_constant_logical_flags(e: &mut Emitter, result: u32, shifter_carry: Option<bool>) {
    let mut set = 0u32;
    let mut mask = N | Z;
    if (result & 0x8000_0000) != 0 {
        set |= N;
    }
    if result == 0 {
        set |= Z;
    }
    if let Some(carry) = shifter_carry {
        mask |= C;
        if carry {
            set |= C;
        }
    }
    e.alu_ri(AluOp::And, CPSR, !mask);
    if set != 0 {
        e.alu_ri(AluOp::Or, CPSR, set);
    }
}

/// Extract N and Z from the flags the last x86 operation left, and apply the
/// shifter carry. **V is not touched**, which is the ARM rule for a logical
/// operation and differs from x86, where the operation cleared OF.
fn emit_logical_flags(e: &mut Emitter, shifter_carry: ShifterCarry) {
    e.lahf(); // ah: bit 7 = SF, bit 6 = ZF  ->  eax bits 15, 14
    e.mov_rr(PACK, FLAGS);
    e.alu_ri(AluOp::And, PACK, 0x0000_C000);
    e.shift_ri(ShiftOp::Shl, PACK, 16); // -> bits 31, 30

    let clear = match shifter_carry {
        ShifterCarry::Unchanged => N | Z,
        _ => N | Z | C,
    };
    e.alu_ri(AluOp::And, CPSR, !clear);
    e.alu_rr(AluOp::Or, CPSR, PACK);
    match shifter_carry {
        ShifterCarry::Unchanged | ShifterCarry::Constant(false) => {}
        ShifterCarry::Constant(true) => e.alu_ri(AluOp::Or, CPSR, C),
        ShifterCarry::InRegister => {
            e.shift_ri(ShiftOp::Shl, CARRY, 29); // 0/1 -> ARM's C position
            e.alu_rr(AluOp::Or, CPSR, CARRY);
        }
    }
}

/// Extract all four flags from an arithmetic result.
///
/// `invert_carry` is set for the subtracting opcodes: x86 sets CF on borrow and
/// ARM clears C on borrow, so the bit has to be flipped before it is read.
fn emit_arithmetic_flags(e: &mut Emitter, invert_carry: bool) {
    if invert_carry {
        e.cmc();
    }
    e.lahf(); // eax bit 15 = SF, bit 14 = ZF, bit 8 = CF
    e.setcc(Cond::O, FLAGS); // eax bit 0 = OF; `al` is disjoint from `ah`

    e.mov_rr(PACK, FLAGS);
    e.alu_ri(AluOp::And, PACK, 0x0000_C000);
    e.shift_ri(crate::jit::x64::ShiftOp::Shl, PACK, 16); // N, Z -> 31, 30

    e.mov_rr(PACK2, FLAGS);
    e.alu_ri(AluOp::And, PACK2, 0x0000_0100);
    e.shift_ri(crate::jit::x64::ShiftOp::Shl, PACK2, 21); // C -> 29
    e.alu_rr(AluOp::Or, PACK, PACK2);

    e.mov_rr(PACK2, FLAGS);
    e.alu_ri(AluOp::And, PACK2, 1);
    e.shift_ri(crate::jit::x64::ShiftOp::Shl, PACK2, 28); // V -> 28
    e.alu_rr(AluOp::Or, PACK, PACK2);

    e.alu_ri(AluOp::And, CPSR, !NZCV);
    e.alu_rr(AluOp::Or, CPSR, PACK);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lay `words` out contiguously from `start`, the shape `compile` takes.
    fn seq(start: u32, words: &[u32]) -> Vec<(u32, u32)> {
        words.iter().enumerate().map(|(i, &w)| (start + (i as u32) * 4, w)).collect()
    }

    /// Decode something the tests already know is data processing.
    fn dp(inst: u32) -> DataProcImm {
        match decode(inst).unwrap_or_else(|| panic!("{inst:#010x} did not decode")) {
            Instr::DataProc(d) => d,
            other => panic!("{inst:#010x} decoded as {other:?}, not data processing"),
        }
    }

    /// The emitted code addresses [`JitContext`] by numeric offset, so a field
    /// reordering the constants do not follow would corrupt guest state
    /// silently. Measured against the real type rather than recomputed.
    #[test]
    fn context_offsets_match_the_declared_layout() {
        let ctx = JitContext::new(std::ptr::null_mut(), 0, 0);
        let base = &ctx as *const JitContext as usize;
        let off = |p: usize| (p - base) as i32;
        assert_eq!(off(&ctx.gpr as *const _ as usize), CTX_GPR);
        assert_eq!(off(&ctx.cpsr as *const _ as usize), CTX_CPSR);
        assert_eq!(off(&ctx.exec_lr as *const _ as usize), CTX_EXEC_LR);
        assert_eq!(off(&ctx.mmu as *const _ as usize), CTX_MMU);
        assert_eq!(off(&ctx.pc_modified as *const _ as usize), CTX_PC_MODIFIED);
        assert_eq!(off(&ctx.class_hits as *const _ as usize), CTX_CLASS_HITS);
        assert_eq!(off(&ctx.extra_cycles as *const _ as usize), CTX_EXTRA_CYCLES);
        assert_eq!(off(&ctx.words as *const _ as usize), CTX_WORDS);
        assert_eq!(off(&ctx.exit_idx as *const _ as usize), CTX_EXIT_IDX);
        assert_eq!(off(&ctx.retired as *const _ as usize), CTX_RETIRED);
        assert_eq!(off(&ctx.cpu as *const _ as usize), CTX_CPU);
        assert_eq!(off(&ctx.cycles as *const _ as usize), CTX_CYCLES);
        assert_eq!(off(&ctx.stop as *const _ as usize), CTX_STOP);
        assert_eq!(off(&ctx.budget as *const _ as usize), CTX_BUDGET);
        assert_eq!(off(&ctx.last_exit as *const _ as usize), CTX_LAST_EXIT);
        assert_eq!(off(&ctx.code_epoch as *const _ as usize), CTX_CODE_EPOCH);
    }

    /// The linked-exit emission contract, checked structurally: which exits
    /// exist, where they may link to, and that every epilogue lies inside the
    /// emitted code. The behavioural half lives in the differential tests.
    #[test]
    fn linked_exits_carry_the_documented_targets() {
        let slots = LinkSlots { addrs: [0x1000, 0x2000, 0x3000] };

        // A store-ended body falls through to the next address.
        let body = seq(0x0200_0000, &[0xE281_0001, 0xE581_2004]);
        let c = compile_with(&body, false, Some(&slots), None, EmitCfg::ARM9).unwrap();
        assert!(c.body_entry > 0, "the prologue has non-zero length");
        assert_eq!(c.exits.len(), 1);
        assert_eq!(c.exits[0].index, 0);
        assert_eq!(c.exits[0].target, Some(0x0200_0008), "fall-through");
        assert!(!c.exits[0].sets_pc);
        assert!(c.exits[0].epilogue_offset < c.code.len());

        // A terminal unconditional branch links to its own target.
        let body = seq(0x0200_0000, &[0xE281_0001, 0xEAFF_FFFD]); // B .-4
        let c = compile_with(&body, false, Some(&slots), None, EmitCfg::ARM9).unwrap();
        assert_eq!(c.exits.len(), 1);
        assert_eq!(c.exits[0].target, Some(0x0200_0000), "backward branch to start");
        assert!(c.exits[0].sets_pc);

        // A terminal *conditional* branch has two live successors and gets a
        // slot for each — one shared slot would link whichever ran last.
        let body = seq(0x0200_0000, &[0xE281_0001, 0x1AFF_FFFD]); // BNE .-4
        let c = compile_with(&body, false, Some(&slots), None, EmitCfg::ARM9).unwrap();
        assert_eq!(c.exits.len(), 2);
        assert_eq!(c.exits[0].index, 2, "taken path is emitted first");
        assert_eq!(c.exits[0].target, Some(0x0200_0000));
        assert!(c.exits[0].sets_pc);
        assert_eq!(c.exits[1].index, 0);
        assert_eq!(c.exits[1].target, Some(0x0200_0008), "fall-through");
        assert!(!c.exits[1].sets_pc);

        // A followed conditional branch mid-body adds the early exit.
        let body = seq(0x0200_0000, &[0xE281_0001, 0x1A00_0010, 0xE282_0002]);
        let c = compile_with(&body, false, Some(&slots), None, EmitCfg::ARM9).unwrap();
        assert_eq!(c.exits.len(), 2);
        assert_eq!(c.exits[0].index, 1, "the early exit");
        assert_eq!(c.exits[0].target, Some(0x0200_0004 + 8 + 0x40));
        assert!(c.exits[0].sets_pc);
        assert_eq!(c.exits[1].index, 0);
        assert_eq!(c.exits[1].target, Some(0x0200_000C));

        // A truncated translation has no linkable fall-through: the next
        // instruction is one the interpreter must run.
        let body = seq(0x0200_0000, &[0xE281_0001, 0xE081_0312]);
        let c = compile_with(&body, false, Some(&slots), None, EmitCfg::ARM9).unwrap();
        assert_eq!(c.instructions, 1, "stopped at the register-specified shift");
        assert_eq!(c.exits.len(), 1);
        assert_eq!(c.exits[0].target, None);

        // Without linking, nothing changes shape.
        let c = compile(&seq(0x0200_0000, &[0xE281_0001]), false).unwrap();
        assert!(c.exits.is_empty());
    }

    /// The dispatching terminators: shape, targets and the decode gates.
    #[test]
    fn dispatching_exits_carry_the_documented_shape() {
        let slots = LinkSlots { addrs: [0x1000, 0x2000, 0x3000] };
        let plan = DispatchPlan { table: 0x4000, mask: 0xFFF };

        // BX ends the block with a dispatching exit: no static target.
        let body = seq(0x0200_0000, &[0xE281_0001, 0xE12F_FF12]); // ADD; BX r2
        let c = compile_with(&body, false, Some(&slots), Some(&plan), EmitCfg::ARM9).unwrap();
        assert_eq!(c.instructions, 2);
        assert_eq!(c.exits.len(), 1);
        assert!(c.exits[0].dispatch);
        assert_eq!(c.exits[0].target, None);
        assert!(c.exits[0].sets_pc);
        assert_eq!(c.static_cycles, 1 + 3, "BX costs 3, like a taken branch");
        assert_eq!(c.unconditional_hist[0], 1, "BX retires in bucket 0");

        // ...and so does an LDM that loads R15.
        let body = seq(0x0200_0000, &[0xE8BD_8004]); // LDMFD sp!,{r2,pc}
        let c = compile_with(&body, false, Some(&slots), Some(&plan), EmitCfg::ARM9).unwrap();
        assert_eq!(c.instructions, 1);
        assert!(c.exits[0].dispatch);
        assert_eq!(c.static_cycles, 2 + 2, "count + 2, the interpreter's formula");

        // Without a dispatch plan the same encodings still decline — a
        // scanner/translator disagreement must decline, not miscompile.
        assert!(compile_with(&body, false, Some(&slots), None, EmitCfg::ARM9).is_none());
        assert!(decode(0xE12F_FF12).is_none(), "BX without dispatch");
        assert!(decode_dispatch(0x112F_FF12, true, true).is_none(), "conditional BX stays excluded");
        assert!(
            decode_dispatch(0xE8FD_8004, true, true).is_none(),
            "the S-bit exception return stays excluded"
        );
    }

    #[test]
    fn the_supported_subset_is_exactly_what_is_documented() {
        assert!(decode(0xE281_0001).is_some(), "ADD r0,r1,#1");
        assert!(decode(0xE291_0001).is_some(), "ADDS is supported now");
        assert!(decode(0x1281_0001).is_some(), "ADDNE is supported now");
        assert!(decode(0xE2A1_0001).is_some(), "ADC is supported now");
        assert!(decode(0xE351_0001).is_some(), "CMP r1,#1");
        assert!(decode(0xE081_0002).is_some(), "the register form is supported now");
        assert!(decode(0xE081_0312).is_none(), "register-*specified* shift amount");
        assert!(decode(0xE329_F0FF).is_some(), "MSR immediate is supported now");
        assert!(decode(0xE281_F001).is_none(), "Rd = pc");
        assert!(decode(0xF281_0001).is_none(), "NV space");
    }

    /// The rotated immediate needs no barrel shifter, and a zero rotation means
    /// "carry unchanged" rather than "carry zero" — a distinction that would
    /// otherwise clobber C on every `MOV rd,#imm`.
    #[test]
    fn the_immediate_and_its_carry_are_resolved_at_compile_time() {
        assert_eq!(
            dp(0xE3A0_00FF).operand,
            Operand2::Immediate { value: 0xFF, carry: None },
            "a rotation of zero preserves C"
        );
        assert_eq!(
            dp(0xE3A0_0402).operand,
            Operand2::Immediate { value: 0x0200_0000, carry: Some(false) },
            "the carry is bit 31 of the rotated value"
        );
        assert_eq!(
            dp(0xE3A0_0102).operand,
            Operand2::Immediate { value: 0x02u32.rotate_right(2), carry: Some(true) }
        );
    }

    /// The `#0` amount encodes a *different instruction* for each shift type,
    /// and each has its own carry source. Getting `LSR #0` wrong yields the
    /// unshifted register instead of zero, which looks plausible forever.
    #[test]
    fn the_zero_shift_amount_means_something_different_per_type() {
        assert_eq!(Shift::decode(0, 0), Shift::Pass, "LSL #0 is the register itself");
        assert_eq!(Shift::decode(1, 0), Shift::Lsr32, "LSR #0 means LSR #32");
        assert_eq!(Shift::decode(2, 0), Shift::Asr32, "ASR #0 means ASR #32");
        assert_eq!(Shift::decode(3, 0), Shift::Rrx, "ROR #0 is RRX");

        assert_eq!(Shift::Pass.carry_bit(), None, "LSL #0 leaves C alone");
        assert_eq!(Shift::Lsr32.carry_bit(), Some(31));
        assert_eq!(Shift::Asr32.carry_bit(), Some(31));
        assert_eq!(Shift::Rrx.carry_bit(), Some(0));
    }

    /// The carry of a non-zero shift comes from a bit of the *original* value,
    /// and the bit differs between a left and a right shift.
    #[test]
    fn the_shifter_carry_bit_matches_the_interpreter_formula() {
        // LSL #n takes bit 32-n; LSR/ASR/ROR #n take bit n-1.
        assert_eq!(Shift::decode(0, 1).carry_bit(), Some(31));
        assert_eq!(Shift::decode(0, 31).carry_bit(), Some(1));
        assert_eq!(Shift::decode(1, 1).carry_bit(), Some(0));
        assert_eq!(Shift::decode(1, 31).carry_bit(), Some(30));
        assert_eq!(Shift::decode(2, 8).carry_bit(), Some(7));
        assert_eq!(Shift::decode(3, 8).carry_bit(), Some(7));
    }

    /// The register form must be told apart from everything sharing its
    /// encoding space. Bit 4 is the separator; these are the encodings that
    /// would otherwise be executed as data processing.
    #[test]
    fn the_register_form_excludes_everything_that_shares_its_space() {
        assert!(decode(0xE081_0002).is_some(), "ADD r0,r1,r2");
        assert!(decode(0xE1A0_0182).is_some(), "MOV r0,r2,LSL #3");
        assert!(decode(0xE1B0_0062).is_some(), "MOVS r0,r2,RRX");

        assert!(decode(0xE081_0312).is_none(), "register-specified shift (bit 4)");
        assert!(decode(0xE003_0291).is_some(), "MUL is supported now");
        assert!(decode(0xE083_2190).is_some(), "UMULL is supported now");
        assert!(decode(0xE00F_0291).is_none(), "MUL with an R15 field stays excluded");
        assert!(decode(0xE103_0092).is_none(), "SWP");
        assert!(decode(0xE1C0_40D0).is_none(), "LDRD");
        assert!(decode(0xE12F_FF10).is_none(), "BX");
        assert!(decode(0xE16F_1F10).is_some(), "CLZ is supported now (ARMv5)");
        assert!(
            decode_dispatch(0xE16F_1F10, false, false).is_none(),
            "CLZ is not an ARMv4 encoding"
        );
        assert!(decode(0xEE07_CF9A).is_some(), "CP15 cache maintenance is a compiled no-op");
        assert!(decode(0xEE07_0F90).is_none(), "the WFI CP15 op still declines");
        assert!(decode(0xEE01_0F10).is_none(), "the control-register CP15 op still declines");
        assert!(decode(0xE10F_0000).is_some(), "MRS CPSR is supported now");
        assert!(decode(0xE129_F000).is_some(), "MSR CPSR is supported now");
        assert!(decode(0xE14F_0000).is_some(), "MRS SPSR is supported now");
        assert!(decode(0xE169_F000).is_none(), "MSR SPSR stays excluded");
        assert!(decode(0xE16F_0281).is_none(), "SMULBB (bit 7 set, bit 4 clear)");
        assert!(decode(0xE103_0052).is_none(), "QADD");
    }

    #[test]
    fn opcode_classification() {
        for (inst, logical, discards, subtracts_) in [
            (0xE201_0001u32, true, false, false),  // AND
            (0xE311_0001, true, true, false),      // TST
            (0xE251_0001, false, false, true),     // SUBS
            (0xE351_0001, false, true, true),      // CMP
            (0xE291_0001, false, false, false),    // ADDS
            (0xE371_0001, false, true, false),     // CMN
            (0xE3E0_0000, true, false, false),     // MVN
        ] {
            let d = dp(inst);
            assert_eq!(d.is_logical(), logical, "{inst:#010x} logical");
            assert_eq!(d.discards_result(), discards, "{inst:#010x} discards");
            assert_eq!(subtracts(d.opcode), subtracts_, "{inst:#010x} subtracts");
        }
    }

    #[test]
    fn an_unsupported_leading_instruction_compiles_to_nothing() {
        // `ADD r0,r1,r2,LSL r3` — a register-specified shift amount, which needs
        // run-time branching on the amount and is not translated.
        assert!(compile(&seq(0x0200_0000, &[0xE081_0312]), false).is_none(), "register-shift leads");
        assert!(compile(&seq(0x0200_0000, &[]), false).is_none(), "an empty body");
    }

    /// Translation stops at the first unsupported instruction rather than
    /// skipping it — skipping would silently drop a guest instruction.
    #[test]
    fn translation_stops_at_the_first_unsupported_instruction() {
        let body = [0xE281_0001, 0xE282_0002, 0xE081_0312, 0xE283_0003];
        let c = compile(&seq(0x0200_0000, &body), false).unwrap();
        assert_eq!(c.instructions, 2, "stopped at the register-specified shift");
        assert_eq!(c.static_cycles, 2, "one cycle per data-processing instruction");
        assert_eq!(c.unconditional_hist[1], 2, "both are immediate-operand data processing");
    }

    /// A block's cycle count is a **sum**: a transfer costs three where data
    /// processing costs one, so counting instructions would under-report by 2
    /// for every load in the block and shift the whole peripheral schedule.
    #[test]
    fn cycles_are_summed_per_instruction_class() {
        // ADD r0,r1,#1 ; LDR r2,[r1] ; STR r2,[r1,#4]
        let body = [0xE281_0001u32, 0xE591_2000, 0xE581_2004];
        let c = compile(&seq(0x0200_0000, &body), false).unwrap();
        assert_eq!(c.instructions, 3);
        assert_eq!(c.static_cycles, 1 + 3 + 3, "one for the ADD, three for each transfer");
        assert_eq!(c.unconditional_hist[1], 1, "data processing, immediate");
        assert_eq!(c.unconditional_hist[2], 2, "single transfers");
    }

    /// A **conditional** transfer charges only its first cycle statically. The
    /// interpreter returns 1 for a failed condition without reaching the
    /// handler, so charging 3 up front over-reports every skipped load — which
    /// is exactly what the differential harness caught on `LDRCS`.
    #[test]
    fn a_conditional_transfer_charges_only_its_first_cycle_statically() {
        let unconditional = compile(&seq(0x0200_0000, &[0xE591_2000]), false).unwrap();
        assert_eq!(unconditional.static_cycles, 3, "LDR always runs, so all three");

        let conditional = compile(&seq(0x0200_0000, &[0x2591_2000]), false).unwrap();
        assert_eq!(conditional.static_cycles, 1, "LDRCS may not run at all");
        assert_eq!(conditional.unconditional_hist, [0; 6], "nothing is known to retire");
    }

    /// The transfer forms the recompiler declines, each for a stated reason.
    #[test]
    fn transfers_outside_the_subset_are_declined() {
        assert!(decode(0xE591_2000).is_some(), "LDR r2,[r1]");
        assert!(decode(0xE5D1_2000).is_some(), "LDRB r2,[r1]");
        assert!(decode(0xE581_2004).is_some(), "STR r2,[r1,#4]");
        assert!(decode(0xE4B1_2004).is_some(), "LDRT-style post-index with writeback");

        assert!(decode(0xE791_2003).is_some(), "register offset is supported now");
        assert!(decode(0xE791_2013).is_none(), "bit 4 set is the UNDEFINED space");
        assert!(decode(0xE791_200F).is_none(), "an R15 offset register stays excluded");
        assert!(decode(0xE59F_F000).is_none(), "LDR pc");
        assert!(decode(0xE49F_0004).is_none(), "post-indexed writeback into R15");
        assert!(decode(0xE5BF_0004).is_none(), "explicit writeback into R15");
        assert!(decode(0xE59F_0004).is_some(), "...but reading R15 as a base is fine");
    }

    /// The writeback rule is direction-dependent: a load whose destination is
    /// its own base skips the writeback, a store does not.
    #[test]
    fn the_writeback_rule_differs_between_load_and_store() {
        let t = |inst: u32| match decode(inst).unwrap() {
            Instr::Transfer(t) => t,
            other => panic!("{other:?}"),
        };
        // LDR r1,[r1],#4 — destination is the base, so no writeback.
        assert!(!t(0xE491_1004).writes_base(), "load into its own base");
        // LDR r2,[r1],#4 — different destination, so the base is written.
        assert!(t(0xE491_2004).writes_base(), "load with a distinct destination");
        // STR r1,[r1],#4 — a store always writes back.
        assert!(t(0xE481_1004).writes_base(), "store into its own base");
        // LDR r2,[r1,#4] — pre-indexed with no W bit writes nothing.
        assert!(!t(0xE591_2004).writes_base(), "pre-indexed without writeback");
    }

    /// Conditional instructions are counted at run time, not statically, because
    /// the interpreter only bumps `arm_class_hist` when the condition passes.
    #[test]
    fn conditional_instructions_are_not_counted_statically() {
        let c = compile(&seq(0x0200_0000, &[0xE281_0001, 0x1282_0002]), false).unwrap();
        assert_eq!(c.instructions, 2);
        assert_eq!(c.unconditional_hist[1], 1, "only the AL one is known to retire");
    }
}
