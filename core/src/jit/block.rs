//! Block scanner: find the straight-line run of ARM instructions the emitter is
//! allowed to translate as one unit.
//!
//! # What a block is
//!
//! A *body* of instructions that provably execute in address order, followed by
//! one *exit* instruction the interpreter runs. Nothing in the body may change
//! R15, take an exception, or need state the emitter does not model — so the
//! emitter can keep guest registers in host registers for the whole body and
//! flush them once, which is the entire reason the recompiler is faster than the
//! interpreter.
//!
//! # Conservative by construction
//!
//! [`ends_block`] answers "must a block stop *before* this instruction?" and is
//! wrong in only one direction: a false positive costs one interpreted
//! instruction, a false negative is silent corruption. Every predicate is
//! therefore written to catch the whole encoding *window* rather than the exact
//! instruction, and anything not positively recognised as safe ends the block.
//!
//! # Why the exit is interpreted rather than compiled
//!
//! `B`/`BL` are 13.5% of retired instructions and are cheap to emit, so folding
//! them into the block is worthwhile — but it is a *separate* change with its
//! own correctness argument (the link register, the pipeline refill, the
//! ARMv5 NV-space `BLX`). Keeping every exit uniform here means the scanner has
//! one contract to test, and promoting branches later only removes work.

/// Longest body the scanner will produce.
///
/// **Refuted, do not re-attempt: raising this to 32 measured 0%.** Block
/// entries moved 13,765,563 -> 13,757,972 and the mean body stayed at 3.11, so
/// the cap is simply not binding. The hypothesis it was testing — that
/// `ExitReason::Branch` ("branch, no room in body") at 14.6% of entries was the
/// cap in disguise — is wrong: that exit is dominated by the ARMv5 `BLX(imm)`
/// NV-space encoding, which `is_direct_branch` never accepts, so no amount of
/// room admits it.
///
/// The interpreter polls for an interrupt before *every* instruction; a compiled
/// body can only be interrupted at its boundaries, so the *applied* cap is the
/// worst-case interrupt latency the recompiler introduces, in guest
/// instructions.
///
/// The value that matters is not the one that makes a micro-benchmark look best,
/// it is the largest one that leaves `nds_boot_handshake_audio_guard` and
/// `nds_ingame_audio_and_perf_report` unchanged — a broken ARM9/ARM7 interleave
/// still draws a plausible picture and simply produces no sound, and those two
/// probes are the only things that see it. Raise the *applied* cap only with a
/// measurement of both.
pub const MAX_BODY_INSTRS: usize = 16;

/// Why the scanner stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockExit {
    /// The next instruction changes control flow, needs an interpreter-only
    /// facility, or is not recognised. `Arm9Cpu::step` runs it, and scanning
    /// resumes after it.
    Interpret {
        /// The word itself, so a caller can log or count exits by kind without
        /// re-reading guest memory.
        instruction: u32,
        /// Why it was rejected. Diagnostics only — every reason is handled the
        /// same way — but "which reason dominates" is what tells the next
        /// milestone which encoding is worth compiling.
        reason: ExitReason,
    },
    /// [`MAX_BODY_INSTRS`] instructions were accepted and the next one is
    /// ordinary. The block ends anyway so interrupts stay responsive.
    LengthCap,
    /// The last instruction in the body writes memory, and a block must not
    /// continue past one. See [`writes_memory`].
    AfterStore,
    /// The last instruction in the body is a direct `B`/`BL`. See
    /// [`is_direct_branch`].
    AfterBranch,
    /// The last instruction in the body redirects R15 to a **run-time** value:
    /// an unconditional `BX`/`BLX(reg)` or an `LDM` that loads R15. Only formed
    /// when the scanner is asked to (`dispatch` in [`scan_state`]); the
    /// translator ends such a block with an indirect-dispatch exit.
    AfterExchange,
    /// The last instruction in the body is an unconditional `MSR` to CPSR. It
    /// is compiled (committed through a thunk), and the block ends so that an
    /// interrupt the write unmasked is honoured before the next instruction.
    AfterStatus,
}

/// An unconditional `MSR CPSR_fields, op` (either encoding) — compiled as a
/// block terminator. SPSR writes stay excluded.
pub fn is_msr_cpsr(inst: u32) -> bool {
    (inst >> 28) == 0xE
        && ((inst & 0x0FF0_FFF0) == 0x0120_F000 || (inst & 0x0FF0_F000) == 0x0320_F000)
}

/// `MRS Rd, CPSR` with an ordinary destination — a plain body instruction
/// (one host mov). The SPSR form stays excluded.
pub fn is_mrs_cpsr(inst: u32) -> bool {
    (inst >> 28) != 0xF
        && (inst & 0x0FFF_0FFF) == 0x010F_0000
        && ((inst >> 12) & 0xF) != 15
}

/// An unconditional `BX Rm` / `BLX Rm` — a run-time branch a dispatching block
/// may **end with**. Conditional forms keep two live successors and still stop
/// the block the old way.
pub fn is_register_exchange(inst: u32) -> bool {
    (inst >> 28) == 0xE
        && ((inst & 0x0FFF_FFF0) == 0x012F_FF10 || (inst & 0x0FFF_FFF0) == 0x012F_FF30)
}

/// An unconditional `LDM` (S clear, base not R15) whose register list includes
/// R15 — a run-time branch a dispatching block may end with. The S-bit form is
/// an exception return and stays excluded.
pub fn is_pc_load_multiple(inst: u32) -> bool {
    (inst >> 28) == 0xE
        && (inst & 0x0E50_8000) == 0x0810_8000
        && ((inst >> 16) & 0xF) != 15
}

/// An unconditional word `LDR` into R15 with an immediate offset — the
/// jump-table / `LDR pc,[sp],#4` return idiom — a run-time branch a
/// dispatching block may end with.
///
/// The interpreter (`arm_single_transfer`) performs it as `load_pc(value)`:
/// ARMv5 **interworking** (T from bit 0 of the loaded word), writeback before
/// the PC write, three cycles. Register offsets need the barrel shifter and
/// stay excluded, as does the unpredictable `LDRB pc`.
pub fn is_pc_single_load(inst: u32) -> bool {
    (inst >> 28) == 0xE && (inst & 0x0E50_F000) == 0x0410_F000
}

/// An unconditional data-processing write to R15 — `MOV pc,lr` and friends —
/// a run-time branch a dispatching block may end with.
///
/// The interpreter (`arm_data_processing`) performs these as
/// `write_pc(result, false)`: **no interworking** (T untouched, unlike a load
/// into PC on ARMv5) and one cycle. Excluded here: the S-bit forms (CPSR is
/// restored from SPSR — an exception return), register-specified shifts and
/// every class-0 special encoding (bit 4), and the result-discarding opcode
/// window, which with S clear is MSR/MRS.
pub fn is_pc_data_proc(inst: u32) -> bool {
    if (inst >> 28) != 0xE || (inst & 0x0C00_0000) != 0 {
        return false; // conditional, or not class 0
    }
    if ((inst >> 12) & 0xF) != 15 || (inst & 0x0010_0000) != 0 {
        return false; // not an R15 destination, or the S-bit exception return
    }
    if (inst & 0x0200_0000) == 0 && (inst & 0x10) != 0 {
        return false; // register-specified shift, or a class-0 special encoding
    }
    !matches!((inst >> 21) & 0xF, 0x8..=0xB)
}

/// The classification [`ends_block`] assigns to an instruction it rejects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExitReason {
    /// `B`, `BL`, or the ARMv5 `BLX(imm)` NV-space encoding.
    Branch,
    /// `BX` / `BLX(reg)`.
    BranchExchange,
    /// Any other instruction whose destination register is R15.
    WritesPc,
    /// `LDM` with R15 in the register list.
    LoadsPc,
    /// `SWI`. Handled by the BIOS HLE inside the interpreter.
    SoftwareInterrupt,
    /// `MCR`/`MRC` p15. `Arm9Cpu::step` intercepts these before the interpreter
    /// ever sees them, and one of them (`c7,c0,4`) halts the core.
    Cp15,
    /// Any other coprocessor or undefined encoding.
    Coprocessor,
    /// `MSR`, which can change the mode or the T bit and therefore invalidates
    /// the emitter's assumptions about which register bank is live.
    StatusWrite,
    /// `LDM`/`STM` with the S bit: user-bank transfer or an exception return.
    BlockTransferSBit,
}

/// A straight-line run of instructions, plus how it ends.
///
/// The body is a fixed array rather than a `Vec` because **most scans are
/// thrown away**: on the player's scene 84% of scan attempts end in a decline,
/// and a heap allocation per attempt measured as one of the two dominant costs
/// of running the recompiler at all. `MAX_BODY_INSTRS` words is 64 bytes of
/// stack, which is cheaper than one `malloc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Guest address of the first instruction of [`Self::body`].
    pub start: u32,
    /// `(address, instruction)` in **execution** order.
    ///
    /// Execution order is not address order: an unconditional direct branch is
    /// *followed* at scan time rather than ending the block, which fuses basic
    /// blocks into a trace. Each instruction therefore has to carry its own
    /// address — R15 is a compile-time constant derived from it, the page guard
    /// is derived from all of them, and neither can be recovered from `start`
    /// plus an index any more.
    body: [(u32, u32); MAX_BODY_INSTRS],
    body_len: usize,
    /// Thumb state. Instructions are 16-bit, addresses step by two, and R15
    /// reads as `address + 4` rather than `+ 8`.
    pub thumb: bool,
    /// How the block ends.
    pub exit: BlockExit,
}

impl Block {
    /// `(address, instruction)` pairs in execution order. Empty when the very
    /// first instruction at `start` must be interpreted.
    pub fn body(&self) -> &[(u32, u32)] {
        &self.body[..self.body_len]
    }

    /// Guest address of the instruction the block stopped *before*, if any.
    ///
    /// Only meaningful for [`BlockExit::Interpret`]; the other exits consumed
    /// their last instruction.
    pub fn exit_address(&self) -> u32 {
        let step = if self.thumb { 2 } else { 4 };
        match self.body().last() {
            Some(&(addr, _)) => addr.wrapping_add(step),
            None => self.start,
        }
    }

    /// Guest address execution continues at when nothing branches away.
    pub fn fall_through(&self) -> u32 {
        self.exit_address()
    }

    /// Bytes one instruction occupies in this block's instruction set.
    pub fn step_bytes(&self) -> u32 {
        if self.thumb {
            2
        } else {
            4
        }
    }
}

/// Scan forward from `start`, reading instruction words through `fetch`.
///
/// `fetch` is a closure rather than a `&mut NdsMmu` so the scanner can be tested
/// against a plain array and so it cannot accidentally perform a *data* read on
/// the guest bus — scanning must never pop the IPC FIFO or advance the gamecard
/// cursor, and the type makes that impossible rather than merely unlikely.
pub fn scan<F: FnMut(u32) -> u32>(start: u32, fetch: F) -> Block {
    scan_state(start, false, false, fetch)
}

/// Scan in either instruction set. `fetch` returns a 32-bit ARM word or a
/// zero-extended 16-bit Thumb halfword.
///
/// `follow_conditional` lets the trace continue down the fall-through of one
/// conditional direct branch; the translator then emits its taken path as an
/// early exit. Off leaves the scanner's previous behaviour exactly.
pub fn scan_state<F: FnMut(u32) -> u32>(
    start: u32,
    thumb: bool,
    follow_conditional: bool,
    fetch: F,
) -> Block {
    scan_state_dispatch(start, thumb, follow_conditional, false, fetch)
}

/// [`scan_state`], with `dispatch` letting an unconditional `BX`/`BLX(reg)` or
/// `LDM {..,pc}` **end** the body (as [`BlockExit::AfterExchange`]) instead of
/// stopping before it. Off leaves the previous behaviour exactly.
pub fn scan_state_dispatch<F: FnMut(u32) -> u32>(
    start: u32,
    thumb: bool,
    follow_conditional: bool,
    dispatch: bool,
    mut fetch: F,
) -> Block {
    if thumb {
        return scan_thumb(start, follow_conditional, dispatch, fetch);
    }
    let mut body = [(0u32, 0u32); MAX_BODY_INSTRS];
    let mut body_len = 0usize;
    let mut addr = start;
    let mut took_conditional = false;
    let done = |body: [(u32, u32); MAX_BODY_INSTRS], body_len, exit| Block {
        start,
        body,
        body_len,
        thumb: false,
        exit,
    };
    loop {
        let instruction = fetch(addr);

        // Checked *before* `ends_block`, which still classifies a direct branch
        // as a reason to stop. That is deliberate: `ends_block` keeps its
        // conservative contract — "this instruction may not sit in a body" — and
        // the exceptions are spelled out here.
        if is_direct_branch(instruction) && body_len < MAX_BODY_INSTRS {
            body[body_len] = (addr, instruction);
            body_len += 1;

            // An **unconditional** direct branch has a compile-time target and
            // only one successor, so the trace can continue there instead of
            // ending. That is the single biggest lever on block length:
            // branches are 13.5% of retired instructions and were the binding
            // terminator, holding the mean block at ~4 where stores alone would
            // allow ~12.
            //
            // A conditional one has two live successors and still ends the
            // block.
            if (instruction >> 28) == 0xE {
                let target = branch_target(addr, instruction);
                let already_here = body[..body_len].iter().any(|&(a, _)| a == target);
                // Stop on a backward edge into the trace — otherwise a loop
                // would be unrolled until the length cap, wasting code space on
                // iterations that a cached block already covers — and stop when
                // there is no room for the target's first instruction.
                if !already_here && body_len < MAX_BODY_INSTRS {
                    addr = target;
                    continue;
                }
            } else if follow_conditional && !took_conditional && body_len < MAX_BODY_INSTRS {
                // **A conditional branch has two live successors.** The trace
                // continues down the *fall-through* and the translator emits the
                // taken path as an early exit out of the block.
                //
                // Measured: this is the dominant terminator, ending **46.0% of
                // all block entries** — more than stores (26.3%) and everything
                // else combined. Blocks average 3.03 instructions largely
                // because of it.
                //
                // **One** per trace, deliberately. Each early exit needs its own
                // retired-count/cycles/histogram tuple, and a single alternate
                // exit keeps that to one extra tuple per block instead of a
                // table; a second conditional branch ends the trace as before.
                took_conditional = true;
                addr = addr.wrapping_add(4);
                continue;
            }
            return done(body, body_len, BlockExit::AfterBranch);
        }

        // A run-time branch the block may end WITH, when dispatching is on:
        // its target is in a register (or loaded), so the trace cannot follow
        // it — but the translator can compute it and dispatch indirectly.
        if dispatch
            && body_len < MAX_BODY_INSTRS
            && (is_register_exchange(instruction)
                || is_pc_load_multiple(instruction)
                || is_pc_data_proc(instruction)
                || is_pc_single_load(instruction))
        {
            body[body_len] = (addr, instruction);
            body_len += 1;
            return done(body, body_len, BlockExit::AfterExchange);
        }

        // `MRS Rd, CPSR` is an ordinary body instruction (the translator has
        // the value pinned in a register); `MSR` to CPSR is compiled but ends
        // the block — it can unmask a pending interrupt, which must be
        // honoured before the next instruction runs.
        if is_mrs_cpsr(instruction) && body_len < MAX_BODY_INSTRS {
            body[body_len] = (addr, instruction);
            body_len += 1;
            addr = addr.wrapping_add(4);
            continue;
        }
        if is_msr_cpsr(instruction) && body_len < MAX_BODY_INSTRS {
            body[body_len] = (addr, instruction);
            body_len += 1;
            return done(body, body_len, BlockExit::AfterStatus);
        }

        if let Some(reason) = ends_block(instruction) {
            return done(body, body_len, BlockExit::Interpret { instruction, reason });
        }
        if body_len == MAX_BODY_INSTRS {
            return done(body, body_len, BlockExit::LengthCap);
        }
        body[body_len] = (addr, instruction);
        body_len += 1;
        if writes_memory(instruction) {
            return done(body, body_len, BlockExit::AfterStore);
        }
        addr = addr.wrapping_add(4);
    }
}

/// Target of a Thumb F18 unconditional / F16 conditional branch at `addr`.
/// R15 reads as `addr + 4` in Thumb; the encoded offsets are halfword counts.
pub fn thumb_branch_target(addr: u32, inst: u16) -> u32 {
    let i = u32::from(inst);
    let delta = if (i & 0xF800) == 0xE000 {
        // F18: signed 11-bit halfword offset.
        let mut s = (i & 0x07FF) as i32;
        if s & 0x0400 != 0 {
            s |= !0x07FF;
        }
        s << 1
    } else {
        // F16: signed 8-bit halfword offset.
        (((i & 0xFF) as i8 as i32) << 1)
    };
    (addr.wrapping_add(4) as i32).wrapping_add(delta) as u32
}

/// Target of a Thumb F19 `BL` pair whose first half sits at `addr`.
/// `LR_tmp = (addr+4) + (s11 << 12)`, then `target = (LR_tmp + (off << 1)) & !1`.
pub fn thumb_bl_target(addr: u32, first: u16, second: u16) -> u32 {
    let mut s = (u32::from(first) & 0x07FF) as i32;
    if s & 0x0400 != 0 {
        s |= !0x07FF;
    }
    let lr_tmp = (addr.wrapping_add(4) as i32).wrapping_add(s << 12) as u32;
    lr_tmp.wrapping_add((u32::from(second) & 0x07FF) << 1) & !1
}

/// Thumb counterpart of [`scan_state`].
///
/// Thumb has no condition field on ordinary instructions — only the branch
/// formats redirect control flow, and they now get the same trace treatment
/// ARM's got: F18 (and the F19 `BL` pair, whose target is equally a
/// compile-time constant) are *followed*, and one F16 conditional branch may
/// have its fall-through followed with the taken path as an early exit.
fn scan_thumb<F: FnMut(u32) -> u32>(
    start: u32,
    follow_conditional: bool,
    dispatch: bool,
    mut fetch: F,
) -> Block {
    let mut body = [(0u32, 0u32); MAX_BODY_INSTRS];
    let mut body_len = 0usize;
    let mut addr = start;
    let mut took_conditional = false;
    let done = |body: [(u32, u32); MAX_BODY_INSTRS], body_len, exit| Block {
        start,
        body,
        body_len,
        thumb: true,
        exit,
    };
    loop {
        let instruction = fetch(addr) & 0xFFFF;
        let i16w = instruction as u16;

        // F18 unconditional branch: a compile-time target — follow it.
        if (instruction & 0xF800) == 0xE000 && body_len < MAX_BODY_INSTRS {
            body[body_len] = (addr, instruction);
            body_len += 1;
            let target = thumb_branch_target(addr, i16w);
            let already_here = body[..body_len].iter().any(|&(a, _)| a == target);
            if !already_here && body_len < MAX_BODY_INSTRS {
                addr = target;
                continue;
            }
            return done(body, body_len, BlockExit::AfterBranch);
        }

        // F19 BL: two halves that only mean something together. With both
        // present the link value and target are compile-time constants, so
        // the pair is followed exactly like ARM's `BL`. A BLX suffix (state
        // switch) or an orphaned half stays an interpreter exit.
        if (instruction & 0xF800) == 0xF000 && body_len + 1 < MAX_BODY_INSTRS {
            let second = fetch(addr.wrapping_add(2)) & 0xFFFF;
            if (second & 0xF800) == 0xF800 {
                body[body_len] = (addr, instruction);
                body[body_len + 1] = (addr.wrapping_add(2), second);
                body_len += 2;
                let target = thumb_bl_target(addr, i16w, second as u16);
                let already_here = body[..body_len].iter().any(|&(a, _)| a == target);
                if !already_here && body_len < MAX_BODY_INSTRS {
                    addr = target;
                    continue;
                }
                return done(body, body_len, BlockExit::AfterBranch);
            }
            return done(
                body,
                body_len,
                BlockExit::Interpret { instruction, reason: ExitReason::Branch },
            );
        }

        // F16 conditional branch (conditions 0..=0xD; 0xE is undefined and
        // 0xF is the SWI window, both left to `ends_block_thumb`).
        if (instruction & 0xF000) == 0xD000
            && (instruction >> 8) & 0xF <= 0xD
            && body_len < MAX_BODY_INSTRS
        {
            body[body_len] = (addr, instruction);
            body_len += 1;
            if follow_conditional && !took_conditional && body_len < MAX_BODY_INSTRS {
                took_conditional = true;
                addr = addr.wrapping_add(2);
                continue;
            }
            return done(body, body_len, BlockExit::AfterBranch);
        }

        // The run-time branches a dispatching Thumb block may END with:
        // F5 `BX`/`BLX(reg)` and F14 `POP {list, pc}`.
        if dispatch
            && body_len < MAX_BODY_INSTRS
            && ((instruction & 0xFF00) == 0x4700 || (instruction & 0xFF00) == 0xBD00)
        {
            body[body_len] = (addr, instruction);
            body_len += 1;
            return done(body, body_len, BlockExit::AfterExchange);
        }

        if let Some(reason) = ends_block_thumb(i16w) {
            return done(body, body_len, BlockExit::Interpret { instruction, reason });
        }
        if body_len == MAX_BODY_INSTRS {
            return done(body, body_len, BlockExit::LengthCap);
        }
        body[body_len] = (addr, instruction);
        body_len += 1;
        if writes_memory_thumb(i16w) {
            return done(body, body_len, BlockExit::AfterStore);
        }
        addr = addr.wrapping_add(2);
    }
}

/// Must a Thumb block stop *before* this instruction?
///
/// Mirrors [`ends_block`]'s contract: wrong in only one direction, and anything
/// not positively recognised as safe ends the block. The format numbers are the
/// ARM architecture manual's, matching the cascade in `GbaCpu::execute_thumb`
/// so the two decoders cannot disagree about what an encoding *is*.
pub fn ends_block_thumb(inst: u16) -> Option<ExitReason> {
    let i = u32::from(inst);
    // F5 hi-register operations. `BX`/`BLX` live here, and the other three can
    // name R15 as a destination through the H1 bit.
    if (i & 0xFC00) == 0x4400 {
        if (i & 0xFF00) == 0x4700 {
            return Some(ExitReason::BranchExchange);
        }
        let rd = (i & 7) | ((i >> 4) & 8);
        return (rd == 15).then_some(ExitReason::WritesPc);
    }
    // F14 PUSH/POP. `POP {..,pc}` is a return.
    if (i & 0xF600) == 0xB400 {
        let load = (i & 0x0800) != 0;
        let r_bit = (i & 0x0100) != 0;
        return (load && r_bit).then_some(ExitReason::LoadsPc);
    }
    // F17 SWI, and the undefined `BKPT` window that shares its prefix.
    if (i & 0xFF00) == 0xDF00 {
        return Some(ExitReason::SoftwareInterrupt);
    }
    // F16 conditional branch, F18 unconditional, F19 BL/BLX pair. The pair is
    // two instructions that only mean something together, so neither half may
    // sit in a body.
    if (i & 0xF000) == 0xD000 || (i & 0xF800) == 0xE000 || (i & 0xF000) == 0xF000 {
        return Some(ExitReason::Branch);
    }
    // The ARMv5 BLX suffix shares F19's space and also switches instruction set.
    if (i & 0xF800) == 0xE800 {
        return Some(ExitReason::Branch);
    }
    None
}

/// Can this Thumb instruction write guest memory? See [`writes_memory`].
pub fn writes_memory_thumb(inst: u16) -> bool {
    let i = u32::from(inst);
    // F7 register-offset and F8 sign-extended: bit 11 clear is a store.
    if (i & 0xF000) == 0x5000 {
        return (i & 0x0800) == 0;
    }
    // F9 immediate-offset word/byte, F10 halfword, F11 SP-relative: bit 11 is L.
    if (i & 0xE000) == 0x6000 || (i & 0xF000) == 0x8000 || (i & 0xF000) == 0x9000 {
        return (i & 0x0800) == 0;
    }
    // F14 PUSH.
    if (i & 0xF600) == 0xB400 {
        return (i & 0x0800) == 0;
    }
    // F15 STMIA.
    if (i & 0xF000) == 0xC000 {
        return (i & 0x0800) == 0;
    }
    false
}

/// Where a direct `B`/`BL` at `addr` goes.
///
/// R15 reads as `addr + 8` while the branch executes, and the encoded offset is
/// a signed 24-bit **word** count.
pub fn branch_target(addr: u32, inst: u32) -> u32 {
    let mut offset = (inst & 0x00FF_FFFF) as i32;
    if (offset & 0x0080_0000) != 0 {
        offset |= !0x00FF_FFFF;
    }
    addr.wrapping_add(8).wrapping_add((offset << 2) as u32)
}

/// Is this a direct `B` or `BL` — a branch whose target is encoded in the
/// instruction rather than taken from a register?
///
/// This is the one control-flow change a block may **contain**, as its final
/// instruction, because its target is a compile-time constant: `R15` inside a
/// block is known, so `pc + 8 + (offset << 2)` is too, and so is `BL`'s link
/// value. Everything else that moves `R15` — `BX`, `BLX(reg)`, a load into
/// `R15`, a data-processing write to it — depends on a runtime value and stays
/// excluded by [`ends_block`].
///
/// Branches are 13.5% of retired instructions, and every block used to end
/// *before* one and then pay a full recompiler dispatch that could only decline.
///
/// The ARMv5 `BLX(immediate)` in the `cond == 0b1111` space is **not** included:
/// it switches the core to Thumb, which a block does not model.
pub fn is_direct_branch(inst: u32) -> bool {
    (inst >> 28) != 0xF && (inst & 0x0E00_0000) == 0x0A00_0000
}

/// Can this instruction write guest memory?
///
/// # Why a block must end here
///
/// The interpreter does not read a block's instructions up front — it fetches
/// instruction `j` during step `j - 2`, **interleaved with execution**. So a
/// store at step `i` is seen by every fetch after it, and a scanner that read
/// the whole run in advance would compile the *pre-store* words while the
/// interpreter executes the post-store ones. That is self-modifying code, which
/// DS games genuinely produce by decompressing into RAM.
///
/// Ending the body after the store makes the two agree exactly: every word the
/// block contains was fetched by the interpreter before the store ran, and the
/// two pipeline-refill words are captured at translation time for the same
/// reason.
///
/// The cost is small. Stores are roughly 8% of retired instructions, so a run
/// reaches one after about a dozen — close to [`MAX_BODY_INSTRS`] anyway.
///
/// Conservative on purpose: it names every memory-writing encoding, including
/// the ones no milestone translates yet, so the scanner does not have to be
/// revisited when they are.
pub fn writes_memory(inst: u32) -> bool {
    match (inst >> 26) & 3 {
        // Class 0: SWP (which writes), and the halfword/doubleword stores.
        0 => {
            if (inst & 0x0FB0_0FF0) == 0x0100_0090 {
                return true; // SWP / SWPB
            }
            if (inst & 0x0E00_0090) != 0x0000_0090 {
                return false; // not the halfword-transfer family
            }
            let sh = (inst >> 5) & 3;
            let load = (inst & 0x0010_0000) != 0;
            // SH = 01 is a halfword transfer whose direction is the L bit;
            // SH = 10/11 with L clear are LDRD/STRD, and STRD (SH = 11) writes.
            match sh {
                0b01 => !load,
                0b11 => !load,
                _ => false,
            }
        }
        // Class 1: STR / STRB.
        1 => (inst & 0x0010_0000) == 0,
        // Class 2: STM (bit 25 clear selects the block transfer).
        2 => (inst & 0x0200_0000) == 0 && (inst & 0x0010_0000) == 0,
        // Class 3: coprocessor stores. Never placed in a body anyway, since
        // `ends_block` rejects the whole class.
        _ => true,
    }
}

/// Must a block stop before this ARM instruction? `None` means it is safe to
/// place in a body.
///
/// Written as a cascade over the same bits-27-26 class gate the interpreter
/// uses, so the two decoders cannot disagree about what an encoding *is* —
/// only about whether it is compilable.
///
/// The condition field is not consulted. A conditional branch that fails still
/// has to be *executed* to know that, and a body must be straight-line
/// unconditionally, so `BNE` ends a block exactly as `B` does.
pub fn ends_block(inst: u32) -> Option<ExitReason> {
    // ARMv5 NV space (cond == 0b1111). It is not "never": `BLX(imm)` lives here
    // and writes both R14 and R15. Everything else in the window is a hint
    // instruction (`PLD`) the interpreter treats as a no-op, which is not worth
    // a special case.
    if (inst >> 28) == 0xF {
        return Some(ExitReason::Branch);
    }

    let class = (inst >> 26) & 3;
    let rd = (inst >> 12) & 0xF;

    match class {
        // ---- Class 0: data processing and the register-operand special cases.
        0 => {
            let opcode = (inst >> 21) & 0xF;
            let s = (inst & 0x0010_0000) != 0;
            let immediate = (inst & 0x0200_0000) != 0;

            // MSR in both operand forms: opcodes 8..=B with S clear. It can
            // rewrite the mode and the T bit, which changes which register bank
            // the emitter's pinned registers even refer to.
            //
            // The register form additionally requires bits 7-4 == 0; without
            // that test the whole ARMv5TE saturating/multiply extension space
            // aliases onto it, which is a bug the interpreter has already been
            // burned by (see `execute_arm_reg_class`).
            let is_psr_window = !s && (0x8..=0xB).contains(&opcode);
            if is_psr_window && (immediate || inst & 0xF0 == 0) {
                // MRS (bit 21 clear) only *reads* the status register, so it is
                // safe; MSR (bit 21 set) is not.
                if (inst & 0x0020_0000) != 0 {
                    return Some(ExitReason::StatusWrite);
                }
                return if rd == 15 { Some(ExitReason::WritesPc) } else { None };
            }

            if !immediate {
                // BX / BLX(reg) share one encoding family, distinguished only by
                // bits 7-4. Matching the family rather than each opcode keeps
                // this conservative: an undecoded member of it still ends the
                // block.
                if (inst & 0x0FFF_FF00) == 0x012F_FF00 {
                    return Some(ExitReason::BranchExchange);
                }
                // Multiply and multiply-long put their destination in bits
                // 19-16 (and 19-16/15-12 for the long forms), not bits 15-12,
                // so the generic `rd` test below would look at the wrong field.
                if (inst & 0x0F80_00F0) == 0x0080_0090 {
                    let rd_hi = (inst >> 16) & 0xF;
                    let rd_lo = (inst >> 12) & 0xF;
                    return (rd_hi == 15 || rd_lo == 15).then_some(ExitReason::WritesPc);
                }
                if (inst & 0x0FC0_00F0) == 0x0000_0090 {
                    let rd_mul = (inst >> 16) & 0xF;
                    return (rd_mul == 15).then_some(ExitReason::WritesPc);
                }
            }

            // ARMv5TE DSP extension space, gated exactly as `arm_dsp_extension`
            // gates it: bits 27-23 == 0b00010 with bit 20 clear.
            //
            // The trap here is that the space holds **two families with
            // different register layouts** — the interpreter's own comment says
            // so, and using one layout for both is a bug it has already been
            // burned by. The saturating family puts Rd at bits 15-12, which the
            // generic test below happens to cover; the signed multiplies put it
            // at bits 19-16, which the generic test does *not*. Without this,
            // `SMULBB pc,r1,r2` reads as an ordinary write to r0 and gets
            // compiled into a body.
            let in_dsp_window =
                (inst & 0x0F80_0000) == 0x0100_0000 && (inst & 0x0010_0000) == 0;
            if in_dsp_window {
                let rd_hi = (inst >> 16) & 0xF;
                let rd_lo = (inst >> 12) & 0xF;
                if (inst >> 4) & 0xF == 0b0101 {
                    // QADD / QSUB / QDADD / QDSUB: Rd at 15-12.
                    return (rd_lo == 15).then_some(ExitReason::WritesPc);
                }
                if (inst & 0x80) != 0 && (inst & 0x10) == 0 {
                    // SMLAxy / SMLAWy / SMULWy / SMLALxy / SMULxy: Rd at 19-16,
                    // and the SMLAL form (op == 0b10) also writes RdLo at 15-12.
                    let writes_lo = (inst >> 21) & 3 == 0b10;
                    return (rd_hi == 15 || (writes_lo && rd_lo == 15))
                        .then_some(ExitReason::WritesPc);
                }
                // Anything else in the window is MRS/MSR (already handled) or
                // undefined; fall through to the generic destination test.
            }

            // Everything else in class 0 — data processing, SWP, the halfword
            // and doubleword transfers, the ARMv5TE DSP space — writes bits
            // 15-12. LDRD/STRD write the pair `rd`/`rd+1`, so `rd == 14` would
            // reach R15 as well.
            if rd == 15 {
                return Some(ExitReason::WritesPc);
            }
            let is_double = !immediate && (inst & 0x0E00_00D0) == 0x0000_00D0;
            if is_double && rd == 14 {
                return Some(ExitReason::WritesPc);
            }
            None
        }

        // ---- Class 1: LDR/STR.
        1 => {
            if rd == 15 {
                return Some(ExitReason::WritesPc); // LDR pc
            }
            // ...and the *base* register is written too, whenever the W bit is
            // set or the form is post-indexed. `arm_single_transfer` assigns
            // `gpr[rn]` directly, so `LDR r0,[pc],#4` moves R15 without ever
            // setting `pc_modified` — invisible to a check that only looks at
            // the destination, and fatal to a block that treats R15 as a
            // compile-time constant.
            let writeback = (inst & 0x0020_0000) != 0;
            let post = (inst & 0x0100_0000) == 0;
            let rn = (inst >> 16) & 0xF;
            (rn == 15 && (writeback || post)).then_some(ExitReason::WritesPc)
        }

        // ---- Class 2: LDM/STM and B/BL.
        2 => {
            if (inst & 0x0200_0000) != 0 {
                return Some(ExitReason::Branch);
            }
            // S bit: user-bank transfer, or `LDM {..,pc}^` which restores CPSR
            // from SPSR. Out of scope by design.
            if (inst & 0x0040_0000) != 0 {
                return Some(ExitReason::BlockTransferSBit);
            }
            // R15 in the list is a load-to-PC (and, on ARMv5, an interworking
            // one). A *store* of R15 is harmless, so only LDM is rejected.
            let load = (inst & 0x0010_0000) != 0;
            if load && (inst & (1 << 15)) != 0 {
                return Some(ExitReason::LoadsPc);
            }
            // Writeback to R15 moves the program counter without going through
            // `write_pc`; see the class-1 note.
            let writeback = (inst & 0x0020_0000) != 0;
            let rn = (inst >> 16) & 0xF;
            (rn == 15 && writeback).then_some(ExitReason::WritesPc)
        }

        // ---- Class 3: coprocessor space and SWI.
        _ => {
            if (inst & 0x0F00_0000) == 0x0F00_0000 {
                return Some(ExitReason::SoftwareInterrupt);
            }
            // `Arm9Cpu::is_cp15_transfer`, kept bit-for-bit identical: bits
            // 27-24 == 1110, coprocessor field == 15, bit 4 set.
            if ((inst >> 24) & 0xF) == 0xE && ((inst >> 8) & 0xF) == 0xF && (inst & 0x10) != 0 {
                // The sub-operations `execute_cp15_transfer` matches have real
                // effects (control/TCM writes remap memory, `c7,c0,4` halts)
                // and still end the block. Everything else — the cache
                // maintenance the census measured at 43% of scan ends — is an
                // architectural no-op on this emulator and stays in the body.
                if is_cp15_nop(inst) {
                    return None;
                }
                return Some(ExitReason::Cp15);
            }
            Some(ExitReason::Coprocessor)
        }
    }
}

/// Is this CP15 transfer one of the encodings `execute_cp15_transfer` treats
/// as a no-op — i.e. NOT one of its matched arms (control register, either
/// TCM control, or the `c7,c0,4` wait-for-interrupt)?
///
/// Mirrors that `match` exactly and must keep doing so: an arm added there
/// without extending this predicate would compile a real side effect as a
/// no-op. The translator applies the same test, so a disagreement declines
/// rather than miscompiles.
pub fn is_cp15_nop(inst: u32) -> bool {
    let crn = (inst >> 16) & 0xF;
    let crm = inst & 0xF;
    let opcode_2 = (inst >> 5) & 0x7;
    !matches!((crn, crm, opcode_2), (1, 0, 0) | (9, 1, 0) | (9, 1, 1) | (7, 0, 4))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jit::difftest::{ArmClass, Fixture, Rng, CODE_BASE};

    /// Scan a program held in an array, addressed from [`CODE_BASE`].
    fn scan_program(program: &[u32]) -> Block {
        scan(CODE_BASE, |addr| {
            let i = (addr - CODE_BASE) as usize / 4;
            program.get(i).copied().unwrap_or(0xEF00_0000) // SWI past the end
        })
    }

    const NOP: u32 = 0xE1A0_0000; // MOV r0,r0

    /// Just the instruction words of a scanned body.
    fn words(block: &Block) -> Vec<u32> {
        block.body().iter().map(|&(_, w)| w).collect()
    }

    /// A direct branch is the one control-flow change a body may *contain*, and
    /// it terminates the body rather than being excluded from it.
    #[test]
    fn a_run_of_ordinary_instructions_follows_its_branch() {
        let block = scan_program(&[NOP, NOP, NOP, 0xEA00_0000]);
        assert_eq!(words(&block), [NOP, NOP, NOP, 0xEA00_0000]);
        assert!(
            matches!(block.exit, BlockExit::Interpret { reason: ExitReason::SoftwareInterrupt, .. }),
            "an unconditional branch is followed, so the trace ended at the sentinel beyond it"
        );
    }

    /// ...but every *other* control-flow change is still excluded, and one at
    /// the start leaves nothing for the recompiler to do.
    #[test]
    fn a_non_foldable_control_flow_instruction_yields_an_empty_body() {
        let block = scan_program(&[0xE12F_FF10]); // BX r0: target is a register
        assert!(block.body().is_empty());
        assert_eq!(
            block.exit,
            BlockExit::Interpret { instruction: 0xE12F_FF10, reason: ExitReason::BranchExchange }
        );
        assert_eq!(block.exit_address(), CODE_BASE, "the interpreter runs the first instruction");
    }

    /// The branch forms that may and may not be folded, side by side. `BX` and
    /// `BLX(reg)` take their target from a register, and `BLX(imm)` switches the
    /// core to Thumb, so none of them has a compile-time target.
    #[test]
    fn only_direct_branches_are_foldable() {
        assert!(is_direct_branch(0xEA00_0000), "B");
        assert!(is_direct_branch(0xEB00_0000), "BL");
        assert!(is_direct_branch(0x1A00_0000), "BNE");
        assert!(!is_direct_branch(0xFA00_0000), "BLX(imm) switches to Thumb");
        assert!(!is_direct_branch(0xE12F_FF10), "BX");
        assert!(!is_direct_branch(0xE12F_FF30), "BLX(reg)");
        assert!(!is_direct_branch(0xE8BD_8000), "LDM with pc");
        assert!(!is_direct_branch(NOP), "MOV");
    }

    #[test]
    fn the_length_cap_ends_a_block_of_ordinary_instructions() {
        let block = scan_program(&vec![NOP; MAX_BODY_INSTRS + 8]);
        assert_eq!(block.body().len(), MAX_BODY_INSTRS);
        assert_eq!(block.exit, BlockExit::LengthCap);
    }

    /// One case per [`ExitReason`], with the encoding spelled out. These are the
    /// instructions a body must never contain, so they are named individually
    /// rather than left to the fuzz to stumble over.
    #[test]
    fn every_exit_reason_is_recognised() {
        let cases: [(u32, ExitReason, &str); 12] = [
            (0xEA00_0000, ExitReason::Branch, "B +0"),
            (0xEB00_0000, ExitReason::Branch, "BL +0"),
            (0xFA00_0000, ExitReason::Branch, "BLX(imm), NV space"),
            (0xE12F_FF10, ExitReason::BranchExchange, "BX r0"),
            (0xE12F_FF30, ExitReason::BranchExchange, "BLX r0"),
            (0xE1A0_F000, ExitReason::WritesPc, "MOV pc,r0"),
            (0xE28F_F004, ExitReason::WritesPc, "ADD pc,pc,#4"),
            (0xE59F_F000, ExitReason::WritesPc, "LDR pc,[pc]"),
            (0xE8BD_8000, ExitReason::LoadsPc, "LDMFD sp!,{pc}"),
            (0xE8FD_0003, ExitReason::BlockTransferSBit, "LDMFD sp!,{r0,r1}^"),
            (0xEF00_0000, ExitReason::SoftwareInterrupt, "SWI 0"),
            (0xEE07_0F90, ExitReason::Cp15, "MCR p15,0,r0,c7,c0,4 (WFI)"),
        ];
        for (inst, expected, what) in cases {
            assert_eq!(ends_block(inst), Some(expected), "{what} ({inst:#010x})");
        }
    }

    /// MSR ends a block; MRS does not. They share an encoding window, and
    /// getting the split wrong either loses every `MRS` to the interpreter or —
    /// far worse — compiles a mode switch.
    #[test]
    fn msr_ends_a_block_but_mrs_does_not() {
        assert_eq!(ends_block(0xE10F_0000), None, "MRS r0,CPSR is readable state");
        assert_eq!(ends_block(0xE129_F000), Some(ExitReason::StatusWrite), "MSR CPSR_fc,r0");
        assert_eq!(ends_block(0xE329_F0FF), Some(ExitReason::StatusWrite), "MSR CPSR_fc,#0xFF");
    }

    /// The ARMv5TE extension space sits inside the MSR window and is told apart
    /// only by bits 7-4. Worse, it holds two families whose register layouts
    /// disagree: the saturating instructions put Rd at bits 15-12 and the signed
    /// multiplies put it at 19-16. A scanner that assumes one layout compiles a
    /// write to R15 from the other, so both are pinned here.
    #[test]
    fn the_armv5te_extension_space_uses_two_register_layouts() {
        // Saturating family (bits 7-4 == 0b0101): Rd at 15-12, Rn at 19-16.
        assert_eq!(ends_block(0xE103_0052), None, "QADD r0,r2,r3");
        assert_eq!(ends_block(0xE103_F052), Some(ExitReason::WritesPc), "QADD pc,r2,r3");
        assert_eq!(ends_block(0xE10F_0052), None, "QADD r0,r2,pc — R15 is only a source");

        // Signed-multiply family (bit 7 set, bit 4 clear): Rd at 19-16.
        assert_eq!(ends_block(0xE160_0281), None, "SMULBB r0,r1,r2");
        assert_eq!(ends_block(0xE16F_0281), Some(ExitReason::WritesPc), "SMULBB pc,r1,r2");
        // SMLALBB (op == 0b10) writes RdHi at 19-16 *and* RdLo at 15-12.
        assert_eq!(ends_block(0xE143_2281), None, "SMLALBB r2,r3,r1,r2");
        assert_eq!(ends_block(0xE14F_2281), Some(ExitReason::WritesPc), "SMLALBB r2,pc,..");
        assert_eq!(ends_block(0xE143_F281), Some(ExitReason::WritesPc), "SMLALBB pc,r3,..");

        // CLZ and BX live in the same bits-27-23 window and must survive it.
        assert_eq!(ends_block(0xE16F_1F10), None, "CLZ r1,r0");
        assert_eq!(ends_block(0xE16F_FF10), Some(ExitReason::WritesPc), "CLZ pc,r0");
        assert_eq!(ends_block(0xE12F_FF10), Some(ExitReason::BranchExchange), "BX r0");
    }

    /// Multiply keeps its destination in bits 19-16, so the generic bits-15-12
    /// test does not apply to it.
    #[test]
    fn multiply_destinations_are_read_from_the_right_field() {
        assert_eq!(ends_block(0xE003_0291), None, "MUL r3,r1,r2");
        assert_eq!(ends_block(0xE00F_0291), Some(ExitReason::WritesPc), "MUL pc,r1,r2");
        assert_eq!(ends_block(0xE083_2190), None, "UMULL r2,r3,r0,r1");
        assert_eq!(ends_block(0xE08F_2190), Some(ExitReason::WritesPc), "UMULL r2,pc,r0,r1");
        assert_eq!(ends_block(0xE083_F190), Some(ExitReason::WritesPc), "UMULL pc,r3,r0,r1");
    }

    /// `LDRD r14,[..]` loads R14 *and R15*, so the pair has to be checked, not
    /// just the named register. Decoded as a plain halfword transfer this reads
    /// as an ordinary write to R14 and would be compiled into a body.
    #[test]
    fn ldrd_into_r14_reaches_r15() {
        assert_eq!(ends_block(0xE1C0_40D0), None, "LDRD r4,[r0]");
        assert_eq!(ends_block(0xE1C0_E0D0), Some(ExitReason::WritesPc), "LDRD r14,[r0]");
        assert_eq!(ends_block(0xE1C0_E0F0), Some(ExitReason::WritesPc), "STRD r14,[r0]");
    }

    /// A transfer writes its **base** register too, whenever the W bit is set or
    /// the form is post-indexed. With `Rn == 15` that moves the program counter
    /// through a plain register assignment, with no `pc_modified` and nothing in
    /// the destination field to give it away.
    #[test]
    fn writeback_to_r15_is_a_control_flow_change() {
        // LDR r0,[pc,#4] — pre-indexed, no writeback: R15 is only read.
        assert_eq!(ends_block(0xE59F_0004), None, "LDR r0,[pc,#4]");
        // LDR r0,[pc],#4 — post-indexed, so R15 is written.
        assert_eq!(ends_block(0xE49F_0004), Some(ExitReason::WritesPc), "LDR r0,[pc],#4");
        // LDR r0,[pc,#4]! — explicit writeback.
        assert_eq!(ends_block(0xE5BF_0004), Some(ExitReason::WritesPc), "LDR r0,[pc,#4]!");
        // The same base with an ordinary register stays compilable.
        assert_eq!(ends_block(0xE491_0004), None, "LDR r0,[r1],#4");
        // LDM/STM with writeback to R15.
        assert_eq!(ends_block(0xE8BF_0003), Some(ExitReason::WritesPc), "LDMIA pc!,{{r0,r1}}");
        assert_eq!(ends_block(0xE8B1_0003), None, "LDMIA r1!,{{r0,r1}}");
    }

    /// STM may store R15 — that only *reads* it — while LDM may not load it.
    #[test]
    fn storing_r15_is_allowed_but_loading_it_is_not() {
        assert_eq!(ends_block(0xE92D_8001), None, "STMFD sp!,{{r0,pc}}");
        assert_eq!(ends_block(0xE8BD_8001), Some(ExitReason::LoadsPc), "LDMFD sp!,{{r0,pc}}");
    }

    /// **The oracle.** A body must be straight-line: run the interpreter from
    /// the block's start for exactly `body.len()` instructions and no step may
    /// request a pipeline refill, because that is precisely what "R15 changed"
    /// means. Fuzzed over the corpus, this is what would catch an `ends_block`
    /// predicate that is too permissive.
    #[test]
    fn a_scanned_body_never_changes_pc_under_the_interpreter() {
        for seed in 1..80u64 {
            let mut rng = Rng::new(seed);
            let program = crate::jit::difftest::random_program(&ArmClass::ALL, 40, &mut rng);
            // Scan the **same memory the interpreter will execute**, not a copy
            // of the generated array. A followed branch can leave the program in
            // either direction, and a scanner reading a sentinel where the
            // interpreter reads real memory would compare two different
            // programs — which is a bug in the oracle, not a finding.
            let fixture = Fixture::arm(&program, seed);
            let (mut cpu, mut mmu) = fixture.instantiate();
            let block = scan(CODE_BASE, |addr| mmu.read_word_arm9(addr));

            // **The oracle, stronger now that a body is a trace.** A trace
            // claims a specific execution *path*, so every entry's recorded
            // address must be exactly where the interpreter is when it reaches
            // that point. A control-flow instruction wrongly admitted to a body
            // and a mis-computed branch target both surface here, at the first
            // entry that diverges — and the second failure mode did not exist
            // before branches were followed.
            for (i, &(addr, word)) in block.body().iter().enumerate() {
                // R15 leads by two instruction widths once primed; a pending
                // refill means it holds the raw branch target instead.
                let executing = if cpu.cpu.pc_modified {
                    cpu.cpu.registers.gpr[15]
                } else {
                    cpu.cpu.registers.gpr[15].wrapping_sub(8)
                };
                assert_eq!(
                    executing, addr,
                    "seed {seed}: trace entry {i} ({word:#010x}) claims {addr:#010x}, \
                     but the interpreter is at {executing:#010x}"
                );
                cpu.step(&mut mmu);
                assert!(!cpu.cpu.halted, "seed {seed}: body instruction {i} halted the core");
            }
        }
    }

    /// The scanner must make progress: a scan either accepts instructions or
    /// names one for the interpreter, never both-nothing, or the run loop
    /// stalls.
    #[test]
    fn scanning_always_advances() {
        // NOP NOP SWI, then a NOP and the sentinel.
        let program = [NOP, NOP, 0xEF00_0000u32, NOP];
        // Past the end reads as SWI, so the walk terminates instead of indexing
        // out of range — the scanner may look one instruction past whatever it
        // ends up accepting.
        let fetch =
            |a: u32| program.get((a - CODE_BASE) as usize / 4).copied().unwrap_or(0xEF00_0000);

        let mut addr = CODE_BASE;
        for _ in 0..3 {
            let block = scan(addr, fetch);
            let next = match block.exit {
                // The interpreter runs the named instruction, so the walk
                // resumes one past it.
                BlockExit::Interpret { .. } => block.exit_address().wrapping_add(4),
                _ => block.fall_through(),
            };
            assert!(next > addr, "a scan at {addr:#010x} consumed nothing");
            addr = next;
        }
    }

    /// **Trace formation.** An unconditional direct branch is followed rather
    /// than ending the block, so a body can span disjoint addresses — which is
    /// the whole reason each instruction carries its own.
    #[test]
    fn an_unconditional_branch_is_followed_into_a_trace() {
        // 0x00 NOP
        // 0x04 B  +2   -> 0x14, skipping two instructions
        // 0x08 (skipped)  0x0C (skipped)  0x10 (skipped)
        // 0x14 NOP
        // 0x18 SWI      (ends the trace)
        let mut program = [0xEF00_0000u32; 8];
        program[0] = NOP;
        program[1] = 0xEA00_0002; // B: 0x04 + 8 + (2 << 2) = 0x14
        program[5] = NOP;
        let block = scan(CODE_BASE, |a| program[(a - CODE_BASE) as usize / 4]);

        let addrs: Vec<u32> = block.body().iter().map(|&(a, _)| a).collect();
        assert_eq!(
            addrs,
            vec![CODE_BASE, CODE_BASE + 0x04, CODE_BASE + 0x14],
            "the trace jumped over 0x08..0x14"
        );
        assert!(
            matches!(block.exit, BlockExit::Interpret { reason: ExitReason::SoftwareInterrupt, .. }),
            "the trace ended at the SWI past the branch target"
        );
    }

    /// A **conditional** branch has two live successors, so it still ends the
    /// block — following one path would silently discard the other.
    #[test]
    fn a_conditional_branch_still_ends_the_block() {
        let program = [NOP, 0x1A00_0002u32, NOP, NOP, NOP, NOP, NOP, NOP];
        let block = scan(CODE_BASE, |a| program[(a - CODE_BASE) as usize / 4]);
        assert_eq!(words(&block), [NOP, 0x1A00_0002], "stopped on the BNE");
        assert_eq!(block.exit, BlockExit::AfterBranch);
    }

    /// A backward branch into the trace must stop it. Otherwise a loop unrolls
    /// to the length cap, spending code space on iterations the cached block
    /// already covers.
    #[test]
    fn a_backward_branch_into_the_trace_stops_it() {
        // NOP ; B -3 -> back to 0x00
        let program = [NOP, 0xEAFF_FFFDu32];
        let block = scan(CODE_BASE, |a| program[(a - CODE_BASE) as usize / 4]);
        assert_eq!(words(&block), [NOP, 0xEAFF_FFFD], "the loop was not unrolled");
        assert_eq!(block.exit, BlockExit::AfterBranch);
    }
}
