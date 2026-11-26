pub mod clock;

pub use clock::{
    compute_song_end_trigger_sec, evaluate_lyrics, PlaybackClock, SyllableProgress, SyncSnapshot,
    DEFAULT_HOLD_GAP_SEC, SONG_END_DURATION_BUFFER_SEC, SONG_END_LYRICS_BUFFER_SEC,
};
