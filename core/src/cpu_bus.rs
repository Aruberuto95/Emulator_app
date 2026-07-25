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
