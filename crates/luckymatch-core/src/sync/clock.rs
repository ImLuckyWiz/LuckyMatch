use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::lyrics::model::{LyricsDocument, LyricsLine, VocalTrack};

pub const DEFAULT_HOLD_GAP_SEC: f64 = 2.5;
pub const SONG_END_DURATION_BUFFER_SEC: f64 = 1.5;
pub const SONG_END_LYRICS_BUFFER_SEC: f64 = 4.0;

/// Compute the playback position (in seconds) at which a song is considered finished.
pub fn compute_song_end_trigger_sec(
    duration_ms: Option<u64>,
    lyrics: Option<&LyricsDocument>,
) -> Option<f64> {
    let lyrics_end = lyrics.and_then(LyricsDocument::synced_end_time_sec);
    let track_dur = duration_ms.filter(|&d| d > 0).map(|d| d as f64 / 1000.0);

    match (track_dur, lyrics_end) {
        (Some(dur), Some(le)) => Some(dur.max(le) + SONG_END_DURATION_BUFFER_SEC),
        (Some(dur), None) => Some(dur + SONG_END_DURATION_BUFFER_SEC),
        (None, Some(le)) => Some(le + SONG_END_LYRICS_BUFFER_SEC),
        (None, None) => None,
    }
}

/// Tracks live playback position from an audio capture anchor + Shazam offset/timeskew.
#[derive(Debug, Clone)]
pub struct PlaybackClock {
    match_offset_sec: f64,
    timeskew: f64,
    capture_anchor: Instant,
    manual_offset_sec: f64,
    paused_at_pos_sec: Option<f64>,
    pub hold_gap_sec: f64,
}

impl PlaybackClock {
    pub fn new(match_offset_sec: f64, timeskew: f64, capture_anchor: Instant) -> Self {
        Self {
            match_offset_sec,
            timeskew,
            capture_anchor,
            manual_offset_sec: 0.0,
            paused_at_pos_sec: None,
            hold_gap_sec: DEFAULT_HOLD_GAP_SEC,
        }
    }

    pub fn position_now(&self) -> f64 {
        self.position_at(Instant::now())
    }

    pub fn position_at(&self, now: Instant) -> f64 {
        let base = match self.paused_at_pos_sec {
            Some(pos) => pos,
            None => {
                let elapsed = now
                    .saturating_duration_since(self.capture_anchor)
                    .as_secs_f64();
                self.match_offset_sec + elapsed * (1.0 + self.timeskew)
            }
        };
        (base + self.manual_offset_sec).max(0.0)
    }

    pub fn nudge(&mut self, delta_sec: f64) {
        self.manual_offset_sec += delta_sec;
    }

    pub fn set_manual_offset(&mut self, offset_sec: f64) {
        self.manual_offset_sec = offset_sec;
    }

    pub fn manual_offset_sec(&self) -> f64 {
        self.manual_offset_sec
    }

    pub fn is_paused(&self) -> bool {
        self.paused_at_pos_sec.is_some()
    }

    pub fn pause(&mut self) {
        self.pause_at(Instant::now());
    }

    pub fn pause_at(&mut self, now: Instant) {
        if self.paused_at_pos_sec.is_none() {
            let elapsed = now
                .saturating_duration_since(self.capture_anchor)
                .as_secs_f64();
            self.paused_at_pos_sec = Some(self.match_offset_sec + elapsed * (1.0 + self.timeskew));
        }
    }

    pub fn resume(&mut self) {
        self.resume_at(Instant::now());
    }

    pub fn resume_at(&mut self, now: Instant) {
        if let Some(pos) = self.paused_at_pos_sec.take() {
            self.match_offset_sec = pos;
            self.capture_anchor = now;
        }
    }

    /// Re-anchor the clock to a new recognition match while preserving `manual_offset_sec`.
    pub fn resync(&mut self, new_offset_sec: f64, new_timeskew: f64, new_anchor: Instant) {
        self.match_offset_sec = new_offset_sec;
        self.timeskew = new_timeskew;
        self.capture_anchor = new_anchor;
        self.paused_at_pos_sec = None;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyllableProgress {
    pub text: String,
    pub progress: f64,
    pub is_part_of_word: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncSnapshot {
    pub position_sec: f64,
    pub active_index: Option<usize>,
    pub focus_index: Option<usize>,
    pub in_instrumental_gap: bool,
    pub lead_syllables: Vec<SyllableProgress>,
    pub background_lines: Vec<Vec<SyllableProgress>>,
}

/// Evaluate active line, instrumental gap, and per-syllable fill progress at `pos_sec`.
pub fn evaluate_lyrics(
    doc: &LyricsDocument,
    pos_sec: f64,
    hold_gap_sec: f64,
) -> SyncSnapshot {
    if doc.lines.is_empty() {
        return SyncSnapshot {
            position_sec: pos_sec,
            active_index: None,
            focus_index: None,
            in_instrumental_gap: true,
            lead_syllables: Vec::new(),
            background_lines: Vec::new(),
        };
    }

    let mut active_index: Option<usize> = None;
    for (idx, line) in doc.lines.iter().enumerate() {
        if line.start_time <= pos_sec {
            active_index = Some(idx);
        } else {
            break;
        }
    }

    let in_instrumental_gap = match active_index {
        None => true,
        Some(idx) => pos_sec > effective_line_end(&doc.lines[idx]) + hold_gap_sec,
    };

    let focus_index = if in_instrumental_gap {
        match active_index {
            None => Some(0),
            Some(idx) => Some((idx + 1).min(doc.lines.len() - 1)),
        }
    } else {
        active_index
    };

    let (lead_syllables, background_lines) = if !in_instrumental_gap {
        if let Some(idx) = active_index {
            let line = &doc.lines[idx];
            (
                track_progress(&line.lead, pos_sec),
                line.background
                    .iter()
                    .map(|bg| track_progress(bg, pos_sec))
                    .collect(),
            )
        } else {
            (Vec::new(), Vec::new())
        }
    } else {
        (Vec::new(), Vec::new())
    };

    SyncSnapshot {
        position_sec: pos_sec,
        active_index,
        focus_index,
        in_instrumental_gap,
        lead_syllables,
        background_lines,
    }
}

fn effective_line_end(line: &LyricsLine) -> f64 {
    let mut end = line.end_time;
    for bg in &line.background {
        if bg.end_time.is_finite() {
            end = end.max(bg.end_time);
        }
    }
    end
}

fn track_progress(track: &VocalTrack, pos_sec: f64) -> Vec<SyllableProgress> {
    track
        .syllables
        .iter()
        .map(|s| {
            let dur = (s.end_time - s.start_time).max(0.001);
            let progress = ((pos_sec - s.start_time) / dur).clamp(0.0, 1.0);
            SyllableProgress {
                text: s.text.clone(),
                progress,
                is_part_of_word: s.is_part_of_word,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::model::{LyricsKind, Syllable};
    use std::time::Duration;

    #[test]
    fn clock_tracks_offset_timeskew_nudge_pause_and_resync() {
        let t0 = Instant::now();
        let mut clock = PlaybackClock::new(10.0, 0.01, t0);

        let pos = clock.position_at(t0 + Duration::from_secs(10));
        assert!((pos - 20.1).abs() < 1e-6);

        clock.nudge(0.2);
        assert!((clock.position_at(t0 + Duration::from_secs(10)) - 20.3).abs() < 1e-6);

        clock.pause_at(t0 + Duration::from_secs(10));
        assert!((clock.position_at(t0 + Duration::from_secs(20)) - 20.3).abs() < 1e-6);

        clock.resume_at(t0 + Duration::from_secs(20));
        assert!((clock.position_at(t0 + Duration::from_secs(25)) - 25.35).abs() < 1e-6);

        clock.resync(50.0, 0.0, t0 + Duration::from_secs(30));
        assert!((clock.position_at(t0 + Duration::from_secs(32)) - 52.2).abs() < 1e-6);
    }

    #[test]
    fn evaluates_syllable_progress_and_instrumental_gap() {
        let doc = LyricsDocument {
            kind: LyricsKind::Syllable,
            song_writers: vec![],
            lines: vec![
                LyricsLine {
                    start_time: 10.0,
                    end_time: 12.0,
                    opposite_aligned: false,
                    lead: VocalTrack {
                        start_time: 10.0,
                        end_time: 12.0,
                        syllables: vec![
                            Syllable {
                                text: "Hel".into(),
                                transliterated_text: None,
                                start_time: 10.0,
                                end_time: 11.0,
                                is_part_of_word: true,
                            },
                            Syllable {
                                text: "lo".into(),
                                transliterated_text: None,
                                start_time: 11.0,
                                end_time: 12.0,
                                is_part_of_word: false,
                            },
                        ],
                    },
                    background: vec![],
                    text: "Hello".into(),
                    transliterated_text: None,
                },
                LyricsLine {
                    start_time: 20.0,
                    end_time: 22.0,
                    opposite_aligned: false,
                    lead: VocalTrack {
                        start_time: 20.0,
                        end_time: 22.0,
                        syllables: vec![Syllable {
                            text: "Next".into(),
                            transliterated_text: None,
                            start_time: 20.0,
                            end_time: 22.0,
                            is_part_of_word: false,
                        }],
                    },
                    background: vec![],
                    text: "Next".into(),
                    transliterated_text: None,
                },
            ],
        };

        let mid = evaluate_lyrics(&doc, 10.5, DEFAULT_HOLD_GAP_SEC);
        assert_eq!(mid.active_index, Some(0));
        assert!(!mid.in_instrumental_gap);
        assert!((mid.lead_syllables[0].progress - 0.5).abs() < 1e-6);
        assert!((mid.lead_syllables[1].progress - 0.0).abs() < 1e-6);

        let gap = evaluate_lyrics(&doc, 15.0, DEFAULT_HOLD_GAP_SEC);
        assert!(gap.in_instrumental_gap);
        assert_eq!(gap.focus_index, Some(1));
    }

    #[test]
    fn computes_song_end_trigger_across_duration_and_lyrics_combinations() {
        let make_doc = |kind: LyricsKind, end_time: f64| LyricsDocument {
            kind,
            song_writers: vec![],
            lines: vec![LyricsLine {
                start_time: 10.0,
                end_time,
                opposite_aligned: false,
                lead: VocalTrack {
                    start_time: 10.0,
                    end_time,
                    syllables: vec![],
                },
                background: vec![],
                text: "End".into(),
                transliterated_text: None,
            }],
        };

        assert_eq!(compute_song_end_trigger_sec(Some(200_000), None), Some(201.5));
        let shorter_lyrics = make_doc(LyricsKind::Syllable, 190.0);
        assert_eq!(
            compute_song_end_trigger_sec(Some(200_000), Some(&shorter_lyrics)),
            Some(201.5)
        );
        let longer_lyrics = make_doc(LyricsKind::Line, 205.0);
        assert_eq!(
            compute_song_end_trigger_sec(Some(200_000), Some(&longer_lyrics)),
            Some(206.5)
        );
        assert_eq!(
            compute_song_end_trigger_sec(None, Some(&shorter_lyrics)),
            Some(194.0)
        );
        let static_lyrics = make_doc(LyricsKind::Static, 190.0);
        assert_eq!(compute_song_end_trigger_sec(None, Some(&static_lyrics)), None);
        assert_eq!(compute_song_end_trigger_sec(Some(0), None), None);
        assert_eq!(compute_song_end_trigger_sec(None, None), None);
    }
}
