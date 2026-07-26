// core/tests/ppu_stress_tests.rs

use emulator_core::nds::ppu::NdsPpu;
use emulator_core::nds::mmu::NdsMmu;

fn bgr555(r: u8, g: u8, b: u8) -> u16 {
    (((b as u16) >> 3) << 10) | (((g as u16) >> 3) << 5) | ((r as u16) >> 3)
}

/// Bus cycles from the start of a scanline to HBlank: 256 visible dots x 6.
///
/// These were 512 / 710 when this file was written — one third of the real
/// values, which made the PPU complete three frames per frame budget and ran the
/// whole game at 180 game-fps (compressed intro, re-struck notes). The constants
/// live here so a future timing change is one edit, not thirty literals.
const HBLANK_AT: u32 = 1536;
/// Bus cycles per NDS scanline: 355 dots x 6.
const LINE_CYCLES: u32 = 2130;
/// Scanlines per frame, including VBlank.
const LINES_PER_FRAME: u16 = 263;

fn read_vcount(mmu: &NdsMmu) -> u16 {
    ((mmu.arm9_io[7] as u16) << 8) | (mmu.arm9_io[6] as u16)
}

fn read_dispstat9(mmu: &NdsMmu) -> u16 {
    ((mmu.arm9_io[5] as u16) << 8) | (mmu.arm9_io[4] as u16)
}

#[test]
fn test_ppu_cycle_accumulation_and_drift() {
    let mut ppu = NdsPpu::new();
    let mut mmu = NdsMmu::new();
    let mut video_buffer = vec![0u16; 256 * 384];

    // Initialize VCOUNT to 0
    mmu.set_vcount(0);

    // 1. One cycle short of HBlank: accumulated, flag still clear.
    ppu.tick(HBLANK_AT - 1, &mut mmu, &mut video_buffer, false);
    assert_eq!(ppu.cycle_accumulator, HBLANK_AT - 1);
    assert_eq!(read_dispstat9(&mmu) & (1 << 1), 0, "HBlank must not be set before {HBLANK_AT}");

    // 2. The cycle that reaches HBlank sets DISPSTAT bit 1.
    ppu.tick(1, &mut mmu, &mut video_buffer, false);
    assert_eq!(ppu.cycle_accumulator, HBLANK_AT);
    assert_ne!(read_dispstat9(&mmu) & (1 << 1), 0, "HBlank must be set at {HBLANK_AT}");

    // 3. Still the same scanline one cycle before the line ends.
    ppu.tick(LINE_CYCLES - 1 - HBLANK_AT, &mut mmu, &mut video_buffer, false);
    assert_eq!(ppu.cycle_accumulator, LINE_CYCLES - 1);
    assert_eq!(read_vcount(&mmu), 0, "scanline must not advance before {LINE_CYCLES}");

    // 4. The cycle that completes the line advances VCOUNT and consumes exactly
    // one line's worth of cycles (subtract, not reset — see case 5).
    ppu.tick(1, &mut mmu, &mut video_buffer, false);
    assert_eq!(read_vcount(&mmu), 1, "scanline advances at {LINE_CYCLES}");
    assert_eq!(ppu.cycle_accumulator, 0);

    // 5. Overshoot must carry over rather than be discarded: dropping the
    // remainder every line is a drift of up to one line per line.
    ppu.reset();
    mmu.reset();
    mmu.set_vcount(0);
    ppu.tick(LINE_CYCLES + 5, &mut mmu, &mut video_buffer, false);
    assert_eq!(read_vcount(&mmu), 1);
    assert_eq!(ppu.cycle_accumulator, 5, "5 cycles must carry into the next line");

    // 6. A multi-line tick advances every line it covers, not just one.
    ppu.reset();
    mmu.reset();
    mmu.set_vcount(0);
    ppu.tick(3 * LINE_CYCLES + 2, &mut mmu, &mut video_buffer, false);
    assert_eq!(read_vcount(&mmu), 3, "three full lines in one tick");
    assert_eq!(ppu.cycle_accumulator, 2);
}

#[test]
fn test_vblank_transition_and_wrap_around() {
    let mut ppu = NdsPpu::new();
    let mut mmu = NdsMmu::new();
    let mut video_buffer = vec![0u16; 256 * 384];

    // Set vcount to 191 (just before VBlank)
    mmu.set_vcount(191);

    // One line advances to 192, where VBlank starts.
    ppu.tick(LINE_CYCLES, &mut mmu, &mut video_buffer, false);
    assert_eq!(read_vcount(&mmu), 192, "VCount should be 192");
    assert_ne!(read_dispstat9(&mmu) & 1, 0, "VBlank flag should be set at line 192");

    // VBlank stays asserted for every remaining line of the frame.
    for line in 193..LINES_PER_FRAME {
        ppu.tick(LINE_CYCLES, &mut mmu, &mut video_buffer, false);
        assert_eq!(read_vcount(&mmu), line, "VCount should match current scanline");
        assert_ne!(
            read_dispstat9(&mmu) & 1,
            0,
            "VBlank flag should remain set at line {line}"
        );
    }

    // The last line wraps to 0 and clears VBlank.
    ppu.tick(LINE_CYCLES, &mut mmu, &mut video_buffer, false);
    assert_eq!(read_vcount(&mmu), 0, "VCount should wrap around to 0");
    assert_eq!(read_dispstat9(&mmu) & 1, 0, "VBlank flag should be cleared at line 0");
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

    // Tick to HBlank start
    ppu.tick(HBLANK_AT, &mut mmu, &mut video_buffer, false);
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

    ppu.tick(HBLANK_AT, &mut mmu, &mut video_buffer, false);
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

    ppu.tick(LINE_CYCLES, &mut mmu, &mut video_buffer, false); // -> 192 (VBlank start)
    assert_eq!(mmu.arm9_if & (1 << 0), 0, "VBlank IRQ should NOT be triggered if disabled in DISPSTAT");

    // Test 5: VBlank Interrupt enabled (bit 3 of DISPSTAT is 1)
    ppu.reset();
    mmu.reset();
    mmu.set_vcount(191);
    mmu.arm9_io[4] = 1 << 3;
    mmu.arm9_if = 0;

    ppu.tick(LINE_CYCLES, &mut mmu, &mut video_buffer, false); // -> 192
    assert_ne!(mmu.arm9_if & (1 << 0), 0, "VBlank IRQ should be triggered when enabled in DISPSTAT");
}

#[test]
fn test_screen_swap_routing() {
    let ppu = NdsPpu::new();
    let mut mmu = NdsMmu::new();
    let mut video_buffer = vec![0u16; 256 * 384];

    // Give each engine a distinct backdrop and put both in graphics mode.
    // DISPCNT bits 16-17 select the display mode; 0 is "display off", which
    // renders white and would make both screens indistinguishable (the reason
    // this test used to compare against hardcoded debug colours).
    let color_a = bgr555(255, 0, 0); // engine A backdrop: red
    let color_b = bgr555(0, 0, 255); // engine B backdrop: blue
    mmu.palette_ram[0..2].copy_from_slice(&color_a.to_le_bytes());
    mmu.palette_ram[0x400..0x402].copy_from_slice(&color_b.to_le_bytes());
    mmu.arm9_io[2] = 1; // DISPCNT_A display mode 1
    mmu.arm9_io[0x1002] = 1; // DISPCNT_B display mode 1

    // 1. Normal layout: POWCNT1 bit 15 clear -> engine A on the BOTTOM screen.
    mmu.arm9_io[0x305] = 0;
    ppu.render_scanline(0, &mmu, &mut video_buffer);
    assert_eq!(video_buffer[0], color_b, "engine B is on top when POWCNT1 bit 15 is clear");
    assert_eq!(video_buffer[192 * 256], color_a, "engine A is on the bottom screen");

    // 2. Swapped layout: bit 15 set -> engine A on the TOP screen.
    mmu.arm9_io[0x305] = 0x80;
    video_buffer.fill(0);
    ppu.render_scanline(0, &mmu, &mut video_buffer);
    assert_eq!(video_buffer[0], color_a, "engine A moves to the top screen");
    assert_eq!(video_buffer[192 * 256], color_b, "engine B moves to the bottom screen");
}

#[test]
fn test_touch_coordinates_safety() {
    let ppu = NdsPpu::new();
    let mut mmu = NdsMmu::new();
    let mut video_buffer = vec![0u16; 256 * 384];

    // Put both engines in graphics mode with a known backdrop, so "no dot here"
    // is a definite colour instead of the white a disabled engine renders.
    let backdrop = bgr555(0, 255, 0);
    mmu.palette_ram[0..2].copy_from_slice(&backdrop.to_le_bytes());
    mmu.palette_ram[0x400..0x402].copy_from_slice(&backdrop.to_le_bytes());
    mmu.arm9_io[2] = 1;
    mmu.arm9_io[0x1002] = 1;

    // Enable touch
    mmu.buttons.nds_touch_pressed = true;

    // Test Case 1: touch within bounds (x=10, y=10)
    mmu.buttons.nds_touch_x = 10;
    mmu.buttons.nds_touch_y = 10;
    ppu.render_scanline(10, &mmu, &mut video_buffer);

    let yellow = bgr555(255, 255, 0);
    assert_eq!(video_buffer[(192 + 10) * 256 + 10], yellow, "Stylus dot should be drawn at (10, 10)");
    assert_eq!(
        video_buffer[(192 + 11) * 256 + 10], 0,
        "rendering scanline 10 must not touch row 11 at all"
    );

    // Render scanline 11 and check that it draws the second row segment
    ppu.render_scanline(11, &mmu, &mut video_buffer);
    assert_eq!(video_buffer[(192 + 11) * 256 + 10], yellow, "Stylus dot should be drawn at (10, 11) when rendering scanline 11");

    // Test Case 2: touch x out of bounds (x=256, y=10) -> no dot, no panic. The
    // pixel holds the rendered backdrop, not zero.
    video_buffer.fill(0);
    mmu.buttons.nds_touch_x = 256;
    mmu.buttons.nds_touch_y = 10;
    ppu.render_scanline(10, &mmu, &mut video_buffer);
    assert_eq!(video_buffer[(192 + 10) * 256 + 10], backdrop);

    // Test Case 3: touch y out of bounds (x=10, y=192) -> no dot, no panic.
    video_buffer.fill(0);
    mmu.buttons.nds_touch_x = 10;
    mmu.buttons.nds_touch_y = 192;
    // render_scanline is called with ly = 10, but touch y = 192, so they never match.
    ppu.render_scanline(10, &mmu, &mut video_buffer);
    assert_eq!(video_buffer[(192 + 10) * 256 + 10], backdrop);
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

    // 2. DISPSTAT write masking. DISPSTAT is 0x04000004, NOT 0x04000204 —
    //    0x204 is EXMEMCNT. This test used to drive the masking through 0x204
    //    because the handler was keyed there by a transposed digit, so it was
    //    asserting that an EXMEMCNT access rewrites DISPSTAT: exactly the defect
    //    (an EXMEMCNT write cleared the VBlank/HBlank/VCount IRQ enables).
    mmu.arm9_io[4] = 0x07; // set read-only bits (0, 1, 2)
    mmu.arm7_io[4] = 0x07;

    mmu.write_byte_arm9(0x04000004, 0xFF);
    mmu.write_byte_arm7(0x04000004, 0xFF);

    assert_eq!(mmu.arm9_io[4], 0xBF);
    assert_eq!(mmu.arm7_io[4], 0xBF);

    mmu.write_byte_arm9(0x04000004, 0x00);
    assert_eq!(mmu.arm9_io[4], 0x07);

    // And the property the fix exists for: EXMEMCNT must not touch DISPSTAT.
    mmu.arm9_io[4] = 0x3F; // all three IRQ enables set
    mmu.write_byte_arm9(0x04000204, 0x80);
    assert_eq!(
        mmu.arm9_io[4], 0x3F,
        "an EXMEMCNT write must leave DISPSTAT alone"
    );
}



#[test]
fn test_timing_carryover_detailed_stress() {
    let mut ppu = NdsPpu::new();
    let mut mmu = NdsMmu::new();
    let mut video_buffer = vec![0u16; 256 * 384];

    // Verify prime cycle increments and timing accumulation
    mmu.reset();
    mmu.set_vcount(0);
    ppu.reset();

    // Many small ticks must accumulate exactly, with no per-tick rounding.
    const STEP: u32 = 17;
    let steps_in_line = LINE_CYCLES / STEP; // 125 steps stay inside line 0
    for _ in 0..steps_in_line {
        ppu.tick(STEP, &mut mmu, &mut video_buffer, false);
    }
    assert_eq!(ppu.cycle_accumulator, steps_in_line * STEP);
    assert_eq!(read_vcount(&mmu), 0);

    // The step that crosses the line boundary advances VCOUNT and carries the
    // remainder.
    ppu.tick(STEP, &mut mmu, &mut video_buffer, false);
    assert_eq!(read_vcount(&mmu), 1);
    assert_eq!(ppu.cycle_accumulator, (steps_in_line + 1) * STEP - LINE_CYCLES);

    // One huge tick must be equivalent to the same cycles delivered in pieces.
    let start_vcount = read_vcount(&mmu);
    let start_accum = ppu.cycle_accumulator;
    const BIG: u32 = 150_000;
    let total_avail = start_accum + BIG;
    let expected_lines = total_avail / LINE_CYCLES;
    let expected_accum = total_avail % LINE_CYCLES;

    ppu.tick(BIG, &mut mmu, &mut video_buffer, false);
    let final_vcount = read_vcount(&mmu);
    let expected_vcount = (start_vcount + expected_lines as u16) % LINES_PER_FRAME;

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

            // 0x04000004 is DISPSTAT; 0x204 is EXMEMCNT (see the note above).
            mmu.write_byte_arm9(0x04000004, write_val);
            mmu.write_byte_arm7(0x04000004, write_val);

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
    let mmu = NdsMmu::new();
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
