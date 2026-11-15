use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use rand::seq::SliceRandom;
use ratatui::layout::Rect;
use souvlaki::MediaControlEvent;

use crate::api::{
    AlbumInfo, ArtistDetail, FavoriteKind, HomeSection, LibraryData, MixInfo, PlaylistInfo,
    SearchResults, TidalClient, TrackInfo,
};
use crate::audio::{self, AudioEngine, RepeatMode};
use crate::auth::{self, DeviceAuthState, DevicePollResult, Session};
use crate::config::{self, Config, ThemeMode};
use crate::cover::CoverManager;
use crate::discord::{DiscordPresence, DiscordRpcBridge};
use crate::download_queue::DownloadQueue;
use crate::lyrics::{self, LyricsState};
use crate::metadata;
use crate::mpris::MprisBridge;
use crate::spectrum::SpectrumState;
use crate::theme::ThemeManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPane {
    SearchBar,
    Main,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarSection {
    Home,
    Search,
    LikedTracks,
    SavedAlbums,
    Artists,
    Mixes,
    Playlists,
    Queue,
    LocalFiles,
    Settings,
}

impl SidebarSection {
    pub const ALL: [SidebarSection; 10] = [
        Self::Home,
        Self::Search,
        Self::LikedTracks,
        Self::SavedAlbums,
        Self::Artists,
        Self::Mixes,
        Self::Playlists,
        Self::Queue,
        Self::LocalFiles,
        Self::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Search => "Search",
            Self::LikedTracks => "Liked",
            Self::SavedAlbums => "Albums",
            Self::Artists => "Artists",
            Self::Mixes => "Mixes",
            Self::Playlists => "Playlists",
            Self::Queue => "Queue",
            Self::LocalFiles => "Local",
            Self::Settings => "Settings",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTab {
    Tracks,
    Albums,
    Artists,
    Playlists,
}

impl SearchTab {
    pub const ALL: [SearchTab; 4] = [Self::Tracks, Self::Albums, Self::Artists, Self::Playlists];
    pub fn label(self) -> &'static str {
        match self {
            Self::Tracks => "Tracks",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Playlists => "Playlists",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Tracks => Self::Albums,
            Self::Albums => Self::Artists,
            Self::Artists => Self::Playlists,
            Self::Playlists => Self::Tracks,
        }
    }
    pub fn prev(self) -> Self {
        match self {
            Self::Tracks => Self::Playlists,
            Self::Albums => Self::Tracks,
            Self::Artists => Self::Albums,
            Self::Playlists => Self::Artists,
        }
    }
}

#[derive(Debug, Clone)]
pub enum DetailContent {
    Album {
        album: AlbumInfo,
        tracks: Vec<TrackInfo>,
    },
    Playlist {
        playlist: PlaylistInfo,
        tracks: Vec<TrackInfo>,
    },
    Mix {
        mix: MixInfo,
        tracks: Vec<TrackInfo>,
    },
    Radio {
        seed: TrackInfo,
        tracks: Vec<TrackInfo>,
    },
    Artist(ArtistDetail),
}

#[derive(Debug, Clone)]
pub enum MainView {
    Home,
    Search,
    LikedTracks,
    SavedAlbums,
    Artists,
    Mixes,
    Playlists,
    Queue,
    LocalFiles,
    Settings,
    Detail(DetailContent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalState {
    None,
    Help,
    Auth,
}

pub enum AppEvent {
    SessionLoaded(Session),
    ProfileRefreshed(Session),
    HomeLoaded(Result<Vec<HomeSection>, String>),
    SearchLoaded(Result<SearchResults, String>),
    LibraryLoaded(Result<LibraryData, String>),
    DetailLoaded(Result<DetailContent, String>),
    LyricsLoaded { track_id: u64, state: LyricsState },
    CoverLoaded { key: String, bytes: Vec<u8> },
    FavoriteToggled { kind: FavoriteKind, id: String, is_fav: bool },
    FavoriteError(String),
    LocalFilesScanned(Vec<TrackInfo>),
    DeviceAuthStarted(Result<DeviceAuthState, String>),
    DeviceAuthPolled(Result<DevicePollResult, String>),
    Toast(String),
}

#[derive(Default, Clone)]
pub struct LayoutAreas {
    pub browser_tabs: Vec<(Rect, usize)>,
    pub search_bar: Rect,
    pub main_area: Rect,
    pub main_tabs: Vec<(Rect, usize)>,
    pub main_rows: Vec<(Rect, usize)>,
    pub player_card: Rect,
    pub cover_thumb: Rect,
    pub btn_play: Rect,
    pub progress_bar: Rect,
    pub volume_bar: Rect,
}

pub struct AuthModalState {
    pub loading: bool,
    pub user_code: String,
    pub verification_uri: String,
    pub status: String,
    pub error: Option<String>,
}

impl Default for AuthModalState {
    fn default() -> Self {
        Self {
            loading: false,
            user_code: String::new(),
            verification_uri: String::new(),
            status: "Starting device authorization...".into(),
            error: None,
        }
    }
}

pub struct App {
    pub should_quit: bool,
    pub config: Config,
    pub theme_mgr: ThemeManager,
    pub session: Option<Session>,
    pub client: Option<TidalClient>,
    pub audio: AudioEngine,
    pub downloads: DownloadQueue,
    pub mpris: MprisBridge,
    pub discord: DiscordRpcBridge,
    pub covers: CoverManager,

    pub focus: FocusPane,
    pub modal: ModalState,
    pub auth_modal: AuthModalState,
    pub browser_open: bool,
    pub show_lyrics: bool,
    pub show_controls: bool,

    // Navigation & Selection
    pub sidebar_idx: usize,
    pub view: MainView,
    pub view_history: Vec<MainView>,
    pub main_selected: usize,
    pub main_scroll: usize,
    pub home_section_idx: usize,
    pub home_item_idx: usize,
    pub search_query: String,
    pub search_tab: SearchTab,
    pub artist_sub_tab: usize, // 0 = Top Tracks, 1 = Albums
    pub filter_active: bool,
    pub filter_query: String,

    // Data
    pub loading: bool,
    pub home_sections: Vec<HomeSection>,
    pub search_results: SearchResults,
    pub library: LibraryData,
    pub local_tracks: Vec<TrackInfo>,
    pub output_devices: Vec<String>,
    pub cache_bytes: u64,
    pub cache_count: usize,

    // Playback & Queue
    pub queue: Vec<TrackInfo>,
    pub queue_idx: Option<usize>,
    pub current_track: Option<TrackInfo>,
    pub current_gen: u64,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub lyrics_state: LyricsState,
    pub lyrics_scroll: usize,

    // Visualizer & Reactivity
    pub spectrum_bar: SpectrumState,
    pub last_beat_idx: usize,
    pub fired_drops: HashSet<u64>,
    pub drop_until: Option<Instant>,

    // Status & Toast
    pub toast: Option<(String, Instant)>,

    // Channels & UI Hit Testing
    pub event_tx: mpsc::Sender<AppEvent>,
    pub event_rx: mpsc::Receiver<AppEvent>,
    pub areas: LayoutAreas,
    last_mpris_push: Instant,
    last_theme_poll: Instant,
    last_discord_sync: Option<(u64, bool, u64, Instant)>,
}

impl App {
    pub fn new(headless: bool, initial_search: Option<String>, start_local: bool) -> Self {
        let cfg = config::load();
        let theme_mgr = ThemeManager::new(cfg.theme_mode);
        let audio = AudioEngine::spawn(cfg.volume, cfg.output_device.clone());
        let downloads = DownloadQueue::new();
        let mpris = MprisBridge::new();
        let discord = DiscordRpcBridge::new(headless, cfg.discord_client_id.clone());
        let covers = if headless {
            CoverManager::new_headless()
        } else {
            CoverManager::new_stdio()
        };
        let (event_tx, event_rx) = mpsc::channel();
        let (cache_bytes, cache_count) = audio::cache_stats();
        let output_devices = if headless {
            Vec::new()
        } else {
            audio::list_output_devices()
        };

        let initial_sidebar = if start_local {
            8
        } else if initial_search.is_some() {
            1
        } else {
            0
        };

        let mut app = Self {
            should_quit: false,
            config: cfg,
            theme_mgr,
            session: None,
            client: None,
            audio,
            downloads,
            mpris,
            discord,
            covers,
            focus: FocusPane::Main,
            modal: ModalState::None,
            auth_modal: AuthModalState::default(),
            browser_open: true,
            show_lyrics: true,
            show_controls: false,
            sidebar_idx: initial_sidebar,
            view: if start_local {
                MainView::LocalFiles
            } else if initial_search.is_some() {
                MainView::Search
            } else {
                MainView::Home
            },
            view_history: Vec::new(),
            main_selected: 0,
            main_scroll: 0,
            home_section_idx: 0,
            home_item_idx: 0,
            search_query: initial_search.clone().unwrap_or_default(),
            search_tab: SearchTab::Tracks,
            artist_sub_tab: 0,
            filter_active: false,
            filter_query: String::new(),
            loading: false,
            home_sections: Vec::new(),
            search_results: SearchResults::default(),
            library: LibraryData::default(),
            local_tracks: Vec::new(),
            output_devices,
            cache_bytes,
            cache_count,
            queue: Vec::new(),
            queue_idx: None,
            current_track: None,
            current_gen: 0,
            shuffle: false,
            repeat: RepeatMode::Off,
            lyrics_state: LyricsState::Idle,
            lyrics_scroll: 0,
            spectrum_bar: SpectrumState::new(10, 44100),
            last_beat_idx: 0,
            fired_drops: HashSet::new(),
            drop_until: None,
            toast: None,
            event_tx,
            event_rx,
            areas: LayoutAreas::default(),
            last_mpris_push: Instant::now(),
            last_theme_poll: Instant::now(),
            last_discord_sync: None,
        };

        if !headless {
            app.bootstrap_async(initial_search);
        }
        app
    }

    fn bootstrap_async(&mut self, initial_search: Option<String>) {
        // Scan local files immediately in background
        let tx_local = self.event_tx.clone();
        let dl_dir = self.config.resolved_download_dir();
        thread::spawn(move || {
            let files = scan_local_tracks(&dl_dir);
            let _ = tx_local.send(AppEvent::LocalFilesScanned(files));
        });

        // Import/load Tidal session in background so UI appears instantly
        self.loading = true;
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            if let Some(session) = auth::load_or_import_session() {
                let _ = tx.send(AppEvent::SessionLoaded(session.clone()));
                let client = TidalClient::new(session);
                if let Ok(profile) = client.fetch_user_profile() {
                    let _ = tx.send(AppEvent::ProfileRefreshed(profile));
                }
                match client.home_feed() {
                    Ok(h) => {
                        let _ = tx.send(AppEvent::HomeLoaded(Ok(h)));
                    }
                    Err(e) => {
                        let _ = tx.send(AppEvent::HomeLoaded(Err(e.to_string())));
                    }
                }
                if let Ok(lib) = client.library() {
                    let _ = tx.send(AppEvent::LibraryLoaded(Ok(lib)));
                }
                if let Some(q) = initial_search {
                    match client.search_all(&q, 30) {
                        Ok(res) => {
                            let _ = tx.send(AppEvent::SearchLoaded(Ok(res)));
                        }
                        Err(e) => {
                            let _ = tx.send(AppEvent::SearchLoaded(Err(e.to_string())));
                        }
                    }
                }
            } else {
                let _ = tx.send(AppEvent::HomeLoaded(Err(
                    "Not signed in — press Ctrl+L to connect your Tidal account".into(),
                )));
            }
        });
    }

    pub fn notify(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now() + Duration::from_secs(4)));
    }

    pub fn tick(&mut self) {
        // Expire toast
        if let Some((_, until)) = self.toast {
            if Instant::now() >= until {
                self.toast = None;
            }
        }

        // Poll background events
        while let Ok(ev) = self.event_rx.try_recv() {
            self.handle_app_event(ev);
        }

        // Check audio telemetry errors / natural track end
        let playback_err = self
            .audio
            .telemetry
            .error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(err) = playback_err {
            self.notify(format!("Playback error: {}", err));
        }

        if let Some(err) = self.downloads.take_last_error() {
            self.notify(format!("Download error: {}", err));
        }

        let ended_gen = self.audio.telemetry.track_ended_generation.load(Ordering::Relaxed);
        if ended_gen != 0 && ended_gen == self.current_gen {
            self.audio
                .telemetry
                .track_ended_generation
                .store(0, Ordering::Relaxed);
            self.handle_track_finished();
        }

        // Beat & drop reactivity
        if !self.config.calm_mode && self.config.drop_detection {
            let elapsed_s = self.audio.telemetry.position_ms.load(Ordering::Relaxed) as f64 / 1000.0;
            let drops = self
                .audio
                .telemetry
                .drops
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            for dt in drops {
                let key = (dt * 1000.0) as u64;
                if !self.fired_drops.contains(&key) && elapsed_s >= dt && elapsed_s - dt < 2.0 {
                    self.fired_drops.insert(key);
                    self.drop_until = Some(Instant::now() + Duration::from_secs(10));
                }
            }
            let drop_active = self.drop_until.map_or(false, |u| Instant::now() < u);
            self.theme_mgr.drop_active = drop_active;
            if drop_active {
                let beats = self
                    .audio
                    .telemetry
                    .beats
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                while self.last_beat_idx < beats.len() && elapsed_s >= beats[self.last_beat_idx] {
                    self.theme_mgr.advance_beat();
                    self.last_beat_idx += 1;
                }
            }
        } else {
            self.theme_mgr.drop_active = false;
        }

        // Poll MPRIS events & update MPRIS position periodically
        for mev in self.mpris.poll_events() {
            match mev {
                MediaControlEvent::Play => {
                    if self.current_gen == 0 && self.queue_idx.is_some() {
                        self.start_current_queue_track();
                    } else {
                        self.audio.set_paused(false);
                    }
                }
                MediaControlEvent::Pause => self.audio.set_paused(true),
                MediaControlEvent::Toggle => self.toggle_play_or_resume(),
                MediaControlEvent::Next => self.play_next(),
                MediaControlEvent::Previous => self.play_prev(),
                MediaControlEvent::Stop => self.audio.stop(),
                MediaControlEvent::SetPosition(pos) => {
                    self.audio.seek_to_ms(pos.0.as_millis() as u64);
                }
                _ => {}
            }
        }
        if self.last_mpris_push.elapsed() >= Duration::from_millis(750) {
            self.last_mpris_push = Instant::now();
            let playing = self.audio.telemetry.is_playing.load(Ordering::Relaxed);
            let paused = self.audio.telemetry.is_paused.load(Ordering::Relaxed);
            let pos = self.audio.telemetry.position_ms.load(Ordering::Relaxed);
            self.mpris.update_playback(playing, paused, pos);
            self.sync_discord_rpc(false);
        }

        // Periodically refresh Omarchy/Pywal theme if active
        if self.last_theme_poll.elapsed() >= Duration::from_secs(3) {
            self.last_theme_poll = Instant::now();
            if matches!(self.config.theme_mode, ThemeMode::Omarchy | ThemeMode::Pywal) {
                self.theme_mgr.reload_system_themes();
            }
        }
    }

    fn sync_discord_rpc(&mut self, force: bool) {
        if !self.config.discord_rpc || self.current_gen == 0 {
            if self.last_discord_sync.is_some() {
                self.last_discord_sync = None;
                self.discord.clear();
            }
            return;
        }
        let Some(ref track) = self.current_track else {
            if self.last_discord_sync.is_some() {
                self.last_discord_sync = None;
                self.discord.clear();
            }
            return;
        };

        let is_playing = self.audio.telemetry.is_playing.load(Ordering::Relaxed);
        let is_buffering = self.audio.telemetry.is_buffering.load(Ordering::Relaxed);
        let is_paused =
            self.audio.telemetry.is_paused.load(Ordering::Relaxed) || (!is_playing && !is_buffering);
        let elapsed_s = self.audio.telemetry.position_ms.load(Ordering::Relaxed) / 1000;
        let dur_ms = self.audio.telemetry.duration_ms.load(Ordering::Relaxed);
        let duration_s = if dur_ms > 0 { dur_ms / 1000 } else { track.duration };
        let now_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let start_epoch = now_epoch.saturating_sub(elapsed_s);

        let should_send = match self.last_discord_sync {
            None => true,
            Some((prev_id, prev_paused, prev_start, last_at)) => {
                force
                    || prev_id != track.id
                    || prev_paused != is_paused
                    || (!is_paused
                        && start_epoch.abs_diff(prev_start) >= 4
                        && last_at.elapsed() >= Duration::from_secs(2))
            }
        };

        if should_send {
            self.last_discord_sync = Some((track.id, is_paused, start_epoch, Instant::now()));
            self.discord.update(Some(DiscordPresence {
                track_id: track.id,
                title: track.title.clone(),
                artist: track.artist_name.clone(),
                album: track.album_name.clone(),
                cover_id: track.album_cover.clone(),
                duration_secs: duration_s,
                elapsed_secs: elapsed_s,
                is_paused,
            }));
        }
    }

    fn handle_app_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::SessionLoaded(s) => {
                self.client = Some(TidalClient::new(s.clone()));
                self.session = Some(s);
            }
            AppEvent::ProfileRefreshed(s) => {
                self.session = Some(s.clone());
                if let Some(ref client) = self.client {
                    client.update_session(s);
                }
            }
            AppEvent::HomeLoaded(res) => {
                self.loading = false;
                match res {
                    Ok(sections) => {
                        self.prefetch_home_covers(&sections);
                        if self.current_track.is_none() {
                            for sec in &sections {
                                if let crate::api::HomeItems::Tracks(ref tracks) = sec.items {
                                    if !tracks.is_empty() {
                                        self.cue_tracks_paused(tracks.clone());
                                        break;
                                    }
                                }
                            }
                        }
                        self.home_sections = sections;
                    }
                    Err(e) => self.notify(e),
                }
            }
            AppEvent::SearchLoaded(res) => {
                self.loading = false;
                match res {
                    Ok(sr) => {
                        self.search_results = sr;
                        self.main_selected = 0;
                        self.main_scroll = 0;
                    }
                    Err(e) => self.notify(format!("Search failed: {}", e)),
                }
            }
            AppEvent::LibraryLoaded(res) => {
                self.loading = false;
                match res {
                    Ok(lib) => {
                        if self.current_track.is_none() && !lib.tracks.is_empty() {
                            self.cue_tracks_paused(lib.tracks.clone());
                        }
                        self.library = lib;
                    }
                    Err(e) => self.notify(format!("Library error: {}", e)),
                }
            }
            AppEvent::DetailLoaded(res) => {
                self.loading = false;
                match res {
                    Ok(detail) => {
                        self.prefetch_detail_cover(&detail);
                        if !self.browser_open {
                            if let DetailContent::Radio { ref seed, ref tracks } = detail {
                                if !tracks.is_empty() {
                                    let count = tracks.len();
                                    let title = seed.title.clone();
                                    self.play_queue_from(tracks.clone(), 0);
                                    self.notify(format!("Track Radio ({}): {} tracks", title, count));
                                    return;
                                }
                            }
                        }
                        self.push_view(MainView::Detail(detail));
                    }
                    Err(e) => self.notify(format!("Load failed: {}", e)),
                }
            }
            AppEvent::LyricsLoaded { track_id, state } => {
                if self.current_track.as_ref().map(|t| t.id) == Some(track_id) {
                    self.lyrics_state = state;
                    self.lyrics_scroll = 0;
                }
            }
            AppEvent::CoverLoaded { key, bytes } => {
                if let Some(palette) = self.covers.insert_bytes(&key, &bytes) {
                    let is_current = self
                        .current_track
                        .as_ref()
                        .and_then(|t| t.album_cover.as_deref())
                        == Some(key.as_str());
                    if is_current {
                        self.theme_mgr.set_album_palette(palette);
                        if let Some(ref t) = self.current_track {
                            self.mpris.update_metadata(
                                &t.title,
                                &t.artist_name,
                                &t.album_name,
                                t.duration,
                                Some(&bytes),
                            );
                        }
                    }
                }
            }
            AppEvent::FavoriteToggled { kind, id, is_fav } => {
                match kind {
                    FavoriteKind::Track => {
                        if let Ok(nid) = id.parse::<u64>() {
                            if is_fav {
                                self.library.fav_track_ids.insert(nid);
                            } else {
                                self.library.fav_track_ids.remove(&nid);
                            }
                        }
                    }
                    FavoriteKind::Album => {
                        if let Ok(nid) = id.parse::<u64>() {
                            if is_fav {
                                self.library.fav_album_ids.insert(nid);
                            } else {
                                self.library.fav_album_ids.remove(&nid);
                            }
                        }
                    }
                    FavoriteKind::Artist => {
                        if let Ok(nid) = id.parse::<u64>() {
                            if is_fav {
                                self.library.fav_artist_ids.insert(nid);
                            } else {
                                self.library.fav_artist_ids.remove(&nid);
                            }
                        }
                    }
                    FavoriteKind::Playlist => {
                        if is_fav {
                            self.library.fav_playlist_uuids.insert(id);
                        } else {
                            self.library.fav_playlist_uuids.remove(&id);
                        }
                    }
                }
                self.notify(if is_fav {
                    "♥ Added to Favorites"
                } else {
                    "Removed from Favorites"
                });
            }
            AppEvent::FavoriteError(e) => self.notify(format!("Favorite error: {}", e)),
            AppEvent::LocalFilesScanned(tracks) => {
                if self.current_track.is_none() && self.session.is_none() && !tracks.is_empty() {
                    self.cue_tracks_paused(tracks.clone());
                }
                self.local_tracks = tracks;
            }
            AppEvent::DeviceAuthStarted(res) => match res {
                Ok(state) => {
                    self.auth_modal.loading = false;
                    self.auth_modal.user_code = state.user_code.clone();
                    self.auth_modal.verification_uri = state.verification_uri.clone();
                    self.auth_modal.status = "Waiting for browser authorization...".into();
                    self.auth_modal.error = None;
                    auth::open_browser(&state.verification_uri);
                    self.spawn_device_poll(state);
                }
                Err(e) => {
                    self.auth_modal.loading = false;
                    self.auth_modal.error = Some(e);
                }
            },
            AppEvent::DeviceAuthPolled(res) => {
                if self.modal != ModalState::Auth {
                    return;
                }
                match res {
                    Ok(DevicePollResult::Authorized(session)) => {
                        self.modal = ModalState::None;
                        self.session = Some(session.clone());
                        let client = TidalClient::new(session);
                        self.client = Some(client.clone());
                        self.notify("Signed in to Tidal!");
                        self.refresh_all_data();
                    }
                    Ok(DevicePollResult::Denied) => {
                        self.auth_modal.error = Some("Authorization was denied.".into());
                    }
                    Ok(DevicePollResult::Expired) => {
                        self.auth_modal.error = Some("Login code expired. Press Enter to retry.".into());
                    }
                    Err(e) => {
                        self.auth_modal.error = Some(e);
                    }
                    _ => {}
                }
            }
            AppEvent::Toast(msg) => self.notify(msg),
        }
    }

    fn spawn_device_poll(&self, state: DeviceAuthState) {
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let mut interval = state.interval.max(2);
            let deadline = Instant::now() + Duration::from_secs(state.expires_in);
            loop {
                if Instant::now() > deadline {
                    let _ = tx.send(AppEvent::DeviceAuthPolled(Ok(DevicePollResult::Expired)));
                    return;
                }
                thread::sleep(Duration::from_secs(interval));
                match auth::poll_device_auth_once(&state) {
                    Ok(DevicePollResult::Pending) => continue,
                    Ok(DevicePollResult::SlowDown) => {
                        interval += 2;
                        continue;
                    }
                    other => {
                        let _ = tx.send(AppEvent::DeviceAuthPolled(other.map_err(|e| e.to_string())));
                        return;
                    }
                }
            }
        });
    }

    pub fn open_auth_modal(&mut self) {
        self.modal = ModalState::Auth;
        self.auth_modal = AuthModalState {
            loading: true,
            ..Default::default()
        };
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let res = auth::start_device_auth().map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::DeviceAuthStarted(res));
        });
    }

    pub fn refresh_all_data(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.loading = true;
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            if let Ok(profile) = client.fetch_user_profile() {
                let _ = tx.send(AppEvent::ProfileRefreshed(profile));
            }
            let _ = tx.send(AppEvent::HomeLoaded(
                client.home_feed().map_err(|e| e.to_string()),
            ));
            let _ = tx.send(AppEvent::LibraryLoaded(
                client.library().map_err(|e| e.to_string()),
            ));
        });
    }

    fn prefetch_home_covers(&self, sections: &[HomeSection]) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let mut keys = Vec::new();
        for s in sections.iter().take(3) {
            match &s.items {
                crate::api::HomeItems::Albums(albums) => {
                    for a in albums.iter().take(4) {
                        if let Some(ref c) = a.cover {
                            keys.push(c.clone());
                        }
                    }
                }
                crate::api::HomeItems::Tracks(tracks) => {
                    for t in tracks.iter().take(4) {
                        if let Some(ref c) = t.album_cover {
                            keys.push(c.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        for key in keys {
            if !self.covers.has(&key) {
                self.fetch_cover_async(client.clone(), key);
            }
        }
    }

    fn prefetch_detail_cover(&self, detail: &DetailContent) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let key = match detail {
            DetailContent::Album { album, .. } => album.cover.clone(),
            DetailContent::Playlist { playlist, .. } => playlist.cover.clone(),
            DetailContent::Radio { seed, .. } => seed.album_cover.clone(),
            DetailContent::Artist(ad) => ad.artist.picture.clone(),
            DetailContent::Mix { .. } => None,
        };
        if let Some(k) = key {
            if !self.covers.has(&k) {
                self.fetch_cover_async(client, k);
            }
        }
    }

    fn fetch_cover_async(&self, client: TidalClient, key: String) {
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            if let Ok(bytes) = client.fetch_cover(&key, 640) {
                let _ = tx.send(AppEvent::CoverLoaded { key, bytes });
            }
        });
    }

    pub fn push_view(&mut self, view: MainView) {
        let current = std::mem::replace(&mut self.view, view);
        self.view_history.push(current);
        self.main_selected = 0;
        self.main_scroll = 0;
        self.filter_active = false;
        self.filter_query.clear();
        self.focus = FocusPane::Main;
    }

    pub fn switch_sidebar(&mut self, idx: usize) {
        let idx = idx.min(SidebarSection::ALL.len() - 1);
        self.sidebar_idx = idx;
        self.view_history.clear();
        self.main_selected = 0;
        self.main_scroll = 0;
        self.filter_active = false;
        self.filter_query.clear();
        self.view = match SidebarSection::ALL[idx] {
            SidebarSection::Home => MainView::Home,
            SidebarSection::Search => MainView::Search,
            SidebarSection::LikedTracks => MainView::LikedTracks,
            SidebarSection::SavedAlbums => MainView::SavedAlbums,
            SidebarSection::Artists => MainView::Artists,
            SidebarSection::Mixes => MainView::Mixes,
            SidebarSection::Playlists => MainView::Playlists,
            SidebarSection::Queue => MainView::Queue,
            SidebarSection::LocalFiles => {
                let (bytes, count) = audio::cache_stats();
                self.cache_bytes = bytes;
                self.cache_count = count;
                MainView::LocalFiles
            }
            SidebarSection::Settings => {
                self.output_devices = audio::list_output_devices();
                let (bytes, count) = audio::cache_stats();
                self.cache_bytes = bytes;
                self.cache_count = count;
                MainView::Settings
            }
        };
    }

    pub fn trigger_search(&mut self, query: String) {
        let q = query.trim().to_string();
        if q.is_empty() {
            return;
        }
        self.search_query = q.clone();
        self.sidebar_idx = 1;
        self.view = MainView::Search;
        self.browser_open = true;
        self.focus = FocusPane::Main;
        let Some(client) = self.client.clone() else {
            self.notify("Sign in first (Ctrl+L) to search Tidal");
            return;
        };
        self.loading = true;
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let res = client.search_all(&q, 30).map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::SearchLoaded(res));
        });
    }

    pub fn cue_tracks_paused(&mut self, tracks: Vec<TrackInfo>) {
        if tracks.is_empty() {
            return;
        }
        let first = tracks[0].clone();
        self.queue = tracks;
        self.queue_idx = Some(0);
        self.current_track = Some(first.clone());
        self.audio.telemetry.is_paused.store(true, Ordering::Relaxed);
        self.prefetch_track_meta(&first);
    }

    pub fn toggle_play_or_resume(&mut self) {
        if self.current_gen == 0 {
            if self.queue_idx.is_some() {
                self.start_current_queue_track();
            } else if !self.library.tracks.is_empty() {
                let tracks = self.library.tracks.clone();
                self.play_queue_from(tracks, 0);
            } else if !self.local_tracks.is_empty() {
                let tracks = self.local_tracks.clone();
                self.play_queue_from(tracks, 0);
            }
        } else {
            self.audio.toggle_pause();
        }
    }

    fn prefetch_track_meta(&mut self, track: &TrackInfo) {
        if let Some(ref local) = track.local_path {
            let path = PathBuf::from(local);
            let (_, cover_bytes) = metadata::read_local_track(&path);
            if let Some(ref bytes) = cover_bytes {
                let key = format!("local:{}", local);
                if let Some(pal) = self.covers.insert_bytes(&key, bytes) {
                    self.theme_mgr.set_album_palette(pal);
                }
            }
            self.lyrics_state = LyricsState::Unavailable;
            self.mpris.update_metadata(
                &track.title,
                &track.artist_name,
                &track.album_name,
                track.duration,
                cover_bytes.as_deref(),
            );
            return;
        }

        let Some(client) = self.client.clone() else {
            return;
        };
        if let Some(ref cover_id) = track.album_cover {
            if let Some(cached) = self.covers.cache.get(cover_id) {
                self.theme_mgr.set_album_palette(cached.palette.clone());
            } else {
                self.fetch_cover_async(client.clone(), cover_id.clone());
            }
        }
        self.mpris.update_metadata(
            &track.title,
            &track.artist_name,
            &track.album_name,
            track.duration,
            None,
        );
        self.lyrics_state = LyricsState::Loading;
        let tx = self.event_tx.clone();
        let tid = track.id;
        thread::spawn(move || {
            let state = match client.lyrics(tid).ok().and_then(|raw| lyrics::parse(&raw)) {
                Some(data) => LyricsState::Loaded(data),
                None => LyricsState::Unavailable,
            };
            let _ = tx.send(AppEvent::LyricsLoaded {
                track_id: tid,
                state,
            });
        });
    }

    pub fn play_queue_from(&mut self, tracks: Vec<TrackInfo>, index: usize) {
        if tracks.is_empty() {
            return;
        }
        let idx = index.min(tracks.len() - 1);
        self.queue = tracks;
        self.queue_idx = Some(idx);
        self.browser_open = false;
        self.start_current_queue_track();
    }

    pub fn play_single_track(&mut self, track: TrackInfo) {
        self.play_queue_from(vec![track], 0);
    }

    pub fn start_current_queue_track(&mut self) {
        let Some(idx) = self.queue_idx else {
            return;
        };
        let Some(track) = self.queue.get(idx).cloned() else {
            return;
        };
        self.current_track = Some(track.clone());
        self.fired_drops.clear();
        self.last_beat_idx = 0;
        self.drop_until = None;
        self.lyrics_scroll = 0;

        if let Some(ref local) = track.local_path {
            let path = PathBuf::from(local);
            let (_, cover_bytes) = metadata::read_local_track(&path);
            if let Some(ref bytes) = cover_bytes {
                let key = format!("local:{}", local);
                if let Some(pal) = self.covers.insert_bytes(&key, bytes) {
                    self.theme_mgr.set_album_palette(pal);
                }
            }
            self.lyrics_state = LyricsState::Unavailable;
            self.mpris.update_metadata(
                &track.title,
                &track.artist_name,
                &track.album_name,
                track.duration,
                cover_bytes.as_deref(),
            );
            self.current_gen =
                self.audio
                    .play_local(path, track, self.config.drop_detection && !self.config.calm_mode);
            self.sync_discord_rpc(true);
            return;
        }

        let Some(client) = self.client.clone() else {
            self.notify("Sign in with Ctrl+L to stream Tidal tracks");
            return;
        };

        self.current_gen = self.audio.play_tidal(
            track.clone(),
            client.clone(),
            self.config.audio_quality,
            self.config.cache_enabled,
            self.config.drop_detection && !self.config.calm_mode,
        );

        // Fetch cover art if not cached
        if let Some(ref cover_id) = track.album_cover {
            if let Some(cached) = self.covers.cache.get(cover_id) {
                self.theme_mgr.set_album_palette(cached.palette.clone());
            } else {
                self.fetch_cover_async(client.clone(), cover_id.clone());
            }
        }
        self.mpris.update_metadata(
            &track.title,
            &track.artist_name,
            &track.album_name,
            track.duration,
            None,
        );

        // Fetch lyrics asynchronously
        self.lyrics_state = LyricsState::Loading;
        let tx = self.event_tx.clone();
        let tid = track.id;
        thread::spawn(move || {
            let state = match client.lyrics(tid).ok().and_then(|raw| lyrics::parse(&raw)) {
                Some(data) => LyricsState::Loaded(data),
                None => LyricsState::Unavailable,
            };
            let _ = tx.send(AppEvent::LyricsLoaded {
                track_id: tid,
                state,
            });
        });
        self.sync_discord_rpc(true);
    }

    pub fn handle_track_finished(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        if self.repeat == RepeatMode::One {
            self.start_current_queue_track();
            return;
        }
        self.play_next_internal(true);
    }

    pub fn play_next(&mut self) {
        self.play_next_internal(false);
    }

    fn play_next_internal(&mut self, from_end: bool) {
        if self.queue.is_empty() {
            return;
        }
        let cur = self.queue_idx.unwrap_or(0);
        let next = if self.shuffle && self.queue.len() > 1 {
            let mut rng = rand::thread_rng();
            let candidates: Vec<usize> = (0..self.queue.len()).filter(|&i| i != cur).collect();
            *candidates.choose(&mut rng).unwrap_or(&0)
        } else if cur + 1 < self.queue.len() {
            cur + 1
        } else if self.repeat == RepeatMode::All || (!from_end && self.queue.len() == 1) {
            0
        } else {
            self.audio.stop();
            return;
        };
        self.queue_idx = Some(next);
        self.start_current_queue_track();
    }

    pub fn play_prev(&mut self) {
        let pos_ms = self.audio.telemetry.position_ms.load(Ordering::Relaxed);
        if pos_ms > 3000 {
            self.audio.seek_to_ms(0);
            return;
        }
        if self.queue.is_empty() {
            return;
        }
        let cur = self.queue_idx.unwrap_or(0);
        let prev = if cur > 0 {
            cur - 1
        } else if self.repeat == RepeatMode::All {
            self.queue.len() - 1
        } else {
            0
        };
        self.queue_idx = Some(prev);
        self.start_current_queue_track();
    }

    pub fn append_to_queue(&mut self, track: TrackInfo) {
        self.queue.push(track.clone());
        if self.queue_idx.is_none() {
            self.queue_idx = Some(0);
            self.start_current_queue_track();
        } else {
            self.notify(format!("Added to queue: {}", track.title));
        }
    }

    pub fn insert_play_next(&mut self, track: TrackInfo) {
        if let Some(idx) = self.queue_idx {
            self.queue.insert(idx + 1, track.clone());
            self.notify(format!("Playing next: {}", track.title));
        } else {
            self.play_single_track(track);
        }
    }

    pub fn open_album(&mut self, album_id: u64) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.loading = true;
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let res = client
                .album_detail(album_id)
                .map(|(album, tracks)| DetailContent::Album { album, tracks })
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::DetailLoaded(res));
        });
    }

    pub fn open_playlist(&mut self, uuid: String) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.loading = true;
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let res = client
                .playlist_detail(&uuid)
                .map(|(playlist, tracks)| DetailContent::Playlist { playlist, tracks })
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::DetailLoaded(res));
        });
    }

    pub fn open_mix(&mut self, mix: MixInfo) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.loading = true;
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let res = client
                .mix_tracks(&mix.id)
                .map(|tracks| DetailContent::Mix { mix, tracks })
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::DetailLoaded(res));
        });
    }

    pub fn open_artist(&mut self, artist_id: u64) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.loading = true;
        self.artist_sub_tab = 0;
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let res = client
                .artist_detail(artist_id)
                .map(DetailContent::Artist)
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::DetailLoaded(res));
        });
    }

    pub fn start_radio(&mut self, seed: TrackInfo) {
        if seed.id == 0 {
            self.notify("Track radio requires a Tidal track");
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self.loading = true;
        self.notify(format!("Starting radio from {}...", seed.title));
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let res = client
                .track_radio(seed.id)
                .map(|tracks| DetailContent::Radio { seed, tracks })
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::DetailLoaded(res));
        });
    }

    pub fn toggle_favorite_item(&mut self, kind: FavoriteKind, id: String) {
        let Some(client) = self.client.clone() else {
            self.notify("Sign in (Ctrl+L) to save favorites");
            return;
        };
        let currently_fav = match kind {
            FavoriteKind::Track => id
                .parse::<u64>()
                .map(|n| self.library.fav_track_ids.contains(&n))
                .unwrap_or(false),
            FavoriteKind::Album => id
                .parse::<u64>()
                .map(|n| self.library.fav_album_ids.contains(&n))
                .unwrap_or(false),
            FavoriteKind::Artist => id
                .parse::<u64>()
                .map(|n| self.library.fav_artist_ids.contains(&n))
                .unwrap_or(false),
            FavoriteKind::Playlist => self.library.fav_playlist_uuids.contains(&id),
        };
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            match client.toggle_favorite(kind, &id, currently_fav) {
                Ok(is_fav) => {
                    let _ = tx.send(AppEvent::FavoriteToggled { kind, id, is_fav });
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::FavoriteError(e.to_string()));
                }
            }
        });
    }

    pub fn download_tracks(&mut self, tracks: Vec<TrackInfo>) {
        let Some(client) = self.client.clone() else {
            self.notify("Sign in (Ctrl+L) to download tracks");
            return;
        };
        let added = self.downloads.push_tracks(
            tracks,
            client,
            self.config.audio_quality,
            self.config.resolved_download_dir(),
        );
        if added > 0 {
            self.notify(format!(
                "Queued {} track(s) for download → {}",
                added,
                self.config.resolved_download_dir()
            ));
        } else {
            self.notify("Already downloaded");
        }
    }

    pub fn change_volume(&mut self, delta: f32) {
        self.config.volume = (self.config.volume + delta).clamp(0.0, 1.0);
        self.audio.set_volume(self.config.volume);
        let _ = config::save(&self.config);
    }

    pub fn cycle_theme(&mut self) {
        self.config.theme_mode = self.config.theme_mode.next();
        self.theme_mgr.mode = self.config.theme_mode;
        self.theme_mgr.reload_system_themes();
        let _ = config::save(&self.config);
        self.notify(format!("Theme: {}", self.config.theme_mode.label()));
    }

    pub fn filter_tracks<'a>(&self, tracks: &'a [TrackInfo]) -> Vec<(usize, &'a TrackInfo)> {
        if self.filter_query.trim().is_empty() {
            return tracks.iter().enumerate().collect();
        }
        let matcher = SkimMatcherV2::default();
        let q = self.filter_query.trim();
        let mut hits: Vec<(i64, usize, &'a TrackInfo)> = tracks
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                let hay = format!("{} {} {}", t.title, t.artist_name, t.album_name);
                matcher.fuzzy_match(&hay, q).map(|score| (score, i, t))
            })
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0));
        hits.into_iter().map(|(_, i, t)| (i, t)).collect()
    }

    pub fn main_item_count(&self) -> usize {
        match &self.view {
            MainView::Home => self
                .home_sections
                .get(self.home_section_idx)
                .map(|s| match &s.items {
                    crate::api::HomeItems::Tracks(v) => v.len(),
                    crate::api::HomeItems::Albums(v) => v.len(),
                    crate::api::HomeItems::Playlists(v) => v.len(),
                    crate::api::HomeItems::Mixes(v) => v.len(),
                })
                .unwrap_or(0),
            MainView::Search => match self.search_tab {
                SearchTab::Tracks => self.filter_tracks(&self.search_results.tracks).len(),
                SearchTab::Albums => self.search_results.albums.len(),
                SearchTab::Artists => self.search_results.artists.len(),
                SearchTab::Playlists => self.search_results.playlists.len(),
            },
            MainView::LikedTracks => self.filter_tracks(&self.library.tracks).len(),
            MainView::SavedAlbums => self.library.albums.len(),
            MainView::Artists => self.library.artists.len(),
            MainView::Mixes => self.library.mixes.len(),
            MainView::Playlists => self.library.playlists.len(),
            MainView::Queue => self.filter_tracks(&self.queue).len(),
            MainView::LocalFiles => self.filter_tracks(&self.local_tracks).len(),
            MainView::Settings => 10 + 1 + self.output_devices.len(),
            MainView::Detail(d) => match d {
                DetailContent::Album { tracks, .. }
                | DetailContent::Playlist { tracks, .. }
                | DetailContent::Mix { tracks, .. }
                | DetailContent::Radio { tracks, .. } => self.filter_tracks(tracks).len(),
                DetailContent::Artist(ad) => {
                    if self.artist_sub_tab == 0 {
                        self.filter_tracks(&ad.top_tracks).len()
                    } else {
                        ad.albums.len()
                    }
                }
            },
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('f') {
            self.browser_open = true;
            self.switch_sidebar(1);
            self.focus = FocusPane::SearchBar;
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('l') {
            self.open_auth_modal();
            return;
        }

        // Modal handling
        if self.modal != ModalState::None {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('H') | KeyCode::Char('?') => {
                    self.modal = ModalState::None;
                }
                KeyCode::Enter if self.modal == ModalState::Auth => {
                    self.open_auth_modal();
                }
                _ => {}
            }
            return;
        }

        // Search bar input mode
        if self.focus == FocusPane::SearchBar {
            match key.code {
                KeyCode::Esc => {
                    self.focus = FocusPane::Main;
                }
                KeyCode::Enter => {
                    let q = self.search_query.clone();
                    self.trigger_search(q);
                }
                KeyCode::Backspace => {
                    self.search_query.pop();
                }
                KeyCode::Char(c) => {
                    self.search_query.push(c);
                }
                _ => {}
            }
            return;
        }

        // Filter bar input mode
        if self.filter_active {
            match key.code {
                KeyCode::Esc => {
                    self.filter_active = false;
                    self.filter_query.clear();
                    self.main_selected = 0;
                }
                KeyCode::Enter => {
                    self.filter_active = false;
                }
                KeyCode::Backspace => {
                    self.filter_query.pop();
                    self.main_selected = 0;
                }
                KeyCode::Char(c) => {
                    self.filter_query.push(c);
                    self.main_selected = 0;
                }
                _ => {}
            }
            return;
        }

        // Global shortcuts (valid in both Now Playing and Browser overlay)
        match key.code {
            KeyCode::Char('Q') => {
                self.should_quit = true;
                return;
            }
            KeyCode::Char('H') => {
                self.modal = ModalState::Help;
                return;
            }
            KeyCode::Char('?') => {
                if self.browser_open {
                    self.modal = ModalState::Help;
                } else {
                    self.show_controls = !self.show_controls;
                }
                return;
            }
            KeyCode::Char('/') => {
                self.browser_open = true;
                self.switch_sidebar(1);
                self.focus = FocusPane::SearchBar;
                return;
            }
            KeyCode::Char('b') => {
                self.browser_open = !self.browser_open;
                self.focus = FocusPane::Main;
                return;
            }
            KeyCode::Char('u') => {
                if self.browser_open && matches!(self.view, MainView::Queue) {
                    self.browser_open = false;
                } else {
                    self.browser_open = true;
                    self.switch_sidebar(7);
                    self.focus = FocusPane::Main;
                }
                return;
            }
            KeyCode::Char(' ') => {
                self.toggle_play_or_resume();
                return;
            }
            KeyCode::Char('t') => {
                self.cycle_theme();
                return;
            }
            KeyCode::Char('s') => {
                self.shuffle = !self.shuffle;
                self.notify(if self.shuffle { "Shuffle: ON" } else { "Shuffle: OFF" });
                return;
            }
            KeyCode::Char('R') => {
                self.repeat = self.repeat.next();
                self.notify(format!("Repeat: {}", self.repeat.label()));
                return;
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.change_volume(0.05);
                return;
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.change_volume(-0.05);
                return;
            }
            KeyCode::Char('>') | KeyCode::Char('N') => {
                self.play_next();
                return;
            }
            KeyCode::Char('<') | KeyCode::Char('P') => {
                self.play_prev();
                return;
            }
            KeyCode::Char('.') => {
                self.audio.seek_relative_ms(10_000);
                return;
            }
            KeyCode::Char(',') => {
                self.audio.seek_relative_ms(-10_000);
                return;
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.audio.seek_relative_ms(10_000);
                return;
            }
            KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.audio.seek_relative_ms(-10_000);
                return;
            }
            KeyCode::Tab => {
                if !self.browser_open {
                    self.browser_open = true;
                    self.focus = FocusPane::Main;
                } else {
                    let next = (self.sidebar_idx + 1) % SidebarSection::ALL.len();
                    self.switch_sidebar(next);
                }
                return;
            }
            KeyCode::BackTab => {
                if self.browser_open {
                    let prev = if self.sidebar_idx == 0 {
                        SidebarSection::ALL.len() - 1
                    } else {
                        self.sidebar_idx - 1
                    };
                    self.switch_sidebar(prev);
                }
                return;
            }
            KeyCode::Esc => {
                if self.browser_open {
                    if let Some(prev) = self.view_history.pop() {
                        self.view = prev;
                        self.main_selected = 0;
                        self.main_scroll = 0;
                    } else {
                        self.browser_open = false;
                    }
                } else if self.show_controls {
                    self.show_controls = false;
                }
                return;
            }
            _ => {}
        }

        // Minimalist Now Playing keybindings when Browser overlay is closed
        if !self.browser_open {
            match key.code {
                KeyCode::Char('q') => self.should_quit = true,
                KeyCode::Char('n') | KeyCode::Right => self.play_next(),
                KeyCode::Char('p') | KeyCode::Left => self.play_prev(),
                KeyCode::Up => self.change_volume(0.05),
                KeyCode::Down => self.change_volume(-0.05),
                KeyCode::Char('j') | KeyCode::PageDown => {
                    self.lyrics_scroll = self.lyrics_scroll.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::PageUp => {
                    self.lyrics_scroll = self.lyrics_scroll.saturating_sub(1);
                }
                KeyCode::Char('l') | KeyCode::Char('L') => {
                    self.show_lyrics = !self.show_lyrics;
                    self.notify(if self.show_lyrics { "Lyrics: ON" } else { "Lyrics: OFF" });
                }
                KeyCode::Char('r') => {
                    if let Some(t) = self.current_track.clone() {
                        self.start_radio(t);
                    }
                }
                KeyCode::Char('m') => {
                    if let Some(ref t) = self.current_track {
                        if t.id > 0 {
                            self.toggle_favorite_item(FavoriteKind::Track, t.id.to_string());
                        }
                    }
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    if let Some(t) = self.current_track.clone() {
                        self.download_tracks(vec![t]);
                    }
                }
                _ => {}
            }
            return;
        }

        // Browser overlay keybindings
        match key.code {
            KeyCode::Char('q') => {
                self.browser_open = false;
                return;
            }
            KeyCode::Char('f') => {
                self.filter_active = true;
                self.filter_query.clear();
                self.main_selected = 0;
                return;
            }
            KeyCode::Backspace => {
                if let Some(prev) = self.view_history.pop() {
                    self.view = prev;
                    self.main_selected = 0;
                    self.main_scroll = 0;
                } else {
                    self.browser_open = false;
                }
                return;
            }
            _ => {}
        }

        self.handle_main_key(key);
    }

    fn handle_main_key(&mut self, key: KeyEvent) {
        if matches!(self.view, MainView::Home) {
            self.handle_home_key(key);
            return;
        }

        if matches!(self.view, MainView::Queue) {
            let filtered = self.filter_tracks(&self.queue);
            let len = filtered.len();
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    if len > 0 && self.main_selected + 1 < len {
                        self.main_selected += 1;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.main_selected = self.main_selected.saturating_sub(1);
                }
                KeyCode::Char('J') if self.filter_query.is_empty() => {
                    if self.main_selected + 1 < self.queue.len() {
                        self.queue.swap(self.main_selected, self.main_selected + 1);
                        if self.queue_idx == Some(self.main_selected) {
                            self.queue_idx = Some(self.main_selected + 1);
                        } else if self.queue_idx == Some(self.main_selected + 1) {
                            self.queue_idx = Some(self.main_selected);
                        }
                        self.main_selected += 1;
                    }
                }
                KeyCode::Char('K') if self.filter_query.is_empty() => {
                    if self.main_selected > 0 && self.main_selected < self.queue.len() {
                        self.queue.swap(self.main_selected, self.main_selected - 1);
                        if self.queue_idx == Some(self.main_selected) {
                            self.queue_idx = Some(self.main_selected - 1);
                        } else if self.queue_idx == Some(self.main_selected - 1) {
                            self.queue_idx = Some(self.main_selected);
                        }
                        self.main_selected -= 1;
                    }
                }
                KeyCode::Char('x') | KeyCode::Delete => {
                    if let Some(&(orig_idx, _)) = filtered.get(self.main_selected) {
                        if orig_idx < self.queue.len() {
                            self.queue.remove(orig_idx);
                            if let Some(qi) = self.queue_idx {
                                if orig_idx < qi {
                                    self.queue_idx = Some(qi - 1);
                                } else if orig_idx == qi {
                                    self.queue_idx = None;
                                }
                            }
                            if self.main_selected >= self.queue.len() && !self.queue.is_empty() {
                                self.main_selected = self.queue.len() - 1;
                            }
                        }
                    }
                }
                KeyCode::Char('C') => {
                    if let Some(qi) = self.queue_idx {
                        if let Some(cur) = self.queue.get(qi).cloned() {
                            self.queue = vec![cur];
                            self.queue_idx = Some(0);
                            self.main_selected = 0;
                        }
                    } else {
                        self.queue.clear();
                        self.main_selected = 0;
                    }
                    self.notify("Queue cleared");
                }
                KeyCode::Enter => self.activate_main_selected(),
                KeyCode::Char('m') => self.favorite_selected(),
                KeyCode::Char('d') => self.download_selected(),
                _ => {}
            }
            return;
        }

        let count = self.main_item_count();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if count > 0 && self.main_selected + 1 < count {
                    self.main_selected += 1;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.main_selected = self.main_selected.saturating_sub(1);
            }
            KeyCode::PageDown => {
                if count > 0 {
                    self.main_selected = (self.main_selected + 10).min(count - 1);
                }
            }
            KeyCode::PageUp => {
                self.main_selected = self.main_selected.saturating_sub(10);
            }
            KeyCode::Char(']') => self.cycle_sub_tab(true),
            KeyCode::Char('[') => self.cycle_sub_tab(false),
            KeyCode::Char('1') => self.switch_tab_num(0),
            KeyCode::Char('2') => self.switch_tab_num(1),
            KeyCode::Char('3') => self.switch_tab_num(2),
            KeyCode::Char('4') => self.switch_tab_num(3),
            KeyCode::Enter => self.activate_main_selected(),
            KeyCode::Char('a') => {
                if let Some(t) = self.selected_track() {
                    self.append_to_queue(t);
                }
            }
            KeyCode::Char('i') => {
                if let Some(t) = self.selected_track() {
                    self.insert_play_next(t);
                }
            }
            KeyCode::Char('r') => {
                if let Some(t) = self.selected_track().or_else(|| self.current_track.clone()) {
                    self.start_radio(t);
                }
            }
            KeyCode::Char('m') => self.favorite_selected(),
            KeyCode::Char('d') => self.download_selected(),
            KeyCode::Char('S') => {
                if let MainView::Detail(ref d) = self.view {
                    let tracks = match d {
                        DetailContent::Album { tracks, .. }
                        | DetailContent::Playlist { tracks, .. }
                        | DetailContent::Mix { tracks, .. }
                        | DetailContent::Radio { tracks, .. } => tracks.clone(),
                        DetailContent::Artist(ad) => ad.top_tracks.clone(),
                    };
                    if !tracks.is_empty() {
                        self.shuffle = true;
                        let mut shuffled = tracks;
                        shuffled.shuffle(&mut rand::thread_rng());
                        self.play_queue_from(shuffled, 0);
                    }
                }
            }
            KeyCode::Char('n') => self.play_next(),
            KeyCode::Char('p') => self.play_prev(),
            _ => {}
        }
    }

    fn handle_home_key(&mut self, key: KeyEvent) {
        let num_sections = self.home_sections.len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                let count = self.main_item_count();
                if count > 0 && self.home_item_idx + 1 < count {
                    self.home_item_idx += 1;
                } else if self.home_section_idx + 1 < num_sections {
                    self.home_section_idx += 1;
                    self.home_item_idx = 0;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.home_item_idx > 0 {
                    self.home_item_idx -= 1;
                } else if self.home_section_idx > 0 {
                    self.home_section_idx -= 1;
                    self.home_item_idx = 0;
                }
            }
            KeyCode::Char(']') | KeyCode::Right | KeyCode::Char('l') => {
                if self.home_section_idx + 1 < num_sections {
                    self.home_section_idx += 1;
                    self.home_item_idx = 0;
                }
            }
            KeyCode::Char('[') | KeyCode::Left | KeyCode::Char('h') => {
                if self.home_section_idx > 0 {
                    self.home_section_idx -= 1;
                    self.home_item_idx = 0;
                }
            }
            KeyCode::Enter => self.activate_home_selected(),
            KeyCode::Char('a') => {
                if let Some(t) = self.selected_track() {
                    self.append_to_queue(t);
                }
            }
            KeyCode::Char('r') => {
                if let Some(t) = self.selected_track() {
                    self.start_radio(t);
                }
            }
            KeyCode::Char('d') => self.download_selected(),
            _ => {}
        }
    }

    fn cycle_sub_tab(&mut self, forward: bool) {
        match self.view {
            MainView::Search => {
                self.search_tab = if forward {
                    self.search_tab.next()
                } else {
                    self.search_tab.prev()
                };
                self.main_selected = 0;
                self.main_scroll = 0;
            }
            MainView::Detail(DetailContent::Artist(_)) => {
                self.artist_sub_tab = if self.artist_sub_tab == 0 { 1 } else { 0 };
                self.main_selected = 0;
                self.main_scroll = 0;
            }
            _ => {}
        }
    }

    fn switch_tab_num(&mut self, idx: usize) {
        match self.view {
            MainView::Search if idx < SearchTab::ALL.len() => {
                self.search_tab = SearchTab::ALL[idx];
                self.main_selected = 0;
                self.main_scroll = 0;
            }
            MainView::Detail(DetailContent::Artist(_)) if idx < 2 => {
                self.artist_sub_tab = idx;
                self.main_selected = 0;
                self.main_scroll = 0;
            }
            _ => {}
        }
    }

    fn selected_track(&self) -> Option<TrackInfo> {
        match &self.view {
            MainView::Home => {
                let sec = self.home_sections.get(self.home_section_idx)?;
                if let crate::api::HomeItems::Tracks(v) = &sec.items {
                    return v.get(self.home_item_idx).cloned();
                }
                None
            }
            MainView::Search if self.search_tab == SearchTab::Tracks => {
                let filtered = self.filter_tracks(&self.search_results.tracks);
                filtered.get(self.main_selected).map(|(_, t)| (*t).clone())
            }
            MainView::LikedTracks => {
                let filtered = self.filter_tracks(&self.library.tracks);
                filtered.get(self.main_selected).map(|(_, t)| (*t).clone())
            }
            MainView::Queue => {
                let filtered = self.filter_tracks(&self.queue);
                filtered.get(self.main_selected).map(|(_, t)| (*t).clone())
            }
            MainView::LocalFiles => {
                let filtered = self.filter_tracks(&self.local_tracks);
                filtered.get(self.main_selected).map(|(_, t)| (*t).clone())
            }
            MainView::Detail(d) => match d {
                DetailContent::Album { tracks, .. }
                | DetailContent::Playlist { tracks, .. }
                | DetailContent::Mix { tracks, .. }
                | DetailContent::Radio { tracks, .. } => {
                    let filtered = self.filter_tracks(tracks);
                    filtered.get(self.main_selected).map(|(_, t)| (*t).clone())
                }
                DetailContent::Artist(ad) if self.artist_sub_tab == 0 => {
                    let filtered = self.filter_tracks(&ad.top_tracks);
                    filtered.get(self.main_selected).map(|(_, t)| (*t).clone())
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn activate_home_selected(&mut self) {
        let Some(sec) = self.home_sections.get(self.home_section_idx).cloned() else {
            return;
        };
        match sec.items {
            crate::api::HomeItems::Tracks(tracks) => {
                if self.home_item_idx < tracks.len() {
                    self.play_queue_from(tracks, self.home_item_idx);
                }
            }
            crate::api::HomeItems::Albums(albums) => {
                if let Some(a) = albums.get(self.home_item_idx) {
                    self.open_album(a.id);
                }
            }
            crate::api::HomeItems::Playlists(pls) => {
                if let Some(p) = pls.get(self.home_item_idx) {
                    self.open_playlist(p.uuid.clone());
                }
            }
            crate::api::HomeItems::Mixes(mixes) => {
                if let Some(m) = mixes.get(self.home_item_idx) {
                    self.open_mix(m.clone());
                }
            }
        }
    }

    pub fn activate_main_selected(&mut self) {
        match self.view.clone() {
            MainView::Home => self.activate_home_selected(),
            MainView::Search => match self.search_tab {
                SearchTab::Tracks => {
                    let filtered = self.filter_tracks(&self.search_results.tracks);
                    if let Some(&(orig_idx, _)) = filtered.get(self.main_selected) {
                        let list = self.search_results.tracks.clone();
                        self.play_queue_from(list, orig_idx);
                    }
                }
                SearchTab::Albums => {
                    if let Some(a) = self.search_results.albums.get(self.main_selected) {
                        self.open_album(a.id);
                    }
                }
                SearchTab::Artists => {
                    if let Some(a) = self.search_results.artists.get(self.main_selected) {
                        self.open_artist(a.id);
                    }
                }
                SearchTab::Playlists => {
                    if let Some(p) = self.search_results.playlists.get(self.main_selected) {
                        self.open_playlist(p.uuid.clone());
                    }
                }
            },
            MainView::LikedTracks => {
                let filtered = self.filter_tracks(&self.library.tracks);
                if let Some(&(orig_idx, _)) = filtered.get(self.main_selected) {
                    let list = self.library.tracks.clone();
                    self.play_queue_from(list, orig_idx);
                }
            }
            MainView::SavedAlbums => {
                if let Some(a) = self.library.albums.get(self.main_selected) {
                    self.open_album(a.id);
                }
            }
            MainView::Artists => {
                if let Some(a) = self.library.artists.get(self.main_selected) {
                    self.open_artist(a.id);
                }
            }
            MainView::Mixes => {
                if let Some(m) = self.library.mixes.get(self.main_selected).cloned() {
                    self.open_mix(m);
                }
            }
            MainView::Playlists => {
                if let Some(p) = self.library.playlists.get(self.main_selected) {
                    self.open_playlist(p.uuid.clone());
                }
            }
            MainView::Queue => {
                let filtered = self.filter_tracks(&self.queue);
                if let Some(&(orig_idx, _)) = filtered.get(self.main_selected) {
                    self.queue_idx = Some(orig_idx);
                    self.browser_open = false;
                    self.start_current_queue_track();
                }
            }
            MainView::LocalFiles => {
                let filtered = self.filter_tracks(&self.local_tracks);
                if let Some(&(orig_idx, _)) = filtered.get(self.main_selected) {
                    let list = self.local_tracks.clone();
                    self.play_queue_from(list, orig_idx);
                }
            }
            MainView::Detail(detail) => match detail {
                DetailContent::Album { tracks, .. }
                | DetailContent::Playlist { tracks, .. }
                | DetailContent::Mix { tracks, .. }
                | DetailContent::Radio { tracks, .. } => {
                    let filtered = self.filter_tracks(&tracks);
                    if let Some(&(orig_idx, _)) = filtered.get(self.main_selected) {
                        self.play_queue_from(tracks, orig_idx);
                    }
                }
                DetailContent::Artist(ad) => {
                    if self.artist_sub_tab == 0 {
                        let filtered = self.filter_tracks(&ad.top_tracks);
                        if let Some(&(orig_idx, _)) = filtered.get(self.main_selected) {
                            self.play_queue_from(ad.top_tracks, orig_idx);
                        }
                    } else if let Some(album) = ad.albums.get(self.main_selected) {
                        self.open_album(album.id);
                    }
                }
            },
            MainView::Settings => self.activate_settings_row(),
        }
    }

    fn activate_settings_row(&mut self) {
        match self.main_selected {
            0 => {
                self.config.audio_quality = self.config.audio_quality.next();
                let _ = config::save(&self.config);
                self.notify(format!("Quality: {}", self.config.audio_quality.label()));
            }
            1 => self.cycle_theme(),
            2 => {
                self.show_lyrics = !self.show_lyrics;
                self.notify(if self.show_lyrics { "Lyrics: ON" } else { "Lyrics: OFF" });
            }
            3 => {
                self.config.image_cover = !self.config.image_cover;
                let _ = config::save(&self.config);
            }
            4 => {
                self.config.cache_enabled = !self.config.cache_enabled;
                let _ = config::save(&self.config);
            }
            5 => {
                self.config.drop_detection = !self.config.drop_detection;
                let _ = config::save(&self.config);
            }
            6 => {
                self.config.calm_mode = !self.config.calm_mode;
                let _ = config::save(&self.config);
            }
            7 => {
                self.config.discord_rpc = !self.config.discord_rpc;
                let _ = config::save(&self.config);
                self.notify(if self.config.discord_rpc {
                    "Discord Rich Presence: ON"
                } else {
                    "Discord Rich Presence: OFF"
                });
                self.sync_discord_rpc(true);
            }
            8 => {
                if let Err(e) = audio::clear_cache() {
                    self.notify(format!("Cache clear failed: {}", e));
                } else {
                    self.cache_bytes = 0;
                    self.cache_count = 0;
                    self.notify("Audio cache cleared");
                }
            }
            9 => {
                if self.session.is_some() {
                    let _ = auth::clear_session();
                    self.session = None;
                    self.client = None;
                    self.notify("Signed out of Tidal");
                } else {
                    self.open_auth_modal();
                }
            }
            10 => {
                self.config.output_device = None;
                self.audio.set_output_device(None);
                let _ = config::save(&self.config);
                self.notify("Audio output: System Default");
            }
            idx => {
                let dev_idx = idx.saturating_sub(11);
                if let Some(dev_name) = self.output_devices.get(dev_idx).cloned() {
                    self.config.output_device = Some(dev_name.clone());
                    self.audio.set_output_device(Some(dev_name.clone()));
                    let _ = config::save(&self.config);
                    self.notify(format!("Audio output: {}", dev_name));
                }
            }
        }
    }

    fn favorite_selected(&mut self) {
        if let Some(t) = self.selected_track() {
            if t.id > 0 {
                self.toggle_favorite_item(FavoriteKind::Track, t.id.to_string());
                return;
            }
        }
        match &self.view {
            MainView::Search => match self.search_tab {
                SearchTab::Albums => {
                    if let Some(a) = self.search_results.albums.get(self.main_selected) {
                        self.toggle_favorite_item(FavoriteKind::Album, a.id.to_string());
                    }
                }
                SearchTab::Artists => {
                    if let Some(a) = self.search_results.artists.get(self.main_selected) {
                        self.toggle_favorite_item(FavoriteKind::Artist, a.id.to_string());
                    }
                }
                SearchTab::Playlists => {
                    if let Some(p) = self.search_results.playlists.get(self.main_selected) {
                        self.toggle_favorite_item(FavoriteKind::Playlist, p.uuid.clone());
                    }
                }
                _ => {}
            },
            MainView::SavedAlbums => {
                if let Some(a) = self.library.albums.get(self.main_selected) {
                    self.toggle_favorite_item(FavoriteKind::Album, a.id.to_string());
                }
            }
            MainView::Artists => {
                if let Some(a) = self.library.artists.get(self.main_selected) {
                    self.toggle_favorite_item(FavoriteKind::Artist, a.id.to_string());
                }
            }
            MainView::Playlists => {
                if let Some(p) = self.library.playlists.get(self.main_selected) {
                    self.toggle_favorite_item(FavoriteKind::Playlist, p.uuid.clone());
                }
            }
            _ => {
                if let Some(ref t) = self.current_track {
                    if t.id > 0 {
                        self.toggle_favorite_item(FavoriteKind::Track, t.id.to_string());
                    }
                }
            }
        }
    }

    fn download_selected(&mut self) {
        if let Some(t) = self.selected_track() {
            self.download_tracks(vec![t]);
            return;
        }
        match &self.view {
            MainView::Detail(d) => {
                let tracks = match d {
                    DetailContent::Album { tracks, .. }
                    | DetailContent::Playlist { tracks, .. }
                    | DetailContent::Mix { tracks, .. }
                    | DetailContent::Radio { tracks, .. } => tracks.clone(),
                    DetailContent::Artist(ad) => ad.top_tracks.clone(),
                };
                self.download_tracks(tracks);
            }
            MainView::SavedAlbums => {
                if let Some(a) = self.library.albums.get(self.main_selected) {
                    if let Some(client) = self.client.clone() {
                        let id = a.id;
                        let q = self.config.audio_quality;
                        let dir = self.config.resolved_download_dir();
                        let tx = self.event_tx.clone();
                        if let Ok((_, tracks)) = client.album_detail(id) {
                            let added = self.downloads.push_tracks(tracks, client, q, dir);
                            let _ = tx.send(AppEvent::Toast(format!("Queued {} tracks from album", added)));
                        }
                    }
                }
            }
            _ => {
                if let Some(t) = self.current_track.clone() {
                    self.download_tracks(vec![t]);
                }
            }
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        let col = mouse.column;
        let row = mouse.row;

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.modal != ModalState::None {
                    self.modal = ModalState::None;
                    return;
                }
                if !self.browser_open {
                    if contains(self.areas.btn_play, col, row) {
                        self.toggle_play_or_resume();
                    } else if contains(self.areas.cover_thumb, col, row) {
                        self.browser_open = true;
                        self.focus = FocusPane::Main;
                    }
                    return;
                }
                if contains(self.areas.search_bar, col, row) {
                    self.focus = FocusPane::SearchBar;
                    return;
                }
                for &(rect, idx) in &self.areas.browser_tabs.clone() {
                    if contains(rect, col, row) {
                        self.switch_sidebar(idx);
                        self.focus = FocusPane::Main;
                        return;
                    }
                }
                for &(rect, tab_idx) in &self.areas.main_tabs.clone() {
                    if contains(rect, col, row) {
                        self.focus = FocusPane::Main;
                        if matches!(self.view, MainView::Home) {
                            self.home_section_idx = tab_idx;
                            self.home_item_idx = 0;
                        } else {
                            self.switch_tab_num(tab_idx);
                        }
                        return;
                    }
                }
                for &(rect, row_idx) in &self.areas.main_rows.clone() {
                    if contains(rect, col, row) {
                        self.focus = FocusPane::Main;
                        if matches!(self.view, MainView::Home) {
                            if self.home_item_idx == row_idx {
                                self.activate_home_selected();
                            } else {
                                self.home_item_idx = row_idx;
                            }
                        } else if self.main_selected == row_idx {
                            self.activate_main_selected();
                        } else {
                            self.main_selected = row_idx;
                        }
                        return;
                    }
                }
                if contains(self.areas.progress_bar, col, row) {
                    let dur = self.audio.telemetry.duration_ms.load(Ordering::Relaxed);
                    if dur > 0 && self.areas.progress_bar.width > 0 {
                        let rel = (col.saturating_sub(self.areas.progress_bar.x)) as f64
                            / self.areas.progress_bar.width as f64;
                        let target = (dur as f64 * rel.clamp(0.0, 1.0)) as u64;
                        self.audio.seek_to_ms(target);
                    }
                    return;
                }
                if contains(self.areas.volume_bar, col, row) {
                    if self.areas.volume_bar.width > 0 {
                        let rel = (col.saturating_sub(self.areas.volume_bar.x)) as f32
                            / self.areas.volume_bar.width as f32;
                        self.config.volume = rel.clamp(0.0, 1.0);
                        self.audio.set_volume(self.config.volume);
                        let _ = config::save(&self.config);
                    }
                    return;
                }
            }
            MouseEventKind::ScrollDown => {
                if !self.browser_open {
                    self.change_volume(-0.05);
                } else {
                    let count = self.main_item_count();
                    if matches!(self.view, MainView::Home) {
                        if count > 0 && self.home_item_idx + 1 < count {
                            self.home_item_idx += 1;
                        }
                    } else if count > 0 && self.main_selected + 1 < count {
                        self.main_selected += 1;
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                if !self.browser_open {
                    self.change_volume(0.05);
                } else if matches!(self.view, MainView::Home) {
                    self.home_item_idx = self.home_item_idx.saturating_sub(1);
                } else {
                    self.main_selected = self.main_selected.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
}

fn contains(r: Rect, col: u16, row: u16) -> bool {
    r.width > 0
        && r.height > 0
        && col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

pub fn scan_local_tracks(download_dir: &str) -> Vec<TrackInfo> {
    let mut roots = vec![PathBuf::from(download_dir)];
    if let Some(music) = dirs::audio_dir().or_else(|| dirs::home_dir().map(|h| h.join("Music"))) {
        roots.push(music);
    }
    let mut paths = Vec::new();
    for root in &roots {
        collect_audio_files(root, 2, &mut paths);
    }
    if let Some(home) = dirs::home_dir() {
        collect_audio_files(&home, 0, &mut paths);
    }
    paths.sort();
    paths.dedup();

    paths
        .into_iter()
        .map(|p| metadata::read_local_track(&p).0)
        .collect()
}

fn collect_audio_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_file() {
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if matches!(ext.to_ascii_lowercase().as_str(), "flac" | "m4a" | "mp3") {
                    out.push(p);
                }
            }
        } else if depth > 0 && p.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with('.') {
                collect_audio_files(&p, depth - 1, out);
            }
        }
    }
}
