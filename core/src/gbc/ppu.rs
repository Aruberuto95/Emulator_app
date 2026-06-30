use crate::gbc::mmu::Mmu;

/// Game Boy Color Picture Processing Unit (PPU).
pub struct Ppu {
    pub cycle_accumulator: u32,
    pub window_y_internal: u8,
}

impl Ppu {
    /// Creates a new PPU instance.
    pub fn new() -> Self {
        Self {
            cycle_accumulator: 0,
            window_y_internal: 0,
        }
    }

    /// Resets PPU state variables.
    pub fn reset(&mut self) {
        self.cycle_accumulator = 0;
        self.window_y_internal = 0;
    }

    /// Advance PPU timing by elapsed cycles.
    pub fn tick(
        &mut self,
        cycles: u32,
        mmu: &mut Mmu,
        video_buffer: &mut [u8],
        double_speed: bool,
    ) {
        let lcdc = mmu.read_byte(0xFF40);
        if (lcdc & 0x80) == 0 {
            // LCD is disabled. Reset registers.
            mmu.write_io(0x44, 0); // LY = 0
            let mut stat = mmu.read_byte(0xFF41);
            stat = (stat & 0xFC) | 0x00; // Mode 0
            mmu.write_io(0x41, stat);
            self.cycle_accumulator = 0;
            self.window_y_internal = 0;
            return;
        }

        let cycles_per_line = if double_speed { 912 } else { 456 };
        self.cycle_accumulator += cycles;

        // Process line increments
        while self.cycle_accumulator >= cycles_per_line {
            self.cycle_accumulator -= cycles_per_line;

            let mut ly = mmu.read_io(0x44);
            let prev_ly = ly;
            ly = (ly + 1) % 154;
            mmu.write_io(0x44, ly);

            // Coincidence check
            let lyc = mmu.read_io(0x45);
            let mut stat = mmu.read_byte(0xFF41);
            let coincidence = ly == lyc;
            if coincidence {
                stat |= 0x04;
                if (stat & 0x40) != 0 {
                    let mut iff = mmu.read_io(0x0F);
                    iff |= 0x02; // Trigger STAT interrupt
                    mmu.write_io(0x0F, iff);
                }
            } else {
                stat &= !0x04;
            }

            // Mode update
            if ly >= 144 {
                // Mode 1 (V-Blank)
                if ly == 144 {
                    stat = (stat & 0xFC) | 0x01;
                    let mut iff = mmu.read_io(0x0F);
                    iff |= 0x01; // Trigger V-Blank interrupt
                    if (stat & 0x10) != 0 {
                        iff |= 0x02; // Trigger STAT V-Blank interrupt
                    }
                    mmu.write_io(0x0F, iff);
                    self.window_y_internal = 0;
                }
            } else {
                // Mode 0 (H-Blank)
                stat = (stat & 0xFC) | 0x00;
                if (stat & 0x08) != 0 {
                    let mut iff = mmu.read_io(0x0F);
                    iff |= 0x02; // Trigger STAT H-Blank interrupt
                    mmu.write_io(0x0F, iff);
                }

                // Draw scanline
                self.render_scanline(prev_ly, mmu, video_buffer);
            }
            mmu.write_io(0x41, stat);
        }

        // Sub-scanline STAT Mode updates for precise cycle timing
        let ly = mmu.read_io(0x44);
        if ly < 144 {
            let mut stat = mmu.read_byte(0xFF41);
            let line_cycle = self.cycle_accumulator % cycles_per_line;
            let current_mode = if line_cycle < (if double_speed { 160 } else { 80 }) {
                2 // Mode 2 (OAM Search)
            } else if line_cycle < (if double_speed { 578 } else { 289 }) {
                3 // Mode 3 (Pixel Transfer)
            } else {
                0 // Mode 0 (H-Blank)
            };

            let prev_mode = stat & 0x03;
            if prev_mode != current_mode {
                stat = (stat & 0xFC) | current_mode;
                let mut trigger = false;
                match current_mode {
                    0 => {
                        if (stat & 0x08) != 0 {
                            trigger = true;
                        }
                    }
                    2 => {
                        if (stat & 0x20) != 0 {
                            trigger = true;
                        }
                    }
                    _ => {}
                }
                if trigger {
                    let mut iff = mmu.read_io(0x0F);
                    iff |= 0x02;
                    mmu.write_io(0x0F, iff);
                }
                mmu.write_io(0x41, stat);
            }
        }
    }

    /// Renders a single horizontal scanline into the FFI video buffer.
    fn render_scanline(&mut self, ly: u8, mmu: &Mmu, video_buffer: &mut [u8]) {
        let lcdc = mmu.read_byte(0xFF40);
        let scy = mmu.read_byte(0xFF42);
        let scx = mmu.read_byte(0xFF43);
        let wy = mmu.read_byte(0xFF4A);
        let wx = mmu.read_byte(0xFF4B);

        // Zero-allocation scanline buffers
        let mut bg_color_indices = [0u8; 160];
        let mut bg_priorities = [false; 160];

        // 1. Render Background layer
        let bg_win_enable = (lcdc & 0x01) != 0; // In GBC, if 0, sprites lose priority bypass
        let bg_tile_map_base = if (lcdc & 0x08) != 0 { 0x9C00 } else { 0x9800 };
        let tile_data_base = if (lcdc & 0x10) != 0 { 0x8000 } else { 0x9000 };

        for x in 0..160 {
            let bg_x = (x as u32 + scx as u32) % 256;
            let bg_y = (ly as u32 + scy as u32) % 256;

            let tile_col = bg_x / 8;
            let tile_row = bg_y / 8;
            let tile_map_offset = tile_row * 32 + tile_col;
            let tile_map_address = bg_tile_map_base + tile_map_offset as u16;

            // VRAM Bank 0 holds tile indices
            let tile_index = mmu.vram[tile_map_address as usize - 0x8000];

            // VRAM Bank 1 holds tile attribute bytes
            let attr = mmu.vram[0x2000 + tile_map_address as usize - 0x8000];
            let palette_num = attr & 0x07;
            let vram_bank = ((attr >> 3) & 0x01) as usize;
            let h_flip = (attr & 0x20) != 0;
            let v_flip = (attr & 0x40) != 0;
            let priority = (attr & 0x80) != 0;

            let mut pixel_row = (bg_y % 8) as u16;
            if v_flip {
                pixel_row = 7 - pixel_row;
            }

            // Fetch tile data bytes (16 bytes per tile, 2 bytes per row)
            let tile_data_address = if (lcdc & 0x10) != 0 {
                tile_data_base + (tile_index as u16 * 16) + (pixel_row * 2)
            } else {
                let signed_index = tile_index as i8 as i16;
                (tile_data_base as i32 + (signed_index as i32 * 16) + (pixel_row as i32 * 2)) as u16
            };

            let vram_bank_offset = vram_bank * 8192;
            let byte1 = mmu.vram[vram_bank_offset + tile_data_address as usize - 0x8000];
            let byte2 = mmu.vram[vram_bank_offset + tile_data_address as usize + 1 - 0x8000];

            let mut pixel_col = (bg_x % 8) as u8;
            if h_flip {
                pixel_col = 7 - pixel_col;
            }
            let bit_shift = 7 - pixel_col;
            let color_bit0 = (byte1 >> bit_shift) & 0x01;
            let color_bit1 = (byte2 >> bit_shift) & 0x01;
            let color_idx = (color_bit1 << 1) | color_bit0;

            bg_color_indices[x as usize] = color_idx;
            bg_priorities[x as usize] = priority;

            // Resolve palette color
            let palette_offset = (palette_num as usize * 8) + (color_idx as usize * 2);
            let bcpd_low = mmu.bg_palette_ram[palette_offset];
            let bcpd_high = mmu.bg_palette_ram[palette_offset + 1];
            let color_bytes = ((bcpd_high as u16) << 8) | (bcpd_low as u16);

            let r5 = color_bytes & 0x1F;
            let g5 = (color_bytes >> 5) & 0x1F;
            let b5 = (color_bytes >> 10) & 0x1F;

            let r = ((r5 << 3) | (r5 >> 2)) as u8;
            let g = ((g5 << 3) | (g5 >> 2)) as u8;
            let b = ((b5 << 3) | (b5 >> 2)) as u8;

            let out_offset = (ly as usize * 160 * 3) + (x as usize * 3);
            if out_offset + 2 < video_buffer.len() {
                video_buffer[out_offset] = r;
                video_buffer[out_offset + 1] = g;
                video_buffer[out_offset + 2] = b;
            }
        }

        // 2. Render Window layer
        let win_enable = (lcdc & 0x20) != 0;
        if win_enable && ly >= wy && wx <= 166 {
            let win_tile_map_base = if (lcdc & 0x40) != 0 { 0x9C00 } else { 0x9800 };
            let mut window_drawn_line = false;

            for x in 0..160 {
                if x + 7 < wx {
                    continue;
                }
                window_drawn_line = true;

                let win_x = (x + 7 - wx) as u32;
                let win_y = self.window_y_internal as u32;

                let tile_col = win_x / 8;
                let tile_row = win_y / 8;
                let tile_map_offset = tile_row * 32 + tile_col;
                let tile_map_address = win_tile_map_base + tile_map_offset as u16;

                let tile_index = mmu.vram[tile_map_address as usize - 0x8000];
                let attr = mmu.vram[0x2000 + tile_map_address as usize - 0x8000];
                let palette_num = attr & 0x07;
                let vram_bank = ((attr >> 3) & 0x01) as usize;
                let h_flip = (attr & 0x20) != 0;
                let v_flip = (attr & 0x40) != 0;
                let priority = (attr & 0x80) != 0;

                let mut pixel_row = (win_y % 8) as u16;
                if v_flip {
                    pixel_row = 7 - pixel_row;
                }

                let tile_data_address = if (lcdc & 0x10) != 0 {
                    tile_data_base + (tile_index as u16 * 16) + (pixel_row * 2)
                } else {
                    let signed_index = tile_index as i8 as i16;
                    (tile_data_base as i32 + (signed_index as i32 * 16) + (pixel_row as i32 * 2))
                        as u16
                };

                let vram_bank_offset = vram_bank * 8192;
                let byte1 = mmu.vram[vram_bank_offset + tile_data_address as usize - 0x8000];
                let byte2 = mmu.vram[vram_bank_offset + tile_data_address as usize + 1 - 0x8000];

                let mut pixel_col = (win_x % 8) as u8;
                if h_flip {
                    pixel_col = 7 - pixel_col;
                }
                let bit_shift = 7 - pixel_col;
                let color_bit0 = (byte1 >> bit_shift) & 0x01;
                let color_bit1 = (byte2 >> bit_shift) & 0x01;
                let color_idx = (color_bit1 << 1) | color_bit0;

                bg_color_indices[x as usize] = color_idx;
                bg_priorities[x as usize] = priority;

                let palette_offset = (palette_num as usize * 8) + (color_idx as usize * 2);
                let bcpd_low = mmu.bg_palette_ram[palette_offset];
                let bcpd_high = mmu.bg_palette_ram[palette_offset + 1];
                let color_bytes = ((bcpd_high as u16) << 8) | (bcpd_low as u16);

                let r5 = color_bytes & 0x1F;
                let g5 = (color_bytes >> 5) & 0x1F;
                let b5 = (color_bytes >> 10) & 0x1F;

                let r = ((r5 << 3) | (r5 >> 2)) as u8;
                let g = ((g5 << 3) | (g5 >> 2)) as u8;
                let b = ((b5 << 3) | (b5 >> 2)) as u8;

                let out_offset = (ly as usize * 160 * 3) + (x as usize * 3);
                if out_offset + 2 < video_buffer.len() {
                    video_buffer[out_offset] = r;
                    video_buffer[out_offset + 1] = g;
                    video_buffer[out_offset + 2] = b;
                }
            }

            if window_drawn_line {
                self.window_y_internal += 1;
            }
        }

        // 3. Render Sprite (OAM) layer
        let sprite_enable = (lcdc & 0x02) != 0;
        if sprite_enable {
            let sprite_height = if (lcdc & 0x04) != 0 { 16 } else { 8 };
            let mut active_sprites = Vec::with_capacity(10);

            // Search OAM for up to 10 matching sprites on the scanline
            for i in 0..40 {
                let oam_offset = i * 4;
                let sprite_y = mmu.oam[oam_offset];
                let sprite_x = mmu.oam[oam_offset + 1];
                let tile_idx = mmu.oam[oam_offset + 2];
                let attr = mmu.oam[oam_offset + 3];

                if ly + 16 >= sprite_y && ly + 16 < sprite_y + sprite_height {
                    active_sprites.push((i, sprite_y, sprite_x, tile_idx, attr));
                    if active_sprites.len() == 10 {
                        break;
                    }
                }
            }

            // GBC priority sorting: lower OAM index has priority.
            // Sort in DESCENDING order of OAM index so that lower index sprites overwrite higher ones.
            active_sprites.sort_by(|a, b| b.0.cmp(&a.0));

            for (_, sprite_y, sprite_x, mut tile_idx, attr) in active_sprites {
                if sprite_x == 0 || sprite_x >= 168 {
                    continue;
                }

                let palette_num = attr & 0x07;
                let vram_bank = ((attr >> 3) & 0x01) as usize;
                let h_flip = (attr & 0x20) != 0;
                let v_flip = (attr & 0x40) != 0;
                let sprite_priority = (attr & 0x80) != 0;

                let mut target_y_offset = (ly + 16 - sprite_y) as u16;
                if v_flip {
                    target_y_offset = (sprite_height as u16 - 1) - target_y_offset;
                }

                if sprite_height == 16 {
                    tile_idx &= 0xFE; // Force low bit of tile index to 0 in 8x16 mode
                }

                let tile_data_address = 0x8000 + (tile_idx as u16 * 16) + (target_y_offset * 2);
                let vram_bank_offset = vram_bank * 8192;
                let byte1 = mmu.vram[vram_bank_offset + tile_data_address as usize - 0x8000];
                let byte2 = mmu.vram[vram_bank_offset + tile_data_address as usize + 1 - 0x8000];

                let target_x_start = sprite_x as i32 - 8;

                for sx in 0..8 {
                    let target_x = target_x_start + sx;
                    if target_x < 0 || target_x >= 160 {
                        continue;
                    }

                    let mut pixel_col = sx as u8;
                    if h_flip {
                        pixel_col = 7 - pixel_col;
                    }
                    let bit_shift = 7 - pixel_col;
                    let color_bit0 = (byte1 >> bit_shift) & 0x01;
                    let color_bit1 = (byte2 >> bit_shift) & 0x01;
                    let color_idx = (color_bit1 << 1) | color_bit0;

                    if color_idx == 0 {
                        continue; // Color index 0 is transparent
                    }

                    // Priority handling
                    let bg_color_idx = bg_color_indices[target_x as usize];
                    if bg_win_enable {
                        let bg_priority = bg_priorities[target_x as usize];
                        if bg_color_idx != 0 {
                            if bg_priority || sprite_priority {
                                continue;
                            }
                        }
                    }

                    // Resolve OBJ Palette color
                    let palette_offset = (palette_num as usize * 8) + (color_idx as usize * 2);
                    let ocpd_low = mmu.obj_palette_ram[palette_offset];
                    let ocpd_high = mmu.obj_palette_ram[palette_offset + 1];
                    let color_bytes = ((ocpd_high as u16) << 8) | (ocpd_low as u16);

                    let r5 = color_bytes & 0x1F;
                    let g5 = (color_bytes >> 5) & 0x1F;
                    let b5 = (color_bytes >> 10) & 0x1F;

                    let r = ((r5 << 3) | (r5 >> 2)) as u8;
                    let g = ((g5 << 3) | (g5 >> 2)) as u8;
                    let b = ((b5 << 3) | (b5 >> 2)) as u8;

                    let out_offset = (ly as usize * 160 * 3) + (target_x as usize * 3);
                    if out_offset + 2 < video_buffer.len() {
                        video_buffer[out_offset] = r;
                        video_buffer[out_offset + 1] = g;
                        video_buffer[out_offset + 2] = b;
                    }
                }
            }
        }
    }
}
