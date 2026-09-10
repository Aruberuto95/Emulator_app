//! Block recompiler (JIT) for the NDS ARM9 and ARM7.
//!
//! # Why
//!
//! Translating a trace once amortizes instruction decoding and CPU bookkeeping
//! over subsequent executions. Generated code keeps the register-file pointer
//! and CPSR in host registers and uses the same memory buses as the interpreter.
//! Performance changes require fresh measurements on representative scenes.
//!
//! # Scope
//!
//! Generated code targets Windows x86-64 and the Win64 ABI. Both NDS CPUs
//! enable their recompiler by default; `EMU_ARM9_JIT=0` and `EMU_ARM7_JIT=0`
//! select their interpreters. Other hosts cannot allocate executable code
//! through this backend and fall back to interpretation.
//!
//! The ARM9 enables successor linking and dispatch by default. The ARM7 uses
//! blocks without linking and requires them to fit its remaining cycle slice.
//! Thumb compilation is optional (`EMU_ARM9_JIT_THUMB=1` or
//! `EMU_ARM7_JIT_THUMB=1`); ordinary ARM instructions are the default subset.
//! Unsupported encodings and states always return to the shared interpreter.
//!
//! # Structure
//!
//! * [`block`] — ARM/Thumb scanning and trace boundaries.
//! * [`compile`] and [`x64`] — instruction translation and machine-code emission.
//! * [`runner`] — per-core caches, invalidation, linking and interpreter handoff.
//! * [`exec_mem`] — executable-page allocation and the raw-pointer thunk
//!   boundary. Read its safety contracts before changing generated calls.
//! * `difftest` (test builds only) — the differential harness every codegen
//!   change is checked against: two backends advance from identical state and
//!   compare CPU state, cycles and memory after each reported instruction batch.
//!
//! Unit regressions cover synthetic code, cache transitions and memory effects
//! without a commercial ROM. Ignored boot/audio/performance probes additionally
//! require their documented ROM or savestate and are separate validation;
//! their historical measurements are not proof for a new code change.

// Diagnostic and experimental emitter entry points are also used by probes.
#![allow(dead_code)]

pub mod block;
pub mod compile;
pub mod exec_mem;
pub mod runner;
pub mod x64;

// Test-only: the harness drives the emulator but is never part of a shipped
// build, so gating it here keeps it out of the staticlib the frontend links.
#[cfg(test)]
pub mod difftest;
