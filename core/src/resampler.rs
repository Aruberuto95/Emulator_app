/// Single-pole DC-blocking high-pass, one per output channel: `y[n] = x[n] - x[n-1] + R*y[n-1]`.
/// Real GB/GBA hardware AC-couples the DAC + speaker; without it, duty-dependent DC (a 12.5%
/// square averages -0.75*v) and channel enable/disable steps leak through as offset and clicks.
/// `R = 0.996` puts the corner near 28 Hz at 44.1 kHz — below the audio band, above DC/thump.
#[derive(Clone, Copy)]
pub struct DcBlocker {
    prev_in: f64,
    prev_out: f64,
}

impl DcBlocker {
    const R: f64 = 0.996;

    pub fn new() -> Self {
        Self {
            prev_in: 0.0,
            prev_out: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.prev_in = 0.0;
        self.prev_out = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let y = x - self.prev_in + Self::R * self.prev_out;
        self.prev_in = x;
        self.prev_out = y;
        y
    }
}

#[derive(Clone, Copy)]
pub struct BoxResampler {
    pub cycle_accumulator: f64,
    pub left_sum: f64,
    pub right_sum: f64,
    pub sample_count: usize,
    // DC blockers run on the finished 44.1 kHz samples, so the fixed R coefficient is correct
    // by construction. State is transient (settles in ~1-2 ms) and, like the rest of the
    // resampler, is intentionally not serialized in savestates.
    dc_l: DcBlocker,
    dc_r: DcBlocker,
}

impl BoxResampler {
    pub fn new() -> Self {
        Self {
            cycle_accumulator: 0.0,
            left_sum: 0.0,
            right_sum: 0.0,
            sample_count: 0,
            dc_l: DcBlocker::new(),
            dc_r: DcBlocker::new(),
        }
    }

    pub fn reset(&mut self) {
        self.cycle_accumulator = 0.0;
        self.left_sum = 0.0;
        self.right_sum = 0.0;
        self.sample_count = 0;
        self.dc_l.reset();
        self.dc_r.reset();
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

            let avg_l = self.dc_l.process(self.left_sum / cycles_per_sample);
            let avg_r = self.dc_r.process(self.right_sum / cycles_per_sample);

            let val_l = (avg_l * 30000.0).clamp(-32768.0, 32767.0) as i16;
            let val_r = (avg_r * 30000.0).clamp(-32768.0, 32767.0) as i16;

            let buffer_idx = audio_offset + self.sample_count * 2;
            // Release builds truncate silently (buffer is 4x oversized, never fires
            // legitimately); surface any regression loudly in debug builds.
            debug_assert!(
                buffer_idx + 1 < audio_buffer.len(),
                "resampler overflow: sample_count={} exceeds audio buffer; output truncated",
                self.sample_count
            );
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
