use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    div, img, linear_color_stop, linear_gradient, prelude::*, px, relative, rgb, rgba, size,
    Context, FocusHandle, Focusable, FontWeight, KeyDownEvent, MouseButton, ObjectFit, Render,
    RenderImage, Rgba, SharedString, Task, Window,
};
use luckymatch_core::{
    compute_song_end_trigger_sec, evaluate_lyrics, LyricsResult, LyricsSource, PlaybackClock,
    RecognizedTrack, ReleaseInfo, SyllableProgress,
};

use crate::audio_capture::CaptureSource;
use crate::backend::{
    decode_bgra_image, spawn_backend, BackendCommand, BackendEvent, BackendHandle, BackendStatus,
    UpdateState,
};
use crate::win32_window;

const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/branding/luckymatch-icon-1024.png");

pub struct LuckyMatchView {
    focus_handle: FocusHandle,
    backend: BackendHandle,
    _poll_task: Task<()>,
    app_icon: Option<Arc<RenderImage>>,

    source: CaptureSource,
    auto_sync: bool,
    always_on_top: bool,
    compact_mode: bool,

    status: BackendStatus,
    track: Option<RecognizedTrack>,
    clock: Option<PlaybackClock>,
    lyrics: Option<LyricsResult>,
    cover_image: Option<Arc<RenderImage>>,
    cover_url: Option<String>,
    accent_rgb: (u8, u8, u8),

    visual_focus: f32,
    beat_level: f32,
    anim_started_at: Instant,
    pending_seek_candidate: Option<(f64, Instant)>,
    last_frame_at: Instant,
    song_end_triggered: bool,

    update_state: UpdateState,
    update_dismissed: bool,
    pending_quit_for_update: bool,
}

impl LuckyMatchView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let backend = spawn_backend();

        // Lightweight poller wakes GPUI when background events arrive or while capturing.
        let poll_task = cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(60))
                .await;
            let keep_going = this
                .update(cx, |view, cx| {
                    let changed = view.drain_backend_events();
                    let triggered = view.check_song_finished();
                    if view.pending_quit_for_update {
                        view.shutdown();
                        cx.quit();
                        return;
                    }
                    let capturing = matches!(view.status, BackendStatus::Capturing { .. });
                    if changed || triggered || capturing {
                        cx.notify();
                    }
                })
                .is_ok();
            if !keep_going {
                break;
            }
        });

        Self {
            focus_handle: cx.focus_handle(),
            backend,
            _poll_task: poll_task,
            app_icon: decode_bgra_image(APP_ICON_PNG),
            source: CaptureSource::Loopback,
            auto_sync: true,
            always_on_top: false,
            compact_mode: false,
            status: BackendStatus::Idle,
            track: None,
            clock: None,
            lyrics: None,
            cover_image: None,
            cover_url: None,
            accent_rgb: (56, 189, 248),
            visual_focus: 0.0,
            beat_level: 0.0,
            anim_started_at: Instant::now(),
            pending_seek_candidate: None,
            last_frame_at: Instant::now(),
            song_end_triggered: false,
            update_state: UpdateState::Idle,
            update_dismissed: false,
            pending_quit_for_update: false,
        }
    }

    pub fn shutdown(&self) {
        let _ = self.backend.cmd_tx.send(BackendCommand::Shutdown);
    }

    fn drain_backend_events(&mut self) -> bool {
        let mut any = false;
        while let Ok(ev) = self.backend.event_rx.try_recv() {
            any = true;
            match ev {
                BackendEvent::StatusChanged(status) => {
                    self.status = status;
                }
                BackendEvent::TrackMatched {
                    track,
                    same_song,
                    is_manual,
                    force_resync,
                } => {
                    if same_song {
                        if let Some(existing_clock) = &mut self.clock {
                            let predicted = existing_clock.position_now();
                            let measured = track.clock.position_now();
                            let drift = (measured - predicted).abs();
                            if is_manual || force_resync {
                                existing_clock.resync(
                                    measured,
                                    track.recognition.timeskew,
                                    Instant::now(),
                                );
                                self.pending_seek_candidate = None;
                            } else if drift <= 0.6 {
                                self.pending_seek_candidate = None;
                            } else if drift <= 8.0 {
                                existing_clock.resync(
                                    measured,
                                    track.recognition.timeskew,
                                    Instant::now(),
                                );
                                self.pending_seek_candidate = None;
                            } else {
                                // Large jump (>8s) on auto-sync is often a chorus repeating near the end;
                                // require a second consecutive auto-check within 2.5s before accepting.
                                let confirmed = self
                                    .pending_seek_candidate
                                    .is_some_and(|(prev_pos, at)| {
                                        at.elapsed() < Duration::from_secs(60)
                                            && (prev_pos - measured).abs() <= 2.5
                                    });
                                if confirmed {
                                    existing_clock.resync(
                                        measured,
                                        track.recognition.timeskew,
                                        Instant::now(),
                                    );
                                    self.pending_seek_candidate = None;
                                } else {
                                    self.pending_seek_candidate = Some((measured, Instant::now()));
                                }
                            }
                        } else {
                            self.clock = Some(track.clock.clone());
                            self.pending_seek_candidate = None;
                        }
                        self.track = Some(track);
                        self.reset_song_end_if_before_trigger();
                    } else {
                        self.clock = Some(track.clock.clone());
                        self.track = Some(track);
                        self.lyrics = None;
                        self.cover_image = None;
                        self.cover_url = None;
                        self.visual_focus = 0.0;
                        self.pending_seek_candidate = None;
                        self.song_end_triggered = false;
                        self.maybe_defer_mid_song_reverify();
                    }
                }
                BackendEvent::LyricsUpdated(res) => {
                    let should_replace = self
                        .lyrics
                        .as_ref()
                        .is_none_or(|existing| res.quality_score() > existing.quality_score());
                    if should_replace {
                        self.lyrics = Some(res);
                        self.reset_song_end_if_before_trigger();
                    }
                }
                BackendEvent::CoverLoaded {
                    art_url,
                    image,
                    accent_rgb,
                } => {
                    let matches_current = self
                        .track
                        .as_ref()
                        .and_then(|t| t.recognition.album_art.as_deref())
                        == Some(art_url.as_str());
                    if matches_current {
                        self.cover_url = Some(art_url);
                        self.cover_image = Some(image);
                        self.accent_rgb = accent_rgb;
                    }
                }
                BackendEvent::UpdateStateChanged(state) => {
                    if matches!(state, UpdateState::Available(_)) {
                        self.update_dismissed = false;
                    }
                    self.update_state = state;
                }
                BackendEvent::QuitForUpdate => {
                    self.pending_quit_for_update = true;
                }
            }
        }
        any
    }

    fn check_for_updates(&mut self) {
        if matches!(
            self.update_state,
            UpdateState::Checking | UpdateState::Downloading { .. } | UpdateState::Launching { .. }
        ) {
            return;
        }
        if matches!(self.update_state, UpdateState::Available(_)) && self.update_dismissed {
            self.update_dismissed = false;
            return;
        }
        let _ = self
            .backend
            .cmd_tx
            .send(BackendCommand::CheckForUpdates { manual: true });
    }

    fn install_update(&mut self, release: ReleaseInfo) {
        self.update_dismissed = false;
        let _ = self
            .backend
            .cmd_tx
            .send(BackendCommand::InstallUpdate(release));
    }

    fn dismiss_update(&mut self) {
        self.update_dismissed = true;
        if matches!(
            self.update_state,
            UpdateState::Error(_) | UpdateState::UpToDate
        ) {
            self.update_state = UpdateState::Idle;
        }
    }

    fn trigger_identify(&self) {
        let is_busy = matches!(
            self.status,
            BackendStatus::Capturing { .. } | BackendStatus::Identifying
        );
        let cmd = if is_busy {
            BackendCommand::CancelIdentify
        } else {
            BackendCommand::IdentifyNow
        };
        let _ = self.backend.cmd_tx.send(cmd);
    }

    fn toggle_source(&mut self) {
        self.source = self.source.toggled();
        let _ = self
            .backend
            .cmd_tx
            .send(BackendCommand::SetSource(self.source));
    }

    fn known_duration_ms(&self) -> Option<u64> {
        self.track
            .as_ref()
            .and_then(|t| t.spotify.as_ref().map(|s| s.duration_ms))
            .filter(|&d| d > 0)
            .or_else(|| self.lyrics.as_ref().and_then(|l| l.duration_ms).filter(|&d| d > 0))
    }

    fn song_end_trigger_sec(&self) -> Option<f64> {
        self.track.as_ref()?;
        compute_song_end_trigger_sec(
            self.known_duration_ms(),
            self.lyrics.as_ref().map(|l| &l.document),
        )
    }

    fn maybe_defer_mid_song_reverify(&self) {
        if !self.auto_sync {
            return;
        }
        let Some(clock) = &self.clock else {
            return;
        };
        if clock.is_paused() {
            return;
        }
        let Some(trigger_sec) = self.song_end_trigger_sec() else {
            return;
        };
        if trigger_sec - clock.position_now() <= 63.0 {
            let _ = self.backend.cmd_tx.send(BackendCommand::DeferMidSongReverify);
        }
    }

    fn reset_song_end_if_before_trigger(&mut self) {
        if let Some(trigger) = self.song_end_trigger_sec() {
            if self
                .clock
                .as_ref()
                .is_some_and(|c| c.position_now() < trigger - 2.0)
            {
                self.song_end_triggered = false;
            }
        }
        self.maybe_defer_mid_song_reverify();
    }

    fn check_song_finished(&mut self) -> bool {
        if !self.auto_sync || self.song_end_triggered {
            return false;
        }
        let is_busy = matches!(
            self.status,
            BackendStatus::Capturing { .. } | BackendStatus::Identifying
        );
        if is_busy {
            return false;
        }
        let Some(clock) = &self.clock else {
            return false;
        };
        if clock.is_paused() {
            return false;
        }
        let Some(trigger_sec) = self.song_end_trigger_sec() else {
            return false;
        };
        let pos_now = clock.position_now();
        if pos_now < trigger_sec {
            return false;
        }

        self.song_end_triggered = true;
        let duration_sec = self.known_duration_ms().map(|d| d as f64 / 1000.0);
        let _ = self.backend.cmd_tx.send(BackendCommand::IdentifyNextSong {
            prev_pos_sec: pos_now,
            duration_sec,
        });
        true
    }

    fn toggle_auto_sync(&mut self) {
        self.auto_sync = !self.auto_sync;
        if !self.auto_sync {
            self.song_end_triggered = false;
        }
        let _ = self
            .backend
            .cmd_tx
            .send(BackendCommand::SetAutoSync(self.auto_sync));
        if self.auto_sync {
            self.maybe_defer_mid_song_reverify();
            self.check_song_finished();
        }
    }

    fn toggle_pin(&mut self, window: &mut Window) {
        self.always_on_top = !self.always_on_top;
        win32_window::set_always_on_top(window, self.always_on_top);
    }

    fn toggle_compact(&mut self, window: &mut Window) {
        self.compact_mode = !self.compact_mode;
        if self.compact_mode {
            window.resize(size(px(560.0), px(250.0)));
        } else {
            window.resize(size(px(560.0), px(720.0)));
        }
    }

    fn nudge_offset(&mut self, delta: f64) {
        if let Some(clock) = &mut self.clock {
            clock.nudge(delta);
        }
        self.reset_song_end_if_before_trigger();
    }

    fn reset_offset(&mut self) {
        if let Some(clock) = &mut self.clock {
            clock.set_manual_offset(0.0);
        }
        self.reset_song_end_if_before_trigger();
    }

    fn toggle_pause(&mut self) {
        let paused = if let Some(clock) = &mut self.clock {
            if clock.is_paused() {
                clock.resume();
            } else {
                clock.pause();
            }
            Some(clock.is_paused())
        } else {
            None
        };
        if let Some(is_paused) = paused {
            let _ = self
                .backend
                .cmd_tx
                .send(BackendCommand::SetPlaybackPaused(is_paused));
            if !is_paused {
                self.maybe_defer_mid_song_reverify();
            }
        }
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "r" => self.trigger_identify(),
            "[" => self.nudge_offset(-0.2),
            "]" => self.nudge_offset(0.2),
            "0" => self.reset_offset(),
            "space" => self.toggle_pause(),
            "t" => self.toggle_pin(window),
            "o" => self.toggle_compact(window),
            "m" => self.toggle_source(),
            "a" => self.toggle_auto_sync(),
            _ => return,
        }
        cx.notify();
    }

    fn accent_color(&self, alpha: f32) -> Rgba {
        let (r, g, b) = self.accent_rgb;
        Rgba {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: alpha,
        }
    }

    fn backdrop_top(&self) -> Rgba {
        let (r, g, b) = self.accent_rgb;
        let mix = 0.22f32;
        let base_r = 0x0d as f32 / 255.0;
        let base_g = 0x0f as f32 / 255.0;
        let base_b = 0x15 as f32 / 255.0;
        Rgba {
            r: base_r * (1.0 - mix) + (r as f32 / 255.0) * mix,
            g: base_g * (1.0 - mix) + (g as f32 / 255.0) * mix,
            b: base_b * (1.0 - mix) + (b as f32 / 255.0) * mix,
            a: 1.0,
        }
    }

    fn render_titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (status_label, status_color) = self.status_badge();
        let accent_bg = self.accent_color(0.28);
        let accent_border = self.accent_color(0.75);

        div()
            .h(px(38.0))
            .w_full()
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgba(0xffffff10))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        win32_window::start_window_drag(window);
                        cx.stop_propagation();
                    })
                    .when_some(self.app_icon.clone(), |d, icon| {
                        d.child(
                            img(icon)
                                .size(px(18.0))
                                .rounded(px(4.0))
                                .object_fit(ObjectFit::Cover),
                        )
                    })
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(0xffffff))
                            .child("LuckyMatch"),
                    )
                    .child(
                        div()
                            .px_2()
                            .py(px(2.0))
                            .rounded_full()
                            .bg(rgba(0xffffff10))
                            .text_xs()
                            .text_color(status_color)
                            .child(status_label),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .when(!self.compact_mode, |d| {
                        let (label, highlight) = match &self.update_state {
                            UpdateState::Checking => ("Checking…".to_string(), false),
                            UpdateState::UpToDate => ("Up to date".to_string(), false),
                            UpdateState::Available(_) => ("Update".to_string(), true),
                            UpdateState::Downloading { .. } => ("Updating…".to_string(), true),
                            UpdateState::Launching { .. } => ("Restarting…".to_string(), true),
                            UpdateState::Idle | UpdateState::Error(_) => {
                                (concat!("v", env!("CARGO_PKG_VERSION")).to_string(), false)
                            }
                        };
                        d.child(pill_button(
                            "btn-update-check",
                            label,
                            highlight,
                            accent_bg,
                            accent_border,
                            cx.listener(|this, _, _, cx| {
                                this.check_for_updates();
                                cx.notify();
                            }),
                        ))
                    })
                    .child(pill_button(
                        "toggle-compact",
                        if self.compact_mode { "Full" } else { "Overlay" },
                        self.compact_mode,
                        accent_bg,
                        accent_border,
                        cx.listener(|this, _, window, cx| {
                            this.toggle_compact(window);
                            cx.notify();
                        }),
                    ))
                    .child(pill_button(
                        "toggle-pin",
                        if self.always_on_top { "Pinned" } else { "Pin" },
                        self.always_on_top,
                        accent_bg,
                        accent_border,
                        cx.listener(|this, _, window, cx| {
                            this.toggle_pin(window);
                            cx.notify();
                        }),
                    ))
                    .child(
                        div()
                            .id("win-min")
                            .w(px(26.0))
                            .h(px(24.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_sm()
                            .text_color(rgba(0xffffff99))
                            .hover(|s| s.bg(rgba(0xffffff18)).text_color(rgb(0xffffff)))
                            .on_click(|_, window, _| window.minimize_window())
                            .child("–"),
                    )
                    .child(
                        div()
                            .id("win-close")
                            .w(px(26.0))
                            .h(px(24.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_sm()
                            .text_color(rgba(0xffffff99))
                            .hover(|s| s.bg(rgba(0xef4444cc)).text_color(rgb(0xffffff)))
                            .on_click(|_, window, _| window.remove_window())
                            .child("×"),
                    ),
            )
    }

    fn render_update_banner(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let accent_bg = self.accent_color(0.24);
        let accent_border = self.accent_color(0.75);

        match &self.update_state {
            UpdateState::Available(info) if !self.update_dismissed => {
                let release = info.clone();
                let ver = info.version.clone();
                div()
                    .w_full()
                    .px_4()
                    .py_2()
                    .bg(self.accent_color(0.14))
                    .border_b_1()
                    .border_color(self.accent_color(0.35))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(0xf8fafc))
                            .truncate()
                            .child(format!("Update available: v{ver}")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .id("btn-install-update")
                                    .px_2p5()
                                    .py(px(3.0))
                                    .rounded_md()
                                    .bg(self.accent_color(0.88))
                                    .hover(|s| s.opacity(0.9))
                                    .cursor_pointer()
                                    .text_xs()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(rgb(0x070a10))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.install_update(release.clone());
                                        cx.notify();
                                    }))
                                    .child("Update now"),
                            )
                            .child(
                                div()
                                    .id("btn-dismiss-update")
                                    .px_2()
                                    .py(px(3.0))
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(rgba(0xffffff99))
                                    .hover(|s| s.bg(rgba(0xffffff18)).text_color(rgb(0xffffff)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.dismiss_update();
                                        cx.notify();
                                    }))
                                    .child("×"),
                            ),
                    )
                    .into_any_element()
            }
            UpdateState::Downloading { version, progress } => {
                let pct = progress.map(|p| (p.clamp(0.0, 1.0) * 100.0).round() as u32);
                let text = match pct {
                    Some(p) => format!("Downloading v{version}… {p}%"),
                    None => format!("Downloading v{version}…"),
                };
                let bar_frac = progress.unwrap_or(0.35).clamp(0.0, 1.0);
                div()
                    .w_full()
                    .px_4()
                    .py_2()
                    .bg(self.accent_color(0.12))
                    .border_b_1()
                    .border_color(self.accent_color(0.30))
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(0xf8fafc))
                            .child(text),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(3.0))
                            .rounded_full()
                            .bg(rgba(0xffffff1a))
                            .overflow_hidden()
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(bar_frac))
                                    .bg(self.accent_color(0.95)),
                            ),
                    )
                    .into_any_element()
            }
            UpdateState::Launching { version } => div()
                .w_full()
                .px_4()
                .py_2()
                .bg(self.accent_color(0.16))
                .border_b_1()
                .border_color(self.accent_color(0.35))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(0xf8fafc))
                .child(format!(
                    "Installing v{version}… LuckyMatch will restart shortly"
                ))
                .into_any_element(),
            UpdateState::Error(msg) => div()
                .w_full()
                .px_4()
                .py_2()
                .bg(rgba(0xef444422))
                .border_b_1()
                .border_color(rgba(0xef444455))
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0xfca5a5))
                        .truncate()
                        .child(format!("Update failed: {}", truncate_str(msg, 46))),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(pill_button(
                            "btn-retry-update",
                            "Retry",
                            false,
                            accent_bg,
                            accent_border,
                            cx.listener(|this, _, _, cx| {
                                this.check_for_updates();
                                cx.notify();
                            }),
                        ))
                        .child(
                            div()
                                .id("btn-dismiss-update-err")
                                .px_2()
                                .py(px(3.0))
                                .rounded_sm()
                                .cursor_pointer()
                                .text_xs()
                                .text_color(rgba(0xffffff99))
                                .hover(|s| s.bg(rgba(0xffffff18)).text_color(rgb(0xffffff)))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.dismiss_update();
                                    cx.notify();
                                }))
                                .child("×"),
                        ),
                )
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }

    fn status_badge(&self) -> (SharedString, Rgba) {
        match &self.status {
            BackendStatus::Capturing {
                source,
                started_at,
                duration_sec,
            } => {
                let raw = started_at.elapsed().as_secs_f32();
                let elapsed = match duration_sec {
                    Some(d) => raw.min(*d),
                    None => raw,
                };
                (
                    format!("Listening ({}) {:.1}s", source.label(), elapsed).into(),
                    rgb(0x38bdf8),
                )
            }
            BackendStatus::Identifying => ("Identifying...".into(), rgb(0xfacc15)),
            BackendStatus::SilentAudio => ("Silence detected".into(), rgba(0xffffff88)),
            BackendStatus::NoMatch => ("No match yet".into(), rgba(0xfbbf24cc)),
            BackendStatus::Error(err) => (
                format!("Error: {}", truncate_str(err, 28)).into(),
                rgb(0xf87171),
            ),
            BackendStatus::Idle => match &self.clock {
                Some(c) if c.is_paused() => ("PAUSED".into(), rgb(0xfbbf24)),
                Some(_) => ("LIVE".into(), rgb(0x4ade80)),
                None => ("Ready".into(), rgba(0xffffff88)),
            },
        }
    }

    fn render_header(&self, pos_sec: f64) -> impl IntoElement {
        let Some(track) = &self.track else {
            return div().into_any_element();
        };

        let title = track.recognition.title.clone();
        let artist = track.recognition.artist.clone();
        let album = track.recognition.album.clone();
        let source_pill: SharedString = match &self.lyrics {
            Some(l) => format!("{} • {:?}", source_label(l.source), l.document.kind).into(),
            None => "Searching lyrics...".into(),
        };

        if self.compact_mode {
            return div()
                .w_full()
                .px_4()
                .pt_2()
                .pb_1()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .flex_1()
                        .child(self.render_cover_thumb(28.0))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(0xffffff))
                                .truncate()
                                .child(format!("{title} — {artist}")),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgba(0xffffff88))
                        .whitespace_nowrap()
                        .child(format_clock(pos_sec)),
                )
                .into_any_element();
        }

        div()
            .w_full()
            .px_5()
            .pt_4()
            .pb_3()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .border_b_1()
            .border_color(rgba(0xffffff0e))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .min_w_0()
                    .flex_1()
                    .child(self.render_cover_thumb(58.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(rgb(0xffffff))
                                    .truncate()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgba(0xffffffaa))
                                    .truncate()
                                    .child(match album {
                                        Some(alb) => format!("{artist} • {alb}"),
                                        None => artist,
                                    }),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .px_2()
                                            .py(px(2.0))
                                            .rounded_full()
                                            .bg(self.accent_color(0.22))
                                            .text_xs()
                                            .text_color(rgb(0xe2e8f0))
                                            .child(source_pill),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_end()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgba(0xffffffcc))
                            .child(format_clock(pos_sec)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgba(0xffffff55))
                            .child(format!("offset {:+.1}s", track.recognition.offset_sec)),
                    ),
            )
            .into_any_element()
    }

    fn render_cover_thumb(&self, size_px: f32) -> impl IntoElement {
        if let Some(cover) = &self.cover_image {
            return img(cover.clone())
                .size(px(size_px))
                .flex_none()
                .rounded_md()
                .object_fit(ObjectFit::Cover)
                .into_any_element();
        }

        div()
            .size(px(size_px))
            .flex_none()
            .rounded_md()
            .bg(self.accent_color(0.25))
            .border_1()
            .border_color(rgba(0xffffff1a))
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgba(0xffffffaa))
            .child("♪")
            .into_any_element()
    }

    fn render_lyrics_stage(&self, pos_sec: f64, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(track) = &self.track else {
            return self.render_welcome_state(cx).into_any_element();
        };
        let _ = track;

        let Some(res) = &self.lyrics else {
            return div()
                .flex_1()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgba(0xffffffcc))
                        .child("Fetching synchronized lyrics..."),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgba(0xffffff66))
                        .child("Checking Cache → Spicy Lyrics → AMLL → NetEase → LRCLIB"),
                )
                .into_any_element();
        };

        let hold_gap = self.clock.as_ref().map_or(2.5, |c| c.hold_gap_sec);
        let snap = evaluate_lyrics(&res.document, pos_sec, hold_gap);
        let target_focus = snap.focus_index.unwrap_or(0);

        let (prev_count, next_count) = if self.compact_mode { (0, 1) } else { (2, 3) };
        let start = target_focus.saturating_sub(prev_count);
        let end = (target_focus + next_count + 1).min(res.document.lines.len());

        // Subtle animated vertical slide offset around the focus index.
        let focus_delta = (target_focus as f32 - self.visual_focus).clamp(-1.5, 1.5);
        let slide_offset = px(focus_delta * 18.0);

        let mut list = div()
            .flex_1()
            .w_full()
            .min_w_0()
            .px_6()
            .py_3()
            .overflow_hidden()
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(if self.compact_mode { 10.0 } else { 16.0 }))
            .mt(slide_offset);

        if snap.in_instrumental_gap {
            list = list.child(render_instrumental_dots(pos_sec, self.accent_color(0.95)));
        }

        for idx in start..end {
            let line = &res.document.lines[idx];
            let is_active = snap.active_index == Some(idx) && !snap.in_instrumental_gap;
            let dist = idx.abs_diff(target_focus);

            if is_active {
                let mut active_box = div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .when(line.opposite_aligned && !self.compact_mode, |d| {
                        d.items_end()
                    });

                active_box = active_box.child(render_syllable_line(
                    &snap.lead_syllables,
                    if self.compact_mode { 22.0 } else { 27.0 },
                    FontWeight::BOLD,
                    rgb(0xffffff),
                    rgba(0xffffff4d),
                    line.opposite_aligned && !self.compact_mode,
                    true,
                ));

                for bg in &snap.background_lines {
                    active_box = active_box.child(render_syllable_line(
                        bg,
                        if self.compact_mode { 15.0 } else { 17.0 },
                        FontWeight::SEMIBOLD,
                        rgb(0x9be7ff),
                        rgba(0x9be7ff44),
                        line.opposite_aligned && !self.compact_mode,
                        false,
                    ));
                }

                list = list.child(active_box);
            } else {
                let (font_px, opacity) = match dist {
                    0 => (if self.compact_mode { 20.0 } else { 23.0 }, 0.72),
                    1 => (if self.compact_mode { 15.0 } else { 19.0 }, 0.44),
                    2 => (16.5, 0.24),
                    _ => (15.0, 0.13),
                };
                let past = snap.active_index.is_some_and(|a| idx < a);
                let final_alpha = if past { opacity * 0.72 } else { opacity };

                list = list.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .whitespace_normal()
                        .when(line.opposite_aligned && !self.compact_mode, |d| {
                            d.text_right()
                        })
                        .text_size(px(font_px))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(Rgba {
                            r: 1.0,
                            g: 1.0,
                            b: 1.0,
                            a: final_alpha,
                        })
                        .child(line.text.clone()),
                );
            }
        }

        list.into_any_element()
    }

    fn render_welcome_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_capturing = matches!(self.status, BackendStatus::Capturing { .. });
        let is_identifying = matches!(self.status, BackendStatus::Identifying);
        let is_busy = is_capturing || is_identifying;

        let pulse = if is_capturing {
            self.beat_level
        } else if is_identifying {
            (self.anim_started_at.elapsed().as_secs_f32() * 5.0).sin() * 0.5 + 0.5
        } else {
            0.0
        };

        let core_size = 76.0 + pulse * 14.0;
        let inner_ring = 96.0 + pulse * 26.0;
        let outer_ring = 114.0 + pulse * 44.0;

        div()
            .flex_1()
            .w_full()
            .px_6()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .id("welcome-music-orb")
                    .size(px(164.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .relative()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.trigger_identify();
                        cx.notify();
                    }))
                    .when(is_busy, |d| {
                        d.child(
                            div()
                                .size(px(outer_ring))
                                .rounded_full()
                                .bg(self.accent_color(0.08 + pulse * 0.14))
                                .border_1()
                                .border_color(self.accent_color(0.15 + pulse * 0.35)),
                        )
                        .child(
                            div()
                                .absolute()
                                .size(px(inner_ring))
                                .rounded_full()
                                .bg(self.accent_color(0.14 + pulse * 0.22)),
                        )
                    })
                    .child(
                        div()
                            .absolute()
                            .size(px(core_size))
                            .rounded_full()
                            .bg(self.accent_color(if is_busy {
                                0.32 + pulse * 0.45
                            } else {
                                0.22
                            }))
                            .border_1()
                            .border_color(self.accent_color(if is_busy {
                                0.70 + pulse * 0.30
                            } else {
                                0.65
                            }))
                            .hover(|s| s.bg(self.accent_color(0.35)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(28.0 + pulse * 6.0))
                            .text_color(rgb(0xffffff))
                            .child("♪"),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .text_center()
                    .whitespace_normal()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(0xffffff))
                    .child(if is_capturing {
                        "Listening to the beat..."
                    } else if is_identifying {
                        "Matching song..."
                    } else {
                        "Tap ♪ to identify & sync lyrics"
                    }),
            )
            .child(
                div()
                    .w_full()
                    .text_center()
                    .whitespace_normal()
                    .text_sm()
                    .text_color(rgba(0xffffff88))
                    .child(if is_busy {
                        "Tap ♪ to cancel".to_string()
                    } else {
                        format!(
                            "Source: {} • Auto-Sync {}",
                            self.source.label(),
                            if self.auto_sync { "ON" } else { "OFF" }
                        )
                    }),
            )
    }

    fn render_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let manual_offset = self
            .clock
            .as_ref()
            .map_or(0.0, |c| c.manual_offset_sec());
        let is_paused = self.clock.as_ref().is_some_and(|c| c.is_paused());
        let is_busy = matches!(
            self.status,
            BackendStatus::Capturing { .. } | BackendStatus::Identifying
        );

        let accent_bg = self.accent_color(0.26);
        let accent_border = self.accent_color(0.75);

        div()
            .w_full()
            .px_4()
            .py(px(if self.compact_mode { 8.0 } else { 12.0 }))
            .border_t_1()
            .border_color(rgba(0xffffff12))
            .bg(rgba(0x05070b99))
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("btn-resync")
                            .px_3()
                            .py(px(5.0))
                            .rounded_md()
                            .bg(self.accent_color(0.85))
                            .hover(|s| s.opacity(0.9))
                            .cursor_pointer()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(0x070a10))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.trigger_identify();
                                cx.notify();
                            }))
                            .child(if is_busy { "Cancel" } else { "Re-sync (R)" }),
                    )
                    .child(pill_button(
                        "btn-source",
                        self.source.label(),
                        false,
                        accent_bg,
                        accent_border,
                        cx.listener(|this, _, _, cx| {
                            this.toggle_source();
                            cx.notify();
                        }),
                    ))
                    .child(pill_button(
                        "btn-autosync",
                        if self.auto_sync { "Auto ON" } else { "Auto OFF" },
                        self.auto_sync,
                        accent_bg,
                        accent_border,
                        cx.listener(|this, _, _, cx| {
                            this.toggle_auto_sync();
                            cx.notify();
                        }),
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(pill_button(
                        "btn-nudge-minus",
                        "-0.2s",
                        false,
                        accent_bg,
                        accent_border,
                        cx.listener(|this, _, _, cx| {
                            this.nudge_offset(-0.2);
                            cx.notify();
                        }),
                    ))
                    .child(
                        div()
                            .id("btn-offset-reset")
                            .px_2()
                            .py(px(4.0))
                            .rounded_md()
                            .cursor_pointer()
                            .text_xs()
                            .text_color(rgba(0xffffffcc))
                            .hover(|s| s.bg(rgba(0xffffff14)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.reset_offset();
                                cx.notify();
                            }))
                            .child(format!("{manual_offset:+.1}s")),
                    )
                    .child(pill_button(
                        "btn-nudge-plus",
                        "+0.2s",
                        false,
                        accent_bg,
                        accent_border,
                        cx.listener(|this, _, _, cx| {
                            this.nudge_offset(0.2);
                            cx.notify();
                        }),
                    ))
                    .when(self.clock.is_some(), |d| {
                        d.child(pill_button(
                            "btn-pause",
                            if is_paused { "Resume" } else { "Pause" },
                            is_paused,
                            accent_bg,
                            accent_border,
                            cx.listener(|this, _, _, cx| {
                                this.toggle_pause();
                                cx.notify();
                            }),
                        ))
                    }),
            )
    }
}

impl Drop for LuckyMatchView {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Focusable for LuckyMatchView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for LuckyMatchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_backend_events();

        let now = Instant::now();
        let dt = now
            .saturating_duration_since(self.last_frame_at)
            .as_secs_f32()
            .min(0.1);
        self.last_frame_at = now;

        let raw_level = f32::from_bits(self.backend.live_level.load(Ordering::Relaxed));
        let level_alpha = 1.0 - (-18.0 * dt).exp();
        self.beat_level += (raw_level - self.beat_level) * level_alpha;

        let pos_sec = self.clock.as_ref().map_or(0.0, |c| c.position_now());

        if let (Some(res), Some(clock)) = (&self.lyrics, &self.clock) {
            let snap = evaluate_lyrics(&res.document, pos_sec, clock.hold_gap_sec);
            let target = snap.focus_index.unwrap_or(0) as f32;
            let alpha = 1.0 - (-10.0 * dt).exp();
            self.visual_focus += (target - self.visual_focus) * alpha;
        }

        let animate = self.clock.as_ref().is_some_and(|c| !c.is_paused())
            || matches!(
                self.status,
                BackendStatus::Capturing { .. } | BackendStatus::Identifying
            );
        if animate {
            window.request_animation_frame();
        }

        div()
            .id("luckymatch-root")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key_down))
            .size_full()
            .flex()
            .flex_col()
            .bg(linear_gradient(
                180.,
                linear_color_stop(self.backdrop_top(), 0.),
                linear_color_stop(rgb(0x080a0f), 1.),
            ))
            .text_color(rgb(0xffffff))
            .child(self.render_titlebar(cx))
            .child(self.render_update_banner(cx))
            .child(self.render_header(pos_sec))
            .child(self.render_lyrics_stage(pos_sec, cx))
            .child(self.render_controls(cx))
    }
}

fn pill_button(
    id: &'static str,
    label: impl Into<SharedString>,
    active: bool,
    accent_bg: Rgba,
    accent_border: Rgba,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_2p5()
        .py(px(4.0))
        .rounded_md()
        .border_1()
        .border_color(if active {
            accent_border
        } else {
            rgba(0xffffff1a)
        })
        .bg(if active {
            accent_bg
        } else {
            rgba(0xffffff0d)
        })
        .hover(|s| s.bg(rgba(0xffffff1f)))
        .cursor_pointer()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(0xf8fafc))
        .on_click(on_click)
        .child(label.into())
}

fn expand_for_wrapping(syllables: &[SyllableProgress]) -> Vec<Vec<SyllableProgress>> {
    let mut flat: Vec<SyllableProgress> = Vec::with_capacity(syllables.len());

    for syl in syllables {
        let words: Vec<&str> = syl.text.split_whitespace().collect();
        if words.len() <= 1 {
            flat.push(syl.clone());
            continue;
        }

        let total_chars: usize = words
            .iter()
            .map(|w| w.chars().count().max(1))
            .sum::<usize>()
            .max(1);
        let overall = syl.progress.clamp(0.0, 1.0);
        let mut offset_chars = 0usize;

        for (idx, word) in words.iter().enumerate() {
            let char_len = word.chars().count().max(1);
            let start_frac = offset_chars as f64 / total_chars as f64;
            let end_frac = (offset_chars + char_len) as f64 / total_chars as f64;
            let span = (end_frac - start_frac).max(1e-6);
            let word_progress = ((overall - start_frac) / span).clamp(0.0, 1.0);
            offset_chars += char_len;

            let is_last = idx + 1 == words.len();
            flat.push(SyllableProgress {
                text: (*word).to_string(),
                progress: word_progress,
                is_part_of_word: if is_last { syl.is_part_of_word } else { false },
            });
        }
    }

    // Group consecutive intra-word syllables so a multi-syllable word never splits across a wrap.
    let mut groups: Vec<Vec<SyllableProgress>> = Vec::new();
    let mut current: Vec<SyllableProgress> = Vec::new();
    for syl in flat {
        let ends_word = !syl.is_part_of_word;
        current.push(syl);
        if ends_word {
            groups.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        groups.push(current);
    }

    groups
}

fn render_syllable_line(
    syllables: &[SyllableProgress],
    font_px: f32,
    weight: FontWeight,
    lit_color: Rgba,
    unlit_color: Rgba,
    right_align: bool,
    lift_active: bool,
) -> impl IntoElement {
    let word_groups = expand_for_wrapping(syllables);
    let word_gap = (font_px * 0.28).round();

    let mut row = div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_baseline()
        .gap_y(px(2.0))
        .when(right_align, |d| d.justify_end());

    for (g_idx, group) in word_groups.iter().enumerate() {
        let mr_px = if g_idx + 1 < word_groups.len() {
            word_gap
        } else {
            0.0
        };

        let mut word_box = div()
            .flex()
            .flex_none()
            .items_baseline()
            .mr(px(mr_px));

        for syl in group {
            let pct = syl.progress.clamp(0.0, 1.0) as f32;
            let sweeping = lift_active && pct > 0.0 && pct < 1.0;

            let syl_box = div()
                .relative()
                .when(sweeping, |d| d.mt(px(-1.5)))
                .text_size(px(font_px))
                .font_weight(weight)
                .child(
                    div()
                        .whitespace_nowrap()
                        .text_color(unlit_color)
                        .child(syl.text.clone()),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .h_full()
                        .w(relative(pct))
                        .overflow_hidden()
                        .child(
                            div()
                                .whitespace_nowrap()
                                .text_color(lit_color)
                                .child(syl.text.clone()),
                        ),
                );

            word_box = word_box.child(syl_box);
        }

        row = row.child(word_box);
    }

    row
}

fn render_instrumental_dots(pos_sec: f64, accent: Rgba) -> impl IntoElement {
    let mut row = div().flex().items_center().gap_2().mb_1();
    for i in 0..3 {
        let phase = (pos_sec * 3.5 - i as f64 * 0.9).sin() as f32 * 0.5 + 0.5;
        let alpha = 0.25 + phase * 0.75;
        let dot_size = 7.0 + phase * 3.0;
        row = row.child(
            div()
                .size(px(dot_size))
                .rounded_full()
                .bg(Rgba { a: alpha, ..accent }),
        );
    }
    row
}

fn format_clock(sec: f64) -> String {
    let clamped = sec.max(0.0);
    let mins = (clamped / 60.0).floor() as u64;
    let rem = (clamped % 60.0).floor() as u64;
    format!("{mins:02}:{rem:02}")
}

fn source_label(src: LyricsSource) -> &'static str {
    match src {
        LyricsSource::Custom => "Custom",
        LyricsSource::Cache => "Cache",
        LyricsSource::SpicyLyrics => "Spicy Lyrics",
        LyricsSource::Amll => "AMLL TTML",
        LyricsSource::NetEase => "NetEase",
        LyricsSource::LrcLib => "LRCLIB",
    }
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_line_mode_into_wrappable_words_with_proportional_progress() {
        let line = vec![SyllableProgress {
            text: "Deep scars hunger".to_string(),
            progress: 0.5,
            is_part_of_word: false,
        }];
        let groups = expand_for_wrapping(&line);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0][0].text, "Deep");
        assert_eq!(groups[1][0].text, "scars");
        assert_eq!(groups[2][0].text, "hunger");
        assert!((groups[0][0].progress - 1.0).abs() < 1e-6);
        assert!(groups[1][0].progress > 0.0 && groups[1][0].progress < 1.0);
        assert!((groups[2][0].progress - 0.0).abs() < 1e-6);
    }

    #[test]
    fn keeps_multi_syllable_words_grouped_together() {
        let syls = vec![
            SyllableProgress {
                text: "Hel".to_string(),
                progress: 1.0,
                is_part_of_word: true,
            },
            SyllableProgress {
                text: "lo".to_string(),
                progress: 0.4,
                is_part_of_word: false,
            },
            SyllableProgress {
                text: "world".to_string(),
                progress: 0.0,
                is_part_of_word: false,
            },
        ];
        let groups = expand_for_wrapping(&syls);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 2);
        assert_eq!(groups[0][0].text, "Hel");
        assert_eq!(groups[0][1].text, "lo");
        assert_eq!(groups[1].len(), 1);
        assert_eq!(groups[1][0].text, "world");
    }
}
