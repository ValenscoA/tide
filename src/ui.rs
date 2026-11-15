use std::sync::atomic::Ordering;

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap},
    Frame,
};
use ratatui_image::{FilterType, Resize, StatefulImage};

use crate::api::{HomeItems, TrackInfo};
use crate::app::{
    App, DetailContent, FocusPane, LayoutAreas, MainView, ModalState, SearchTab, SidebarSection,
};
use crate::audio::RepeatMode;
use crate::config::ThemeMode;
use crate::cover::render_wave_placeholder;
use crate::lyrics::{self, LyricsData, LyricsState};
use crate::theme::Theme;
use crate::utils::{fmt_bytes, fmt_time};

pub fn render(f: &mut Frame, app: &mut App) {
    app.areas = LayoutAreas::default();
    let theme = app.theme_mgr.active_theme();
    let root = f.area();

    if app.browser_open {
        render_browser_overlay(f, app, root, &theme);
    } else {
        let hide_center_graphics = app.modal != ModalState::None;
        render_now_playing_canvas(f, app, root, &theme, hide_center_graphics);
    }

    match app.modal {
        ModalState::Help => render_help_modal(f, root, &theme),
        ModalState::Auth => render_auth_modal(f, app, root, &theme),
        ModalState::None => {}
    }

    if let Some((ref msg, _)) = app.toast {
        render_toast(f, root, msg, active_accent_rgb(app, &theme));
    }
}

fn active_accent_rgb(app: &App, theme: &Theme) -> (u8, u8, u8) {
    match app.config.theme_mode {
        ThemeMode::AlbumArt => app.theme_mgr.visualizer_rgb(),
        _ => match theme.accent {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => app.theme_mgr.visualizer_rgb(),
        },
    }
}

fn border_style(theme: &Theme, focused: bool) -> Style {
    if focused {
        Style::default().fg(theme.border_focused)
    } else {
        Style::default().fg(theme.border)
    }
}

fn render_now_playing_canvas(
    f: &mut Frame,
    app: &mut App,
    root: Rect,
    theme: &Theme,
    hide_graphics: bool,
) {
    let (ar, ag, ab) = active_accent_rgb(app, theme);
    let accent = Color::Rgb(ar, ag, ab);
    let dim = Style::new().fg(Color::DarkGray);

    // Bottom-right hint or active background download status
    let dl_status = app.downloads.status();
    let hint_text = if let Some(ref dl) = dl_status {
        format!(" {} ", dl)
    } else {
        "Press ? for ctrl".to_string()
    };
    let hint_w = (hint_text.chars().count() as u16).min(root.width);
    let hint_y = if root.height >= 16 {
        root.y + root.height - 2
    } else {
        root.y + root.height.saturating_sub(1)
    };
    let hint_x = root.x + root.width.saturating_sub(hint_w + 2);
    if hint_w > 0 && hint_y < root.y + root.height {
        f.render_widget(
            Paragraph::new(Line::styled(
                hint_text,
                if dl_status.is_some() {
                    Style::new().fg(accent)
                } else {
                    dim
                },
            )),
            Rect::new(hint_x, hint_y, hint_w, 1),
        );
    }

    // Build centered lyrics (if visible & available)
    let elapsed_ms = app.audio.telemetry.position_ms.load(Ordering::Relaxed);
    let elapsed_s = elapsed_ms as f64 / 1000.0;
    let lyrics_lines = if app.show_lyrics {
        build_minimal_lyrics_lines(&app.lyrics_state, elapsed_s, app.lyrics_scroll, accent)
    } else {
        Vec::new()
    };

    // Dimensions matching Lumitide's compact centered Now Playing layout:
    // 22x11 album cover + 2-col gap + 46-col right pane (11 rows tall).
    let cover_w: u16 = 22;
    let cover_gap: u16 = 2;
    let cover_col_w: u16 = cover_w + cover_gap;
    let right_col_w: u16 = 46;
    let player_w: u16 = (cover_col_w + right_col_w).min(root.width.saturating_sub(4)).max(36);
    let main_h: u16 = 11;

    let lyrics_h = lyrics_lines.len() as u16;
    let show_lyrics = lyrics_h > 0 && root.height >= main_h + lyrics_h + 4;
    let extra_lyrics_h = if show_lyrics { 1 + lyrics_h } else { 0 };
    let block_h = main_h + extra_lyrics_h;

    let (top_area, lyrics_band) = if app.show_controls {
        let frame_w = (player_w + 6).max(74).min(root.width);
        let frame_h = (block_h + 2).min(root.height);
        let fx = root.x + root.width.saturating_sub(frame_w) / 2;
        let fy = root.y + root.height.saturating_sub(frame_h) / 2;
        let frame_area = Rect::new(fx, fy, frame_w, frame_h);
        app.areas.player_card = frame_area;

        let ctrl_top = " ←/p prev · Spc pause · →/n next · ↑↓ vol · / search · b browse · u queue ";
        let ctrl_bot = " m fav · d save · r radio · l lyrics · t theme · H all keys · q quit ";
        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(dim)
            .title_top(Line::styled(ctrl_top, dim).alignment(Alignment::Center))
            .title_bottom(Line::styled(ctrl_bot, dim).alignment(Alignment::Center));
        let inner = outer.inner(frame_area);
        f.render_widget(outer, frame_area);

        let content_y = inner.y + inner.height.saturating_sub(block_h) / 2;
        let top_x = inner.x + inner.width.saturating_sub(player_w) / 2;
        let top = Rect::new(top_x, content_y, player_w.min(inner.width), main_h.min(inner.height));
        let lyr = if show_lyrics {
            let ly = content_y + main_h + 1;
            Some(Rect::new(
                inner.x,
                ly,
                inner.width,
                lyrics_h.min((inner.y + inner.height).saturating_sub(ly)),
            ))
        } else {
            None
        };
        (top, lyr)
    } else {
        let cy = root.y + root.height.saturating_sub(block_h) / 2;
        let cx = root.x + root.width.saturating_sub(player_w) / 2;
        let top = Rect::new(cx, cy, player_w, main_h.min(root.height));
        app.areas.player_card = top;
        let lyr = if show_lyrics {
            let lyr_w = root.width.saturating_sub(4);
            let lx = root.x + 2;
            let ly = cy + main_h + 1;
            Some(Rect::new(
                lx,
                ly,
                lyr_w,
                lyrics_h.min((root.y + root.height).saturating_sub(ly)),
            ))
        } else {
            None
        };
        (top, lyr)
    };

    if hide_graphics {
        return;
    }

    let left_w = cover_col_w.min(top_area.width);
    let cols = Layout::horizontal([Constraint::Length(left_w), Constraint::Min(10)]).split(top_area);

    // ── Left: 22x11 Square Cover Art ─────────────────────────────────────────
    let art_w = cover_w.min(cols[0].width);
    let art_h = main_h.min(cols[0].height);
    let art_rect = Rect::new(cols[0].x, cols[0].y, art_w, art_h);
    app.areas.cover_thumb = art_rect;

    let mut rendered_cover = false;
    if let Some(ref track) = app.current_track {
        let cover_key = track
            .album_cover
            .clone()
            .or_else(|| track.local_path.as_ref().map(|p| format!("local:{}", p)));
        if let Some(key) = cover_key {
            if let Some(cached) = app.covers.get_mut(&key) {
                if app.config.image_cover {
                    let widget = StatefulImage::new().resize(Resize::Scale(Some(FilterType::Lanczos3)));
                    f.render_stateful_widget(widget, art_rect, &mut cached.protocol);
                } else {
                    f.render_widget(
                        Paragraph::new(Text::from(cached.player_braille.clone())),
                        art_rect,
                    );
                }
                rendered_cover = true;
            }
        }
    }
    if !rendered_cover {
        f.render_widget(
            Paragraph::new(Text::from(render_wave_placeholder(art_w as usize))),
            art_rect,
        );
    }

    // ── Right: Info Rows (0..5) + 10x5 Spectrum Bars (6..10) ────────────────
    let right_rows =
        Layout::vertical([Constraint::Length(6), Constraint::Length(5)]).split(cols[1]);

    let info_lines = if let Some(ref track) = app.current_track {
        let max_w = cols[1].width as usize;
        let is_fav = track.id > 0 && app.library.fav_track_ids.contains(&track.id);
        let q_str = if !app.queue.is_empty() {
            let pos = app.queue_idx.map(|i| i + 1).unwrap_or(1);
            format!("{} / {}", pos, app.queue.len())
        } else {
            String::new()
        };
        let mut q_spans = vec![Span::styled(q_str, dim)];
        if is_fav {
            q_spans.push(Span::styled("  ♥", Style::new().fg(accent)));
        }

        let dur_ms = app.audio.telemetry.duration_ms.load(Ordering::Relaxed);
        let total_s = if dur_ms > 0 {
            dur_ms as f64 / 1000.0
        } else {
            track.duration as f64
        };
        let is_playing = app.audio.telemetry.is_playing.load(Ordering::Relaxed);
        let is_paused = app.current_gen == 0 || app.audio.telemetry.is_paused.load(Ordering::Relaxed);
        let is_buffering = app.audio.telemetry.is_buffering.load(Ordering::Relaxed);
        let vol_pct = (app.config.volume * 100.0).round() as u32;

        let mut status_spans = vec![
            Span::styled(
                fmt_time(elapsed_s),
                Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" / {}", fmt_time(total_s)), dim),
            Span::styled(format!("  vol {}%", vol_pct), Style::new().fg(Color::White)),
        ];
        if is_buffering && app.current_gen > 0 {
            status_spans.push(Span::styled("  ⋯", dim));
        } else if is_paused || !is_playing {
            status_spans.push(Span::styled("  ⏸", Style::new().fg(Color::White)));
        }

        app.areas.btn_play = Rect::new(right_rows[0].x, right_rows[0].y + 4, right_rows[0].width, 1);

        vec![
            Line::styled(
                truncate_str(&track.title, max_w),
                Style::new().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Line::styled(truncate_str(&track.artist_name, max_w), dim),
            Line::styled(truncate_str(&track.album_name, max_w), dim),
            Line::from(q_spans),
            Line::from(status_spans),
            Line::raw(""),
        ]
    } else if app.session.is_none() {
        vec![
            Line::styled(
                "Tide",
                Style::new().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Line::styled("Not signed in", dim),
            Line::styled("Press Ctrl+L to connect Tidal", dim),
            Line::styled("or press b to browse Local Files", dim),
            Line::raw(""),
            Line::raw(""),
        ]
    } else {
        vec![
            Line::styled(
                "Tide",
                Style::new().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                if app.loading {
                    "Loading your Tidal library..."
                } else {
                    "Press / to search or b to browse"
                },
                dim,
            ),
            Line::raw(""),
            Line::raw(""),
            Line::raw(""),
            Line::raw(""),
        ]
    };

    f.render_widget(Paragraph::new(Text::from(info_lines)), right_rows[0]);

    // 10 flat-topped spectrum bars, 2 chars wide each with 1 space gap (29 chars wide, 5 rows tall)
    let is_playing = app.audio.telemetry.is_playing.load(Ordering::Relaxed);
    let is_paused = app.current_gen == 0 || app.audio.telemetry.is_paused.load(Ordering::Relaxed);
    let sr = app.audio.telemetry.sample_rate.load(Ordering::Relaxed);
    app.spectrum_bar.ensure_bars(10, sr);
    let spec_snap = app
        .audio
        .telemetry
        .spec_buf
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let norm = app
        .spectrum_bar
        .compute_and_update(&spec_snap, is_playing && !is_paused, app.config.calm_mode);
    let spec_lines = app.spectrum_bar.render_lines(&norm, 5, (ar, ag, ab), true);
    f.render_widget(Paragraph::new(Text::from(spec_lines)), right_rows[1]);

    // ── Centered Lyrics below Top Block ──────────────────────────────────────
    if let Some(area) = lyrics_band {
        f.render_widget(
            Paragraph::new(Text::from(lyrics_lines)).alignment(Alignment::Center),
            area,
        );
    }
}

fn build_minimal_lyrics_lines(
    lyrics: &LyricsState,
    elapsed: f64,
    plain_scroll: usize,
    accent: Color,
) -> Vec<Line<'static>> {
    let LyricsState::Loaded(data) = lyrics else {
        return Vec::new();
    };
    let active_style = Style::new().fg(Color::White).add_modifier(Modifier::BOLD);
    let near_style = Style::new().fg(Color::Gray);
    let far_style = Style::new().fg(Color::DarkGray);
    let note_style = Style::new().fg(accent);

    match data {
        LyricsData::Synced(lines) if !lines.is_empty() => {
            let active = lyrics::active_line_index(lines, elapsed);
            let cur = active.unwrap_or(0) as isize;
            let mut out: Vec<Line<'static>> = Vec::with_capacity(6);

            for offset in -2isize..=2isize {
                let idx = cur + offset;
                if idx < 0 || idx >= lines.len() as isize {
                    out.push(Line::raw(""));
                    continue;
                }
                let i = idx as usize;
                let style = match offset {
                    0 => {
                        if active.is_some() {
                            active_style
                        } else {
                            near_style
                        }
                    }
                    -1 | 1 => {
                        if active.is_some() {
                            near_style
                        } else {
                            far_style
                        }
                    }
                    _ => far_style,
                };
                let txt = if lines[i].text.trim().is_empty() {
                    "·".to_string()
                } else {
                    lines[i].text.clone()
                };
                out.push(Line::styled(txt, style));
            }
            out.push(Line::styled("♪", note_style));
            out
        }
        LyricsData::Plain(lines) if !lines.is_empty() => {
            let cur = plain_scroll.min(lines.len().saturating_sub(1)) as isize;
            let mut out: Vec<Line<'static>> = Vec::with_capacity(6);

            for offset in -2isize..=2isize {
                let idx = cur + offset;
                if idx < 0 || idx >= lines.len() as isize {
                    out.push(Line::raw(""));
                    continue;
                }
                let i = idx as usize;
                let style = match offset {
                    0 => active_style,
                    -1 | 1 => near_style,
                    _ => far_style,
                };
                out.push(Line::styled(lines[i].clone(), style));
            }
            out.push(Line::styled("♪", note_style));
            out
        }
        _ => Vec::new(),
    }
}

fn render_browser_overlay(f: &mut Frame, app: &mut App, root: Rect, theme: &Theme) {
    let pct_x = if root.width < 95 { 92 } else { 78 };
    let pct_y = if root.height < 28 { 90 } else { 78 };
    let area = centered_rect(pct_x, pct_y, root);
    app.areas.main_area = area;
    f.render_widget(Clear, area);

    let title = match &app.view {
        MainView::Home => " Home Feed ".to_string(),
        MainView::Search => {
            if app.search_results.query.is_empty() {
                " Search Tidal ".to_string()
            } else {
                format!(" Search: {} ", app.search_results.query)
            }
        }
        MainView::LikedTracks => format!(" Liked Tracks ({}) ", app.library.tracks.len()),
        MainView::SavedAlbums => format!(" Saved Albums ({}) ", app.library.albums.len()),
        MainView::Artists => format!(" Artists ({}) ", app.library.artists.len()),
        MainView::Mixes => format!(" My Mixes ({}) ", app.library.mixes.len()),
        MainView::Playlists => format!(" Playlists ({}) ", app.library.playlists.len()),
        MainView::Queue => format!(" Play Queue ({}) ", app.queue.len()),
        MainView::LocalFiles => format!(" Local Files ({}) ", app.local_tracks.len()),
        MainView::Settings => " Settings & Audio ".to_string(),
        MainView::Detail(d) => match d {
            DetailContent::Album { album, .. } => format!(" Album: {} ", album.title),
            DetailContent::Playlist { playlist, .. } => format!(" Playlist: {} ", playlist.title),
            DetailContent::Mix { mix, .. } => format!(" Mix: {} ", mix.title),
            DetailContent::Radio { seed, .. } => format!(" Radio: {} ", seed.title),
            DetailContent::Artist(ad) => format!(" Artist: {} ", ad.artist.name),
        },
    };

    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(theme, app.focus == FocusPane::Main))
        .title_top(
            Line::from(title)
                .style(Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
        )
        .title_bottom(
            Line::from(" Enter play · a queue · i next · f filter · Tab switch · b/Esc close ")
                .style(Style::default().fg(Color::DarkGray)),
        );
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let show_search_row = matches!(app.view, MainView::Search) || app.focus == FocusPane::SearchBar;
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(if show_search_row { 3 } else { 1 }),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .split(inner);

    // Compact Top Navigation Tabs + Account/Quality badge
    let mut tab_spans = vec![Span::raw(" ")];
    let mut cur_x = rows[0].x + 1;
    for (idx, sec) in SidebarSection::ALL.iter().enumerate() {
        let is_active = app.sidebar_idx == idx && !matches!(app.view, MainView::Detail(_));
        let txt = format!(" {} ", sec.label());
        let w = txt.chars().count() as u16;
        let style = if is_active {
            Style::default()
                .fg(Color::Black)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        if cur_x + w <= rows[0].x + rows[0].width.saturating_sub(14) {
            app.areas
                .browser_tabs
                .push((Rect::new(cur_x, rows[0].y, w, 1), idx));
            tab_spans.push(Span::styled(txt, style));
            tab_spans.push(Span::raw(" "));
            cur_x += w + 1;
        }
    }
    let badge = if let Some(ref sess) = app.session {
        format!(
            "[{}] ● {} ",
            app.config.audio_quality.short_label(),
            sess.country_code
        )
    } else {
        "Ctrl+L Sign In ".to_string()
    };
    let nav_cols =
        Layout::horizontal([Constraint::Min(20), Constraint::Length(badge.len() as u16 + 2)])
            .split(rows[0]);
    f.render_widget(Paragraph::new(Line::from(tab_spans)), nav_cols[0]);
    f.render_widget(
        Paragraph::new(Line::styled(
            badge,
            if app.session.is_some() {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.warning)
            },
        ))
        .alignment(Alignment::Right),
        nav_cols[1],
    );

    if show_search_row {
        app.areas.search_bar = rows[1];
        let search_focused = app.focus == FocusPane::SearchBar;
        let search_text = if app.search_query.is_empty() && !search_focused {
            Span::styled(
                "Press / to type a Tidal search query...",
                Style::default().fg(Color::DarkGray),
            )
        } else if search_focused {
            Span::styled(
                format!("{}▏", app.search_query),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(app.search_query.clone(), Style::default().fg(Color::White))
        };
        let sblock = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style(theme, search_focused))
            .title_top(Line::from(" Search ").style(Style::default().fg(Color::DarkGray)));
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::raw(" "), search_text])).block(sblock),
            rows[1],
        );
    } else {
        app.areas.search_bar = Rect::default();
        let rule = "─".repeat(rows[1].width as usize);
        f.render_widget(
            Paragraph::new(Line::styled(rule, Style::default().fg(theme.border))),
            rows[1],
        );
    }

    let content_area = if app.filter_active || !app.filter_query.is_empty() {
        let chunks =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(rows[2]);
        let filter_line = Line::from(vec![
            Span::styled(" Filter: ", Style::default().fg(theme.accent)),
            Span::styled(
                if app.filter_active {
                    format!("{}▏", app.filter_query)
                } else {
                    format!("{} (Esc to clear)", app.filter_query)
                },
                Style::default().fg(theme.fg),
            ),
        ]);
        f.render_widget(Paragraph::new(filter_line), chunks[0]);
        chunks[1]
    } else {
        rows[2]
    };

    match app.view.clone() {
        MainView::Home => render_home_view(f, app, content_area, theme),
        MainView::Search => render_search_view(f, app, content_area, theme),
        MainView::LikedTracks => {
            let tracks = app.library.tracks.clone();
            render_track_table(
                f,
                app,
                &tracks,
                content_area,
                theme,
                "No liked tracks yet — press Ctrl+L to sign in or refresh.",
            );
        }
        MainView::SavedAlbums => {
            render_albums_view(f, app, &app.library.albums.clone(), content_area, theme)
        }
        MainView::Artists => {
            render_artists_view(f, app, &app.library.artists.clone(), content_area, theme)
        }
        MainView::Mixes => {
            render_mixes_view(f, app, &app.library.mixes.clone(), content_area, theme)
        }
        MainView::Playlists => {
            render_playlists_view(f, app, &app.library.playlists.clone(), content_area, theme)
        }
        MainView::Queue => render_queue_view(f, app, content_area, theme),
        MainView::LocalFiles => {
            let tracks = app.local_tracks.clone();
            render_track_table(
                f,
                app,
                &tracks,
                content_area,
                theme,
                "No local .flac/.m4a/.mp3 files found.",
            );
        }
        MainView::Settings => render_settings_view(f, app, content_area, theme),
        MainView::Detail(detail) => render_detail_view(f, app, &detail, content_area, theme),
    }

    let shf = if app.shuffle { " [SHF]" } else { "" };
    let rpt = match app.repeat {
        RepeatMode::Off => "",
        RepeatMode::All => " [RPT:ALL]",
        RepeatMode::One => " [RPT:ONE]",
    };
    let footer_left = if let Some(ref t) = app.current_track {
        let elapsed_s = app.audio.telemetry.position_ms.load(Ordering::Relaxed) as f64 / 1000.0;
        let paused = app.current_gen == 0 || app.audio.telemetry.is_paused.load(Ordering::Relaxed);
        let sym = if paused { "⏸" } else { "▶" };
        format!(
            " {} {} — {}  {}{}{}",
            sym,
            truncate_str(&t.title, 26),
            truncate_str(&t.artist_name, 18),
            fmt_time(elapsed_s),
            shf,
            rpt
        )
    } else {
        format!(" Idle — select a track to play{}{}", shf, rpt)
    };
    let footer_right = if matches!(app.view, MainView::Queue) {
        "J/K reorder · x remove · C clear "
    } else if !app.view_history.is_empty() {
        "Backspace back · / search · H keys "
    } else {
        "/ search · u queue · t theme · H keys "
    };
    let foot_cols = Layout::horizontal([
        Constraint::Min(20),
        Constraint::Length(footer_right.len() as u16 + 1),
    ])
    .split(rows[3]);
    f.render_widget(
        Paragraph::new(Line::styled(footer_left, Style::default().fg(theme.accent))),
        foot_cols[0],
    );
    f.render_widget(
        Paragraph::new(Line::styled(footer_right, Style::default().fg(Color::DarkGray)))
            .alignment(Alignment::Right),
        foot_cols[1],
    );
}

fn render_queue_view(f: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    let tracks = app.queue.clone();
    render_track_table(
        f,
        app,
        &tracks,
        area,
        theme,
        "Play Queue is empty. Press Enter on any track to play, or 'a' to append.",
    );
}

fn render_home_view(f: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    if app.home_sections.is_empty() {
        let msg = if app.loading {
            "Loading your Tidal Home feed..."
        } else if app.session.is_none() {
            "Not signed in to Tidal.\n\nPress Ctrl+L to sign in, or pick Local Files (8) in the sidebar."
        } else {
            "Home feed is empty — try Search (/), Mixes, or Liked Tracks."
        };
        f.render_widget(
            Paragraph::new(msg)
                .style(Style::default().fg(theme.muted))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let chunks = Layout::vertical([Constraint::Length(2), Constraint::Min(4)]).split(area);

    // Section tabs across top
    let mut tab_spans = Vec::new();
    let mut x_cursor = chunks[0].x;
    for (idx, sec) in app.home_sections.iter().enumerate() {
        let active = idx == app.home_section_idx;
        let label = format!(" {} ", truncate_str(&sec.title, 20));
        let w = label.chars().count() as u16;
        if x_cursor + w > chunks[0].x + chunks[0].width {
            break;
        }
        let style = if active {
            Style::default()
                .bg(theme.accent)
                .fg(theme.bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        tab_spans.push(Span::styled(label, style));
        tab_spans.push(Span::raw(" "));
        app.areas
            .main_tabs
            .push((Rect::new(x_cursor, chunks[0].y, w, 1), idx));
        x_cursor += w + 1;
    }
    f.render_widget(Paragraph::new(Line::from(tab_spans)), chunks[0]);

    // Active section content
    let sec = app.home_sections[app.home_section_idx].clone();
    let selected = app.home_item_idx;
    match sec.items {
        HomeItems::Tracks(tracks) => {
            render_track_table_with_selected(f, app, &tracks, selected, chunks[1], theme);
        }
        HomeItems::Albums(albums) => {
            render_albums_list_with_selected(f, app, &albums, selected, chunks[1], theme);
        }
        HomeItems::Playlists(playlists) => {
            render_playlists_list_with_selected(f, app, &playlists, selected, chunks[1], theme);
        }
        HomeItems::Mixes(mixes) => {
            render_mixes_list_with_selected(f, app, &mixes, selected, chunks[1], theme);
        }
    }
}

fn render_search_view(f: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    let chunks = Layout::vertical([Constraint::Length(2), Constraint::Min(3)]).split(area);

    let counts = [
        app.search_results.tracks.len(),
        app.search_results.albums.len(),
        app.search_results.artists.len(),
        app.search_results.playlists.len(),
    ];
    let mut tab_spans = Vec::new();
    let mut x = chunks[0].x;
    for (idx, tab) in SearchTab::ALL.iter().enumerate() {
        let active = *tab == app.search_tab;
        let label = format!(" [{}] {} ({}) ", idx + 1, tab.label(), counts[idx]);
        let w = label.chars().count() as u16;
        let style = if active {
            Style::default()
                .bg(theme.accent)
                .fg(theme.bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        tab_spans.push(Span::styled(label, style));
        tab_spans.push(Span::raw(" "));
        app.areas
            .main_tabs
            .push((Rect::new(x, chunks[0].y, w, 1), idx));
        x += w + 1;
    }
    f.render_widget(Paragraph::new(Line::from(tab_spans)), chunks[0]);

    if app.search_results.query.is_empty() && !app.loading {
        f.render_widget(
            Paragraph::new("Press / or Ctrl+F to search Tidal tracks, albums, artists, and playlists.")
                .style(Style::default().fg(theme.muted))
                .alignment(Alignment::Center),
            chunks[1],
        );
        return;
    }

    match app.search_tab {
        SearchTab::Tracks => {
            let tracks = app.search_results.tracks.clone();
            render_track_table(f, app, &tracks, chunks[1], theme, "No matching tracks.");
        }
        SearchTab::Albums => {
            let albums = app.search_results.albums.clone();
            render_albums_view(f, app, &albums, chunks[1], theme);
        }
        SearchTab::Artists => {
            let artists = app.search_results.artists.clone();
            render_artists_view(f, app, &artists, chunks[1], theme);
        }
        SearchTab::Playlists => {
            let pls = app.search_results.playlists.clone();
            render_playlists_view(f, app, &pls, chunks[1], theme);
        }
    }
}

fn render_detail_view(
    f: &mut Frame,
    app: &mut App,
    detail: &DetailContent,
    area: Rect,
    theme: &Theme,
) {
    let chunks = Layout::vertical([Constraint::Length(6), Constraint::Min(4)]).split(area);
    let header_area = chunks[0];
    let body_area = chunks[1];

    let (cover_key, title, subtitle, meta) = match detail {
        DetailContent::Album { album, .. } => (
            album.cover.clone(),
            album.title.clone(),
            album.artist_name.clone(),
            format!(
                "{} · {} tracks · {} · {}",
                album.release_year.as_deref().unwrap_or("—"),
                album.track_count,
                fmt_time(album.duration as f64),
                album.audio_quality
            ),
        ),
        DetailContent::Playlist { playlist, .. } => (
            playlist.cover.clone(),
            playlist.title.clone(),
            format!("By {}", playlist.creator),
            format!(
                "{} tracks · {}",
                playlist.track_count,
                fmt_time(playlist.duration as f64)
            ),
        ),
        DetailContent::Mix { mix, tracks } => (
            None,
            mix.title.clone(),
            mix.subtitle.clone(),
            format!("{} tracks", tracks.len()),
        ),
        DetailContent::Radio { seed, tracks } => (
            seed.album_cover.clone(),
            format!("Radio: {}", seed.title),
            format!("Seeded from {}", seed.artist_name),
            format!("{} tracks", tracks.len()),
        ),
        DetailContent::Artist(ad) => (
            ad.artist.picture.clone(),
            ad.artist.name.clone(),
            "Artist".to_string(),
            format!(
                "{} top tracks · {} albums",
                ad.top_tracks.len(),
                ad.albums.len()
            ),
        ),
    };

    // Header layout: optional cover + metadata
    let h_cols =
        Layout::horizontal([Constraint::Length(12), Constraint::Min(10)]).split(header_area);
    if let Some(ref key) = cover_key {
        if let Some(cached) = app.covers.get_mut(key) {
            if app.config.image_cover {
                let widget = StatefulImage::new().resize(Resize::Scale(Some(FilterType::Lanczos3)));
                f.render_stateful_widget(widget, h_cols[0], &mut cached.protocol);
            } else {
                f.render_widget(
                    Paragraph::new(Text::from(cached.mini_braille.clone())),
                    h_cols[0],
                );
            }
        }
    }

    let header_lines = vec![
        Line::from(Span::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(subtitle, Style::default().fg(theme.fg))),
        Line::from(Span::styled(meta, Style::default().fg(theme.muted))),
        Line::from(Span::styled(
            "[Enter] Play  [S] Shuffle Play  [m] ♥ Favorite  [d] ⬇ Download All",
            Style::default().fg(theme.accent_secondary),
        )),
    ];
    f.render_widget(Paragraph::new(header_lines), h_cols[1]);

    match detail {
        DetailContent::Album { tracks, .. }
        | DetailContent::Playlist { tracks, .. }
        | DetailContent::Mix { tracks, .. }
        | DetailContent::Radio { tracks, .. } => {
            render_track_table(f, app, tracks, body_area, theme, "No tracks available.");
        }
        DetailContent::Artist(ad) => {
            let sub = Layout::vertical([Constraint::Length(2), Constraint::Min(3)]).split(body_area);
            let mut spans = Vec::new();
            let mut x = sub[0].x;
            for (idx, label) in ["Top Tracks", "Discography (Albums)"].iter().enumerate() {
                let active = app.artist_sub_tab == idx;
                let text = format!(" [{}] {} ", idx + 1, label);
                let w = text.chars().count() as u16;
                let style = if active {
                    Style::default()
                        .bg(theme.accent)
                        .fg(theme.bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.muted)
                };
                spans.push(Span::styled(text, style));
                spans.push(Span::raw(" "));
                app.areas
                    .main_tabs
                    .push((Rect::new(x, sub[0].y, w, 1), idx));
                x += w + 1;
            }
            f.render_widget(Paragraph::new(Line::from(spans)), sub[0]);
            if app.artist_sub_tab == 0 {
                render_track_table(f, app, &ad.top_tracks, sub[1], theme, "No top tracks.");
            } else {
                render_albums_view(f, app, &ad.albums, sub[1], theme);
            }
        }
    }
}

fn render_track_table(
    f: &mut Frame,
    app: &mut App,
    tracks: &[TrackInfo],
    area: Rect,
    theme: &Theme,
    empty_msg: &str,
) {
    let filtered = app.filter_tracks(tracks);
    if filtered.is_empty() {
        f.render_widget(
            Paragraph::new(empty_msg)
                .style(Style::default().fg(theme.muted))
                .alignment(Alignment::Center),
            area,
        );
        return;
    }
    let only_tracks: Vec<TrackInfo> = filtered.iter().map(|(_, t)| (*t).clone()).collect();
    render_track_table_with_selected(f, app, &only_tracks, app.main_selected, area, theme);
}

fn render_track_table_with_selected(
    f: &mut Frame,
    app: &mut App,
    tracks: &[TrackInfo],
    selected: usize,
    area: Rect,
    theme: &Theme,
) {
    let visible_rows = area.height.saturating_sub(2) as usize;
    let selected = selected.min(tracks.len().saturating_sub(1));
    let scroll = if visible_rows == 0 {
        0
    } else if selected >= visible_rows {
        selected + 1 - visible_rows
    } else {
        0
    };

    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("♥"),
        Cell::from("Title"),
        Cell::from("Artist"),
        Cell::from("Album"),
        Cell::from("Qual"),
        Cell::from("Time"),
    ])
    .style(
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    );

    let playing_id = app.current_track.as_ref().map(|t| (t.id, t.local_path.clone()));
    let mut rows = Vec::new();
    for (offset, track) in tracks.iter().skip(scroll).take(visible_rows.max(1)).enumerate() {
        let idx = scroll + offset;
        let is_selected = idx == selected;
        let is_playing = playing_id
            .as_ref()
            .map(|(id, lp)| {
                if track.id > 0 {
                    *id == track.id
                } else {
                    *lp == track.local_path
                }
            })
            .unwrap_or(false);
        let is_fav = track.id > 0 && app.library.fav_track_ids.contains(&track.id);

        let row_style = if is_selected {
            Style::default()
                .bg(theme.selection_bg)
                .fg(theme.selection_fg)
                .add_modifier(Modifier::BOLD)
        } else if is_playing {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };

        let num_str = if is_playing {
            "▶".to_string()
        } else {
            format!("{}", idx + 1)
        };
        let fav_str = if is_fav { "♥" } else { " " };
        let title_str = if track.explicit {
            format!("{} [E]", track.title)
        } else {
            track.title.clone()
        };
        let qual = if track.hires {
            "HI-RES"
        } else {
            track.audio_quality.as_str()
        };

        rows.push(
            Row::new(vec![
                Cell::from(num_str),
                Cell::from(fav_str).style(if is_fav {
                    Style::default().fg(theme.error)
                } else {
                    row_style
                }),
                Cell::from(title_str),
                Cell::from(track.artist_name.clone()),
                Cell::from(track.album_name.clone()),
                Cell::from(qual.to_string()),
                Cell::from(fmt_time(track.duration as f64)),
            ])
            .style(row_style),
        );

        let y = area.y + 1 + offset as u16;
        if y < area.y + area.height {
            app.areas
                .main_rows
                .push((Rect::new(area.x, y, area.width, 1), idx));
        }
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(2),
            Constraint::Percentage(34),
            Constraint::Percentage(24),
            Constraint::Percentage(24),
            Constraint::Length(8),
            Constraint::Length(6),
        ],
    )
    .header(header)
    .column_spacing(1);
    f.render_widget(table, area);
}

fn render_albums_view(
    f: &mut Frame,
    app: &mut App,
    albums: &[crate::api::AlbumInfo],
    area: Rect,
    theme: &Theme,
) {
    render_albums_list_with_selected(f, app, albums, app.main_selected, area, theme);
}

fn render_albums_list_with_selected(
    f: &mut Frame,
    app: &mut App,
    albums: &[crate::api::AlbumInfo],
    selected: usize,
    area: Rect,
    theme: &Theme,
) {
    if albums.is_empty() {
        f.render_widget(
            Paragraph::new("No albums.")
                .style(Style::default().fg(theme.muted))
                .alignment(Alignment::Center),
            area,
        );
        return;
    }
    let visible = area.height.saturating_sub(2) as usize;
    let selected = selected.min(albums.len() - 1);
    let scroll = if visible > 0 && selected >= visible {
        selected + 1 - visible
    } else {
        0
    };

    let header = Row::new(vec!["#", "♥", "Album", "Artist", "Year", "Tracks", "Qual"]).style(
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    );

    let mut rows = Vec::new();
    for (offset, a) in albums.iter().skip(scroll).take(visible.max(1)).enumerate() {
        let idx = scroll + offset;
        let is_fav = app.library.fav_album_ids.contains(&a.id);
        let style = if idx == selected {
            Style::default()
                .bg(theme.selection_bg)
                .fg(theme.selection_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        rows.push(
            Row::new(vec![
                Cell::from(format!("{}", idx + 1)),
                Cell::from(if is_fav { "♥" } else { " " }),
                Cell::from(a.title.clone()),
                Cell::from(a.artist_name.clone()),
                Cell::from(a.release_year.as_deref().unwrap_or("—").to_string()),
                Cell::from(format!("{}", a.track_count)),
                Cell::from(a.audio_quality.clone()),
            ])
            .style(style),
        );
        let y = area.y + 1 + offset as u16;
        if y < area.y + area.height {
            app.areas
                .main_rows
                .push((Rect::new(area.x, y, area.width, 1), idx));
        }
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(2),
            Constraint::Percentage(40),
            Constraint::Percentage(30),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Length(9),
        ],
    )
    .header(header);
    f.render_widget(table, area);
}

fn render_artists_view(
    f: &mut Frame,
    app: &mut App,
    artists: &[crate::api::ArtistInfo],
    area: Rect,
    theme: &Theme,
) {
    if artists.is_empty() {
        f.render_widget(
            Paragraph::new("No artists.")
                .style(Style::default().fg(theme.muted))
                .alignment(Alignment::Center),
            area,
        );
        return;
    }
    let visible = area.height as usize;
    let selected = app.main_selected.min(artists.len() - 1);
    let scroll = if visible > 0 && selected >= visible {
        selected + 1 - visible
    } else {
        0
    };
    let mut items = Vec::new();
    for (offset, a) in artists.iter().skip(scroll).take(visible.max(1)).enumerate() {
        let idx = scroll + offset;
        let is_fav = app.library.fav_artist_ids.contains(&a.id);
        let style = if idx == selected {
            Style::default()
                .bg(theme.selection_bg)
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        let heart = if is_fav { "♥" } else { " " };
        items.push(ListItem::new(Line::from(Span::styled(
            format!(" {:>3}. {} {}", idx + 1, heart, a.name),
            style,
        ))));
        app.areas
            .main_rows
            .push((Rect::new(area.x, area.y + offset as u16, area.width, 1), idx));
    }
    f.render_widget(List::new(items), area);
}

fn render_mixes_view(
    f: &mut Frame,
    app: &mut App,
    mixes: &[crate::api::MixInfo],
    area: Rect,
    theme: &Theme,
) {
    render_mixes_list_with_selected(f, app, mixes, app.main_selected, area, theme);
}

fn render_mixes_list_with_selected(
    f: &mut Frame,
    app: &mut App,
    mixes: &[crate::api::MixInfo],
    selected: usize,
    area: Rect,
    theme: &Theme,
) {
    if mixes.is_empty() {
        f.render_widget(
            Paragraph::new("No mixes available.")
                .style(Style::default().fg(theme.muted))
                .alignment(Alignment::Center),
            area,
        );
        return;
    }
    let visible = area.height as usize;
    let selected = selected.min(mixes.len() - 1);
    let scroll = if visible > 0 && selected >= visible {
        selected + 1 - visible
    } else {
        0
    };
    let mut items = Vec::new();
    for (offset, m) in mixes.iter().skip(scroll).take(visible.max(1)).enumerate() {
        let idx = scroll + offset;
        let style = if idx == selected {
            Style::default()
                .bg(theme.selection_bg)
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        let line = if m.subtitle.is_empty() {
            format!(" {:>2}. ◈ {}", idx + 1, m.title)
        } else {
            format!(" {:>2}. ◈ {} — {}", idx + 1, m.title, m.subtitle)
        };
        items.push(ListItem::new(Line::from(Span::styled(line, style))));
        app.areas
            .main_rows
            .push((Rect::new(area.x, area.y + offset as u16, area.width, 1), idx));
    }
    f.render_widget(List::new(items), area);
}

fn render_playlists_view(
    f: &mut Frame,
    app: &mut App,
    playlists: &[crate::api::PlaylistInfo],
    area: Rect,
    theme: &Theme,
) {
    render_playlists_list_with_selected(f, app, playlists, app.main_selected, area, theme);
}

fn render_playlists_list_with_selected(
    f: &mut Frame,
    app: &mut App,
    playlists: &[crate::api::PlaylistInfo],
    selected: usize,
    area: Rect,
    theme: &Theme,
) {
    if playlists.is_empty() {
        f.render_widget(
            Paragraph::new("No playlists found.")
                .style(Style::default().fg(theme.muted))
                .alignment(Alignment::Center),
            area,
        );
        return;
    }
    let visible = area.height.saturating_sub(2) as usize;
    let selected = selected.min(playlists.len() - 1);
    let scroll = if visible > 0 && selected >= visible {
        selected + 1 - visible
    } else {
        0
    };
    let header = Row::new(vec!["#", "♥", "Playlist", "Creator", "Tracks", "Duration"]).style(
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::BOLD),
    );
    let mut rows = Vec::new();
    for (offset, p) in playlists.iter().skip(scroll).take(visible.max(1)).enumerate() {
        let idx = scroll + offset;
        let is_fav = app.library.fav_playlist_uuids.contains(&p.uuid);
        let style = if idx == selected {
            Style::default()
                .bg(theme.selection_bg)
                .fg(theme.selection_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        rows.push(
            Row::new(vec![
                Cell::from(format!("{}", idx + 1)),
                Cell::from(if is_fav { "♥" } else { " " }),
                Cell::from(p.title.clone()),
                Cell::from(p.creator.clone()),
                Cell::from(format!("{}", p.track_count)),
                Cell::from(fmt_time(p.duration as f64)),
            ])
            .style(style),
        );
        let y = area.y + 1 + offset as u16;
        if y < area.y + area.height {
            app.areas
                .main_rows
                .push((Rect::new(area.x, y, area.width, 1), idx));
        }
    }
    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(2),
            Constraint::Percentage(45),
            Constraint::Percentage(25),
            Constraint::Length(8),
            Constraint::Length(9),
        ],
    )
    .header(header);
    f.render_widget(table, area);
}

fn render_settings_view(f: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    let mut entries: Vec<(String, String)> = vec![
        (
            "Audio Streaming Quality".into(),
            app.config.audio_quality.label().into(),
        ),
        ("Color Theme".into(), app.config.theme_mode.label().into()),
        (
            "Lyrics Below Player".into(),
            if app.show_lyrics { "Enabled" } else { "Hidden" }.into(),
        ),
        (
            "Pixel Cover Art (Kitty/Sixel/Halfblocks)".into(),
            if app.config.image_cover { "Enabled" } else { "Braille Mode" }.into(),
        ),
        (
            "Disk Audio Cache".into(),
            if app.config.cache_enabled { "Enabled" } else { "Disabled" }.into(),
        ),
        (
            "Beat / Drop Detection".into(),
            if app.config.drop_detection { "Enabled" } else { "Disabled" }.into(),
        ),
        (
            "Calm Visualizer Mode".into(),
            if app.config.calm_mode { "On" } else { "Off" }.into(),
        ),
        (
            "Discord Rich Presence".into(),
            if app.config.discord_rpc { "Enabled" } else { "Disabled" }.into(),
        ),
        (
            "Clear Audio Cache".into(),
            format!("{} ({} tracks) — Enter to clear", fmt_bytes(app.cache_bytes), app.cache_count),
        ),
        (
            "Tidal Account".into(),
            app.session
                .as_ref()
                .map(|s| format!("{} — Enter to sign out", s.display_label()))
                .unwrap_or_else(|| "Not signed in — Enter to sign in".into()),
        ),
        (
            "Output Device: System Default".into(),
            if app.config.output_device.is_none() { "● Active" } else { "Click/Enter to select" }.into(),
        ),
    ];

    for dev in &app.output_devices {
        let active = app.config.output_device.as_deref() == Some(dev.as_str());
        entries.push((
            format!("Output Device: {}", dev),
            if active { "● Active" } else { "Select" }.into(),
        ));
    }

    let selected = app.main_selected.min(entries.len().saturating_sub(1));
    let visible = area.height as usize;
    let scroll = if visible > 0 && selected >= visible {
        selected + 1 - visible
    } else {
        0
    };

    let mut items = Vec::new();
    for (offset, (label, val)) in entries.iter().skip(scroll).take(visible.max(1)).enumerate() {
        let idx = scroll + offset;
        let is_sel = idx == selected;
        let style = if is_sel {
            Style::default()
                .bg(theme.selection_bg)
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!(" {:<42} ", label), style),
            Span::styled(
                val.clone(),
                if is_sel {
                    style
                } else {
                    Style::default().fg(theme.accent_secondary)
                },
            ),
        ])));
        app.areas
            .main_rows
            .push((Rect::new(area.x, area.y + offset as u16, area.width, 1), idx));
    }

    f.render_widget(List::new(items), area);
}

fn render_help_modal(f: &mut Frame, root: Rect, theme: &Theme) {
    let area = centered_rect(68, 76, root);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title_top(
            Line::from(" Keyboard Shortcuts & Mouse Controls (Esc to close) ")
                .style(Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
        );
    let lines = vec![
        Line::from(Span::styled("Minimalist Now Playing", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))),
        Line::from("  Space               Play / Pause"),
        Line::from("  n / →  &  p / ←     Next / Previous track"),
        Line::from("  ↑ / ↓  or  + / -    Volume up / down (or scroll wheel)"),
        Line::from("  . / ,               Seek +10s / -10s"),
        Line::from("  l  /  j, k          Toggle lyrics display / Scroll plain lyrics"),
        Line::from("  ?                   Toggle controls border around player card"),
        Line::from("  /  or  Ctrl+F       Open Search overlay"),
        Line::from("  b  or  Tab          Open / toggle Browser overlay"),
        Line::from("  u                   Open Play Queue overlay"),
        Line::from(""),
        Line::from(Span::styled("Queue, Discovery & Tidal", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))),
        Line::from("  r                   Start Track Radio from current or selected track"),
        Line::from("  m                   Toggle Favorite (♥) on Tidal"),
        Line::from("  d                   Download track / album / playlist in background"),
        Line::from("  s  /  R             Toggle Shuffle / Cycle Repeat (Off/All/One)"),
        Line::from("  t                   Cycle Theme (Album Art -> Omarchy -> Pywal -> Dark)"),
        Line::from("  Ctrl+L              Sign in to Tidal (device code flow)"),
        Line::from(""),
        Line::from(Span::styled("Inside Browser & Queue Overlay", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))),
        Line::from("  ↑/↓ or j/k          Navigate list (mouse click / double-click supported)"),
        Line::from("  Enter               Play track (returns to Now Playing) / Open item"),
        Line::from("  a  /  i             Append to Queue / Insert as Play Next"),
        Line::from("  f                   Fuzzy-filter current track list"),
        Line::from("  Tab / Shift+Tab     Cycle Browser sections"),
        Line::from("  J / K / x / C       (Queue tab) Move down/up / Remove / Clear queue"),
        Line::from("  Esc / b / Backspace Back / Close Browser overlay"),
        Line::from("  q (Now Playing) / Q Quit"),
    ];
    f.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        area,
    );
}

fn render_auth_modal(f: &mut Frame, app: &App, root: Rect, theme: &Theme) {
    let area = centered_rect(60, 42, root);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title_top(
            Line::from(" Sign in to Tidal (Esc to close) ")
                .style(Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
        );

    let mut lines = vec![Line::raw("")];
    if app.auth_modal.loading {
        lines.push(Line::styled(
            "Requesting login code from Tidal...",
            Style::default().fg(theme.fg),
        ));
    } else if let Some(ref err) = app.auth_modal.error {
        lines.push(Line::styled(
            format!("Error: {}", err),
            Style::default().fg(theme.error),
        ));
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Press Enter to generate a new code.",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        lines.push(Line::styled(
            "1. Open this URL in your browser (opened automatically):",
            Style::default().fg(Color::DarkGray),
        ));
        lines.push(Line::styled(
            app.auth_modal.verification_uri.clone(),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "2. Confirm code:",
            Style::default().fg(Color::DarkGray),
        ));
        lines.push(Line::styled(
            format!("   {}", app.auth_modal.user_code),
            Style::default()
                .fg(theme.accent_secondary)
                .add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            app.auth_modal.status.clone(),
            Style::default().fg(theme.success),
        ));
    }
    f.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(block)
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_toast(f: &mut Frame, root: Rect, msg: &str, accent_rgb: (u8, u8, u8)) {
    let w = (msg.chars().count() as u16 + 2).min(root.width.saturating_sub(4));
    let y = if root.height >= 16 {
        root.y + root.height - 2
    } else {
        root.y + root.height.saturating_sub(1)
    };
    let area = Rect::new(root.x + 2, y, w, 1);
    f.render_widget(
        Paragraph::new(Line::styled(
            truncate_str(msg, w as usize),
            Style::new().fg(Color::Rgb(accent_rgb.0, accent_rgb.1, accent_rgb.2)),
        )),
        area,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(r);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup_layout[1])[1]
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else if max_chars == 1 {
        "…".to_string()
    } else {
        let mut out: String = s.chars().take(max_chars - 1).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AlbumInfo, ArtistInfo, MixInfo, PlaylistInfo, SearchResults};
    use ratatui::{backend::TestBackend, Terminal};

    fn dummy_track(id: u64, title: &str) -> TrackInfo {
        TrackInfo {
            id,
            title: title.into(),
            artist_name: "Artist".into(),
            artist_id: Some(10),
            artists: vec!["Artist".into()],
            album_name: "Album".into(),
            album_id: Some(20),
            album_cover: None,
            album_artist: "Artist".into(),
            album_copyright: None,
            album_release_year: Some(2024),
            track_num: 1,
            volume_num: 1,
            isrc: None,
            audio_quality: "LOSSLESS".into(),
            duration: 210,
            explicit: false,
            hires: false,
            local_path: None,
        }
    }

    #[test]
    fn renders_all_views_cleanly_on_test_backend() {
        let mut app = App::new(true, None, false);
        let t = dummy_track(1, "Midnight Tide");
        app.queue = vec![t.clone()];
        app.queue_idx = Some(0);
        app.current_track = Some(t.clone());
        app.home_sections = vec![crate::api::HomeSection {
            title: "Mixes".into(),
            items: HomeItems::Mixes(vec![MixInfo {
                id: "m1".into(),
                title: "My Mix 1".into(),
                subtitle: "Synthwave".into(),
                cover: None,
            }]),
        }];
        app.search_results = SearchResults {
            query: "tide".into(),
            tracks: vec![t.clone()],
            albums: vec![AlbumInfo {
                id: 20,
                title: "Ocean".into(),
                artist_name: "Artist".into(),
                artist_id: Some(10),
                cover: None,
                release_year: Some("2024".into()),
                track_count: 10,
                duration: 2000,
                audio_quality: "LOSSLESS".into(),
                explicit: false,
                copyright: None,
            }],
            artists: vec![ArtistInfo {
                id: 10,
                name: "Artist".into(),
                picture: None,
            }],
            playlists: vec![PlaylistInfo {
                uuid: "p1".into(),
                title: "Chill".into(),
                description: "".into(),
                creator: "Tidal".into(),
                cover: None,
                track_count: 5,
                duration: 900,
            }],
        };
        app.library.tracks = vec![t.clone()];
        app.lyrics_state = LyricsState::Loaded(LyricsData::Synced(vec![
            lyrics::LyricLine {
                time: 0.0,
                text: "Waves crash in the dark".into(),
            },
        ]));

        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();

        let views = vec![
            MainView::Home,
            MainView::Search,
            MainView::LikedTracks,
            MainView::SavedAlbums,
            MainView::Artists,
            MainView::Mixes,
            MainView::Playlists,
            MainView::Queue,
            MainView::LocalFiles,
            MainView::Settings,
            MainView::Detail(DetailContent::Album {
                album: app.search_results.albums[0].clone(),
                tracks: vec![t.clone()],
            }),
        ];

        // Minimalist Now Playing canvas + controls border
        app.browser_open = false;
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let all_text: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all_text.contains("Midnight Tide"));
        assert!(all_text.contains("Press ? for ctrl"));
        assert!(all_text.contains("♪"));
        assert!(all_text.contains("Waves crash in the dark"));

        app.show_controls = true;
        terminal.draw(|f| render(f, &mut app)).unwrap();
        app.show_controls = false;

        // Browser overlay across every view
        app.browser_open = true;
        for v in views {
            app.view = v;
            terminal.draw(|f| render(f, &mut app)).unwrap();
        }

        app.modal = ModalState::Help;
        terminal.draw(|f| render(f, &mut app)).unwrap();

        app.modal = ModalState::Auth;
        terminal.draw(|f| render(f, &mut app)).unwrap();
    }
}
