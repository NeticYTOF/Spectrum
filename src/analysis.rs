//! FFT analysis: windowing, frequency bands, smoothing and beat detection.
//!
//! The [`Analyzer`] owns a rolling sample history and converts it into
//! per-band magnitudes (bars), an unsmoothed spectrum (spectrogram), a
//! zero-aligned waveform (oscilloscope) and a beat flag (spectral flux).

use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::audio::SampleQueue;

pub const FFT_SIZE: usize = 2048;
pub const DEFAULT_BANDS: usize = 64;
pub const MIN_FREQ: f32 = 30.0;
pub const MAX_FREQ: f32 = 16_000.0;

const BARS_RISE: f32 = 0.55;
const SPECTRUM_RISE: f32 = 0.6;
const BEAT_MULTIPLIER: f32 = 1.45;
const BEAT_MIN_FRAMES: usize = 20;
const BEAT_REFRACTORY: Duration = Duration::from_millis(140);
const DB_FLOOR: f32 = 96.0;
const FLUX_HISTORY: usize = 64;

/// Tunables shared between the UI and the analyzer thread.
#[derive(Clone, Copy)]
pub struct Params {
    pub sensitivity: f32,
    /// Bar fall speed: 0.01 sluggish .. 0.95 instant.
    pub fall: f32,
}

impl Params {
    pub fn clamped(mut self) -> Self {
        self.sensitivity = self.sensitivity.clamp(0.1, 4.0);
        self.fall = self.fall.clamp(0.01, 0.95);
        self
    }
}

#[derive(Clone)]
pub struct FrameAnalysis {
    /// Smoothed per-band magnitude in 0..=1, low -> high frequency.
    pub bars: Vec<f32>,
    /// Unsmoothed per-band magnitude in 0..=1 (spectrogram input).
    pub spectrum: Vec<f32>,
    /// Recent mono waveform, rising-zero aligned for a stable display.
    pub waveform: Vec<f32>,
    pub beat: bool,
    pub flux: f32,
}

impl FrameAnalysis {
    pub fn quiet(bands: usize) -> Self {
        Self {
            bars: vec![0.0; bands],
            spectrum: vec![0.0; bands],
            waveform: Vec::new(),
            beat: false,
            flux: 0.0,
        }
    }
}

pub struct SharedState {
    pub latest: FrameAnalysis,
    pub params: Params,
    /// Beats detected since the UI last looked; lets the UI never miss one.
    pub beats_pending: u32,
}

pub struct Analyzer {
    bands: usize,
    history: VecDeque<f32>,
    hann: Vec<f32>,
    fft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex<f32>>,
    mags: Vec<f32>,
    prev_mags: Vec<f32>,
    band_bins: Vec<(usize, usize)>,
    flux_hist: VecDeque<f32>,
    bars: Vec<f32>,
    spectrum: Vec<f32>,
    last_beat: Instant,
}

impl Analyzer {
    pub fn new(sample_rate: u32, bands: usize) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);

        let hann: Vec<f32> = (0..FFT_SIZE)
            .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / (FFT_SIZE - 1) as f32).cos()))
            .collect();

        let bands = bands.clamp(8, 256);
        let sample_rate = (if sample_rate == 0 {
            44_100
        } else {
            sample_rate
        }) as f32;
        let bins = FFT_SIZE / 2;
        let nyquist = sample_rate / 2.0;
        let f_max = MAX_FREQ.min(nyquist * 0.95).max(MIN_FREQ * 4.0);

        // Log-spaced band edges. Sub-bin-width bands share bins rather
        // than distorting the scale: with a 2048-sample window the lowest
        // octaves simply cannot be resolved further.
        let mut band_bins = Vec::with_capacity(bands);
        for i in 0..bands {
            let t0 = i as f32 / bands as f32;
            let t1 = (i + 1) as f32 / bands as f32;
            let f0 = MIN_FREQ * (f_max / MIN_FREQ).powf(t0);
            let f1 = MIN_FREQ * (f_max / MIN_FREQ).powf(t1);
            let b0 = (f0 * FFT_SIZE as f32 / sample_rate).round() as usize;
            let b1 = (f1 * FFT_SIZE as f32 / sample_rate).round() as usize;
            let b0 = b0.clamp(1, bins - 1);
            let b1 = b1.clamp(b0 + 1, bins);
            band_bins.push((b0, b1));
        }

        Self {
            bands,
            history: VecDeque::with_capacity(FFT_SIZE * 4),
            hann,
            fft,
            scratch: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            mags: vec![0.0; bins],
            prev_mags: vec![0.0; bins],
            band_bins,
            flux_hist: VecDeque::with_capacity(FLUX_HISTORY),
            bars: vec![0.0; bands],
            spectrum: vec![0.0; bands],
            last_beat: Instant::now(),
        }
    }

    pub fn push_sample(&mut self, sample: f32) {
        // Guard against garbage from a flaky capture device.
        if sample.is_finite() {
            self.history.push_back(sample);
        }
        if self.history.len() > FFT_SIZE * 4 {
            self.history.pop_front();
        }
    }

    /// Produce one analysis frame.
    ///
    /// `active` is false when no new samples arrived since the last tick
    /// (playback paused or finished): the bars then decay to silence
    /// instead of freezing or showing stale data.
    pub fn tick(&mut self, params: &Params, active: bool) -> FrameAnalysis {
        let params = params.clamped();

        if !active || self.history.len() < FFT_SIZE {
            let decay = 1.0 - params.fall;
            for b in &mut self.bars {
                *b *= decay;
            }
            for s in &mut self.spectrum {
                *s *= 0.9;
            }
            return FrameAnalysis {
                bars: self.bars.clone(),
                spectrum: self.spectrum.clone(),
                waveform: Vec::new(),
                beat: false,
                flux: 0.0,
            };
        }

        // Build the Hann-windowed analysis frame.
        let start = self.history.len() - FFT_SIZE;
        for i in 0..FFT_SIZE {
            let s = self.history[start + i];
            self.scratch[i] = Complex::new(s * self.hann[i], 0.0);
        }
        self.fft.process(&mut self.scratch);

        // Magnitude spectrum, normalized so a full-scale sine peaks near 1.
        let half = FFT_SIZE / 2;
        for k in 0..half {
            let c = self.scratch[k];
            self.mags[k] = (c.re * c.re + c.im * c.im).sqrt() * (2.0 / FFT_SIZE as f32);
        }

        // Per-band values, computed into a temp so borrows stay disjoint.
        let mut targets = vec![0.0f32; self.bands];
        for i in 0..self.bands {
            let (b0, b1) = self.band_bins[i];
            let mut best = 0.0f32;
            for k in b0..b1 {
                if self.mags[k] > best {
                    best = self.mags[k];
                }
            }
            targets[i] = norm(best, params.sensitivity);
        }
        for i in 0..self.bands {
            let target = targets[i];
            let cur = self.bars[i];
            // Asymmetric smoothing: bars jump up fast, fall gracefully.
            self.bars[i] = if target >= cur {
                cur + (target - cur) * BARS_RISE
            } else {
                cur + (target - cur) * params.fall
            };
            let s = &mut self.spectrum[i];
            *s += (target - *s) * SPECTRUM_RISE;
        }

        // Spectral flux -> beat detection.
        let mut flux = 0.0f32;
        for k in 0..half {
            let d = self.mags[k] - self.prev_mags[k];
            if d > 0.0 {
                flux += d;
            }
        }
        let mean = if self.flux_hist.is_empty() {
            0.0
        } else {
            self.flux_hist.iter().sum::<f32>() / self.flux_hist.len() as f32
        };
        let beat = self.flux_hist.len() >= BEAT_MIN_FRAMES
            && flux > mean * BEAT_MULTIPLIER
            && self.last_beat.elapsed() >= BEAT_REFRACTORY;
        if beat {
            self.last_beat = Instant::now();
        }
        self.flux_hist.push_back(flux);
        if self.flux_hist.len() > FLUX_HISTORY {
            self.flux_hist.pop_front();
        }
        self.prev_mags.copy_from_slice(&self.mags);

        // Waveform: recent tail, aligned to a rising zero crossing so the
        // scope stays visually stable instead of jumping around.
        let take = self.history.len().min(1_100);
        let tail: Vec<f32> = self
            .history
            .iter()
            .skip(self.history.len() - take)
            .copied()
            .collect();
        let waveform = zero_align(tail);

        FrameAnalysis {
            bars: self.bars.clone(),
            spectrum: self.spectrum.clone(),
            waveform,
            beat,
            flux,
        }
    }
}

/// Map a linear magnitude to 0..=1 with a dB curve, then apply sensitivity.
fn norm(mag: f32, sensitivity: f32) -> f32 {
    let db = 20.0 * mag.max(1e-8).log10();
    let v = (db + DB_FLOOR) / DB_FLOOR;
    (v * sensitivity).clamp(0.0, 1.0)
}

fn zero_align(mut sig: Vec<f32>) -> Vec<f32> {
    if sig.len() < 2 {
        return sig;
    }
    for i in (0..sig.len() - 1).rev() {
        if sig[i] <= 0.0 && sig[i + 1] > 0.0 {
            sig.drain(..i);
            break;
        }
    }
    sig
}

/// Run the analysis loop on its own thread at ~60 Hz.
pub fn spawn_analyzer(
    sample_rate: u32,
    bands: usize,
    queue: SampleQueue,
    shared: Arc<Mutex<SharedState>>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("spectrum-analyzer".into())
        .spawn(move || {
            let mut analyzer = Analyzer::new(sample_rate, bands);
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(15));
                let mut active = false;
                {
                    let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                    for s in q.drain(..) {
                        analyzer.push_sample(s);
                        active = true;
                    }
                }
                let params = shared.lock().unwrap_or_else(|e| e.into_inner()).params;
                let frame = analyzer.tick(&params, active);
                let mut sh = shared.lock().unwrap_or_else(|e| e.into_inner());
                if frame.beat {
                    sh.beats_pending = sh.beats_pending.saturating_add(1);
                }
                sh.latest = frame;
            }
        })
        .expect("failed to spawn analyzer thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_sine(an: &mut Analyzer, rate: f32, freq: f32, samples: usize) {
        for i in 0..samples {
            let t = i as f32 / rate;
            an.push_sample((2.0 * PI * freq * t).sin() * 0.9);
        }
    }

    #[test]
    fn sine_peaks_in_expected_band() {
        let params = Params {
            sensitivity: 1.0,
            fall: 0.2,
        };
        let mut an = Analyzer::new(44_100, 64);
        feed_sine(&mut an, 44_100.0, 440.0, FFT_SIZE * 2);
        let frame = an.tick(&params, true);

        let peak = frame
            .bars
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        // 440 Hz on a 30 Hz..16 kHz log scale lands around band 27 of 64.
        assert!(
            (peak as i32 - 27).abs() <= 2,
            "peak band was {peak}, expected ~27"
        );
        assert!(
            frame.bars[peak] > 0.3,
            "peak value {} too low",
            frame.bars[peak]
        );
        assert!(!frame.waveform.is_empty());
    }

    #[test]
    fn inactive_ticks_decay_to_silence() {
        let params = Params {
            sensitivity: 1.0,
            fall: 0.5,
        };
        let mut an = Analyzer::new(44_100, 64);
        feed_sine(&mut an, 44_100.0, 440.0, FFT_SIZE * 2);
        let first = an.tick(&params, true);
        assert!(first.bars.iter().any(|&b| b > 0.3));

        for _ in 0..40 {
            let f = an.tick(&params, false);
            assert!(f.waveform.is_empty());
            assert!(!f.beat);
        }
        let last = an.tick(&params, false);
        assert!(last.bars.iter().all(|&b| b < 0.01));
    }

    #[test]
    fn quiet_when_history_short() {
        let params = Params {
            sensitivity: 1.0,
            fall: 0.2,
        };
        let mut an = Analyzer::new(44_100, 64);
        an.push_sample(0.5);
        let f = an.tick(&params, true);
        assert!(f.bars.iter().all(|&b| b == 0.0));
        assert!(!f.beat);
    }

    #[test]
    fn zero_align_starts_at_rising_crossing() {
        let sig = vec![1.0, -0.5, 0.5, -0.2, 0.8, -0.1, 0.3, 0.9];
        let aligned = zero_align(sig);
        assert_eq!(aligned[0], -0.1);
        assert_eq!(aligned.len(), 3);
    }

    #[test]
    fn zero_align_keeps_signal_without_crossing() {
        let sig = vec![0.5, 0.9, 0.2];
        let aligned = zero_align(sig.clone());
        assert_eq!(aligned, sig);
    }

    #[test]
    fn non_finite_samples_ignored() {
        let mut an = Analyzer::new(44_100, 64);
        an.push_sample(f32::NAN);
        an.push_sample(f32::INFINITY);
        an.push_sample(0.5);
        assert_eq!(an.history.len(), 1);
    }

    #[test]
    fn params_are_clamped() {
        let p = Params {
            sensitivity: 99.0,
            fall: 0.0,
        }
        .clamped();
        assert_eq!(p.sensitivity, 4.0);
        assert_eq!(p.fall, 0.01);
    }

    #[test]
    fn norm_covers_range() {
        assert_eq!(norm(0.0, 1.0), 0.0);
        assert!((norm(0.5, 1.0) - 0.937).abs() < 0.01);
        assert_eq!(norm(1.0, 1.0), 1.0);
        assert_eq!(norm(1.0, 0.5), 0.5);
    }
}
