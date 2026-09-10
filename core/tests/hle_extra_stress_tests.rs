// core/tests/hle_extra_stress_tests.rs

use emulator_core::nds::mmu::NdsMmu;
use emulator_core::nds::cpu::Arm9Cpu;
use emulator_core::nds::cpu::Arm7Cpu;
use emulator_core::nds::hle::boot_load_rom;

/// TCM enable lives in CP15 c1 — bit 18 for ITCM, bit 16 for DTCM — and nowhere
/// else. The TCM *Region* registers hold `base[31:12] | size[5:1]`; their bit 0
/// is not an enable, which is why real games leave it clear (SoulSilver writes
/// `itcm_control = 0x20`, i.e. base 0 with size field 16, and its ITCM code
/// fetches must still resolve).
///
/// These cases used to assert a "both bits must be 1" model. That model would
/// leave every real cartridge running with its TCMs off.
#[test]
fn test_cp15_tcm_enable_combinations() {
    let mut mmu = NdsMmu::new();

    // Neither control bit: both off, whatever the region registers say.
    let mut cp15 = mmu.cp15();
    cp15.control = 0;
    cp15.itcm_control = 0x20;
    cp15.dtcm_control = 0x0B000000 | 0x0A;
    mmu.set_cp15(cp15);
    assert!(!mmu.itcm_enabled(), "ITCM off without c1 bit 18");
    assert!(!mmu.dtcm_enabled(), "DTCM off without c1 bit 16");

    // ITCM only.
    cp15.control = 1 << 18;
    mmu.set_cp15(cp15);
    assert!(mmu.itcm_enabled(), "c1 bit 18 enables ITCM");
    assert!(!mmu.dtcm_enabled(), "bit 18 must not enable DTCM");

    // DTCM only.
    cp15.control = 1 << 16;
    mmu.set_cp15(cp15);
    assert!(!mmu.itcm_enabled(), "bit 16 must not enable ITCM");
    assert!(mmu.dtcm_enabled(), "c1 bit 16 enables DTCM");

    // Both, with region-register bit 0 clear exactly as hardware sees it.
    cp15.control = (1 << 18) | (1 << 16);
    mmu.set_cp15(cp15);
    assert!(mmu.itcm_enabled() && mmu.dtcm_enabled(), "both control bits enable both TCMs");
}

/// Region membership is `addr - base < size` in wrapping arithmetic, so a region
/// based near the top of memory reaches up to 0xFFFFFFFF without overflowing —
/// and stops there rather than wrapping into low memory.
///
/// `size` comes from bits 5:1 of the region register as `512 << N`; these cases
/// spell it out instead of assuming a fixed 32 KB / 16 KB window.
#[test]
fn test_tcm_range_check_overflow_boundaries() {
    let mut mmu = NdsMmu::new();

    // ITCM: base 0xFFFF8000, N=6 -> 512<<6 = 0x8000, i.e. exactly the top 32 KB.
    let mut cp15 = mmu.cp15();
    cp15.itcm_control = 0xFFFF8000 | (6 << 1);
    mmu.set_cp15(cp15);
    assert!(mmu.in_itcm_range_arm9(0xFFFF8000), "base is in range");
    assert!(mmu.in_itcm_range_arm9(0xFFFFFFFF), "last byte of memory is in range");
    assert!(!mmu.in_itcm_range_arm9(0x00000000), "must not wrap past the top into 0");
    assert!(!mmu.in_itcm_range_arm9(0xFFFF7FFF), "one byte below base is out of range");

    // DTCM: base 0, N=5 -> 512<<5 = 0x4000 (the NDS DTCM size).
    cp15.dtcm_control = 0x00000000 | (5 << 1);
    mmu.set_cp15(cp15);
    assert!(mmu.in_dtcm_range_arm9(0x00000000), "base 0 is in range");
    assert!(mmu.in_dtcm_range_arm9(0x00003FFF), "last byte of the window is in range");
    assert!(!mmu.in_dtcm_range_arm9(0x00004000), "first byte past the window is out");
    assert!(!mmu.in_dtcm_range_arm9(0xFFFFFFFF), "underflowing address is out");
}

#[test]
fn test_ipc_fifo_arm9_rx_full_flag() {
    let mut mmu = NdsMmu::new();
    mmu.write_ipc_fifo_cnt_arm9(1 << 15);
    mmu.write_ipc_fifo_cnt_arm7(1 << 15);

    // Fill ARM9's RX FIFO (fifo_7to9)
    for i in 0..16 {
        mmu.write_ipc_fifo_tx_arm7(i);
    }

    let cnt9 = mmu.read_ipc_fifo_cnt_arm9();
    // According to the GBATEK requirement, the ARM9 RX Full flag is bit 9
    assert_eq!(cnt9 & (1 << 9), 1 << 9, "ARM9 RX Full flag (bit 9) must be set when RX FIFO is full");
    // Verify it doesn't leak into bit 1 (or check if it is set there too)
    assert_eq!(cnt9 & (1 << 1), 0, "Bit 1 should not be set for RX Full under this design");
}

#[test]
fn test_ipc_fifo_arm7_status_flags() {
    let mut mmu = NdsMmu::new();
    mmu.write_ipc_fifo_cnt_arm9(1 << 15);
    mmu.write_ipc_fifo_cnt_arm7(1 << 15);

    // Verify ARM7 TX Empty / RX Empty initially
    let cnt7_init = mmu.read_ipc_fifo_cnt_arm7();
    assert_eq!(cnt7_init & (1 << 0), 1 << 0, "ARM7 TX Empty (bit 0) should be 1 initially");
    assert_eq!(cnt7_init & (1 << 8), 1 << 8, "ARM7 RX Empty (bit 8) should be 1 initially");

    // Write a word from ARM9 -> ARM7 (populates fifo_9to7, which is ARM7's RX)
    mmu.write_ipc_fifo_tx_arm9(0x42);
    let cnt7_one = mmu.read_ipc_fifo_cnt_arm7();
    assert_eq!(cnt7_one & (1 << 8), 0, "ARM7 RX Empty (bit 8) should be 0 when RX queue has data");
    assert_eq!(cnt7_one & (1 << 0), 1 << 0, "ARM7 TX Empty (bit 0) should remain 1");

    // Write 15 more words to fill ARM7 RX FIFO
    for i in 0..15 {
        mmu.write_ipc_fifo_tx_arm9(i);
    }
    let cnt7_full = mmu.read_ipc_fifo_cnt_arm7();
    assert_eq!(cnt7_full & (1 << 9), 1 << 9, "ARM7 RX Full (bit 9) should be 1 when RX queue is full");

    // Write to ARM7 TX FIFO (fifo_7to9)
    mmu.write_ipc_fifo_tx_arm7(0x24);
    let cnt7_tx = mmu.read_ipc_fifo_cnt_arm7();
    assert_eq!(cnt7_tx & (1 << 0), 0, "ARM7 TX Empty (bit 0) should be 0 when TX queue has data");
}

#[test]
fn test_ipc_fifo_disable_mask_updates() {
    let mut mmu = NdsMmu::new();
    
    // Enable and set bits 2 and 10 to 1
    mmu.write_ipc_fifo_cnt_arm9(0x8000 | (1 << 2) | (1 << 10));
    assert_eq!(mmu.read_ipc_fifo_cnt_arm9() & 0x8404, 0x8404, "Bits 2, 10, and 15 should be set");

    // Disable (bit 15 = 0) and try to clear bits 2 and 10 by writing 0
    mmu.write_ipc_fifo_cnt_arm9(0);
    let cnt_after = mmu.read_ipc_fifo_cnt_arm9();
    assert_eq!(cnt_after & (1 << 15), 0, "FIFO must be disabled");
    assert_eq!(cnt_after & (1 << 2), 0, "Bit 2 must be cleared on disable");
    assert_eq!(cnt_after & (1 << 10), 0, "Bit 10 must be cleared on disable");

    // Test that disabling while preserving bits 2 and 10 works
    mmu.write_ipc_fifo_cnt_arm9(0x8000 | (1 << 2) | (1 << 10));
    mmu.write_ipc_fifo_cnt_arm9((1 << 2) | (1 << 10)); // bit 15 = 0, but bits 2 and 10 are written as 1
    let cnt_after_preserve = mmu.read_ipc_fifo_cnt_arm9();
    assert_eq!(cnt_after_preserve & (1 << 15), 0, "FIFO must be disabled");
    assert_eq!(cnt_after_preserve & (1 << 2), 1 << 2, "Bit 2 should remain set since it was written as 1");
    assert_eq!(cnt_after_preserve & (1 << 10), 1 << 10, "Bit 10 should remain set since it was written as 1");
}

#[test]
fn test_autoload_robustness_stress() {
    let mut mmu = NdsMmu::new();
    let mut arm9 = Arm9Cpu::new();
    let mut arm7 = Arm7Cpu::new();

    // Create a mock ROM that contains invalid/malformed/overflowing autoload entries
    let mut rom = vec![0u8; 0x2000];

    // ROM Header setup
    rom[0..12].copy_from_slice(b"STRESSTEST\0\0");
    rom[0x0C..0x10].copy_from_slice(b"STRS");

    let arm9_rom_offset = 0x200u32;
    let arm9_entry = 0x02000800u32;
    let arm9_ram = 0x02000000u32;
    let arm9_size = 0x1000u32;

    let arm7_rom_offset = 0x300u32;
    let arm7_entry = 0x03800800u32;
    let arm7_ram = 0x03800000u32;
    let arm7_size = 0x80u32;

    rom[0x20..0x24].copy_from_slice(&arm9_rom_offset.to_le_bytes());
    rom[0x24..0x28].copy_from_slice(&arm9_entry.to_le_bytes());
    rom[0x28..0x2C].copy_from_slice(&arm9_ram.to_le_bytes());
    rom[0x2C..0x30].copy_from_slice(&arm9_size.to_le_bytes());

    rom[0x30..0x34].copy_from_slice(&arm7_rom_offset.to_le_bytes());
    rom[0x34..0x38].copy_from_slice(&arm7_entry.to_le_bytes());
    rom[0x38..0x3C].copy_from_slice(&arm7_ram.to_le_bytes());
    rom[0x3C..0x40].copy_from_slice(&arm7_size.to_le_bytes());

    // Autoload address setup: pointing to 0x02000100 (offset 0x100 inside ARM9 binary)
    let arm9_autoload_addr = 0x02000100u32;
    rom[0x074..0x078].copy_from_slice(&arm9_autoload_addr.to_le_bytes());

    // Autoload Entry 1: dest = 0x0B000000 (DTCM), size = u32::MAX (overflow check), bss_size = 0
    let entry1_offset = 0x200 + 0x100;
    rom[entry1_offset..entry1_offset + 4].copy_from_slice(&0x0B000000u32.to_le_bytes());
    rom[entry1_offset + 4..entry1_offset + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    rom[entry1_offset + 8..entry1_offset + 12].copy_from_slice(&0u32.to_le_bytes());

    // Autoload Entry 2: dest = 0x0B000100, size = 0x10, bss_size = 9 * 1024 * 1024 (huge BSS check)
    let entry2_offset = entry1_offset + 12;
    rom[entry2_offset..entry2_offset + 4].copy_from_slice(&0x0B000100u32.to_le_bytes());
    rom[entry2_offset + 4..entry2_offset + 8].copy_from_slice(&0x10u32.to_le_bytes());
    rom[entry2_offset + 8..entry2_offset + 12]
        .copy_from_slice(&(9u32 * 1024 * 1024).to_le_bytes());

    // Terminate table
    let entry3_offset = entry2_offset + 12;
    rom[entry3_offset..entry3_offset + 4].copy_from_slice(&0u32.to_le_bytes());

    // Boot load should not crash or run out of memory
    let result = boot_load_rom(&mut mmu, &mut arm9, &mut arm7, &rom);
    assert!(result.is_ok(), "Boot loading must succeed without panic under malformed/extreme inputs");

    // Verify DTCM RAM is unaffected by the huge BSS size
    assert_eq!(mmu.dtcm[0], 0);
}

/// A TCM window based near the top of memory continues across the 32-bit wrap,
/// so address 0 lands inside it — provided the size field actually spans that
/// far. The physical TCM is mirrored within the window, so the byte shows up at
/// `(addr - base) % len`.
#[test]
fn test_tcm_wrapping_address_itcm() {
    let mut mmu = NdsMmu::new();
    // ITCM only: the reset DTCM window would otherwise also cover address 0 and
    // it is ambiguous which TCM claims the access.
    let mut cp15 = mmu.cp15();
    cp15.control = 1 << 18;
    // Base 0xFFFF9000 with N=6 -> 0x8000 bytes, so the window covers
    // 0xFFFF9000..0xFFFFFFFF and wraps on through 0x00006FFF.
    cp15.itcm_control = 0xFFFF9000 | (6 << 1);
    mmu.set_cp15(cp15);
    assert!(mmu.in_itcm_range_arm9(0x00000000), "the window wraps to cover address 0");

    mmu.write_byte_arm9(0x00000000, 0x55);
    assert_eq!(mmu.read_byte_arm9(0x00000000), 0x55);

    let offset = 0x00000000u32.wrapping_sub(0xFFFF9000) as usize;
    assert_eq!(mmu.itcm[offset % mmu.itcm.len()], 0x55, "byte lands in the mirrored ITCM");
}

/// DTCM counterpart of [`test_tcm_wrapping_address_itcm`].
#[test]
fn test_tcm_wrapping_address_dtcm() {
    let mut mmu = NdsMmu::new();
    // DTCM only: with ITCM also enabled its window covers address 0 and claims
    // the access first, which is what this test used to trip over.
    let mut cp15 = mmu.cp15();
    cp15.control = 1 << 16;
    // Base 0xFFFFE000 with N=5 -> 0x4000 bytes: covers 0xFFFFE000..0x00001FFF.
    cp15.dtcm_control = 0xFFFFE000 | (5 << 1);
    mmu.set_cp15(cp15);
    assert!(mmu.in_dtcm_range_arm9(0x00000000), "the window wraps to cover address 0");

    mmu.write_byte_arm9(0x00000000, 0xAA);
    assert_eq!(mmu.read_byte_arm9(0x00000000), 0xAA);

    let offset = 0x00000000u32.wrapping_sub(0xFFFFE000) as usize;
    assert_eq!(mmu.dtcm[offset % mmu.dtcm.len()], 0xAA, "byte lands in the mirrored DTCM");
}
