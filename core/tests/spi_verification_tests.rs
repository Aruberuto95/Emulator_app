// core/tests/spi_verification_tests.rs

use emulator_core::nds::mmu::NdsMmu;

#[test]
fn test_spi_continuous_cs_hold_tsc() {
    let mut mmu = NdsMmu::new();
    mmu.spi.tsc.touch_x = 100;
    mmu.spi.tsc.touch_y = 120;
    mmu.spi.tsc.touch_pressed = true;

    // --- CASE A: CS Hold Enabled ---
    // Enable SPI, chipselect hold, device 2 (TSC), 8-bit transfers.
    // SPICNT = enable (0x8000) | CS hold (bit 11 = 0x0800) | device 2 (0x0200).
    // This used to say 0x8A00, which sets bit 10 (transfer size) and leaves the
    // hold bit clear — so every byte was its own transaction, the TSC state
    // machine reset between the control byte and the dummy reads, and every
    // expectation in this file read back 0.
    mmu.write_halfword_arm7(0x040001C0, 0x8A00);

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
    assert_eq!(raw_x, 100 * 15 + 128); // 15 ADC counts/px, 128 offset

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

    // Enable SPI, chipselect hold (bit 11), device 0 (PMIC), 8-bit transfers.
    mmu.write_halfword_arm7(0x040001C0, 0x8800);

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

/// SPICNT bit 10 ("16-bit transfer size") must change nothing.
///
/// It is non-functional on retail hardware: devices clock one byte per SPIDATA
/// write either way. SoulSilver's ARM7 flash driver *sets* it, and honouring it
/// as a real 16-bit transfer bypassed the device state machines and returned
/// 0xFFFF — so the flash STATUS byte that gates the ARM9's sound-init handshake
/// was never produced and boot stalled before any graphics setup.
#[test]
fn test_spi_transfer_size_bit_is_ignored() {
    let read_adc = |size_bit: u16| -> u16 {
        let mut mmu = NdsMmu::new();
        mmu.spi.tsc.touch_x = 100;
        mmu.spi.tsc.touch_y = 120;
        mmu.spi.tsc.touch_pressed = true;
        // Enable | chipselect hold | device 2, with the size bit under test.
        mmu.write_halfword_arm7(0x040001C0, 0x8A00 | size_bit);

        // One byte per write, exactly as the driver does it: control byte for
        // channel 5 (X) in 12-bit mode, then two dummy bytes for MSB and LSB.
        mmu.write_byte_arm7(0x040001C2, 0xD0);
        assert_eq!(mmu.read_byte_arm7(0x040001C2), 0, "the control byte itself returns 0");
        mmu.write_byte_arm7(0x040001C2, 0);
        let msb = mmu.read_byte_arm7(0x040001C2) as u16;
        mmu.write_byte_arm7(0x040001C2, 0);
        let lsb = mmu.read_byte_arm7(0x040001C2) as u16;
        (msb << 5) | (lsb >> 3)
    };

    let with_8bit = read_adc(0x0000);
    let with_16bit = read_adc(0x0400);
    assert_eq!(with_8bit, 100 * 15 + 128, "15 ADC counts/px, 128 offset");
    assert_eq!(with_16bit, with_8bit, "the size bit must not change the protocol");
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

    // GBATEK assigns ARM7 SPI to IF bit 23; bit 8 belongs to DMA0.
    // Source: https://problemkaputt.de/gbatek.htm#dsinterrupts
    assert_ne!(mmu.arm7_if & (1 << 23), 0, "SPI interrupt (bit 23) should be triggered when IRQ is enabled");
    assert_eq!(mmu.arm7_if & (1 << 8), 0, "SPI completion must not request DMA0 IRQ");

    // --- CASE B: SPI Interrupt Disabled ---
    mmu.arm7_if = 0;
    // SPICNT = Enable (0x8000) | Device 2 (0x0200) = 0x8200 (IRQ Enable bit 14 is 0)
    mmu.write_halfword_arm7(0x040001C0, 0x8200);

    // Perform write to SPIDATA
    mmu.write_byte_arm7(0x040001C2, 0);

    // SPI interrupt should NOT be triggered
    assert_eq!(mmu.arm7_if & (1 << 23), 0, "SPI interrupt (bit 23) should NOT be triggered when IRQ is disabled");
    assert_eq!(mmu.arm7_if & (1 << 8), 0, "SPI completion must not request DMA0 IRQ");
}

use emulator_core::emulator::Emulator;

/// The stylus must reach the ARM7 through EXTKEYIN, and must latch **no**
/// interrupt.
///
/// This test previously asserted the opposite — that a pen-down edge raises
/// ARM7 IF bit 22 ("PENIRQ"). Bit 22 is the *hinge* (lid-open) line; latching it
/// on a touch faked a wake-from-sleep event, and SoulSilver's driver polls the
/// TSC every frame rather than taking an interrupt. The bogus latch was removed
/// from the emulator, so the expectation is inverted here to match the hardware
/// the driver was measured against.
#[test]
fn test_arm7_touch_publishes_extkeyin_and_latches_no_irq() {
    let mut emu = Emulator::new();
    emu.play();

    let touch = |emu: &mut Emulator, pressed: bool, x: u16, y: u16| {
        let mut b = emu.get_button_state();
        b.nds_touch_pressed = pressed;
        b.nds_touch_x = x;
        b.nds_touch_y = y;
        emu.inject_input(b);
        // The per-tick sync the NDS branch of `tick` performs, called directly so
        // the test needs no cartridge.
        emu.poll_nds_touch_penirq();
    };

    emu.nds_mmu.arm7_if = 0;
    touch(&mut emu, false, 100, 100);
    assert_eq!(emu.nds_mmu.arm7_if, 0, "pen-up must latch nothing");
    assert_ne!(emu.nds_mmu.get_extkeyin() & 0x40, 0, "pen-up leaves EXTKEYIN bit 6 high");

    touch(&mut emu, true, 100, 100);
    assert_eq!(
        emu.nds_mmu.arm7_if, 0,
        "a pen-down edge must latch no ARM7 IRQ (bit 22 is the lid line)"
    );
    assert_eq!(emu.nds_mmu.get_extkeyin() & 0x40, 0, "pen-down drives EXTKEYIN bit 6 low");
    assert!(emu.nds_mmu.spi.tsc.touch_pressed);

    touch(&mut emu, false, 100, 100);
    assert_eq!(emu.nds_mmu.arm7_if, 0, "release must latch nothing either");
    assert!(!emu.nds_mmu.spi.tsc.touch_pressed);
}

#[test]
fn test_spi_touch_stress_harness() {
    let mut emu = Emulator::new();
    emu.play();

    // Helper to simulate touch input through the public API and run the same
    // per-tick sync the NDS branch of `tick` does.
    let simulate_touch = |emu: &mut Emulator, pressed: bool, x: u16, y: u16| {
        let mut b = emu.get_button_state();
        b.nds_touch_pressed = pressed;
        b.nds_touch_x = x;
        b.nds_touch_y = y;
        emu.inject_input(b);
        emu.poll_nds_touch_penirq();
    };

    // Stress Test Coordinate Scaling & 8-Bit MSB Mode
    // Let's test boundary and out-of-bounds coordinates
    // adc_x = x * 15 + 128, adc_y = y * 20 + 128 — the exact inverse of the
    // calibration pair `hle::build_user_settings` publishes. The whole panel
    // fits inside the 12-bit converter, so NOTHING on screen clamps. This table
    // previously asserted (0,0)->(0,0) and (255,191)->(4095,4095), i.e. it
    // certified the saturation that made the screen edges unreachable.
    let test_coords = vec![
        (0, 0, 128, 128),             // Left/top edge: distinct, not folded to 0
        (32, 24, 608, 608),           // Calibration point 1
        (224, 168, 3488, 3488),       // Calibration point 2
        (255, 191, 3953, 3948),       // Bottom-right: distinct, not folded to 4095
        (300, 300, 4095, 4095),       // Genuinely out of bounds: clamps
    ];

    for (sx, sy, expected_adc_x, expected_adc_y) in test_coords {
        // Set coordinates and press touch (publishes them to the SPI TSC)
        simulate_touch(&mut emu, true, sx, sy);

        // --- Verify 12-bit mode with CS Hold ---
        // SPICNT: enable | chipselect hold (bit 11) | device 2 (TSC), 8-bit
        emu.nds_mmu.write_halfword_arm7(0x040001C0, 0x8A00);

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

