use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::lyrics::lrc::parse_lrc;
use crate::lyrics::model::LyricsDocument;
use crate::lyrics::normalize::sanitize_file_name;
use crate::lyrics::ttml::parse_ttml;
use crate::lyrics::waterfall::LyricsQuery;
use crate::lyrics::yrc::parse_yrc;

pub fn cache_key_for_query(query: &LyricsQuery) -> String {
    if let Some(id) = query
        .spotify_track_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return sanitize_file_name(id);
    }
    if let Some(isrc) = query
        .isrc
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return format!("isrc_{}", sanitize_file_name(isrc));
    }
    sanitize_file_name(&format!("{} - {}", query.title, query.artist))
}

pub fn check_custom_lyrics(custom_dir: &Path, query: &LyricsQuery) -> Option<LyricsDocument> {
    if !custom_dir.exists() {
        return None;
    }

    let base_name = sanitize_file_name(&format!("{} - {}", query.title, query.artist));
    let mut candidates: Vec<PathBuf> = Vec::new();
    let exts = ["json", "ttml", "yrc", "lrc"];

    if let Some(track_id) = query
        .spotify_track_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        for ext in exts {
            candidates.push(custom_dir.join(format!("{track_id}.{ext}")));
        }
    }
    for ext in exts {
        candidates.push(custom_dir.join(format!("{base_name}.{ext}")));
    }

    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let Ok(raw_with_bom) = std::fs::read_to_string(&path) else {
            continue;
        };
        let raw = raw_with_bom.trim_start_matches('\u{FEFF}');
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();

        let parsed = match ext.as_str() {
            "json" => serde_json::from_str::<Value>(raw)
                .ok()
                .and_then(|v| LyricsDocument::from_spicy_or_cache_json(&v)),
            "ttml" => parse_ttml(raw),
            "yrc" => parse_yrc(raw),
            "lrc" => parse_lrc(raw, query.duration_ms),
            _ => None,
        };

        if let Some(doc) = parsed {
            if !doc.lines.is_empty() {
                return Some(doc);
            }
        }
    }

    None
}

pub fn load_cache(cache_dir: &Path, key: &str) -> Option<LyricsDocument> {
    if key.trim().is_empty() {
        return None;
    }
    let file = cache_dir.join(format!("{key}.json"));
    let raw = std::fs::read_to_string(file).ok()?;
    let clean = raw.trim_start_matches('\u{FEFF}');
    let val = serde_json::from_str::<Value>(clean).ok()?;
    LyricsDocument::from_spicy_or_cache_json(&val)
}

pub fn save_cache(cache_dir: &Path, key: &str, doc: &LyricsDocument) {
    if key.trim().is_empty() || doc.lines.is_empty() {
        return;
    }
    if std::fs::create_dir_all(cache_dir).is_err() {
        return;
    }
    let file = cache_dir.join(format!("{key}.json"));
    if let Ok(serialized) = serde_json::to_string_pretty(&doc.to_cache_json()) {
        let _ = std::fs::write(file, serialized);
    }
}
