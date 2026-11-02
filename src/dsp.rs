//! Spectrum analysis and bar dynamics.

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex, num_complex::Complex};

/// Short window for responsive mids and treble.
const SHORT_FFT: usize = 2048;
/// Long window for narrow bass bands that a short FFT cannot resolve.
const LONG_FFT: usize = 4096;
/// Samples the analyzer needs per frame.
pub const WINDOW: usize = LONG_FFT;

/// One windowed FFT with a prefix sum of bin power.
struct Fft {
    plan: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    output: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    cumulative: Vec<f64>,
    norm: f64,
}

impl Fft {
    fn new(size: usize) -> Self {
        let plan = RealFftPlanner::<f32>::new().plan_fft_forward(size);
        let window: Vec<f32> = (0..size)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (size - 1) as f32).cos())
            .collect();
        // Parseval scaling: a full-scale sine reads about -3 dB in its band.
        let norm = size as f64 * window.iter().map(|w| (*w as f64).powi(2)).sum::<f64>() / 2.0;
        Self {
            input: plan.make_input_vec(),
            output: plan.make_output_vec(),
            scratch: plan.make_scratch_vec(),
            cumulative: vec![0.0; size / 2 + 2],
            plan,
            window,
            norm,
        }
    }

    fn bins(&self) -> usize {
        self.output.len()
    }

    /// Analyzes the newest `size` samples.
    fn run(&mut self, samples: &[f32]) {
        let tail = &samples[samples.len().saturating_sub(self.input.len())..];
        for ((dst, s), w) in self.input.iter_mut().zip(tail).zip(&self.window) {
            *dst = s * w;
        }
        if self.plan.process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch).is_err() {
            self.cumulative.fill(0.0);
            return;
        }
        let mut acc = 0.0f64;
        for (i, c) in self.output.iter().enumerate() {
            acc += c.norm_sqr() as f64;
            self.cumulative[i + 1] = acc;
        }
    }

    fn band_power(&self, lo: usize, hi: usize) -> f64 {
        (self.cumulative[hi] - self.cumulative[lo]) / self.norm
    }
}

struct Band {
    long: bool,
    lo: usize,
    hi: usize,
    tilt_db: f32,
}

/// FFT power summed into log-spaced bands, in dB relative to full scale.
pub struct Spectrum {
    short: Fft,
    long: Fft,
    bands: Vec<Band>,
    needs_long: bool,
}

impl Spectrum {
    pub fn new(sample_rate: u32, n_bands: usize, fmin: f32, fmax: f32, tilt_db: f32) -> Self {
        let short = Fft::new(SHORT_FFT);
        let long = Fft::new(LONG_FFT);
        let rate = sample_rate as f32;
        let fmax = fmax.min(rate / 2.0 * 0.95);
        let fmin = fmin.min(fmax / 2.0);
        let edge = |i: usize| fmin * (fmax / fmin).powf(i as f32 / n_bands as f32);
        let short_bin = rate / SHORT_FFT as f32;

        let bands: Vec<Band> = (0..n_bands)
            .map(|i| {
                let (f0, f1) = (edge(i), edge(i + 1));
                let long_fft = f1 - f0 < 2.0 * short_bin;
                let (fft, size) = if long_fft { (&long, LONG_FFT) } else { (&short, SHORT_FFT) };
                let bin_hz = rate / size as f32;
                // Index of the first bin at or above the frequency.
                let first_bin = |f: f32| ((f / bin_hz).ceil() as usize).min(fft.bins());
                // Narrow bands may fall between bins: give each band at least one bin.
                let lo = first_bin(f0).min(fft.bins() - 1);
                let hi = first_bin(f1).max(lo + 1);
                Band { long: long_fft, lo, hi, tilt_db: tilt_db * ((f0 * f1).sqrt() / 1000.0).log2() }
            })
            .collect();
        let needs_long = bands.iter().any(|b| b.long);
        Self { short, long, bands, needs_long }
    }

    /// `samples` holds the newest `WINDOW` samples.
    pub fn bands_db(&mut self, samples: &[f32], out: &mut [f32]) {
        self.short.run(samples);
        if self.needs_long {
            self.long.run(samples);
        }
        for (band, dst) in self.bands.iter().zip(out.iter_mut()) {
            let fft = if band.long { &self.long } else { &self.short };
            *dst = (10.0 * (fft.band_power(band.lo, band.hi) + 1e-12).log10()) as f32 + band.tilt_db;
        }
    }
}

pub struct DynamicsConfig {
    pub range_db: f32,
    pub floor_db: f32,
    pub headroom_db: f32,
    pub equalize: f32,
}

/// Auto gain, per-band equalization and bar smoothing. Values are 0..1.
pub struct BarDynamics {
    pub config: DynamicsConfig,
    ref_db: f32,
    band_avg: Option<Vec<f32>>,
    corrected: Vec<f32>,
    pub bars: Vec<f32>,
}

// Slow in both directions, so the loudest band swings above and below the reference
// and headroom sets how often bars reach the top row.
const REF_ATTACK_SEC: f32 = 1.0;
const REF_RELEASE_SEC: f32 = 2.0;
/// Limits how far a sudden loud passage can overshoot the reference.
const REF_MAX_LAG_DB: f32 = 10.0;
const EQ_AVERAGE_SEC: f32 = 4.0;
const EQ_MAX_DB: f32 = 30.0;
/// Bands whose average is this far below the loudest band's are treated as empty.
const EQ_DEAD_BELOW_LOUDEST_DB: f32 = 50.0;
/// Below this a band holds no signal at all, only rounding noise.
const DIGITAL_SILENCE_DB: f32 = -100.0;
const ATTACK_TAU_SEC: f32 = 0.012;
const BAR_FALL_PER_SEC: f32 = 2.2;

fn smoothing(dt: f32, tau: f32) -> f32 {
    1.0 - (-dt / tau).exp()
}

impl BarDynamics {
    pub fn new(n: usize, config: DynamicsConfig) -> Self {
        Self {
            ref_db: config.floor_db + config.range_db,
            config,
            band_avg: None,
            corrected: vec![0.0; n],
            bars: vec![0.0; n],
        }
    }

    pub fn update(&mut self, levels_db: &[f32], dt: f32) {
        let c = &self.config;
        let top = levels_db.iter().copied().fold(f32::MIN, f32::max);

        // Pull each band towards the average of all bands, judged over a few seconds,
        // so quiet sub-bass and treble columns move as much as the mids. Averages are
        // only updated while something plays.
        if top > c.floor_db {
            let avg = self.band_avg.get_or_insert_with(|| levels_db.to_vec());
            let k = smoothing(dt, EQ_AVERAGE_SEC);
            for (a, l) in avg.iter_mut().zip(levels_db) {
                *a += (l.max(DIGITAL_SILENCE_DB) - *a) * k;
            }
        }
        // A band far below the loudest one carries no real content (e.g. above a codec's
        // cutoff); it neither counts towards the target nor gets lifted, so it stays dark.
        let plan = self.band_avg.as_ref().and_then(|avg| {
            let loudest = avg.iter().copied().fold(f32::MIN, f32::max);
            let alive_from = (loudest - EQ_DEAD_BELOW_LOUDEST_DB).max(DIGITAL_SILENCE_DB);
            let (sum, n) = avg.iter().filter(|a| **a > alive_from).fold((0.0, 0), |(s, n), a| (s + a, n + 1));
            (n > 0).then(|| (sum / n as f32, alive_from))
        });
        match (&self.band_avg, plan) {
            (Some(avg), Some((mean, alive_from))) if c.equalize > 0.0 => {
                for ((dst, l), a) in self.corrected.iter_mut().zip(levels_db).zip(avg) {
                    let boost =
                        if *a > alive_from { (mean - a).clamp(-EQ_MAX_DB, EQ_MAX_DB) * c.equalize } else { 0.0 };
                    *dst = if *l > DIGITAL_SILENCE_DB { l + boost } else { *l };
                }
            }
            _ => self.corrected.copy_from_slice(levels_db),
        }

        // Reference is a slow average of the loudest band; headroom lifts the top row above it.
        let top = self.corrected.iter().copied().fold(f32::MIN, f32::max);
        let tau = if top > self.ref_db { REF_ATTACK_SEC } else { REF_RELEASE_SEC };
        self.ref_db += (top - self.ref_db) * smoothing(dt, tau);
        self.ref_db = self.ref_db.max(top - REF_MAX_LAG_DB).max(c.floor_db + c.range_db - c.headroom_db);
        let bottom = self.ref_db + c.headroom_db - c.range_db;

        let attack = smoothing(dt, ATTACK_TAU_SEC);
        for i in 0..self.bars.len() {
            let target = ((self.corrected[i] - bottom) / c.range_db).clamp(0.0, 1.0);
            let bar = &mut self.bars[i];
            if target > *bar {
                *bar += (target - *bar) * attack;
            } else {
                *bar = target.max(*bar - BAR_FALL_PER_SEC * dt);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, amp: f32) -> Vec<f32> {
        (0..WINDOW)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / 48000.0).sin())
            .collect()
    }

    fn loudest(levels: &[f32]) -> (usize, f32) {
        levels.iter().enumerate().fold((0, f32::MIN), |acc, (i, v)| if *v > acc.1 { (i, *v) } else { acc })
    }

    fn config(equalize: f32) -> DynamicsConfig {
        DynamicsConfig { range_db: 30.0, floor_db: -75.0, headroom_db: 6.0, equalize }
    }

    #[test]
    fn tones_land_in_expected_bands() {
        let mut spec = Spectrum::new(48000, 17, 40.0, 16000.0, 0.0);
        let mut out = vec![0.0; 17];
        for (freq, band) in [(50.0, 0), (100.0, 2), (440.0, 6), (1000.0, 9), (5000.0, 13), (12000.0, 16)] {
            spec.bands_db(&sine(freq, 1.0), &mut out);
            let (best, level) = loudest(&out);
            assert_eq!(best, band, "{freq} Hz");
            assert!((-6.0..=-2.0).contains(&level), "{freq} Hz level {level}");
        }
    }

    #[test]
    fn full_range_resolves_deep_bass() {
        let mut spec = Spectrum::new(48000, 17, 20.0, 20000.0, 0.0);
        let mut out = vec![0.0; 17];
        for (freq, band) in [(25.0, 0), (36.0, 1), (18000.0, 16)] {
            spec.bands_db(&sine(freq, 1.0), &mut out);
            assert_eq!(loudest(&out).0, band, "{freq} Hz");
        }
    }

    #[test]
    fn steady_tone_leaves_headroom() {
        let mut spec = Spectrum::new(48000, 17, 40.0, 16000.0, 0.0);
        let mut dynamics = BarDynamics::new(17, config(0.0));
        let mut levels = vec![0.0; 17];
        spec.bands_db(&sine(100.0, 0.5), &mut levels);
        for _ in 0..600 {
            dynamics.update(&levels, 1.0 / 60.0);
        }
        let bar = dynamics.bars[2];
        assert!((0.75..0.85).contains(&bar), "steady bar {bar}");
    }

    #[test]
    fn equalize_lifts_quiet_bands() {
        let mut levels = vec![-20.0; 17];
        levels[16] = -40.0;
        let mut flat = BarDynamics::new(17, config(0.0));
        let mut eq = BarDynamics::new(17, config(0.5));
        for _ in 0..600 {
            flat.update(&levels, 1.0 / 60.0);
            eq.update(&levels, 1.0 / 60.0);
        }
        assert!(eq.bars[16] > flat.bars[16] + 0.2, "{} vs {}", eq.bars[16], flat.bars[16]);
    }

    #[test]
    fn equalize_wakes_up_faint_sub_bass() {
        // Sub-bass 35 dB below the rest, as measured on real music.
        let mut levels = vec![-25.0; 17];
        levels[0] = -60.0;
        let mut flat = BarDynamics::new(17, config(0.0));
        let mut eq = BarDynamics::new(17, config(1.0));
        for _ in 0..600 {
            flat.update(&levels, 1.0 / 60.0);
            eq.update(&levels, 1.0 / 60.0);
        }
        assert_eq!(flat.bars[0], 0.0);
        assert!(eq.bars[0] > 0.5, "{}", eq.bars[0]);
    }

    #[test]
    fn equalize_leaves_dead_bands_dark() {
        // Band 16 sits 60 dB under the rest, like treble past a codec cutoff.
        let mut levels = vec![-20.0; 17];
        levels[16] = -80.0;
        let mut eq = BarDynamics::new(17, config(1.0));
        for _ in 0..600 {
            eq.update(&levels, 1.0 / 60.0);
        }
        assert_eq!(eq.bars[16], 0.0);
    }

    #[test]
    fn silence_decays_to_zero() {
        let mut spec = Spectrum::new(48000, 17, 40.0, 16000.0, 2.0);
        let mut dynamics = BarDynamics::new(17, config(0.5));
        let mut levels = vec![0.0; 17];
        spec.bands_db(&sine(100.0, 0.5), &mut levels);
        for _ in 0..30 {
            dynamics.update(&levels, 1.0 / 60.0);
        }
        assert!(dynamics.bars[2] > 0.7);
        spec.bands_db(&vec![0.0; WINDOW], &mut levels);
        for _ in 0..120 {
            dynamics.update(&levels, 1.0 / 60.0);
        }
        assert!(dynamics.bars.iter().all(|v| *v == 0.0));
    }
}
