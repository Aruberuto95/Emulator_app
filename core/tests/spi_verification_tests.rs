// core/tests/spi_verification_tests.rs

use emulator_core::nds::mmu::NdsMmu;

#[test]
fn test_spi_continuous_cs_hold_tsc() {
    let mut mmu = NdsMmu::new();
    mmu.spi.tsc.touch_x = 100;
    mmu.spi.tsc.touch_y = 120;
    mmu.spi.tsc.touch_pressed = true;

    // --- CASE A: CS Hold Enabled ---
    // Enable SPI, CS Hold, Device 2 (TSC), 8-bit size
    // SPICNT = Bit 15 (0x8000) | Bit 10 (0x0400) | Device 2 (0x0200) = 0x8600
    mmu.write_halfword_arm7(0x040001C0, 0x8600);

    // Write Control Byte: Start=1, Channel=5 (X-coord), 12-bit mode
    // 0x90 | (5 << 4) = 0xD0
    mmu.write_byte_arm7(0x040001C2, 0xD0);
    assert_eq!(mmu.read_byte_arm7(0x040001C2), 0);

    // Read MSB of result (first dummy byte write)
    mmu.write_byte_arm7(0x040001C2, 0);
    let msb = mmu.read_byte_arm7(0x040001C2);

    // Read LSB of result (second dummy byte write)
    mmu.write_byte_arm7(0x040001C2, 0);
    let lsb = mmu.read_byte_arm7(0x040001C2);

    let raw_x = ((msb as u16) << 5) | ((lsb as u16) >> 3);
    assert_eq!(raw_x, 1528); // Expected: (100 - 64) * 18 + 880 = 1528

    // --- CASE B: CS Hold Disabled ---
    // Enable SPI, CS Hold disabled (0x0000), Device 2 (TSC), 8-bit size
    // SPICNT = Bit 15 (0x8000) | Device 2 (0x0200) = 0x8200
    mmu.write_halfword_arm7(0x040001C0, 0x8200);

    // Write Control Byte
    mmu.write_byte_arm7(0x040001C2, 0xD0);
    // Since CS Hold is 0, the state machine should be reset immediately at the end of this write.
    // Let's verify that subsequent dummy writes do not return the result (they should return 0)
    mmu.write_byte_arm7(0x040001C2, 0);
    let msb_no_hold = mmu.read_byte_arm7(0x040001C2);
    assert_eq!(msb_no_hold, 0, "TSC state machine should reset immediately when CS Hold is disabled");
}

#[test]
fn test_spi_continuous_cs_hold_pmic() {
    let mut mmu = NdsMmu::new();

    // Enable SPI, CS Hold, Device 0 (PMIC), 8-bit size
    // SPICNT = Bit 15 (0x8000) | Bit 10 (0x0400) | Device 0 (0x0000) = 0x8400
    mmu.write_halfword_arm7(0x040001C0, 0x8400);

    // Write command: Read Reg 0 (backlight status) -> Bit 7=1 (read), Reg=0 -> 0x80
    mmu.write_byte_arm7(0x040001C2, 0x80);
    assert_eq!(mmu.read_byte_arm7(0x040001C2), 0);

    // Write dummy byte to read response
    mmu.write_byte_arm7(0x040001C2, 0);
    let resp = mmu.read_byte_arm7(0x040001C2);
    assert_eq!(resp, 0x03, "PMIC read register 0 should return 0x03 (backlight status)");

    // Disable CS Hold
    mmu.write_halfword_arm7(0x040001C0, 0x8000);
    mmu.write_byte_arm7(0x040001C2, 0x80);
    // State machine resets at the end of the write. Next dummy write should NOT return PMIC response.
    mmu.write_byte_arm7(0x040001C2, 0);
    let resp_no_hold = mmu.read_byte_arm7(0x040001C2);
    assert_eq!(resp_no_hold, 0, "PMIC state machine should reset when CS Hold is disabled");
}

#[test]
fn test_spi_transfer_size_configurations() {
    let mut mmu = NdsMmu::new();
    mmu.spi.tsc.touch_x = 100;
    mmu.spi.tsc.touch_y = 120;
    mmu.spi.tsc.touch_pressed = true;

    // Enable SPI, CS Hold, Device 2 (TSC), 16-bit transfer size
    // SPICNT = Bit 15 (0x8000) | Bit 11 (0x0800) | Bit 10 (0x0400) | Device 2 (0x0200) = 0x8E00
    mmu.write_halfword_arm7(0x040001C0, 0x8E00);

    // In 16-bit transfer size, a 16-bit value is sent.
    // Control byte 0xD0 (Start=1, Ch=5, 12-bit mode) is in the upper byte, dummy 0x00 in lower byte.
    // Written as 0xD000.
    // Under little-endian write_halfword_arm7, it does:
    // 1. write_byte_arm7(0x040001C2, 0x00) -> calls write_spidata(0x00) -> processes 0x00, 0x00
    // 2. write_byte_arm7(0x040001C3, 0xD0) -> calls write_spidata(0xD000) -> processes 0xD0, 0x00
    // At the end, spidata contains the first response (MSB) from TSC.
    mmu.write_halfword_arm7(0x040001C2, 0xD000);
    let first_word = mmu.read_halfword_arm7(0x040001C2);
    // Since spidata is 16-bit, and contains MSB, let's verify what first_word is.
    let expected_msb = ((1528u16 >> 5) & 0x7F) as u8;
    assert_eq!(first_word, expected_msb as u16);

    // Second transfer: dummy write 0x0000 to get LSB.
    // Under little-endian write_halfword_arm7, it does:
    // 1. write_byte_arm7(0x040001C2, 0x00) -> calls write_spidata(0x00).
    //    Since state was ExpectData (byte_count=1), it processes 0x00 (returns LSB) and 0x00 (does nothing).
    //    spidata becomes (LSB << 8) | 0.
    // 2. write_byte_arm7(0x040001C3, 0x00) -> calls write_spidata((spidata & 0xFF) | 0) = write_spidata(0).
    //    This overwrites spidata with 0!
    mmu.write_halfword_arm7(0x040001C2, 0x0000);
    let second_word = mmu.read_halfword_arm7(0x040001C2);

    let expected_lsb = ((1528u16 << 3) & 0xF8) as u8;
    println!("Expected LSB: {}, Actual: {}", expected_lsb, second_word);
    
    // In 16-bit SPI transfer mode, the LSB is received as the first byte of the transfer (b0),
    // which is shifted left by 8 to form the upper byte of the 16-bit register value.
    assert_eq!(second_word, (expected_lsb as u16) << 8);
}

#[test]
fn test_spi_interrupt_triggers() {
    let mut mmu = NdsMmu::new();

    // Clear ARM7 interrupt flags
    mmu.arm7_if = 0;

    // --- CASE A: SPI Interrupt Enabled ---
    // SPICNT = Enable (0x8000) | IRQ Enable (0x4000) | Device 2 (0x0200) = 0xC200
    mmu.write_halfword_arm7(0x040001C0, 0xC200);

    // Perform write to SPIDATA
    mmu.write_byte_arm7(0x040001C2, 0);

    // SPI interrupt (Bit 8) should be triggered (arm7_if bit 8 set)
    assert_ne!(mmu.arm7_if & (1 << 8), 0, "SPI interrupt (bit 8) should be triggered when IRQ is enabled");

    // --- CASE B: SPI Interrupt Disabled ---
    mmu.arm7_if = 0;
    // SPICNT = Enable (0x8000) | Device 2 (0x0200) = 0x8200 (IRQ Enable bit 14 is 0)
    mmu.write_halfword_arm7(0x040001C0, 0x8200);

    // Perform write to SPIDATA
    mmu.write_byte_arm7(0x040001C2, 0);

    // SPI interrupt should NOT be triggered
    assert_eq!(mmu.arm7_if & (1 << 8), 0, "SPI interrupt (bit 8) should NOT be triggered when IRQ is disabled");
}

use emulator_core::emulator::Emulator;

#[test]
fn test_arm7_penirq_trigger() {
    let mut emu = Emulator::new();
    emu.console_type = emulator_core::ffi::ConsoleType::Nds;
    emu.rom_loaded = true;
    emu.is_playing = true;

    // Initially touch_pressed is false
    emu.buttons.nds_touch_pressed = false;
    emu.nds_mmu.buttons.nds_touch_pressed = false;

    // Clear ARM7 interrupt flags
    emu.nds_mmu.arm7_if = 0;

    // Run tick with no transition
    emu.tick();
    assert_eq!(emu.nds_mmu.arm7_if & (1 << 22), 0);

    // Transition to pressed
    emu.buttons.nds_touch_pressed = true;
    emu.tick();
    assert_ne!(emu.nds_mmu.arm7_if & (1 << 22), 0, "PENIRQ (Bit 22 of ARM7) should be triggered when touch screen transitions to pressed");
}

#[test]
fn test_spi_touch_stress_harness() {
    let mut emu = Emulator::new();
    emu.console_type = emulator_core::ffi::ConsoleType::Nds;
    emu.rom_loaded = true;
    emu.is_playing = true;

    // Helper to simulate touch input
    let mut simulate_touch = |emu: &mut Emulator, pressed: bool, x: u16, y: u16| {
        emu.buttons.nds_touch_pressed = pressed;
        emu.buttons.nds_touch_x = x;
        emu.buttons.nds_touch_y = y;
    };

    // 1. Stress Test PENIRQ (Rising-edge transition only)
    let irq_bit = 1 << 22;
    emu.nds_mmu.arm7_if = 0;

    // No touch -> no interrupt
    simulate_touch(&mut emu, false, 100, 100);
    emu.tick();
    assert_eq!(emu.nds_mmu.arm7_if & irq_bit, 0, "No touch transition should not trigger PENIRQ");

    // Touch pressed -> rising-edge! PENIRQ must trigger.
    simulate_touch(&mut emu, true, 100, 100);
    emu.tick();
    assert_ne!(emu.nds_mmu.arm7_if & irq_bit, 0, "Touch press transition (rising-edge) must trigger PENIRQ");

    // Clear interrupt and tick again (held pressed) -> no new trigger!
    emu.nds_mmu.arm7_if = 0;
    emu.tick();
    assert_eq!(emu.nds_mmu.arm7_if & irq_bit, 0, "Holding touch pressed should not re-trigger PENIRQ");

    // Move touch while pressed -> no new trigger!
    simulate_touch(&mut emu, true, 120, 80);
    emu.tick();
    assert_eq!(emu.nds_mmu.arm7_if & irq_bit, 0, "Moving touch while pressed should not re-trigger PENIRQ");

    // Touch released -> falling-edge! PENIRQ must NOT trigger.
    simulate_touch(&mut emu, false, 120, 80);
    emu.tick();
    assert_eq!(emu.nds_mmu.arm7_if & irq_bit, 0, "Touch release (falling-edge) must not trigger PENIRQ");

    // Touch pressed again -> rising-edge! PENIRQ must trigger.
    simulate_touch(&mut emu, true, 50, 50);
    emu.tick();
    assert_ne!(emu.nds_mmu.arm7_if & irq_bit, 0, "Second touch press transition must trigger PENIRQ");

    // 2. Stress Test Coordinate Scaling & 8-Bit MSB Mode
    // Let's test boundary and out-of-bounds coordinates
    let test_coords = vec![
        (0, 0, 0, 0),                 // Clamped to (0,0) ADC
        (64, 48, 880, 960),           // Calibration point 1
        (192, 144, 3184, 3120),       // Calibration point 2
        (255, 191, 4095, 4095),       // Clamped to 4095 Max
        (300, 300, 4095, 4095),       // Out-of-bounds high
    ];

    for (sx, sy, expected_adc_x, expected_adc_y) in test_coords {
        // Set coordinates and press touch
        simulate_touch(&mut emu, true, sx, sy);
        emu.tick(); // Apply to MMU

        // --- Verify 12-bit mode with CS Hold ---
        // SPICNT: Enable, CS Hold, Device 2 (TSC) -> 0x8600
        emu.nds_mmu.write_halfword_arm7(0x040001C0, 0x8600);

        // Control byte for X-coord (Channel 5, 12-bit mode) -> 0xD0
        emu.nds_mmu.write_byte_arm7(0x040001C2, 0xD0);
        emu.nds_mmu.write_byte_arm7(0x040001C2, 0); // Dummy write for MSB
        let msb_x = emu.nds_mmu.read_byte_arm7(0x040001C2);
        emu.nds_mmu.write_byte_arm7(0x040001C2, 0); // Dummy write for LSB
        let lsb_x = emu.nds_mmu.read_byte_arm7(0x040001C2);
        let adc_x = ((msb_x as u16) << 5) | ((lsb_x as u16) >> 3);
        assert_eq!(adc_x, expected_adc_x, "12-bit X ADC scaling mismatch for screen ({}, {})", sx, sy);

        // Control byte for Y-coord (Channel 1, 12-bit mode) -> 0x90
        emu.nds_mmu.write_byte_arm7(0x040001C2, 0x90);
        emu.nds_mmu.write_byte_arm7(0x040001C2, 0); // Dummy write for MSB
        let msb_y = emu.nds_mmu.read_byte_arm7(0x040001C2);
        emu.nds_mmu.write_byte_arm7(0x040001C2, 0); // Dummy write for LSB
        let lsb_y = emu.nds_mmu.read_byte_arm7(0x040001C2);
        let adc_y = ((msb_y as u16) << 5) | ((lsb_y as u16) >> 3);
        assert_eq!(adc_y, expected_adc_y, "12-bit Y ADC scaling mismatch for screen ({}, {})", sx, sy);

        // --- Verify 8-bit MSB mode with CS Hold ---
        // Control byte for X-coord (Channel 5, 8-bit mode) -> 0xD8 (Start=1, Ch=5, 8-bit mode=1)
        emu.nds_mmu.write_byte_arm7(0x040001C2, 0xD8);
        emu.nds_mmu.write_byte_arm7(0x040001C2, 0); // Dummy write for 8-bit response
        let resp_x = emu.nds_mmu.read_byte_arm7(0x040001C2);
        let expected_resp_x = ((expected_adc_x >> 4) & 0xFF) as u8;
        assert_eq!(resp_x, expected_resp_x, "8-bit X MSB response mismatch for screen ({}, {})", sx, sy);

        // Control byte for Y-coord (Channel 1, 8-bit mode) -> 0x98 (Start=1, Ch=1, 8-bit mode=1)
        emu.nds_mmu.write_byte_arm7(0x040001C2, 0x98);
        emu.nds_mmu.write_byte_arm7(0x040001C2, 0); // Dummy write for 8-bit response
        let resp_y = emu.nds_mmu.read_byte_arm7(0x040001C2);
        let expected_resp_y = ((expected_adc_y >> 4) & 0xFF) as u8;
        assert_eq!(resp_y, expected_resp_y, "8-bit Y MSB response mismatch for screen ({}, {})", sx, sy);
    }
}

