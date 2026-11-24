mod audio_windows;
mod karaoke;
mod wav;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand, ValueEnum};
use luckymatch_core::{
    LuckyMatchConfig, LuckyMatchEngine, LyricsQuery, PlaybackClock, RecognizedTrack,
};

use crate::audio_windows::{capture_audio, CaptureSource};
use crate::karaoke::{run_karaoke, KaraokeExit, KaraokeMetadata};
use crate::wav::load_wav_16khz_mono;

#[derive(Debug, Parser)]
#[command(name = "luckymatch-cli", about = "Real-time song recognition & synced lyrics CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SourceArg {
    Loopback,
    Mic,
}

impl From<SourceArg> for CaptureSource {
    fn from(s: SourceArg) -> Self {
        match s {
            SourceArg::Loopback => CaptureSource::Loopback,
            SourceArg::Mic => CaptureSource::Mic,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Capture live audio from Windows WASAPI loopback or microphone, identify, and run karaoke.
    Listen {
        #[arg(long, value_enum, default_value_t = SourceArg::Loopback)]
        source: SourceArg,
        #[arg(long, default_value_t = 5.0)]
        seconds: f32,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        no_karaoke: bool,
    },
    /// Identify a WAV file and optionally launch terminal karaoke from its matched offset.
    File {
        path: PathBuf,
        #[arg(long, default_value_t = 0.0)]
        start_sec: f32,
        #[arg(long, default_value_t = 6.0)]
        seconds: f32,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        no_karaoke: bool,
    },
    /// Fetch and preview lyrics directly by metadata without audio capture.
    Lyrics {
        #[arg(long)]
        title: String,
        #[arg(long)]
        artist: String,
        #[arg(long)]
        spotify_id: Option<String>,
        #[arg(long)]
        isrc: Option<String>,
        #[arg(long)]
        duration_ms: Option<u64>,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        preview: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Listen {
            source,
            seconds,
            config,
            no_karaoke,
        } => cmd_listen(source.into(), seconds, config, no_karaoke).await,
        Command::File {
            path,
            start_sec,
            seconds,
            config,
            no_karaoke,
        } => cmd_file(path, start_sec, seconds, config, no_karaoke).await,
        Command::Lyrics {
            title,
            artist,
            spotify_id,
            isrc,
            duration_ms,
            config,
            preview,
        } => {
            cmd_lyrics(
                title,
                artist,
                spotify_id,
                isrc,
                duration_ms,
                config,
                preview,
            )
            .await
        }
    }
}

fn load_engine(config_path: Option<PathBuf>) -> Result<LuckyMatchEngine> {
    let mut cfg = LuckyMatchConfig::load(config_path.as_deref());
    if cfg.cache_dir.is_none() {
        cfg.cache_dir = Some(PathBuf::from("cache").join("lyrics"));
    }
    if cfg.custom_lyrics_dir.is_none() {
        cfg.custom_lyrics_dir = Some(PathBuf::from("lyrics_custom"));
    }
    Ok(LuckyMatchEngine::new(cfg)?)
}

fn print_recognized(track: &RecognizedTrack) {
    println!(
        "Matched: {} — {}{}",
        track.recognition.title,
        track.recognition.artist,
        track
            .recognition
            .album
            .as_deref()
            .map(|a| format!(" [{a}]"))
            .unwrap_or_default()
    );
    println!(
        "  Offset: {:.3}s | Timeskew: {:+.6} | ISRC: {} | Spotify ID: {}",
        track.recognition.offset_sec,
        track.recognition.timeskew,
        track.recognition.isrc.as_deref().unwrap_or("-"),
        track.recognition.spotify_track_id.as_deref().unwrap_or("-")
    );
}

async fn cmd_listen(
    source: CaptureSource,
    seconds: f32,
    config_path: Option<PathBuf>,
    no_karaoke: bool,
) -> Result<()> {
    let engine = load_engine(config_path)?;
    let mut preserved_manual_offset = 0.0f64;

    loop {
        println!("Capturing {seconds:.1}s from {source:?}...");
        let captured = tokio::task::spawn_blocking(move || capture_audio(source, seconds)).await??;
        println!(
            "Captured {} mono 16kHz samples (RMS {:.4}), identifying...",
            captured.samples_16khz_mono.len(),
            captured.rms
        );

        if captured.rms < 0.0005 {
            println!("Warning: captured audio looks nearly silent (RMS {:.5}).", captured.rms);
        }

        let Some(mut track) = engine
            .recognize_pcm_i16(
                &captured.samples_16khz_mono,
                luckymatch_core::audio::TARGET_SAMPLE_RATE,
                1,
                captured.capture_start,
            )
            .await?
        else {
            bail!("No song match found.");
        };

        track.clock.set_manual_offset(preserved_manual_offset);
        print_recognized(&track);

        let query = LyricsQuery {
            title: track.recognition.title.clone(),
            artist: track.recognition.artist.clone(),
            spotify_track_id: track.recognition.spotify_track_id.clone(),
            isrc: track.recognition.isrc.clone(),
            duration_ms: track.spotify.as_ref().map(|s| s.duration_ms),
        };

        if no_karaoke {
            match engine.fetch_lyrics(&query).await {
                Some(res) => {
                    println!(
                        "Lyrics ({:?}, {:?}) — {} lines:",
                        res.source,
                        res.document.kind,
                        res.document.lines.len()
                    );
                    for line in res.document.lines.iter().take(10) {
                        println!("  [{:6.2}s] {}", line.start_time, line.text);
                    }
                }
                None => println!("No synchronized lyrics found."),
            }
            return Ok(());
        }

        let rx = engine.fetch_lyrics_streaming(query);
        let meta = KaraokeMetadata {
            title: track.recognition.title,
            artist: track.recognition.artist,
            album: track.recognition.album,
            spotify_track_id: track.recognition.spotify_track_id,
        };

        match run_karaoke(meta, &mut track.clock, rx, None, true).await? {
            KaraokeExit::Quit => return Ok(()),
            KaraokeExit::ResyncRequested(manual_offset) => {
                preserved_manual_offset = manual_offset;
                continue;
            }
        }
    }
}

async fn cmd_file(
    path: PathBuf,
    start_sec: f32,
    seconds: f32,
    config_path: Option<PathBuf>,
    no_karaoke: bool,
) -> Result<()> {
    let engine = load_engine(config_path)?;
    let samples = load_wav_16khz_mono(&path, start_sec, seconds)?;
    let anchor = Instant::now();

    println!(
        "Loaded {} mono 16kHz samples from {} (offset {start_sec:.1}s), identifying...",
        samples.len(),
        path.display()
    );

    let Some(mut track) = engine
        .recognize_pcm_i16(
            &samples,
            luckymatch_core::audio::TARGET_SAMPLE_RATE,
            1,
            anchor,
        )
        .await?
    else {
        bail!("No song match found for {}", path.display());
    };

    print_recognized(&track);

    let query = LyricsQuery {
        title: track.recognition.title.clone(),
        artist: track.recognition.artist.clone(),
        spotify_track_id: track.recognition.spotify_track_id.clone(),
        isrc: track.recognition.isrc.clone(),
        duration_ms: track.spotify.as_ref().map(|s| s.duration_ms),
    };

    if no_karaoke {
        match engine.fetch_lyrics(&query).await {
            Some(res) => {
                println!(
                    "Lyrics ({:?}, {:?}) — {} lines:",
                    res.source,
                    res.document.kind,
                    res.document.lines.len()
                );
                for line in res.document.lines.iter().take(12) {
                    println!("  [{:6.2}s] {}", line.start_time, line.text);
                }
            }
            None => println!("No synchronized lyrics found."),
        }
        return Ok(());
    }

    let rx = engine.fetch_lyrics_streaming(query);
    let meta = KaraokeMetadata {
        title: track.recognition.title,
        artist: track.recognition.artist,
        album: track.recognition.album,
        spotify_track_id: track.recognition.spotify_track_id,
    };

    let _ = run_karaoke(meta, &mut track.clock, rx, None, false).await?;
    Ok(())
}

async fn cmd_lyrics(
    title: String,
    artist: String,
    spotify_id: Option<String>,
    isrc: Option<String>,
    duration_ms: Option<u64>,
    config_path: Option<PathBuf>,
    preview: bool,
) -> Result<()> {
    let engine = load_engine(config_path)?;
    let query = LyricsQuery {
        title: title.clone(),
        artist: artist.clone(),
        spotify_track_id: spotify_id.clone(),
        isrc,
        duration_ms,
    };

    if preview {
        let mut clock = PlaybackClock::new(0.0, 0.0, Instant::now());
        let rx = engine.fetch_lyrics_streaming(query);
        let meta = KaraokeMetadata {
            title,
            artist,
            album: None,
            spotify_track_id: spotify_id,
        };
        let _ = run_karaoke(meta, &mut clock, rx, None, false).await?;
        return Ok(());
    }

    match engine.fetch_lyrics(&query).await {
        Some(res) => {
            println!(
                "Found lyrics from {:?} ({:?}), {} lines:",
                res.source,
                res.document.kind,
                res.document.lines.len()
            );
            for line in res.document.lines.iter().take(15) {
                println!(
                    "  [{:6.2}s -> {:6.2}s] {} ({} syl)",
                    line.start_time,
                    line.end_time,
                    line.text,
                    line.lead.syllables.len()
                );
            }
        }
        None => println!("No lyrics found for {title} — {artist}."),
    }

    Ok(())
}
