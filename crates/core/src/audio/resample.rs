use std::collections::VecDeque;

/// Streaming mono resampler: box-filter anti-aliasing followed by linear
/// interpolation. Speech-to-text quality is insensitive to the small
/// passband ripple, and it keeps the dependency tree small.
pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Fractional read position into `filtered`.
    pos: f64,
    filtered: Vec<f32>,
    window: VecDeque<f32>,
    window_len: usize,
    window_sum: f32,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        let step = from_rate as f64 / to_rate as f64;
        Self {
            step,
            pos: 0.0,
            filtered: Vec::new(),
            window: VecDeque::new(),
            window_len: step.round().max(1.0) as usize,
            window_sum: 0.0,
        }
    }

    pub fn is_passthrough(&self) -> bool {
        self.step == 1.0
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.is_passthrough() {
            out.extend_from_slice(input);
            return;
        }
        for &x in input {
            self.window.push_back(x);
            self.window_sum += x;
            if self.window.len() > self.window_len {
                self.window_sum -= self.window.pop_front().unwrap_or(0.0);
            }
            self.filtered.push(self.window_sum / self.window.len() as f32);
        }
        while self.pos + 1.0 < self.filtered.len() as f64 {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            out.push(self.filtered[i] * (1.0 - frac) + self.filtered[i + 1] * frac);
            self.pos += self.step;
        }
        let consumed = (self.pos as usize).min(self.filtered.len());
        self.filtered.drain(..consumed);
        self.pos -= consumed as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_length_tracks_rate_ratio_across_chunks() {
        let mut r = Resampler::new(48_000, 16_000);
        let mut out = Vec::new();
        for _ in 0..100 {
            r.process(&[0.5; 480], &mut out);
        }
        // 48_000 input samples -> ~16_000 output samples.
        assert!((15_990..=16_000).contains(&out.len()), "{}", out.len());
        assert!(out[100..].iter().all(|&s| (s - 0.5).abs() < 1e-5));
    }

    #[test]
    fn upsampling_interpolates() {
        let mut r = Resampler::new(8_000, 16_000);
        let mut out = Vec::new();
        r.process(&[0.0, 1.0, 0.0], &mut out);
        assert_eq!(out.len(), 4);
        assert!((out[1] - 0.5).abs() < 1e-6);
    }
}
