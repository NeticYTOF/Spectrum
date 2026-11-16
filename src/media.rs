//! Media session tracking: what is playing right now.
//!
//! On Windows this polls the System Media Transport Controls (SMTC) via the
//! `windows` crate - the same metadata players expose to the OS (title,
//! artist, album, duration, timeline position, playing state). On other
//! platforms there is no equivalent OS API, so the state is honestly
//! `available: false` and the lyrics mode shows its unavailable state.
//!
//! The SMTC position does not advance by itself: the poller re-reads it
//! every second and interpolates between reads while playing, which both
//! smooths the timeline and corrects drift and seeks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A snapshot of the current media session.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MediaSnapshot {
    /// Whether any media session exists at all.
    pub available: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Track duration in seconds, when the player reports it.
    pub duration_s: Option<f64>,
    /// Timeline position in seconds (interpolated while playing).
    pub position_s: f64,
    /// Whether the track is currently playing (vs paused/stopped).
    pub playing: bool,
}

impl MediaSnapshot {
    pub fn unavailable() -> Self {
        Self::default()
    }
}

pub type MediaShared = Arc<Mutex<MediaSnapshot>>;

/// An immutable SMTC reading: the position as of `read_at`, plus the
/// track state that goes with it. Between polls the current position is
/// COMPUTED from this base - never accumulated onto the published
/// snapshot, which previously ran ~4.5x fast between polls and snapped
/// back at each poll.
#[derive(Clone, Debug)]
pub struct MediaBase {
    pub snapshot: MediaSnapshot,
    /// When `snapshot.position_s` was accurate.
    pub read_at: Instant,
}

/// Extrapolate the current position from an immutable base. Pure and
/// clock-injectable for tests.
///
/// - advances only while `playing`
/// - clamps to the track duration (end boundary)
/// - never goes negative
pub fn extrapolate(base: &MediaBase, now: Instant) -> f64 {
    let mut pos = base.snapshot.position_s;
    if base.snapshot.playing {
        pos += now.duration_since(base.read_at).as_secs_f64();
    }
    if let Some(d) = base.snapshot.duration_s {
        pos = pos.min(d);
    }
    pos.max(0.0)
}

pub fn new_shared() -> MediaShared {
    Arc::new(Mutex::new(MediaSnapshot::unavailable()))
}

/// Poll the OS media session until `stop`. Publishes snapshots at ~10 Hz
/// computed from an immutable base (see [`MediaBase`]), re-reading SMTC
/// about once per second so drift, seeks and track changes correct
/// themselves within a second.
pub fn spawn_media(shared: MediaShared, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("spectrum-media".into())
        .spawn(move || {
            let mut last_poll = Instant::now() - Duration::from_secs(10);
            let mut base = MediaBase {
                snapshot: MediaSnapshot::unavailable(),
                read_at: Instant::now(),
            };
            loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let now = Instant::now();
                if last_poll.elapsed() >= Duration::from_secs(1) {
                    last_poll = now;
                    base = read_base();
                    // Publish the fresh base immediately so track changes
                    // and seeks land without waiting for the next tick.
                    let mut snap = shared.lock().unwrap_or_else(|e| e.into_inner());
                    *snap = base.snapshot.clone();
                    continue;
                }
                // Between polls: compute from the immutable base.
                let mut snap = base.snapshot.clone();
                if snap.available {
                    snap.position_s = extrapolate(&base, now);
                }
                let mut sh = shared.lock().unwrap_or_else(|e| e.into_inner());
                *sh = snap;
                drop(sh);
                std::thread::sleep(Duration::from_millis(100));
            }
        })
        .expect("failed to spawn media thread")
}

/// Read SMTC and build an immutable base, anchoring `read_at` with the
/// timeline's `LastUpdatedTime` so extrapolation starts from when the
/// position was actually accurate (the poll itself can lag the player).
pub fn read_base() -> MediaBase {
    let now = Instant::now();
    let state = read_smtc();
    let anchor_lag = state
        .position_last_updated
        .map(|dt| dt.clamp(0.0, 10.0))
        .unwrap_or(0.0);
    MediaBase {
        snapshot: state.to_snapshot(),
        read_at: now
            .checked_sub(Duration::from_secs_f64(anchor_lag))
            .unwrap_or(now),
    }
}

#[cfg(not(windows))]
mod imp {
    //! Non-Windows: no OS media API, honestly unavailable. The module is
    //! trivially compilable (std only) on every platform.

    /// A raw SMTC-equivalent reading. On non-Windows there is never one.
    #[derive(Default)]
    pub struct SmtcState {
        pub position_last_updated: Option<f64>,
    }

    impl SmtcState {
        pub fn to_snapshot(&self) -> super::MediaSnapshot {
            super::MediaSnapshot::unavailable()
        }
    }

    pub fn read_smtc() -> SmtcState {
        SmtcState::default()
    }
}

#[cfg(windows)]
mod imp {
    use super::MediaSnapshot;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSessionManager as SessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
    };

    /// A raw SMTC reading, refreshed every poll.
    #[derive(Default)]
    pub struct SmtcState {
        available: bool,
        title: String,
        artist: String,
        album: String,
        duration_s: Option<f64>,
        position_s: f64,
        playing: bool,
        /// Seconds between the position reading and the poll (the
        /// player updated the timeline slightly before we read it).
        pub position_last_updated: Option<f64>,
    }

    impl SmtcState {
        pub fn to_snapshot(&self) -> MediaSnapshot {
            MediaSnapshot {
                available: self.available,
                title: self.title.clone(),
                artist: self.artist.clone(),
                album: self.album.clone(),
                duration_s: self.duration_s,
                position_s: self.position_s,
                playing: self.playing,
            }
        }
    }

    fn to_seconds(span: windows::Foundation::TimeSpan) -> f64 {
        span.Duration as f64 / 10_000_000.0
    }

    pub fn read_smtc() -> SmtcState {
        let Ok(op) = SessionManager::RequestAsync() else {
            return SmtcState::default();
        };
        let manager = match op.get() {
            Ok(m) => m,
            Err(_) => return SmtcState::default(),
        };
        // GetCurrentSession returns the active session; None means no
        // player has registered one (honest unavailable, not an error).
        // GetCurrentSession: in windows 0.58 this is Result<Session> where
        // the session is a nullable COM interface - a missing session
        // surfaces as an error or an unusable object. Absent = honest
        // unavailable, not a failure.
        let Ok(session) = manager.GetCurrentSession() else {
            return SmtcState::default();
        };
        // Probe the session cheaply: a dead/unregistered session fails on
        // playback info; that keeps stale sessions from polluting the UI.
        if session.GetPlaybackInfo().is_err() {
            return SmtcState::default();
        }

        let props = match session.TryGetMediaPropertiesAsync() {
            Ok(op) => match op.get() {
                Ok(p) => p,
                Err(_) => return SmtcState::default(), // stale/unreadable session
            },
            Err(_) => return SmtcState::default(),
        };
        let title = props.Title().map(|s| s.to_string()).unwrap_or_default();
        let artist = props.Artist().map(|s| s.to_string()).unwrap_or_default();
        let album = props
            .AlbumTitle()
            .map(|s| s.to_string())
            .unwrap_or_default();

        let timeline = session.GetTimelineProperties().ok();
        let position_s = timeline
            .as_ref()
            .and_then(|t| t.Position().ok())
            .map(to_seconds)
            .unwrap_or(0.0);
        let duration_s = timeline
            .as_ref()
            .and_then(|t| t.EndTime().ok())
            .map(to_seconds)
            .filter(|d| *d > 0.0);
        // LastUpdatedTime anchors extrapolation: how long ago (in wall
        // time) the position value was accurate. Clamped later to a sane
        // range; garbage values just fall back to "just read".
        let position_last_updated = timeline
            .as_ref()
            .and_then(|t| t.LastUpdatedTime().ok())
            .and_then(|dt| {
                let now_100ns = system_time_100ns();
                let then = dt.UniversalTime;
                if then > 0 && now_100ns > then {
                    Some(((now_100ns - then) as f64) / 10_000_000.0)
                } else {
                    None
                }
            });

        let playing = session
            .GetPlaybackInfo()
            .ok()
            .and_then(|i| i.PlaybackStatus().ok())
            .map(|s| s == PlaybackStatus::Playing)
            .unwrap_or(false);

        // A session with no title/artist is not useful metadata; treat it
        // as unavailable so the lyrics UI never shows junk.
        let available = !title.trim().is_empty() || !artist.trim().is_empty();

        SmtcState {
            available,
            title,
            artist,
            album,
            duration_s,
            position_s,
            playing,
            position_last_updated,
        }
    }

    /// Current wall time in Windows 100ns ticks (same clock as
    /// Foundation::DateTime.UniversalTime) for the LastUpdatedTime anchor.
    fn system_time_100ns() -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64 / 100 + 116_444_736_000_000_000)
            .unwrap_or(0)
    }
}

pub use imp::{read_smtc, SmtcState};

#[cfg(test)]
mod tests {
    use super::*;

    fn base(pos: f64, at: Instant, playing: bool, dur: Option<f64>) -> MediaBase {
        MediaBase {
            snapshot: MediaSnapshot {
                available: true,
                title: "T".into(),
                artist: "A".into(),
                album: String::new(),
                duration_s: dur,
                position_s: pos,
                playing,
            },
            read_at: at,
        }
    }

    /// Ten 100ms ticks between polls must advance ~0.9s total - not the
    /// cumulative-elapsed ~4.5s the old accumulator produced.
    #[test]
    fn ten_ticks_advance_nine_tenths_of_a_second() {
        let t0 = Instant::now();
        let b = base(10.0, t0, true, Some(300.0));
        let mut prev = 10.0;
        let mut total = 0.0;
        for tick in 1..=9 {
            let now = t0 + Duration::from_millis(100 * tick);
            let pos = extrapolate(&b, now);
            total += pos - prev;
            prev = pos;
        }
        assert!((total - 0.9).abs() < 0.01, "advanced {total}");
    }

    #[test]
    fn fresh_poll_resets_the_base() {
        // A seek jumps: the new base position wins immediately.
        let t0 = Instant::now();
        let b1 = base(10.0, t0, true, Some(300.0));
        let p1 = extrapolate(&b1, t0 + Duration::from_millis(400));
        assert!((p1 - 10.4).abs() < 0.01);

        // New poll at t0+0.5s sees the player seeked to 200.
        let t1 = t0 + Duration::from_millis(500);
        let b2 = base(200.0, t1, true, Some(300.0));
        let p2 = extrapolate(&b2, t1 + Duration::from_millis(100));
        assert!((p2 - 200.1).abs() < 0.01, "seek snapped to {p2}");
    }

    #[test]
    fn paused_does_not_advance() {
        let t0 = Instant::now();
        let b = base(42.0, t0, false, Some(300.0));
        for tick in 1..=9 {
            let pos = extrapolate(&b, t0 + Duration::from_millis(100 * tick));
            assert!((pos - 42.0).abs() < 1e-9, "paused moved to {pos}");
        }
    }

    #[test]
    fn stale_or_huge_elapsed_clamps_to_duration() {
        let t0 = Instant::now();
        let b = base(295.0, t0, true, Some(300.0));
        // "Stale" clock: a huge elapsed gap must clamp at the end.
        let pos = extrapolate(&b, t0 + Duration::from_secs(600));
        assert!((pos - 300.0).abs() < 1e-9, "clamped to {pos}");
    }

    #[test]
    fn missing_duration_still_advances() {
        let t0 = Instant::now();
        let b = base(1.0, t0, true, None);
        let pos = extrapolate(&b, t0 + Duration::from_millis(500));
        assert!((pos - 1.5).abs() < 0.01);
    }

    #[test]
    fn anchor_lag_shifts_read_at_back() {
        // LastUpdatedTime says the position was accurate 2s before the
        // poll: extrapolation must include those 2 seconds.
        let t0 = Instant::now();
        let read_at = t0.checked_sub(Duration::from_secs(2)).unwrap_or(t0);
        let b = base(5.0, read_at, true, Some(300.0));
        let pos = extrapolate(&b, t0);
        assert!((pos - 7.0).abs() < 0.01, "anchored pos {pos}");
    }

    #[test]
    fn position_never_negative() {
        let t0 = Instant::now();
        let b = base(0.0, t0, true, None);
        let pos = extrapolate(&b, t0);
        assert!(pos >= 0.0);
    }
}
