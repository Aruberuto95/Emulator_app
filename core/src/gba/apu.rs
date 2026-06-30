pub struct SoundFifo {
    pub buffer: [i8; 32],
    pub write_ptr: usize,
    pub read_ptr: usize,
    pub count: usize,
}

impl SoundFifo {
    pub fn new() -> Self {
        Self {
            buffer: [0; 32],
            write_ptr: 0,
            read_ptr: 0,
            count: 0,
        }
    }

    pub fn push(&mut self, val: i8) {
        if self.count < 32 {
            self.buffer[self.write_ptr] = val;
            self.write_ptr = (self.write_ptr + 1) % 32;
            self.count += 1;
        }
    }

    pub fn pop(&mut self) -> i8 {
        if self.count > 0 {
            let val = self.buffer[self.read_ptr];
            self.read_ptr = (self.read_ptr + 1) % 32;
            self.count -= 1;
            val
        } else {
            0
        }
    }

    pub fn clear(&mut self) {
        self.write_ptr = 0;
        self.read_ptr = 0;
        self.count = 0;
        self.buffer.fill(0);
    }
}

pub struct GbaApu {
    pub fifo_a: SoundFifo,
    pub fifo_b: SoundFifo,
    pub dma_request_a: bool,
    pub dma_request_b: bool,

    // Direct Sound A & B current sample output
    pub current_sample_a: i8,
    pub current_sample_b: i8,

    // Sound registers
    pub soundcnt_h: u16,
    pub soundcnt_x: u16,

    // Downsampling accumulator
    pub cycle_accumulator: f64,

    // Resampler
    pub resampler: crate::resampler::BoxResampler,
}

impl GbaApu {
    pub fn new() -> Self {
        Self {
            fifo_a: SoundFifo::new(),
            fifo_b: SoundFifo::new(),
            dma_request_a: false,
            dma_request_b: false,
            current_sample_a: 0,
            current_sample_b: 0,
            soundcnt_h: 0,
            soundcnt_x: 0x80, // enabled
            cycle_accumulator: 0.0,
            resampler: crate::resampler::BoxResampler::new(),
        }
    }

    pub fn write_register(&mut self, offset: u32, value: u8) {
        match offset {
            0x82 => {
                self.soundcnt_h = (self.soundcnt_h & 0xFF00) | (value as u16);
                if (value & 0x08) != 0 {
                    // Reset FIFO A
                    self.fifo_a.clear();
                }
            }
            0x83 => {
                self.soundcnt_h = (self.soundcnt_h & 0x00FF) | ((value as u16) << 8);
                if (value & 0x80) != 0 {
                    // Reset FIFO B
                    self.fifo_b.clear();
                }
            }
            0x84 => {
                self.soundcnt_x = (self.soundcnt_x & 0xFF00) | (value as u16);
            }
            0x85 => {
                self.soundcnt_x = (self.soundcnt_x & 0x00FF) | ((value as u16) << 8);
            }
            _ => {}
        }
    }

    pub fn write_fifo_a(&mut self, value: u32) {
        // Push 4 bytes of 32-bit word into FIFO A
        self.fifo_a.push((value & 0xFF) as i8);
        self.fifo_a.push(((value >> 8) & 0xFF) as i8);
        self.fifo_a.push(((value >> 16) & 0xFF) as i8);
        self.fifo_a.push(((value >> 24) & 0xFF) as i8);
    }

    pub fn write_fifo_b(&mut self, value: u32) {
        // Push 4 bytes of 32-bit word into FIFO B
        self.fifo_b.push((value & 0xFF) as i8);
        self.fifo_b.push(((value >> 8) & 0xFF) as i8);
        self.fifo_b.push(((value >> 16) & 0xFF) as i8);
        self.fifo_b.push(((value >> 24) & 0xFF) as i8);
    }

    pub fn on_timer_overflow(&mut self, timer_index: usize) {
        // Direct Sound A Timer Select: Bit 10 of SOUNDCNT_H (0=Timer 0, 1=Timer 1)
        let timer_a = if (self.soundcnt_h & 0x0400) != 0 {
            1
        } else {
            0
        };
        if timer_index == timer_a {
            self.current_sample_a = self.fifo_a.pop();
            // Trigger DMA request if FIFO has 16 or fewer bytes (4 words or fewer)
            if self.fifo_a.count <= 16 {
                self.dma_request_a = true;
            }
        }

        // Direct Sound B Timer Select: Bit 14 of SOUNDCNT_H (0=Timer 0, 1=Timer 1)
        let timer_b = if (self.soundcnt_h & 0x4000) != 0 {
            1
        } else {
            0
        };
        if timer_index == timer_b {
            self.current_sample_b = self.fifo_b.pop();
            if self.fifo_b.count <= 16 {
                self.dma_request_b = true;
            }
        }
    }

    pub fn tick(&mut self, cycles: u32, audio_buffer: &mut [i16], audio_offset: usize, speed: f32) {
        if (self.soundcnt_x & 0x80) == 0 {
            // APU disabled: output silence
            return;
        }

        let cycles_per_sample = (16777216.0 * speed as f64) / 44100.0;

        // Direct Sound A
        let dsa_l = (self.soundcnt_h & 0x0200) != 0;
        let dsa_r = (self.soundcnt_h & 0x0100) != 0;
        let dsa_vol_100 = (self.soundcnt_h & 0x0004) != 0;
        let sample_a = (self.current_sample_a as f32) / 128.0;
        let scaled_a = sample_a * (if dsa_vol_100 { 1.0 } else { 0.5 });

        // Direct Sound B
        let dsb_l = (self.soundcnt_h & 0x2000) != 0;
        let dsb_r = (self.soundcnt_h & 0x1000) != 0;
        let dsb_vol_100 = (self.soundcnt_h & 0x0008) != 0;
        let sample_b = (self.current_sample_b as f32) / 128.0;
        let scaled_b = sample_b * (if dsb_vol_100 { 1.0 } else { 0.5 });

        // Direct Sound mixing
        let mut left_ds = 0.0;
        let mut right_ds = 0.0;

        if dsa_l {
            left_ds += scaled_a;
        }
        if dsa_r {
            right_ds += scaled_a;
        }
        if dsb_l {
            left_ds += scaled_b;
        }
        if dsb_r {
            right_ds += scaled_b;
        }

        self.resampler.tick(
            cycles,
            left_ds as f64,
            right_ds as f64,
            cycles_per_sample,
            audio_buffer,
            audio_offset,
        );
    }
}
