//! Timestamped LRC parsing and track-identity matching (pure, testable).
//!
//! LRCLIB (https://lrclib.net) is an anonymous, keyless lyrics API. The
//! content it returns is community-sourced; matching here is deliberately
//! conservative: we require artist AND title identity and (when both
//! durations are known) agreement within 2 seconds, and we reject ambiguity
//! rather than show lyrics for another track. Version markers ("remix",
//! "live", "feat.") are preserved verbatim - they are part of the identity.

/// One synced lyric line.
#[derive(Clone, Debug, PartialEq)]
pub struct LrcLine {
    /// Start time in seconds.
    pub time_s: f64,
    pub text: String,
}

/// Normalized comparison: lowercase + collapsed whitespace. Nothing is
/// stripped - version text like "(remix)" or "feat." stays part of the
/// identity, so a different version of the same song does not match.
pub fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Identity check for artist and title together.
pub fn identity_matches(
    query_artist: &str,
    query_title: &str,
    found_artist: &str,
    found_title: &str,
) -> bool {
    normalize(query_artist) == normalize(found_artist)
        && normalize(query_title) == normalize(found_title)
}

/// Duration safeguard: both known and within 2 seconds.
pub fn duration_matches(query: Option<f64>, found: Option<f64>) -> bool {
    match (query, found) {
        (Some(q), Some(f)) => (q - f).abs() <= 2.0,
        _ => true,
    }
}

/// Parse a timestamp like `12:34.56`, `12:34.5`, `12:34` into seconds.
fn parse_timestamp(s: &str) -> Option<f64> {
    let mut parts = s.splitn(2, ':');
    let min: f64 = parts.next()?.trim().parse().ok()?;
    let rest = parts.next()?;
    let sec: f64 = rest.trim().parse().ok()?;
    if !(0.0..60.0).contains(&sec) {
        return None;
    }
    Some(min * 60.0 + sec)
}

/// Parse a full LRC document into sorted lines. Handles:
/// - `[mm:ss.xx]text` and `[mm:ss]text`
/// - multiple timestamps per line (`[00:01.0][01:30.0]chorus`)
/// - the `[offset:+/-ms]` tag (positive offset shifts lyrics EARLIER,
///   per the common LRC convention `display = timestamp - offset`)
/// - malformed lines without a valid tag (skipped)
///
/// Returns None when the document has no timestamped lines at all (callers
/// treat that as unsynced plain text).
pub fn parse_lrc(input: &str) -> Option<Vec<LrcLine>> {
    let mut offset_ms = 0.0f64;
    let mut lines: Vec<LrcLine> = Vec::new();

    for raw in input.lines() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        // Collect all leading [..] tags.
        let mut rest = line;
        let mut stamps: Vec<f64> = Vec::new();
        while rest.starts_with('[') {
            let Some(close) = rest.find(']') else {
                break; // unterminated tag: treat the rest as text
            };
            let tag = &rest[1..close];
            if tag.contains(':') {
                if let Some(t) = parse_timestamp(tag) {
                    stamps.push(t);
                } else if let Some(v) = tag.strip_prefix("offset:") {
                    if let Ok(v) = v.trim().parse::<f64>() {
                        offset_ms = v;
                    }
                }
                // Other meta tags (ti:, ar:, al:, by:, ...) are ignored.
            }
            rest = &rest[close + 1..];
        }
        let text = rest.trim().to_string();
        for t in stamps {
            let time = (t - offset_ms / 1000.0).max(0.0);
            lines.push(LrcLine {
                time_s: time,
                text: text.clone(),
            });
        }
    }

    if lines.is_empty() {
        return None;
    }
    lines.sort_by(|a, b| a.time_s.partial_cmp(&b.time_s).unwrap());
    // Deduplicate identical (time, text) pairs that multi-stamp lines can
    // produce with zero-length repeats.
    lines.dedup_by(|a, b| a.time_s == b.time_s && a.text == b.text);
    Some(lines)
}

/// The lyric line active at `position_s`: the last line at or before it.
pub fn line_at(lines: &[LrcLine], position_s: f64) -> Option<usize> {
    match lines.binary_search_by(|l| l.time_s.partial_cmp(&position_s).unwrap()) {
        Ok(i) => Some(i),
        Err(0) => None,
        Err(i) => Some(i - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_lrc() {
        let lrc = "[00:01.00]First line\n[00:04.50]Second line\n";
        let lines = parse_lrc(lrc).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "First line");
        assert!((lines[0].time_s - 1.0).abs() < 1e-9);
        assert!((lines[1].time_s - 4.5).abs() < 1e-9);
    }

    #[test]
    fn parses_multiple_timestamps_and_sorts() {
        let lrc = "[01:30.0][00:02.0]chorus\n[00:00.5]intro\n";
        let lines = parse_lrc(lrc).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "intro");
        assert!((lines[0].time_s - 0.5).abs() < 1e-9);
        // The chorus appears at both times, sorted.
        assert_eq!(lines[1].text, "chorus");
        assert_eq!(lines[2].text, "chorus");
        assert!((lines[2].time_s - 90.0).abs() < 1e-9);
    }

    #[test]
    fn applies_offset_tag() {
        let lrc = "[offset:+500]\n[00:10.00]shifted\n";
        let lines = parse_lrc(lrc).unwrap();
        assert!(
            (lines[0].time_s - 9.5).abs() < 1e-9,
            "positive offset shifts earlier"
        );
    }

    #[test]
    fn handles_malformed_lines_without_poisoning() {
        let lrc = "no tags here\n[00:01]real line\n[12:99]bad minute:second is skipped\n[]\n[00:02.5]another\n";
        let lines = parse_lrc(lrc).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "real line");
    }

    #[test]
    fn plain_text_returns_none() {
        assert!(parse_lrc("just some\nplain words\nno timestamps\n").is_none());
        assert!(parse_lrc("").is_none());
    }

    #[test]
    fn crlf_and_blank_lines() {
        let lrc = "[00:01.00]a\r\n\r\n[00:02.00]b\r\n";
        let lines = parse_lrc(lrc).unwrap();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn line_at_finds_current() {
        let lines = parse_lrc("[00:01]a\n[00:05]b\n[00:10]c\n").unwrap();
        assert_eq!(line_at(&lines, 0.5), None, "before the first line");
        assert_eq!(line_at(&lines, 1.0), Some(0));
        assert_eq!(line_at(&lines, 4.9), Some(0));
        assert_eq!(line_at(&lines, 5.0), Some(1));
        assert_eq!(line_at(&lines, 999.0), Some(2), "holds the last line");
    }

    #[test]
    fn identity_matching_preserves_versions() {
        // Exact match.
        assert!(identity_matches("Artist", "Song", "artist", "SONG"));
        // Whitespace/case-insensitive.
        assert!(identity_matches(
            " The  Artist ",
            "Song",
            "the artist",
            "song"
        ));
        // A remix does NOT match the base title (and vice versa).
        assert!(!identity_matches(
            "Artist",
            "Song",
            "Artist",
            "Song (remix)"
        ));
        assert!(!identity_matches("Artist", "Song - Live", "Artist", "Song"));
        // Featuring artists stay part of the identity.
        assert!(!identity_matches("A feat. B", "Song", "A", "Song"));
        // Wrong artist.
        assert!(!identity_matches("Artist", "Song", "Other", "Song"));
    }

    #[test]
    fn duration_safeguard() {
        assert!(duration_matches(Some(180.0), Some(181.5)));
        assert!(duration_matches(Some(180.0), Some(178.0)));
        assert!(
            duration_matches(Some(180.0), Some(182.0)),
            "exactly 2s is within"
        );
        assert!(!duration_matches(Some(180.0), Some(182.1)));
        assert!(
            duration_matches(Some(180.0), None),
            "unknown duration passes"
        );
        assert!(duration_matches(None, None));
    }
}
