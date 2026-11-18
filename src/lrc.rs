//! Timestamped LRC parsing for sidecar lyric files (pure, testable).
//!
//! Lyrics are LOCAL sidecar files: the same-basename `.lrc` next to the
//! audio file (song.mp3 -> song.lrc). Nothing is fetched, nothing is
//! guessed: plain line LRC carries LINE timing only - no word-level
//! onsets exist, so none are ever invented.

/// One synced lyric line.
#[derive(Clone, Debug, PartialEq)]
pub struct LrcLine {
    /// Start time in seconds.
    pub time_s: f64,
    pub text: String,
}

/// Parse a timestamp like `12:34.56`, `12:34.5`, `12:34` into seconds.
/// Malformed values are REJECTED (None), never parsed into something
/// that masquerades as synced: negative, NaN or infinite minutes or
/// seconds are all invalid (QA 1732 - bounded malformed-input
/// rejection, not a feature).
fn parse_timestamp(s: &str) -> Option<f64> {
    let mut parts = s.splitn(2, ':');
    let min: f64 = parts.next()?.trim().parse().ok()?;
    let rest = parts.next()?;
    let sec: f64 = rest.trim().parse().ok()?;
    if !min.is_finite() || min < 0.0 || !(0.0..60.0).contains(&sec) {
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
/// Returns None when the document has no timestamped lines at all (the
/// caller reports the sidecar as malformed honestly - never rendered
/// as if it were synced).
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
                        if v.is_finite() {
                            offset_ms = v;
                        }
                    }
                }
                // Other meta tags (ti:, ar:, al:, by:, ...) are ignored.
            }
            rest = &rest[close + 1..];
        }
        let text = rest.trim().to_string();
        for t in stamps {
            // The offset tag is parsed with the same honesty: a NaN or
            // infinite offset would poison every line's time; an
            // UNPARSEABLE offset tag is simply ignored (the line keeps
            // its raw stamp), while a parseable-but-non-finite one must
            // not masquerade as sync data.
            if t.is_finite() {
                let time = (t - offset_ms / 1000.0).max(0.0);
                lines.push(LrcLine {
                    time_s: time,
                    text: text.clone(),
                });
            }
        }
    }

    if lines.is_empty() {
        return None;
    }
    // total_cmp: the IEEE-754 total order - panic-free even if some
    // path ever feeds a non-finite time (defense in depth).
    lines.sort_by(|a, b| a.time_s.total_cmp(&b.time_s));
    // Deduplicate identical (time, text) pairs that multi-stamp lines can
    // produce with zero-length repeats.
    lines.dedup_by(|a, b| a.time_s == b.time_s && a.text == b.text);
    Some(lines)
}

/// The lyric line active at `position_s`: the last line at or before it.
pub fn line_at(lines: &[LrcLine], position_s: f64) -> Option<usize> {
    match lines.binary_search_by(|l| l.time_s.total_cmp(&position_s)) {
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

    // QA 1732: malformed timestamps must be REJECTED (skipped), never
    // parsed into a panic or a fake-synced line.
    #[test]
    fn malformed_timestamps_are_rejected_not_parsed() {
        // Negative minutes are invalid.
        assert!(parse_lrc("[-1:30.0]no\n[00:02.0]yes\n").unwrap().len() == 1);
        // NaN / inf minutes are invalid (the f64 parse succeeds, the
        // value is still rejected).
        assert!(parse_lrc("[NaN:30.0]no\n[00:02.0]yes\n").unwrap().len() == 1);
        assert!(parse_lrc("[inf:30.0]no\n[00:02.0]yes\n").unwrap().len() == 1);
        // Negative / NaN / infinite seconds are invalid.
        assert!(parse_lrc("[00:-1.0]no\n[00:02.0]yes\n").unwrap().len() == 1);
        assert!(parse_lrc("[00:NaN]no\n[00:02.0]yes\n").unwrap().len() == 1);
        assert!(parse_lrc("[00:inf]no\n[00:02.0]yes\n").unwrap().len() == 1);
        // A NaN offset tag is ignored (never poisons the stamps); the
        // valid lines stay synced at their raw stamps.
        let lrc = "[offset:NaN]\n[00:10.00]kept\n";
        let lines = parse_lrc(lrc).unwrap();
        assert_eq!(lines.len(), 1);
        assert!((lines[0].time_s - 10.0).abs() < 1e-9);
        // An infinite offset tag is ignored the same way.
        let lrc = "[offset:inf]\n[00:10.00]kept\n";
        let lines = parse_lrc(lrc).unwrap();
        assert_eq!(lines.len(), 1);
        assert!((lines[0].time_s - 10.0).abs() < 1e-9);
        // A document with ONLY malformed stamps is NOT synced (None).
        assert!(parse_lrc("[NaN:NaN]no\n[-5:-5]no\n").is_none());
        // No panic on any of it: the sort is total_cmp (IEEE-754 total
        // order) and rejects happen before any stamp is stored.
    }

    #[test]
    fn huge_finite_minutes_do_not_overflow_into_inf() {
        // 1e308 minutes * 60 = inf seconds: the stamp is dropped, the
        // valid line stays, no inf time masquerades as synced.
        let lines = parse_lrc("[1e308:00.0]no\n[00:02.0]yes\n").unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].time_s.is_finite());
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
}
