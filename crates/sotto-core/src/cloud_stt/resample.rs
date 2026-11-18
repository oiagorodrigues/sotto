//! Streaming 16 kHz to 24 kHz conversion (ratio 3:2) with linear interpolation. Speech has no
//! content near 8 kHz that matters for recognition, so a linear filter is enough, and the
//! converter stays pure Rust with no dependency.

/// Output sample `k` sits at input position `2k/3`: `floor(2k/3)` picks the left neighbour and
/// `2k mod 3` thirds is the distance to the right one.
#[derive(Debug, Default)]
pub struct Resampler16To24 {
    next_out: u64,
    consumed: u64,
    last: i16,
}

impl Resampler16To24 {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends the converted samples to `out`. The converter holds back nothing beyond the one
    /// sample it needs for the next interpolation.
    pub fn push(&mut self, input: &[i16], out: &mut Vec<i16>) {
        for &sample in input {
            let j = self.consumed;
            if j > 0 {
                while (2 * self.next_out) / 3 == j - 1 {
                    let frac = i32::try_from((2 * self.next_out) % 3).unwrap_or(0);
                    let value = (i32::from(self.last) * (3 - frac) + i32::from(sample) * frac) / 3;
                    out.push(i16::try_from(value).unwrap_or(0));
                    self.next_out += 1;
                }
            }
            self.last = sample;
            self.consumed += 1;
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Convenience for tests and one-shot use.
#[must_use]
pub fn resample_16k_to_24k(input: &[i16]) -> Vec<i16> {
    let mut r = Resampler16To24::new();
    let mut out = Vec::with_capacity(input.len() * 3 / 2 + 2);
    r.push(input, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: f64, n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| {
                let v = (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin();
                (v * 10_000.0).round() as i16
            })
            .collect()
    }

    #[test]
    fn output_length_is_three_halves_of_the_input() {
        for n in [2usize, 3, 160, 1_600, 16_000] {
            let out = resample_16k_to_24k(&vec![100; n]);
            let expected = (n - 1) * 3 / 2;
            assert!(
                out.len().abs_diff(expected) <= 1,
                "{n}: {} vs {expected}",
                out.len()
            );
        }
    }

    #[test]
    fn constant_signals_stay_constant() {
        assert!(resample_16k_to_24k(&[1234; 300]).iter().all(|&s| s == 1234));
        assert!(
            resample_16k_to_24k(&[-32768; 300])
                .iter()
                .all(|&s| s == -32768)
        );
    }

    #[test]
    fn interpolates_between_neighbours() {
        // Input 0, 300, 600: outputs at positions 0, 2/3, 4/3 are 0, 200, 400.
        let out = resample_16k_to_24k(&[0, 300, 600, 900]);
        assert_eq!(&out[..3], &[0, 200, 400]);
    }

    #[test]
    fn a_sine_keeps_its_frequency_and_amplitude() {
        let out = resample_16k_to_24k(&sine(440.0, 16_000.0, 16_000));
        let expected = sine(440.0, 24_000.0, out.len());
        let worst = out
            .iter()
            .zip(&expected)
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
            .max()
            .unwrap_or(0);
        assert!(worst < 120, "worst error {worst}");
    }

    #[test]
    fn chunked_input_matches_one_shot_for_any_split() {
        let input = sine(300.0, 16_000.0, 997);
        let whole = resample_16k_to_24k(&input);
        for chunk in [1usize, 2, 3, 7, 320, 500] {
            let mut r = Resampler16To24::new();
            let mut out = Vec::new();
            for part in input.chunks(chunk) {
                r.push(part, &mut out);
            }
            assert_eq!(out, whole, "chunk size {chunk}");
        }
    }

    #[test]
    fn extremes_do_not_overflow() {
        let out = resample_16k_to_24k(&[i16::MAX, i16::MIN, i16::MAX, i16::MIN]);
        assert!(!out.is_empty());
    }
}
