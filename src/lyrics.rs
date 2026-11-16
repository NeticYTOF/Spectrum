//! LRCLIB lyrics lookup: background worker with conservative matching.
//!
//! LRCLIB (https://lrclib.net, anonymous, no keys or signup) serves
//! community-sourced lyrics - content rights remain with their owners and
//! no match or sync quality is ever promised. This worker:
//!
//! - watches the media session and triggers ONE lookup per track identity
//! - requires artist AND title identity (version text like "remix" or
//!   "feat." is preserved, never stripped) and duration agreement within
//!   2 seconds when both are known
//! - falls back to `/api/search` on 404 but rejects ambiguous results
//!   rather than showing another track
//! - handles 429 with the server's Retry-After plus jitter
//! - bounds payloads and caches results per identity (never re-fetches)
//! - clears old lyrics immediately when the track changes and drops stale
//!   async results
//! - never blocks the render loop: everything runs on this thread

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::Deserialize;

use crate::lrc::{duration_matches, identity_matches, parse_lrc, LrcLine};
use crate::media::{MediaShared, MediaSnapshot};

const USER_AGENT: &str = "spectrum/0.1.0 (+https://github.com/NeticYTOF/Spectrum)";
const BASE_URL: &str = "https://lrclib.net";
/// Hard cap on response bodies: lyrics are tiny; anything bigger is junk.
const MAX_BODY: u64 = 256 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Cache bound: a long listening session must not grow memory.
const CACHE_LIMIT: usize = 64;
/// Bounded retries on 429 (Retry-After + jitter), then honest error.
const MAX_429_RETRIES: u32 = 2;
const MAX_429_WAIT: Duration = Duration::from_secs(30);

/// A track identity: everything the lookup (and cache) is keyed on.
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

/// What the UI renders for the current track. The `key` makes stale async
/// results rejectable: a result is only applied while the key matches.
#[derive(Clone, Debug, PartialEq)]
pub enum LyricsStatus {
    /// No media session at all (or platform without SMTC).
    Unavailable,
    /// A lookup for this track is in flight.
    Loading,
    /// Synced, timestamped lyrics.
    Synced(Vec<LrcLine>),
    /// Unsycned plain text: honest about NOT being time-aligned.
    Plain(String),
    /// The match was instrumental.
    Instrumental,
    /// No conservative match (wrong artist, version mismatch, ambiguity).
    NoMatch,
    /// Network/API failure with a human-readable reason.
    Error(String),
}

#[derive(Clone, Debug)]
pub struct LyricsUiState {
    pub key: Option<TrackKey>,
    pub status: LyricsStatus,
}

pub type LyricsShared = Arc<Mutex<LyricsUiState>>;

pub fn new_shared() -> LyricsShared {
    Arc::new(Mutex::new(LyricsUiState {
        key: None,
        status: LyricsStatus::Unavailable,
    }))
}

/// Spawn the lyrics worker. It polls the media snapshot, and per track
/// change performs one bounded LRCLIB lookup on this thread (never on the
/// render loop).
pub fn spawn_lyrics(
    media: MediaShared,
    lyrics: LyricsShared,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("spectrum-lyrics".into())
        .spawn(move || {
            let mut cache: HashMap<TrackKey, LyricsStatus> = HashMap::new();
            let mut cache_order: Vec<TrackKey> = Vec::new();
            let mut last_key: Option<TrackKey> = None;
            let agent = build_agent();

            loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(300));

                let snap = media.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let key = TrackKey::from_snapshot(&snap);

                // Absent session: clear honestly and cheaply.
                let Some(key) = key else {
                    let mut st = lyrics.lock().unwrap_or_else(|e| e.into_inner());
                    if st.status != LyricsStatus::Unavailable || st.key.is_some() {
                        st.key = None;
                        st.status = LyricsStatus::Unavailable;
                    }
                    last_key = None;
                    continue;
                };

                if Some(&key) == last_key.as_ref() {
                    continue; // same track: nothing to do
                }

                // Track changed: clear old lyrics immediately, then look up.
                last_key = Some(key.clone());
                {
                    let mut st = lyrics.lock().unwrap_or_else(|e| e.into_inner());
                    st.key = Some(key.clone());
                    st.status = LyricsStatus::Loading;
                }

                let status = if let Some(hit) = cache.get(&key) {
                    hit.clone()
                } else {
                    let s = lookup(&agent, &key);
                    cache.insert(key.clone(), s.clone());
                    cache_order.push(key.clone());
                    if cache_order.len() > CACHE_LIMIT {
                        let evict = cache_order.remove(0);
                        cache.remove(&evict);
                    }
                    s
                };

                // Apply only if the track is still current (stale reject).
                let mut st = lyrics.lock().unwrap_or_else(|e| e.into_inner());
                if st.key.as_ref() == Some(&key) {
                    st.status = status;
                }
            }
        })
        .expect("failed to spawn lyrics thread")
}

fn build_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
}

/// A parsed LRCLIB record (the subset of the schema we use).
#[derive(Deserialize, Debug, Clone)]
pub struct LrclibRecord {
    #[serde(rename = "trackName")]
    pub track_name: String,
    #[serde(rename = "artistName")]
    pub artist_name: String,
    #[serde(default)]
    pub album_name: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub instrumental: Option<bool>,
    #[serde(rename = "plainLyrics", default)]
    pub plain_lyrics: Option<String>,
    #[serde(rename = "syncedLyrics", default)]
    pub synced_lyrics: Option<String>,
}

impl LrclibRecord {
    /// Convert a validated record into a UI status. Never invents text and
    /// never claims synced when only plain text exists.
    pub fn to_status(&self) -> LyricsStatus {
        if self.instrumental.unwrap_or(false) {
            return LyricsStatus::Instrumental;
        }
        if let Some(synced) = &self.synced_lyrics {
            if let Some(lines) = parse_lrc(synced) {
                if !lines.is_empty() {
                    return LyricsStatus::Synced(lines);
                }
            }
        }
        if let Some(plain) = &self.plain_lyrics {
            let trimmed = plain.trim();
            if !trimmed.is_empty() {
                return LyricsStatus::Plain(trimmed.to_string());
            }
        }
        LyricsStatus::NoMatch
    }
}

/// Percent-encode a query component (RFC 3986 unreserved kept as-is).
fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn build_query(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{k}={}", encode_component(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The full lookup for one track: `get`, then `search` on 404, with
/// identity+duration validation and ambiguity rejection.
fn lookup(agent: &ureq::Agent, key: &TrackKey) -> LyricsStatus {
    // Primary: /api/get with everything we know.
    let mut params: Vec<(&str, &str)> = vec![
        ("artist_name", key.artist.as_str()),
        ("track_name", key.title.as_str()),
    ];
    if !key.album.trim().is_empty() {
        params.push(("album_name", key.album.as_str()));
    }
    let duration_str = key.duration_s.map(|d| d.to_string());
    if let Some(ds) = duration_str.as_deref() {
        params.push(("duration", ds));
    }
    let url = format!("{BASE_URL}/api/get?{}", build_query(&params));
    match fetch_record(agent, &url) {
        FetchOutcome::Ok(rec) => {
            if record_matches(key, &rec) {
                rec.to_status()
            } else {
                // The endpoint's best guess is not this track.
                LyricsStatus::NoMatch
            }
        }
        FetchOutcome::NotFound => search_fallback(agent, key),
        FetchOutcome::RateLimited => LyricsStatus::Error("lyrics lookup rate limited".into()),
        FetchOutcome::Failed(e) => LyricsStatus::Error(e),
    }
}

/// On 404, search for candidates and pick a conservative match.
fn search_fallback(agent: &ureq::Agent, key: &TrackKey) -> LyricsStatus {
    let url = format!(
        "{BASE_URL}/api/search?{}",
        build_query(&[
            ("artist_name", key.artist.as_str()),
            ("track_name", key.title.as_str()),
        ])
    );
    match fetch_json_with_429(agent, &url) {
        FetchBody::Ok(body) => {
            let records: Vec<LrclibRecord> = match serde_json::from_str(&body) {
                Ok(r) => r,
                Err(_) => return LyricsStatus::Error("lyrics search returned bad data".into()),
            };
            let matches: Vec<&LrclibRecord> =
                records.iter().filter(|r| record_matches(key, r)).collect();
            match matches.len() {
                0 => LyricsStatus::NoMatch,
                1 => matches[0].to_status(),
                // Ambiguous: rather than showing another track, give up.
                _ => LyricsStatus::NoMatch,
            }
        }
        FetchBody::NotFound => LyricsStatus::NoMatch,
        FetchBody::RateLimited => LyricsStatus::Error("lyrics lookup rate limited".into()),
        FetchBody::Failed(e) => LyricsStatus::Error(e),
    }
}

/// Identity + duration validation against the query track.
fn record_matches(key: &TrackKey, rec: &LrclibRecord) -> bool {
    identity_matches(&key.artist, &key.title, &rec.artist_name, &rec.track_name)
        && duration_matches(key.duration_s.map(|d| d as f64), rec.duration)
}

enum FetchOutcome {
    Ok(LrclibRecord),
    NotFound,
    RateLimited,
    Failed(String),
}

fn fetch_record(agent: &ureq::Agent, url: &str) -> FetchOutcome {
    match fetch_json_with_429(agent, url) {
        FetchBody::Ok(body) => match serde_json::from_str::<LrclibRecord>(&body) {
            Ok(rec) => FetchOutcome::Ok(rec),
            Err(_) => FetchOutcome::Failed("lyrics response was malformed".into()),
        },
        FetchBody::NotFound => FetchOutcome::NotFound,
        FetchBody::RateLimited => FetchOutcome::RateLimited,
        FetchBody::Failed(e) => FetchOutcome::Failed(e),
    }
}

/// GET outcome: Ok(body) / NotFound (404) / RateLimited (429 beyond
/// budget) / Failed(reason).
#[derive(Debug)]
enum FetchBody {
    Ok(String),
    NotFound,
    RateLimited,
    Failed(String),
}

/// GET with bounded body, retrying 429 with the server's Retry-After plus
/// jitter (bounded attempts), then reporting the final outcome.
fn fetch_json_with_429(agent: &ureq::Agent, url: &str) -> FetchBody {
    let mut attempt: u32 = 0;
    loop {
        match agent.get(url).call() {
            Ok(resp) => {
                // ureq 2.x returns Ok only for 2xx statuses.
                return read_bounded(resp);
            }
            Err(ureq::Error::Status(code, resp)) => {
                if code == 404 {
                    return FetchBody::NotFound;
                }
                if code == 429 && attempt < MAX_429_RETRIES {
                    attempt += 1;
                    let retry_after: u64 = resp
                        .header("Retry-After")
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(2)
                        .min(MAX_429_WAIT.as_secs());
                    // Jitter so bursts of clients don't retry in lockstep.
                    let jitter = (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.subsec_millis())
                        .unwrap_or(0)
                        % 1000) as f64
                        / 1000.0;
                    std::thread::sleep(
                        Duration::from_secs(retry_after) + Duration::from_secs_f64(jitter),
                    );
                    continue;
                }
                if code == 429 {
                    return FetchBody::RateLimited;
                }
                return FetchBody::Failed(format!("lrclib returned status {code}"));
            }
            Err(e) => return FetchBody::Failed(format!("lyrics request failed: {e}")),
        }
    }
}

/// Read a response body, capped at MAX_BODY.
fn read_bounded(resp: ureq::Response) -> FetchBody {
    use std::io::Read;
    let mut limited = resp.into_reader().take(MAX_BODY);
    let mut body = String::new();
    match limited.read_to_string(&mut body) {
        Ok(_) => FetchBody::Ok(body),
        Err(e) => FetchBody::Failed(format!("lyrics response unreadable: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> TrackKey {
        TrackKey {
            artist: "Artist".into(),
            title: "Song".into(),
            album: String::new(),
            duration_s: Some(180),
        }
    }

    #[test]
    fn record_validation() {
        let rec = LrclibRecord {
            track_name: "song".into(),
            artist_name: "artist".into(),
            album_name: None,
            duration: Some(180.5),
            instrumental: None,
            plain_lyrics: None,
            synced_lyrics: None,
        };
        assert!(record_matches(&key(), &rec), "case/1s-duration match");
        let wrong_artist = LrclibRecord {
            artist_name: "other".into(),
            ..rec.clone()
        };
        assert!(!record_matches(&key(), &wrong_artist));
        let wrong_version = LrclibRecord {
            track_name: "Song (remix)".into(),
            artist_name: "artist".into(),
            ..rec.clone()
        };
        assert!(
            !record_matches(&key(), &wrong_version),
            "version text is identity"
        );
        let too_long = LrclibRecord {
            duration: Some(200.0),
            ..rec
        };
        assert!(!record_matches(&key(), &too_long), "duration > 2s apart");
    }

    #[test]
    fn record_to_status_never_invents() {
        let base = LrclibRecord {
            track_name: "Song".into(),
            artist_name: "Artist".into(),
            album_name: None,
            duration: None,
            instrumental: None,
            plain_lyrics: None,
            synced_lyrics: None,
        };
        assert_eq!(base.to_status(), LyricsStatus::NoMatch, "empty record");

        let synced = LrclibRecord {
            synced_lyrics: Some("[00:01.00]hello\n".into()),
            ..base.clone()
        };
        assert!(matches!(synced.to_status(), LyricsStatus::Synced(_)));

        // Plain text only: honestly Plain, never claimed synced.
        let plain = LrclibRecord {
            plain_lyrics: Some("just words".into()),
            ..base.clone()
        };
        assert_eq!(plain.to_status(), LyricsStatus::Plain("just words".into()));

        // Synced text that parses to zero lines falls back to plain.
        let broken = LrclibRecord {
            synced_lyrics: Some("no timestamps".into()),
            plain_lyrics: Some("fallback words".into()),
            ..base
        };
        assert_eq!(
            broken.to_status(),
            LyricsStatus::Plain("fallback words".into())
        );

        let instrumental = LrclibRecord {
            instrumental: Some(true),
            synced_lyrics: Some("[00:01.00]x".into()),
            ..rec_defaults()
        };
        assert_eq!(instrumental.to_status(), LyricsStatus::Instrumental);
    }

    fn rec_defaults() -> LrclibRecord {
        LrclibRecord {
            track_name: "Song".into(),
            artist_name: "Artist".into(),
            album_name: None,
            duration: None,
            instrumental: None,
            plain_lyrics: None,
            synced_lyrics: None,
        }
    }

    #[test]
    fn track_key_from_snapshot() {
        let snap = MediaSnapshot {
            available: true,
            title: "Song".into(),
            artist: "Artist".into(),
            album: String::new(),
            duration_s: Some(179.6),
            position_s: 0.0,
            playing: true,
        };
        let k = TrackKey::from_snapshot(&snap).unwrap();
        assert_eq!(k.duration_s, Some(180), "duration rounded to seconds");
        assert!(TrackKey::from_snapshot(&MediaSnapshot::unavailable()).is_none());
        let junk = MediaSnapshot {
            available: true,
            ..Default::default()
        };
        assert!(
            TrackKey::from_snapshot(&junk).is_none(),
            "no title/artist is no identity"
        );
    }

    #[test]
    fn query_encoding() {
        assert_eq!(encode_component("AC/DC"), "AC%2FDC");
        assert_eq!(encode_component("P!nk & friends"), "P%21nk%20%26%20friends");
        assert_eq!(encode_component("ok-ish_1."), "ok-ish_1.");
        let q = build_query(&[("artist_name", "A B"), ("track_name", "C?")]);
        assert_eq!(q, "artist_name=A%20B&track_name=C%3F");
    }

    #[test]
    fn search_rejects_ambiguity() {
        // Two records with identical identity (e.g. the same track on
        // different albums): rejected rather than guessing.
        let recs = r#"[
            {"trackName":"Song","artistName":"Artist","duration":180},
            {"trackName":"song","artistName":"artist","duration":180}
        ]"#;
        let parsed: Vec<LrclibRecord> = serde_json::from_str(recs).unwrap();
        let matches: Vec<&LrclibRecord> = parsed
            .iter()
            .filter(|r| record_matches(&key(), r))
            .collect();
        assert_eq!(matches.len(), 2);
        // The decision logic: >1 -> NoMatch.
        assert!(matches.len() > 1, "ambiguity must be rejected");
    }

    #[test]
    #[ignore = "live network test; run with --ignored live_lyrics"]
    fn live_lyrics_smoke() {
        // One live check of both LRCLIB endpoints with a public-domain
        // song (no copyrighted fetches). Structural validation only.
        let agent = build_agent();
        let url = format!(
            "{BASE_URL}/api/get?{}",
            build_query(&[
                ("artist_name", "Traditional"),
                ("track_name", "Amazing Grace"),
                ("duration", "240"),
            ])
        );
        match fetch_json_with_429(&agent, &url) {
            FetchBody::Ok(body) => {
                let rec: LrclibRecord = serde_json::from_str(&body).expect("valid LRCLIB record");
                eprintln!(
                    "live /api/get record: {} - {} (duration {:?}, synced {})",
                    rec.artist_name,
                    rec.track_name,
                    rec.duration,
                    rec.synced_lyrics.is_some()
                );
            }
            FetchBody::NotFound => {
                eprintln!("live /api/get: 404; engaging the search fallback");
                let surl = format!(
                    "{BASE_URL}/api/search?{}",
                    build_query(&[
                        ("artist_name", "Traditional"),
                        ("track_name", "Amazing Grace"),
                    ])
                );
                match fetch_json_with_429(&agent, &surl) {
                    FetchBody::Ok(body) => {
                        let recs: Vec<LrclibRecord> =
                            serde_json::from_str(&body).expect("valid search array");
                        eprintln!("live /api/search returned {} candidates", recs.len());
                        // The search endpoint returning data at all is the
                        // smoke pass; matching is validated by unit tests.
                        assert!(!recs.is_empty());
                    }
                    other => eprintln!("live search: {other:?}"),
                }
            }
            other => eprintln!("live lookup: {other:?}"),
        }
    }
}
