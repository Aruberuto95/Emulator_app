pub struct GbaPpu {
    // Cycle timing
    pub cycle_accumulator: u32,
    /// Set on the VBlank edge (entering line 160) so the caller can present only complete
    /// frames (back->front copy), avoiding mid-frame tearing during fast transitions.
    pub frame_completed: bool,
}

/// One composited pixel, keeping the top TWO opaque contributors so the color-effect pass can
/// alpha-blend the 1st target (top) with the 2nd target (the layer directly below it).
///
/// `z` fully orders contributors (lower = closer to front): `z = priority*8 + subrank`, where
/// subrank is OBJ=0, BG0=1 .. BG3=4. That single key encodes both hardware tie-breaks — OBJ over
/// BG at equal priority, and lower BG index over higher — so insertion is a plain "keep the two
/// smallest z", independent of draw order. The backdrop seeds both slots with `z = 0xFFFF`.
#[derive(Clone, Copy)]
struct CompositorPixel {
    top_color: u16,     // BGR555
    top_z: u16,
    top_source: u8,     // 0=Backdrop, 1..4=BG0..BG3, 5=OBJ
    top_obj_semi: bool, // top pixel is a semi-transparent OBJ (forces alpha blending)
    snd_color: u16,
    snd_z: u16,
    snd_source: u8,
}

impl CompositorPixel {
    /// Insert a candidate, keeping the two smallest-z contributors. Order-independent: because
    /// each source has a unique z at a given pixel, the two front-most always survive.
    #[inline]
    fn insert(&mut self, color: u16, z: u16, source: u8, obj_semi: bool) {
        if z < self.top_z {
            self.snd_color = self.top_color;
            self.snd_z = self.top_z;
            self.snd_source = self.top_source;
            self.top_color = color;
            self.top_z = z;
            self.top_source = source;
            self.top_obj_semi = obj_semi;
        } else if z < self.snd_z {
            self.snd_color = color;
            self.snd_z = z;
            self.snd_source = source;
        }
    }
}

/// One resolved sprite pixel for a scanline (sprites resolve among themselves before entering the
/// z-ordered compositor as a single OBJ candidate per pixel).
#[derive(Clone, Copy, Default)]
struct ObjPixel {
    color: u16,
    priority: u8,
    semi: bool,    // OBJ Mode == 1 (semi-transparent)
    present: bool,
}

/// Per-scanline compositing context, read once per line and shared read-only by every layer
/// renderer (keeps the hot per-pixel loops free of register re-reads).
struct ScanCtx {
    /// Per-pixel window control. Layout matches WININ/WINOUT: bit0-4 = BG0-3/OBJ enable,
    /// bit5 = color-effect enable. When no window is active it is `0x3F` everywhere.
    win: [u8; 240],
    // Mosaic block sizes (1..16; 1 == no-op). BG mosaic gated per-BG by BGxCNT bit6,
    // OBJ mosaic gated per-sprite by ATTR0 bit12.
    bg_mos_h: u16,
    bg_mos_v: u16,
    obj_mos_h: u16,
    obj_mos_v: u16,
}

// Window control bits (shared by ScanCtx.win entries and WININ/WINOUT).
const WIN_OBJ_BIT: u8 = 1 << 4;
const WIN_EFFECT_BIT: u8 = 1 << 5;

impl GbaPpu {
    pub fn new() -> Self {
        Self {
            cycle_accumulator: 0,
            frame_completed: false,
        }
    }

    /// Cycles until the next observable PPU event (HBlank set at 960 or scanline
    /// end at 1232). Between these boundaries DISPSTAT/VCOUNT never change and no
    /// PPU interrupt can be raised, so the caller may batch component ticks up to
    /// this distance without altering any CPU-visible timing.
    pub fn cycles_to_next_boundary(&self) -> u32 {
        let target = if self.cycle_accumulator < 960 { 960 } else { 1232 };
        target - self.cycle_accumulator
    }

    pub fn tick(
        &mut self,
        cycles: u32,
        mmu: &mut crate::gba::mmu::GbaMmu,
        video_buffer: &mut [u16],
        is_render_tick: bool,
    ) {
        // Event-driven advance: the per-cycle loop below only ever did observable work when
        // `cycle_accumulator` hit exactly 960 (HBlank set) or reached 1232 (scanline end). On
        // every other cycle it merely incremented a private counter, so ~150k calls/frame paid a
        // per-cycle branch cost for nothing. We instead jump straight to the next boundary.
        //
        // Fast idle path: the incoming chunk stays entirely within the current segment and does
        // not touch either boundary -> just accumulate and return. This is the common case
        // (chunks are 1..8 cycles; boundaries are 960/1232 cycles apart).
        let next_boundary = if self.cycle_accumulator < 960 { 960 } else { 1232 };
        if self.cycle_accumulator + cycles < next_boundary {
            self.cycle_accumulator += cycles;
            return;
        }

        let mut remaining = cycles;
        while remaining > 0 {
            // Distance to the next event boundary from the current accumulator position.
            let target = if self.cycle_accumulator < 960 { 960 } else { 1232 };
            let step = (target - self.cycle_accumulator).min(remaining);
            self.cycle_accumulator += step;
            remaining -= step;

            if self.cycle_accumulator == 960 {
                let mut dispstat = mmu.read_halfword_safe(0x04000004);
                let was_hblank = (dispstat & 0x0002) != 0;
                dispstat |= 0x0002;
                if !was_hblank && (dispstat & 0x0010) != 0 {
                    mmu.trigger_interrupt(0x0002); // HBlank Int
                }
                mmu.write_halfword_safe(0x04000004, dispstat);
            }

            if self.cycle_accumulator >= 1232 {
                self.cycle_accumulator = 0;

                let mut dispstat = mmu.read_halfword_safe(0x04000004);
                let mut vcount = mmu.read_halfword_safe(0x04000006);

                vcount = (vcount + 1) % 228;

                // VBlank flag (bit 0 of dispstat). Entering line 160 is the frame edge.
                if vcount == 160 {
                    self.frame_completed = true;
                }
                if vcount >= 160 {
                    dispstat |= 0x0001;
                    // Trigger VBlank interrupt if enabled (bit 3 of dispstat)
                    if (dispstat & 0x0008) != 0 {
                        mmu.trigger_interrupt(0x0001); // VBlank Int
                    }
                } else {
                    dispstat &= !0x0001;
                }

                // VCounter Match flag (bit 2 of dispstat)
                let vcount_setting = (dispstat >> 8) & 0xFF;
                if vcount == vcount_setting {
                    dispstat |= 0x0004;
                    if (dispstat & 0x0020) != 0 {
                        mmu.trigger_interrupt(0x0004); // VCounter Int
                    }
                } else {
                    dispstat &= !0x0004;
                }

                // HBlank off
                dispstat &= !0x0002;

                mmu.write_halfword_safe(0x04000004, dispstat);
                mmu.set_vcount(vcount); // bypass the CPU read-only guard on 0x06/0x07

                // Render scanline if visible and frame is not skipped
                if vcount < 160 && is_render_tick {
                    self.render_scanline(vcount, mmu, video_buffer);
                }
            }
        }
    }

    fn render_scanline(&self, ly: u16, mmu: &crate::gba::mmu::GbaMmu, video_buffer: &mut [u16]) {
        let dispcnt = mmu.read_halfword_safe(0x04000000);
        // Single validated boundary for the whole scanline (BGR555 pixels): an
        // out-of-range `ly` panics here instead of silently corrupting memory.
        let line = &mut video_buffer[ly as usize * 240..ly as usize * 240 + 240];

        // Forced blank (DISPCNT bit 7): the GBA outputs a white screen. Honor it so the
        // previous frame's pixels don't linger during fades / scene loads.
        if (dispcnt & 0x0080) != 0 {
            line.fill(0x7FFF); // BGR555 white
            return;
        }

        // --- Per-scanline compositing registers (read ONCE; never per-pixel) ---
        let mosaic = mmu.read_halfword_safe(0x0400004C);
        let ctx = ScanCtx {
            win: self.build_window_mask(ly, mmu, dispcnt),
            bg_mos_h: (mosaic & 0xF) + 1,
            bg_mos_v: ((mosaic >> 4) & 0xF) + 1,
            obj_mos_h: ((mosaic >> 8) & 0xF) + 1,
            obj_mos_v: ((mosaic >> 12) & 0xF) + 1,
        };

        // Color special effects (BLDCNT/BLDALPHA/BLDY). Coefficients clamp to 16 in hardware.
        let bldcnt = mmu.read_halfword_safe(0x04000050);
        let effect_mode = ((bldcnt >> 6) & 3) as u8;
        let bldalpha = mmu.read_halfword_safe(0x04000052);
        let eva = (bldalpha & 0x1F).min(16) as u32;
        let evb = ((bldalpha >> 8) & 0x1F).min(16) as u32;
        let evy = (mmu.read_halfword_safe(0x04000054) & 0x1F).min(16) as u32;

        // Seed both compositor slots with the backdrop (BG palette index 0).
        let backdrop = mmu.read_palette_halfword(0);
        let mut scanline = [CompositorPixel {
            top_color: backdrop,
            top_z: 0xFFFF,
            top_source: 0,
            top_obj_semi: false,
            snd_color: backdrop,
            snd_z: 0xFFFF,
            snd_source: 0,
        }; 240];

        let mode = dispcnt & 7;

        // Dispatch BG rendering by DISPCNT mode. Text BGs use render_bg_layer, affine BGs use
        // render_affine_bg, bitmap modes carry the frame on BG2. All feed the z-ordered
        // compositor via CompositorPixel::insert, so priority ties resolve by the shared z key.
        match mode {
            0 => {
                for bg in 0..4 {
                    if (dispcnt & (1 << (8 + bg))) != 0 {
                        self.render_bg_layer(bg, ly, &mut scanline, mmu, &ctx);
                    }
                }
            }
            1 => {
                // BG0/BG1 text, BG2 affine.
                for bg in 0..2 {
                    if (dispcnt & (1 << (8 + bg))) != 0 {
                        self.render_bg_layer(bg, ly, &mut scanline, mmu, &ctx);
                    }
                }
                if (dispcnt & (1 << 10)) != 0 {
                    self.render_affine_bg(2, ly, &mut scanline, mmu, &ctx);
                }
            }
            2 => {
                // BG2/BG3 affine.
                for bg in [2usize, 3] {
                    if (dispcnt & (1 << (8 + bg))) != 0 {
                        self.render_affine_bg(bg, ly, &mut scanline, mmu, &ctx);
                    }
                }
            }
            3 => self.render_bitmap_mode3(ly, &mut scanline, mmu, dispcnt, &ctx),
            4 => self.render_bitmap_mode4(ly, &mut scanline, mmu, dispcnt, &ctx),
            5 => self.render_bitmap_mode5(ly, &mut scanline, mmu, dispcnt, &ctx),
            _ => {}
        }

        // Render OAM Sprites (OBJ)
        if (dispcnt & 0x1000) != 0 {
            self.render_sprites_layer(ly, &mut scanline, mmu, dispcnt, &ctx);
        }

        // Final compositing pass: apply color effects and store BGR555 directly.
        // `& 0x7FFF` forces bit 15 clear (games can set it in palette RAM; XBGR1555
        // ignores it, masking keeps dumps deterministic).
        for (x, out) in line.iter_mut().enumerate() {
            let px = &scanline[x];
            let effect_enabled = ctx.win[x] & WIN_EFFECT_BIT != 0;
            *out = apply_effect(px, bldcnt, effect_mode, eva, evb, evy, effect_enabled) & 0x7FFF;
        }
    }

    /// Builds the per-pixel window control mask for this scanline. When no window is enabled
    /// (DISPCNT bits 13/14/15 clear) every layer and color effect is on everywhere (`0x3F`).
    /// Region priority: WIN0 > WIN1 > OBJ window > outside (WINOUT).
    fn build_window_mask(&self, ly: u16, mmu: &crate::gba::mmu::GbaMmu, dispcnt: u16) -> [u8; 240] {
        let win0_en = dispcnt & 0x2000 != 0;
        let win1_en = dispcnt & 0x4000 != 0;
        let objwin_en = dispcnt & 0x8000 != 0;

        if !(win0_en || win1_en || objwin_en) {
            return [0x3F; 240];
        }

        let winin = mmu.read_halfword_safe(0x04000048);
        let winout = mmu.read_halfword_safe(0x0400004A);
        let win0_ctrl = (winin & 0x3F) as u8;
        let win1_ctrl = ((winin >> 8) & 0x3F) as u8;
        let out_ctrl = (winout & 0x3F) as u8;
        let objwin_ctrl = ((winout >> 8) & 0x3F) as u8;

        // WINxV: Y1 = high byte, Y2 = low byte (exclusive). Vertical span is per-line.
        let win0_active = win0_en && {
            let v = mmu.read_halfword_safe(0x04000044);
            in_range_1d(ly as u32, (v >> 8) as u32, (v & 0xFF) as u32)
        };
        let win1_active = win1_en && {
            let v = mmu.read_halfword_safe(0x04000046);
            in_range_1d(ly as u32, (v >> 8) as u32, (v & 0xFF) as u32)
        };

        // WINxH: X1 = high byte, X2 = low byte (exclusive).
        let h0 = mmu.read_halfword_safe(0x04000040);
        let (w0x1, w0x2) = ((h0 >> 8) as u32, (h0 & 0xFF) as u32);
        let h1 = mmu.read_halfword_safe(0x04000042);
        let (w1x1, w1x2) = ((h1 >> 8) as u32, (h1 & 0xFF) as u32);

        let objmask = if objwin_en {
            self.build_objwin_mask(ly, mmu, dispcnt)
        } else {
            [false; 240]
        };

        let mut mask = [out_ctrl; 240];
        for x in 0..240usize {
            mask[x] = if win0_active && in_range_1d(x as u32, w0x1, w0x2) {
                win0_ctrl
            } else if win1_active && in_range_1d(x as u32, w1x1, w1x2) {
                win1_ctrl
            } else if objwin_en && objmask[x] {
                objwin_ctrl
            } else {
                out_ctrl
            };
        }
        mask
    }

    /// Marks pixels covered by OBJ-window sprites (ATTR0 mode == 2). Those sprites are not drawn;
    /// their opaque pixels define the OBJ-window region.
    // ponytail: OBJ mosaic ignored for the window mask (mos=1,1) — mosaic-on-obj-window is a
    // vanishingly rare combo; wire real sizes here if a game ever needs it.
    fn build_objwin_mask(&self, ly: u16, mmu: &crate::gba::mmu::GbaMmu, dispcnt: u16) -> [bool; 240] {
        let mut mask = [false; 240];
        for idx in (0..128u32).rev() {
            let oam_addr = idx * 8;
            let attr0 = mmu.read_oam_halfword(oam_addr);
            if (attr0 >> 10) & 3 != 2 {
                continue;
            }
            self.for_each_sprite_pixel(mmu, ly, oam_addr, dispcnt, 1, 1, |x, _color| {
                mask[x] = true;
            });
        }
        mask
    }

    fn render_bg_layer(
        &self,
        bg: usize,
        ly: u16,
        scanline: &mut [CompositorPixel; 240],
        mmu: &crate::gba::mmu::GbaMmu,
        ctx: &ScanCtx,
    ) {
        let control = mmu.read_halfword_safe(0x04000008 + bg as u32 * 2);
        let priority = (control & 3) as u8;

        // Screen/Character base blocks
        let char_base = ((control >> 2) & 3) as u32 * 16384;
        let screen_base = ((control >> 8) & 31) as u32 * 2048;
        let is_8bpp = (control & 0x0080) != 0;
        let size = (control >> 14) & 3;

        let hofs = mmu.read_halfword_safe(0x04000010 + bg as u32 * 4);
        let vofs = mmu.read_halfword_safe(0x04000012 + bg as u32 * 4);

        // Screen size dimensions in pixels
        let (map_w, map_h) = match size {
            0 => (256, 256),
            1 => (512, 256),
            2 => (256, 512),
            _ => (512, 512),
        };

        let win_bit = 1u8 << bg; // BG0..BG3 -> window bits 0..3
        let z = layer_z(priority, (bg + 1) as u8);
        let source = (bg + 1) as u8;

        // BG mosaic (BGxCNT bit6): snap the sampled coordinate to mosaic blocks; the scroll
        // offset is applied AFTER the snap. mos=1 is a no-op.
        let (mh, mv) = if control & 0x40 != 0 {
            (ctx.bg_mos_h, ctx.bg_mos_v)
        } else {
            (1, 1)
        };
        let ly_eff = ly - ly % mv;

        let y_map = (ly_eff.wrapping_add(vofs)) % map_h;
        let tile_y = y_map / 8;
        let py = y_map % 8;

        for x in 0..240usize {
            if ctx.win[x] & win_bit == 0 {
                continue;
            }

            let x_eff = x as u16 - (x as u16) % mh;
            let x_map = (x_eff.wrapping_add(hofs)) % map_w;
            let tile_x = x_map / 8;
            let px = x_map % 8;

            // Find map address block offset
            let block_offset = match size {
                1 => {
                    // 64x32
                    if tile_x >= 32 {
                        2048 + (tile_x - 32) * 2
                    } else {
                        tile_x * 2
                    }
                }
                2 => {
                    // 32x64
                    if tile_y >= 32 {
                        2048 + tile_x * 2
                    } else {
                        tile_x * 2
                    }
                }
                3 => {
                    // 64x64
                    match (tile_x >= 32, tile_y >= 32) {
                        (false, false) => tile_x * 2,
                        (true, false) => 2048 + (tile_x - 32) * 2,
                        (false, true) => 4096 + tile_x * 2,
                        (true, true) => 6144 + (tile_x - 32) * 2,
                    }
                }
                _ => tile_x * 2, // 32x32
            };

            let row_offset = (tile_y % 32) * 64;
            let map_addr = screen_base + (row_offset as u32) + (block_offset as u32);

            // Read screen map entry (16-bit)
            let entry = mmu.read_vram_halfword(map_addr);
            let tile_idx = entry & 0x03FF;
            let hflip = (entry & 0x0400) != 0;
            let vflip = (entry & 0x0800) != 0;
            let palette_idx = ((entry >> 12) & 0x0F) as u8;

            let final_px = if hflip { 7 - px } else { px };
            let final_py = if vflip { 7 - py } else { py };

            // Fetch color index
            let color_idx = if is_8bpp {
                let tile_offset = tile_idx as u32 * 64;
                let pixel_addr = char_base + tile_offset + (final_py as u32 * 8) + final_px as u32;
                mmu.read_vram_byte(pixel_addr)
            } else {
                let tile_offset = tile_idx as u32 * 32;
                let pixel_addr =
                    char_base + tile_offset + (final_py as u32 * 4) + (final_px as u32 / 2);
                let byte = mmu.read_vram_byte(pixel_addr);
                if final_px % 2 == 0 {
                    byte & 0x0F
                } else {
                    byte >> 4
                }
            };

            if color_idx != 0 {
                let color_addr = if is_8bpp {
                    color_idx as u32 * 2
                } else {
                    (palette_idx as u32 * 16 + color_idx as u32) * 2
                };
                let bgr555 = mmu.read_palette_halfword(color_addr);
                scanline[x].insert(bgr555, z, source, false);
            }
        }
    }

    fn render_sprites_layer(
        &self,
        ly: u16,
        scanline: &mut [CompositorPixel; 240],
        mmu: &crate::gba::mmu::GbaMmu,
        dispcnt: u16,
        ctx: &ScanCtx,
    ) {
        // Resolve sprites among themselves first: reverse OAM order + "priority <= existing" so a
        // lower OAM index wins ties and a lower priority number wins outright.
        let mut obj = [ObjPixel::default(); 240];
        for idx in (0..128u32).rev() {
            let oam_addr = idx * 8;
            let attr0 = mmu.read_oam_halfword(oam_addr);
            let mode = (attr0 >> 10) & 3;
            if mode == 2 {
                continue; // OBJ-window sprites define a mask, they are not drawn
            }
            let attr2 = mmu.read_oam_halfword(oam_addr + 4);
            let priority = ((attr2 >> 10) & 3) as u8;
            let semi = mode == 1;
            let (mh, mv) = if attr0 & 0x1000 != 0 {
                (ctx.obj_mos_h, ctx.obj_mos_v)
            } else {
                (1, 1)
            };

            self.for_each_sprite_pixel(mmu, ly, oam_addr, dispcnt, mh, mv, |x, color| {
                let p = &mut obj[x];
                if !p.present || priority <= p.priority {
                    p.present = true;
                    p.color = color;
                    p.priority = priority;
                    p.semi = semi;
                }
            });
        }

        // Insert the resolved OBJ layer into the compositor, gated by the window OBJ bit.
        for x in 0..240 {
            let p = &obj[x];
            if p.present && ctx.win[x] & WIN_OBJ_BIT != 0 {
                scanline[x].insert(p.color, layer_z(p.priority, 5), 5, p.semi);
            }
        }
    }

    /// Fetches one sprite texel at texture coords `(tex_x, tex_y)` inside a `w`x`h` sprite,
    /// returning its BGR555 color, or `None` when the palette index is 0 (transparent). Holds the
    /// tile-mapping (1D vs 2D), bit-depth (4bpp/8bpp) and palette logic in one place so the affine
    /// and non-affine sprite walks stay in sync. Caller guarantees `tex_x < w` and `tex_y < h`.
    #[allow(clippy::too_many_arguments)]
    fn sample_sprite_texel(
        &self,
        mmu: &crate::gba::mmu::GbaMmu,
        tile_start: u16,
        w: u16,
        is_8bpp: bool,
        is_1d_mapping: bool,
        palette_idx: u8,
        tex_x: u16,
        tex_y: u16,
    ) -> Option<u16> {
        let tile_px = tex_x % 8;
        let tile_py = tex_y % 8;
        let grid_x = tex_x / 8;
        let grid_y = tex_y / 8;

        // Tile-number offset within the sprite's tile block. 1D: tiles are linear, so the row
        // stride is `tiles_per_row` (doubled for 8bpp, whose tiles span two numbers). 2D: the char
        // grid is a fixed 32 tile-numbers wide regardless of depth — only the horizontal step
        // doubles for 8bpp, the vertical stride stays 32.
        let tile_offset = if is_1d_mapping {
            let tiles_per_row = w / 8;
            if is_8bpp {
                (grid_y * tiles_per_row * 2 + grid_x * 2) as u32
            } else {
                (grid_y * tiles_per_row + grid_x) as u32
            }
        } else if is_8bpp {
            (grid_y * 32 + grid_x * 2) as u32
        } else {
            (grid_y * 32 + grid_x) as u32
        };

        let cur_tile = tile_start as u32 + tile_offset;

        let pixel_idx = if is_8bpp {
            let tile_addr = 0x10000 + cur_tile * 32 + (tile_py as u32 * 8) + tile_px as u32;
            mmu.read_vram_byte(tile_addr)
        } else {
            let tile_addr = 0x10000 + cur_tile * 32 + (tile_py as u32 * 4) + (tile_px as u32 / 2);
            let byte = mmu.read_vram_byte(tile_addr);
            if tile_px % 2 == 0 {
                byte & 0x0F
            } else {
                byte >> 4
            }
        };

        if pixel_idx == 0 {
            return None; // palette index 0 is transparent
        }
        let palette_offset = if is_8bpp {
            pixel_idx as u32 * 2
        } else {
            (palette_idx as u32 * 16 + pixel_idx as u32) * 2
        };
        // Sprite palette RAM starts at 0x200.
        Some(mmu.read_palette_halfword(0x200 + palette_offset))
    }

    /// Walks the opaque pixels of one sprite on scanline `ly`, calling `plot(screen_x, bgr555)`
    /// for each. Shared by the visible-sprite pass and the OBJ-window mask pass. OBJ mosaic snaps
    /// the sampled texture coordinate (screen position is unchanged); `mos=1` is a no-op.
    ///
    /// Rotation/scaling sprites (ATTR0 bit8) are sampled through their PA-PD affine matrix; for
    /// those sprites ATTR1 bits 12/13 are the parameter-group index, NOT H/V flip, so flip is
    /// applied only on the non-affine path.
    fn for_each_sprite_pixel(
        &self,
        mmu: &crate::gba::mmu::GbaMmu,
        ly: u16,
        oam_addr: u32,
        dispcnt: u16,
        mos_h: u16,
        mos_v: u16,
        mut plot: impl FnMut(usize, u16),
    ) {
        let attr0 = mmu.read_oam_halfword(oam_addr);
        let attr1 = mmu.read_oam_halfword(oam_addr + 2);
        let attr2 = mmu.read_oam_halfword(oam_addr + 4);

        // bit8 = rot/scale flag. For non-rot/scale sprites bit9 = disable; for rot/scale sprites
        // bit9 = double-size (a larger on-screen bounding box, never a disable).
        let rotscale = attr0 & 0x0100 != 0;
        if !rotscale && attr0 & 0x0200 != 0 {
            return;
        }

        // Decode content dimensions (the sprite's texture size in pixels).
        let shape = (attr0 >> 14) & 3;
        let size = (attr1 >> 14) & 3;
        let (w, h) = match (shape, size) {
            // Square
            (0, 0) => (8u16, 8u16),
            (0, 1) => (16, 16),
            (0, 2) => (32, 32),
            (0, 3) => (64, 64),
            // Horizontal
            (1, 0) => (16, 8),
            (1, 1) => (32, 8),
            (1, 2) => (32, 16),
            (1, 3) => (64, 32),
            // Vertical
            (2, 0) => (8, 16),
            (2, 1) => (8, 32),
            (2, 2) => (16, 32),
            (2, 3) => (32, 64),
            _ => (8, 8),
        };

        // Sprite top-left in signed screen space (ATTR0 Y is R0-255, ATTR1 X is R0-511).
        let mut y = (attr0 & 0x00FF) as i16;
        if y >= 160 {
            y -= 256;
        }
        let mut x_pos = (attr1 & 0x01FF) as i16;
        if x_pos >= 256 {
            x_pos -= 512;
        }

        let is_8bpp = (attr0 & 0x2000) != 0;
        let tile_start = attr2 & 0x03FF;
        let palette_idx = ((attr2 >> 12) & 0x0F) as u8;
        let is_1d_mapping = (dispcnt & 0x0040) != 0;
        let ly_i = ly as i16;

        if rotscale {
            // --- Affine (rotation/scaling) sprite ---
            // Double-size (ATTR0 bit9) doubles the on-screen bounding box; the texture stays w x h.
            let double = attr0 & 0x0200 != 0;
            let (bw, bh) = if double { (w * 2, h * 2) } else { (w, h) };

            // Vertical overlap tests the bounding box, not the texture height.
            if ly_i < y || ly_i >= y + bh as i16 {
                return;
            }
            let iy = (ly_i - y) as i32; // row within the bounding box, 0..bh

            // Affine matrix: group index = ATTR1 bits 9-13. Each s16 8.8 parameter is the 4th
            // halfword of one of the group's four 8-byte OAM slots (byte offsets +6/+14/+22/+30).
            let group = ((attr1 >> 9) & 0x1F) as u32;
            let base = group * 32;
            let pa = mmu.read_oam_halfword(base + 0x06) as i16 as i32;
            let pb = mmu.read_oam_halfword(base + 0x0E) as i16 as i32;
            let pc = mmu.read_oam_halfword(base + 0x16) as i16 as i32;
            let pd = mmu.read_oam_halfword(base + 0x1E) as i16 as i32;

            let hw = (w / 2) as i32; // texture center
            let hh = (h / 2) as i32;
            let cx = (bw / 2) as i32; // bounding-box center
            let cy = (bh / 2) as i32;
            let w_i = w as i32;
            let h_i = h as i32;

            // OBJ mosaic snaps the sampled row once before transforming (screen row unchanged).
            let iy_s = iy - iy % mos_v as i32;
            let dy = iy_s - cy;

            for ix in 0..bw as i32 {
                let sx = x_pos as i32 + ix;
                if sx < 0 || sx >= 240 {
                    continue;
                }
                let ix_s = ix - ix % mos_h as i32;
                let dx = ix_s - cx;

                // texcoord = P * (screen_offset - bbox_center) + tex_center, P in 8.8 fixed point.
                let tx = ((pa * dx + pb * dy) >> 8) + hw;
                let ty = ((pc * dx + pd * dy) >> 8) + hh;
                if tx < 0 || tx >= w_i || ty < 0 || ty >= h_i {
                    continue; // sampled outside the texture -> transparent
                }

                if let Some(color) = self.sample_sprite_texel(
                    mmu, tile_start, w, is_8bpp, is_1d_mapping, palette_idx, tx as u16, ty as u16,
                ) {
                    plot(sx as usize, color);
                }
            }
            return;
        }

        // --- Regular sprite (hardware H/V flip; no matrix) ---
        if ly_i < y || ly_i >= y + h as i16 {
            return;
        }
        let py = (ly_i - y) as u16;
        let hflip = (attr1 & 0x1000) != 0;
        let vflip = (attr1 & 0x2000) != 0;

        // OBJ mosaic: snap the vertical sample once per sprite.
        let py_s = py - py % mos_v;
        let final_py = if vflip { h - 1 - py_s } else { py_s };

        for px in 0..w {
            let sx = x_pos + px as i16;
            if sx < 0 || sx >= 240 {
                continue;
            }

            // OBJ mosaic horizontal snap of the sampled column (screen position sx unchanged).
            let px_s = px - px % mos_h;
            let final_px = if hflip { w - 1 - px_s } else { px_s };

            if let Some(color) = self.sample_sprite_texel(
                mmu, tile_start, w, is_8bpp, is_1d_mapping, palette_idx, final_px, final_py,
            ) {
                plot(sx as usize, color);
            }
        }
    }

    /// Renders an affine (rotation/scaling) background layer (BG2/BG3 in modes 1/2).
    /// Affine BGs are always 8bpp with a 1-byte-per-tile map. The reference point and
    /// PA-PD matrix are read once per scanline; texture coords are derived per pixel.
    fn render_affine_bg(
        &self,
        bg: usize,
        ly: u16,
        scanline: &mut [CompositorPixel; 240],
        mmu: &crate::gba::mmu::GbaMmu,
        ctx: &ScanCtx,
    ) {
        let control = mmu.read_halfword_safe(0x04000008 + bg as u32 * 2);
        let priority = (control & 3) as u8;
        let char_base = ((control >> 2) & 3) as u32 * 16384;
        let screen_base = ((control >> 8) & 31) as u32 * 2048;
        let wrap = (control & 0x2000) != 0; // bit13: display-area overflow -> wrap
        let size_px: i32 = match (control >> 14) & 3 {
            0 => 128,
            1 => 256,
            2 => 512,
            _ => 1024,
        };
        let tiles_per_row = (size_px / 8) as u32;

        let win_bit = 1u8 << bg; // BG2/BG3 -> window bits 2/3
        let z = layer_z(priority, (bg + 1) as u8);
        let source = (bg + 1) as u8;

        // BG mosaic: snap the screen coordinates fed into the affine transform.
        let (mh, mv) = if control & 0x40 != 0 {
            (ctx.bg_mos_h as i32, ctx.bg_mos_v as i32)
        } else {
            (1, 1)
        };

        // Affine params: BG2 at 0x20, BG3 at 0x30. PA-PD are i16 8.8 fixed-point;
        // the reference point is 28-bit signed with an 8-bit fraction.
        let base = if bg == 2 { 0x04000020 } else { 0x04000030 };
        let pa = mmu.read_halfword_safe(base) as i16 as i32;
        let pb = mmu.read_halfword_safe(base + 2) as i16 as i32;
        let pc = mmu.read_halfword_safe(base + 4) as i16 as i32;
        let pd = mmu.read_halfword_safe(base + 6) as i16 as i32;
        let ref_x = sign_extend_28(mmu.read_word_safe(base + 8));
        let ref_y = sign_extend_28(mmu.read_word_safe(base + 12));

        let ly_i = ly as i32;
        let ly_eff = ly_i - ly_i % mv;
        for x in 0..240i32 {
            if ctx.win[x as usize] & win_bit == 0 {
                continue;
            }
            let x_eff = x - x % mh;

            let mut tx = (pa * x_eff + pb * ly_eff + ref_x) >> 8;
            let mut ty = (pc * x_eff + pd * ly_eff + ref_y) >> 8;

            if tx < 0 || tx >= size_px || ty < 0 || ty >= size_px {
                if wrap {
                    tx = tx.rem_euclid(size_px);
                    ty = ty.rem_euclid(size_px);
                } else {
                    continue; // outside the map and no wrap -> transparent
                }
            }

            let tile_x = (tx / 8) as u32;
            let tile_y = (ty / 8) as u32;
            let tile_idx = mmu.read_vram_byte(screen_base + tile_y * tiles_per_row + tile_x) as u32;
            let px = (tx % 8) as u32;
            let py = (ty % 8) as u32;
            let color_idx = mmu.read_vram_byte(char_base + tile_idx * 64 + py * 8 + px);

            if color_idx != 0 {
                let bgr555 = mmu.read_palette_halfword(color_idx as u32 * 2);
                scanline[x as usize].insert(bgr555, z, source, false);
            }
        }
    }

    /// Mode 3: single 16bpp (BGR555) frame, 240x160, no paging. Every pixel is opaque.
    fn render_bitmap_mode3(
        &self,
        ly: u16,
        scanline: &mut [CompositorPixel; 240],
        mmu: &crate::gba::mmu::GbaMmu,
        dispcnt: u16,
        ctx: &ScanCtx,
    ) {
        if (dispcnt & (1 << 10)) == 0 {
            return; // BG2 carries the bitmap
        }
        let priority = (mmu.read_halfword_safe(0x0400000C) & 3) as u8;
        let z = layer_z(priority, 3);
        let row = ly as u32 * 240;
        for x in 0..240usize {
            if ctx.win[x] & 0x04 == 0 {
                continue; // BG2 window bit
            }
            let color = mmu.read_vram_halfword((row + x as u32) * 2);
            scanline[x].insert(color, z, 3, false);
        }
    }

    /// Mode 4: 8bpp paletted, 240x160, page-flipped via DISPCNT bit4. Index 0 is transparent.
    fn render_bitmap_mode4(
        &self,
        ly: u16,
        scanline: &mut [CompositorPixel; 240],
        mmu: &crate::gba::mmu::GbaMmu,
        dispcnt: u16,
        ctx: &ScanCtx,
    ) {
        if (dispcnt & (1 << 10)) == 0 {
            return;
        }
        let priority = (mmu.read_halfword_safe(0x0400000C) & 3) as u8;
        let z = layer_z(priority, 3);
        let page: u32 = if (dispcnt & 0x10) != 0 { 0xA000 } else { 0 };
        let row = ly as u32 * 240;
        for x in 0..240usize {
            if ctx.win[x] & 0x04 == 0 {
                continue;
            }
            let idx = mmu.read_vram_byte(page + row + x as u32);
            if idx != 0 {
                let color = mmu.read_palette_halfword(idx as u32 * 2);
                scanline[x].insert(color, z, 3, false);
            }
        }
    }

    /// Mode 5: 16bpp (BGR555), 160x128, page-flipped via DISPCNT bit4. Pixels outside the
    /// 160x128 region keep the backdrop.
    fn render_bitmap_mode5(
        &self,
        ly: u16,
        scanline: &mut [CompositorPixel; 240],
        mmu: &crate::gba::mmu::GbaMmu,
        dispcnt: u16,
        ctx: &ScanCtx,
    ) {
        if (dispcnt & (1 << 10)) == 0 || ly >= 128 {
            return;
        }
        let priority = (mmu.read_halfword_safe(0x0400000C) & 3) as u8;
        let z = layer_z(priority, 3);
        let page: u32 = if (dispcnt & 0x10) != 0 { 0xA000 } else { 0 };
        let row = ly as u32 * 160;
        for x in 0..160usize {
            if ctx.win[x] & 0x04 == 0 {
                continue;
            }
            let color = mmu.read_vram_halfword(page + (row + x as u32) * 2);
            scanline[x].insert(color, z, 3, false);
        }
    }
}

/// Sign-extends a 28-bit two's-complement value (GBA affine reference point) to i32.
fn sign_extend_28(v: u32) -> i32 {
    ((v << 4) as i32) >> 4
}

/// z-order key: lower is closer to the front. `priority*8 + subrank`, where subrank places OBJ
/// above BGs and lower BG index above higher at equal priority. Backdrop is seeded separately.
#[inline]
fn layer_z(priority: u8, source: u8) -> u16 {
    let subrank = if source == 5 { 0 } else { source as u16 }; // OBJ=0, BG0..3=1..4
    (priority as u16) * 8 + subrank
}

/// Maps a compositor source to its BLDCNT target bit (backdrop=bit5, OBJ=bit4, BGn=bit n-1).
/// Used for both the 1st-target field (low 6 bits) and 2nd-target field (bits 8-13, pre-shifted
/// by the caller).
#[inline]
fn bldcnt_target_bit(source: u8) -> u16 {
    match source {
        0 => 1 << 5,        // backdrop
        5 => 1 << 4,        // OBJ
        s => 1 << (s - 1),  // BG0..BG3 -> bit0..bit3
    }
}

/// Alpha blend two BGR555 colors per channel: `min(31, chA*eva/16 + chB*evb/16)`.
#[inline]
fn blend_alpha(a: u16, b: u16, eva: u32, evb: u32) -> u16 {
    let ar = (a & 0x1F) as u32;
    let ag = ((a >> 5) & 0x1F) as u32;
    let ab = ((a >> 10) & 0x1F) as u32;
    let br = (b & 0x1F) as u32;
    let bg = ((b >> 5) & 0x1F) as u32;
    let bb = ((b >> 10) & 0x1F) as u32;
    let r = (((ar * eva + br * evb) >> 4).min(31)) as u16;
    let g = (((ag * eva + bg * evb) >> 4).min(31)) as u16;
    let bl = (((ab * eva + bb * evb) >> 4).min(31)) as u16;
    r | (g << 5) | (bl << 10)
}

/// Brighten toward white by evy/16 per channel: `ch + (31-ch)*evy/16`.
#[inline]
fn blend_brighten(a: u16, evy: u32) -> u16 {
    let f = |c: u32| (c + (((31 - c) * evy) >> 4)) as u16;
    let r = (a & 0x1F) as u32;
    let g = ((a >> 5) & 0x1F) as u32;
    let b = ((a >> 10) & 0x1F) as u32;
    f(r) | (f(g) << 5) | (f(b) << 10)
}

/// Darken toward black by evy/16 per channel: `ch - ch*evy/16`.
#[inline]
fn blend_darken(a: u16, evy: u32) -> u16 {
    let f = |c: u32| (c - ((c * evy) >> 4)) as u16;
    let r = (a & 0x1F) as u32;
    let g = ((a >> 5) & 0x1F) as u32;
    let b = ((a >> 10) & 0x1F) as u32;
    f(r) | (f(g) << 5) | (f(b) << 10)
}

/// Wrap-aware 1D range test used for window horizontal/vertical spans. `c2` is exclusive.
/// When `c1 > c2` the range wraps (set at c1, still set through the line end). `c2 >= edge`
/// naturally covers to the screen edge.
#[inline]
fn in_range_1d(coord: u32, c1: u32, c2: u32) -> bool {
    if c1 <= c2 {
        coord >= c1 && coord < c2
    } else {
        coord >= c1 || coord < c2
    }
}

/// Applies the color special effect to a composited pixel, returning the final BGR555 color.
/// Semi-transparent OBJ forces alpha blending (ignoring BLDCNT mode) when the layer below is a
/// 2nd target. Otherwise BLDCNT mode drives: 1=alpha, 2=brighten, 3=darken. All effects require
/// the per-pixel window color-effect flag.
#[inline]
fn apply_effect(
    px: &CompositorPixel,
    bldcnt: u16,
    mode: u8,
    eva: u32,
    evb: u32,
    evy: u32,
    effect_enabled: bool,
) -> u16 {
    // Semi-transparent OBJ: always alpha-blend with the 2nd target (if it is a B-target),
    // regardless of the BLDCNT effect mode.
    if px.top_obj_semi {
        if effect_enabled && (bldcnt >> 8) & bldcnt_target_bit(px.snd_source) != 0 {
            return blend_alpha(px.top_color, px.snd_color, eva, evb);
        }
        return px.top_color;
    }

    if !effect_enabled || mode == 0 {
        return px.top_color;
    }

    // Effects apply only when the top pixel is a 1st (A) target.
    if bldcnt & bldcnt_target_bit(px.top_source) == 0 {
        return px.top_color;
    }

    match mode {
        1 => {
            // Alpha: also requires the 2nd pixel to be a B target.
            if (bldcnt >> 8) & bldcnt_target_bit(px.snd_source) != 0 {
                blend_alpha(px.top_color, px.snd_color, eva, evb)
            } else {
                px.top_color
            }
        }
        2 => blend_brighten(px.top_color, evy),
        3 => blend_darken(px.top_color, evy),
        _ => px.top_color,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gba::mmu::GbaMmu;

    #[test]
    fn mode3_renders_pixel_from_vram() {
        let mut mmu = GbaMmu::new(vec![]);
        // DISPCNT: mode 3 (bits 0-2 = 3) with BG2 enabled (bit 10).
        mmu.write_halfword_safe(0x04000000, 0x0400 | 0x0003);
        // Red BGR555 (0x001F) at frame pixel (0,0).
        mmu.write_halfword_safe(0x06000000, 0x001F);

        let ppu = GbaPpu::new();
        let mut buf = vec![0u16; 240 * 160];
        ppu.render_scanline(0, &mmu, &mut buf);

        // Native BGR555 passthrough: red stays 0x001F.
        assert_eq!(buf[0], 0x001F);
    }

    #[test]
    fn sign_extend_28_handles_negative() {
        assert_eq!(sign_extend_28(0x0FFF_FFFF), -1); // all 28 bits set -> -1
        assert_eq!(sign_extend_28(0x0000_0001), 1);
    }

    #[test]
    fn layer_z_orders_obj_over_bg_and_low_index_over_high() {
        // Same priority: OBJ above BG0 above BG1; different priority dominates.
        assert!(layer_z(0, 5) < layer_z(0, 1)); // OBJ over BG0
        assert!(layer_z(0, 1) < layer_z(0, 2)); // BG0 over BG1
        assert!(layer_z(0, 4) < layer_z(1, 5)); // priority 0 beats priority 1 regardless
    }

    #[test]
    fn blend_alpha_clamps_and_mixes() {
        // Max red A + max red B, full coefficients -> clamp to 31.
        assert_eq!(blend_alpha(0x001F, 0x001F, 16, 16) & 0x1F, 31);
        // Half of white (eva=8) alone -> 15 per channel.
        let half = blend_alpha(0x7FFF, 0x0000, 8, 0);
        assert_eq!(half, 15 | (15 << 5) | (15 << 10));
    }

    #[test]
    fn blend_brighten_darken_endpoints() {
        let white = 0x7FFF;
        assert_eq!(blend_brighten(0x0000, 16), white); // black -> white at full evy
        assert_eq!(blend_brighten(white, 0), white); // evy 0 is a no-op
        assert_eq!(blend_darken(white, 16), 0x0000); // white -> black at full evy
        assert_eq!(blend_darken(white, 0), white); // evy 0 is a no-op
    }

    #[test]
    fn in_range_1d_normal_wrap_edge() {
        // Normal, exclusive upper bound.
        assert!(in_range_1d(5, 0, 10));
        assert!(!in_range_1d(10, 0, 10));
        assert!(!in_range_1d(5, 10, 20));
        // Wrap (c1 > c2).
        assert!(in_range_1d(250, 240, 5));
        assert!(in_range_1d(2, 240, 5));
        assert!(!in_range_1d(100, 240, 5));
        // c2 beyond the visible edge covers to the right edge.
        assert!(in_range_1d(239, 0, 255));
    }

    #[test]
    fn apply_effect_alpha_blends_ab_targets() {
        let px = CompositorPixel {
            top_color: 0x001F, // BG0, red
            top_z: layer_z(0, 1),
            top_source: 1,
            top_obj_semi: false,
            snd_color: 0x7C00, // BG1, blue
            snd_z: layer_z(0, 2),
            snd_source: 2,
        };
        // Alpha mode, A = BG0 (bit0), B = BG1 (bit9).
        let bldcnt = (1u16 << 6) | (1 << 0) | (1 << 9);
        let out = apply_effect(&px, bldcnt, 1, 8, 8, 0, true);
        assert_eq!(out, blend_alpha(0x001F, 0x7C00, 8, 8));
        // Window effect flag off -> passthrough of the top pixel.
        assert_eq!(apply_effect(&px, bldcnt, 1, 8, 8, 0, false), 0x001F);
        // B not a target -> no blend, top passes through.
        let bldcnt_no_b = (1u16 << 6) | (1 << 0);
        assert_eq!(apply_effect(&px, bldcnt_no_b, 1, 8, 8, 0, true), 0x001F);
    }

    #[test]
    fn apply_effect_semi_transparent_obj_forces_alpha() {
        let px = CompositorPixel {
            top_color: 0x001F,
            top_z: layer_z(0, 5),
            top_source: 5, // OBJ
            top_obj_semi: true,
            snd_color: 0x7C00,
            snd_z: layer_z(1, 1),
            snd_source: 1, // BG0
        };
        // BLDCNT mode is "none" (0) but OBJ-semi still blends when 2nd is a B target (BG0=bit8).
        let bldcnt = 1u16 << 8;
        assert_eq!(
            apply_effect(&px, bldcnt, 0, 8, 8, 0, true),
            blend_alpha(0x001F, 0x7C00, 8, 8)
        );
    }

    // --- Affine (rotation/scaling) sprite rendering -------------------------

    /// Fresh MMU with an 8x8 4bpp OBJ tile (tile 0) whose texel palette-index = `f(x, y)`, plus a
    /// distinct nonzero OBJ palette entry per index. Tile 0 lives at OBJ VRAM offset 0x10000.
    fn mmu_with_obj_tile(f: impl Fn(u16, u16) -> u8) -> GbaMmu {
        let mut mmu = GbaMmu::new(vec![]);
        for idx in 1u32..16 {
            mmu.write_halfword_safe(0x05000000 + 0x200 + idx * 2, 0x8000 | idx as u16);
        }
        for ty in 0u16..8 {
            for pair in 0u16..2 {
                let x0 = pair * 4;
                let b0 = (f(x0, ty) & 0x0F) | ((f(x0 + 1, ty) & 0x0F) << 4);
                let b1 = (f(x0 + 2, ty) & 0x0F) | ((f(x0 + 3, ty) & 0x0F) << 4);
                let hw = b0 as u16 | ((b1 as u16) << 8);
                mmu.write_halfword_safe(0x06010000 + ty as u32 * 4 + pair as u32 * 2, hw);
            }
        }
        mmu
    }

    fn set_oam(mmu: &mut GbaMmu, a0: u16, a1: u16, a2: u16) {
        mmu.write_halfword_safe(0x07000000, a0);
        mmu.write_halfword_safe(0x07000002, a1);
        mmu.write_halfword_safe(0x07000004, a2);
    }

    fn set_affine(mmu: &mut GbaMmu, group: u32, pa: u16, pb: u16, pc: u16, pd: u16) {
        let base = 0x07000000 + group * 32;
        mmu.write_halfword_safe(base + 0x06, pa);
        mmu.write_halfword_safe(base + 0x0E, pb);
        mmu.write_halfword_safe(base + 0x16, pc);
        mmu.write_halfword_safe(base + 0x1E, pd);
    }

    fn collect(ppu: &GbaPpu, mmu: &GbaMmu, ly: u16, dispcnt: u16) -> Vec<(usize, u16)> {
        let mut out = Vec::new();
        ppu.for_each_sprite_pixel(mmu, ly, 0, dispcnt, 1, 1, |x, c| out.push((x, c)));
        out
    }

    #[test]
    fn affine_identity_matches_upright_sprite() {
        let ppu = GbaPpu::new();
        let dispcnt = 0x0040; // 1D mapping
        let mut mmu = mmu_with_obj_tile(|x, y| if x == 0 { 15 } else { (y + 1) as u8 });

        // Reference: plain non-rot/scale sprite at (0,0).
        set_oam(&mut mmu, 0x0000, 0x0000, 0x0000);
        let upright: Vec<Vec<(usize, u16)>> =
            (0..8).map(|ly| collect(&ppu, &mmu, ly, dispcnt)).collect();

        // Rot/scale sprite with an identity matrix (group 0) must render identically.
        set_oam(&mut mmu, 0x0100, 0x0000, 0x0000);
        set_affine(&mut mmu, 0, 0x0100, 0, 0, 0x0100);
        let affine: Vec<Vec<(usize, u16)>> =
            (0..8).map(|ly| collect(&ppu, &mmu, ly, dispcnt)).collect();

        assert_eq!(upright, affine);
        assert!(upright.iter().any(|row| !row.is_empty())); // sanity: it drew something
    }

    #[test]
    fn affine_param_index_bit13_is_not_vflip() {
        let ppu = GbaPpu::new();
        let dispcnt = 0x0040;
        let mut mmu = mmu_with_obj_tile(|x, y| if x == 3 && y == 1 { 5 } else { 0 });
        let color = mmu.read_palette_halfword(0x200 + 5 * 2);

        // Rot/scale, ATTR1 parameter group = 16 (bit13 set). The old bug read bit13 as V-flip.
        set_oam(&mut mmu, 0x0100, 0x2000, 0x0000);
        set_affine(&mut mmu, 16, 0x0100, 0, 0, 0x0100);

        // Texel (3,1) stays upright at scanline 1 / x=3 — it is NOT flipped down to scanline 6.
        assert_eq!(collect(&ppu, &mmu, 1, dispcnt), vec![(3, color)]);
        assert!(collect(&ppu, &mmu, 6, dispcnt).is_empty());
    }

    #[test]
    fn affine_180_matrix_flips_sprite() {
        let ppu = GbaPpu::new();
        let dispcnt = 0x0040;
        let mut mmu = mmu_with_obj_tile(|x, y| if x == 3 && y == 1 { 5 } else { 0 });
        let color = mmu.read_palette_halfword(0x200 + 5 * 2);

        // Rot/scale, group 0, 180-degree matrix (PA=PD=-1.0 in 8.8): point reflection about center.
        set_oam(&mut mmu, 0x0100, 0x0000, 0x0000);
        set_affine(&mut mmu, 0, 0xFF00, 0, 0, 0xFF00);

        // Texel (3,1) reflects to screen (5,7): the matrix drives orientation, not stray bits.
        assert_eq!(collect(&ppu, &mmu, 7, dispcnt), vec![(5, color)]);
        assert!(collect(&ppu, &mmu, 1, dispcnt).is_empty());
    }

    #[test]
    fn obj_2d_8bpp_row_stride_is_32_tile_numbers() {
        // 2D mapping, 8bpp, 16x16 sprite. Texel (0,8) is in tile-grid row 1, so its tile number is
        // grid_y*32 + grid_x*2 = 32 (NOT 64). Its byte lives at OBJ VRAM 0x10000 + 32*0x20.
        let ppu = GbaPpu::new();
        let mut mmu = GbaMmu::new(vec![]);
        let idx = 7u8;
        mmu.write_halfword_safe(0x05000000 + 0x200 + idx as u32 * 2, 0x1234);
        mmu.write_halfword_safe(0x06010000 + 32 * 0x20, idx as u16); // tile 32, row 0, col 0
        assert_eq!(
            ppu.sample_sprite_texel(&mmu, 0, 16, true, false, 0, 0, 8),
            Some(0x1234)
        );
    }
}
