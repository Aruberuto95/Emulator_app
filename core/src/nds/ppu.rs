use crate::nds::mmu::NdsMmu;

/// Decode DISPSTAT's split V-Count setting.
///
/// GBATEK: LYC bits 0-7 live in DISPSTAT bits 8-15 and LYC bit 8 in DISPSTAT
/// bit 7. The previous `(dispstat >> 7) & 0x1FF` landed bit 8 in the LSB and
/// shifted the low byte UP by one, so LYC 100 matched at scanline 200 and any
/// LYC >= 128 decoded past the 262-line frame and could never match at all.
///
/// This drives the V-Counter *flag* (DISPSTAT bit 2) as well as the IRQ, and
/// software can poll that flag with the IRQ disabled — measured on SoulSilver,
/// which never enables the IRQ (bit 5 clear on both cores) but does program the
/// field, so the game was unaffected and this is a correctness fix, not a
/// symptom fix.
#[inline]
fn vcount_setting(dispstat: u16) -> u16 {
    ((dispstat >> 8) & 0xFF) | (((dispstat >> 7) & 1) << 8)
}

pub struct NdsPpu {
    pub cycle_accumulator: u32,
    pub frame_completed: bool,
    pub frame_count: u32,
    /// Wall-clock nanoseconds spent compositing visible scanlines, accumulated
    /// only while [`NdsMmu::prof_cpu_on`] is set.
    ///
    /// Evidence for throughput, not emulated state (hence not snapshotted):
    /// the frontend paces on the audio queue, so a tick costing more than one
    /// frame of real time starves the device. Gated because it samples once per
    /// visible scanline — 192 `QueryPerformanceCounter` pairs per frame, paid by
    /// every player to serve a measurement nobody is reading. Same switch as
    /// `NdsMmu::prof_cpu_ns`, so one env var turns the whole NDS profile on.
    pub prof_render_ns: u64,
}

impl NdsPpu {
    pub fn new() -> Self {
        Self {
            cycle_accumulator: 0,
            frame_completed: false,
            frame_count: 0,
            prof_render_ns: 0,
        }
    }

    pub fn reset(&mut self) {
        self.cycle_accumulator = 0;
        self.frame_completed = false;
        self.frame_count = 0;
    }

    /// Advance the 2D engines by `cycles`.
    ///
    /// `render_pixels` gates **only** the visible-scanline composition. VCOUNT,
    /// DISPSTAT, the HBlank/VBlank IRQs and [`Self::frame_completed`] advance
    /// identically either way, which is what makes it safe for the run loop to
    /// clear it on the intermediate frames of a fast-forward tick.
    pub fn tick(
        &mut self,
        cycles: u32,
        mmu: &mut NdsMmu,
        video_buffer: &mut [u16],
        render_pixels: bool,
    ) {
        let mut vcount = ((mmu.arm9_io[7] as u16) << 8) | (mmu.arm9_io[6] as u16);
        self.cycle_accumulator += cycles;

        loop {
            let dispstat_val = ((mmu.arm9_io[5] as u16) << 8) | (mmu.arm9_io[4] as u16);
            let hblank_set = (dispstat_val & (1 << 1)) != 0;

            // NDS scanline = 355 dots x 6 = 2130 bus cycles (33.55 MHz), 256
            // visible dots = 1536 cycles before HBlank. U30 root cause: these
            // were 512/710 (one third of real), so the PPU completed THREE
            // frames — three VBlank IRQs — per 560190-cycle frame budget and
            // the whole game (intro script, music cues, animations) ran at
            // 180 game-fps: the intro played 3x fast, notes re-struck early
            // (the reported bell "echo"), and scenes flew by.
            if !hblank_set && self.cycle_accumulator >= 1536 {
                let mut dispstat_arm9 = ((mmu.arm9_io[5] as u16) << 8) | (mmu.arm9_io[4] as u16);
                let mut dispstat_arm7 = ((mmu.arm7_io[5] as u16) << 8) | (mmu.arm7_io[4] as u16);

                dispstat_arm9 |= 1 << 1;
                dispstat_arm7 |= 1 << 1;

                if (dispstat_arm9 & (1 << 4)) != 0 {
                    mmu.trigger_interrupt_arm9(1 << 1);
                }
                if (dispstat_arm7 & (1 << 4)) != 0 {
                    mmu.trigger_interrupt_arm7(1 << 1);
                }

                mmu.arm9_io[4] = dispstat_arm9 as u8;
                mmu.arm9_io[5] = (dispstat_arm9 >> 8) as u8;
                mmu.arm7_io[4] = dispstat_arm7 as u8;
                mmu.arm7_io[5] = (dispstat_arm7 >> 8) as u8;

                continue;
            }

            if self.cycle_accumulator >= 2130 {
                self.cycle_accumulator -= 2130;
                vcount = (vcount + 1) % 263;
                mmu.set_vcount(vcount);

                let mut dispstat_arm9 = ((mmu.arm9_io[5] as u16) << 8) | (mmu.arm9_io[4] as u16);
                let mut dispstat_arm7 = ((mmu.arm7_io[5] as u16) << 8) | (mmu.arm7_io[4] as u16);

                dispstat_arm9 &= !(1 << 1);
                dispstat_arm7 &= !(1 << 1);

                // Handle VBlank transition
                if vcount == 192 {
                    self.frame_completed = true;
                    self.frame_count = self.frame_count.wrapping_add(1);

                    dispstat_arm9 |= 1 << 0;
                    dispstat_arm7 |= 1 << 0;

                    if (dispstat_arm9 & (1 << 3)) != 0 {
                        mmu.trigger_interrupt_arm9(1 << 0);
                    }
                    if (dispstat_arm7 & (1 << 3)) != 0 {
                        mmu.trigger_interrupt_arm7(1 << 0);
                    }

                    mmu.has_3d_activity = false;
                    // The 3D buffer swap happens here, at the frame boundary,
                    // exactly as hardware defers SWAP_BUFFERS to VBlank. The
                    // rasterizer runs synchronously from the CPU store that
                    // carries the command, which lands mid-visible-period (the
                    // SDK swaps before `OS_WaitVBlankIntr`), so presenting at
                    // the store instead of here rewrote the buffer the loop
                    // below is scanning out of — a horizontal seam with the
                    // scene above it one frame behind the scene below, moving
                    // only while the camera moves. See `Gx3d::fb_back`.
                    mmu.gx.engine.present();
                } else if vcount == 0 {
                    dispstat_arm9 &= !(1 << 0);
                    dispstat_arm7 &= !(1 << 0);
                }

                // Handle VCompare match
                let setting_arm9 = vcount_setting(dispstat_arm9);
                let setting_arm7 = vcount_setting(dispstat_arm7);

                if vcount == setting_arm9 {
                    dispstat_arm9 |= 1 << 2;
                    if (dispstat_arm9 & (1 << 5)) != 0 {
                        mmu.trigger_interrupt_arm9(1 << 2);
                    }
                } else {
                    dispstat_arm9 &= !(1 << 2);
                }

                if vcount == setting_arm7 {
                    dispstat_arm7 |= 1 << 2;
                    if (dispstat_arm7 & (1 << 5)) != 0 {
                        mmu.trigger_interrupt_arm7(1 << 2);
                    }
                } else {
                    dispstat_arm7 &= !(1 << 2);
                }

                mmu.arm9_io[4] = dispstat_arm9 as u8;
                mmu.arm9_io[5] = (dispstat_arm9 >> 8) as u8;
                mmu.arm7_io[4] = dispstat_arm7 as u8;
                mmu.arm7_io[5] = (dispstat_arm7 >> 8) as u8;

                // Render visible scanlines
                if render_pixels && vcount < 192 {
                    // Resolve the published 3D job before timing the 2D compositor.
                    mmu.gx.engine.prepare_front();
                    let prof_t0 = mmu.prof_cpu_on.then(std::time::Instant::now);
                    self.render_scanline(vcount, mmu, video_buffer);
                    if let Some(t0) = prof_t0 {
                        self.prof_render_ns = self
                            .prof_render_ns
                            .wrapping_add(t0.elapsed().as_nanos() as u64);
                    }
                }

                continue;
            }

            break;
        }
    }

    /// Compose one visible scanline of both 2D engines into the stacked
    /// 256x384 BGR555 framebuffer (engine A + engine B, swapped per POWCNT1
    /// bit 15). Text backgrounds only: 4/8bpp tiles, scroll, H/V flip,
    /// per-BG priority, palette color 0 as backdrop. Not yet rendered --
    /// affine/extended BGs, OBJ sprites, blending/windows and the 3D engine
    /// (engine A BG0 with DISPCNT bit 3 is skipped as transparent).
    pub fn render_scanline(&self, ly: u16, mmu: &NdsMmu, video_buffer: &mut [u16]) {
        if ly >= 192 {
            return;
        }
        let line_a = Self::render_engine_line(false, ly, mmu);
        let line_b = Self::render_engine_line(true, ly, mmu);

        // POWCNT1 bit 15: 1 = engine A on the top screen.
        let a_on_top = (mmu.arm9_io[0x305] & 0x80) != 0;
        let top_offset = ly as usize * 256;
        let bottom_offset = (192 + ly) as usize * 256;
        if a_on_top {
            video_buffer[top_offset..top_offset + 256].copy_from_slice(&line_a);
            video_buffer[bottom_offset..bottom_offset + 256].copy_from_slice(&line_b);
        } else {
            video_buffer[top_offset..top_offset + 256].copy_from_slice(&line_b);
            video_buffer[bottom_offset..bottom_offset + 256].copy_from_slice(&line_a);
        }

        // Stylus feedback dot on the bottom screen.
        if mmu.buttons.nds_touch_pressed {
            let tx = mmu.buttons.nds_touch_x as usize;
            let ty = mmu.buttons.nds_touch_y as usize;
            if (ly as usize == ty || ly as usize == ty + 1) && ty < 192 && tx < 256 {
                let yellow: u16 = 0x03FF; // BGR555 yellow
                video_buffer[bottom_offset + tx] = yellow;
                if tx + 1 < 256 {
                    video_buffer[bottom_offset + tx + 1] = yellow;
                }
            }
        }
    }

    /// One engine scanline. `engine_b` selects the sub engine (register
    /// block at +0x1000, palettes at +0x400, VRAM target 4 instead of 1).
    fn render_engine_line(engine_b: bool, ly: u16, mmu: &NdsMmu) -> [u16; 256] {
        let io = if engine_b { 0x1000usize } else { 0 };
        let dispcnt = u32::from_le_bytes([
            mmu.arm9_io[io],
            mmu.arm9_io[io + 1],
            mmu.arm9_io[io + 2],
            mmu.arm9_io[io + 3],
        ]);
        let pal_base = if engine_b { 0x400usize } else { 0 };
        let backdrop = u16::from_le_bytes([
            mmu.palette_ram[pal_base],
            mmu.palette_ram[pal_base + 1],
        ]) & 0x7FFF;

        let mut line = [backdrop; 256];
        // Per-pixel layer bookkeeping for BLDCNT color effects, using the
        // BLDCNT bit layout (BG0-3 = 0-3, OBJ = 4, backdrop = 5): the current
        // top-most layer, plus the pixel (and layer) directly beneath it.
        // Painting runs strictly back-to-front, so "what was on top when this
        // pixel landed" IS the top-most pixel underneath — hardware's second
        // blend input.
        let mut layer = [5u8; 256];
        let mut under = [backdrop; 256];
        let mut under_layer = [5u8; 256];
        // WIN0/WIN1 layer masking (U31h): per pixel, bits 0-3 gate BG0-3,
        // bit 4 the OBJ layer, bit 5 the BLDCNT color effects. Inside WIN0 ->
        // WININ low byte; inside WIN1 -> WININ high byte; outside all enabled
        // windows -> WINOUT low byte. The intro's Unown-swarm wipe drives
        // WIN0/WIN1 per frame with WINOUT masking every BG — without this the
        // in-progress swarm BG showed as a garbage band.
        // ponytail: OBJ window (DISPCNT bit 15 + mode-2 sprites) falls back
        // to WINOUT — no scene evidence yet.
        let win_enable = (dispcnt >> 13) & 7;
        let wmask: [u8; 256] = if win_enable & 3 == 0 {
            [0x3F; 256]
        } else {
            let winin = u16::from_le_bytes([mmu.arm9_io[io + 0x48], mmu.arm9_io[io + 0x49]]);
            let winout = u16::from_le_bytes([mmu.arm9_io[io + 0x4A], mmu.arm9_io[io + 0x4B]]);
            let mut m = [(winout & 0x3F) as u8; 256];
            // WIN1 first, then WIN0 on top (WIN0 wins where they overlap).
            for w in [1usize, 0] {
                if (win_enable >> w) & 1 == 0 {
                    continue;
                }
                let h = u16::from_le_bytes([
                    mmu.arm9_io[io + 0x40 + w * 2],
                    mmu.arm9_io[io + 0x41 + w * 2],
                ]);
                let v = u16::from_le_bytes([
                    mmu.arm9_io[io + 0x44 + w * 2],
                    mmu.arm9_io[io + 0x45 + w * 2],
                ]);
                let (y1, y2) = (v >> 8, v & 0xFF);
                // Y1>Y2 wraps (GBATEK): the window covers [Y1,192)+[0,Y2).
                let in_y = if y1 <= y2 { ly >= y1 && ly < y2 } else { ly >= y1 || ly < y2 };
                if !in_y {
                    continue;
                }
                let bits = if w == 0 { winin as u8 & 0x3F } else { (winin >> 8) as u8 & 0x3F };
                let (x1, x2) = ((h >> 8) as usize, (h & 0xFF) as usize);
                if x1 <= x2 {
                    m[x1..x2.min(256)].fill(bits);
                } else {
                    m[x1.min(256)..].fill(bits);
                    m[..x2.min(256)].fill(bits);
                }
            }
            m
        };
        match (dispcnt >> 16) & 3 {
            // Display off: white.
            0 => line = [0x7FFF; 256],
            // Graphics mode: text BGs and OBJ sprites by priority
            // (3 = back, 0 = front; OBJ beats a BG of the same priority).
            1 => {
                let objs = if dispcnt & 0x1000 != 0 {
                    Some(Self::draw_objs(engine_b, ly, dispcnt, mmu))
                } else {
                    None
                };
                let bg_mode = (dispcnt & 7) as usize;
                // Which BGs are TEXT type per mode (others: affine/extended,
                // not rendered yet — U27 evidence: SoulSilver's intro runs
                // mode 0 throughout, so nothing here drops layers).
                let text_mask =
                    [0b1111u32, 0b0111, 0b0011, 0b0111, 0b0011, 0b0011, 0, 0][bg_mode.min(7)];
                for prio in (0..4u32).rev() {
                    for bg in (0..4usize).rev() {
                        if (dispcnt >> (8 + bg)) & 1 == 0 {
                            continue;
                        }
                        // Engine A BG0 with DISPCNT bit3 = the 3D engine's
                        // output (U27 milestone 1: flat-filled geometry).
                        let is_3d = !engine_b && bg == 0 && (dispcnt & 0x8) != 0;
                        if !is_3d && (text_mask >> bg) & 1 == 0 {
                            continue;
                        }
                        let bgcnt = u16::from_le_bytes([
                            mmu.arm9_io[io + 8 + bg * 2],
                            mmu.arm9_io[io + 9 + bg * 2],
                        ]);
                        if (bgcnt & 3) as u32 != prio {
                            continue;
                        }
                        if is_3d {
                            let base = ly as usize * 256;
                            for x in 0..256usize {
                                if wmask[x] & 1 == 0 {
                                    continue;
                                }
                                let px = mmu.gx.engine.fb[base + x];
                                if px & 0x8000 != 0 {
                                    under[x] = line[x];
                                    under_layer[x] = layer[x];
                                    layer[x] = 0;
                                    line[x] = px & 0x7FFF;
                                }
                            }
                        } else {
                            Self::draw_text_bg(
                                engine_b, bg, dispcnt, bgcnt, ly, mmu, &wmask, &mut line,
                                &mut layer, &mut under, &mut under_layer,
                            );
                        }
                    }
                    // OBJ pixels of this priority go over the BGs of the same
                    // priority (and under lower-numbered ones, drawn later).
                    if let Some((oc, op)) = &objs {
                        for x in 0..256usize {
                            if op[x] == prio as u8 && wmask[x] & 0x10 != 0 {
                                under[x] = line[x];
                                under_layer[x] = layer[x];
                                layer[x] = 4;
                                line[x] = oc[x];
                            }
                        }
                    }
                }
                // BLDCNT (0x50) color effects: 1 = alpha-blend the top pixel
                // over the one beneath when their layers match the first/
                // second target masks; 2/3 = brighten/darken first-target
                // pixels by BLDY. The intro leans on these (BG0 blended out
                // at EVA=0 over the movie layers, partial blends on the
                // title transition — U27 evidence strip). ponytail: semi-
                // transparent OBJs (attr0 mode 1) don't force alpha yet; add
                // when a scene shows it.
                let bldcnt = u16::from_le_bytes([mmu.arm9_io[io + 0x50], mmu.arm9_io[io + 0x51]]);
                let effect = (bldcnt >> 6) & 3;
                if effect != 0 {
                    let first = bldcnt as u32 & 0x3F;
                    let second = (bldcnt as u32 >> 8) & 0x3F;
                    let bldalpha =
                        u16::from_le_bytes([mmu.arm9_io[io + 0x52], mmu.arm9_io[io + 0x53]]);
                    let eva = (bldalpha as u32 & 0x1F).min(16);
                    let evb = ((bldalpha as u32 >> 8) & 0x1F).min(16);
                    let evy = (mmu.arm9_io[io + 0x54] as u32 & 0x1F).min(16);
                    for x in 0..256usize {
                        if (first >> layer[x]) & 1 == 0 || wmask[x] & 0x20 == 0 {
                            continue;
                        }
                        line[x] = match effect {
                            1 if (second >> under_layer[x]) & 1 == 1 => {
                                Self::alpha_blend(line[x], under[x], eva, evb)
                            }
                            2 => Self::brighten(line[x], evy),
                            3 => Self::darken(line[x], evy),
                            _ => line[x],
                        };
                    }
                }
            }
            // VRAM display (LCDC): raw 16bpp bitmap from the selected bank.
            2 => {
                let bank = (dispcnt >> 18) & 3;
                let base = bank * 0x20000 + ly as u32 * 512;
                for (x, px) in line.iter_mut().enumerate() {
                    let off = base + x as u32 * 2;
                    *px = u16::from_le_bytes([
                        mmu.vram.read_lcdc(off),
                        mmu.vram.read_lcdc(off + 1),
                    ]) & 0x7FFF;
                }
            }
            // Main-memory display: not implemented (unused by boot flows).
            _ => {}
        }
        // MASTER_BRIGHT (0x6C / 0x106C): final whole-engine fade toward
        // white (mode 1) or black (mode 2), factor 0-16. The intro's fades
        // are driven entirely by this register (U27 evidence strip); with it
        // ignored, every fade rendered as a hard cut at full color.
        let bright = u16::from_le_bytes([mmu.arm9_io[io + 0x6C], mmu.arm9_io[io + 0x6D]]);
        let f = (bright as u32 & 0x1F).min(16);
        if f > 0 {
            match (bright >> 14) & 3 {
                1 => line.iter_mut().for_each(|p| *p = Self::brighten(*p, f)),
                2 => line.iter_mut().for_each(|p| *p = Self::darken(*p, f)),
                _ => {}
            }
        }
        line
    }

    /// Scale a BGR555 color toward white by factor/16 (BLDCNT brightness-up
    /// and MASTER_BRIGHT mode 1 share this math per GBATEK).
    fn brighten(c: u16, f: u32) -> u16 {
        let ch = |v: u32| v + (31 - v) * f / 16;
        (ch(c as u32 & 31) | (ch((c as u32 >> 5) & 31) << 5) | (ch((c as u32 >> 10) & 31) << 10))
            as u16
    }

    /// Scale a BGR555 color toward black by factor/16.
    fn darken(c: u16, f: u32) -> u16 {
        let ch = |v: u32| v - v * f / 16;
        (ch(c as u32 & 31) | (ch((c as u32 >> 5) & 31) << 5) | (ch((c as u32 >> 10) & 31) << 10))
            as u16
    }

    /// BLDCNT alpha: top*eva/16 + under*evb/16 per channel, clamped at 31.
    fn alpha_blend(top: u16, under: u16, eva: u32, evb: u32) -> u16 {
        let ch = |t: u32, u: u32| ((t * eva + u * evb) / 16).min(31);
        (ch(top as u32 & 31, under as u32 & 31)
            | (ch((top as u32 >> 5) & 31, (under as u32 >> 5) & 31) << 5)
            | (ch((top as u32 >> 10) & 31, (under as u32 >> 10) & 31) << 10)) as u16
    }

    /// Rasterize this engine's sprites for one scanline into (color,
    /// priority) buffers; priority 0xFF marks "no OBJ pixel here".
    /// OBJ-vs-OBJ overlap: the LOWER PRIORITY VALUE wins; equal priorities
    /// resolve by OAM index (scanning 0..128, the earlier entry keeps the
    /// pixel). U32 evidence: the intro moon (OAM 4-7, prio 1) must NOT cover
    /// the flying Lugia (OAM 8-11, prio 0) — index-first drew Lugia behind
    /// the moon; the reference shows it in front.
    /// ponytail: affine OBJs, bitmap OBJs (mode 3), the OBJ window (mode 2)
    /// and mosaic are skipped; semi-transparent (mode 1) draws opaque (no
    /// blending yet). Add each when a visible scene needs it.
    fn draw_objs(
        engine_b: bool,
        ly: u16,
        dispcnt: u32,
        mmu: &NdsMmu,
    ) -> ([u16; 256], [u8; 256]) {
        let mut color = [0u16; 256];
        let mut prio = [0xFFu8; 256];
        let oam_base = if engine_b { 0x400usize } else { 0 };
        let map_1d = dispcnt & 0x10 != 0;
        // 1D mapping: attr2 tile index counts in units of 32 << boundary bytes.
        let boundary = (dispcnt >> 20) & 3;
        let pal_base = if engine_b { 0x600usize } else { 0x200 };
        for i in 0..128usize {
            let at = oam_base + i * 8;
            let a0 = u16::from_le_bytes([mmu.oam[at], mmu.oam[at + 1]]);
            let a1 = u16::from_le_bytes([mmu.oam[at + 2], mmu.oam[at + 3]]);
            let a2 = u16::from_le_bytes([mmu.oam[at + 4], mmu.oam[at + 5]]);
            let rotscale = a0 & 0x100 != 0;
            if !rotscale && a0 & 0x200 != 0 {
                continue; // disabled (bit9 is double-size when rotscale)
            }
            let mode = (a0 >> 10) & 3;
            if mode >= 2 {
                continue; // OBJ window / bitmap OBJ
            }
            let (w, h): (u32, u32) = match ((a0 >> 14) & 3, (a1 >> 14) & 3) {
                (0, 0) => (8, 8),
                (0, 1) => (16, 16),
                (0, 2) => (32, 32),
                (0, 3) => (64, 64),
                (1, 0) => (16, 8),
                (1, 1) => (32, 8),
                (1, 2) => (32, 16),
                (1, 3) => (64, 32),
                (2, 0) => (8, 16),
                (2, 1) => (8, 32),
                (2, 2) => (16, 32),
                (2, 3) => (32, 64),
                _ => continue, // shape 3 prohibited
            };
            // Rotscale sprites render into a (possibly doubled) window and
            // sample texels through their PA-PD 8.8 affine matrix (GBATEK;
            // same scheme as the GBA OBJ path). H/V flip bits are parameter-
            // select bits in this mode, so flips apply only to regular OBJs.
            let (aw, ah) = if rotscale && a0 & 0x200 != 0 { (w * 2, h * 2) } else { (w, h) };
            // Y wraps mod 256; X is 9-bit signed (>= 256 means off-left).
            let row = (ly as u32).wrapping_sub((a0 & 0xFF) as u32) & 0xFF;
            if row >= ah {
                continue;
            }
            let x0 = {
                let x = (a1 & 0x1FF) as i32;
                if x >= 256 { x - 512 } else { x }
            };
            let (pa, pb, pc, pd) = if rotscale {
                let g = oam_base + (((a1 >> 9) & 0x1F) as usize) * 32;
                let p = |o: usize| i16::from_le_bytes([mmu.oam[g + o], mmu.oam[g + o + 1]]) as i32;
                (p(6), p(14), p(22), p(30))
            } else {
                (0x100, 0, 0, 0x100)
            };
            let color256 = a0 & 0x2000 != 0;
            let tile = (a2 & 0x3FF) as u32;
            let oprio = ((a2 >> 10) & 3) as u8;
            let pal16 = ((a2 >> 12) & 0xF) as usize;
            let py_flip = if !rotscale && a1 & 0x2000 != 0 { h - 1 - row } else { row };
            for sx in 0..aw {
                let xi = x0 + sx as i32;
                if !(0..256).contains(&xi) {
                    continue;
                }
                let x = xi as usize;
                if oprio >= prio[x] {
                    continue; // an earlier OBJ with equal/better priority owns it
                }
                let (px, py) = if rotscale {
                    let dx = sx as i32 - (aw / 2) as i32;
                    let dy = row as i32 - (ah / 2) as i32;
                    let tx = ((pa * dx + pb * dy) >> 8) + (w / 2) as i32;
                    let ty = ((pc * dx + pd * dy) >> 8) + (h / 2) as i32;
                    if !(0..w as i32).contains(&tx) || !(0..h as i32).contains(&ty) {
                        continue;
                    }
                    (tx as u32, ty as u32)
                } else {
                    (if a1 & 0x1000 != 0 { w - 1 - sx } else { sx }, py_flip)
                };
                let (tx, ty, ipx, ipy) = (px / 8, py / 8, px & 7, py & 7);
                let off = if color256 {
                    let t = if map_1d {
                        tile * (32 << boundary) + (ty * (w / 8) + tx) * 64
                    } else {
                        // ponytail: simple additive 2D map (row stride 32
                        // tile slots); exact 5-bit row wrap when a game uses it.
                        (tile + ty * 32 + tx * 2) * 32
                    };
                    t + ipy * 8 + ipx
                } else {
                    let t = if map_1d {
                        tile * (32 << boundary) + (ty * (w / 8) + tx) * 32
                    } else {
                        (tile + ty * 32 + tx) * 32
                    };
                    t + ipy * 4 + ipx / 2
                };
                let raw = if engine_b {
                    mmu.vram.read_obj_b(off)
                } else {
                    mmu.vram.read_obj_a(off)
                };
                let ci = if color256 {
                    raw as usize
                } else if ipx & 1 != 0 {
                    (raw >> 4) as usize
                } else {
                    (raw & 0xF) as usize
                };
                if ci == 0 {
                    continue; // color 0 = transparent (later OBJs may fill it)
                }
                let pi = if color256 { ci } else { pal16 * 16 + ci };
                color[x] = u16::from_le_bytes([
                    mmu.palette_ram[pal_base + pi * 2],
                    mmu.palette_ram[pal_base + pi * 2 + 1],
                ]) & 0x7FFF;
                prio[x] = oprio;
            }
        }
        (color, prio)
    }

    /// Draw one TEXT background line over `line` (color index 0 = transparent).
    #[allow(clippy::too_many_arguments)] // one paint site, four coupled buffers
    fn draw_text_bg(
        engine_b: bool,
        bg: usize,
        dispcnt: u32,
        bgcnt: u16,
        ly: u16,
        mmu: &NdsMmu,
        wmask: &[u8; 256],
        line: &mut [u16; 256],
        layer: &mut [u8; 256],
        under: &mut [u16; 256],
        under_layer: &mut [u8; 256],
    ) {
        let io = if engine_b { 0x1000usize } else { 0 };
        // Engine A adds the DISPCNT global character/screen base blocks.
        let (gchar, gscreen) = if engine_b {
            (0u32, 0u32)
        } else {
            (((dispcnt >> 24) & 7) * 0x1_0000, ((dispcnt >> 27) & 7) * 0x1_0000)
        };
        let char_base = gchar + ((bgcnt as u32 >> 2) & 0xF) * 0x4000;
        let screen_base = gscreen + ((bgcnt as u32 >> 8) & 0x1F) * 0x800;
        let color256 = (bgcnt & 0x80) != 0;
        let size = (bgcnt >> 14) & 3;
        let (w_mask, h_mask) = match size {
            0 => (255u32, 255u32),
            1 => (511, 255),
            2 => (255, 511),
            _ => (511, 511),
        };
        let hofs = u16::from_le_bytes([
            mmu.arm9_io[io + 0x10 + bg * 4],
            mmu.arm9_io[io + 0x11 + bg * 4],
        ]) as u32
            & 0x1FF;
        let vofs = u16::from_le_bytes([
            mmu.arm9_io[io + 0x12 + bg * 4],
            mmu.arm9_io[io + 0x13 + bg * 4],
        ]) as u32
            & 0x1FF;
        // VRAM byte read within this engine's BG address space. Engine B needs
        // the per-bank resolver: banks H and I carry engine-B BG at MST 1,
        // which a generic "MST == 4" match can never see.
        //
        let vram = |off: u32| {
            if engine_b {
                mmu.vram.read_bg_b(off)
            } else {
                mmu.vram.read_bg_a(off)
            }
        };
        let pal_base = if engine_b { 0x400usize } else { 0 };

        let y = (ly as u32 + vofs) & h_mask;
        for x in 0u32..256 {
            let xx = (x + hofs) & w_mask;
            // Screen block: +1 per 256px column, +2 per 256px row (512-wide).
            let sbb = match size {
                1 => xx >> 8,
                2 => y >> 8,
                3 => (xx >> 8) | ((y >> 8) << 1),
                _ => 0,
            };
            let map_off =
                screen_base + sbb * 0x800 + (((y >> 3) & 31) * 32 + ((xx >> 3) & 31)) * 2;
            let entry = u16::from_le_bytes([vram(map_off), vram(map_off + 1)]);
            let tile = (entry & 0x3FF) as u32;
            let px = if entry & 0x400 != 0 { 7 - (xx & 7) } else { xx & 7 };
            let py = if entry & 0x800 != 0 { 7 - (y & 7) } else { y & 7 };
            let color_idx = if color256 {
                vram(char_base + tile * 64 + py * 8 + px) as usize
            } else {
                let b = vram(char_base + tile * 32 + py * 4 + px / 2);
                (if px & 1 != 0 { b >> 4 } else { b & 0xF }) as usize
            };
            if color_idx == 0 {
                continue; // transparent
            }
            if wmask[x as usize] >> bg & 1 == 0 {
                continue; // masked out by WIN0/WIN1/WINOUT
            }
            let pal_idx = if color256 {
                color_idx
            } else {
                ((entry >> 12) & 0xF) as usize * 16 + color_idx
            };
            let xi = x as usize;
            under[xi] = line[xi];
            under_layer[xi] = layer[xi];
            layer[xi] = bg as u8;
            line[xi] = u16::from_le_bytes([
                mmu.palette_ram[pal_base + pal_idx * 2],
                mmu.palette_ram[pal_base + pal_idx * 2 + 1],
            ]) & 0x7FFF;
        }
    }
}

impl crate::snapshot::Snap for NdsPpu {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.cycle_accumulator.snap(v);
        self.frame_completed.snap(v);
        self.frame_count.snap(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GBATEK splits the V-Count setting across DISPSTAT: LYC bits 0-7 sit in
    /// bits 8-15 and LYC bit 8 in bit 7. The old `(dispstat >> 7) & 0x1FF`
    /// shifted the low byte up by one and put bit 8 in the LSB, so LYC 100
    /// matched at scanline 200 and any LYC >= 128 decoded past the 262-line
    /// frame and never matched.
    #[test]
    fn deferred_gx_resolves_only_when_a_visible_scanline_is_composed() {
        let mut mmu = NdsMmu::new();
        let mut ppu = NdsPpu::new();
        let mut video = vec![0; 256 * 384];
        mmu.gx.engine.clear_px = 0x9234;
        mmu.gx.engine.swap_buffers(&mmu.vram, false);
        ppu.tick(2130 * 192, &mut mmu, &mut video, false);
        assert_eq!(mmu.gx.engine.fb[0], 0, "VBlank publishes without raster work");
        mmu.gx.engine.clear_px = 0xFFFF;
        ppu.tick(2130 * 71, &mut mmu, &mut video, true);
        assert_eq!(mmu.gx.engine.fb[0], 0x9234, "scanout resolves the captured swap");
    }

    #[test]
    fn dispstat_vcount_setting_decodes_the_split_field() {
        assert_eq!(vcount_setting(0x6400), 100, "low byte alone");
        assert_eq!(
            vcount_setting(0xC800),
            200,
            "a low byte >= 128 must not spill into bit 8"
        );
        assert_eq!(vcount_setting(0x0080), 256, "DISPSTAT bit 7 is LYC bit 8");
        assert_eq!(vcount_setting(0x0680), 262, "the last scanline of a frame");
        assert_eq!(vcount_setting(0x0000), 0);
        // The flag and IRQ-enable bits below bit 7 must not leak into the value.
        assert_eq!(vcount_setting(0x643F), 100, "bits 0-5 are flags/enables");
    }

    /// A regular 4bpp 8x8 sprite on engine A (1D mapping, OBJ VRAM in bank F
    /// as SoulSilver maps it) renders its palette color at its position, honors
    /// H-flip, clips at the left edge, and treats color index 0 as transparent.
    #[test]
    fn obj_sprite_renders_flips_and_clips() {
        let mut mmu = NdsMmu::new();
        mmu.arm9_io[0] = 0x10; // DISPCNT bit4: OBJ 1D mapping
        mmu.arm9_io[1] = 0x11; // bits 8 (BG0 on, harmless) + 12 (OBJ enable)
        mmu.arm9_io[2] = 0x01; // bits 16-17: display mode 1 (graphics)
        mmu.arm9_io[0x305] = 0x80; // POWCNT1 bit15: engine A on top screen
        mmu.vram.banks[5].control = 0x82; // bank F: enabled, MST 2 = OBJ-A, OFS 0
        // Tile 1: row 0 = pixels [1,0,0,0,0,0,0,2], rows 1+ = color 3 solid.
        let f = &mut mmu.vram.banks[5].data;
        f[32] = 0x01; // px0 lo-nibble=1, px1 hi-nibble=0
        f[35] = 0x20; // px6 lo-nibble=0, px7 hi-nibble=2
        for b in 36..64 {
            f[b] = 0x33;
        }
        // OBJ palette A entries 1/2/3 = distinct BGR555 colors.
        for (idx, col) in [(1usize, 0x001Fu16), (2, 0x03E0), (3, 0x7C00)] {
            mmu.palette_ram[0x200 + idx * 2..0x200 + idx * 2 + 2]
                .copy_from_slice(&col.to_le_bytes());
        }
        // Sprite 0: 8x8 at (x=-4, y=20), tile 1, priority 0, palette bank 0.
        let x = (-4i32 & 0x1FF) as u16;
        mmu.oam[0..2].copy_from_slice(&20u16.to_le_bytes());
        mmu.oam[2..4].copy_from_slice(&x.to_le_bytes());
        mmu.oam[4..6].copy_from_slice(&1u16.to_le_bytes());

        let ppu = NdsPpu::new();
        let mut fb = vec![0u16; 256 * 384];
        ppu.render_scanline(20, &mmu, &mut fb); // sprite row 0
        let top = |x: usize| fb[20 * 256 + x];
        assert_eq!(top(3), 0x03E0, "px7 (color 2) lands at x=-4+7=3");
        assert_eq!(top(0), 0, "px4 is color 0 = transparent -> backdrop");
        // px0 (color 1) is at x=-4 -> clipped off-screen; nothing leaked.
        ppu.render_scanline(21, &mmu, &mut fb); // sprite row 1: solid color 3
        assert_eq!(fb[21 * 256 + 0], 0x7C00);
        assert_eq!(fb[21 * 256 + 3], 0x7C00);
        assert_eq!(fb[21 * 256 + 4], 0, "x=4 is past the visible tail");

        // H-flip: px0's color 1 now lands at the right edge (x=-4+7=3).
        mmu.oam[2..4].copy_from_slice(&(x | 0x1000).to_le_bytes());
        ppu.render_scanline(20, &mmu, &mut fb);
        assert_eq!(fb[20 * 256 + 3], 0x001F, "flipped px0 at x=3");

        // Disable bit hides the sprite entirely.
        mmu.oam[0..2].copy_from_slice(&(20u16 | 0x200).to_le_bytes());
        ppu.render_scanline(21, &mmu, &mut fb);
        assert_eq!(fb[21 * 256 + 0], 0, "disabled sprite renders nothing");
    }

    /// Affine (rotscale) sprites: an identity PA-PD matrix renders like the
    /// regular path, and the double-size flag pads the window so the texel
    /// area sits centered in it (U31 — the intro's growing-Lugia / Unown
    /// beats are rotscale OBJs that were skipped entirely).
    #[test]
    fn affine_obj_identity_and_double_size() {
        let mut mmu = NdsMmu::new();
        mmu.arm9_io[0] = 0x10; // OBJ 1D mapping
        mmu.arm9_io[1] = 0x11; // OBJ enable
        mmu.arm9_io[2] = 0x01; // display mode 1
        mmu.arm9_io[0x305] = 0x80; // engine A on top
        mmu.vram.banks[5].control = 0x82; // bank F = OBJ-A
        // Tile 1: rows 1+ solid color 3.
        for b in 36..64 {
            mmu.vram.banks[5].data[b] = 0x33;
        }
        mmu.palette_ram[0x200 + 6..0x200 + 8].copy_from_slice(&0x7C00u16.to_le_bytes());
        // Sprite 0: 8x8 at (100, 20), rotscale, param group 0, tile 1.
        mmu.oam[0..2].copy_from_slice(&(20u16 | 0x100).to_le_bytes());
        mmu.oam[2..4].copy_from_slice(&100u16.to_le_bytes());
        mmu.oam[4..6].copy_from_slice(&1u16.to_le_bytes());
        // Param group 0: identity (PA=PD=1.0, PB=PC=0).
        mmu.oam[6..8].copy_from_slice(&0x0100u16.to_le_bytes());
        mmu.oam[30..32].copy_from_slice(&0x0100u16.to_le_bytes());

        let ppu = NdsPpu::new();
        let mut fb = vec![0u16; 256 * 384];
        ppu.render_scanline(21, &mmu, &mut fb); // sprite row 1: solid
        assert_eq!(fb[21 * 256 + 100], 0x7C00, "identity affine == regular");
        assert_eq!(fb[21 * 256 + 107], 0x7C00);
        assert_eq!(fb[21 * 256 + 108], 0, "past the 8px window");

        // Double-size: 16x16 window, texels centered (visible in x 104..112).
        mmu.oam[0..2].copy_from_slice(&(20u16 | 0x300).to_le_bytes());
        ppu.render_scanline(29, &mmu, &mut fb); // dy=1 -> ty=5 (solid row)
        assert_eq!(fb[29 * 256 + 104], 0x7C00, "centered texel area");
        assert_eq!(fb[29 * 256 + 111], 0x7C00);
        assert_eq!(fb[29 * 256 + 102], 0, "window padding is transparent");
        assert_eq!(fb[29 * 256 + 112], 0, "right padding transparent");
    }

    /// OBJ-vs-OBJ: lower priority VALUE wins even from a higher OAM index;
    /// equal priorities keep the earlier index (U32 — the moon/Lugia bug).
    #[test]
    fn obj_overlap_resolves_by_priority_then_index() {
        let mut mmu = NdsMmu::new();
        mmu.arm9_io[0] = 0x10;
        mmu.arm9_io[1] = 0x11;
        mmu.arm9_io[2] = 0x01;
        mmu.arm9_io[0x305] = 0x80;
        mmu.vram.banks[5].control = 0x82;
        let f = &mut mmu.vram.banks[5].data;
        f[32..64].iter_mut().for_each(|b| *b = 0x11); // tile 1: color 1
        f[64..96].iter_mut().for_each(|b| *b = 0x22); // tile 2: color 2
        mmu.palette_ram[0x202..0x204].copy_from_slice(&0x001Fu16.to_le_bytes());
        mmu.palette_ram[0x204..0x206].copy_from_slice(&0x03E0u16.to_le_bytes());
        // Both sprites 8x8 at (50, 20): idx0 = tile1 prio1, idx1 = tile2 prio0.
        for (i, tile, prio) in [(0usize, 1u16, 1u16), (1, 2, 0)] {
            mmu.oam[i * 8..i * 8 + 2].copy_from_slice(&20u16.to_le_bytes());
            mmu.oam[i * 8 + 2..i * 8 + 4].copy_from_slice(&50u16.to_le_bytes());
            mmu.oam[i * 8 + 4..i * 8 + 6].copy_from_slice(&(tile | (prio << 10)).to_le_bytes());
        }
        let ppu = NdsPpu::new();
        let mut fb = vec![0u16; 256 * 384];
        ppu.render_scanline(20, &mmu, &mut fb);
        assert_eq!(fb[20 * 256 + 50], 0x03E0, "prio 0 beats lower index at prio 1");
        // Equal priorities: the earlier OAM entry keeps the pixel.
        mmu.oam[12..14].copy_from_slice(&(2u16 | (1 << 10)).to_le_bytes());
        ppu.render_scanline(20, &mmu, &mut fb);
        assert_eq!(fb[20 * 256 + 50], 0x001F, "equal prio -> lower index wins");
    }

    /// WIN0 gates layers per pixel (U31h — the Unown-swarm wipe): with WININ
    /// passing OBJ and WINOUT passing nothing, sprite pixels survive only
    /// inside the window rectangle.
    #[test]
    fn win0_masks_layers_outside_rectangle() {
        let mut mmu = NdsMmu::new();
        mmu.arm9_io[0] = 0x10;
        mmu.arm9_io[1] = 0x31; // OBJ enable + WIN0 enable (DISPCNT bit 13)
        mmu.arm9_io[2] = 0x01;
        mmu.arm9_io[0x305] = 0x80;
        mmu.vram.banks[5].control = 0x82;
        for b in 36..64 {
            mmu.vram.banks[5].data[b] = 0x33; // tile 1 rows 1+: color 3
        }
        mmu.palette_ram[0x200 + 6..0x200 + 8].copy_from_slice(&0x7C00u16.to_le_bytes());
        // Sprite 0: 8x8 at (0, 20), tile 1.
        mmu.oam[0..2].copy_from_slice(&20u16.to_le_bytes());
        mmu.oam[2..4].copy_from_slice(&0u16.to_le_bytes());
        mmu.oam[4..6].copy_from_slice(&1u16.to_le_bytes());
        // WIN0 rect: x in [2,6), full height. WININ0 = OBJ only; WINOUT = 0.
        mmu.arm9_io[0x40] = 6; // X2
        mmu.arm9_io[0x41] = 2; // X1
        mmu.arm9_io[0x44] = 192; // Y2
        mmu.arm9_io[0x45] = 0; // Y1
        mmu.arm9_io[0x48] = 0x10;
        mmu.arm9_io[0x4A] = 0x00;

        let ppu = NdsPpu::new();
        let mut fb = vec![0u16; 256 * 384];
        ppu.render_scanline(21, &mmu, &mut fb);
        assert_eq!(fb[21 * 256 + 1], 0, "outside WIN0: OBJ masked by WINOUT");
        assert_eq!(fb[21 * 256 + 2], 0x7C00, "inside WIN0: OBJ passes");
        assert_eq!(fb[21 * 256 + 5], 0x7C00, "inside WIN0 right edge");
        assert_eq!(fb[21 * 256 + 6], 0, "X2 is exclusive");
    }

    /// MASTER_BRIGHT is the intro's fade mechanism (U27 evidence strip):
    /// mode 2 factor 16 blacks the engine out, mode 1 whites it out, and
    /// factors in between scale each channel linearly. Also pins the BLDCNT
    /// alpha math: EVA=0/EVB=16 shows the pixel beneath, EVA=16/EVB=0 the
    /// top, 8/8 averages.
    #[test]
    fn master_bright_fades_and_alpha_blend_math() {
        let mut mmu = NdsMmu::new();
        mmu.arm9_io[0x305] = 0x80; // engine A on the top screen
        let ppu = NdsPpu::new();
        let mut fb = vec![0u16; 256 * 384];
        // Engine A display-off renders white; fade fully down = black.
        mmu.arm9_io[0x6C] = 0x10;
        mmu.arm9_io[0x6D] = 0x80; // mode 2 (toward black), factor 16
        ppu.render_scanline(0, &mmu, &mut fb);
        assert_eq!(fb[0], 0, "factor-16 fade-down = black");
        // Fade-up leaves white white.
        mmu.arm9_io[0x6D] = 0x40;
        ppu.render_scanline(0, &mmu, &mut fb);
        assert_eq!(fb[0], 0x7FFF);
        // Half fade-down of white: every 5-bit channel 31 -> 16.
        mmu.arm9_io[0x6C] = 0x08;
        mmu.arm9_io[0x6D] = 0x80;
        ppu.render_scanline(0, &mmu, &mut fb);
        assert_eq!(fb[0], 16 | (16 << 5) | (16 << 10));
        // Pure blend math.
        assert_eq!(NdsPpu::alpha_blend(0x7FFF, 0, 0, 16), 0);
        assert_eq!(NdsPpu::alpha_blend(0x7FFF, 0x1234, 16, 0), 0x7FFF);
        assert_eq!(
            NdsPpu::alpha_blend(0x7FFF, 0, 8, 8),
            15 | (15 << 5) | (15 << 10)
        );
        // Saturating: two whites at 16/16 clamp to white.
        assert_eq!(NdsPpu::alpha_blend(0x7FFF, 0x7FFF, 16, 16), 0x7FFF);
    }

    /// Layer bookkeeping end-to-end: with BLDCNT selecting alpha from OBJ
    /// (first target) onto the backdrop (second target) at EVA=0/EVB=16, an
    /// opaque sprite pixel must render as the BACKDROP color — the compose
    /// loop has to know both what the top layer is and what sat beneath it.
    #[test]
    fn bldcnt_alpha_blends_obj_over_backdrop() {
        let mut mmu = NdsMmu::new();
        mmu.arm9_io[0] = 0x10; // DISPCNT bit4: OBJ 1D mapping
        mmu.arm9_io[1] = 0x11; // BG0 on + OBJ enable
        mmu.arm9_io[2] = 0x01; // display mode 1 (graphics)
        mmu.arm9_io[0x305] = 0x80; // engine A on top
        mmu.vram.banks[5].control = 0x82; // bank F: MST 2 = OBJ-A
        // Tile 1, all pixels color 1 (4bpp).
        for b in 32..64 {
            mmu.vram.banks[5].data[b] = 0x11;
        }
        // OBJ palette color 1 = red; backdrop (BG palette 0) = green.
        let red: u16 = 0x001F;
        let green: u16 = 0x03E0;
        mmu.palette_ram[0x202] = (red & 0xFF) as u8;
        mmu.palette_ram[0x203] = (red >> 8) as u8;
        mmu.palette_ram[0] = (green & 0xFF) as u8;
        mmu.palette_ram[1] = (green >> 8) as u8;
        // OAM entry 0: 8x8 sprite at (10, 20), tile 1.
        mmu.oam[0] = 20; // y
        mmu.oam[2] = 10; // x
        mmu.oam[4] = 1; // tile
        let ppu = NdsPpu::new();
        let mut fb = vec![0u16; 256 * 384];
        ppu.render_scanline(20, &mmu, &mut fb);
        assert_eq!(fb[20 * 256 + 10], red, "no blend: sprite is opaque red");
        // BLDCNT: effect 1 (alpha), first = OBJ (bit4), second = BD (bit5);
        // EVA=0 EVB=16 -> the sprite pixel shows pure backdrop.
        mmu.arm9_io[0x50] = 0x50; // bits 4 (OBJ first) + 6 (effect 1)
        mmu.arm9_io[0x51] = 0x20; // bit 13 -> second target backdrop
        mmu.arm9_io[0x52] = 0x00; // EVA 0
        mmu.arm9_io[0x53] = 0x10; // EVB 16
        ppu.render_scanline(20, &mmu, &mut fb);
        assert_eq!(fb[20 * 256 + 10], green, "EVA=0/EVB=16 shows the layer beneath");
        assert_eq!(fb[20 * 256 + 0], green, "backdrop itself (not first target) untouched");
    }

    /// Engine A BG0 with DISPCNT bit3 presents the 3D rasterizer's
    /// framebuffer (bit15 = opaque) instead of skipping the layer.
    #[test]
    fn bg0_3d_presents_rasterizer_output() {
        let mut mmu = NdsMmu::new();
        mmu.arm9_io[0] = 0x08; // DISPCNT bit3: BG0 is the 3D output
        mmu.arm9_io[1] = 0x01; // BG0 enable
        mmu.arm9_io[2] = 0x01; // display mode 1 (graphics)
        mmu.arm9_io[0x305] = 0x80; // engine A on top
        mmu.gx.engine.fb[7 * 256 + 5] = 0x8000 | 0x001F;
        let ppu = NdsPpu::new();
        let mut fb = vec![0u16; 256 * 384];
        ppu.render_scanline(7, &mmu, &mut fb);
        assert_eq!(fb[7 * 256 + 5], 0x001F, "opaque 3D pixel presented");
        assert_eq!(fb[7 * 256 + 6], 0, "transparent 3D pixel shows backdrop");
    }
}
