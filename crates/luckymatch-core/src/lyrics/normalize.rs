use std::sync::LazyLock;

use regex::Regex;

static RE_DASH_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\s*-\s*((?:\d{4}\s+)?remaster(ed)?(\s*\d+)?|live|acoustic|mono|stereo|bonus track).*",
    )
    .expect("valid dash suffix regex")
});

static RE_BRACKET_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\s*[\(\[](feat\.|ft\.|with\b|remaster(ed)?(\s*\d+)?|official\b|audio\b|video\b)[\s\S]*?[\)\]]",
    )
    .expect("valid bracket suffix regex")
});

/// Validate and normalize a 12-character ISRC (`CCXXXNNNNNNN`).
pub fn valid_isrc(raw: &str) -> Option<String> {
    let code = raw.trim().to_ascii_uppercase();
    let b = code.as_bytes();
    let ok = b.len() == 12
        && b[..2].iter().all(u8::is_ascii_uppercase)
        && b[2..5]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && b[5..].iter().all(u8::is_ascii_digit);
    ok.then_some(code)
}

/// Strip remaster/live/featuring/official tags for lyrics search queries.
pub fn clean_title(title: &str) -> String {
    let step1 = RE_DASH_SUFFIX.replace_all(title, "");
    let step2 = RE_BRACKET_SUFFIX.replace_all(&step1, "");
    step2.trim().to_string()
}

/// Return the primary artist before separators (`,`, `&`, `/`, `feat.`, `ft.`).
pub fn primary_artist(artists: &str) -> String {
    let lower = artists.to_lowercase();
    let cut = [",", "&", "/", " feat", " ft.", " ft "]
        .iter()
        .filter_map(|sep| lower.find(sep))
        .min()
        .unwrap_or(artists.len());
    artists[..cut].trim().to_string()
}

/// Replace filesystem-unsafe characters with `_`.
pub fn sanitize_file_name(input: &str) -> String {
    input
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_titles_and_artists() {
        assert_eq!(clean_title("Good Times - 2018 Remaster"), "Good Times");
        assert_eq!(clean_title("Blinding Lights (Official Audio)"), "Blinding Lights");
        assert_eq!(primary_artist("Daft Punk, Pharrell Williams"), "Daft Punk");
        assert_eq!(primary_artist("Lil Nas X & Billy Ray Cyrus"), "Lil Nas X");
        assert_eq!(valid_isrc(" qm6mz1422245 "), Some("QM6MZ1422245".into()));
        assert_eq!(valid_isrc("invalid"), None);
    }
}
