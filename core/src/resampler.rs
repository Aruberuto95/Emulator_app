/// Output sample rate used until the host reports its audio device's real one.
///
/// The host should always report: opening an SDL device with `allowed_changes = 0`
/// makes `obtained` echo `desired`, so a device actually running at 48 kHz still
/// reports 44100 while SDL resamples every queued block behind the app's back.
/// Producing at the device's own rate removes that hidden stage entirely.
pub const DEFAULT_OUTPUT_HZ: u32 = 44_100;

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

// Output low-pass cutoff. Everything a GBA DirectSound stream emits above ~9 kHz is
// resampling artifact (source Nyquist at the usual 13-18 kHz FIFO timer rates), and the
// real consoles' analog output stages roll off the top end anyway — so the filter only
// removes junk while staying ≤ -1.5 dB through 12 kHz (GBC chiptune brightness intact).
// Single tuning knob: drop to ~13 kHz if residual fizz is still audible.
const LPF_CUTOFF_HZ: f64 = 15_000.0;
const LPF_Q: f64 = std::f64::consts::FRAC_1_SQRT_2; // Butterworth (maximally flat)

/// Second-order IIR low-pass (RBJ cookbook), Direct Form I, run per output channel on
/// the finished 44.1 kHz samples — the output rate never changes with emulation speed
/// or GBC double-speed (only `cycles_per_sample` scales upstream), so the coefficients
/// are fixed-valid by construction, same argument as `DcBlocker`. It attenuates the
/// 12-22 kHz mirror band where ZOH/PSG fold-back lands (~-3 dB @ 15 k, -6 dB @ 20 k).
#[derive(Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    fn lowpass(fs: f64, fc: f64, q: f64) -> Self {
        let w0 = std::f64::consts::TAU * fc / fs;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - cos) / 2.0 / a0,
            b1: (1.0 - cos) / a0,
            b2: (1.0 - cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

#[derive(Clone, Copy)]
pub struct BoxResampler {
    pub cycle_accumulator: f64,
    pub left_sum: f64,
    pub right_sum: f64,
    pub sample_count: usize,
    // DC blockers run on the finished output samples, so the fixed R coefficient is correct
    // by construction. State is transient (settles in ~1-2 ms) but is carried in binary
    // snapshots anyway (see the `Snap` impl below), so a restore cannot start with a
    // filter transient the running machine had already settled past.
    dc_l: DcBlocker,
    dc_r: DcBlocker,
    // Output low-pass, same finished-sample placement and transient-state policy.
    lp_l: Biquad,
    lp_r: Biquad,
    /// Rate the two biquads above are designed for. Held so a host rate change
    /// rebuilds them instead of leaving a 15 kHz filter cutting at 16.3 kHz.
    output_hz: f64,
}

impl BoxResampler {
    pub fn new() -> Self {
        Self::with_output_rate(DEFAULT_OUTPUT_HZ)
    }

    pub fn with_output_rate(hz: u32) -> Self {
        let fs = f64::from(hz.max(1));
        Self {
            cycle_accumulator: 0.0,
            left_sum: 0.0,
            right_sum: 0.0,
            sample_count: 0,
            dc_l: DcBlocker::new(),
            dc_r: DcBlocker::new(),
            lp_l: Biquad::lowpass(fs, LPF_CUTOFF_HZ, LPF_Q),
            lp_r: Biquad::lowpass(fs, LPF_CUTOFF_HZ, LPF_Q),
            output_hz: fs,
        }
    }

    /// The rate this resampler emits at. Single source of truth: every console's
    /// `cycles_per_sample` divides its bus clock by this, so the whole pipeline
    /// follows the host device without threading a rate through each APU call.
    #[inline]
    pub fn output_hz(&self) -> f64 {
        self.output_hz
    }

    /// Retune the output filters for a new host sample rate. A no-op when the
    /// rate is unchanged, so it is safe to call on every device (re)open.
    pub fn set_output_rate(&mut self, hz: u32) {
        let fs = f64::from(hz.max(1));
        if (fs - self.output_hz).abs() < f64::EPSILON {
            return;
        }
        self.output_hz = fs;
        self.lp_l = Biquad::lowpass(fs, LPF_CUTOFF_HZ, LPF_Q);
        self.lp_r = Biquad::lowpass(fs, LPF_CUTOFF_HZ, LPF_Q);
    }

    pub fn reset(&mut self) {
        self.cycle_accumulator = 0.0;
        self.left_sum = 0.0;
        self.right_sum = 0.0;
        self.sample_count = 0;
        self.dc_l.reset();
        self.dc_r.reset();
        self.lp_l.reset();
        self.lp_r.reset();
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
        // A rate at or below zero would let this loop emit samples without ever
        // consuming `remaining_cycles`. Nothing legitimate lands here — every
        // console divides a MHz bus clock by an 8 kHz+ output rate, with `speed`
        // bounded by `Emulator::set_speed` — so this is purely about the loop
        // being provably bounded no matter what a caller computes.
        let cycles_per_sample = if cycles_per_sample > 0.0 { cycles_per_sample } else { 1.0 };
        let mut remaining_cycles = cycles as f64;
        while self.cycle_accumulator + remaining_cycles >= cycles_per_sample {
            // Clamped at zero. The loop's invariant is "accumulator <
            // cycles_per_sample", but that only holds while the rate is fixed —
            // and it is not: `cycles_per_sample` scales with emulation speed, so
            // dropping from 4.0x to 0.5x mid-frame shrinks it ~8x while the
            // accumulator still holds the larger rate's partial window. `needed`
            // then went negative, subtracting the current mix out of the running
            // sums and *increasing* `remaining_cycles`, which emitted a burst of
            // wrongly-weighted samples on every speed change.
            let needed = (cycles_per_sample - self.cycle_accumulator).max(0.0);
            self.left_sum += curr_left * needed;
            self.right_sum += curr_right * needed;

            let avg_l = self.lp_l.process(self.dc_l.process(self.left_sum / cycles_per_sample));
            let avg_r = self.lp_r.process(self.dc_r.process(self.right_sum / cycles_per_sample));

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

impl crate::snapshot::Snap for DcBlocker {
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.prev_in.snap(v);
        self.prev_out.snap(v);
    }
}

impl crate::snapshot::Snap for Biquad {
    /// Delay line only: the coefficients are a pure function of the output rate,
    /// which [`BoxResampler::snap`] restores by rebuilding both filters.
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.x1.snap(v);
        self.x2.snap(v);
        self.y1.snap(v);
        self.y2.snap(v);
    }
}

impl crate::snapshot::Snap for BoxResampler {
    /// `output_hz` is deliberately NOT carried. It is a property of the *host
    /// audio device*, like emulation speed — not of the emulated machine.
    ///
    /// Carrying it was measured to break the pipeline: a state captured on a
    /// 44.1 kHz build restored into a 48 kHz session left this resampler
    /// emitting 735 stereo frames per tick while the frontend's device consumed
    /// 800, so the queue drained faster than it filled — audible as stutter,
    /// with the emulation forced ~9% fast to keep up.
    fn snap(&mut self, v: &mut dyn crate::snapshot::Visitor) {
        self.cycle_accumulator.snap(v);
        self.left_sum.snap(v);
        self.right_sum.snap(v);
        self.sample_count.snap(v);
        self.dc_l.snap(v);
        self.dc_r.snap(v);
        self.lp_l.snap(v);
        self.lp_r.snap(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowpass_attenuates_near_nyquist_keeps_passband() {
        // 18 kHz (mirror-band junk) must be strongly cut while 1 kHz (musical content)
        // passes essentially untouched. The bilinear-transform biquad has a zero at
        // Nyquist, so near-Nyquist attenuation is steeper than the analog Butterworth
        // 1/sqrt(1+(f/fc)^4) estimate: |H(18k)| ~= 0.28, not ~0.57 — strictly better
        // artifact suppression. The warp works the other way in the passband (10-12 kHz
        // is ~0.2-0.7 dB down, less than analog), so GBC brightness is preserved.
        // RMS is measured past the settling transient.
        let rms_ratio = |freq_hz: f64| -> f64 {
            let mut f = Biquad::lowpass(44_100.0, LPF_CUTOFF_HZ, LPF_Q);
            let w = std::f64::consts::TAU * freq_hz / 44_100.0;
            let (mut in_sq, mut out_sq) = (0.0, 0.0);
            for n in 0..1000 {
                let x = (w * n as f64).sin();
                let y = f.process(x);
                if n >= 200 {
                    in_sq += x * x;
                    out_sq += y * y;
                }
            }
            (out_sq / in_sq).sqrt()
        };
        let hi = rms_ratio(18_000.0);
        assert!((0.20..=0.36).contains(&hi), "18 kHz ratio {hi}, expected ~0.28");
        let lo = rms_ratio(1_000.0);
        assert!(lo >= 0.98, "1 kHz ratio {lo}, passband must stay ~unity");
    }

    #[test]
    fn lowpass_unity_dc_gain() {
        // Pins the a0 normalization: (b0+b1+b2)/(1+a1+a2) must be exactly 1.
        let mut f = Biquad::lowpass(44_100.0, LPF_CUTOFF_HZ, LPF_Q);
        let mut y = 0.0;
        for _ in 0..200 {
            y = f.process(1.0);
        }
        assert!((y - 1.0).abs() < 1e-6, "DC gain must be unity, converged to {y}");
    }

    /// The host rate is the single source of truth every console's
    /// `cycles_per_sample` divides by, so it must survive construction and be
    /// retunable in place; a stale rate here silently detunes the output filter
    /// and, worse, leaves the core producing at a rate the device does not run
    /// at (the case SDL papers over with a hidden resampler).
    #[test]
    fn output_rate_is_reported_and_retunable() {
        let mut r = BoxResampler::new();
        assert_eq!(r.output_hz(), f64::from(DEFAULT_OUTPUT_HZ));

        r.set_output_rate(48_000);
        assert_eq!(r.output_hz(), 48_000.0);

        // Retuning must actually rebuild the filter, not just record the rate:
        // a 15 kHz corner designed at 44.1 kHz sits at ~16.3 kHz when run at
        // 48 kHz. Compare the steady-state gain at the cutoff against a filter
        // built for 48 kHz from the start.
        let mut fresh = BoxResampler::with_output_rate(48_000);
        let drive = |b: &mut BoxResampler| {
            let mut last = 0.0;
            for n in 0..4096 {
                let t = std::f64::consts::TAU * LPF_CUTOFF_HZ * f64::from(n) / 48_000.0;
                last = b.lp_l.process(t.sin());
            }
            last
        };
        let (a, b) = (drive(&mut r), drive(&mut fresh));
        assert!((a - b).abs() < 1e-9, "retuned filter must match a fresh one: {a} vs {b}");
    }

    #[test]
    fn lowpass_impulse_response_decays() {
        // Pins pole stability (both poles strictly inside the unit circle).
        let mut f = Biquad::lowpass(44_100.0, LPF_CUTOFF_HZ, LPF_Q);
        let mut y = f.process(1.0);
        for _ in 0..500 {
            y = f.process(0.0);
        }
        assert!(y.abs() < 1e-9, "impulse response must decay, still at {y}");
    }
}
