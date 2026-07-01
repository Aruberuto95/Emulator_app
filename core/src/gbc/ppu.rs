use crate::gbc::mmu::Mmu;

/// Decodes a CGB palette-RAM entry (little-endian BGR555) with bit 15 forced clear.
/// This is the native framebuffer format — no 5-to-8-bit expansion needed.
#[inline]
fn palette_color(lo: u8, hi: u8) -> u16 {
    u16::from_le_bytes([lo, hi]) & 0x7FFF
}

/// Game Boy Color Picture Processing Unit (PPU).
pub struct Ppu {
    pub cycle_accumulator: u32,
    pub window_y_internal: u8,
    /// Tracks whether the LCD was disabled on the previous tick, so the framebuffer is
    /// blanked exactly once on the on->off edge (transient render state, not serialized).
    pub lcd_was_off: bool,
    /// Set on the VBlank edge (entering line 144) so the caller can present only complete
    /// frames (back->front copy), avoiding mid-frame tearing during fast transitions.
    pub frame_completed: bool,
}

impl Ppu {
    /// Creates a new PPU instance.
    pub fn new() -> Self {
        Self {
            cycle_accumulator: 0,
            window_y_internal: 0,
            lcd_was_off: false,
            frame_completed: false,
        }
    }

    /// Resets PPU state variables.
    pub fn reset(&mut self) {
        self.cycle_accumulator = 0;
        self.window_y_internal = 0;
        self.lcd_was_off = false;
        self.frame_completed = false;
    }

    /// Advance PPU timing by elapsed cycles. When `is_render_tick` is false (frame skipping)
    /// timing still advances but scanlines are not drawn, so the caller can reuse the real
    /// framebuffer instead of allocating a throwaway one each step.
    pub fn tick(
        &mut self,
        cycles: u32,
        mmu: &mut Mmu,
        video_buffer: &mut [u16],
        is_render_tick: bool,
        double_speed: bool,
    ) {
        let lcdc = mmu.read_byte(0xFF40);
        if (lcdc & 0x80) == 0 {
            // LCD is disabled. On real hardware the panel goes blank (white); blank the
            // framebuffer once on the on->off edge so a stale frame doesn't bleed through
            // scene transitions (games disable the LCD to reload VRAM). Present that blank
            // frame (frame_completed) — otherwise the caller never copies back->front and
            // the display freezes on the last drawn frame for the whole load. Gate on
            // is_render_tick (and only then mark lcd_was_off) so the blank still presents if
            // the on->off edge lands on a frame-skip tick.
            if !self.lcd_was_off && is_render_tick {
                video_buffer.fill(0x7FFF); // BGR555 white
                self.frame_completed = true;
                self.lcd_was_off = true;
            }
            mmu.write_io(0x44, 0); // LY = 0
            let mut stat = mmu.read_byte(0xFF41);
            stat = (stat & 0xFC) | 0x00; // Mode 0
            mmu.write_io(0x41, stat);
            self.cycle_accumulator = 0;
            self.window_y_internal = 0;
            return;
        }
        self.lcd_was_off = false;

        let cycles_per_line = if double_speed { 912 } else { 456 };

        for _ in 0..cycles {
            self.cycle_accumulator += 1;

            // 1. Evaluate STAT mode for current cycle
            let ly = mmu.read_io(0x44);
            if ly < 144 {
                let mut stat = mmu.read_byte(0xFF41);
                let current_mode = if self.cycle_accumulator < (if double_speed { 160 } else { 80 }) {
                    2 // Mode 2 (OAM Search)
                } else if self.cycle_accumulator < (if double_speed { 578 } else { 289 }) {
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
                            // Entering H-Blank on a visible line: advance any active HDMA
                            // by one 16-byte block (no-op when none is running).
                            mmu.hdma_step();
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
                        iff |= 0x02; // Trigger STAT interrupt
                        mmu.write_io(0x0F, iff);
                    }
                    mmu.write_io(0x41, stat);
                }
            } else {
                let mut stat = mmu.read_byte(0xFF41);
                if (stat & 0x03) != 1 {
                    stat = (stat & 0xFC) | 0x01;
                    mmu.write_io(0x41, stat);
                }
            }

            // 2. Check if we reached the end of the line
            if self.cycle_accumulator >= cycles_per_line {
                self.cycle_accumulator = 0;

                let mut ly = mmu.read_io(0x44);
                let prev_ly = ly;
                ly = (ly + 1) % 154;
                mmu.write_io(0x44, ly);

                // Render scanline if it was visible (skipped on frame-skip ticks)
                if prev_ly < 144 && is_render_tick {
                    self.render_scanline(prev_ly, mmu, video_buffer);
                }

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

                // Mode update upon entering the new line
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
                        self.frame_completed = true;
                    }
                }
                mmu.write_io(0x41, stat);
            }
        }
    }

    /// Renders a single horizontal scanline into the FFI video buffer (BGR555 pixels).
    fn render_scanline(&mut self, ly: u8, mmu: &Mmu, video_buffer: &mut [u16]) {
        // Single validated boundary for the whole scanline: an out-of-range `ly`
        // panics here instead of silently corrupting memory further down.
        let line = &mut video_buffer[ly as usize * 160..ly as usize * 160 + 160];
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
            line[x as usize] = palette_color(
                mmu.bg_palette_ram[palette_offset],
                mmu.bg_palette_ram[palette_offset + 1],
            );
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
                line[x as usize] = palette_color(
                    mmu.bg_palette_ram[palette_offset],
                    mmu.bg_palette_ram[palette_offset + 1],
                );
            }

            if window_drawn_line {
                self.window_y_internal += 1;
            }
        }

        // 3. Render Sprite (OAM) layer
        let sprite_enable = (lcdc & 0x02) != 0;
        if sprite_enable {
            let sprite_height = if (lcdc & 0x04) != 0 { 16 } else { 8 };
            // Zero-allocation sprite scan: hardware caps at 10 sprites per scanline.
            let mut active_sprites = [(0u8, 0u8, 0u8, 0u8); 10];
            let mut active_count = 0usize;

            // Search OAM for up to 10 matching sprites on the scanline
            for i in 0..40 {
                let oam_offset = i * 4;
                let sprite_y = mmu.oam[oam_offset];
                let sprite_x = mmu.oam[oam_offset + 1];
                let tile_idx = mmu.oam[oam_offset + 2];
                let attr = mmu.oam[oam_offset + 3];

                // u16 math: sprite_y + sprite_height can exceed 255 for off-screen sprites.
                let row = ly as u16 + 16;
                if row >= sprite_y as u16 && row < sprite_y as u16 + sprite_height as u16 {
                    active_sprites[active_count] = (sprite_y, sprite_x, tile_idx, attr);
                    active_count += 1;
                    if active_count == 10 {
                        break;
                    }
                }
            }

            // GBC priority: lower OAM index wins. The scan above visits OAM in ascending
            // index order, so draw in reverse (descending index) and let later writes
            // (lower indices) overwrite — no sort needed.
            for &(sprite_y, sprite_x, tile_idx, attr) in active_sprites[..active_count].iter().rev()
            {
                let mut tile_idx = tile_idx;
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
                    line[target_x as usize] = palette_color(
                        mmu.obj_palette_ram[palette_offset],
                        mmu.obj_palette_ram[palette_offset + 1],
                    );
                }
            }
        }
    }
}
