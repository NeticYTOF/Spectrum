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

pub fn new_shared() -> MediaShared {
    Arc::new(Mutex::new(MediaSnapshot::unavailable()))
}

/// Poll the OS media session until `stop`. Publishes interpolated
/// snapshots at ~10 Hz and re-reads SMTC about once per second.
pub fn spawn_media(shared: MediaShared, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("spectrum-media".into())
        .spawn(move || {
            let mut last_read = Instant::now() - Duration::from_secs(10);
            loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let now = Instant::now();
                if last_read.elapsed() >= Duration::from_secs(1) {
                    last_read = now;
                    let fresh = read_smtc();
                    // Publish the fresh read immediately so track changes
                    // and seeks land without waiting for the next tick.
                    let mut snap = shared.lock().unwrap_or_else(|e| e.into_inner());
                    *snap = fresh.to_snapshot(0.0);
                    continue;
                }
                // Interpolate between reads while playing.
                let dt = now.duration_since(last_read).as_secs_f64();
                let mut snap = shared.lock().unwrap_or_else(|e| e.into_inner());
                if snap.available && snap.playing {
                    snap.position_s += dt;
                    if let Some(d) = snap.duration_s {
                        snap.position_s = snap.position_s.min(d);
                    }
                }
                drop(snap);
                std::thread::sleep(Duration::from_millis(100));
            }
        })
        .expect("failed to spawn media thread")
}

#[cfg(not(windows))]
mod imp {
    //! Non-Windows: no OS media API, honestly unavailable. The module is
    //! trivially compilable (std only) on every platform.

    #[derive(Default)]
    pub struct SmtcState;

    impl SmtcState {
        pub fn to_snapshot(&self, _dt: f64) -> super::MediaSnapshot {
            super::MediaSnapshot::unavailable()
        }
    }

    pub fn read_smtc() -> SmtcState {
        SmtcState
    }
}

#[cfg(windows)]
mod imp {
    use super::MediaSnapshot;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSessionManager as SessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
    };

    /// A cached SMTC reading: raw values from the OS, refreshed every poll.
    #[derive(Default)]
    pub struct SmtcState {
        available: bool,
        title: String,
        artist: String,
        album: String,
        duration_s: Option<f64>,
        position_s: f64,
        playing: bool,
    }

    impl SmtcState {
        /// Snapshot with `dt` seconds of interpolation already applied by
        /// the caller (the poller publishes fresh reads with dt = 0).
        pub fn to_snapshot(&self, _dt: f64) -> MediaSnapshot {
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
        }
    }
}

pub use imp::{read_smtc, SmtcState};
