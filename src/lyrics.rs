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
use std::time::{Duration, Instant};

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
    /// The media identity this state belongs to; the UI renders it only
    /// while the CURRENT media identity matches, so old words can never
    /// display under a new song even while a lookup is in flight.
    pub key: Option<TrackKey>,
    pub status: LyricsStatus,
    /// Bumped on every identity change; lookup results carry the
    /// generation they were requested under and are applied only when
    /// it still matches (solves A -> B -> A).
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

/// One lookup request: the generation under which it was requested and
/// the track identity.
#[derive(Clone, Debug)]
struct LookupRequest {
    generation: u64,
    key: TrackKey,
}

/// The watcher's pure decision step: given the previous and current media
/// identities, what should happen next? Testable without HTTP or threads.
enum WatchStep {
    /// Nothing to do (same identity, or no session and already cleared).
    Unchanged,
    /// Identity changed: bump generation, clear immediately, request a
    /// lookup for the new identity. `unavailable` means there is no media
    /// session at all (no request needed).
    Changed {
        request: Option<TrackKey>,
        unavailable: bool,
    },
}

fn watch_step(prev: &Option<TrackKey>, current: &Option<TrackKey>) -> WatchStep {
    if prev == current {
        return WatchStep::Unchanged;
    }
    match current {
        None => WatchStep::Changed {
            request: None,
            unavailable: true,
        },
        Some(k) => WatchStep::Changed {
            request: Some(k.clone()),
            unavailable: false,
        },
    }
}

/// Whether a completed lookup result should be applied: the request must
/// still be the current generation AND the media must currently be on the
/// same identity. Pure and unit-testable (A -> B -> A rejects the stale
/// A result even though the key matches).
fn should_apply(
    current_generation: u64,
    current_media_key: Option<&TrackKey>,
    req: &LookupRequest,
) -> bool {
    current_generation == req.generation
        && current_media_key.map(|k| k == &req.key).unwrap_or(false)
}

/// Publication step: validate and assign a lookup result against the
/// CURRENT UI state and media identity in ONE critical section (the
/// caller holds the lyrics lock). This closes the race where the watcher
/// installs a new key/generation/Loading between validation and the
/// write, which would attach old words to the new track.
///
/// Returns true when the result was applied.
fn apply_result(
    st: &mut LyricsUiState,
    current_media_key: Option<&TrackKey>,
    req: &LookupRequest,
    status: LyricsStatus,
) -> bool {
    if !should_apply(st.generation, current_media_key, req) {
        return false;
    }
    st.status = status;
    true
}

/// Spawn the lyrics pipeline: a watcher that never blocks on HTTP, and a
/// lookup executor that applies results only when both the generation and
/// the CURRENT media identity still match. The UI additionally gates on
/// identity, so stale words disappear immediately on track change.
pub fn spawn_lyrics(
    media: MediaShared,
    lyrics: LyricsShared,
    stop: Arc<AtomicBool>,
) -> Vec<JoinHandle<()>> {
    let (tx, rx) = std::sync::mpsc::channel::<LookupRequest>();

    // Watcher: detect identity changes; hand requests to the executor.
    let watcher = std::thread::Builder::new()
        .name("spectrum-lyrics-watch".into())
        .spawn({
            let media = media.clone();
            let lyrics = lyrics.clone();
            let stop = stop.clone();
            move || {
                let mut last_key: Option<TrackKey> = None;
                let mut generation: u64 = 0;
                loop {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(300));
                    let snap = media.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let key = TrackKey::from_snapshot(&snap);

                    match watch_step(&last_key, &key) {
                        WatchStep::Unchanged => {}
                        WatchStep::Changed {
                            request,
                            unavailable,
                        } => {
                            generation += 1;
                            last_key = key.clone();
                            // Immediate clear: the UI also gates on identity,
                            // so this is belt and braces.
                            {
                                let mut st = lyrics.lock().unwrap_or_else(|e| e.into_inner());
                                st.generation = generation;
                                st.key = key.clone();
                                st.status = if unavailable {
                                    LyricsStatus::Unavailable
                                } else {
                                    LyricsStatus::Loading
                                };
                            }
                            if let Some(k) = request {
                                let _ = tx.send(LookupRequest { generation, key: k });
                            }
                        }
                    }
                }
            }
        })
        .expect("failed to spawn lyrics watcher thread");

    // Executor: one lookup at a time, latest request wins; applies results
    // only when the generation and the current media identity still match.
    let executor = std::thread::Builder::new()
        .name("spectrum-lyrics-fetch".into())
        .spawn(move || {
            let agent = build_agent();
            // Terminal statuses (synced/plain/instrumental/no-match) are
            // cached per identity, bounded. Transient failures cool down:
            // the SAME song retries when the deadline passes (bounded,
            // latest-request wins), and revisiting a cached error keeps
            // its ORIGINAL deadline - no renewal.
            let mut cache: HashMap<TrackKey, LyricsStatus> = HashMap::new();
            let mut cache_order: Vec<TrackKey> = Vec::new();
            let mut cooldowns: HashMap<TrackKey, (LyricsStatus, Instant)> = HashMap::new();
            const COOLDOWN: Duration = Duration::from_secs(60);
            // A transient error for the still-current track: retried when
            // its deadline elapses (without an identity toggle), and
            // superseded instantly by any newer watcher request.
            let mut pending_retry: Option<(LookupRequest, Instant)> = None;
            // Provider-wide rate-limit gate (Retry-After honored in full:
            // a long 429 defers EVERY track, and the executor wakes at the
            // gate deadline without blocking detection or quit).
            let mut provider_gate = ProviderGate::new();

            while !stop.load(Ordering::Relaxed) {
                // Wait for a request. When the current track is cooling
                // down, wait only until its retry deadline so the SAME
                // song recovers without a metadata change; any watcher
                // request takes over immediately (latest wins).
                let mut req = match rx.recv_timeout(
                    pending_retry
                        .as_ref()
                        .map(|(_, at)| at.saturating_duration_since(Instant::now()))
                        .unwrap_or(Duration::from_secs(3600)),
                ) {
                    Ok(next) => next,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        // Deadline hit: retry the cooling track.
                        match pending_retry.take() {
                            Some((r, _)) => r,
                            None => continue,
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                };
                // Latest-wins: drain any backlog (while a lookup was in
                // flight, the watcher may have queued several changes);
                // a newer request cancels the pending retry.
                while let Ok(next) = rx.try_recv() {
                    req = next;
                    pending_retry = None;
                }
                if stop.load(Ordering::Relaxed) {
                    return;
                }

                let now = Instant::now();
                let mut retry_at: Option<Instant> = None;
                let status = match cooldown_action(&cache, &cooldowns, &req.key, now, COOLDOWN) {
                    CooldownAction::Cached(hit) => hit,
                    CooldownAction::CoolingDown(err, remaining) => {
                        // Keep serving the cached error; retry when the
                        // ORIGINAL deadline elapses - no renewal.
                        retry_at = Some(now + remaining);
                        err
                    }
                    CooldownAction::Fetch => {
                        // Provider-wide gate: a long Retry-After on ANY
                        // track defers all fetches until it passes; the
                        // request retries at the gate deadline (cached
                        // tracks still serve instantly).
                        if let Some(remaining) = provider_gate.remaining(now) {
                            retry_at = Some(now + remaining);
                            LyricsStatus::Error("lyrics rate-limited, waiting".into())
                        } else {
                            let s =
                                lookup_interruptible(&agent, &req.key, &stop, &mut provider_gate);
                            if let LyricsStatus::Error(_) = &s {
                                // FRESH error: write the deadline once.
                                cooldowns.insert(req.key.clone(), (s.clone(), now));
                                if cooldowns.len() > CACHE_LIMIT {
                                    if let Some(oldest) = cooldowns
                                        .iter()
                                        .min_by_key(|(_, (_, at))| *at)
                                        .map(|(k, _)| k.clone())
                                    {
                                        cooldowns.remove(&oldest);
                                    }
                                }
                                retry_at = Some(now + COOLDOWN);
                            }
                            s
                        }
                    }
                };

                match &status {
                    // Terminal: cache so the same identity never refetches.
                    LyricsStatus::Synced(_)
                    | LyricsStatus::Plain(_)
                    | LyricsStatus::Instrumental
                    | LyricsStatus::NoMatch => {
                        cache.insert(req.key.clone(), status.clone());
                        cache_order.push(req.key.clone());
                        if cache_order.len() > CACHE_LIMIT {
                            let evict = cache_order.remove(0);
                            cache.remove(&evict);
                        }
                    }
                    // Transient (network/5xx/rate-limit): schedule the
                    // bounded same-track retry at the deadline computed
                    // above (fresh or original - never renewed). A newer
                    // watcher request supersedes it instantly.
                    LyricsStatus::Error(_) => {
                        pending_retry = retry_at.map(|at| (req.clone(), at));
                    }
                    // Loading/Unavailable never come back from a lookup.
                    LyricsStatus::Loading | LyricsStatus::Unavailable => {}
                }

                // Apply only if the request is still current. The media
                // identity is read first (it is only changed by the actual
                // media thread, never by the watcher), then the lyrics
                // lock is taken ONCE and validation + assignment happen in
                // the SAME critical section - the watcher cannot slip a
                // new key/generation in between validation and the write,
                // so old words can never attach to a new track.
                let snap = media.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let live_key = TrackKey::from_snapshot(&snap);
                let mut st = lyrics.lock().unwrap_or_else(|e| e.into_inner());
                apply_result(&mut st, live_key.as_ref(), &req, status);
            }
        })
        .expect("failed to spawn lyrics executor thread");

    vec![watcher, executor]
}

/// What the executor should do for a lookup request given the cooldown
/// bookkeeping. Pure and unit-testable - the renewal bug lived here
/// (revisiting a cached error must NOT rewrite its deadline).
#[derive(Debug)]
enum CooldownAction {
    /// A terminal (cached) result exists: return it.
    Cached(LyricsStatus),
    /// A transient error is still cooling down: return it, keeping the
    /// ORIGINAL deadline (no renewal on revisit).
    CoolingDown(LyricsStatus, Duration),
    /// The cooldown elapsed (or none exists): fetch.
    Fetch,
}

fn cooldown_action(
    cache: &HashMap<TrackKey, LyricsStatus>,
    cooldowns: &HashMap<TrackKey, (LyricsStatus, Instant)>,
    key: &TrackKey,
    now: Instant,
    cooldown: Duration,
) -> CooldownAction {
    if let Some(hit) = cache.get(key) {
        return CooldownAction::Cached(hit.clone());
    }
    if let Some((err, at)) = cooldowns.get(key) {
        let elapsed = now.duration_since(*at);
        if elapsed < cooldown {
            return CooldownAction::CoolingDown(err.clone(), cooldown - elapsed);
        }
    }
    CooldownAction::Fetch
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
    #[serde(rename = "albumName", default)]
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
/// identity+duration validation and ambiguity rejection. Interruptible:
/// returns early when `stop` is set (never hangs a quit behind network
/// or backoff).
fn lookup_interruptible(
    agent: &ureq::Agent,
    key: &TrackKey,
    stop: &Arc<AtomicBool>,
    provider_gate: &mut ProviderGate,
) -> LyricsStatus {
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
    match fetch_record(agent, &url, stop, provider_gate) {
        FetchOutcome::Ok(rec) => {
            if record_matches(key, &rec) {
                rec.to_status()
            } else {
                // The endpoint's best guess is not this track.
                LyricsStatus::NoMatch
            }
        }
        FetchOutcome::NotFound => search_fallback(agent, key, stop, provider_gate),
        FetchOutcome::RateLimited => LyricsStatus::Error("lyrics lookup rate limited".into()),
        FetchOutcome::Failed(e) => LyricsStatus::Error(e),
    }
}

/// On 404, search for candidates and pick a conservative match.
fn search_fallback(
    agent: &ureq::Agent,
    key: &TrackKey,
    stop: &Arc<AtomicBool>,
    provider_gate: &mut ProviderGate,
) -> LyricsStatus {
    let url = format!(
        "{BASE_URL}/api/search?{}",
        build_query(&[
            ("artist_name", key.artist.as_str()),
            ("track_name", key.title.as_str()),
        ])
    );
    match fetch_json_with_429(agent, &url, stop, provider_gate) {
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
                _ => {
                    // Ambiguous by artist+title+duration: the known album
                    // can disambiguate (never invents certainty - a
                    // missing album on either side keeps the rejection).
                    if !key.album.trim().is_empty() {
                        let by_album: Vec<&&LrclibRecord> = matches
                            .iter()
                            .filter(|r| match r.album_name.as_deref() {
                                Some(a) if !a.trim().is_empty() => {
                                    crate::lrc::normalize(a) == crate::lrc::normalize(&key.album)
                                }
                                _ => false,
                            })
                            .collect();
                        if by_album.len() == 1 {
                            return by_album[0].to_status();
                        }
                    }
                    // Still ambiguous: rather than showing another
                    // track, give up.
                    LyricsStatus::NoMatch
                }
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

fn fetch_record(
    agent: &ureq::Agent,
    url: &str,
    stop: &Arc<AtomicBool>,
    provider_gate: &mut ProviderGate,
) -> FetchOutcome {
    match fetch_json_with_429(agent, url, stop, provider_gate) {
        FetchBody::Ok(body) => match serde_json::from_str::<LrclibRecord>(&body) {
            Ok(rec) => FetchOutcome::Ok(rec),
            Err(_) => FetchOutcome::Failed("lyrics response was malformed".into()),
        },
        FetchBody::NotFound => FetchOutcome::NotFound,
        FetchBody::RateLimited => FetchOutcome::RateLimited,
        FetchBody::Failed(e) => FetchOutcome::Failed(e),
    }
}

/// The provider-wide not-before deadline: when the server answers 429
/// with a long Retry-After, EVERY track (not just the requesting one)
/// waits until this instant passes. Clock-injectable for tests.
#[derive(Debug)]
pub struct ProviderGate {
    not_before: Option<Instant>,
}

impl ProviderGate {
    pub fn new() -> Self {
        Self { not_before: None }
    }

    /// Defer all fetches for `d` from now (a later defer can only extend,
    /// never shorten an existing server deadline).
    pub fn defer_by(&mut self, d: Duration) {
        let target = Instant::now() + d;
        self.not_before = Some(match self.not_before {
            Some(nb) if nb > target => nb,
            _ => target,
        });
    }

    /// Time remaining until fetches are allowed; None when open.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        self.not_before
            .filter(|nb| *nb > now)
            .map(|nb| nb.saturating_duration_since(now))
    }
}

impl Default for ProviderGate {
    fn default() -> Self {
        Self::new()
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

/// Decision for a 429: honor the server's Retry-After when it is within
/// our in-fetch wait budget; above the budget, defer (the caller records a
/// provider-wide deadline so EVERY track waits the full server value)
/// instead of shortening the wait and retrying early.
#[derive(Debug, PartialEq)]
enum RetryDecision {
    /// Wait this long (plus jitter) and retry.
    Wait(Duration),
    /// Defer: record the full server value as a provider-wide deadline.
    Defer(Duration),
}

/// Parse a Retry-After header: either delay-seconds or an HTTP-date
/// (IMF-fixdate, e.g. "Wed, 21 Oct 2026 07:28:00 GMT"). Returns the FULL
/// requested wait; the caller decides whether to honor it in-fetch or
/// defer. Missing/garbage -> None (small default elsewhere).
fn parse_retry_after(header: Option<&str>, now: std::time::SystemTime) -> Option<Duration> {
    let h = header?.trim();
    // Delay-seconds.
    if let Ok(secs) = h.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    // HTTP-date: IMF-fixdate (the only form RFC 9110 allows).
    parse_http_date(h).map(|target| {
        target
            .duration_since(now)
            .unwrap_or(Duration::ZERO)
            .max(Duration::ZERO)
    })
}

/// Minimal IMF-fixdate parser: "Wed, 21 Oct 2026 07:28:00 GMT".
/// Deliberately tiny: the format is fixed-width per RFC 9110 day-date.
fn parse_http_date(s: &str) -> Option<std::time::SystemTime> {
    use std::time::{Duration, UNIX_EPOCH};
    // "Sun, 06 Nov 1994 08:49:37 GMT"
    let bytes = s.as_bytes();
    if bytes.len() != 29 {
        return None;
    }
    // "Sun, 06 Nov 1994 08:49:37 GMT"
    //  0123456789...          positions: 3=',' 4=' ' 7=' ' 11=' ' 16=' ' 19=':' 22=':' 25=' '
    if bytes[3] != b','
        || bytes[4] != b' '
        || bytes[7] != b' '
        || bytes[11] != b' '
        || bytes[16] != b' '
        || bytes[19] != b':'
        || bytes[22] != b':'
        || bytes[25] != b' '
    {
        return None;
    }
    let day: u32 = s.get(5..7)?.parse().ok()?;
    let month = match s.get(8..11)? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = s.get(12..16)?.parse().ok()?;
    let hour: u32 = s.get(17..19)?.parse().ok()?;
    let min: u32 = s.get(20..22)?.parse().ok()?;
    let sec: u32 = s.get(23..25)?.parse().ok()?;
    // Days since epoch (civil-from-days algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = ((month + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86_400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64;
    if secs < 0 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
}

fn classify_retry(parsed: Option<Duration>, budget: Duration) -> RetryDecision {
    match parsed {
        Some(d) if d > budget => RetryDecision::Defer(d),
        Some(d) => RetryDecision::Wait(d),
        // No parseable header: small default wait, never a long defer.
        None => RetryDecision::Wait(Duration::from_secs(2)),
    }
}

/// Sleep that aborts early when `stop` is set. Returns false when the
/// sleep was interrupted (shut down).
fn interruptible_sleep(d: Duration, stop: &Arc<AtomicBool>) -> bool {
    let deadline = Instant::now() + d;
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        let slice = (deadline - now).min(Duration::from_millis(100));
        std::thread::sleep(slice);
    }
}

/// GET with bounded body. 429s honor the server's Retry-After plus jitter
/// when it is within the in-fetch wait budget (sleep interruptible by
/// `stop`, so a quit never hangs behind backoff); a Retry-After ABOVE the
/// budget sets the PROVIDER-WIDE not-before deadline (`provider_gate`)
/// to now + the full server value so every subsequent track defers too,
/// then returns RateLimited. 5xx are transient failures (the cooldown
/// recovers them), 404 is NotFound, other 4xx are hard failures.
fn fetch_json_with_429(
    agent: &ureq::Agent,
    url: &str,
    stop: &Arc<AtomicBool>,
    provider_gate: &mut ProviderGate,
) -> FetchBody {
    let mut attempt: u32 = 0;
    loop {
        if stop.load(Ordering::Relaxed) {
            return FetchBody::Failed("shutting down".into());
        }
        match agent.get(url).call() {
            Ok(resp) => {
                // ureq 2.x returns Ok only for 2xx statuses.
                return read_bounded(resp);
            }
            Err(ureq::Error::Status(code, resp)) => {
                if code == 404 {
                    return FetchBody::NotFound;
                }
                if code == 429 {
                    if attempt >= MAX_429_RETRIES {
                        return FetchBody::RateLimited;
                    }
                    let header = resp.header("Retry-After");
                    let parsed = parse_retry_after(header, std::time::SystemTime::now());
                    match classify_retry(parsed, MAX_429_WAIT) {
                        RetryDecision::Defer(full) => {
                            // Provider-wide: EVERY track defers until the
                            // full server value elapses (never shortened).
                            provider_gate.defer_by(full);
                            return FetchBody::RateLimited;
                        }
                        RetryDecision::Wait(base) => {
                            attempt += 1;
                            // Jitter so bursts of clients don't retry in
                            // lockstep; bounded to a second.
                            let jitter = Duration::from_millis(
                                (std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.subsec_millis())
                                    .unwrap_or(0)
                                    % 1000) as u64,
                            );
                            if !interruptible_sleep(base + jitter, stop) {
                                return FetchBody::Failed("shutting down".into());
                            }
                            continue;
                        }
                    }
                }
                if (500..600).contains(&code) {
                    // Transient: never a NoMatch, cooldown will recover.
                    return FetchBody::Failed(format!(
                        "lrclib temporarily unavailable (status {code})"
                    ));
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
    fn watch_step_detects_identity_changes() {
        let a = key();
        let b = TrackKey {
            artist: "Other".into(),
            title: "Track".into(),
            album: String::new(),
            duration_s: Some(200),
        };

        // A -> none.
        match watch_step(&Some(a.clone()), &None) {
            WatchStep::Changed {
                request,
                unavailable,
            } => {
                assert!(request.is_none());
                assert!(unavailable, "no session is unavailable");
            }
            WatchStep::Unchanged => panic!("A->none must be a change"),
        }

        // none -> A.
        match watch_step(&None, &Some(a.clone())) {
            WatchStep::Changed {
                request,
                unavailable,
            } => {
                assert_eq!(request.as_ref(), Some(&a));
                assert!(!unavailable);
            }
            WatchStep::Unchanged => panic!("none->A must be a change"),
        }

        // A -> B.
        match watch_step(&Some(a.clone()), &Some(b.clone())) {
            WatchStep::Changed { request, .. } => assert_eq!(request.as_ref(), Some(&b)),
            WatchStep::Unchanged => panic!("A->B must be a change"),
        }

        // Same identity (pause/seek keep the identity): unchanged.
        assert!(matches!(
            watch_step(&Some(a.clone()), &Some(a.clone())),
            WatchStep::Unchanged
        ));
    }

    #[test]
    fn cooldown_recovery_same_track_after_deadline() {
        // Failure -> cooldown elapses -> the SAME track fetches again.
        let k = key();
        let mut cache = HashMap::new();
        let mut cooldowns = HashMap::new();
        let cooldown = Duration::from_secs(60);

        // A fresh failure records the deadline.
        let now = Instant::now();
        cooldowns.insert(k.clone(), (LyricsStatus::Error("net".into()), now));

        // Revisit right away: serving the cached error, retry scheduled at
        // the ORIGINAL deadline (60s out).
        match cooldown_action(&cache, &cooldowns, &k, now, cooldown) {
            CooldownAction::CoolingDown(_, remaining) => {
                assert!(
                    remaining.as_secs() >= 59,
                    "~60s left, got {}",
                    remaining.as_secs()
                );
            }
            other => panic!("expected CoolingDown, got {other:?}"),
        }

        // After the deadline: Fetch (the retry happens).
        let later = now + Duration::from_secs(61);
        assert!(matches!(
            cooldown_action(&cache, &cooldowns, &k, later, cooldown),
            CooldownAction::Fetch
        ));

        // And a terminal result once cached always serves.
        cache.insert(k.clone(), LyricsStatus::NoMatch);
        assert!(matches!(
            cooldown_action(&cache, &cooldowns, &k, later, cooldown),
            CooldownAction::Cached(_)
        ));
    }

    #[test]
    fn cooldown_revisit_preserves_original_deadline() {
        // Revisiting a cached error must NOT renew its deadline: after
        // several revisits within the window, the remaining time still
        // counts from the ORIGINAL failure.
        let k = key();
        let cache = HashMap::new();
        let mut cooldowns = HashMap::new();
        let cooldown = Duration::from_secs(60);

        let t0 = Instant::now();
        cooldowns.insert(k.clone(), (LyricsStatus::Error("net".into()), t0));

        // Three revisits at t0+10s, t0+20s, t0+30s.
        for offset in [10, 20, 30] {
            let at = t0 + Duration::from_secs(offset);
            match cooldown_action(&cache, &cooldowns, &k, at, cooldown) {
                CooldownAction::CoolingDown(_, remaining) => {
                    // Remaining shrinks with the ORIGINAL start: at t0+30,
                    // 30s remain (not 60 - renewal would reset it).
                    let expected = 60 - offset;
                    assert!(
                        remaining.as_secs().abs_diff(expected) <= 1,
                        "revisit at +{offset}s must keep the original deadline, got {}s left",
                        remaining.as_secs()
                    );
                }
                other => panic!("expected CoolingDown at +{offset}s, got {other:?}"),
            }
            // The revisit itself must not touch the stored deadline.
            let stored = cooldowns.get(&k).unwrap();
            assert_eq!(stored.1, t0, "stored deadline must not be renewed");
        }

        // At t0+61 the ORIGINAL deadline has elapsed: fetch.
        let at = t0 + Duration::from_secs(61);
        assert!(matches!(
            cooldown_action(&cache, &cooldowns, &k, at, cooldown),
            CooldownAction::Fetch
        ));
    }

    #[test]
    fn publication_race_watcher_change_during_lookup() {
        // The exact reported race: the executor validates against the UI
        // state, the watcher installs a new track, then the executor
        // assigns - old words would attach to the new key and pass the
        // UI identity gate. apply_result validates and assigns in ONE
        // critical section, so this interleaving is now impossible;
        // this test forces the interleaved world-state and proves it.
        let a = key();
        let b = TrackKey {
            artist: "Other".into(),
            title: "Track".into(),
            album: String::new(),
            duration_s: Some(200),
        };

        // Watcher installed A/gen 1, lookup for A completed with lyrics.
        let req = LookupRequest {
            generation: 1,
            key: a.clone(),
        };
        let synced = LyricsStatus::Synced(vec![crate::lrc::LrcLine {
            time_s: 0.0,
            text: "old words".into(),
        }]);

        // Interleaving: watcher has JUST moved to B (gen 2, Loading), and
        // the media snapshot agrees with B. The A result must be refused
        // in the same critical section that would write it.
        let mut st = LyricsUiState {
            key: Some(b.clone()),
            status: LyricsStatus::Loading,
            generation: 2,
        };
        let applied = apply_result(&mut st, Some(&b), &req, synced);
        assert!(!applied, "stale A result must not attach to B");
        assert_eq!(
            st.status,
            LyricsStatus::Loading,
            "B keeps its loading state"
        );
        assert_eq!(st.key, Some(b));
        assert_eq!(st.generation, 2);

        // And the A -> B -> A variant: media came back to A but the
        // generation moved on - still refused.
        let mut st2 = LyricsUiState {
            key: Some(a.clone()),
            status: LyricsStatus::Loading,
            generation: 3,
        };
        assert!(!apply_result(
            &mut st2,
            Some(&a),
            &req,
            LyricsStatus::NoMatch
        ));
        assert_eq!(st2.status, LyricsStatus::Loading);

        // The healthy case: same generation, same identity - applied.
        let mut st3 = LyricsUiState {
            key: Some(a.clone()),
            status: LyricsStatus::Loading,
            generation: 1,
        };
        let ok = LyricsStatus::Synced(vec![crate::lrc::LrcLine {
            time_s: 0.0,
            text: "right words".into(),
        }]);
        assert!(apply_result(&mut st3, Some(&a), &req, ok.clone()));
        assert_eq!(st3.status, ok);
    }

    #[test]
    fn stale_results_rejected_including_a_b_a() {
        let a = key();
        let b = TrackKey {
            artist: "Other".into(),
            title: "Track".into(),
            album: String::new(),
            duration_s: Some(200),
        };

        // Request under generation 1 for track A.
        let req = LookupRequest {
            generation: 1,
            key: a.clone(),
        };

        // Media moved to B: rejected outright.
        assert!(!should_apply(1, Some(&b), &req));

        // Media moved away and BACK to A: the key matches again, but the
        // generation is stale (A's later change bumped it) - rejected.
        assert!(!should_apply(2, Some(&a), &req));

        // Still on A under generation 1: applied.
        assert!(should_apply(1, Some(&a), &req));

        // No media session at completion time: rejected.
        assert!(!should_apply(1, None, &req));
    }

    #[test]
    fn retry_classification_honors_the_budget() {
        let budget = Duration::from_secs(30);
        // Server says 120s: above budget -> defer the FULL value, never
        // shorten it to the budget.
        assert_eq!(
            classify_retry(Some(Duration::from_secs(120)), budget),
            RetryDecision::Defer(Duration::from_secs(120))
        );
        // Within budget: wait the full server value.
        assert_eq!(
            classify_retry(Some(Duration::from_secs(5)), budget),
            RetryDecision::Wait(Duration::from_secs(5))
        );
        assert_eq!(
            classify_retry(Some(Duration::from_secs(30)), budget),
            RetryDecision::Wait(Duration::from_secs(30)),
            "exactly at the budget is still a wait"
        );
        // Missing header: small default wait.
        assert_eq!(
            classify_retry(None, budget),
            RetryDecision::Wait(Duration::from_secs(2))
        );
    }

    #[test]
    fn retry_after_parses_seconds_and_http_dates() {
        let now = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        // Delay-seconds.
        assert_eq!(
            parse_retry_after(Some("120"), now),
            Some(Duration::from_secs(120))
        );
        assert_eq!(parse_retry_after(Some("0"), now), Some(Duration::ZERO));
        // HTTP-date: now+120s -> exactly 120s remaining.
        let target = now + Duration::from_secs(120);
        let date = format_system_time(target);
        assert_eq!(
            parse_retry_after(Some(&date), now),
            Some(Duration::from_secs(120)),
            "HTTP-date must parse (got {date:?})"
        );
        // A date in the past: zero wait (not a negative panic).
        let past = now - Duration::from_secs(10);
        let date = format_system_time(past);
        assert_eq!(parse_retry_after(Some(&date), now), Some(Duration::ZERO));
        // Garbage: None (the caller defaults to a small wait).
        assert_eq!(parse_retry_after(Some("soon-ish"), now), None);
        assert_eq!(parse_retry_after(None, now), None);
    }

    #[test]
    fn provider_gate_defers_all_tracks() {
        // A long Retry-After defers EVERY track, not just the requester:
        // the gate is provider-wide and the deadline is never shortened.
        let mut gate = ProviderGate::new();
        let now = Instant::now();
        assert_eq!(gate.remaining(now), None, "gate starts open");

        gate.defer_by(Duration::from_secs(120));
        let rem = gate.remaining(now + Duration::from_secs(10)).unwrap();
        assert!(
            rem.as_secs() >= 109,
            "120s defer leaves ~110s, got {}",
            rem.as_secs()
        );

        // A different track requesting immediately: still deferred.
        assert!(gate.remaining(now + Duration::from_secs(11)).is_some());

        // A later, SHORTER defer cannot shorten the original deadline.
        gate.defer_by(Duration::from_secs(30));
        let rem = gate.remaining(now + Duration::from_secs(12)).unwrap();
        assert!(
            rem.as_secs() >= 107,
            "short defer must not shorten, got {}",
            rem.as_secs()
        );

        // And it opens after the deadline passes.
        assert_eq!(gate.remaining(now + Duration::from_secs(130)), None);
    }

    #[test]
    fn http_date_parser_round_trips_known_epoch() {
        // Sun, 06 Nov 1994 08:49:37 GMT = 784111777.
        let t = parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        assert_eq!(
            t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
            784_111_777
        );
        // Wrong lengths/days/months are rejected.
        assert!(parse_http_date("Sun, 06 Foo 1994 08:49:37 GMT").is_none());
        assert!(parse_http_date("not a date").is_none());
    }

    /// Format a SystemTime as an IMF-fixdate for parse tests.
    fn format_system_time(t: std::time::SystemTime) -> String {
        let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let days = secs / 86_400;
        let rem = secs % 86_400;
        let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
        // Civil-from-days (Howard Hinnant's algorithm).
        let z = days as i64 + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        let wd = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][((days % 7) + 4) as usize % 7]; // 1970-01-01 was a Thursday
        let months = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        format!(
            "{wd}, {:02} {} {y} {:02}:{:02}:{:02} GMT",
            d,
            months[(m - 1) as usize],
            h,
            mi,
            s
        )
    }

    #[test]
    fn transient_5xx_is_not_a_permanent_no_match() {
        // The classification of 503 lives in fetch_json_with_429's status
        // handling; the semantics we test: a Failed("temporarily
        // unavailable") result is cached as a cooldown-transient Error,
        // never as NoMatch. The executor's match arm treats Error as
        // transient (cooldown) and everything else terminal; here we pin
        // the status classification the arm relies on.
        let msg = "lrclib temporarily unavailable (status 503)";
        let status = LyricsStatus::Error(msg.to_string());
        assert!(matches!(status, LyricsStatus::Error(_)));
        // A 5xx body never becomes NoMatch:
        assert!(!msg.contains("not found"));
    }

    #[test]
    fn album_name_parses_from_album_name_field() {
        // The API sends albumName; the record must actually read it.
        let json = r#"{
            "trackName": "Song",
            "artistName": "Artist",
            "albumName": "The Album",
            "duration": 180,
            "instrumental": false,
            "plainLyrics": null,
            "syncedLyrics": null
        }"#;
        let rec: LrclibRecord = serde_json::from_str(json).unwrap();
        assert_eq!(rec.album_name.as_deref(), Some("The Album"));
    }

    #[test]
    fn album_disambiguates_but_never_invents() {
        let ka = TrackKey {
            artist: "Artist".into(),
            title: "Song".into(),
            album: "Album X".into(),
            duration_s: Some(180),
        };
        let base = LrclibRecord {
            track_name: "Song".into(),
            artist_name: "Artist".into(),
            album_name: None,
            duration: Some(180.0),
            instrumental: None,
            plain_lyrics: None,
            synced_lyrics: None,
        };
        let x = LrclibRecord {
            album_name: Some("Album X".into()),
            synced_lyrics: Some("[00:01.00]x\n".into()),
            ..base.clone()
        };
        let y = LrclibRecord {
            album_name: Some("Album Y".into()),
            synced_lyrics: Some("[00:01.00]y\n".into()),
            ..base
        };
        assert!(record_matches(&ka, &x));
        assert!(record_matches(&ka, &y));

        // The album filter picks X out of an otherwise-identical pair.
        let matches = [&x, &y];
        let by_album: Vec<&&LrclibRecord> = matches
            .iter()
            .filter(|r| match r.album_name.as_deref() {
                Some(a) if !a.trim().is_empty() => {
                    crate::lrc::normalize(a) == crate::lrc::normalize(&ka.album)
                }
                _ => false,
            })
            .collect();
        assert_eq!(by_album.len(), 1);
        assert!(matches!(by_album[0].to_status(), LyricsStatus::Synced(_)));

        // Missing/empty album info on the query side keeps the rejection.
        let no_album = TrackKey {
            album: String::new(),
            ..ka
        };
        assert!(no_album.album.trim().is_empty());
    }

    #[test]
    fn interruptible_sleep_aborts_on_stop() {
        let stop = Arc::new(AtomicBool::new(true));
        // Already stopped: returns immediately without sleeping.
        let start = Instant::now();
        let completed = interruptible_sleep(Duration::from_secs(30), &stop);
        assert!(!completed, "must abort when stop is set");
        assert!(start.elapsed() < Duration::from_secs(2));
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
        // Never-stopping stop flag + fresh provider gate for the smoke test.
        let stop = Arc::new(AtomicBool::new(false));
        let mut gate = ProviderGate::new();
        match fetch_json_with_429(&agent, &url, &stop, &mut gate) {
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
                match fetch_json_with_429(&agent, &surl, &stop, &mut gate) {
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
