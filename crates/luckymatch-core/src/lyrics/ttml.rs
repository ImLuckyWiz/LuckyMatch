use crate::lyrics::model::{
    join_syllables_text, LyricsDocument, LyricsKind, LyricsLine, Syllable, VocalTrack,
};

const TTM_NS: &str = "http://www.w3.org/ns/ttml#metadata";

/// Parse Apple Music / AMLL TTML lyrics into a unified `LyricsDocument`.
///
/// Supports syllable-level `<span>` timing, `ttm:role="x-bg"` background vocals,
/// `ttm:role="x-roman"` transliterations, `ttm:agent="v2"` opposite alignment,
/// and skips `ttm:role="x-translation"`.
pub fn parse_ttml(xml: &str) -> Option<LyricsDocument> {
    let trimmed = xml.trim();
    if trimmed.is_empty() {
        return None;
    }

    let doc = roxmltree::Document::parse(trimmed).ok()?;
    let mut lines = Vec::new();
    let mut has_any_syllables = false;

    for node in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "p") {
        let p_begin = attr_any(&node, "begin").and_then(parse_time_str);
        let p_end = attr_any(&node, "end").and_then(parse_time_str);
        let opposite_aligned = attr_ttm(&node, "agent").is_some_and(|a| a == "v2");

        let mut lead_syllables: Vec<Syllable> = Vec::new();
        let mut background: Vec<VocalTrack> = Vec::new();
        let mut plain_text_buf = String::new();
        let mut line_translit: Option<String> = None;

        for child in node.children() {
            if child.is_text() {
                let txt = child.text().unwrap_or("");
                if !txt.trim().is_empty() {
                    plain_text_buf.push_str(txt);
                } else if !txt.is_empty() && !lead_syllables.is_empty() {
                    // Intervening whitespace between sibling <span> elements means word break.
                    if let Some(last) = lead_syllables.last_mut() {
                        last.is_part_of_word = false;
                    }
                }
                continue;
            }

            if !child.is_element() {
                continue;
            }

            if child.tag_name().name() == "br" {
                if let Some(last) = lead_syllables.last_mut() {
                    last.is_part_of_word = false;
                }
                continue;
            }

            if child.tag_name().name() != "span" {
                continue;
            }

            let role = attr_ttm(&child, "role").unwrap_or("");
            match role {
                "x-translation" => continue,
                "x-roman" => {
                    let roman = collect_plain_text(child, false).trim().to_string();
                    if !roman.is_empty() {
                        if let Some(last) = lead_syllables.last_mut() {
                            if last.transliterated_text.is_none() {
                                last.transliterated_text = Some(roman.clone());
                            }
                        }
                        line_translit = Some(match line_translit {
                            Some(prev) => format!("{prev} {roman}"),
                            None => roman,
                        });
                    }
                    continue;
                }
                "x-bg" => {
                    let bg_begin = attr_any(&child, "begin").and_then(parse_time_str);
                    let bg_end = attr_any(&child, "end").and_then(parse_time_str);
                    let mut bg_syls = Vec::new();
                    collect_timed_spans(child, &mut bg_syls);
                    finalize_syllables(&mut bg_syls);

                    if !bg_syls.is_empty() {
                        let start_time = bg_begin.unwrap_or_else(|| bg_syls[0].start_time);
                        let end_time = bg_end
                            .unwrap_or_else(|| bg_syls.last().map_or(start_time, |s| s.end_time));
                        background.push(VocalTrack {
                            start_time,
                            end_time,
                            syllables: bg_syls,
                        });
                    }
                    continue;
                }
                _ => {}
            }

            // Regular lead span (or container span)
            let before_len = lead_syllables.len();
            collect_timed_spans(child, &mut lead_syllables);
            if lead_syllables.len() == before_len {
                let txt = collect_plain_text(child, true);
                if !txt.trim().is_empty() {
                    plain_text_buf.push_str(txt.trim());
                    plain_text_buf.push(' ');
                }
            }
        }

        if !lead_syllables.is_empty() {
            has_any_syllables = true;
            finalize_syllables(&mut lead_syllables);
            let start_time = p_begin.unwrap_or_else(|| lead_syllables[0].start_time);
            let mut end_time = p_end.unwrap_or_else(|| {
                lead_syllables
                    .last()
                    .map_or(start_time, |s| s.end_time)
            });
            for bg in &background {
                if bg.end_time.is_finite() {
                    end_time = end_time.max(bg.end_time);
                }
            }

            let text = join_syllables_text(&lead_syllables, false);
            let lead = VocalTrack {
                start_time: round_ms(start_time),
                end_time: round_ms(end_time),
                syllables: lead_syllables,
            };

            lines.push(LyricsLine {
                start_time: lead.start_time,
                end_time: round_ms(end_time),
                opposite_aligned,
                lead,
                background,
                text,
                transliterated_text: line_translit,
            });
        } else {
            let text = plain_text_buf.split_whitespace().collect::<Vec<_>>().join(" ");
            if !text.is_empty() {
                if let Some(start) = p_begin {
                    let start_time = round_ms(start);
                    let end_time = round_ms(p_end.unwrap_or(start + 5.0));
                    let lead = VocalTrack {
                        start_time,
                        end_time,
                        syllables: vec![Syllable {
                            text: text.clone(),
                            transliterated_text: line_translit.clone(),
                            start_time,
                            end_time,
                            is_part_of_word: false,
                        }],
                    };
                    lines.push(LyricsLine {
                        start_time,
                        end_time,
                        opposite_aligned,
                        lead,
                        background,
                        text,
                        transliterated_text: line_translit,
                    });
                }
            }
        }
    }

    if lines.is_empty() {
        return None;
    }

    lines.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));

    Some(LyricsDocument {
        kind: if has_any_syllables {
            LyricsKind::Syllable
        } else {
            LyricsKind::Line
        },
        lines,
        song_writers: Vec::new(),
    })
}

fn collect_timed_spans(node: roxmltree::Node<'_, '_>, out: &mut Vec<Syllable>) {
    let mut has_nested_timed_span = false;
    for child in node.children() {
        if child.is_element() && child.tag_name().name() == "span" {
            let role = attr_ttm(&child, "role").unwrap_or("");
            if role == "x-translation" || role == "x-roman" || role == "x-bg" {
                continue;
            }
            if attr_any(&child, "begin").is_some() {
                has_nested_timed_span = true;
                collect_timed_spans(child, out);
            }
        } else if child.is_text() {
            let txt = child.text().unwrap_or("");
            if !txt.is_empty() && txt.trim().is_empty() && !out.is_empty() {
                if let Some(last) = out.last_mut() {
                    last.is_part_of_word = false;
                }
            }
        }
    }

    if has_nested_timed_span {
        return;
    }

    let role = attr_ttm(&node, "role").unwrap_or("");
    if role == "x-translation" || role == "x-roman" {
        return;
    }

    let Some(begin) = attr_any(&node, "begin").and_then(parse_time_str) else {
        return;
    };
    let end = attr_any(&node, "end")
        .and_then(parse_time_str)
        .unwrap_or(begin);

    let raw = collect_plain_text(node, true);
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return;
    }

    let trailing_space = raw.chars().last().is_some_and(char::is_whitespace);

    out.push(Syllable {
        text: trimmed.to_string(),
        transliterated_text: None,
        start_time: round_ms(begin),
        end_time: round_ms(end),
        // Assume part of word unless trailing whitespace or whitespace text node follows.
        is_part_of_word: !trailing_space,
    });
}

fn finalize_syllables(syllables: &mut [Syllable]) {
    if let Some(last) = syllables.last_mut() {
        last.is_part_of_word = false;
    }
}

fn collect_plain_text(node: roxmltree::Node<'_, '_>, exclude_special_roles: bool) -> String {
    let mut buf = String::new();
    for desc in node.descendants() {
        if desc.is_text() {
            let skip = exclude_special_roles
                && desc.ancestors().any(|anc| {
                    anc.is_element()
                        && matches!(
                            attr_ttm(&anc, "role"),
                            Some("x-translation") | Some("x-roman")
                        )
                });
            if !skip {
                if let Some(t) = desc.text() {
                    buf.push_str(t);
                }
            }
        }
    }
    buf
}

fn attr_any<'a>(node: &roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attribute(name)
}

fn attr_ttm<'a>(node: &roxmltree::Node<'a, '_>, local: &str) -> Option<&'a str> {
    node.attribute((TTM_NS, local))
        .or_else(|| {
            let prefixed = format!("ttm:{local}");
            node.attributes()
                .find(|a| a.name() == local || a.name() == prefixed)
                .map(|a| a.value())
        })
}

/// Parse TTML timestamp strings (`hh:mm:ss.fff`, `mm:ss.fff`, `ss.fff`, `12.3s`, `1230ms`).
pub fn parse_time_str(raw: &str) -> Option<f64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(ms_str) = s.strip_suffix("ms") {
        return ms_str.trim().parse::<f64>().ok().map(|ms| ms / 1000.0);
    }
    if let Some(sec_str) = s.strip_suffix('s') {
        return sec_str.trim().parse::<f64>().ok();
    }

    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        1 => parts[0].parse::<f64>().ok(),
        2 => {
            let min = parts[0].parse::<f64>().ok()?;
            let sec = parts[1].parse::<f64>().ok()?;
            Some(min * 60.0 + sec)
        }
        3 => {
            let hrs = parts[0].parse::<f64>().ok()?;
            let min = parts[1].parse::<f64>().ok()?;
            let sec = parts[2].parse::<f64>().ok()?;
            Some(hrs * 3600.0 + min * 60.0 + sec)
        }
        _ => None,
    }
}

#[inline]
fn round_ms(val: f64) -> f64 {
    (val * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ttml_syllables_word_splits_and_background_vocals() {
        let xml = r#"
        <tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata">
          <body>
            <div>
              <p begin="00:10.000" end="00:13.500" ttm:agent="v2">
                <span begin="00:10.000" end="00:10.300">Hel</span><span begin="00:10.300" end="00:10.700">lo</span>
                <span begin="00:10.800" end="00:11.400">world</span>
                <span ttm:role="x-translation">Hola mundo</span>
                <span ttm:role="x-roman">Hello world</span>
                <span ttm:role="x-bg" begin="00:11.500" end="00:13.500">
                  <span begin="00:11.500" end="00:12.200">(oh</span>
                  <span begin="00:12.200" end="00:13.500">yeah)</span>
                </span>
              </p>
            </div>
          </body>
        </tt>
        "#;

        let doc = parse_ttml(xml).expect("TTML should parse");
        assert_eq!(doc.kind, LyricsKind::Syllable);
        assert_eq!(doc.lines.len(), 1);
        let line = &doc.lines[0];
        assert!(line.opposite_aligned);
        assert_eq!(line.text, "Hello world");
        assert_eq!(line.transliterated_text.as_deref(), Some("Hello world"));
        assert_eq!(line.lead.syllables.len(), 3);
        assert!(line.lead.syllables[0].is_part_of_word);
        assert!(!line.lead.syllables[1].is_part_of_word);
        assert!(!line.lead.syllables[2].is_part_of_word);
        assert_eq!(line.background.len(), 1);
        assert_eq!(line.background[0].syllables.len(), 2);
        assert_eq!(line.background[0].syllables[0].text, "(oh");
        assert!(!line.background[0].syllables[0].is_part_of_word);
    }
}
