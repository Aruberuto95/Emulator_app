pub struct GbaPpu {
    // Cycle timing
    pub cycle_accumulator: u32,
}

#[derive(Clone, Copy)]
struct CompositorPixel {
    color: u16,   // BGR555
    priority: u8, // 0-4 (4 is backdrop)
    source: u8,   // 0=Backdrop, 1=BG0, 2=BG1, 3=BG2, 4=BG3, 5=OBJ
}

impl GbaPpu {
    pub fn new() -> Self {
        Self {
            cycle_accumulator: 0,
        }
    }

    pub fn tick(
        &mut self,
        cycles: u32,
        mmu: &mut crate::gba::mmu::GbaMmu,
        video_buffer: &mut [u8],
        is_render_tick: bool,
    ) {
        for _ in 0..cycles {
            self.cycle_accumulator += 1;

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

                // VBlank flag (bit 0 of dispstat)
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
                mmu.write_halfword_safe(0x04000006, vcount);

                // Render scanline if visible and frame is not skipped
                if vcount < 160 && is_render_tick {
                    self.render_scanline(vcount, mmu, video_buffer);
                }
            }
        }
    }

    fn render_scanline(&self, ly: u16, mmu: &crate::gba::mmu::GbaMmu, video_buffer: &mut [u8]) {
        let dispcnt = mmu.read_halfword_safe(0x04000000);

        // Forced blank (DISPCNT bit 7): the GBA outputs a white screen. Honor it so the
        // previous frame's pixels don't linger during fades / scene loads.
        if (dispcnt & 0x0080) != 0 {
            let line_offset = (ly as usize) * 240 * 3;
            let end = line_offset + 240 * 3;
            if end <= video_buffer.len() {
                for byte in &mut video_buffer[line_offset..end] {
                    *byte = 0xFF;
                }
            }
            return;
        }

        // Read backdrop color from Palette RAM (BG index 0)
        let backdrop_color = mmu.read_palette_halfword(0);

        let mut scanline = [CompositorPixel {
            color: backdrop_color,
            priority: 4,
            source: 0,
        }; 240];

        let mode = dispcnt & 7;

        // Render Mode 0 Backgrounds BG0-BG3
        if mode == 0 {
            for bg in (0..4).rev() {
                // BG3 (lowest) to BG0 (highest) in terms of draw order
                let bg_enabled = (dispcnt & (1 << (8 + bg))) != 0;
                if !bg_enabled {
                    continue;
                }
                self.render_bg_layer(bg, ly, &mut scanline, mmu);
            }
        }

        // Render OAM Sprites (OBJ)
        let obj_enabled = (dispcnt & 0x1000) != 0;
        if obj_enabled {
            self.render_sprites_layer(ly, &mut scanline, mmu);
        }

        // Convert the final composited scanline to RGB888 in the video buffer
        let line_offset = (ly as usize) * 240 * 3;
        for x in 0..240 {
            let pixel = scanline[x];
            let r = ((pixel.color & 0x1F) << 3) as u8;
            let g = (((pixel.color >> 5) & 0x1F) << 3) as u8;
            let b = (((pixel.color >> 10) & 0x1F) << 3) as u8;

            let idx = line_offset + x * 3;
            if idx + 2 < video_buffer.len() {
                video_buffer[idx] = r;
                video_buffer[idx + 1] = g;
                video_buffer[idx + 2] = b;
            }
        }
    }

    fn render_bg_layer(
        &self,
        bg: usize,
        ly: u16,
        scanline: &mut [CompositorPixel; 240],
        mmu: &crate::gba::mmu::GbaMmu,
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

        let y_map = (ly.wrapping_add(vofs)) % map_h;
        let tile_y = y_map / 8;
        let py = y_map % 8;

        for x in 0..240 {
            let x_map = ((x as u16).wrapping_add(hofs)) % map_w;
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

                let dest_pix = &mut scanline[x];
                // Layer priority check (lower is higher priority)
                if priority < dest_pix.priority {
                    dest_pix.color = bgr555;
                    dest_pix.priority = priority;
                    dest_pix.source = (bg + 1) as u8;
                }
            }
        }
    }

    fn render_sprites_layer(
        &self,
        ly: u16,
        scanline: &mut [CompositorPixel; 240],
        mmu: &crate::gba::mmu::GbaMmu,
    ) {
        // Iterate through all 128 sprites in OAM in reverse order (lowest index is highest priority)
        for obj_idx in (0..128).rev() {
            let oam_addr = obj_idx * 8;
            let attr0 = mmu.read_oam_halfword(oam_addr);
            let attr1 = mmu.read_oam_halfword(oam_addr + 2);
            let attr2 = mmu.read_oam_halfword(oam_addr + 4);

            // Sprite disable check (bit 9 of attr0 if Obj Mode is normal)
            let is_disabled = (attr0 & 0x0200) != 0 && (attr0 & 0x0800) == 0;
            if is_disabled {
                continue;
            }

            // Decode dimensions
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

            // Sprite positions (R0-R255 coordinates)
            let mut y = (attr0 & 0x00FF) as i16;
            if y >= 160 {
                y -= 256;
            }
            let mut x_pos = (attr1 & 0x01FF) as i16;
            if x_pos >= 256 {
                x_pos -= 512;
            }

            // Scanline overlap check
            let ly_i = ly as i16;
            if ly_i < y || ly_i >= y + h as i16 {
                continue;
            }

            let py = (ly_i - y) as u16;
            let is_8bpp = (attr0 & 0x2000) != 0;
            let priority = ((attr2 >> 10) & 3) as u8;
            let tile_start = attr2 & 0x03FF;
            let palette_idx = ((attr2 >> 12) & 0x0F) as u8;

            let hflip = (attr1 & 0x1000) != 0;
            let vflip = (attr1 & 0x2000) != 0;

            let final_py = if vflip { h - 1 - py } else { py };
            let dispcnt = mmu.read_halfword_safe(0x04000000);
            let is_1d_mapping = (dispcnt & 0x0040) != 0;

            // Sprite VRAM tile layout
            for px in 0..w {
                let sx = x_pos + px as i16;
                if sx < 0 || sx >= 240 {
                    continue;
                }

                let final_px = if hflip { w - 1 - px } else { px };

                // Get coordinates relative to tiles
                let tile_px = final_px % 8;
                let tile_py = final_py % 8;
                let grid_x = final_px / 8;
                let grid_y = final_py / 8;

                // Calculate the tile index offset based on 1D or 2D mapping
                let tile_offset = if is_1d_mapping {
                    let tiles_per_row = w / 8;
                    if is_8bpp {
                        (grid_y * tiles_per_row * 2 + grid_x * 2) as u32
                    } else {
                        (grid_y * tiles_per_row + grid_x) as u32
                    }
                } else {
                    // 2D grid is 32 tiles wide
                    if is_8bpp {
                        (grid_y * 32 * 2 + grid_x * 2) as u32
                    } else {
                        (grid_y * 32 + grid_x) as u32
                    }
                };

                let cur_tile = tile_start as u32 + tile_offset;

                // Fetch pixel index in tile
                let pixel_idx = if is_8bpp {
                    let tile_addr = 0x10000 + cur_tile * 32 + (tile_py as u32 * 8) + tile_px as u32;
                    mmu.read_vram_byte(tile_addr)
                } else {
                    let tile_addr =
                        0x10000 + cur_tile * 32 + (tile_py as u32 * 4) + (tile_px as u32 / 2);
                    let byte = mmu.read_vram_byte(tile_addr);
                    if tile_px % 2 == 0 {
                        byte & 0x0F
                    } else {
                        byte >> 4
                    }
                };

                if pixel_idx != 0 {
                    let palette_offset = if is_8bpp {
                        pixel_idx as u32 * 2
                    } else {
                        (palette_idx as u32 * 16 + pixel_idx as u32) * 2
                    };
                    let bgr555 = mmu.read_palette_halfword(0x200 + palette_offset); // Sprite palette starts at 0x200 in palette RAM

                    let dest_pix = &mut scanline[sx as usize];
                    // Overwrite if sprite priority <= current pixel priority
                    if priority <= dest_pix.priority {
                        dest_pix.color = bgr555;
                        dest_pix.priority = priority;
                        dest_pix.source = 5; // Sprite
                    }
                }
            }
        }
    }
}
