//! CPU ↔ memory bus abstraction shared by the ARM7TDMI-derived cores (GBA and
//! the two NDS CPUs). The ARM interpreter in `gba::cpu` is written against this
//! trait instead of a concrete MMU, so the exact same instruction-decode code
//! drives GBA memory and the NDS ARM9/ARM7 memory maps.
//!
//! It is a *generic* bound (`<B: CpuBus>`), never a `dyn` object: every call
//! monomorphizes to a direct MMU call, so there is zero dynamic-dispatch cost in
//! the CPU hot loop. For the GBA this compiles to byte-identical code to the
//! previous direct `&mut GbaMmu` calls.

/// The subset of memory operations the ARM interpreter needs.
///
/// The six data accessors are `&mut self` because a read can mutate state on
/// some cores (e.g. reading the NDS IPC FIFO pops it). The `*_safe` variants are
/// the bounds-checked forms the SWI/BIOS HLE uses; their defaults forward to the
/// plain accessors, which is exactly what the GBA MMU did.
pub trait CpuBus {
    fn read_byte(&mut self, addr: u32) -> u8;
    fn read_halfword(&mut self, addr: u32) -> u16;
    fn read_word(&mut self, addr: u32) -> u32;
    fn write_byte(&mut self, addr: u32, val: u8);
    fn write_halfword(&mut self, addr: u32, val: u16);
    fn write_word(&mut self, addr: u32, val: u32);

    /// Instruction fetch of one ARM word.
    ///
    /// Separate from [`Self::read_word`] because a fetch is a strictly narrower
    /// operation, and the difference is paid once per emulated instruction:
    ///
    /// * the address is **always aligned** (the caller masks it), so the
    ///   unaligned-LDR rotate a data read must apply is dead work;
    /// * it can never target a side-effecting port — no core executes out of the
    ///   IPC receive FIFO or the gamecard data register — so the address
    ///   comparisons that route those are dead work;
    /// * it is not a data read, so a data watchpoint must not see it.
    ///
    /// The default forwards, so a bus with none of those concerns (the GBA MMU)
    /// is unaffected and there is one implementation of the plain read.
    fn fetch_word(&mut self, addr: u32) -> u32 {
        self.read_word(addr)
    }

    /// Instruction fetch of one Thumb halfword; see [`Self::fetch_word`].
    fn fetch_halfword(&mut self, addr: u32) -> u16 {
        self.read_halfword(addr)
    }

    /// Read `out.len()` consecutive words starting at the word-aligned `addr`.
    ///
    /// Exists because LDM is **one instruction that performs up to sixteen bus
    /// reads**, and doing them one `read_word` at a time re-runs the entire
    /// region decode — the TCM windows, the `addr >> 24` dispatch, the range
    /// arithmetic — once per register. `nds_arm9_synthetic_throughput_probe`
    /// prices an eight-register `LDMIA` at 38-43 ns against 14-15 ns for an
    /// `ADD`, i.e. ~3 ns per register where a standalone `LDR`'s marginal cost
    /// is 1.4-2.6 ns, and LDM/STM is 7.5% of retired instructions on the
    /// SoulSilver overworld.
    ///
    /// The default is exactly the loop it replaces, so a bus that cannot do
    /// better is unaffected and there is one definition of the semantics. A bus
    /// that can hand back a contiguous slice overrides it and decodes once.
    ///
    /// Addresses are consecutive words from `addr`; the caller has already
    /// resolved the ARM addressing mode, so this must not re-order or re-align.
    fn read_words(&mut self, addr: u32, out: &mut [u32]) {
        for (i, w) in out.iter_mut().enumerate() {
            *w = self.read_word(addr.wrapping_add(i as u32 * 4));
        }
    }

    /// Store `vals` to consecutive words starting at the word-aligned `addr`.
    ///
    /// Write-side twin of [`Self::read_words`], for STM, and the same argument
    /// applies: one instruction, up to sixteen bus writes, one region decode
    /// per write. The values are computed by the caller *before* the first
    /// store, which is equivalent to interleaving because a store cannot change
    /// a register.
    ///
    /// An overriding bus must decline for anything that is not plain contiguous
    /// memory — I/O ports, VRAM, the GX command window — since those writes have
    /// side effects that a block copy would skip.
    fn write_words(&mut self, addr: u32, vals: &[u32]) {
        for (i, v) in vals.iter().enumerate() {
            self.write_word(addr.wrapping_add(i as u32 * 4), *v);
        }
    }

    fn read_byte_safe(&mut self, addr: u32) -> u8 {
        self.read_byte(addr)
    }
    fn read_halfword_safe(&mut self, addr: u32) -> u16 {
        self.read_halfword(addr)
    }
    fn read_word_safe(&mut self, addr: u32) -> u32 {
        self.read_word(addr)
    }
    fn write_byte_safe(&mut self, addr: u32, val: u8) {
        self.write_byte(addr, val);
    }
    fn write_halfword_safe(&mut self, addr: u32, val: u16) {
        self.write_halfword(addr, val);
    }
    fn write_word_safe(&mut self, addr: u32, val: u32) {
        self.write_word(addr, val);
    }

    /// Is an enabled interrupt pending, with the master enable set?
    ///
    /// Asked once per instruction, before every fetch, so it is on the hottest
    /// path in the interpreter. The default is the literal register read the
    /// interpreter used to inline — correct for any bus, and what a core with no
    /// dedicated fields still gets — but a bus that keeps IME/IE/IF in fields
    /// should override it: through the default, one poll costs eight `read_byte`
    /// region decodes (a word and two halfwords, each fanned out per byte)
    /// purely to look at three values the MMU is already holding.
    ///
    /// Deliberately *not* including the CPSR I bit: that is CPU state, not bus
    /// state, and the caller checks it first so a masked core never asks.
    fn irq_pending(&mut self) -> bool {
        (self.read_word_safe(0x0400_0208) & 1) != 0
            && (self.read_halfword_safe(0x0400_0200) & self.read_halfword_safe(0x0400_0202)) != 0
    }

    // GBA BIOS `RegisterRamReset`/`SoftReset` HLE hooks. These clear whole
    // hardware regions and only make sense for the GBA memory map, so the
    // default is a no-op and only the GBA bus overrides them. The NDS cores use
    // their own SWI HLE and never reach this path.
    // ponytail: no-op defaults keep this a single trait instead of forcing every
    // core to implement a GBA-only sub-trait; split it out only if a future core
    // needs real reset semantics here.
    fn clear_iwram_safe(&mut self) {}
    fn clear_ewram(&mut self) {}
    fn clear_vram(&mut self) {}
    fn clear_palette_ram(&mut self) {}
    fn clear_oam(&mut self) {}
    fn reset_sound_registers(&mut self) {}
    fn reset_sio_registers(&mut self) {}
    fn reset_other_io_registers(&mut self) {}
}
