// core/tests/ppu_stress_tests.rs

use emulator_core::nds::ppu::NdsPpu;
use emulator_core::nds::mmu::NdsMmu;

fn bgr555(r: u8, g: u8, b: u8) -> u16 {
    (((b as u16) >> 3) << 10) | (((g as u16) >> 3) << 5) | ((r as u16) >> 3)
}


#[test]
fn test_ppu_cycle_accumulation_and_drift() {
    let mut ppu = NdsPpu::new();
    let mut mmu = NdsMmu::new();
    let mut video_buffer = vec![0u16; 256 * 384];

    // Initialize VCOUNT to 0
    mmu.set_vcount(0);

    // 1. Tick 511 cycles. Accumulator should be 511. HBlank should NOT be set.
    ppu.tick(511, &mut mmu, &mut video_buffer, false);
    assert_eq!(ppu.cycle_accumulator, 511);
    let dispstat_arm9 = ((mmu.arm9_io[5] as u16) << 8) | (mmu.arm9_io[4] as u16);
    assert_eq!(dispstat_arm9 & (1 << 1), 0, "HBlank should not be set at 511 cycles");

    // 2. Tick 1 more cycle (total 512). HBlank should be set (bit 1).
    ppu.tick(1, &mut mmu, &mut video_buffer, false);
    assert_eq!(ppu.cycle_accumulator, 512);
    let dispstat_arm9 = ((mmu.arm9_io[5] as u16) << 8) | (mmu.arm9_io[4] as u16);
    assert_ne!(dispstat_arm9 & (1 << 1), 0, "HBlank should be set at 512 cycles");

    // 3. Tick up to 709 cycles (total 709). Accumulator is 709. Scanline should still be 0.
    ppu.tick(197, &mut mmu, &mut video_buffer, false);
    assert_eq!(ppu.cycle_accumulator, 709);
    let vcount = ((mmu.arm9_io[7] as u16) << 8) | (mmu.arm9_io[6] as u16);
    assert_eq!(vcount, 0, "Scanline should still be 0 at 709 cycles");

    // 4. Tick 1 more cycle (total 710). Scanline should increment to 1.
    // Let's observe the behavior of cycle accumulator.
    ppu.tick(1, &mut mmu, &mut video_buffer, false);
    let vcount = ((mmu.arm9_io[7] as u16) << 8) | (mmu.arm9_io[6] as u16);
    assert_eq!(vcount, 1, "Scanline should increment to 1 at 710 cycles");
    // Verify accumulator reset: does it reset to 0 or subtract 710?
    // In our implementation inspection, we noticed:
    // `if self.cycle_accumulator >= 710 { self.cycle_accumulator = 0; ... }`
    // So it resets to 0. Let's verify this behavior.
    assert_eq!(ppu.cycle_accumulator, 0, "Cycle accumulator was reset to 0");

    // 5. Let's test cycle truncation / drift.
    // If we tick with 715 cycles, 5 cycles should carry over, but in current code they are lost.
    ppu.reset();
    mmu.reset();
    mmu.set_vcount(0);
    ppu.tick(715, &mut mmu, &mut video_buffer, false);
    let vcount = ((mmu.arm9_io[7] as u16) << 8) | (mmu.arm9_io[6] as u16);
    assert_eq!(vcount, 1, "VCount should be 1 after 715 cycles");
    assert_eq!(ppu.cycle_accumulator, 5, "Cycle accumulator should carry over 5 cycles");

    // 6. Test tick with a large cycle count, e.g., 2132 (which is exactly 3 scanlines of 710 cycles).
    ppu.reset();
    mmu.reset();
    mmu.set_vcount(0);
    ppu.tick(2132, &mut mmu, &mut video_buffer, false);
    let vcount = ((mmu.arm9_io[7] as u16) << 8) | (mmu.arm9_io[6] as u16);
    // If accumulator carried over, vcount should be 3.
    // If accumulator was discarded, vcount will only be 1.
    println!("With 2132 cycles, vcount is {}, accumulator is {}", vcount, ppu.cycle_accumulator);
    assert_eq!(vcount, 3, "VCount should be 3 after 2132 cycles");
    assert_eq!(ppu.cycle_accumulator, 2, "Accumulator should carry over 2 cycles");
}

#[test]
fn test_vblank_transition_and_wrap_around() {
    let mut ppu = NdsPpu::new();
    let mut mmu = NdsMmu::new();
    let mut video_buffer = vec![0u16; 256 * 384];

    // Set vcount to 191 (just before VBlank)
    mmu.set_vcount(191);

    // Tick 710 cycles to advance to line 192 (VBlank start)
    ppu.tick(710, &mut mmu, &mut video_buffer, false);
    let vcount = ((mmu.arm9_io[7] as u16) << 8) | (mmu.arm9_io[6] as u16);
    assert_eq!(vcount, 192, "VCount should be 192");
    
    // Check VBlank flag in DISPSTAT (bit 0)
    let dispstat = ((mmu.arm9_io[5] as u16) << 8) | (mmu.arm9_io[4] as u16);
    assert_ne!(dispstat & (1 << 0), 0, "VBlank flag should be set at line 192");

    // Tick up to line 262. VBlank flag should remain set.
    for line in 193..=262 {
        ppu.tick(710, &mut mmu, &mut video_buffer, false);
        let vcount = ((mmu.arm9_io[7] as u16) << 8) | (mmu.arm9_io[6] as u16);
        assert_eq!(vcount, line, "VCount should match current scanline");
        let dispstat = ((mmu.arm9_io[5] as u16) << 8) | (mmu.arm9_io[4] as u16);
        assert_ne!(dispstat & (1 << 0), 0, "VBlank flag should remain set at line {}", line);
    }

    // Tick 710 more cycles to wrap around to line 0
    ppu.tick(710, &mut mmu, &mut video_buffer, false);
    let vcount = ((mmu.arm9_io[7] as u16) << 8) | (mmu.arm9_io[6] as u16);
    assert_eq!(vcount, 0, "VCount should wrap around to 0");
    let dispstat = ((mmu.arm9_io[5] as u16) << 8) | (mmu.arm9_io[4] as u16);
    assert_eq!(dispstat & (1 << 0), 0, "VBlank flag should be cleared at line 0");
}

#[test]
fn test_interrupt_rising_edge_and_enabling_bit() {
    let mut ppu = NdsPpu::new();
    let mut mmu = NdsMmu::new();
    let mut video_buffer = vec![0u16; 256 * 384];

    // Test 1: HBlank Interrupt not enabled in DISPSTAT (bit 4 of DISPSTAT is 0)
    mmu.set_vcount(0);
    // Write DISPSTAT (0x04000004) for ARM9 as 0 (HBlank IRQ disabled)
    mmu.arm9_io[4] = 0;
    mmu.arm9_io[5] = 0;
    mmu.arm9_if = 0; // Clear interrupt flags

    // Tick to 512 to trigger HBlank start
    ppu.tick(512, &mut mmu, &mut video_buffer, false);
    // Check if HBlank IRQ (bit 1 of arm9_if) was triggered
    assert_eq!(mmu.arm9_if & (1 << 1), 0, "HBlank IRQ should NOT be triggered if disabled in DISPSTAT");

    // Test 2: HBlank Interrupt enabled in DISPSTAT (bit 4 of DISPSTAT is 1)
    ppu.reset();
    mmu.reset();
    mmu.set_vcount(0);
    // Enable HBlank IRQ in DISPSTAT (bit 4)
    mmu.arm9_io[4] = 1 << 4;
    mmu.arm9_io[5] = 0;
    mmu.arm9_if = 0;

    ppu.tick(512, &mut mmu, &mut video_buffer, false);
    assert_ne!(mmu.arm9_if & (1 << 1), 0, "HBlank IRQ should be triggered when enabled in DISPSTAT");

    // Test 3: Edge trigger check. HBlank is already set. Ticking more should NOT trigger another interrupt.
    mmu.arm9_if = 0; // Clear interrupt flags
    ppu.tick(10, &mut mmu, &mut video_buffer, false);
    assert_eq!(mmu.arm9_if & (1 << 1), 0, "HBlank IRQ should not be triggered again if already in HBlank");

    // Test 4: VBlank Interrupt not enabled (bit 3 of DISPSTAT is 0)
    ppu.reset();
    mmu.reset();
    mmu.set_vcount(191);
    mmu.arm9_io[4] = 0;
    mmu.arm9_if = 0;

    ppu.tick(710, &mut mmu, &mut video_buffer, false); // Transitions to 192 (VBlank start)
    assert_eq!(mmu.arm9_if & (1 << 0), 0, "VBlank IRQ should NOT be triggered if disabled in DISPSTAT");

    // Test 5: VBlank Interrupt enabled (bit 3 of DISPSTAT is 1)
    ppu.reset();
    mmu.reset();
    mmu.set_vcount(191);
    mmu.arm9_io[4] = 1 << 3;
    mmu.arm9_if = 0;

    ppu.tick(710, &mut mmu, &mut video_buffer, false); // Transitions to 192
    assert_ne!(mmu.arm9_if & (1 << 0), 0, "VBlank IRQ should be triggered when enabled in DISPSTAT");
}

#[test]
fn test_screen_swap_routing() {
    let ppu = NdsPpu::new();
    let mut mmu = NdsMmu::new();
    let mut video_buffer = vec![0u16; 256 * 384];

    // Let's set some distinct color for the screens
    // Engine A renders line_a (default is self.bgr555(10, 10, 50))
    // Engine B renders line_b (default is self.bgr555(30, 30, 30))
    let color_a = bgr555(10, 10, 50); // We'll verify what color it produces
    let color_b = (((30 as u16) >> 3) << 10) | (((30 as u16) >> 3) << 5) | ((30 as u16) >> 3); // BGR555(30,30,30) = 0x7BE

    // 1. Normal layout (swap = 0)
    // POWCNT (0x04000304) bit 15 is 0. So byte 0x305 bit 7 is 0.
    mmu.arm9_io[0x305] = 0;
    ppu.render_scanline(0, &mmu, &mut video_buffer);

    // Top screen (0..256) should contain color_a
    assert_eq!(video_buffer[0], bgr555(10, 10, 50));
    // Bottom screen (192*256..192*256+256) should contain color_b
    assert_eq!(video_buffer[192 * 256], color_b);

    // 2. Swapped layout (swap = 1)
    mmu.arm9_io[0x305] = 0x80; // set bit 7
    video_buffer.fill(0);
    ppu.render_scanline(0, &mmu, &mut video_buffer);

    // Top screen (0..256) should contain color_b
    assert_eq!(video_buffer[0], color_b);
    // Bottom screen (192*256..192*256+256) should contain color_a
    assert_eq!(video_buffer[192 * 256], bgr555(10, 10, 50));
}

#[test]
fn test_touch_coordinates_safety() {
    let ppu = NdsPpu::new();
    let mut mmu = NdsMmu::new();
    let mut video_buffer = vec![0u16; 256 * 384];

    // Enable touch
    mmu.buttons.nds_touch_pressed = true;

    // Test Case 1: touch within bounds (x=10, y=10)
    mmu.buttons.nds_touch_x = 10;
    mmu.buttons.nds_touch_y = 10;
    ppu.render_scanline(10, &mmu, &mut video_buffer);

    let yellow = bgr555(255, 255, 0);
    assert_eq!(video_buffer[(192 + 10) * 256 + 10], yellow, "Stylus dot should be drawn at (10, 10)");
    assert_eq!(video_buffer[(192 + 11) * 256 + 10], 0, "Stylus dot should not overwrite row 11 when rendering scanline 10");

    // Render scanline 11 and check that it draws the second row segment
    ppu.render_scanline(11, &mmu, &mut video_buffer);
    assert_eq!(video_buffer[(192 + 11) * 256 + 10], yellow, "Stylus dot should be drawn at (10, 11) when rendering scanline 11");

    // Test Case 2: touch x out of bounds (x=256, y=10) -> Should not draw or panic
    video_buffer.fill(0);
    mmu.buttons.nds_touch_x = 256;
    mmu.buttons.nds_touch_y = 10;
    ppu.render_scanline(10, &mmu, &mut video_buffer);
    assert_eq!(video_buffer[(192 + 10) * 256 + 10], 0);

    // Test Case 3: touch y out of bounds (x=10, y=192) -> Should not draw or panic
    video_buffer.fill(0);
    mmu.buttons.nds_touch_x = 10;
    mmu.buttons.nds_touch_y = 192;
    // render_scanline is called with ly = 10, but touch y = 192. So they don't match, nothing is drawn.
    ppu.render_scanline(10, &mmu, &mut video_buffer);
    assert_eq!(video_buffer[(192 + 10) * 256 + 10], 0);
}

#[test]
fn test_palette_ram_and_dispstat_write_masking() {
    let mut mmu = NdsMmu::new();

    // 1. Palette RAM should be 4096 bytes and mask should be 0xFFF
    mmu.write_byte_arm9(0x05000000, 0x11);
    mmu.write_byte_arm9(0x050007FF, 0x22);
    mmu.write_byte_arm9(0x05000FFF, 0x33);
    mmu.write_byte_arm9(0x05001000, 0x44); // wraps around to 0x05000000 due to 0xFFF mask

    assert_eq!(mmu.read_byte_arm9(0x05000000), 0x44);
    assert_eq!(mmu.read_byte_arm9(0x050007FF), 0x22);
    assert_eq!(mmu.read_byte_arm9(0x05000FFF), 0x33);
    assert_eq!(mmu.read_byte_arm9(0x05001000), 0x44);

    // 2. DISPSTAT write masking
    mmu.arm9_io[4] = 0x07; // set read-only bits (0, 1, 2)
    mmu.arm7_io[4] = 0x07;

    mmu.write_byte_arm9(0x04000204, 0xFF);
    mmu.write_byte_arm7(0x04000204, 0xFF);

    assert_eq!(mmu.arm9_io[4], 0xBF);
    assert_eq!(mmu.arm7_io[4], 0xBF);

    mmu.write_byte_arm9(0x04000204, 0x00);
    assert_eq!(mmu.arm9_io[4], 0x07);
}



#[test]
fn test_timing_carryover_detailed_stress() {
    let mut ppu = NdsPpu::new();
    let mut mmu = NdsMmu::new();
    let mut video_buffer = vec![0u16; 256 * 384];

    let read_vcount = |mmu: &NdsMmu| -> u16 {
        ((mmu.arm9_io[7] as u16) << 8) | (mmu.arm9_io[6] as u16)
    };

    // Verify prime cycle increments and timing accumulation
    mmu.reset();
    mmu.set_vcount(0);
    ppu.reset();

    // Tick 17 cycles 41 times (total 697 cycles)
    for _ in 0..41 {
        ppu.tick(17, &mut mmu, &mut video_buffer, false);
    }
    assert_eq!(ppu.cycle_accumulator, 697);
    assert_eq!(read_vcount(&mmu), 0);

    // Tick another 17 cycles (total 714 cycles) -> should cross 710 cycles boundary
    ppu.tick(17, &mut mmu, &mut video_buffer, false);
    assert_eq!(read_vcount(&mmu), 1);
    assert_eq!(ppu.cycle_accumulator, 4); // 714 - 710 = 4

    // Tick a massive number of cycles: 150000 cycles
    // 150000 / 710 = 211 scanlines.
    // 150000 % 710 = 190 cycles.
    let start_vcount = read_vcount(&mmu); // 1
    let start_accum = ppu.cycle_accumulator; // 4
    let total_avail = start_accum + 150000; // 150004
    let expected_lines = total_avail / 710; // 211
    let expected_accum = total_avail % 710; // 194

    ppu.tick(150000, &mut mmu, &mut video_buffer, false);
    let final_vcount = read_vcount(&mmu);
    let expected_vcount = (start_vcount + expected_lines as u16) % 263;

    assert_eq!(final_vcount, expected_vcount, "VCount advanced incorrectly over massive tick");
    assert_eq!(ppu.cycle_accumulator, expected_accum, "Accumulator carryover is incorrect");
}

#[test]
fn test_dispstat_write_masking_exhaustive() {
    let mut mmu = NdsMmu::new();

    // For all 256 possible bytes
    for write_val in 0..=255u8 {
        // Set an initial state for read-only/protected bits (0, 1, 2, 6)
        for initial_protected in [0x00u8, 0x07u8, 0x40u8, 0x47u8] {
            mmu.arm9_io[4] = initial_protected;
            mmu.arm7_io[4] = initial_protected;

            mmu.write_byte_arm9(0x04000204, write_val);
            mmu.write_byte_arm7(0x04000204, write_val);

            let expected = (initial_protected & 0x47) | (write_val & 0xB8);
            assert_eq!(
                mmu.arm9_io[4], expected,
                "ARM9 DISPSTAT write mismatch: wrote {}, initial_protected {}",
                write_val, initial_protected
            );
            assert_eq!(
                mmu.arm7_io[4], expected,
                "ARM7 DISPSTAT write mismatch: wrote {}, initial_protected {}",
                write_val, initial_protected
            );
        }
    }
}

#[test]
fn test_palette_ram_indexing_exhaustive() {
    let mut mmu = NdsMmu::new();

    // Zero out palette RAM
    mmu.reset();

    // Write different values to index 0 and index 2048
    mmu.write_byte_arm9(0x05000000, 0xA5);
    mmu.write_byte_arm9(0x05000800, 0x5A);

    // Verify index 2048 does not overwrite index 0
    assert_eq!(mmu.read_byte_arm9(0x05000000), 0xA5, "Writing to Palette RAM index 2048 overwrote index 0!");
    assert_eq!(mmu.read_byte_arm9(0x05000800), 0x5A);

    // Verify indexing wraps at 4096 (0x1000)
    mmu.write_byte_arm9(0x05001000, 0x99);
    assert_eq!(mmu.read_byte_arm9(0x05000000), 0x99, "Index 4096 did not wrap to index 0!");
    assert_eq!(mmu.read_byte_arm9(0x05000800), 0x5A, "Index 4096 overwrite affected index 2048!");
}

#[test]
fn test_render_scanline_oob_safety_exhaustive() {
    let ppu = NdsPpu::new();
    let mut mmu = NdsMmu::new();
    let mut video_buffer = vec![0u16; 256 * 384];

    // Fill buffer with signature pattern
    for i in 0..video_buffer.len() {
        video_buffer[i] = i as u16;
    }

    let cloned_buffer = video_buffer.clone();

    // Call render_scanline with out of bounds lines (ly >= 192)
    for ly in 192..=1000u16 {
        ppu.render_scanline(ly, &mmu, &mut video_buffer);
        // Verify buffer is completely unchanged (no writes performed)
        assert_eq!(video_buffer, cloned_buffer, "render_scanline wrote to video_buffer for out-of-bounds ly = {}", ly);
    }
}
