pub mod cache;
pub mod lrc;
pub mod model;
pub mod normalize;
pub mod ttml;
pub mod waterfall;
pub mod yrc;

pub use model::{
    LyricsDocument, LyricsKind, LyricsLine, LyricsResult, LyricsSource, Syllable, VocalTrack,
};
pub use waterfall::{fetch_lyrics, fetch_lyrics_streaming, LyricsQuery};
