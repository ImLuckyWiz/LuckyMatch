use std::sync::LazyLock;

use regex::Regex;

use crate::lyrics::model::{LyricsDocument, LyricsKind, LyricsLine, Syllable, VocalTrack};

static RE_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[(\d{1,2}):(\d{2}(?:\.\d{1,3})?)\]").expect("valid LRC tag regex"));

/// Parse standard `[mm:ss.xx]` LRC lyrics (including multi-timestamp lines).
pub fn parse_lrc(lrc_text: &str, duration_ms: Option<u64>) -> Option<LyricsDocument> {
    if lrc_text.trim().is_empty() {
        return None;
    }

    let mut raw_items: Vec<(f64, String)> = Vec::new();

    for line in lrc_text.lines() {
        let mut tags = Vec::new();
        let mut last_end = 0usize;
        for cap in RE_TAG.captures_iter(line) {
            let m = cap.get(0).unwrap();
            // Require tags to be contiguous from the start of the line.
            if m.start() != last_end {
                break;
            }
            last_end = m.end();
            let Ok(min) = cap[1].parse::<f64>() else {
                continue;
            };
            let Ok(sec) = cap[2].parse::<f64>() else {
                continue;
            };
            tags.push(round_ms(min * 60.0 + sec));
        }

        if tags.is_empty() {
            continue;
        }
        let text = line[last_end..].trim();
        if text.is_empty() {
            continue;
        }
        for start in tags {
            raw_items.push((start, text.to_string()));
        }
    }

    if raw_items.is_empty() {
        return None;
    }

    raw_items.sort_by(|a, b| a.0.total_cmp(&b.0));

    let fallback_dur = duration_ms
        .filter(|&d| d > 0)
        .map(|d| d as f64 / 1000.0)
        .unwrap_or_else(|| raw_items.last().map_or(5.0, |(s, _)| s + 5.0));

    let mut lines = Vec::with_capacity(raw_items.len());
    for i in 0..raw_items.len() {
        let (start_time, ref text) = raw_items[i];
        let next_start = raw_items
            .get(i + 1)
            .map(|(s, _)| *s)
            .unwrap_or(fallback_dur);
        let end_time = next_start.min(round_ms(start_time + 7.0)).max(start_time);

        let lead = VocalTrack {
            start_time,
            end_time,
            syllables: vec![Syllable {
                text: text.clone(),
                transliterated_text: None,
                start_time,
                end_time,
                is_part_of_word: false,
            }],
        };

        lines.push(LyricsLine {
            start_time,
            end_time,
            opposite_aligned: false,
            lead,
            background: Vec::new(),
            text: text.clone(),
            transliterated_text: None,
        });
    }

    Some(LyricsDocument {
        kind: LyricsKind::Line,
        lines,
        song_writers: Vec::new(),
    })
}

#[inline]
fn round_ms(val: f64) -> f64 {
    (val * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc_including_multi_timestamps_and_caps_duration() {
        let lrc = "[ar:Artist]\n[00:10.50][00:40.00]Chorus line\n[00:13.00]Second line";
        let doc = parse_lrc(lrc, Some(60_000)).expect("LRC should parse");
        assert_eq!(doc.kind, LyricsKind::Line);
        assert_eq!(doc.lines.len(), 3);
        assert!((doc.lines[0].start_time - 10.5).abs() < 1e-6);
        assert!((doc.lines[0].end_time - 13.0).abs() < 1e-6);
        assert!((doc.lines[1].start_time - 13.0).abs() < 1e-6);
        assert!((doc.lines[1].end_time - 20.0).abs() < 1e-6); // capped at start + 7s
        assert!((doc.lines[2].start_time - 40.0).abs() < 1e-6);
        assert!((doc.lines[2].end_time - 47.0).abs() < 1e-6);
    }
}
