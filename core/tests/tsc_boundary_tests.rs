// core/tests/tsc_boundary_tests.rs

use emulator_core::nds::mmu::NdsMmu;
use emulator_core::emulator::Emulator;

fn read_tsc_coordinate(mmu: &mut NdsMmu, channel: u8, is_8bit: bool) -> u16 {
    // Enable SPI, CS Hold, Device 2 (TSC), 8-bit transfer size (SPIDATA uses 8-bit interface here)
    // SPICNT = enable (0x8000) | chipselect hold (0x0800) | device 2 (0x0200).
    mmu.write_halfword_arm7(0x040001C0, 0x8A00);

    // Control Byte: Start=1, Channel=channel, 8-bit/12-bit mode
    let start_bit = 0x80;
    let channel_bits = (channel & 0x7) << 4;
    let mode_bit = if is_8bit { 0x08 } else { 0x00 };
    let control_byte = start_bit | channel_bits | mode_bit;

    mmu.write_byte_arm7(0x040001C2, control_byte);
    let _ = mmu.read_byte_arm7(0x040001C2); // Dummy read

    let result = if is_8bit {
        // Read 8-bit response (1 dummy byte write)
        mmu.write_byte_arm7(0x040001C2, 0);
        let resp = mmu.read_byte_arm7(0x040001C2);
        resp as u16
    } else {
        // Read 12-bit response (2 dummy byte writes)
        mmu.write_byte_arm7(0x040001C2, 0);
        let msb = mmu.read_byte_arm7(0x040001C2);

        mmu.write_byte_arm7(0x040001C2, 0);
        let lsb = mmu.read_byte_arm7(0x040001C2);

        ((msb as u16) << 5) | ((lsb as u16) >> 3)
    };

    // Release CS Hold (Disable CS Hold bit and clear SPI enable or just disable CS hold)
    mmu.write_halfword_arm7(0x040001C0, 0x8000);
    mmu.write_byte_arm7(0x040001C2, 0); // resets TSC state machine

    result
}

/// Pen-up must produce the panel's *rail signature*, not clean zeroes.
///
/// With the panel open, the PENIRQ pull-up drags the Y input (and Z2) to the
/// rail — they read 0xFFF — while X and Z1 float low. The SDK driver keeps
/// sampling for ~12 frames after release and keys its pen-up detection on
/// exactly that pattern. Returning 0 for Y instead made those trailing samples
/// decode as a valid touch at the screen corner, so a stroke appeared to slide
/// off the button and UI buttons (which fire on release-inside-button) cancelled
/// instead of firing. This test used to assert the zeroes that caused it.
#[test]
fn test_tsc_pen_up_returns_rail_signature() {
    let mut mmu = NdsMmu::new();
    mmu.spi.tsc.touch_pressed = false;

    for x in &[0, 64, 100, 255, 1000, 65535] {
        for y in &[0, 48, 120, 191, 1000, 65535] {
            mmu.spi.tsc.touch_x = *x;
            mmu.spi.tsc.touch_y = *y;

            // X position and Z1 float low regardless of the last coordinates.
            assert_eq!(read_tsc_coordinate(&mut mmu, 5, false), 0, "X floats low on pen-up");
            assert_eq!(read_tsc_coordinate(&mut mmu, 5, true), 0);
            assert_eq!(read_tsc_coordinate(&mut mmu, 3, false), 0, "Z1 floats low on pen-up");

            // Y position and Z2 rail high.
            assert_eq!(read_tsc_coordinate(&mut mmu, 1, false), 0xFFF, "Y rails on pen-up");
            assert_eq!(read_tsc_coordinate(&mut mmu, 4, false), 0xFFF, "Z2 rails on pen-up");
        }
    }
}

#[test]
fn test_tsc_x_coordinate_boundaries() {
    let mut mmu = NdsMmu::new();
    mmu.spi.tsc.touch_pressed = true;

    // Test cases for touch_x coordinate
    // format: (touch_x, expected_12bit, expected_8bit)
    let test_cases = vec![
        // raw_x = touch_x * 15 + 128. The whole 256-pixel panel fits inside the
        // 12-bit converter (128..3953), so NO on-screen coordinate clamps --
        // that is the point of the constants. This table previously asserted
        // `(255, 4095, ..)` with the comment "raw_x = 4318, clamped to 4095",
        // i.e. it documented the saturation defect as intended behaviour: at 18
        // counts/px the transform needed 4608 counts and the top 13 columns all
        // decoded to the same x. Only genuinely off-screen inputs clamp now.
        (0, 128, (128 >> 4) & 0xFF),               // left edge, no longer folded onto 0
        (30, 578, (578 >> 4) & 0xFF),
        (63, 1073, (1073 >> 4) & 0xFF),
        (64, 1088, (1088 >> 4) & 0xFF),
        (65, 1103, (1103 >> 4) & 0xFF),
        (100, 1628, (1628 >> 4) & 0xFF),
        (255, 3953, (3953 >> 4) & 0xFF),           // Max NDS screen X -- distinct, not clamped
        (256, 3968, (3968 >> 4) & 0xFF),           // First off-screen column
        (1000, 4095, (4095 >> 4) & 0xFF),          // Way off-screen: clamps
        (65535, 4095, (4095 >> 4) & 0xFF),         // u16::MAX boundary: clamps
    ];

    for (touch_x, expected_12bit, expected_8bit) in test_cases {
        mmu.spi.tsc.touch_x = touch_x;
        
        // Test 12-bit mode
        let got_12bit = read_tsc_coordinate(&mut mmu, 5, false);
        assert_eq!(
            got_12bit, expected_12bit,
            "X coord 12-bit failed for touch_x = {}: expected {}, got {}",
            touch_x, expected_12bit, got_12bit
        );

        // Test 8-bit mode
        let got_8bit = read_tsc_coordinate(&mut mmu, 5, true);
        assert_eq!(
            got_8bit, expected_8bit,
            "X coord 8-bit failed for touch_x = {}: expected {}, got {}",
            touch_x, expected_8bit, got_8bit
        );
    }
}

#[test]
fn test_tsc_y_coordinate_boundaries() {
    let mut mmu = NdsMmu::new();
    mmu.spi.tsc.touch_pressed = true;

    // Test cases for touch_y coordinate
    // format: (touch_y, expected_12bit, expected_8bit)
    // raw_y = touch_y * 20 + 128. The whole 192-row panel fits inside the 12-bit
    // converter (128..3948), so no on-screen row clamps. The old table asserted
    // rows 0-5 all reading 0 and row 191 reading 4095 ("raw_y = 4177, clamped"),
    // i.e. it certified that 6 rows at the top and 4 at the bottom of the touch
    // screen were unreachable.
    let test_cases = vec![
        (0, 128, (128 >> 4) & 0xFF),               // Top edge: distinct, not folded to 0
        (5, 228, (228 >> 4) & 0xFF),
        (6, 248, (248 >> 4) & 0xFF),
        (47, 1068, (1068 >> 4) & 0xFF),
        (48, 1088, (1088 >> 4) & 0xFF),
        (49, 1108, (1108 >> 4) & 0xFF),
        (120, 2528, (2528 >> 4) & 0xFF),
        (187, 3868, (3868 >> 4) & 0xFF),
        (191, 3948, (3948 >> 4) & 0xFF),           // Max NDS screen Y: distinct
        (256, 4095, (4095 >> 4) & 0xFF),           // Off-screen: clamps
        (65535, 4095, (4095 >> 4) & 0xFF),         // u16::MAX boundary: clamps
    ];

    for (touch_y, expected_12bit, expected_8bit) in test_cases {
        mmu.spi.tsc.touch_y = touch_y;
        
        // Test 12-bit mode
        let got_12bit = read_tsc_coordinate(&mut mmu, 1, false);
        assert_eq!(
            got_12bit, expected_12bit,
            "Y coord 12-bit failed for touch_y = {}: expected {}, got {}",
            touch_y, expected_12bit, got_12bit
        );

        // Test 8-bit mode
        let got_8bit = read_tsc_coordinate(&mut mmu, 1, true);
        assert_eq!(
            got_8bit, expected_8bit,
            "Y coord 8-bit failed for touch_y = {}: expected {}, got {}",
            touch_y, expected_8bit, got_8bit
        );
    }
}

#[test]
fn test_tsc_pressure_channels() {
    let mut mmu = NdsMmu::new();
    
    // Z1 and Z2 should return constant values when pressed
    mmu.spi.tsc.touch_pressed = true;
    assert_eq!(read_tsc_coordinate(&mut mmu, 3, false), 100);
    assert_eq!(read_tsc_coordinate(&mut mmu, 4, false), 200);

    // Pen-up: Z1 floats low, Z2 rails high (see the pen-up signature test).
    mmu.spi.tsc.touch_pressed = false;
    assert_eq!(read_tsc_coordinate(&mut mmu, 3, false), 0);
    assert_eq!(read_tsc_coordinate(&mut mmu, 4, false), 0xFFF);
}

#[test]
fn test_tsc_invalid_channels() {
    let mut mmu = NdsMmu::new();
    mmu.spi.tsc.touch_pressed = true;

    // Channels other than 1, 3, 4, 5 should return 0
    for ch in &[0, 2, 6, 7] {
        assert_eq!(read_tsc_coordinate(&mut mmu, *ch, false), 0);
    }
}

#[test]
fn test_emulator_tsc_coordinates_stress() {
    let mut emu = Emulator::new();
    emu.play();

    // Stress with different boundary input coordinates
    let test_coords = vec![
        (0, 0),
        (255, 191),
        (256, 192),
        (65535, 65535),
    ];

    for (x, y) in test_coords {
        // Public API only: this test lives outside the crate, so it drives the
        // emulator the way the frontend does instead of writing private fields
        // (which is what stopped this whole target from compiling).
        let mut buttons = emu.get_button_state();
        buttons.nds_touch_x = x;
        buttons.nds_touch_y = y;
        buttons.nds_touch_pressed = true;
        emu.inject_input(buttons);

        // The per-tick touch sync is what publishes the stylus to the SPI TSC and
        // EXTKEYIN. It must not panic or clamp-overflow on out-of-range coords,
        // and neither must a full tick afterwards.
        emu.poll_nds_touch_penirq();
        emu.tick();

        // Verify that the values were properly propagated to the MMU's TSC
        assert_eq!(emu.nds_mmu.spi.tsc.touch_x, x);
        assert_eq!(emu.nds_mmu.spi.tsc.touch_y, y);
        assert_eq!(emu.nds_mmu.spi.tsc.touch_pressed, true);
    }
}
