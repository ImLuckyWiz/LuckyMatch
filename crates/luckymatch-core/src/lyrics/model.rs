use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LyricsKind {
    Syllable,
    Line,
    Static,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LyricsSource {
    Custom,
    Cache,
    SpicyLyrics,
    Amll,
    NetEase,
    LrcLib,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Syllable {
    pub text: String,
    pub transliterated_text: Option<String>,
    pub start_time: f64,
    pub end_time: f64,
    pub is_part_of_word: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VocalTrack {
    pub start_time: f64,
    pub end_time: f64,
    pub syllables: Vec<Syllable>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LyricsLine {
    pub start_time: f64,
    pub end_time: f64,
    pub opposite_aligned: bool,
    pub lead: VocalTrack,
    pub background: Vec<VocalTrack>,
    pub text: String,
    pub transliterated_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LyricsDocument {
    pub kind: LyricsKind,
    pub lines: Vec<LyricsLine>,
    pub song_writers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LyricsResult {
    pub document: LyricsDocument,
    pub source: LyricsSource,
}

impl LyricsResult {
    /// Rank lyrics quality so higher-accuracy syllable sources beat lower-tier fallbacks.
    pub fn quality_score(&self) -> u8 {
        match (self.document.kind, self.source) {
            (_, LyricsSource::Custom) => 100,
            (LyricsKind::Syllable, LyricsSource::SpicyLyrics) => 90,
            (LyricsKind::Syllable, LyricsSource::Amll) => 85,
            (LyricsKind::Syllable, LyricsSource::Cache) => 85,
            (LyricsKind::Syllable, LyricsSource::NetEase) => 75,
            (LyricsKind::Syllable, LyricsSource::LrcLib) => 70,
            (LyricsKind::Line, LyricsSource::SpicyLyrics) => 60,
            (LyricsKind::Line, LyricsSource::Amll) => 55,
            (LyricsKind::Line, LyricsSource::Cache) => 55,
            (LyricsKind::Line, LyricsSource::NetEase) => 50,
            (LyricsKind::Line, LyricsSource::LrcLib) => 45,
            (LyricsKind::Static, _) => 10,
        }
    }
}

impl LyricsDocument {
    /// Parse either Spicy Lyrics API `Body` JSON or `LuckySpotifyQueue`-compatible cached JSON.
    pub fn from_spicy_or_cache_json(value: &Value) -> Option<Self> {
        let type_str = value.get("Type").and_then(Value::as_str)?;
        let kind = match type_str {
            "Syllable" => LyricsKind::Syllable,
            "Line" => LyricsKind::Line,
            "Static" => LyricsKind::Static,
            _ => return None,
        };

        let song_writers = value
            .get("SongWriters")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let content = value.get("Content").and_then(Value::as_array)?;
        let mut lines = Vec::new();

        for item in content {
            // Spicy Lyrics may include `"Type": "Instrumental"` items; skip them cleanly.
            if item.get("Type").and_then(Value::as_str) == Some("Instrumental") {
                continue;
            }

            let opposite_aligned = item
                .get("OppositeAligned")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            match kind {
                LyricsKind::Syllable => {
                    let Some(lead_val) = item.get("Lead") else {
                        continue;
                    };
                    let Some(lead) = parse_vocal_track(lead_val) else {
                        continue;
                    };
                    let background: Vec<VocalTrack> = item
                        .get("Background")
                        .and_then(Value::as_array)
                        .map(|arr| arr.iter().filter_map(parse_vocal_track).collect())
                        .unwrap_or_default();

                    let mut end_time = lead.end_time;
                    for bg in &background {
                        if bg.end_time.is_finite() {
                            end_time = end_time.max(bg.end_time);
                        }
                    }

                    let text = join_syllables_text(&lead.syllables, false);
                    let transliterated_text = join_syllables_translit(&lead.syllables);

                    lines.push(LyricsLine {
                        start_time: lead.start_time,
                        end_time,
                        opposite_aligned,
                        lead,
                        background,
                        text,
                        transliterated_text,
                    });
                }
                LyricsKind::Line | LyricsKind::Static => {
                    let Some(text) = item
                        .get("Text")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                    else {
                        continue;
                    };
                    let start_time = item.get("StartTime").and_then(Value::as_f64).unwrap_or(0.0);
                    let end_time = item
                        .get("EndTime")
                        .and_then(Value::as_f64)
                        .unwrap_or(start_time + 5.0);
                    let transliterated_text = item
                        .get("TransliteratedText")
                        .and_then(Value::as_str)
                        .map(str::to_string);

                    let lead = VocalTrack {
                        start_time,
                        end_time,
                        syllables: vec![Syllable {
                            text: text.clone(),
                            transliterated_text: transliterated_text.clone(),
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
                        background: Vec::new(),
                        text,
                        transliterated_text,
                    });
                }
            }
        }

        if lines.is_empty() {
            return None;
        }
        if kind != LyricsKind::Static {
            lines.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
        }

        Some(Self {
            kind,
            lines,
            song_writers,
        })
    }

    /// Serialize to `LuckySpotifyQueue`-compatible cache JSON.
    pub fn to_cache_json(&self) -> Value {
        let type_str = match self.kind {
            LyricsKind::Syllable => "Syllable",
            LyricsKind::Line => "Line",
            LyricsKind::Static => "Static",
        };

        let content: Vec<Value> = match self.kind {
            LyricsKind::Syllable => self
                .lines
                .iter()
                .map(|line| {
                    let mut obj = serde_json::Map::new();
                    obj.insert("Type".to_string(), json!("Vocal"));
                    obj.insert(
                        "OppositeAligned".to_string(),
                        json!(line.opposite_aligned),
                    );
                    obj.insert("Lead".to_string(), vocal_track_to_json(&line.lead));
                    if !line.background.is_empty() {
                        obj.insert(
                            "Background".to_string(),
                            Value::Array(
                                line.background.iter().map(vocal_track_to_json).collect(),
                            ),
                        );
                    }
                    Value::Object(obj)
                })
                .collect(),
            LyricsKind::Line | LyricsKind::Static => self
                .lines
                .iter()
                .map(|line| {
                    let mut obj = serde_json::Map::new();
                    obj.insert("StartTime".to_string(), json!(line.start_time));
                    obj.insert("EndTime".to_string(), json!(line.end_time));
                    obj.insert("Text".to_string(), json!(line.text));
                    if let Some(tr) = &line.transliterated_text {
                        obj.insert("TransliteratedText".to_string(), json!(tr));
                    }
                    Value::Object(obj)
                })
                .collect(),
        };

        let mut root = serde_json::Map::new();
        root.insert("Type".to_string(), json!(type_str));
        if !self.song_writers.is_empty() {
            root.insert("SongWriters".to_string(), json!(self.song_writers));
        }
        root.insert("Content".to_string(), Value::Array(content));
        Value::Object(root)
    }
}

fn parse_vocal_track(val: &Value) -> Option<VocalTrack> {
    let syls_arr = val.get("Syllables")?.as_array()?;
    let mut syllables = Vec::with_capacity(syls_arr.len());
    for s in syls_arr {
        let text = s.get("Text").and_then(Value::as_str)?.trim().to_string();
        if text.is_empty() {
            continue;
        }
        let start_time = s.get("StartTime").and_then(Value::as_f64)?;
        let end_time = s.get("EndTime").and_then(Value::as_f64)?;
        if !start_time.is_finite() || !end_time.is_finite() {
            continue;
        }
        let is_part_of_word = s
            .get("IsPartOfWord")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let transliterated_text = s
            .get("TransliteratedText")
            .and_then(Value::as_str)
            .map(str::to_string);

        syllables.push(Syllable {
            text,
            transliterated_text,
            start_time,
            end_time,
            is_part_of_word,
        });
    }

    if syllables.is_empty() {
        return None;
    }

    let start_time = val
        .get("StartTime")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| syllables[0].start_time);
    let end_time = val
        .get("EndTime")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| syllables.last().map_or(start_time, |s| s.end_time));

    Some(VocalTrack {
        start_time,
        end_time,
        syllables,
    })
}

fn vocal_track_to_json(track: &VocalTrack) -> Value {
    let syllables: Vec<Value> = track
        .syllables
        .iter()
        .map(|s| {
            let mut obj = serde_json::Map::new();
            obj.insert("Text".to_string(), json!(s.text));
            obj.insert("StartTime".to_string(), json!(s.start_time));
            obj.insert("EndTime".to_string(), json!(s.end_time));
            obj.insert("IsPartOfWord".to_string(), json!(s.is_part_of_word));
            if let Some(tr) = &s.transliterated_text {
                obj.insert("TransliteratedText".to_string(), json!(tr));
            }
            Value::Object(obj)
        })
        .collect();

    json!({
        "StartTime": track.start_time,
        "EndTime": track.end_time,
        "Syllables": syllables,
    })
}

pub(crate) fn join_syllables_text(syllables: &[Syllable], _translit: bool) -> String {
    let mut out = String::new();
    for (i, syl) in syllables.iter().enumerate() {
        out.push_str(&syl.text);
        if i + 1 < syllables.len() && !syl.is_part_of_word {
            out.push(' ');
        }
    }
    out
}

fn join_syllables_translit(syllables: &[Syllable]) -> Option<String> {
    if !syllables.iter().any(|s| s.transliterated_text.is_some()) {
        return None;
    }
    let mut out = String::new();
    for (i, syl) in syllables.iter().enumerate() {
        let piece = syl
            .transliterated_text
            .as_deref()
            .unwrap_or(&syl.text);
        out.push_str(piece);
        if i + 1 < syllables.len() && !syl.is_part_of_word {
            out.push(' ');
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spicy_lyrics_json_with_background_and_instrumental() {
        let raw = json!({
            "Type": "Syllable",
            "SongWriters": ["Justin Timberlake", "Timbaland"],
            "StartTime": 15.877,
            "Content": [
                {
                    "Type": "Instrumental",
                    "StartTime": 0.0,
                    "EndTime": 15.0
                },
                {
                    "Type": "Vocal",
                    "OppositeAligned": false,
                    "Lead": {
                        "StartTime": 15.877,
                        "EndTime": 17.856,
                        "Syllables": [
                            { "Text": "I'm", "IsPartOfWord": false, "StartTime": 15.877, "EndTime": 16.103 },
                            { "Text": "back", "IsPartOfWord": false, "StartTime": 17.11, "EndTime": 17.856 }
                        ]
                    },
                    "Background": [
                        {
                            "StartTime": 17.866,
                            "EndTime": 18.485,
                            "Syllables": [
                                { "Text": "Yeah", "IsPartOfWord": false, "StartTime": 17.866, "EndTime": 18.485 }
                            ]
                        }
                    ]
                }
            ]
        });

        let doc = LyricsDocument::from_spicy_or_cache_json(&raw).expect("should parse");
        assert_eq!(doc.kind, LyricsKind::Syllable);
        assert_eq!(doc.song_writers.len(), 2);
        assert_eq!(doc.lines.len(), 1);
        assert_eq!(doc.lines[0].text, "I'm back");
        assert_eq!(doc.lines[0].background.len(), 1);
        assert!((doc.lines[0].end_time - 18.485).abs() < 1e-6);

        let roundtrip = LyricsDocument::from_spicy_or_cache_json(&doc.to_cache_json()).unwrap();
        assert_eq!(roundtrip.lines[0].text, "I'm back");
    }

    #[test]
    fn ranks_syllable_and_source_quality_correctly() {
        let make = |kind: LyricsKind, source: LyricsSource| LyricsResult {
            document: LyricsDocument {
                kind,
                lines: Vec::new(),
                song_writers: Vec::new(),
            },
            source,
        };
        let spicy = make(LyricsKind::Syllable, LyricsSource::SpicyLyrics);
        let amll = make(LyricsKind::Syllable, LyricsSource::Amll);
        let netease_syl = make(LyricsKind::Syllable, LyricsSource::NetEase);
        let netease_line = make(LyricsKind::Line, LyricsSource::NetEase);
        let lrclib = make(LyricsKind::Line, LyricsSource::LrcLib);

        assert!(spicy.quality_score() > amll.quality_score());
        assert!(amll.quality_score() > netease_syl.quality_score());
        assert!(netease_syl.quality_score() > netease_line.quality_score());
        assert!(netease_line.quality_score() > lrclib.quality_score());
    }
}
