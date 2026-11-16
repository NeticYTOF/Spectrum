//! Silent synthetic demo source: music-reactive visuals without touching
//! any audio hardware. Publishes synthetic `FrameAnalysis` frames into the
//! shared state on a thread, exactly like the real analyzer would.
//!
//! The 8-second structure cycle exercises every visual state:
//! build-up (0-4 s) -> drop (4-6 s) -> fade (6-7.5 s) -> silence (7.5-8 s).

use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::analysis::{FrameAnalysis, SharedState};

/// Demo tick rate, matching the analyzer thread's cadence.
const DEMO_TICK: Duration = Duration::from_millis(15);
/// One cycle of the synthetic structure, in seconds.
const CYCLE_SECS: f32 = 8.0;
/// Beat cadence during the energetic sections (120 BPM).
const BEAT_SECS: f32 = 0.5;

/// Kick drum envelope: exponential decay after each beat.
fn kick(t_since_beat: f32) -> f32 {
    (-t_since_beat * 14.0).exp()
}

/// Section amplitude envelope over one cycle.
fn section_energy(t: f32) -> f32 {
    let t = t % CYCLE_SECS;
    if t < 4.0 {
        // Build-up: steady rise with a heartbeat pulse.
        0.25 + 0.35 * (t / 4.0) + 0.1 * (t * 4.0).sin()
    } else if t < 6.0 {
        // Drop: loud.
        0.95
    } else if t < 7.5 {
        // Fade.
        0.95 * (1.0 - (t - 6.0) / 1.5)
    } else {
        // Silence.
        0.0
    }
}

/// Band profile: bass bump that tracks the kick, two melodic bumps that
/// wander across the mid range, and a faint sparkle tail. The whole
/// profile - kick included - follows the section energy so the silence
/// phase is genuinely silent.
fn band_value(band_t: f32, t: f32, energy: f32, t_since_beat: f32) -> f32 {
    let bass = kick(t_since_beat) * (-band_t * 3.5).exp();
    let mel1 = 0.55 * (-((band_t - (0.35 + 0.12 * (t * 0.9).sin())) * 9.0).powi(2)).exp();
    let mel2 = 0.4 * (-((band_t - (0.65 + 0.10 * (t * 0.6).cos())) * 11.0).powi(2)).exp();
    let sparkle = 0.12 * (-((band_t - 0.92) * 6.0).powi(2)).exp() * (t * 7.0).sin().abs();
    let v = (bass * 0.9 + mel1 + mel2 + sparkle) * energy;
    v.clamp(0.0, 1.0)
}

/// One synthetic waveform window: kick (decaying low sine) plus a tone
/// whose amplitude follows the section energy. Starts at a rising zero
/// crossing so the trace reads as phase-triggered.
fn synth_waveform(energy: f32, t_since_beat: f32) -> Vec<f32> {
    let len = 768;
    let rate = 44_100.0f32;
    let mut v = Vec::with_capacity(len);
    for i in 0..len {
        let tt = i as f32 / rate;
        // The kick follows the section energy too: the silence phase
        // produces a flat line, not a pulsing bass.
        let kick_env = kick(t_since_beat) * kick(tt * 2.0) * energy;
        let kick = (2.0 * PI * 55.0 * tt).sin() * kick_env * 0.8;
        let tone = (2.0 * PI * 220.0 * tt).sin() * 0.25 * energy;
        let air = (2.0 * PI * 880.0 * tt).sin() * 0.06 * energy;
        v.push((kick + tone + air).clamp(-1.0, 1.0));
    }
    // Rotate to a rising zero crossing for a stable look.
    if let Some(idx) = v
        .windows(2)
        .enumerate()
        .find(|(_, w)| w[0] <= 0.0 && w[1] > 0.0)
        .map(|(i, _)| i)
    {
        v.rotate_left(idx);
    }
    v
}

/// One synthetic analysis frame at demo time `t` (seconds since start).
pub fn synthetic_frame(t: f32, bands: usize) -> FrameAnalysis {
    let energy = section_energy(t);
    let beat_phase = t % BEAT_SECS;
    let t_since_beat = beat_phase;
    let is_beat = beat_phase < DEMO_TICK.as_secs_f32() && energy > 0.4;

    let bars: Vec<f32> = (0..bands)
        .map(|i| {
            let band_t = i as f32 / (bands - 1).max(1) as f32;
            band_value(band_t, t, energy, t_since_beat)
        })
        .collect();

    // Spectrogram input: same profile with a touch of flicker.
    let spectrum: Vec<f32> = bars
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let flick = 0.94 + 0.06 * (t * 13.0 + i as f32).sin();
            (b * flick).clamp(0.0, 1.0)
        })
        .collect();

    FrameAnalysis {
        bars,
        spectrum,
        waveform: if energy <= 0.01 {
            Vec::new()
        } else {
            let _ = energy;
            synth_waveform(energy, t_since_beat)
        },
        beat: is_beat,
        flux: energy * kick(t_since_beat) * 0.5,
    }
}

/// Publish synthetic frames at ~60 Hz until `stop` is set. Honors the demo
/// pause flag: time freezes while paused, exactly like real paused audio.
#[allow(clippy::too_many_arguments)]
pub fn spawn_demo(
    shared: Arc<Mutex<SharedState>>,
    bands: usize,
    stop: Arc<AtomicBool>,
    pause: Option<Arc<AtomicBool>>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("spectrum-demo".into())
        .spawn(move || {
            let mut t = 0.0f32;
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(DEMO_TICK);
                let paused = pause
                    .as_ref()
                    .map(|p| p.load(Ordering::Relaxed))
                    .unwrap_or(false);
                if !paused {
                    t += DEMO_TICK.as_secs_f32();
                    let frame = synthetic_frame(t, bands);
                    let mut sh = shared.lock().unwrap_or_else(|e| e.into_inner());
                    if frame.beat {
                        sh.beats_pending = sh.beats_pending.saturating_add(1);
                    }
                    sh.latest = Arc::new(frame);
                }
            }
        })
        .expect("failed to spawn demo thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_hits_silence_and_peak() {
        // Silence at the end of the cycle, full energy in the drop.
        assert!(section_energy(7.75) == 0.0);
        assert!(section_energy(5.0) > 0.9);
        // Build-up is quieter than the drop.
        assert!(section_energy(1.0) < section_energy(5.0));
    }

    #[test]
    fn beats_fire_only_when_energetic() {
        // Just after a beat boundary during the loud drop -> beat.
        let b = synthetic_frame(5.0 + BEAT_SECS + 0.001, 64);
        assert!(b.beat, "first tick after a beat in the drop");
        // Mid-beat (well past the boundary) -> no beat.
        let mid = synthetic_frame(5.0 + 0.25, 64);
        assert!(!mid.beat);
        // In the silence section no beats fire even at the boundary.
        let silent = synthetic_frame(7.75, 64);
        assert!(!silent.beat);
    }

    #[test]
    fn frame_shapes_are_sane() {
        let f = synthetic_frame(4.5, 64);
        assert_eq!(f.bars.len(), 64);
        assert_eq!(f.spectrum.len(), 64);
        assert!(f.bars.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!(!f.waveform.is_empty(), "energetic frame has a waveform");
        assert!(f.waveform.iter().all(|v| v.abs() <= 1.0));
        // Waveform triggers near a rising crossing.
        assert!(f.waveform[0] <= 0.0);
        assert!(f.waveform[1] > 0.0);

        // Silence phase: no waveform, bars collapse toward zero.
        let s = synthetic_frame(7.9, 64);
        assert!(s.waveform.is_empty());
        assert!(s.bars.iter().all(|v| *v <= 0.01));
    }

    #[test]
    fn bass_dominates_low_bands() {
        let f = synthetic_frame(4.5, 64);
        let low: f32 = f.bars.iter().take(10).sum();
        let high: f32 = f.bars.iter().skip(40).sum();
        assert!(
            low > high,
            "kick energy should sit in the low bands (low={low:.2} high={high:.2})"
        );
    }
}
