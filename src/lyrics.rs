//! Lyrics state shared between the media pipeline and the UI.
//!
//! Lyrics are LOCAL sidecar files only: the same-basename `.lrc` next
//! to the audio file (song.mp3 -> song.lrc), parsed by [`crate::lrc`].
//! Nothing is fetched from the network; the states below carry the
//! honest sidecar outcome (synced lines, or a clear missing/malformed
//! error) and the UI renders them as-is.

use std::sync::{Arc, Mutex};

use crate::lrc::LrcLine;
use crate::media::MediaSnapshot;

/// A track identity for the current lyrics (keyed against the media
/// snapshot so stale states never render under a new track).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TrackKey {
    pub artist: String,
    pub title: String,
    pub album: String,
    /// Duration rounded to the second (safeguard only).
    pub duration_s: Option<u64>,
}

impl TrackKey {
    pub fn from_snapshot(snap: &MediaSnapshot) -> Option<TrackKey> {
        if !snap.available {
            return None;
        }
        if snap.title.trim().is_empty() && snap.artist.trim().is_empty() {
            return None;
        }
        Some(TrackKey {
            artist: snap.artist.clone(),
            title: snap.title.clone(),
            album: snap.album.clone(),
            duration_s: snap.duration_s.map(|d| d.round() as u64),
        })
    }
}

/// What the UI renders for the current track. The `key` makes stale
/// states rejectable: a state is only rendered while the key matches.
#[derive(Clone, Debug, PartialEq)]
pub enum LyricsStatus {
    /// No media session at all (or platform without SMTC).
    Unavailable,
    /// A sidecar is being loaded.
    Loading,
    /// Synced, timestamped lyrics (line timing only - plain LRC has no
    /// word onsets and none are invented).
    Synced(Vec<LrcLine>),
    /// Unsycned plain text: honest about NOT being time-aligned (an
    /// .lrc sidecar without timestamps renders as clearly-unsynced
    /// text, never as fake synced lines).
    Plain(String),
    /// The match was instrumental.
    Instrumental,
    /// No sidecar found next to the audio.
    NoMatch,
    /// Sidecar read/parse failure with a human-readable reason.
    Error(String),
}

/// The lyrics the UI shows, keyed to the media identity.
#[derive(Clone, Debug)]
pub struct LyricsUiState {
    /// The media identity this state belongs to; the UI renders it only
    /// while the CURRENT media identity matches, so old words can never
    /// display under a new song.
    pub key: Option<TrackKey>,
    pub status: LyricsStatus,
    /// Bumped on every identity change.
    pub generation: u64,
}

pub type LyricsShared = Arc<Mutex<LyricsUiState>>;

pub fn new_shared() -> LyricsShared {
    Arc::new(Mutex::new(LyricsUiState {
        key: None,
        status: LyricsStatus::Unavailable,
        generation: 0,
    }))
}
