// core/tests/hle_verification_tests.rs

use emulator_core::nds::mmu::NdsMmu;
use emulator_core::nds::cpu::Arm9Cpu;
use emulator_core::nds::cpu::Arm7Cpu;
use emulator_core::nds::hle::boot_load_rom;

// Helper to encode MCR p15 instruction
fn make_mcr_p15(crn: u32, rd: u32, crm: u32, opcode_2: u32) -> u32 {
    let cond = 0xEu32;
    let const1 = 0xEu32;
    let opcode_1 = 0u32;
    let l = 0u32; // MCR
    let const2 = 0xFu32;
    let bit4 = 1u32;
    (cond << 28)
        | (const1 << 24)
        | (opcode_1 << 21)
        | (l << 20)
        | (crn << 16)
        | (rd << 12)
        | (const2 << 8)
        | (opcode_2 << 5)
        | (bit4 << 4)
        | crm
}

#[test]
fn test_cp15_tcm_mapping() {
    let mut mmu = NdsMmu::new();
    let mut cpu = Arm9Cpu::new();

    // Set CPU to ARM state (not Thumb)
    cpu.cpu.registers.cpsr = 0x13; // Supervisor Mode

    // 1. Enable TCM globally via Control Register c1
    // Write 0x00050000 (bits 16 and 18 set) to r0
    cpu.cpu.registers.gpr[0] = 0x00050000;
    // Instruction: MCR p15, 0, r0, c1, c0, 0
    let inst_c1 = make_mcr_p15(1, 0, 0, 0);
    cpu.cpu.pipeline[0] = inst_c1;
    cpu.cpu.pipeline[1] = 0; // NOP
    cpu.step(&mut mmu);

    // Verify propagation
    assert_eq!(mmu.cp15().control, 0x00050000);

    // 2. Point DTCM at 0x0B000000 with a 16 KB window. Bits 5:1 are the size
    // field (`512 << N`), so N=5 gives 0x4000 — big enough for the 0x321 offset
    // written below. Bit 0 is not an enable; enabling is c1 bit 16, set above.
    cpu.cpu.registers.gpr[1] = 0x0B000000 | (5 << 1);
    // Instruction: MCR p15, 0, r1, c9, c1, 0
    let inst_dtcm = make_mcr_p15(9, 1, 1, 0); // Wait, CRm=1, opcode_2=0 for DTCM
    cpu.cpu.pipeline[0] = inst_dtcm;
    cpu.step(&mut mmu);

    assert_eq!(mmu.cp15().dtcm_control, 0x0B000000 | (5 << 1));

    // 3. Point ITCM at 0x01000000 with a 32 KB window (N=6).
    cpu.cpu.registers.gpr[2] = 0x01000000 | (6 << 1);
    // Instruction: MCR p15, 0, r2, c9, c1, 1
    let inst_itcm = make_mcr_p15(9, 2, 1, 1); // Wait, CRn=9, Rd=2, CRm=1, opcode_2=1
    cpu.cpu.pipeline[0] = inst_itcm;
    cpu.step(&mut mmu);

    assert_eq!(mmu.cp15().itcm_control, 0x01000000 | (6 << 1));

    // 4. Verify TCM read/write routing
    assert!(mmu.itcm_enabled());
    assert!(mmu.dtcm_enabled());
    assert_eq!(mmu.itcm_base(), 0x01000000);
    assert_eq!(mmu.dtcm_base(), 0x0B000000);

    // Write to ITCM range
    mmu.write_byte_arm9(0x01000123, 0x42);
    assert_eq!(mmu.read_byte_arm9(0x01000123), 0x42);
    assert_eq!(mmu.itcm[0x123], 0x42);

    // Write to DTCM range
    mmu.write_byte_arm9(0x0B000321, 0x24);
    assert_eq!(mmu.read_byte_arm9(0x0B000321), 0x24);
    assert_eq!(mmu.dtcm[0x321], 0x24);
}

#[test]
fn test_shared_wram_modes() {
    let mut mmu = NdsMmu::new();

    // Exercise WRAMCNT's ownership table through its register write path.
    // Source: https://problemkaputt.de/gbatek.htm#dsmemorycontrolwram
    // Addresses and sizes below describe this emulator's existing expanded
    // WRAM layout (256 KB); they do not assert the hardware's physical size.
    // --- MODE 0: All 256KB to ARM9 ---
    mmu.write_byte_arm9(0x04000247, 0);
    mmu.write_byte_arm9(0x02400000, 0x11);
    mmu.write_byte_arm9(0x02420000, 0x22); // offset 128KB
    assert_eq!(mmu.shared_wram[0], 0x11);
    assert_eq!(mmu.shared_wram[128 * 1024], 0x22);
    assert_eq!(mmu.read_byte_arm9(0x02400000), 0x11);
    assert_eq!(mmu.read_byte_arm9(0x02420000), 0x22);

    // Mode 0 gives ARM7 no shared WRAM at all, so its 0x03000000 window falls
    // through to its own 64 KB ARM7-WRAM (the 0x03800000 mirror) — the write
    // must miss shared WRAM but still be readable back from ARM7.
    mmu.write_byte_arm7(0x03000000, 0x99);
    assert_eq!(mmu.shared_wram[0], 0x11, "shared WRAM must be untouched in mode 0");
    assert_eq!(mmu.arm7_wram[0], 0x99, "the write lands in ARM7-WRAM instead");
    assert_eq!(mmu.read_byte_arm7(0x03000000), 0x99);
    assert_eq!(mmu.read_byte_arm7(0x03800000), 0x99, "same byte through the direct window");

    // --- MODE 3: All 256KB to ARM7 ---
    mmu.write_byte_arm9(0x04000247, 3);
    mmu.shared_wram.fill(0);
    mmu.write_byte_arm7(0x03000000, 0x33);
    mmu.write_byte_arm7(0x03020000, 0x44); // offset 128KB
    assert_eq!(mmu.shared_wram[0], 0x33);
    assert_eq!(mmu.shared_wram[128 * 1024], 0x44);
    assert_eq!(mmu.read_byte_arm7(0x03000000), 0x33);
    assert_eq!(mmu.read_byte_arm7(0x03020000), 0x44);

    // ARM9 should not access it in Mode 3
    mmu.write_byte_arm9(0x02400000, 0x99);
    assert_eq!(mmu.shared_wram[0], 0x33); // Unchanged
    assert_eq!(mmu.read_byte_arm9(0x02400000), 0);

    // --- MODE 2: Split, Block 0 to ARM9, Block 1 to ARM7 ---
    mmu.write_byte_arm9(0x04000247, 2);
    mmu.shared_wram.fill(0);

    // ARM9 writes to Block 0 (offset < 128KB)
    mmu.write_byte_arm9(0x02400005, 0x55);
    assert_eq!(mmu.shared_wram[5], 0x55);
    assert_eq!(mmu.read_byte_arm9(0x02400005), 0x55);
    // ARM9 trying to access Block 1 range (offset >= 128KB) should be ignored/return 0
    mmu.write_byte_arm9(0x02420005, 0x99);
    assert_eq!(mmu.shared_wram[128 * 1024 + 5], 0);
    assert_eq!(mmu.read_byte_arm9(0x02420005), 0);

    // ARM7 writes to Block 1 (should mirror inside Block 1 range: shared_wram[128KB..256KB])
    mmu.write_byte_arm7(0x03000005, 0x66); // offset 5 % 128KB = 5 -> shared_wram[128KB + 5]
    assert_eq!(mmu.shared_wram[128 * 1024 + 5], 0x66);
    assert_eq!(mmu.read_byte_arm7(0x03000005), 0x66);
    assert_eq!(mmu.read_byte_arm7(0x03020005), 0x66); // Mirroring check

    // --- MODE 1: Split, Block 1 to ARM9, Block 0 to ARM7 ---
    mmu.write_byte_arm9(0x04000247, 1);
    mmu.shared_wram.fill(0);

    // ARM9 writes to Block 1 (offset 256KB..384KB maps to Block 1)
    mmu.write_byte_arm9(0x02440005, 0x77); // 0x440005 - 0x400000 = 256KB + 5
    assert_eq!(mmu.shared_wram[128 * 1024 + 5], 0x77);
    assert_eq!(mmu.read_byte_arm9(0x02440005), 0x77);
    // ARM9 trying to access Block 0 range (offset < 256KB) should return 0/ignored
    mmu.write_byte_arm9(0x02400005, 0x99);
    assert_eq!(mmu.shared_wram[5], 0);
    assert_eq!(mmu.read_byte_arm9(0x02400005), 0);

    // ARM7 writes to Block 0
    mmu.write_byte_arm7(0x03000005, 0x88);
    assert_eq!(mmu.shared_wram[5], 0x88);
    assert_eq!(mmu.read_byte_arm7(0x03000005), 0x88);
    assert_eq!(mmu.read_byte_arm7(0x03020005), 0x88); // Mirroring check
}

#[test]
fn test_ipc_fifo_control_updates_and_errors() {
    let mut mmu = NdsMmu::new();

    // 1. Enable FIFOs on both sides
    mmu.write_ipc_fifo_cnt_arm9(1 << 15);
    mmu.write_ipc_fifo_cnt_arm7(1 << 15);

    // 2. Clear TX FIFO on ARM9 (bit 3)
    mmu.write_ipc_fifo_tx_arm9(0xAA);
    assert_eq!(mmu.ipc.fifo_9to7.len(), 1);
    mmu.write_ipc_fifo_cnt_arm9((1 << 15) | (1 << 3)); // Enable + Clear TX
    assert!(mmu.ipc.fifo_9to7.is_empty());

    // 3. Disabling empties THIS core's send FIFO and leaves the other core's
    //    alone. IPCFIFOCNT is per-CPU: `fifo_7to9` is the ARM7's send queue and
    //    the ARM9's receive view of it, so clearing it from the ARM9 side is a
    //    cross-core write no hardware can perform. This test used to assert the
    //    opposite ("Disabling should clear everything"), which is why an ARM9
    //    that wrote the low byte of its own IPCFIFOCNT — the read-modify-write
    //    a plain STRH performs — could destroy a handshake word the ARM7 had
    //    already queued.
    mmu.write_ipc_fifo_tx_arm9(0x11);
    mmu.write_ipc_fifo_tx_arm7(0x22);
    assert_eq!(mmu.ipc.fifo_9to7.len(), 1);
    assert_eq!(mmu.ipc.fifo_7to9.len(), 1);

    // Disable ARM9 FIFO
    mmu.write_ipc_fifo_cnt_arm9(0);
    assert!(mmu.ipc.fifo_9to7.is_empty(), "own send FIFO is emptied");
    assert_eq!(
        mmu.ipc.fifo_7to9.len(),
        1,
        "the ARM7's queued word must survive an ARM9 disable"
    );
    assert_eq!(mmu.ipc.fifo_control_arm9 & ((1 << 15) | (1 << 14)), 0);

    // Drain it the only legitimate way: from the core that owns it.
    mmu.write_ipc_fifo_cnt_arm7((1 << 15) | (1 << 3));
    assert!(mmu.ipc.fifo_7to9.is_empty());

    // 4. Test Underflow and Overflow Error Flags (Bit 14)
    mmu.write_ipc_fifo_cnt_arm9(1 << 15);
    mmu.write_ipc_fifo_cnt_arm7(1 << 15);

    // Read from empty RX on ARM9 -> Underflow
    assert_eq!(mmu.ipc.fifo_7to9.len(), 0);
    let val = mmu.read_ipc_fifo_rx_arm9();
    assert_eq!(val, 0);
    // Error bit 14 should be set on ARM9
    assert_ne!(mmu.ipc.fifo_control_arm9 & (1 << 14), 0);

    // Write 1 to bit 14 to clear it
    mmu.write_ipc_fifo_cnt_arm9((1 << 15) | (1 << 14));
    assert_eq!(mmu.ipc.fifo_control_arm9 & (1 << 14), 0);

    // Fill ARM9's TX FIFO (16 words)
    for i in 0..16 {
        mmu.write_ipc_fifo_tx_arm9(i);
    }
    assert_eq!(mmu.ipc.fifo_9to7.len(), 16);
    assert_eq!(mmu.ipc.fifo_control_arm9 & (1 << 14), 0);

    // Push 17th word -> Overflow
    mmu.write_ipc_fifo_tx_arm9(99);
    assert_ne!(mmu.ipc.fifo_control_arm9 & (1 << 14), 0);
    assert_eq!(mmu.ipc.fifo_9to7.len(), 16); // Should not have grown
}

#[test]
fn test_ipc_fifo_bugs_status_and_swaps() {
    let mut mmu = NdsMmu::new();

    // Enable FIFOs
    mmu.write_ipc_fifo_cnt_arm9(1 << 15);
    mmu.write_ipc_fifo_cnt_arm7(1 << 15);

    // --- Bug 1: ARM9 RX Full status bit typo (sets bit 9 instead of bit 1) ---
    // Fill fifo_7to9 (ARM9's RX FIFO)
    for i in 0..16 {
        mmu.write_ipc_fifo_tx_arm7(i);
    }
    let cnt9 = mmu.read_ipc_fifo_cnt_arm9();
    // RX FIFO is full, so Bit 9 (RX Full) should be 1.
    // TX FIFO is empty, so Bit 1 (TX Full) should be 0.
    // Under buggy code, cnt9 will have bit 9 set instead of bit 1.
    // We expect this assert to FAIL under the buggy implementation.
    assert_eq!(cnt9 & (1 << 9), 1 << 9, "RX Full bit 9 should be set");
    assert_eq!(cnt9 & (1 << 1), 0, "TX Full bit 1 should NOT be set when TX is empty");

    // --- Bug 2: ARM7 RX/TX status Swap ---
    // Clear everything
    // Reset both directions explicitly. Disabling the ARM9 no longer flushes the
    // ARM7's send queue (see the note in the control test), so each core clears
    // its own with the documented bit-3 flush.
    mmu.write_ipc_fifo_cnt_arm9((1 << 15) | (1 << 3));
    mmu.write_ipc_fifo_cnt_arm7((1 << 15) | (1 << 3));
    assert!(mmu.ipc.fifo_9to7.is_empty() && mmu.ipc.fifo_7to9.is_empty());

    // Push 1 word from ARM9 to ARM7 (fifo_9to7 has length 1).
    // This is ARM7's RX FIFO. So ARM7's RX is not empty.
    // ARM7's TX FIFO (fifo_7to9) is empty.
    mmu.write_ipc_fifo_tx_arm9(0x123);

    let cnt7 = mmu.read_ipc_fifo_cnt_arm7();
    // Bit 8 (RX Empty) should be 0 (since RX has 1 word).
    // Bit 0 (TX Empty) should be 1 (since TX is empty).
    // Under buggy code, cnt7 checks fifo_7to9 for RX (Bit 0) and fifo_9to7 for TX (Bit 8),
    // which swaps them: RX Empty is set to 1, and TX Empty is set to 0.
    assert_eq!(cnt7 & (1 << 8), 0, "ARM7 RX Empty bit 8 should be 0");
    assert_eq!(cnt7 & (1 << 0), 1, "ARM7 TX Empty bit 0 should be 1");

    // --- Bug 3: Swapped Interrupt Enable Bits ---
    // Clear everything
    mmu.write_ipc_fifo_cnt_arm9(0);
    mmu.write_ipc_fifo_cnt_arm9(1 << 15);
    mmu.write_ipc_fifo_cnt_arm7(1 << 15);

    // Enable ARM7 RX Not Empty Interrupt (bit 10) and clear bit 2
    mmu.write_ipc_fifo_cnt_arm7((1 << 15) | (1 << 10));
    mmu.arm7_if = 0; // Clear interrupt flags

    // Push a word from ARM9 to ARM7.
    // This should trigger ARM7 IRQ (bit 17: IPC Recv IRQ) because RX is now not empty and bit 10 is set.
    mmu.write_ipc_fifo_tx_arm9(0xABC);
    // ARM7 IF bit 18 is "IPC Recv FIFO Not Empty"; bit 17 is "IPC Send FIFO
    // Empty", which this assertion used to name by mistake.
    assert_ne!(
        mmu.arm7_if & (1 << 18),
        0,
        "Should trigger ARM7 Recv IRQ when RX becomes non-empty"
    );
    assert_eq!(mmu.arm7_if & (1 << 17), 0, "the Send-Empty IRQ is a different line");
}

#[test]
fn test_nds_boot_loading() {
    let mut mmu = NdsMmu::new();
    let mut arm9 = Arm9Cpu::new();
    let mut arm7 = Arm7Cpu::new();

    // Create a mock NDS ROM image
    let mut rom = vec![0u8; 0x1000];

    // Write title and game code
    rom[0..12].copy_from_slice(b"TESTGAME\0\0\0\0");
    rom[0x0C..0x10].copy_from_slice(b"NTRP");

    // Write offsets and entry points
    let arm9_rom_offset = 0x200u32;
    let arm9_entry = 0x02000800u32;
    let arm9_ram = 0x02000000u32;
    let arm9_size = 0x100u32;

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

    // Write autoload tables
    let arm9_autoload_addr = 0x02000080u32; // inside ARM9 binary range
    rom[0x074..0x078].copy_from_slice(&arm9_autoload_addr.to_le_bytes());

    // Let's populate the ARM9 binary in ROM
    // The binary in ROM is at 0x200 to 0x300.
    // The autoload table is at offset 0x80 inside the binary (RAM address 0x02000080).
    // So ROM offset of autoload table is 0x200 + 0x80 = 0x280.
    // Let's put an entry in the autoload table:
    // Dest = 0x0B000000 (DTCM address), Size = 0x20, BSS Size = 0x10.
    let dest_addr = 0x0B000000u32;
    let sec_size = 0x20u32;
    let bss_size = 0x10u32;

    rom[0x280..0x284].copy_from_slice(&dest_addr.to_le_bytes());
    rom[0x284..0x288].copy_from_slice(&sec_size.to_le_bytes());
    rom[0x288..0x28C].copy_from_slice(&bss_size.to_le_bytes());
    // Terminate entry (dest = 0)
    rom[0x28C..0x290].copy_from_slice(&0u32.to_le_bytes());

    // Write some dummy code at the entry point of ARM9 (0x200 + 0x800 - 0x02000000 is not within 0x100 size).
    // Wait, the entry address 0x02000800 is outside the loaded range of size 0x100 (which is 0x02000000 to 0x02000100).
    // Let's adjust sizes so entry point is within the loaded binary.
    // Let's make ARM9 size = 0x1000. So binary is 0x02000000 to 0x02001000.
    // Let's write the size to 0x1000.
    let arm9_size_large = 0x1000u32;
    rom[0x2C..0x30].copy_from_slice(&arm9_size_large.to_le_bytes());

    // Autoload table in ROM is at 0x200 + 0x80 = 0x280.
    // The sections in ROM start after the ARM9 binary. So at 0x200 + 0x1000 = 0x1200.
    // Let's resize rom to 0x2000 to fit everything.
    rom.resize(0x2000, 0);

    // Let's write the source data for DTCM section at 0x1200
    for i in 0..0x20 {
        rom[0x1200 + i] = 0xBB;
    }

    // Write some instructions at ARM9 entry point (0x200 + 0x800 = 0xA00)
    // Instruction 1: NOP (0xE1A00000)
    // Instruction 2: NOP (0xE1A00000)
    rom[0xA00..0xA04].copy_from_slice(&0xE1A00000u32.to_le_bytes());
    rom[0xA04..0xA08].copy_from_slice(&0xE1A00000u32.to_le_bytes());

    // Perform HLE boot load
    let result = boot_load_rom(&mut mmu, &mut arm9, &mut arm7, &rom);
    assert!(result.is_ok());

    // Verify PC and pipeline
    // ARM9 PC should be entry_point + 8 = 0x02000808
    assert_eq!(arm9.cpu.registers.gpr[15], 0x02000808);
    // Pipeline should be filled with instructions from entry point
    assert_eq!(arm9.cpu.pipeline[0], 0xE1A00000);
    assert_eq!(arm9.cpu.pipeline[1], 0xE1A00000);

    // Verify DTCM autoloaded section is in DTCM RAM
    // Since DTCM is not enabled yet, read_byte_arm9 will check it if DTCM base is set.
    // Wait, the autoload table loads bytes to 0x0B000000, but is DTCM enabled by boot?
    // In `boot_load_rom`, we reset CP15, so DTCM is disabled. We can read it directly from `mmu.dtcm`.
    // Wait, let's verify if `dest_addr` was 0x0B000000 (DTCM).
    // The BSS section should also be cleared (dest_addr + size to dest_addr + size + bss_size).
    // Let's check:
    assert_eq!(mmu.dtcm[0], 0xBB);
    assert_eq!(mmu.dtcm[0x1F], 0xBB);
    assert_eq!(mmu.dtcm[0x20], 0); // BSS start
    assert_eq!(mmu.dtcm[0x2F], 0); // BSS end

    // Verify boot success indicator flags at top of main RAM
    assert_eq!(mmu.read_byte_arm9(0x027FFFC0), 0x66);
    assert_eq!(mmu.read_byte_arm7(0x027FFFC4), 0x66);
}

#[test]
fn test_nds_boot_autoload_robustness() {
    let mut mmu = NdsMmu::new();
    let mut arm9 = Arm9Cpu::new();
    let mut arm7 = Arm7Cpu::new();

    let mut rom = vec![0u8; 0x2000];

    // Write title and game code
    rom[0..12].copy_from_slice(b"TESTGAME\0\0\0\0");
    rom[0x0C..0x10].copy_from_slice(b"NTRP");

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

    // Write autoload tables
    let arm9_autoload_addr = 0x02000080u32;
    rom[0x074..0x078].copy_from_slice(&arm9_autoload_addr.to_le_bytes());

    // Write an entry with size = u32::MAX (to trigger overflow on current_rom_src + size)
    let dest_addr = 0x0B000000u32;
    let sec_size = u32::MAX;
    // Typed: the header field is a 4-byte LE size, so the literal must be u32.
    let bss_size: u32 = 9 * 1024 * 1024; // > 8MB

    rom[0x280..0x284].copy_from_slice(&dest_addr.to_le_bytes());
    rom[0x284..0x288].copy_from_slice(&sec_size.to_le_bytes());
    rom[0x288..0x28C].copy_from_slice(&bss_size.to_le_bytes());
    // Terminate entry (dest = 0)
    rom[0x28C..0x290].copy_from_slice(&0u32.to_le_bytes());

    // Perform HLE boot load
    let result = boot_load_rom(&mut mmu, &mut arm9, &mut arm7, &rom);
    // Should succeed because overflow size and huge BSS size are safely ignored and do not panic
    assert!(result.is_ok());

    // Verify DTCM RAM is still empty/0 since copying and BSS clearing were bypassed
    assert_eq!(mmu.dtcm[0], 0);
}

#[test]
fn test_milestone4_ppu_and_rendering() {
    let mut mmu = NdsMmu::new();

    // 1. Palette RAM and OAM routing
    mmu.write_byte_arm9(0x05000123, 0xAA);
    mmu.write_byte_arm9(0x07000456, 0x55);
    assert_eq!(mmu.read_byte_arm9(0x05000123), 0xAA);
    assert_eq!(mmu.read_byte_arm9(0x07000456), 0x55);

    // 2. VRAMCNT_B and WRAMCNT mapping
    mmu.vram.banks[1].control = 0x12;
    mmu.wram_control = 0x34;
    assert_eq!(mmu.read_byte_arm9(0x04000241), 0x12);
    assert_eq!(mmu.read_byte_arm9(0x04000247), 0x34);

    mmu.write_byte_arm9(0x04000241, 0x88);
    mmu.write_byte_arm9(0x04000247, 0x99);
    assert_eq!(mmu.vram.banks[1].control, 0x88);
    assert_eq!(mmu.wram_control, 0x99);

    // 3. REG_GXSTAT intercept. Byte 0 carries bit 1 = BOX_TEST result, and the
    // HLE always answers "inside the view volume" (0x02): the overworld
    // box-tests every object before drawing it, and answering "all outside"
    // made the game skip drawing the player, NPCs and furniture, leaving only
    // the always-drawn map. This assertion used to require the 0 that did that.
    assert_eq!(mmu.read_byte_arm9(0x04000600), 0x02);
    // Byte 1 holds the matrix stack level (bits 8-12) and the stack-error flag
    // (bit 14). Both must read 0: `G3X_Reset` acknowledges the error with bit 15
    // and then polls for it to clear, so a hardwired 0xC0 here spun the game
    // forever before the title screen. The old expectation was that hang.
    assert_eq!(mmu.read_byte_arm9(0x04000601), 0);
    assert_eq!(mmu.read_byte_arm9(0x04000602), 0, "FIFO entry count is always 0");
    // Byte 3 holds bit 25 (FIFO less than half full) and bit 26 (FIFO empty).
    // Commands are consumed the moment they arrive, so both always read 1 —
    // drivers that wait for space or for drain would otherwise block forever.
    assert_eq!(mmu.read_byte_arm9(0x04000603), 0x06);

    // 4. has_3d_activity toggle
    assert!(!mmu.has_3d_activity);
    mmu.write_byte_arm9(0x04000400, 0x11);
    assert!(mmu.has_3d_activity);

    // Reset activity for PPU tick tests
    mmu.has_3d_activity = false;

    // 5. PPU ticking & interrupts
    let mut ppu = emulator_core::nds::ppu::NdsPpu::new();
    let mut video_buffer = vec![0u16; 256 * 384];

    // Enable HBlank and VBlank interrupts in DISPSTAT (arm9_io[4] & arm7_io[4])
    // bit 3: VBlank IRQ enable, bit 4: HBlank IRQ enable
    mmu.arm9_io[4] = (1 << 3) | (1 << 4);
    mmu.arm7_io[4] = (1 << 3) | (1 << 4);
    mmu.set_vcount(0);

    // Tick to HBlank start: 256 visible dots x 6 = 1536 bus cycles. This said
    // 512 — one third of the real value — from before the NDS scanline timing
    // was corrected.
    ppu.tick(1536, &mut mmu, &mut video_buffer, true);

    // Check HBlank flag (bit 1) is set and interrupts triggered
    let dispstat_9 = ((mmu.arm9_io[5] as u16) << 8) | (mmu.arm9_io[4] as u16);
    assert_ne!(dispstat_9 & (1 << 1), 0);
    assert_ne!(mmu.arm9_if & (1 << 1), 0);
    assert_ne!(mmu.arm7_if & (1 << 1), 0);

    // Tick to the end of the scanline: 355 dots x 6 = 2130 bus cycles.
    ppu.tick(2130 - 1536, &mut mmu, &mut video_buffer, true);
    // VCOUNT should now be 1
    let vcount = ((mmu.arm9_io[7] as u16) << 8) | (mmu.arm9_io[6] as u16);
    assert_eq!(vcount, 1);
    // HBlank flag should be cleared
    let dispstat_9 = ((mmu.arm9_io[5] as u16) << 8) | (mmu.arm9_io[4] as u16);
    assert_eq!(dispstat_9 & (1 << 1), 0);

    // 6. VCompare match, LYC = 2.
    //
    // GBATEK splits the V-Count setting: LYC bits 0-7 are DISPSTAT bits 8-15
    // (the high byte, io[5]) and LYC bit 8 is DISPSTAT bit 7. This setup used to
    // write io[5] = 1 for an intended LYC of 2, matching the PPU's old
    // `(dispstat >> 7) & 0x1FF` decode — i.e. the test encoded the same
    // off-by-one-shift the code had, so it passed while both were wrong.
    mmu.arm9_io[4] |= 1 << 5; // VCounter Match IRQ enable
    mmu.arm9_io[5] = 2; // LYC bits 0-7
    mmu.arm9_io[4] &= !0x80; // LYC bit 8 = 0
    mmu.arm7_io[4] |= 1 << 5;
    mmu.arm7_io[5] = 2;
    mmu.arm7_io[4] &= !0x80;

    // Run until vcount reaches 2
    // We are at scanline 1.
    // Tick through scanline 1 (710 cycles)
    ppu.tick(2130, &mut mmu, &mut video_buffer, true);
    // vcount is now 2. Check VCompare Match flag (bit 2) is set and interrupt triggered
    let dispstat_9 = ((mmu.arm9_io[5] as u16) << 8) | (mmu.arm9_io[4] as u16);
    assert_ne!(dispstat_9 & (1 << 2), 0);
    assert_ne!(mmu.arm9_if & (1 << 2), 0);
    assert_ne!(mmu.arm7_if & (1 << 2), 0);

    // 7. VBlank transition at vcount 192
    // Fast-forward VCOUNT to 191
    mmu.set_vcount(191);
    // Tick 710 cycles to wrap to 192
    ppu.tick(2130, &mut mmu, &mut video_buffer, true);
    // VCOUNT is now 192
    let vcount = ((mmu.arm9_io[7] as u16) << 8) | (mmu.arm9_io[6] as u16);
    assert_eq!(vcount, 192);
    // VBlank flag (bit 0) should be set
    let dispstat_9 = ((mmu.arm9_io[5] as u16) << 8) | (mmu.arm9_io[4] as u16);
    assert_ne!(dispstat_9 & (1 << 0), 0);
    // VBlank interrupt triggered
    assert_ne!(mmu.arm9_if & (1 << 0), 0);
    assert_ne!(mmu.arm7_if & (1 << 0), 0);
    // frame_completed should be set
    assert!(ppu.frame_completed);

    // 8. Screen swap and Grid Rendering
    mmu.has_3d_activity = true;
    ppu.frame_count = 5;
    mmu.arm9_io[0x305] = 0x80; // Set POWCNT bit 15 (bit 7 of arm9_io[0x305]) to 1 for swap

    // Render scanline 100 (which is > 96, the ground)
    ppu.render_scanline(100, &mmu, &mut video_buffer);

    // With swap enabled, bottom screen starts at (192+100)*256. It routes Engine A (Grid)
    // Let's check some pixel in bottom screen. Since has_3d_activity is true and ly > 96,
    // it will have grid lines or fading blue.
    let pixel = video_buffer[(192 + 100) * 256 + 128];
    assert_ne!(pixel, 0); // Should be grid color or fading blue

    // 9. Touch dot overlay
    mmu.buttons.nds_touch_pressed = true;
    mmu.buttons.nds_touch_x = 50;
    mmu.buttons.nds_touch_y = 100;
    ppu.render_scanline(100, &mmu, &mut video_buffer);

    // Check 2x2 yellow dot on bottom screen (using bottom screen offset)
    // yellow is bgr555(255, 255, 0) => (((0u16) >> 3) << 10) | (((255 as u16) >> 3) << 5) | ((255 as u16) >> 3) = (31 << 5) | 31 = 0x03FF
    let yellow = 0x03FF;
    assert_eq!(video_buffer[(192 + 100) * 256 + 50], yellow);
    assert_eq!(video_buffer[(192 + 100) * 256 + 51], yellow);
}
