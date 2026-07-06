pub struct TileDescriptor {
    pub format: u8,
    pub size: u8,
    pub tmem_addr: u16,
    pub line_width: u16,
    pub clamp_s: bool,
    pub clamp_t: bool,
    pub mask_s: u8,
    pub mask_t: u8,
    pub shift_s: u8,
    pub shift_t: u8,
}

impl Default for TileDescriptor {
    fn default() -> Self {
        Self {
            format: 0,
            size: 0,
            tmem_addr: 0,
            line_width: 0,
            clamp_s: false,
            clamp_t: false,
            mask_s: 0,
            mask_t: 0,
            shift_s: 0,
            shift_t: 0,
        }
    }
}

pub struct Rdp {
    pub cmd_buffer: Vec<u64>,
    pub tmem: [u8; 4096],
    pub color_image_addr: u32,
    pub color_image_format: u8,
    pub color_image_size: u8,
    pub color_image_width: u32,
    pub depth_image_addr: u32,
    pub texture_image_addr: u32,
    pub texture_image_format: u8,
    pub texture_image_size: u8,
    pub texture_image_width: u32,
    pub tiles: [TileDescriptor; 8],
    pub scissor_xh: u16,
    pub scissor_yh: u16,
    pub scissor_xl: u16,
    pub scissor_yl: u16,
    pub fill_color: u32,
    pub blend_color: u32,
    pub fog_color: u32,
    pub prim_color: u32,
    pub env_color: u32,
    pub cycle_type: u8,
}

impl Rdp {
    pub fn new() -> Self {
        Self {
            cmd_buffer: Vec::new(),
            tmem: [0; 4096],
            color_image_addr: 0,
            color_image_format: 0,
            color_image_size: 0,
            color_image_width: 0,
            depth_image_addr: 0,
            texture_image_addr: 0,
            texture_image_format: 0,
            texture_image_size: 0,
            texture_image_width: 0,
            tiles: [
                TileDescriptor::default(),
                TileDescriptor::default(),
                TileDescriptor::default(),
                TileDescriptor::default(),
                TileDescriptor::default(),
                TileDescriptor::default(),
                TileDescriptor::default(),
                TileDescriptor::default(),
            ],
            scissor_xh: 0,
            scissor_yh: 0,
            scissor_xl: 320,
            scissor_yl: 240,
            fill_color: 0,
            blend_color: 0,
            fog_color: 0,
            prim_color: 0,
            env_color: 0,
            cycle_type: 0,
        }
    }

    pub fn reset(&mut self) {
        self.cmd_buffer.clear();
        self.tmem.fill(0);
        self.color_image_addr = 0;
        self.color_image_format = 0;
        self.color_image_size = 0;
        self.color_image_width = 0;
        self.depth_image_addr = 0;
        self.texture_image_addr = 0;
        self.texture_image_format = 0;
        self.texture_image_size = 0;
        self.texture_image_width = 0;
        for tile in &mut self.tiles {
            *tile = TileDescriptor::default();
        }
        self.scissor_xh = 0;
        self.scissor_yh = 0;
        self.scissor_xl = 320;
        self.scissor_yl = 240;
        self.fill_color = 0;
        self.blend_color = 0;
        self.fog_color = 0;
        self.prim_color = 0;
        self.env_color = 0;
        self.cycle_type = 0;
    }

    pub fn process_command(&mut self, w0: u32, w1: u32, _rdram: &mut crate::n64::rdram::Rdram) {
        let cmd = ((w0 >> 24) & 0x3F) as u8;
        match cmd {
            0x2D => { // Set Other Modes
                self.cycle_type = ((w0 >> 20) & 0x3) as u8;
            }
            0x2E => { // Set Scissor
                self.scissor_xh = ((w0 >> 12) & 0xFFF) as u16;
                self.scissor_yh = (w0 & 0xFFF) as u16;
                self.scissor_xl = ((w1 >> 12) & 0xFFF) as u16;
                self.scissor_yl = (w1 & 0xFFF) as u16;
            }
            0x2F => { // Set Prim Color
                self.prim_color = w1;
            }
            0x30 => { // Set Env Color
                self.env_color = w1;
            }
            0x32 => { // Set Blend Color
                self.blend_color = w1;
            }
            0x33 => { // Set Fog Color
                self.fog_color = w1;
            }
            0x34 => { // Set Fill Color
                self.fill_color = w1;
            }
            0x35 => { // Set Color Image
                self.color_image_format = ((w0 >> 21) & 0x7) as u8;
                self.color_image_size = ((w0 >> 19) & 0x3) as u8;
                self.color_image_width = (w0 & 0xFFF) + 1;
                self.color_image_addr = w1;
            }
            0x36 => { // Set Depth Image
                self.depth_image_addr = w1;
            }
            0x37 => { // Set Texture Image
                self.texture_image_format = ((w0 >> 21) & 0x7) as u8;
                self.texture_image_size = ((w0 >> 19) & 0x3) as u8;
                self.texture_image_width = (w0 & 0xFFF) + 1;
                self.texture_image_addr = w1;
            }
            0x3F => { // Set Tile
                let tile_idx = ((w1 >> 24) & 0x7) as usize;
                let tile = &mut self.tiles[tile_idx];
                tile.format = ((w0 >> 21) & 0x7) as u8;
                tile.size = ((w0 >> 19) & 0x3) as u8;
                tile.line_width = ((w0 >> 9) & 0x1FF) as u16;
                tile.tmem_addr = (w0 & 0x1FF) as u16;
            }
            _ => {}
        }
    }

    pub fn draw_triangle(
        &mut self,
        rdram: &mut crate::n64::rdram::Rdram,
        v0_pos: [f32; 3],
        v0_col: [f32; 4],
        v0_uv: [f32; 2],
        v1_pos: [f32; 3],
        v1_col: [f32; 4],
        v1_uv: [f32; 2],
        v2_pos: [f32; 3],
        v2_col: [f32; 4],
        v2_uv: [f32; 2],
    ) {
        // Scissor bounds pre-checking
        if self.scissor_xl <= self.scissor_xh || self.scissor_yl <= self.scissor_yh {
            return;
        }

        let limit_x = (self.scissor_xl as i32 - 1).min(self.color_image_width as i32 - 1);
        if limit_x < self.scissor_xh as i32 {
            return;
        }
        let limit_y = self.scissor_yl as i32 - 1;
        if limit_y < self.scissor_yh as i32 {
            return;
        }

        let tri_min_x = v0_pos[0].min(v1_pos[0]).min(v2_pos[0]);
        let tri_max_x = v0_pos[0].max(v1_pos[0]).max(v2_pos[0]);
        let tri_min_y = v0_pos[1].min(v1_pos[1]).min(v2_pos[1]);
        let tri_max_y = v0_pos[1].max(v1_pos[1]).max(v2_pos[1]);

        if !tri_min_x.is_finite() || !tri_max_x.is_finite() || !tri_min_y.is_finite() || !tri_max_y.is_finite() {
            return;
        }

        if tri_max_x < self.scissor_xh as f32 || tri_min_x > limit_x as f32 ||
           tri_max_y < self.scissor_yh as f32 || tri_min_y > limit_y as f32 {
            return;
        }

        let min_x = (tri_min_x.floor() as i32).clamp(self.scissor_xh as i32, limit_x);
        let max_x = (tri_max_x.ceil() as i32).clamp(self.scissor_xh as i32, limit_x);
        let min_y = (tri_min_y.floor() as i32).clamp(self.scissor_yh as i32, limit_y);
        let max_y = (tri_max_y.ceil() as i32).clamp(self.scissor_yh as i32, limit_y);

        let area = (v1_pos[0] - v0_pos[0]) * (v2_pos[1] - v0_pos[1])
            - (v1_pos[1] - v0_pos[1]) * (v2_pos[0] - v0_pos[0]);

        if !area.is_finite() || area.abs() < 0.00001 {
            return;
        }

        let bytes_per_pixel = match self.color_image_size {
            0 => 1,
            1 => 2,
            2 => 4,
            _ => 2,
        };

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;

                let w0 = ((v1_pos[0] - px) * (v2_pos[1] - py) - (v1_pos[1] - py) * (v2_pos[0] - px)) / area;
                let w1 = ((v2_pos[0] - px) * (v0_pos[1] - py) - (v2_pos[1] - py) * (v0_pos[0] - px)) / area;
                let w2 = 1.0 - w0 - w1;

                if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                    let mut interp_z = w0 * v0_pos[2] + w1 * v1_pos[2] + w2 * v2_pos[2];
                    if !interp_z.is_finite() {
                        interp_z = 0.0;
                    }
                    let interp_z = interp_z.clamp(0.0, 1.0);

                    // Z-buffer check (always 16-bit depth)
                    let offset = y as u64 * self.color_image_width as u64 + x as u64;
                    let depth_addr_64 = self.depth_image_addr as u64 + offset * 2;
                    if depth_addr_64 + 2 <= crate::n64::rdram::Rdram::SIZE as u64 {
                        let z_old = rdram.read_u16(depth_addr_64 as u32);
                        let z_new = (interp_z * 65535.0) as u16;

                        if z_new <= z_old {
                            // Write to Z-buffer
                            rdram.write_u16(depth_addr_64 as u32, z_new);

                            // Interpolate color with NaN/Infinity clamping checks
                            let mut r = w0 * v0_col[0] + w1 * v1_col[0] + w2 * v2_col[0];
                            let mut g = w0 * v0_col[1] + w1 * v1_col[1] + w2 * v2_col[1];
                            let mut b = w0 * v0_col[2] + w1 * v1_col[2] + w2 * v2_col[2];

                            if !r.is_finite() { r = 0.0; }
                            if !g.is_finite() { g = 0.0; }
                            if !b.is_finite() { b = 0.0; }

                            let r = r.clamp(0.0, 1.0);
                            let g = g.clamp(0.0, 1.0);
                            let b = b.clamp(0.0, 1.0);

                            let color_addr_64 = self.color_image_addr as u64 + offset * bytes_per_pixel;
                            if color_addr_64 + bytes_per_pixel <= crate::n64::rdram::Rdram::SIZE as u64 {
                                match self.color_image_size {
                                    0 => { // 8-bit color image size
                                        let val = (((r * 7.0) as u8) << 5) | (((g * 7.0) as u8) << 2) | ((b * 3.0) as u8);
                                        rdram.write_u8(color_addr_64 as u32, val);
                                    }
                                    1 => { // 16-bit color image size
                                        let pixel_color = (((r * 31.0) as u16) << 11)
                                            | (((g * 31.0) as u16) << 6)
                                            | (((b * 31.0) as u16) << 1)
                                            | 1;
                                        rdram.write_u16(color_addr_64 as u32, pixel_color);
                                    }
                                    2 => { // 32-bit color image size
                                        let pixel_color = (((r * 255.0) as u32) << 24)
                                            | (((g * 255.0) as u32) << 16)
                                            | (((b * 255.0) as u32) << 8)
                                            | 0xFF;
                                        rdram.write_u32(color_addr_64 as u32, pixel_color);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
