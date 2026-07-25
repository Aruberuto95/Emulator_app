// core/tests/tsc_boundary_tests.rs

use emulator_core::nds::mmu::NdsMmu;
use emulator_core::emulator::Emulator;

fn read_tsc_coordinate(mmu: &mut NdsMmu, channel: u8, is_8bit: bool) -> u16 {
    // Enable SPI, CS Hold, Device 2 (TSC), 8-bit transfer size (SPIDATA uses 8-bit interface here)
    // SPICNT = Bit 15 (0x8000) | Bit 10 (0x0400) | Device 2 (0x0200) = 0x8600
    mmu.write_halfword_arm7(0x040001C0, 0x8600);

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

// 1. Stress coordinates at limits, empty inputs, max values, and check for any panics or wrap-arounds.
#[test]
fn test_tsc_touch_not_pressed() {
    let mut mmu = NdsMmu::new();
    mmu.spi.tsc.touch_pressed = false;

    // Test multiple coordinates while not pressed. They should all return 0.
    for x in &[0, 64, 100, 255, 1000, 65535] {
        for y in &[0, 48, 120, 191, 1000, 65535] {
            mmu.spi.tsc.touch_x = *x;
            mmu.spi.tsc.touch_y = *y;

            // X-coordinate channel (5)
            assert_eq!(read_tsc_coordinate(&mut mmu, 5, false), 0);
            assert_eq!(read_tsc_coordinate(&mut mmu, 5, true), 0);

            // Y-coordinate channel (1)
            assert_eq!(read_tsc_coordinate(&mut mmu, 1, false), 0);
            assert_eq!(read_tsc_coordinate(&mut mmu, 1, true), 0);

            // Z1 channel (3)
            assert_eq!(read_tsc_coordinate(&mut mmu, 3, false), 0);
            // Z2 channel (4)
            assert_eq!(read_tsc_coordinate(&mut mmu, 4, false), 0);
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
        // Below offset threshold (64)
        (0, 0, 0),                                 // Clamped to 0 (raw_x = -1152)
        (30, 0, 0),                                // Clamped to 0 (raw_x = -612)
        (63, 862, (862 >> 4) & 0xFF),              // Just below threshold (raw_x = 862)
        // Offset threshold
        (64, 880, (880 >> 4) & 0xFF),              // Exactly threshold (raw_x = 880)
        // Above offset threshold
        (65, 898, (898 >> 4) & 0xFF),              // Just above threshold (raw_x = 898)
        (100, 1528, (1528 >> 4) & 0xFF),           // Regular coordinate (raw_x = 1528)
        (255, 4095, (4095 >> 4) & 0xFF),           // Max NDS screen X coord (raw_x = 4318, clamped to 4095)
        (256, 4095, (4095 >> 4) & 0xFF),           // Above NDS screen X (clamped to 4095)
        (1000, 4095, (4095 >> 4) & 0xFF),          // Way above NDS screen (clamped to 4095)
        (65535, 4095, (4095 >> 4) & 0xFF),         // u16::MAX boundary (clamped to 4095)
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
    let test_cases = vec![
        // Below offset threshold (48)
        (0, 0, 0),                                 // Clamped to 0 (raw_y = -120)
        (5, 0, 0),                                 // Clamped to 0 (raw_y = -7)
        (6, 15, (15 >> 4) & 0xFF),                 // First positive value (raw_y = 15)
        (47, 938, (938 >> 4) & 0xFF),              // Just below threshold (raw_y = 938)
        // Offset threshold
        (48, 960, (960 >> 4) & 0xFF),              // Exactly threshold (raw_y = 960)
        // Above offset threshold
        (49, 982, (982 >> 4) & 0xFF),              // Just above threshold (raw_y = 982)
        (120, 2580, (2580 >> 4) & 0xFF),           // Regular coordinate (raw_y = 2580)
        (187, 4087, (4087 >> 4) & 0xFF),           // Under maximum (raw_y = 4087)
        (191, 4095, (4095 >> 4) & 0xFF),           // Max NDS screen Y (raw_y = 4177, clamped to 4095)
        (256, 4095, (4095 >> 4) & 0xFF),           // Above NDS screen Y (clamped to 4095)
        (65535, 4095, (4095 >> 4) & 0xFF),         // u16::MAX boundary (clamped to 4095)
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

    // Should return 0 when not pressed
    mmu.spi.tsc.touch_pressed = false;
    assert_eq!(read_tsc_coordinate(&mut mmu, 3, false), 0);
    assert_eq!(read_tsc_coordinate(&mut mmu, 4, false), 0);
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
    emu.console_type = emulator_core::ffi::ConsoleType::Nds;
    emu.rom_loaded = true;
    emu.is_playing = true;

    // Stress with different boundary input coordinates
    let test_coords = vec![
        (0, 0),
        (255, 191),
        (256, 192),
        (65535, 65535),
    ];

    for (x, y) in test_coords {
        emu.buttons.nds_touch_x = x;
        emu.buttons.nds_touch_y = y;
        emu.buttons.nds_touch_pressed = true;

        // Tick emulator. This should run NDS-level tick logic
        // where it sets SPI values, checks limits, and draws a stylus dot if it is within bounds.
        // It must NOT panic or cause memory corruption/overflow.
        emu.tick();

        // Verify that the values were properly propagated to the MMU's TSC
        assert_eq!(emu.nds_mmu.spi.tsc.touch_x, x);
        assert_eq!(emu.nds_mmu.spi.tsc.touch_y, y);
        assert_eq!(emu.nds_mmu.spi.tsc.touch_pressed, true);
    }
}
