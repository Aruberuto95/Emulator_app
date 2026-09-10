//! The boundary between Rust and generated machine code, in both directions.
//!
//! # Why this module exists, and why it is the only one of its kind
//!
//! The rest of `emulator_core` contains **zero `unsafe`** — `grep -rn "unsafe"
//! core/src` returns nothing outside this file. A recompiler cannot preserve
//! that, and it needs exactly two things safe Rust cannot express:
//!
//! * **outbound** — turning a `Vec<u8>` of machine code into something the CPU
//!   will branch into: an OS call no safe API covers, plus a transmute from a
//!   data pointer to a function pointer;
//! * **inbound** — the [`thunks`] a compiled block calls to reach guest memory,
//!   which receive the MMU as a raw pointer because a generated `call` cannot
//!   carry a Rust reference.
//!
//! Both live here and nowhere else, so the audit surface for "can this emulator
//! corrupt the host process?" is one file.
//!
//! # W^X, and why it is not optional
//!
//! Pages are **never simultaneously writable and executable**. A buffer is
//! allocated `PAGE_READWRITE`, filled, and only then flipped to
//! `PAGE_EXECUTE_READ`. This is enforced by the type system rather than by
//! discipline: [`CodeBuffer`] can be written but not called, [`ExecBuffer`] can
//! be called but not written, and the only way to obtain the second is
//! [`CodeBuffer::finalize`], which *consumes* the first.
//!
//! Beyond being good hygiene, `PAGE_EXECUTE_READWRITE` is refused outright on
//! processes running under Arbitrary Code Guard, so the W^X form is also the
//! only one that is portable across host mitigation policy.
//!
//! # Threading
//!
//! Both types hold a raw pointer, so neither is `Send` or `Sync`. That matches
//! the emulator: each recompiler instance belongs to one CPU core and is driven
//! from the thread that owns it.

use std::fmt;

/// Host page size assumed when rounding an allocation up.
///
/// x86-64 Windows has a 4 KiB page and no supported configuration changes it.
/// Rounding here (rather than letting `VirtualAlloc` round silently) keeps
/// [`CodeBuffer::capacity`] equal to the number of bytes that were actually
/// committed, so the same length can be handed to `VirtualProtect`.
const PAGE_SIZE: usize = 4096;

/// Why an executable allocation could not be produced.
///
/// Every variant is a host or caller condition the recompiler must survive by
/// falling back to the interpreter — none of them is a reason to abort, which
/// matters because the release profile is `panic = "abort"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMemError {
    /// `VirtualAlloc` refused the reservation (out of address space or memory).
    Reserve { bytes: usize },
    /// `VirtualProtect` refused the RW -> RX transition. Expected on hosts whose
    /// exploit-mitigation policy forbids new executable pages.
    Protect { bytes: usize },
    /// The emitter tried to append past the end of the buffer. Recoverable: the
    /// caller retries the block with a larger capacity, or gives up on it.
    Overflow { capacity: usize, needed: usize },
    /// The offset handed to [`ExecBuffer::entry`] is not inside the written code.
    BadEntry { offset: usize, len: usize },
    /// This platform has no implementation (not Windows, or not x86-64).
    Unsupported,
}

impl fmt::Display for ExecMemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reserve { bytes } => write!(f, "could not reserve {bytes} bytes of code memory"),
            Self::Protect { bytes } => {
                write!(f, "could not make {bytes} bytes executable (mitigation policy?)")
            }
            Self::Overflow { capacity, needed } => {
                write!(f, "code buffer overflow: capacity {capacity}, needed {needed}")
            }
            Self::BadEntry { offset, len } => {
                write!(f, "entry offset {offset} is outside the {len} bytes emitted")
            }
            Self::Unsupported => f.write_str("no executable-memory backend for this platform"),
        }
    }
}

impl std::error::Error for ExecMemError {}

/// The calling convention every compiled block presents.
///
/// Deliberately the platform C ABI (`win64` on this host) rather than `sysv64`:
/// generated code calls back into Rust `extern "C"` thunks for every memory
/// access, and a single convention on both sides of that boundary removes an
/// entire class of argument-register and shadow-space mistakes. Win64's
/// callee-saved set (`rbx`, `rbp`, `rsi`, `rdi`, `r12`-`r15`) is also exactly
/// what a block needs to keep guest registers pinned across a thunk call.
///
/// The argument is a [`JitContext`], through which the block reaches the guest
/// register file and the MMU; the return value is the cycle count the block
/// consumed, which the NDS run loop schedules every peripheral off.
pub type BlockFn = extern "C" fn(*mut crate::jit::compile::JitContext) -> u32;

/// A writable, **non-executable** buffer of machine code under construction.
///
/// Owns one `VirtualAlloc` reservation and frees it on drop.
pub struct CodeBuffer {
    region: Region,
    len: usize,
}

/// Sizes only. The base address is deliberately omitted: it is ASLR noise that
/// differs every run, and printing it would make test output non-reproducible.
impl fmt::Debug for CodeBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodeBuffer")
            .field("len", &self.len)
            .field("capacity", &self.capacity())
            .finish()
    }
}

impl CodeBuffer {
    /// Reserve and commit at least `bytes` of read-write memory, rounded up to
    /// a whole page.
    pub fn with_capacity(bytes: usize) -> Result<Self, ExecMemError> {
        let capacity = bytes
            .checked_add(PAGE_SIZE - 1)
            .map(|n| n / PAGE_SIZE * PAGE_SIZE)
            .filter(|n| *n > 0)
            .ok_or(ExecMemError::Reserve { bytes })?;
        let region = Region::reserve(capacity)?;
        Ok(Self { region, len: 0 })
    }

    /// Bytes committed. Fixed for the life of the buffer: a compiled block must
    /// not move, because emitted branches are relative to where they landed.
    pub fn capacity(&self) -> usize {
        self.region.len
    }

    /// Bytes written so far, and the offset the next byte will land at.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Append `bytes`, or fail without writing anything.
    ///
    /// The all-or-nothing rule matters: a partially emitted instruction is
    /// indistinguishable from a complete one once the buffer is executable.
    pub fn push(&mut self, bytes: &[u8]) -> Result<usize, ExecMemError> {
        let end = self.len.checked_add(bytes.len()).ok_or(ExecMemError::Overflow {
            capacity: self.capacity(),
            needed: usize::MAX,
        })?;
        if end > self.capacity() {
            return Err(ExecMemError::Overflow { capacity: self.capacity(), needed: end });
        }
        let at = self.len;
        self.region.as_mut_slice()[at..end].copy_from_slice(bytes);
        self.len = end;
        Ok(at)
    }

    /// The code written so far, for tests and disassembly.
    pub fn as_slice(&self) -> &[u8] {
        &self.region.as_slice()[..self.len]
    }

    /// Flip the pages to `PAGE_EXECUTE_READ` and hand back a callable buffer.
    ///
    /// Consuming `self` on success is what makes W^X a type-level guarantee: no
    /// writable handle to these pages survives the transition. On failure the
    /// pages stay `PAGE_READWRITE` and never became executable, so handing the
    /// buffer back does not weaken that.
    ///
    /// Hands `self` back on failure for the same reason as
    /// [`ExecBuffer::reopen`]: after a `reopen` this buffer can already hold
    /// published blocks whose [`BlockFn`] pointers are live, so dropping it on
    /// an error would free memory that is still reachable.
    pub fn finalize(self) -> Result<ExecBuffer, (Self, ExecMemError)> {
        if let Err(e) = self.region.make_executable() {
            return Err((self, e));
        }
        Ok(ExecBuffer { region: self.region, len: self.len })
    }
}

/// A finished, **read-execute** buffer of machine code. Cannot be written to.
pub struct ExecBuffer {
    region: Region,
    len: usize,
}

impl ExecBuffer {
    /// Bytes of code in this buffer.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Bytes still unused in this buffer's reservation.
    pub fn spare(&self) -> usize {
        self.region.len - self.len
    }

    /// Re-open the region for writing so another block can be appended.
    ///
    /// # Why this exists
    ///
    /// [`CodeBuffer::with_capacity`] rounds up to a whole page, so publishing
    /// one block per buffer gave **one 4 KiB page per block** — 152 blocks,
    /// 152 pages, a couple hundred bytes used in each. A typical L1 iTLB holds
    /// 64-128 entries, so every one of 47,000 block entries per frame took an
    /// iTLB miss, and no two blocks ever shared a cache line. The synthetic
    /// cost model never saw this because it compiles one to four blocks.
    ///
    /// # W^X
    ///
    /// The invariant is "never simultaneously writable and executable", not
    /// "write once". Consuming `self` means no executable handle survives while
    /// the region is writable, exactly as [`CodeBuffer::finalize`] guarantees
    /// the reverse. The emulator is single-threaded, so no block in this region
    /// can be running while the window is open.
    /// # Why the error case hands `self` back
    ///
    /// Consuming `self` and returning a bare `Err` **frees the region**, and
    /// live [`BlockFn`] pointers from every block already published into this
    /// arena point into it — the next block entry would be a use-after-free.
    /// `VirtualProtect` failing is rare but reachable: Windows
    /// exploit-mitigation policies can refuse a protection change. Returning
    /// the buffer lets the caller keep it alive and simply decline to compile.
    pub fn reopen(self) -> Result<CodeBuffer, (Self, ExecMemError)> {
        if let Err(e) = self.region.make_writable() {
            return Err((self, e));
        }
        Ok(CodeBuffer { region: self.region, len: self.len })
    }

    /// The block that starts at `offset`, as a callable function pointer.
    ///
    /// Returning a *safe* `extern "C" fn` is the deliberate design: callers
    /// invoke compiled blocks without writing `unsafe` themselves, which is what
    /// keeps every `unsafe` in the crate inside this file. The invariant that
    /// makes it sound is stated on [`Region::entry`] and is discharged by the
    /// emitter, not by the caller.
    pub fn entry(&self, offset: usize) -> Result<BlockFn, ExecMemError> {
        if offset >= self.len {
            return Err(ExecMemError::BadEntry { offset, len: self.len });
        }
        Ok(self.region.entry(offset))
    }
}

// ---------------------------------------------------------------------------
// The owned reservation. Every `unsafe` in the crate is below this line.
// ---------------------------------------------------------------------------

/// One page-aligned reservation, freed on drop.
struct Region {
    ptr: *mut u8,
    len: usize,
}

impl Region {
    fn reserve(len: usize) -> Result<Self, ExecMemError> {
        let ptr = sys::reserve_rw(len).ok_or(ExecMemError::Reserve { bytes: len })?;
        Ok(Self { ptr, len })
    }

    fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` came from a successful `reserve_rw(len)` and is owned by
        // `self`, so it points to `len` committed, readable, initialised (the OS
        // zero-fills fresh pages) bytes for as long as `self` lives. No other
        // handle to the region exists, so the shared borrow cannot alias a
        // concurrent write.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as `as_slice`, and `&mut self` proves this is the only live
        // borrow. Only ever called on a region that is still `PAGE_READWRITE` —
        // `make_executable` is reachable only through `CodeBuffer::finalize`,
        // which consumes the buffer, so no `&mut` can outlive the RW phase.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    fn make_executable(&self) -> Result<(), ExecMemError> {
        if sys::make_rx(self.ptr, self.len) {
            Ok(())
        } else {
            Err(ExecMemError::Protect { bytes: self.len })
        }
    }

    fn make_writable(&self) -> Result<(), ExecMemError> {
        if sys::make_rw(self.ptr, self.len) {
            Ok(())
        } else {
            Err(ExecMemError::Protect { bytes: self.len })
        }
    }

    /// Reinterpret the code at `offset` as a function pointer.
    ///
    /// This is the transmute the whole module exists to contain.
    fn entry(&self, offset: usize) -> BlockFn {
        let addr = self.ptr.wrapping_add(offset);
        // SAFETY: three conditions, all established by the caller chain rather
        // than assumed here.
        // 1. `addr` is in-bounds: `ExecBuffer::entry` rejects `offset >= len`
        //    and `len <= self.len`.
        // 2. The page is executable: an `ExecBuffer` can only be built by
        //    `CodeBuffer::finalize`, which returns `Err` unless `VirtualProtect`
        //    to `PAGE_EXECUTE_READ` succeeded.
        // 3. The bytes at `addr` are a complete function that obeys `BlockFn`'s
        //    ABI. This is the emitter's obligation: only this crate's code
        //    generator writes into a `CodeBuffer`, it emits whole instructions
        //    (`CodeBuffer::push` is all-or-nothing), and it terminates every
        //    block with a `ret` that leaves the callee-saved registers as it
        //    found them.
        unsafe { std::mem::transmute::<*mut u8, BlockFn>(addr) }
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        sys::release(self.ptr);
    }
}

// ---------------------------------------------------------------------------
// Inbound: the calls a compiled block makes back into Rust
// ---------------------------------------------------------------------------

/// Guest-memory accessors a compiled block calls.
///
/// # Why these wrap [`Arm9Bus`] rather than [`NdsMmu`]
///
/// [`Arm9Bus`] is where the ARM9's *data-access* semantics live, and they are
/// not the raw MMU's: an unaligned `LDR` fetches the aligned word and rotates
/// it, a read of `0x04100000` pops the IPC receive FIFO, a read of `0x04100010`
/// advances the gamecard cursor, a write to `0x04000188` pushes the send FIFO,
/// and a store ignores the low two address bits. Going through the same bus the
/// interpreter uses means there is **one** definition of all of that instead of
/// a second one that has to be kept in step.
///
/// # ABI
///
/// `extern "C"` — the platform C ABI, which on this host is Win64, the same
/// convention the emitted prologue is built around.
///
/// # Panics
///
/// The release profile is `panic = "abort"`, so a panic inside one of these
/// terminates the process rather than unwinding through a generated frame that
/// has no unwind tables. That is the intended behaviour: unwinding through
/// generated code would be undefined, and aborting is the safe failure.
///
/// [`Arm9Bus`]: crate::nds::cpu::Arm9Bus
/// [`NdsMmu`]: crate::nds::mmu::NdsMmu
pub mod thunks {
    use crate::jit::compile::JitContext;
    use crate::nds::mmu::NdsMmu;

    /// Which core's bus a compiled block reaches guest memory through.
    ///
    /// Selected at **translation** time: the emitter bakes the chosen module's
    /// function addresses into the code as immediates, so a block can never
    /// call the other core's accessors, and no run-time choice exists to cost
    /// anything.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum BusKind {
        Arm9,
        Arm7,
    }

    /// The address a generated `call` should target for `access` on `bus`.
    ///
    /// Taking the address lives here rather than at the call site because the
    /// function-pointer-to-integer round trip is an ABI detail of this
    /// boundary, and this is the module that owns the boundary.
    pub fn address(access: Access, bus: BusKind) -> usize {
        match bus {
            BusKind::Arm9 => arm9::address(access),
            BusKind::Arm7 => arm7::address(access),
        }
    }

    /// Record, after a store ran, whether the chain it is part of must break at
    /// the next block exit — ARM9 rules.
    ///
    /// These are exactly the conditions the run loop or the recompiler's entry
    /// path would observe **between** blocks — see `Arm9Cpu::run` and
    /// `Jit::may_run` — and a store is the only instruction inside a block
    /// that can raise any of them, because everything else that could (MSR,
    /// CP15, SWI) ends its block into the interpreter:
    ///
    /// * an IPCSYNC write set [`NdsMmu::ipc_yield`], and the run loop must hand
    ///   the bus to the ARM7 now;
    /// * the store landed on a page holding compiled code
    ///   ([`NdsMmu::code_write_epoch`] moved, directly or via a DMA the store
    ///   triggered), so every successor link is suspect;
    /// * the store unmasked a pending interrupt (IME/IE/IF), which the
    ///   interpreter would service before the next instruction — conservative
    ///   about the CPSR I bit, which costs a broken chain, never correctness;
    /// * the store armed the touch read-watch, under which blocks decline.
    ///
    /// Word-read thunks also call this after reaching the Gamecard data port:
    /// consuming its last word can request a transfer-complete IRQ.
    fn note_store_effects_arm9(ctx: &mut JitContext, mmu: &NdsMmu) {
        let unmasked_irq =
            (mmu.arm9_ime & 1) != 0 && (mmu.arm9_ie & mmu.arm9_if) != 0;
        if mmu.ipc_yield
            || mmu.code_write_epoch != ctx.code_epoch
            || unmasked_irq
            || mmu.tp_read_watch_on
        {
            ctx.stop = 1;
        }
    }

    /// ARM7 twin of [`note_store_effects_arm9`]: the same chain-break
    /// conditions against the ARM7's IME/IE/IF. The touch read-watch is
    /// absent deliberately — it records *ARM9* data reads (`Arm9Bus` is where
    /// `tp_note_read` hangs), so an ARM7 chain has nothing to observe.
    fn note_store_effects_arm7(ctx: &mut JitContext, mmu: &NdsMmu) {
        let unmasked_irq =
            (mmu.arm7_ime & 1) != 0 && (mmu.arm7_ie & mmu.arm7_if) != 0;
        // The ARM7 epoch, deliberately: the split is what keeps this core's
        // chains standing while the other core's data stores churn.
        if mmu.ipc_yield || mmu.code_write_epoch7 != ctx.code_epoch || unmasked_irq {
            ctx.stop = 1;
        }
    }

    /// Which guest-memory accessor a block wants.
    ///
    /// An enum rather than a pair of booleans so a caller cannot silently swap
    /// "load" and "byte" and get a plausible wrong thunk.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Access {
        ReadWord,
        ReadByte,
        ReadHalfword,
        WriteWord,
        WriteByte,
        WriteHalfword,
        ReadWords,
        WriteWords,
        MsrCpsr,
        MrsSpsr,
    }

    /// `MRS Rd, SPSR`: the banked field only the CPU owns, read through the
    /// same `ctx.cpu` contract as `msr_cpsr`. Core-agnostic — both wrappers
    /// expose the shared `GbaCpu` — so one function serves both tables.
    pub extern "C" fn mrs_spsr(ctx: *mut JitContext) -> u32 {
        // SAFETY: the `bus_thunks!` contract (`ctx`, then `ctx.cpu`).
        let ctx = unsafe { &mut *ctx };
        // SAFETY: `ctx.cpu` is the live `*mut GbaCpu` `try_step` armed.
        let gba = unsafe { &*ctx.cpu.cast::<crate::gba::cpu::GbaCpu>() };
        gba.registers.spsr
    }

    /// Longest register list an ARM block transfer can name.
    ///
    /// Every buffer these thunks touch is a `[u32; 16]` inside the context, so
    /// clamping to this bound makes an out-of-range count a slow no-op rather
    /// than an out-of-bounds host write — the failure mode the task brief calls
    /// out for generated memory accesses.
    pub const MAX_BLOCK_REGS: usize = 16;

    /// The clamp above is only a bound if it matches the buffer it slices.
    /// Sixteen is also the longest register list ARM can name, so neither side
    /// should move — but a build error is the right way to find out.
    const _: () = assert!(MAX_BLOCK_REGS == crate::jit::compile::JitContext::WORDS);

    /// One bus-specific thunk set: the nine `extern "C"` entry points a block
    /// compiled for that core calls, over the core's own `CpuBus` adapter and
    /// store-effect rules.
    ///
    /// A macro rather than generics because `extern "C"` items need concrete
    /// symbols whose addresses the emitter bakes as immediates, and the two
    /// cores differ in exactly two points — the bus newtype and the
    /// store-effects function — which is what the parameters name.
    ///
    /// # Safety (the contract every `unsafe` below relies on)
    ///
    /// * `mmu` (or `ctx.mmu`) must be the pointer `Jit::try_step` wrote into
    ///   the block's `JitContext`, taken from a live `&mut NdsMmu` that
    ///   outlives the call. Generated code passes it back unchanged and never
    ///   synthesises a pointer, so the reference is valid and unaliased for
    ///   the duration of the thunk — the caller is blocked in the `call` and
    ///   holds no other borrow.
    /// * `ctx` must be the `JitContext` pointer the generated code holds in
    ///   its pinned `CTX` register — the same `&mut Jit::ctx` that `try_step`
    ///   passed to the block, live and unaliased for the whole block call.
    ///   Stores and word reads receive it instead of the MMU directly because
    ///   they must both reach guest memory through `ctx.mmu` and publish
    ///   instruction-boundary conditions into `ctx.stop`.
    /// * `ctx.cpu` must be the `*mut GbaCpu` that `try_step` armed from the
    ///   live `&mut` core wrapper — the shared ARM core both wrappers expose,
    ///   which is what makes the bank swap core-agnostic.
    macro_rules! bus_thunks {
        ($name:ident, $Bus:path, $note:path, $core:literal) => {
            #[doc = concat!("The ", $core, " thunk set; see `bus_thunks!`.")]
            pub mod $name {
                use super::{Access, JitContext, NdsMmu, MAX_BLOCK_REGS};
                use crate::cpu_bus::CpuBus;

                /// The address a generated `call` should target for `access`.
                ///
                /// Taking the address lives here rather than at the call site
                /// because the function-pointer-to-integer round trip is an ABI
                /// detail of this boundary, and this module owns the boundary.
                pub fn address(access: Access) -> usize {
                    let f: *const () = match access {
                        Access::ReadWord => read_word as *const (),
                        Access::ReadByte => read_byte as *const (),
                        Access::ReadHalfword => read_halfword as *const (),
                        Access::WriteWord => write_word as *const (),
                        Access::WriteByte => write_byte as *const (),
                        Access::WriteHalfword => write_halfword as *const (),
                        Access::ReadWords => read_words as *const (),
                        Access::WriteWords => write_words as *const (),
                        Access::MsrCpsr => msr_cpsr as *const (),
                        Access::MrsSpsr => super::mrs_spsr as *const (),
                    };
                    f as usize
                }

                /// `MSR CPSR_fields, op`, exactly as `arm_psr_transfer`'s CPSR
                /// path: merge under the field mask, swap register banks if the
                /// mode changed (state only the CPU owns — the reason this is
                /// a thunk), never toggle T, and return the committed CPSR for
                /// the block's pinned register. `regs.cpsr` itself is NOT
                /// written: the block owns it until its epilogue publishes.
                ///
                /// Also raises `JitContext::stop` via the store-effects rule:
                /// an MSR can unmask a pending interrupt, and the interpreter
                /// would service it before the next instruction — the emitting
                /// block terminates on MSR so the flag is honoured at exactly
                /// that boundary.
                pub extern "C" fn msr_cpsr(
                    ctx: *mut JitContext,
                    operand: u32,
                    mask: u32,
                    cpsr: u32,
                ) -> u32 {
                    // SAFETY: the `bus_thunks!` contract (`ctx`).
                    let ctx = unsafe { &mut *ctx };
                    // SAFETY: the `bus_thunks!` contract (`ctx.cpu`): a live
                    // `*mut GbaCpu`, the shared core both wrappers expose.
                    let gba =
                        unsafe { &mut *ctx.cpu.cast::<crate::gba::cpu::GbaCpu>() };
                    let new = (cpsr & !mask) | (operand & mask);
                    if mask & 0xFF != 0 {
                        let old_mode = crate::gba::cpu::CpuRegisters::mode_of(cpsr);
                        let new_mode = crate::gba::cpu::CpuRegisters::mode_of(new);
                        if old_mode != new_mode {
                            gba.registers.swap_mode(old_mode, new_mode);
                        }
                    }
                    let committed = (new & !0x20) | (cpsr & 0x20); // MSR never toggles T
                    let mmu = ctx.mmu.cast::<NdsMmu>();
                    // SAFETY: the `bus_thunks!` contract (`ctx.mmu`); no bus
                    // borrow is live.
                    $note(ctx, unsafe { &*mmu });
                    committed
                }

                /// `LDR` word, including the rotate an unaligned address applies.
                pub extern "C" fn read_word(ctx: *mut JitContext, addr: u32) -> u32 {
                    // SAFETY: the `bus_thunks!` contract (`ctx`, `ctx.mmu`).
                    let ctx = unsafe { &mut *ctx };
                    let mmu = ctx.mmu.cast::<NdsMmu>();
                    let value = $Bus(unsafe { &mut *mmu }).read_word(addr);
                    if addr == 0x0410_0010 {
                        // The final Gamecard word can raise an IRQ. Ordinary
                        // RAM reads avoid the extra interrupt/epoch checks.
                        $note(ctx, unsafe { &*mmu });
                    }
                    value
                }

                /// `LDRB`. Returns `u32` because the guest zero-extends into a
                /// register. `LDRSB` reuses it and sign-extends host-side.
                pub extern "C" fn read_byte(mmu: *mut NdsMmu, addr: u32) -> u32 {
                    // SAFETY: the `bus_thunks!` contract (`mmu`).
                    u32::from($Bus(unsafe { &mut *mmu }).read_byte(addr))
                }

                /// `LDRH`/`LDRSH`. Zero-extended here; `LDRSH` sign-extends
                /// host-side.
                pub extern "C" fn read_halfword(mmu: *mut NdsMmu, addr: u32) -> u32 {
                    // SAFETY: the `bus_thunks!` contract (`mmu`).
                    u32::from($Bus(unsafe { &mut *mmu }).read_halfword(addr))
                }

                /// `STRH`. Takes the context for the same reason as
                /// [`write_word`]; the bus truncates to 16 bits exactly as the
                /// interpreter does.
                pub extern "C" fn write_halfword(ctx: *mut JitContext, addr: u32, val: u32) {
                    // SAFETY: the `bus_thunks!` contract (`ctx`).
                    let ctx = unsafe { &mut *ctx };
                    let mmu = ctx.mmu.cast::<NdsMmu>();
                    // SAFETY: the `bus_thunks!` contract (`ctx.mmu`).
                    $Bus(unsafe { &mut *mmu }).write_halfword(addr, (val & 0xFFFF) as u16);
                    // SAFETY: the bus borrow above ended; same contract.
                    $note(ctx, unsafe { &*mmu });
                }

                /// `STR` word. The bus ignores the low two address bits.
                ///
                /// Store thunks take the **context**, not the MMU: the memory
                /// pointer comes from `ctx.mmu`, and the chain-break conditions
                /// the store may have raised go back through `ctx.stop`.
                pub extern "C" fn write_word(ctx: *mut JitContext, addr: u32, val: u32) {
                    // SAFETY: the `bus_thunks!` contract (`ctx`).
                    let ctx = unsafe { &mut *ctx };
                    let mmu = ctx.mmu.cast::<NdsMmu>();
                    // SAFETY: the `bus_thunks!` contract (`ctx.mmu`).
                    $Bus(unsafe { &mut *mmu }).write_word(addr, val);
                    // SAFETY: the bus borrow above ended; same contract.
                    $note(ctx, unsafe { &*mmu });
                }

                /// `STRB`. Takes `u32` so the ABI has one integer width; the
                /// bus truncates.
                pub extern "C" fn write_byte(ctx: *mut JitContext, addr: u32, val: u32) {
                    // SAFETY: the `bus_thunks!` contract (`ctx`).
                    let ctx = unsafe { &mut *ctx };
                    let mmu = ctx.mmu.cast::<NdsMmu>();
                    // SAFETY: the `bus_thunks!` contract (`ctx.mmu`).
                    $Bus(unsafe { &mut *mmu }).write_byte(addr, (val & 0xFF) as u8);
                    // SAFETY: the bus borrow above ended; same contract.
                    $note(ctx, unsafe { &*mmu });
                }

                /// `LDM`: read `count` consecutive words into `out`.
                ///
                /// Goes through `CpuBus::read_words`, which is where the NDS
                /// buses decode the region once for the whole block and decline
                /// — falling back to a per-word loop — for I/O, VRAM, shared
                /// WRAM or a straddled mirror, so no side effect is bypassed.
                pub extern "C" fn read_words(ctx: *mut JitContext, addr: u32, out: *mut u32, count: u32) {
                    // Do not retain a context borrow while `out` aliases its
                    // staging buffer; reborrow only after the transfer.
                    let mmu = unsafe { &mut *ctx }.mmu.cast::<NdsMmu>();
                    let n = (count as usize).min(MAX_BLOCK_REGS);
                    // SAFETY: `out` is `&mut JitContext::words`, a `[u32; 16]`
                    // the caller owns for the duration of the call; `n` is
                    // clamped to that length, so the slice is in bounds however
                    // the generated code computed `count`. `mmu` per the
                    // `bus_thunks!` contract.
                    let out = unsafe { std::slice::from_raw_parts_mut(out, n) };
                    // SAFETY: the `bus_thunks!` contract (`mmu`).
                    $Bus(unsafe { &mut *mmu }).read_words(addr, out);
                    let card_offset = 0x0410_0010u32.wrapping_sub(addr);
                    if card_offset & 3 == 0 && card_offset / 4 < n as u32 {
                        // LDM completes all its transfers before polling IRQ,
                        // matching the interpreter's instruction boundary.
                        $note(unsafe { &mut *ctx }, unsafe { &*mmu });
                    }
                }

                /// `STM`: write `count` consecutive words from `vals`. Takes
                /// the context for the same reason as [`write_word`].
                ///
                /// `vals` points into `ctx.words`, so no `&mut JitContext` may
                /// be live while the slice is — the context is re-borrowed only
                /// after the transfer.
                pub extern "C" fn write_words(ctx: *mut JitContext, addr: u32, vals: *const u32, count: u32) {
                    // SAFETY: the `bus_thunks!` contract (`ctx`).
                    let mmu = unsafe { &mut *ctx }.mmu.cast::<NdsMmu>();
                    let n = (count as usize).min(MAX_BLOCK_REGS);
                    // SAFETY: as `read_words`, and read-only here.
                    let vals = unsafe { std::slice::from_raw_parts(vals, n) };
                    // SAFETY: the `bus_thunks!` contract (`ctx.mmu`).
                    $Bus(unsafe { &mut *mmu }).write_words(addr, vals);
                    // SAFETY: the bus borrow above ended; both pointers per the
                    // `bus_thunks!` contract.
                    $note(unsafe { &mut *ctx }, unsafe { &*mmu });
                }
            }
        };
    }

    bus_thunks!(arm9, crate::nds::cpu::Arm9Bus, super::note_store_effects_arm9, "ARM9");
    bus_thunks!(arm7, crate::nds::cpu::Arm7Bus, super::note_store_effects_arm7, "ARM7");
}

// ---------------------------------------------------------------------------
// Platform layer
// ---------------------------------------------------------------------------

#[cfg(all(windows, target_arch = "x86_64"))]
mod sys {
    use std::ffi::c_void;

    const MEM_COMMIT: u32 = 0x0000_1000;
    const MEM_RESERVE: u32 = 0x0000_2000;
    const MEM_RELEASE: u32 = 0x0000_8000;
    const PAGE_READWRITE: u32 = 0x04;
    const PAGE_EXECUTE_READ: u32 = 0x20;

    // Declared here rather than pulled from `windows-sys` on purpose: four
    // functions do not justify a dependency (and the supply-chain surface that
    // comes with one) in a crate whose only other dependency is `cxx`.
    #[link(name = "kernel32")]
    extern "system" {
        fn VirtualAlloc(
            lpAddress: *mut c_void,
            dwSize: usize,
            flAllocationType: u32,
            flProtect: u32,
        ) -> *mut c_void;
        fn VirtualProtect(
            lpAddress: *mut c_void,
            dwSize: usize,
            flNewProtect: u32,
            lpflOldProtect: *mut u32,
        ) -> i32;
        fn VirtualFree(lpAddress: *mut c_void, dwSize: usize, dwFreeType: u32) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn FlushInstructionCache(
            hProcess: *mut c_void,
            lpBaseAddress: *const c_void,
            dwSize: usize,
        ) -> i32;
    }

    /// Commit `len` bytes of zeroed read-write memory, or `None`.
    pub(super) fn reserve_rw(len: usize) -> Option<*mut u8> {
        // SAFETY: `VirtualAlloc` with a null base address is the documented
        // "choose an address for me" form and has no preconditions beyond a
        // non-zero size, which `CodeBuffer::with_capacity` guarantees. It
        // returns null on failure, which is checked below rather than used.
        let p = unsafe {
            VirtualAlloc(std::ptr::null_mut(), len, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE)
        };
        if p.is_null() {
            None
        } else {
            Some(p.cast::<u8>())
        }
    }

    /// Flip `len` bytes at `ptr` from read-execute back to read-write.
    ///
    /// The other half of W^X: pages hold many blocks, and appending one means
    /// re-opening the region. The invariant is unchanged — the region is never
    /// *simultaneously* writable and executable — and the emulator is
    /// single-threaded, so no block in this region can be executing while the
    /// window is open.
    pub(super) fn make_rw(ptr: *mut u8, len: usize) -> bool {
        let mut old = 0u32;
        // SAFETY: identical to `make_rx` — `ptr`/`len` describe a live
        // reservation from `reserve_rw` that the caller still owns, and `old`
        // is a valid writable `u32`.
        let ok = unsafe {
            VirtualProtect(ptr.cast::<c_void>(), len, PAGE_READWRITE, &mut old as *mut u32)
        };
        ok != 0
    }

    /// Flip `len` bytes at `ptr` from read-write to read-execute.
    pub(super) fn make_rx(ptr: *mut u8, len: usize) -> bool {
        let mut old = 0u32;
        // SAFETY: `ptr`/`len` describe a live reservation made by `reserve_rw`
        // (the caller owns it and has not freed it), which is what
        // `VirtualProtect` requires. `old` is a valid writable `u32`.
        let ok = unsafe {
            VirtualProtect(ptr.cast::<c_void>(), len, PAGE_EXECUTE_READ, &mut old as *mut u32)
        };
        if ok == 0 {
            return false;
        }
        // SAFETY: same live reservation. x86-64 has a coherent instruction
        // cache, so this is a formality the Win32 contract asks for after
        // publishing new code rather than a correctness requirement here; it is
        // called anyway so the module stays correct if it is ever ported to a
        // host where it is not.
        unsafe {
            FlushInstructionCache(GetCurrentProcess(), ptr.cast::<c_void>(), len);
        }
        true
    }

    /// Release the whole reservation at `ptr`.
    pub(super) fn release(ptr: *mut u8) {
        if ptr.is_null() {
            return;
        }
        // SAFETY: `ptr` is the exact base address returned by `VirtualAlloc`,
        // called once from `Region::drop` so it cannot be released twice.
        // `MEM_RELEASE` requires a zero size, per the Win32 contract.
        unsafe {
            VirtualFree(ptr.cast::<c_void>(), 0, MEM_RELEASE);
        }
    }
}

#[cfg(not(all(windows, target_arch = "x86_64")))]
mod sys {
    //! Fallback for hosts the recompiler does not target. Allocation always
    //! fails, so [`super::CodeBuffer::with_capacity`] returns
    //! [`super::ExecMemError::Reserve`] and the caller stays on the interpreter.
    //! Nothing here is `unsafe`, and nothing here can be reached with a
    //! non-null pointer.

    pub(super) fn reserve_rw(_len: usize) -> Option<*mut u8> {
        None
    }

    pub(super) fn make_rw(_ptr: *mut u8, _len: usize) -> bool {
        false
    }

    pub(super) fn make_rx(_ptr: *mut u8, _len: usize) -> bool {
        false
    }

    pub(super) fn release(_ptr: *mut u8) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These blocks ignore their argument, so a null context is the honest
    /// thing to pass: they are testing the *allocator*, not the translation.
    const NO_CTX: *mut crate::jit::compile::JitContext = std::ptr::null_mut();

    /// The go/no-go probe for the whole recompiler: can this host allocate a
    /// page, write x86-64 into it, flip it to executable and branch into it?
    ///
    /// `mov eax, 0x2A ; ret` — the smallest program with an observable result.
    /// If this fails under the host's exploit-mitigation policy, no amount of
    /// correct ARM translation matters.
    #[test]
    #[cfg(all(windows, target_arch = "x86_64"))]
    fn host_executes_generated_code() {
        const MOV_EAX_RET: [u8; 6] = [0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3];

        let mut buf = CodeBuffer::with_capacity(64).expect("reserve RW page");
        let at = buf.push(&MOV_EAX_RET).expect("emit");
        assert_eq!(at, 0, "first emission starts at offset 0");
        assert_eq!(buf.as_slice(), &MOV_EAX_RET, "the bytes really landed");

        let exec = buf.finalize().expect("RW -> RX");
        let block = exec.entry(0).expect("entry at 0");
        assert_eq!(block(NO_CTX), 42, "the host ran generated code");
    }

    /// Two blocks in one buffer: the second must start where the first ended,
    /// because block dispatch indexes by offset.
    #[test]
    #[cfg(all(windows, target_arch = "x86_64"))]
    fn offsets_address_distinct_blocks() {
        let mut buf = CodeBuffer::with_capacity(64).unwrap();
        let a = buf.push(&[0xB8, 0x01, 0x00, 0x00, 0x00, 0xC3]).unwrap();
        let b = buf.push(&[0xB8, 0x02, 0x00, 0x00, 0x00, 0xC3]).unwrap();
        assert_eq!((a, b), (0, 6));

        let exec = buf.finalize().unwrap();
        assert_eq!(exec.entry(a).unwrap()(NO_CTX), 1);
        assert_eq!(exec.entry(b).unwrap()(NO_CTX), 2);
    }

    /// Overflow must be reported, not truncated: half an instruction that the
    /// CPU will still branch into is the worst possible failure mode.
    #[test]
    fn push_past_capacity_writes_nothing() {
        let Ok(mut buf) = CodeBuffer::with_capacity(1) else {
            return; // unsupported host; nothing to assert
        };
        let cap = buf.capacity();
        assert_eq!(cap, PAGE_SIZE, "capacity rounds up to a whole page");

        buf.push(&vec![0x90; cap - 1]).expect("fits");
        let err = buf.push(&[0x90, 0x90]).expect_err("must not truncate");
        assert_eq!(err, ExecMemError::Overflow { capacity: cap, needed: cap + 1 });
        assert_eq!(buf.len(), cap - 1, "a failed push leaves the buffer untouched");
    }

    /// An entry offset outside the emitted code is rejected rather than
    /// producing a function pointer into uninitialised bytes.
    #[test]
    fn entry_outside_emitted_code_is_rejected() {
        let Ok(mut buf) = CodeBuffer::with_capacity(64) else {
            return; // unsupported host
        };
        buf.push(&[0xC3]).unwrap();
        let exec = buf.finalize().expect("RW -> RX");
        assert_eq!(exec.len(), 1);
        assert_eq!(exec.entry(1), Err(ExecMemError::BadEntry { offset: 1, len: 1 }));
    }
}
