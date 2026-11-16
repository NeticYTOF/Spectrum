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
/// Nominal tick rate: per-tick smoothing fractions are converted to
/// per-second rates as if evaluated at this frequency.
const TICKS_PER_SEC: f32 = 60.0;

/// Convert a per-tick smoothing fraction into the time-based factor for
/// a given dt: `1 - e^(-rate*dt)`. Matches the old behavior exactly at
/// the nominal tick rate and stays correct when ticks drift.
fn rate_from_fraction(fraction: f32) -> f32 {
    -(1.0 - fraction.clamp(0.0, 0.999)).ln() * TICKS_PER_SEC
}

fn smoothing_factor(rate: f32, dt: f32) -> f32 {
    1.0 - (-rate * dt).exp()
}

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
    /// Published analysis frame; shared by pointer so the UI never
    /// deep-copies bars/spectrum/waveform per frame.
    pub latest: Arc<FrameAnalysis>,
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
    targets: Vec<f32>,
    flux_hist: VecDeque<f32>,
    bars: Vec<f32>,
    spectrum: Vec<f32>,
    last_beat: Instant,
    /// Whether the previous tick saw fresh samples; used to reseed the
    /// spectral-flux baseline after a pause so resuming can't register
    /// a phantom beat.
    was_active: bool,
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
            targets: vec![0.0; bands],
            flux_hist: VecDeque::with_capacity(FLUX_HISTORY),
            bars: vec![0.0; bands],
            spectrum: vec![0.0; bands],
            last_beat: Instant::now(),
            was_active: false,
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
    /// instead of freezing or showing stale data. `dt` is the time since
    /// the previous tick in seconds, so smoothing is wall-clock based.
    pub fn tick(&mut self, params: &Params, active: bool, dt: f32) -> FrameAnalysis {
        let params = params.clamped();
        let dt = dt.clamp(0.001, 0.25);

        if !active || self.history.len() < FFT_SIZE {
            // Time-based decay toward silence.
            let decay = (-rate_from_fraction(params.fall) * dt).exp();
            for b in &mut self.bars {
                *b *= decay;
            }
            for s in &mut self.spectrum {
                *s *= 0.9;
            }
            self.was_active = active;
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

        // Per-band values, reused across ticks to stay allocation-free.
        for i in 0..self.bands {
            let (b0, b1) = self.band_bins[i];
            let mut best = 0.0f32;
            for k in b0..b1 {
                if self.mags[k] > best {
                    best = self.mags[k];
                }
            }
            self.targets[i] = norm(best, params.sensitivity);
        }
        let bars_fall = smoothing_factor(rate_from_fraction(params.fall), dt);
        let bars_rise = smoothing_factor(rate_from_fraction(BARS_RISE), dt);
        let spectrum_rise = smoothing_factor(rate_from_fraction(SPECTRUM_RISE), dt);
        for i in 0..self.bands {
            let target = self.targets[i];
            let cur = self.bars[i];
            // Asymmetric smoothing: bars jump up fast, fall gracefully.
            self.bars[i] = if target >= cur {
                cur + (target - cur) * bars_rise
            } else {
                cur + (target - cur) * bars_fall
            };
            let s = &mut self.spectrum[i];
            *s += (target - *s) * spectrum_rise;
        }

        // After inactivity the flux baseline is stale: diffing against the
        // pre-pause window would spike. Reseed it so the resume tick has
        // zero flux and cannot fire a phantom beat.
        if !self.was_active {
            self.prev_mags.copy_from_slice(&self.mags);
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
        self.was_active = true;

        // Waveform: trigger on the first rising zero crossing where a
        // full display window still fits, then show a fixed-length window
        // from there. (The old backward scan triggered on the LAST
        // crossing and left under a period on screen.)
        let waveform = waveform_window(&self.history);

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

/// Oscilloscope trigger search region: ~46 ms at 44.1 kHz. Guarantees at
/// least one rising crossing for anything above ~22 Hz.
const WAVE_SEARCH: usize = 2048;
/// Oscilloscope display window: a fixed ~17 ms of signal.
const WAVE_DISPLAY: usize = 768;

/// Oscilloscope display window: phase-aligned and fixed-length.
///
/// Searches forward from the start of the recent search region for the
/// first rising zero crossing from which a full `WAVE_DISPLAY` window
/// still fits, and returns exactly that window. This keeps the trace
/// phase-stable while the time axis never stretches. When no crossing
/// exists in the allowed range (silence, DC, sub-audio frequencies), the
/// newest samples are shown instead - the display never goes blank and
/// never shrinks merely because a crossing happened near the end.
fn waveform_window(history: &VecDeque<f32>) -> Vec<f32> {
    if history.is_empty() {
        return Vec::new();
    }
    let search = history.len().min(WAVE_SEARCH);
    let start = history.len() - search;
    // Earliest position from which a full display window still fits.
    let full_end = history.len().saturating_sub(WAVE_DISPLAY);
    let hi = full_end.min(start + WAVE_SEARCH);

    let mut trigger = None;
    if start < hi {
        for i in start..hi {
            if history[i] <= 0.0 && history[i + 1] > 0.0 {
                trigger = Some(i);
                break;
            }
        }
    }
    let from = trigger.unwrap_or(full_end);
    history
        .iter()
        .skip(from)
        .take(WAVE_DISPLAY)
        .copied()
        .collect()
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
            let mut last = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(15));
                let now = Instant::now();
                let dt = (now - last).as_secs_f32();
                last = now;
                let mut active = false;
                {
                    let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                    for s in q.drain(..) {
                        analyzer.push_sample(s);
                        active = true;
                    }
                }
                let params = shared.lock().unwrap_or_else(|e| e.into_inner()).params;
                let frame = analyzer.tick(&params, active, dt);
                let mut sh = shared.lock().unwrap_or_else(|e| e.into_inner());
                if frame.beat {
                    sh.beats_pending = sh.beats_pending.saturating_add(1);
                }
                sh.latest = Arc::new(frame);
            }
        })
        .expect("failed to spawn analyzer thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nominal 60 Hz tick time: reproduces the pre-existing smoothing
    /// behavior exactly while exercising the time-based code path.
    const DT: f32 = 1.0 / 60.0;

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
        let frame = an.tick(&params, true, DT);

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
        let first = an.tick(&params, true, DT);
        assert!(first.bars.iter().any(|&b| b > 0.3));

        for _ in 0..40 {
            let f = an.tick(&params, false, DT);
            assert!(f.waveform.is_empty());
            assert!(!f.beat);
        }
        let last = an.tick(&params, false, DT);
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
        let f = an.tick(&params, true, DT);
        assert!(f.bars.iter().all(|&b| b == 0.0));
        assert!(!f.beat);
    }

    #[test]
    fn waveform_window_fixed_length_and_triggered() {
        // Regression for the reported bug: a 440 Hz sine at 44.1 kHz used
        // to leave only ~87 samples (< 1 period) on screen.
        let params = Params {
            sensitivity: 1.0,
            fall: 0.2,
        };
        let mut an = Analyzer::new(44_100, 64);
        feed_sine(&mut an, 44_100.0, 440.0, 4096);
        let frame = an.tick(&params, true, DT);
        assert_eq!(frame.waveform.len(), WAVE_DISPLAY);
        assert!(frame.waveform[0] <= 0.0, "must trigger on a crossing");
        assert!(frame.waveform[1] > 0.0, "must be a rising crossing");
    }

    #[test]
    fn waveform_window_stable_across_frequencies_and_phases() {
        let params = Params {
            sensitivity: 1.0,
            fall: 0.2,
        };
        // Frequencies whose period fits the allowed trigger range must
        // always produce a full, triggered window.
        for freq in [40.0, 50.0, 220.0, 440.0, 1000.0, 5000.0] {
            for phase in [0.0f32, 1.1, 2.3, 4.2] {
                let mut an = Analyzer::new(44_100, 64);
                for i in 0..(FFT_SIZE * 4) {
                    let t = i as f32 / 44_100.0;
                    an.push_sample((2.0 * PI * freq * t + phase).sin() * 0.9);
                }
                let frame = an.tick(&params, true, DT);
                assert_eq!(
                    frame.waveform.len(),
                    WAVE_DISPLAY,
                    "freq {freq} phase {phase}: fixed window"
                );
                assert!(
                    frame.waveform[0] <= 0.0 && frame.waveform[1] > 0.0,
                    "freq {freq} phase {phase}: must trigger"
                );
            }
        }
        // Sub-audio frequencies may fall back, but the window must stay
        // full-length and never blank out.
        for freq in [20.0, 25.0, 30.0] {
            for phase in [0.0f32, 2.0] {
                let mut an = Analyzer::new(44_100, 64);
                for i in 0..(FFT_SIZE * 4) {
                    let t = i as f32 / 44_100.0;
                    an.push_sample((2.0 * PI * freq * t + phase).sin() * 0.9);
                }
                let frame = an.tick(&params, true, DT);
                assert_eq!(
                    frame.waveform.len(),
                    WAVE_DISPLAY,
                    "freq {freq}: fallback window must stay full"
                );
            }
        }
    }

    #[test]
    fn waveform_window_short_history_and_dc() {
        // Short history: return what exists.
        let mut an = Analyzer::new(44_100, 64);
        an.push_sample(0.5);
        an.push_sample(-0.5);
        let params = Params {
            sensitivity: 1.0,
            fall: 0.2,
        };
        let frame = an.tick(&params, true, DT);
        assert!(frame.waveform.len() <= 2, "history shorter than window");

        // Constant positive signal (DC): no crossing, still a full window.
        let mut an = Analyzer::new(44_100, 64);
        for _ in 0..(FFT_SIZE * 2) {
            an.push_sample(0.5);
        }
        let frame = an.tick(&params, true, DT);
        assert_eq!(frame.waveform.len(), WAVE_DISPLAY);
    }

    #[test]
    fn resume_after_pause_does_not_fire_phantom_beat() {
        let params = Params {
            sensitivity: 1.0,
            fall: 0.2,
        };
        let mut an = Analyzer::new(44_100, 64);
        feed_sine(&mut an, 44_100.0, 440.0, FFT_SIZE * 4);
        for _ in 0..30 {
            an.tick(&params, true, DT);
        }
        // Pause: no fresh samples.
        for _ in 0..10 {
            an.tick(&params, false, DT);
        }
        // Resume: more samples flow; the first active tick must have zero
        // flux and no phantom beat.
        for i in 0..(FFT_SIZE * 2) {
            let t = (FFT_SIZE * 4 + i) as f32 / 44_100.0;
            an.push_sample((2.0 * PI * 440.0 * t).sin() * 0.9);
        }
        let frame = an.tick(&params, true, DT);
        assert!(!frame.beat, "resume tick must not register a beat");
        assert_eq!(frame.flux, 0.0, "resume flux must be reseeded");
        // Once running normally, flux is non-zero again.
        for i in 0..FFT_SIZE {
            let t = (FFT_SIZE * 6 + i) as f32 / 44_100.0;
            an.push_sample((2.0 * PI * 440.0 * t).sin() * 0.9);
        }
        let frame = an.tick(&params, true, DT);
        assert!(frame.flux >= 0.0);
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
