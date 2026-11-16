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
pub const CYCLE_SECS: f32 = 8.0;
/// Beat cadence during the energetic sections (120 BPM).
const BEAT_SECS: f32 = 0.5;

/// The demo "track": original fixture text written for this project (not
/// real song lyrics), timed against the 8 s cycle so the lyrics garden has
/// something honest to render without any media session.
pub const DEMO_TRACK_TITLE: &str = "Garden of Sound";
pub const DEMO_TRACK_ARTIST: &str = "Spectrum Fixture";

pub fn demo_lyric_lines() -> Vec<crate::lrc::LrcLine> {
    use crate::lrc::LrcLine;
    [
        (0.2, "Little signals in the dark"),
        (1.8, "grow a garden out of sound"),
        (3.4, "every beat a budding bloom"),
        (5.0, "silence is the resting soil"),
        (6.6, "and the night hums on"),
    ]
    .into_iter()
    .map(|(t, text)| LrcLine {
        time_s: t,
        text: text.to_string(),
    })
    .collect()
}

/// A demo media snapshot at cycle position `pos` (seconds).
pub fn demo_snapshot(pos: f64, paused: bool) -> crate::media::MediaSnapshot {
    crate::media::MediaSnapshot {
        available: true,
        title: DEMO_TRACK_TITLE.to_string(),
        artist: DEMO_TRACK_ARTIST.to_string(),
        album: String::new(),
        duration_s: Some(CYCLE_SECS as f64),
        position_s: pos,
        playing: !paused,
    }
}

/// Seed the media + lyrics shared state for the demo BEFORE the UI starts.
/// The fixture's media identity and lyrics identity are derived from the
/// same snapshot, so the UI's real-track identity gate passes honestly
/// (the gate itself is untouched).
pub fn seed_demo_state(
    media: &crate::media::MediaShared,
    lyrics: &crate::lyrics::LyricsShared,
    pos: f64,
    paused: bool,
) {
    let snap = demo_snapshot(pos, paused);
    let key = crate::lyrics::TrackKey::from_snapshot(&snap);
    *media.lock().unwrap_or_else(|e| e.into_inner()) = snap;
    let mut st = lyrics.lock().unwrap_or_else(|e| e.into_inner());
    st.key = key;
    st.generation = st.generation.wrapping_add(1);
    st.status = crate::lyrics::LyricsStatus::Synced(demo_lyric_lines());
}

/// A demo "media session" thread: publishes the synthetic timeline so the
/// lyrics garden works identically without any real player. With
/// `freeze_at`, the position is pinned for deterministic frames.
pub fn spawn_demo_media(
    shared: crate::media::MediaShared,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    freeze_at: Option<f32>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("spectrum-demo-media".into())
        .spawn(move || {
            let cycle = CYCLE_SECS as f64;
            let mut t = 0.0f64;
            loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let is_paused = paused.load(Ordering::Relaxed);
                if freeze_at.is_none() && !is_paused {
                    t += 0.1;
                }
                let pos = freeze_at.map(|f| f as f64).unwrap_or(t % cycle);
                {
                    let mut snap = shared.lock().unwrap_or_else(|e| e.into_inner());
                    *snap = demo_snapshot(pos, is_paused || freeze_at.is_some());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
        .expect("failed to spawn demo media thread")
}

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
///
/// With `freeze_at`, the generator publishes the same deterministic frame
/// at that exact cycle time forever - pixel-identical output for before/
/// after comparison without touching any hardware.
#[allow(clippy::too_many_arguments)]
pub fn spawn_demo(
    shared: Arc<Mutex<SharedState>>,
    bands: usize,
    stop: Arc<AtomicBool>,
    pause: Option<Arc<AtomicBool>>,
    freeze_at: Option<f32>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("spectrum-demo".into())
        .spawn(move || {
            let mut t = 0.0f32;
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(DEMO_TICK);
                let frozen = freeze_at.is_some();
                let paused = !frozen
                    && pause
                        .as_ref()
                        .map(|p| p.load(Ordering::Relaxed))
                        .unwrap_or(false);
                if paused {
                    continue;
                }
                if !frozen {
                    t += DEMO_TICK.as_secs_f32();
                }
                let t = freeze_at.unwrap_or(t);
                let frame = synthetic_frame(t, bands);
                let mut sh = shared.lock().unwrap_or_else(|e| e.into_inner());
                // A frozen frame must not re-fire the beat flash forever.
                if frame.beat && !frozen {
                    sh.beats_pending = sh.beats_pending.saturating_add(1);
                }
                sh.latest = Arc::new(frame);
            }
        })
        .expect("failed to spawn demo thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_frames_are_deterministic() {
        // The generator is a pure function of cycle time: the same frozen
        // time yields pixel-identical frames (what --freeze-at relies on).
        let a = synthetic_frame(4.375, 64);
        let b = synthetic_frame(4.375, 64);
        assert_eq!(a.bars, b.bars);
        assert_eq!(a.spectrum, b.spectrum);
        assert_eq!(a.waveform.len(), b.waveform.len());
        assert!(!a.bars.is_empty());
    }

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
