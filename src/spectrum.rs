use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use std::sync::Arc;

pub const FFT_SIZE: usize = 4096;
pub const BANDS: usize = 64;

/// Half-open FFT bin ranges; adjacent narrow bands may share a bin.
pub fn band_ranges(rate: u32, count: usize) -> Vec<std::ops::Range<usize>> {
    let low = 35.0_f32;
    let high = (rate as f32 * 0.48).min(16_000.0);
    (0..count)
        .map(|i| {
            let edge = |n: usize| low * (high / low).powf(n as f32 / count as f32);
            // Include the nearest FFT bin at each edge, including sub-bin bass bands.
            let start = ((edge(i) * FFT_SIZE as f32 / rate as f32).round() as usize)
                .clamp(1, FFT_SIZE / 2 - 1);
            let end = ((edge(i + 1) * FFT_SIZE as f32 / rate as f32).round() as usize + 1)
                .clamp(start + 1, FFT_SIZE / 2 + 1);
            start..end
        })
        .collect()
}

pub struct Spectrum {
    fft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex32>,
    bins: Vec<Complex32>,
    window: Vec<f32>,
    ranges: Vec<std::ops::Range<usize>>,
    pub heights: Vec<f32>,
    targets: Vec<f32>,
    rate: u32,
    sensitivity: f32,
    bass_average: f32,
    bass_energy: f32,
    cooldown: f32,
    warmup: f32,
}

impl Spectrum {
    pub fn new(rate: u32, sensitivity: f32) -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);
        let scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
        Self {
            fft,
            scratch,
            bins: vec![Complex32::default(); FFT_SIZE],
            window: (0..FFT_SIZE)
                .map(|i| {
                    0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (FFT_SIZE - 1) as f32).cos()
                })
                .collect(),
            ranges: band_ranges(rate, BANDS),
            heights: vec![0.0; BANDS],
            targets: vec![0.0; BANDS],
            rate,
            sensitivity,
            bass_average: 0.0,
            bass_energy: 0.0,
            cooldown: 0.0,
            warmup: 0.0,
        }
    }

    /// New windows are optional: smoothing and the beat clock still advance in silence.
    pub fn update(&mut self, samples: Option<&[f32]>, silent: bool, dt: f32) -> bool {
        let mut beat = false;
        self.cooldown = (self.cooldown - dt).max(0.0);
        self.warmup += dt;
        if silent {
            self.targets.fill(0.0);
            self.bass_energy = 0.0;
        } else if let Some(samples) = samples {
            let mean = samples.iter().sum::<f32>() / FFT_SIZE as f32;
            for ((bin, sample), window) in self.bins.iter_mut().zip(samples).zip(&self.window) {
                *bin = Complex32::new((sample - mean) * window, 0.0);
            }
            self.fft
                .process_with_scratch(&mut self.bins, &mut self.scratch);
            // Hann coherent gain is 1/2; double the positive-frequency half.
            let scale = 4.0 / FFT_SIZE as f32;
            for (target, range) in self.targets.iter_mut().zip(&self.ranges) {
                let power: f32 = self.bins[range.clone()].iter().map(|v| v.norm_sqr()).sum();
                let magnitude = (power / range.len() as f32).sqrt() * scale * self.sensitivity;
                let db = 20.0 * magnitude.max(1e-8).log10();
                *target = ((db + 65.0) / 65.0).clamp(0.0, 1.0);
            }
            let bass_end = ((180.0 * FFT_SIZE as f32 / self.rate as f32) as usize).max(2);
            let energy = self.bins[1..=bass_end]
                .iter()
                .map(|v| v.norm_sqr())
                .sum::<f32>()
                * scale
                * scale;
            self.bass_energy = energy;
            beat = self.warmup > 0.4
                && self.cooldown == 0.0
                && energy > 0.002
                && energy > self.bass_average * 1.65;
            if beat {
                self.cooldown = 0.24;
            }
        }
        // Audio can arrive less often than rendering. Adapt on elapsed wall time,
        // holding the most recent energy between FFT windows.
        let bass_tau = if silent { 0.8 } else { 0.65 };
        self.bass_average +=
            (self.bass_energy - self.bass_average) * (1.0 - (-dt / bass_tau).exp());
        for (height, target) in self.heights.iter_mut().zip(&self.targets) {
            let tau = if target > height { 0.035 } else { 0.24 };
            *height += (target - *height) * (1.0 - (-dt / tau).exp());
        }
        beat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logarithmic_ranges_cover_tones_at_supported_rates() {
        for rate in [8_000, 44_100, 48_000, 192_000] {
            let ranges = band_ranges(rate, BANDS);
            assert!(ranges
                .iter()
                .all(|r| r.start >= 1 && r.end <= FFT_SIZE / 2 + 1 && !r.is_empty()));
            assert!(ranges
                .windows(2)
                .all(|p| p[0].start <= p[1].start && p[0].end >= p[1].start));
            let high = (rate as f32 * 0.48).min(16_000.0);
            for frequency in [80.0_f32, 220.0, 1_000.0, 3_000.0] {
                let band = ((frequency / 35.0).ln() / (high / 35.0).ln() * BANDS as f32) as usize;
                let bin = (frequency * FFT_SIZE as f32 / rate as f32).round() as usize;
                assert!(
                    ranges[band].contains(&bin.max(1)),
                    "rate {rate}, frequency {frequency}"
                );
            }
        }
    }

    #[test]
    fn fft_tones_peak_in_their_logarithmic_bands() {
        for frequency in [220.0_f32, 1_000.0, 6_000.0] {
            let mut analyzer = Spectrum::new(48_000, 1.0);
            let samples: Vec<f32> = (0..FFT_SIZE)
                .map(|i| 0.5 * (std::f32::consts::TAU * frequency * i as f32 / 48_000.0).sin())
                .collect();
            analyzer.update(Some(&samples), false, 0.05);
            let peak = analyzer
                .heights
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap()
                .0;
            let bin = (frequency * FFT_SIZE as f32 / 48_000.0).round() as usize;
            assert!(
                analyzer.ranges[peak].contains(&bin),
                "frequency {frequency}, peak band {peak}"
            );
        }
    }

    #[test]
    fn silence_decays_and_dc_does_not_trigger_beats() {
        let mut analyzer = Spectrum::new(48_000, 1.0);
        let samples = [0.5; FFT_SIZE];
        for _ in 0..90 {
            assert!(!analyzer.update(Some(&samples), false, 1.0 / 60.0));
        }
        assert!(analyzer.heights.iter().all(|&h| h < 0.001));
        analyzer.heights.fill(1.0);
        for _ in 0..120 {
            analyzer.update(None, true, 1.0 / 60.0);
        }
        assert!(analyzer.heights.iter().all(|&h| h < 0.001));
    }

    #[test]
    fn bass_onset_triggers_once_within_cooldown() {
        let mut analyzer = Spectrum::new(48_000, 1.0);
        for _ in 0..60 {
            analyzer.update(None, true, 1.0 / 60.0);
        }
        let tone: Vec<f32> = (0..FFT_SIZE)
            .map(|i| 0.5 * (std::f32::consts::TAU * 70.0 * i as f32 / 48_000.0).sin())
            .collect();
        assert!(analyzer.update(Some(&tone), false, 1.0 / 60.0));
        for _ in 0..12 {
            assert!(!analyzer.update(Some(&tone), false, 1.0 / 60.0));
        }
    }

    #[test]
    fn regression_chunked_sustained_bass_stops_retriggering_beats() {
        let mut analyzer = Spectrum::new(48_000, 1.0);
        let dt = 1.0 / 120.0;
        for _ in 0..120 {
            analyzer.update(None, true, dt);
        }
        let tone: Vec<f32> = (0..FFT_SIZE)
            .map(|i| 0.5 * (std::f32::consts::TAU * 70.0 * i as f32 / 48_000.0).sin())
            .collect();
        let mut late_beats = 0;
        for frame in 0..240 {
            // A real-time producer writes one PCM block every 100 ms.
            let samples = (frame % 12 == 0).then_some(tone.as_slice());
            if analyzer.update(samples, false, dt) && frame >= 120 {
                late_beats += 1;
            }
        }
        assert_eq!(late_beats, 0, "a sustained bass tone is not a new onset");
    }
}
