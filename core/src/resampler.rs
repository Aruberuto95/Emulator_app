#[derive(Clone, Copy)]
pub struct BoxResampler {
    pub cycle_accumulator: f64,
    pub left_sum: f64,
    pub right_sum: f64,
    pub sample_count: usize,
}

impl BoxResampler {
    pub fn new() -> Self {
        Self {
            cycle_accumulator: 0.0,
            left_sum: 0.0,
            right_sum: 0.0,
            sample_count: 0,
        }
    }

    pub fn reset(&mut self) {
        self.cycle_accumulator = 0.0;
        self.left_sum = 0.0;
        self.right_sum = 0.0;
        self.sample_count = 0;
    }

    pub fn tick(
        &mut self,
        cycles: u32,
        curr_left: f64,
        curr_right: f64,
        cycles_per_sample: f64,
        audio_buffer: &mut [i16],
        audio_offset: usize,
    ) {
        let mut remaining_cycles = cycles as f64;
        while self.cycle_accumulator + remaining_cycles >= cycles_per_sample {
            let needed = cycles_per_sample - self.cycle_accumulator;
            self.left_sum += curr_left * needed;
            self.right_sum += curr_right * needed;

            let avg_l = self.left_sum / cycles_per_sample;
            let avg_r = self.right_sum / cycles_per_sample;

            let val_l = (avg_l * 30000.0).clamp(-32768.0, 32767.0) as i16;
            let val_r = (avg_r * 30000.0).clamp(-32768.0, 32767.0) as i16;

            let buffer_idx = audio_offset + self.sample_count * 2;
            if buffer_idx + 1 < audio_buffer.len() {
                audio_buffer[buffer_idx] = val_l;
                audio_buffer[buffer_idx + 1] = val_r;
            }
            self.sample_count += 1;
            remaining_cycles -= needed;
            self.cycle_accumulator = 0.0;
            self.left_sum = 0.0;
            self.right_sum = 0.0;
        }

        if remaining_cycles > 0.0 {
            self.left_sum += curr_left * remaining_cycles;
            self.right_sum += curr_right * remaining_cycles;
            self.cycle_accumulator += remaining_cycles;
        }
    }
}
