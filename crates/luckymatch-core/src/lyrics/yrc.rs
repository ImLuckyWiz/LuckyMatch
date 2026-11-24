use std::sync::LazyLock;

use regex::Regex;

use crate::lyrics::model::{
    join_syllables_text, LyricsDocument, LyricsKind, LyricsLine, Syllable, VocalTrack,
};

static RE_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(\d+),(\d+)\]").expect("valid YRC header regex"));
static RE_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\((\d+),(\d+),\d+\)([^\(]*)").expect("valid YRC word regex"));
static RE_PUNCT_ONLY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\s,\.!?\)]+$").expect("valid YRC punct regex"));

/// Parse NetEase YRC syllable-level lyrics into a `LyricsDocument`.
pub fn parse_yrc(yrc_text: &str) -> Option<LyricsDocument> {
    if yrc_text.trim().is_empty() {
        return None;
    }

    let mut lines = Vec::new();

    for raw_line in yrc_text.lines() {
        let Some(header) = RE_HEADER.captures(raw_line) else {
            continue;
        };
        let line_start = header[1].parse::<f64>().ok()? / 1000.0;
        let line_dur = header[2].parse::<f64>().ok()? / 1000.0;
        let line_end = line_start + line_dur;

        let rest = &raw_line[header[0].len()..];
        let mut syllables: Vec<Syllable> = Vec::new();

        for cap in RE_WORD.captures_iter(rest) {
            let Ok(w_start_ms) = cap[1].parse::<f64>() else {
                continue;
            };
            let Ok(w_dur_ms) = cap[2].parse::<f64>() else {
                continue;
            };
            let w_start = w_start_ms / 1000.0;
            let w_dur = w_dur_ms / 1000.0;
            let raw_text = &cap[3];
            let has_leading_space = raw_text.starts_with(char::is_whitespace);
            let has_trailing_space = raw_text.ends_with(char::is_whitespace);
            let text = raw_text.trim();
            if text.is_empty() {
                if has_leading_space || has_trailing_space {
                    if let Some(prev) = syllables.last_mut() {
                        prev.is_part_of_word = false;
                    }
                }
                continue;
            }

            if has_leading_space {
                if let Some(prev) = syllables.last_mut() {
                    prev.is_part_of_word = false;
                }
            }

            if w_dur == 0.0 && !syllables.is_empty() && RE_PUNCT_ONLY.is_match(text) {
                if let Some(prev) = syllables.last_mut() {
                    prev.text.push_str(text);
                    if has_trailing_space {
                        prev.is_part_of_word = false;
                    }
                }
                continue;
            }

            syllables.push(Syllable {
                text: text.to_string(),
                transliterated_text: None,
                start_time: round_ms(w_start),
                end_time: round_ms(w_start + w_dur),
                is_part_of_word: !has_trailing_space,
            });
        }

        if syllables.is_empty() {
            continue;
        }
        if let Some(last) = syllables.last_mut() {
            last.is_part_of_word = false;
        }

        let start_time = round_ms(line_start);
        let end_time = round_ms(line_end);
        let text = join_syllables_text(&syllables, false);
        let lead = VocalTrack {
            start_time,
            end_time,
            syllables,
        };

        lines.push(LyricsLine {
            start_time,
            end_time,
            opposite_aligned: false,
            lead,
            background: Vec::new(),
            text,
            transliterated_text: None,
        });
    }

    if lines.is_empty() {
        return None;
    }

    lines.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));

    Some(LyricsDocument {
        kind: LyricsKind::Syllable,
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
    fn parses_yrc_syllables_and_zero_duration_punctuation() {
        let yrc = "[18310,2730](18310,450,0)Good (18760,540,0)times(19300,0,0), \n\
                   [21040,2910](21040,690,0)I'm(21730,420,0) going(22150,200,0) to(22350,300,0)night";
        let doc = parse_yrc(yrc).expect("YRC should parse");
        assert_eq!(doc.kind, LyricsKind::Syllable);
        assert_eq!(doc.lines.len(), 2);
        assert_eq!(doc.lines[0].lead.syllables.len(), 2);
        assert_eq!(doc.lines[0].lead.syllables[1].text, "times,");
        assert!(!doc.lines[0].lead.syllables[1].is_part_of_word);
        assert_eq!(doc.lines[0].text, "Good times,");
        assert_eq!(doc.lines[1].text, "I'm going tonight");
    }
}
