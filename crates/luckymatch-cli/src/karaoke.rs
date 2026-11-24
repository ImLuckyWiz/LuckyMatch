use std::io::{stdout, BufWriter, Write};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::queue;
use crossterm::style::{Attribute, Color, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, Clear, ClearType,
    EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
};
use luckymatch_core::{
    evaluate_lyrics, LyricsDocument, LyricsResult, LyricsSource, PlaybackClock, SyllableProgress,
};
use tokio::sync::mpsc;

pub struct KaraokeMetadata {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub spotify_track_id: Option<String>,
}

pub enum KaraokeExit {
    Quit,
    ResyncRequested(f64),
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = stdout();
        let _ = disable_raw_mode();
        let _ = execute!(out, Show, LeaveAlternateScreen, ResetColor);
    }
}

pub async fn run_karaoke(
    meta: KaraokeMetadata,
    clock: &mut PlaybackClock,
    mut lyrics_rx: mpsc::UnboundedReceiver<LyricsResult>,
    initial_doc: Option<LyricsResult>,
    allow_resync: bool,
) -> Result<KaraokeExit> {
    enable_raw_mode()?;
    let mut out = BufWriter::with_capacity(8192, stdout());
    execute!(out, EnterAlternateScreen, Hide)?;
    let _guard = TerminalGuard;

    let mut current: Option<LyricsResult> = initial_doc;
    let mut last_render = Instant::now() - Duration::from_secs(1);

    loop {
        while let Ok(incoming) = lyrics_rx.try_recv() {
            let should_replace = match &current {
                None => true,
                Some(existing) => {
                    incoming.document.kind == luckymatch_core::LyricsKind::Syllable
                        || existing.document.kind != luckymatch_core::LyricsKind::Syllable
                }
            };
            if should_replace {
                current = Some(incoming);
            }
        }

        while event::poll(Duration::from_millis(0))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(KaraokeExit::Quit),
                    KeyCode::Char('[') => clock.nudge(-0.2),
                    KeyCode::Char(']') => clock.nudge(0.2),
                    KeyCode::Char('0') => clock.set_manual_offset(0.0),
                    KeyCode::Char(' ') => {
                        if clock.is_paused() {
                            clock.resume();
                        } else {
                            clock.pause();
                        }
                    }
                    KeyCode::Char('r') if allow_resync => {
                        return Ok(KaraokeExit::ResyncRequested(clock.manual_offset_sec()));
                    }
                    _ => {}
                }
            }
        }

        if last_render.elapsed() >= Duration::from_millis(33) {
            render_frame(&mut out, &meta, clock, current.as_ref(), allow_resync)?;
            last_render = Instant::now();
        }

        tokio::time::sleep(Duration::from_millis(16)).await;
    }
}

fn render_frame<W: Write>(
    out: &mut W,
    meta: &KaraokeMetadata,
    clock: &PlaybackClock,
    lyrics: Option<&LyricsResult>,
    allow_resync: bool,
) -> Result<()> {
    let pos = clock.position_now();
    queue!(out, BeginSynchronizedUpdate, MoveTo(0, 0))?;

    // Header
    queue!(
        out,
        SetForegroundColor(Color::Cyan),
        SetAttribute(Attribute::Bold)
    )?;
    write!(out, "♪ {} — {}", meta.title, meta.artist)?;
    queue!(out, SetAttribute(Attribute::Reset), ResetColor)?;
    if let Some(album) = &meta.album {
        queue!(out, SetForegroundColor(Color::DarkGrey))?;
        write!(out, "  [{album}]")?;
        queue!(out, ResetColor)?;
    }
    queue!(out, Clear(ClearType::UntilNewLine))?;
    write!(out, "\r\n")?;

    let src_label = lyrics
        .map(|l| format!("{} ({:?})", source_name(l.source), l.document.kind))
        .unwrap_or_else(|| "Searching lyrics...".to_string());

    queue!(out, SetForegroundColor(Color::DarkGrey))?;
    write!(
        out,
        "Time: {}  |  Offset: {:+.1}s  |  {}  |  Source: {}",
        format_clock(pos),
        clock.manual_offset_sec(),
        if clock.is_paused() { "PAUSED" } else { "LIVE" },
        src_label
    )?;
    if let Some(id) = &meta.spotify_track_id {
        write!(out, "  |  Spotify: {id}")?;
    }
    queue!(out, ResetColor, Clear(ClearType::UntilNewLine))?;
    write!(out, "\r\n")?;
    queue!(out, Clear(ClearType::UntilNewLine))?;
    write!(out, "\r\n")?;

    match lyrics {
        None => {
            queue!(out, SetForegroundColor(Color::Yellow))?;
            write!(out, "  Waiting for lyrics waterfall...")?;
            queue!(out, ResetColor, Clear(ClearType::UntilNewLine))?;
            write!(out, "\r\n")?;
        }
        Some(res) => {
            render_lyrics_body(out, &res.document, pos, clock.hold_gap_sec)?;
        }
    }

    queue!(out, Clear(ClearType::UntilNewLine))?;
    write!(out, "\r\n")?;
    queue!(out, SetForegroundColor(Color::DarkGrey))?;
    if allow_resync {
        write!(
            out,
            "Controls: [ -0.2s  |  ] +0.2s  |  0 reset  |  Space pause  |  r re-sync  |  q quit"
        )?;
    } else {
        write!(
            out,
            "Controls: [ -0.2s  |  ] +0.2s  |  0 reset  |  Space pause  |  q quit"
        )?;
    }
    queue!(
        out,
        ResetColor,
        Clear(ClearType::UntilNewLine),
        Clear(ClearType::FromCursorDown),
        EndSynchronizedUpdate
    )?;
    out.flush()?;
    Ok(())
}

fn render_lyrics_body<W: Write>(
    out: &mut W,
    doc: &LyricsDocument,
    pos: f64,
    hold_gap_sec: f64,
) -> Result<()> {
    let snap = evaluate_lyrics(doc, pos, hold_gap_sec);
    let focus = snap.focus_index.unwrap_or(0);
    let start = focus.saturating_sub(2);
    let end = (focus + 4).min(doc.lines.len());

    for idx in start..end {
        let line = &doc.lines[idx];
        let is_active = snap.active_index == Some(idx) && !snap.in_instrumental_gap;

        if is_active {
            write!(out, "  ▶ ")?;
            render_syllable_row(out, &snap.lead_syllables, Color::Green, Color::DarkGrey)?;
            queue!(out, Clear(ClearType::UntilNewLine))?;
            write!(out, "\r\n")?;

            for bg in &snap.background_lines {
                write!(out, "      ")?;
                render_syllable_row(out, bg, Color::Cyan, Color::DarkGrey)?;
                queue!(out, Clear(ClearType::UntilNewLine))?;
                write!(out, "\r\n")?;
            }
        } else {
            let color = if snap.active_index.is_some_and(|a| idx <= a) {
                Color::DarkGrey
            } else if idx == focus && snap.in_instrumental_gap {
                Color::Yellow
            } else {
                Color::Grey
            };
            queue!(out, SetForegroundColor(color))?;
            write!(out, "    {}", line.text)?;
            queue!(out, ResetColor, Clear(ClearType::UntilNewLine))?;
            write!(out, "\r\n")?;
        }
    }

    if snap.in_instrumental_gap {
        queue!(out, Clear(ClearType::UntilNewLine))?;
        write!(out, "\r\n")?;
        queue!(out, SetForegroundColor(Color::DarkGrey))?;
        write!(out, "  ··· instrumental / waiting ···")?;
        queue!(out, ResetColor, Clear(ClearType::UntilNewLine))?;
        write!(out, "\r\n")?;
    }

    Ok(())
}

fn render_syllable_row<W: Write>(
    out: &mut W,
    syllables: &[SyllableProgress],
    lit_color: Color,
    unlit_color: Color,
) -> Result<()> {
    for (i, syl) in syllables.iter().enumerate() {
        let chars: Vec<char> = syl.text.chars().collect();
        let split_at = ((chars.len() as f64) * syl.progress).round() as usize;
        let split_at = split_at.min(chars.len());

        let lit: String = chars[..split_at].iter().collect();
        let unlit: String = chars[split_at..].iter().collect();

        if !lit.is_empty() {
            queue!(
                out,
                SetForegroundColor(lit_color),
                SetAttribute(Attribute::Bold)
            )?;
            write!(out, "{lit}")?;
            queue!(out, SetAttribute(Attribute::Reset))?;
        }
        if !unlit.is_empty() {
            queue!(out, SetForegroundColor(unlit_color))?;
            write!(out, "{unlit}")?;
        }
        queue!(out, ResetColor)?;

        if i + 1 < syllables.len() && !syl.is_part_of_word {
            write!(out, " ")?;
        }
    }
    Ok(())
}

fn format_clock(sec: f64) -> String {
    let clamped = sec.max(0.0);
    let mins = (clamped / 60.0).floor() as u64;
    let rem = clamped - (mins as f64) * 60.0;
    format!("{mins:02}:{rem:04.1}")
}

fn source_name(src: LyricsSource) -> &'static str {
    match src {
        LyricsSource::Custom => "Custom",
        LyricsSource::Cache => "Cache",
        LyricsSource::SpicyLyrics => "Spicy Lyrics",
        LyricsSource::Amll => "AMLL TTML",
        LyricsSource::NetEase => "NetEase",
        LyricsSource::LrcLib => "LRCLIB",
    }
}
