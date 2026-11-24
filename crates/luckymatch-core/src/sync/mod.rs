pub mod clock;

pub use clock::{
    evaluate_lyrics, PlaybackClock, SyllableProgress, SyncSnapshot, DEFAULT_HOLD_GAP_SEC,
};
