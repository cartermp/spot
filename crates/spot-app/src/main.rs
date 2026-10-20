mod app_icon;
mod artwork;
mod icons;
mod theme;
mod tray;
mod updater;
mod visualizer;

use artwork::{download_and_decode_artwork, ArtworkCache, DecodedArtwork};
use iced::alignment::{Horizontal, Vertical};
use iced::widget::{
    button, column, container, horizontal_rule, horizontal_space, image, pick_list, row,
    scrollable, slider, svg, text, text_input, vertical_space,
};
use iced::window;
use iced::{Alignment, Element, Length, Padding, Size, Subscription, Task, Theme};
use spot_audio::SpectrumAnalyzer;
use spot_core::{
    delete_last_word_verified, format_compact_count, format_seconds, merge_devices_verified,
    sanitize_position_secs, sanitize_unit_float, select_device_verified,
    should_toggle_play_pause_on_space_verified, summarize_artist_bio, AlbumDetail,
    AlbumPlaybackQueue, AlbumSummary, ArtistDetail, ArtistSummary, AudioDevice, ConnectionStatus,
    EngineEvent, FormalInvariant, InvariantViolation, NavHistoryMachine, NavRouteTarget,
    NavSection, PlaybackContext, PlaybackState, PlayerCommand, PodcastEpisode, PodcastShowDetail,
    PodcastShowSummary, RecentListenedItem, RecentListenedKind, RecentListenedStore,
    RecentSearchEntry, RecentSearchKind, RecentSearchStore, RepeatMode, SpectrumFrame,
    StoredUpdateEntry, Track, UpdateAvailability, UpdateCatalogStore, UpdateReleaseManifest,
    VerifiedTransportAction, RECENTLY_LISTENED_TOP_LIMIT,
};
use spot_spotify::{spawn_engine, EngineHandle};
use std::path::PathBuf;
use std::time::Duration;
use theme::{palette, INSTRUMENT_SERIF_BYTES, SERIF_FONT};
use tray::{SpotTray, TrayAction};
use visualizer::RibbonVisualizer;

fn search_input_id() -> text_input::Id {
    text_input::Id::new("spot-search-input")
}

pub fn main() -> iced::Result {
    // Support `--export-icon <path>` for macOS `.icns` app bundle packaging scripts.
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--export-icon") {
        if let Some(out_path) = args.get(pos + 1) {
            app_icon::export_icon_png(out_path, 1024)
                .expect("failed to export 1024x1024 Spot app icon PNG");
            return Ok(());
        }
    }

    match updater::handle_cli_update_args(&args) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(err) => {
            eprintln!("error: {err:#}");
            std::process::exit(1);
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "info,symphonia_bundle_mp3=error,symphonia_core=warn",
                )
            }),
        )
        .init();

    #[cfg(target_os = "macos")]
    let platform_specific = window::settings::PlatformSpecific {
        title_hidden: true,
        titlebar_transparent: true,
        fullsize_content_view: true,
    };

    #[cfg(not(target_os = "macos"))]
    let platform_specific = window::settings::PlatformSpecific::default();

    let window_settings = window::Settings {
        size: Size::new(1120.0, 740.0),
        min_size: Some(Size::new(940.0, 640.0)),
        resizable: true,
        decorations: true,
        icon: app_icon::window_icon(),
        platform_specific,
        ..Default::default()
    };

    let _ = app_icon::install_macos_dock_icon();

    iced::application("Spot", SpotApp::update, SpotApp::view)
        .window(window_settings)
        .font(INSTRUMENT_SERIF_BYTES)
        .subscription(SpotApp::subscription)
        .theme(|_| Theme::Light)
        .antialiasing(true)
        .run_with(SpotApp::new)
}

/// Multi-level navigation route supporting deep back/forward history and breadcrumbs
/// (e.g. `Search` -> `Artist: Mastodon` -> `Album: Marrow Deep` -> `Now Playing`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageRoute {
    Section(NavSection),
    Artist {
        id_or_uri: String,
        label: String,
    },
    Album {
        id_or_uri: String,
        label: String,
        artist_name: Option<String>,
        artist_uri: Option<String>,
    },
    Show {
        id_or_uri: String,
        label: String,
    },
}

impl StageRoute {
    pub fn label(&self) -> &str {
        match self {
            StageRoute::Section(sec) => sec.label(),
            StageRoute::Artist { label, .. } => label,
            StageRoute::Album { label, .. } => label,
            StageRoute::Show { label, .. } => label,
        }
    }

    pub fn kind_badge(&self) -> Option<&'static str> {
        match self {
            StageRoute::Section(_) => None,
            StageRoute::Artist { .. } => Some("Artist"),
            StageRoute::Album { .. } => Some("Album"),
            StageRoute::Show { .. } => Some("Podcast"),
        }
    }

    pub fn breadcrumb_label(&self) -> String {
        let base = truncate_nav_label(self.label(), 22);
        match self.kind_badge() {
            Some(kind) => format!("{kind} · {base}"),
            None => base,
        }
    }

    pub fn matches_target(&self, other: &Self) -> bool {
        match (self, other) {
            (StageRoute::Section(a), StageRoute::Section(b)) => a == b,
            (
                StageRoute::Artist {
                    id_or_uri: a,
                    label: la,
                },
                StageRoute::Artist {
                    id_or_uri: b,
                    label: lb,
                },
            ) => {
                a.eq_ignore_ascii_case(b)
                    || (!la.is_empty() && !lb.is_empty() && la.eq_ignore_ascii_case(lb))
            }
            (
                StageRoute::Album {
                    id_or_uri: a,
                    label: la,
                    ..
                },
                StageRoute::Album {
                    id_or_uri: b,
                    label: lb,
                    ..
                },
            ) => {
                a.eq_ignore_ascii_case(b)
                    || (!la.is_empty() && !lb.is_empty() && la.eq_ignore_ascii_case(lb))
            }
            (
                StageRoute::Show {
                    id_or_uri: a,
                    label: la,
                },
                StageRoute::Show {
                    id_or_uri: b,
                    label: lb,
                },
            ) => {
                a.eq_ignore_ascii_case(b)
                    || (!la.is_empty() && !lb.is_empty() && la.eq_ignore_ascii_case(lb))
            }
            _ => false,
        }
    }
}

impl NavRouteTarget for StageRoute {
    fn matches_target(&self, other: &Self) -> bool {
        StageRoute::matches_target(self, other)
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    AnimationTick,
    SelectNav(NavSection),
    NavigateBack,
    NavigateForward,
    NavigateHistory(usize),
    TogglePlayPause,
    NextTrack,
    PreviousTrack,
    SeekChanged(f32),
    SeekReleased,
    VolumeChanged(f32),
    ToggleShuffle,
    CycleRepeat,
    ToggleLike,
    SelectQueueTrack(String),
    PlaySpecificTrack(Track),
    SelectPlaylist(String),
    OpenArtist(String),
    CloseArtistView,
    ToggleArtistBio,
    ToggleArtistTopTracks,
    OpenAlbum(String),
    PlayAlbum(String),
    PlayAlbumTrack(usize),
    PlayArtistTopTracks,
    OpenShow(String),
    PlayShow(String),
    PlayShowEpisode(usize),
    PlaySearchEpisode(PodcastEpisode),
    ToggleShowBio,
    PodcastFilterChanged(String),
    ClearPodcastFilter,
    ToggleRecentlyListenedExpanded,
    CreateNewPlaylist,
    SelectOutputDevice(AudioDevice),
    ConnectSpotifyClicked { force_reauth: bool },
    LogoutSpotifyClicked,
    ModifiersChanged(iced::keyboard::Modifiers),
    SpacePressed {
        status_captured: bool,
        has_command_ctrl_alt: bool,
    },
    SpaceReleased,
    SearchQueryChanged(String),
    SearchDeleteWord,
    SubmitSearch,
    ClearSearchQuery,
    SelectRecentSearch(usize),
    RemoveRecentSearch(usize),
    ClearRecentSearches,
    ToggleTrackMenu,
    CheckForMainUpdate,
    MainUpdateChecked(Result<UpdateReleaseManifest, String>),
    DownloadMainUpdate(UpdateReleaseManifest),
    MainUpdateDownloaded(Result<(StoredUpdateEntry, PathBuf), String>),
    InstallStoredMainUpdate(StoredUpdateEntry),
    MainUpdateInstalled(Result<PathBuf, String>),
    ArtworkLoaded(Option<(String, DecodedArtwork)>),
}

pub struct SpotApp {
    state: PlaybackState,
    active_nav: NavSection,
    nav_history: NavHistoryMachine<StageRoute>,
    analyzer: SpectrumAnalyzer,
    spectrum: SpectrumFrame,
    engine: EngineHandle,
    artwork: ArtworkCache,
    sidebar_app_icon: image::Handle,
    dock_icon_installed: bool,
    tray: Option<SpotTray>,
    tray_init_attempted: bool,
    keyboard_modifiers: iced::keyboard::Modifiers,
    space_key_held: bool,
    text_input_edited_this_event: bool,
    pre_backspace_search_query: Option<String>,
    last_cmd_backspace_applied: bool,
    scrubbing_position: Option<f32>,
    search_query: String,
    recent_searches: RecentSearchStore,
    recently_listened: RecentListenedStore,
    recently_listened_expanded: bool,
    update_catalog: UpdateCatalogStore,
    update_busy: bool,
    update_status_note: Option<String>,
    search_artists: Vec<ArtistSummary>,
    search_albums: Vec<AlbumSummary>,
    search_shows: Vec<PodcastShowSummary>,
    search_episodes: Vec<PodcastEpisode>,
    search_results: Vec<Track>,
    podcast_directory: Vec<PodcastShowSummary>,
    podcast_filter_query: String,
    selected_artist: Option<ArtistDetail>,
    selected_album: Option<AlbumDetail>,
    selected_show: Option<PodcastShowDetail>,
    artist_bio_expanded: bool,
    artist_top_tracks_expanded: bool,
    show_bio_expanded: bool,
    show_track_details_banner: bool,
}

impl FormalInvariant for SpotApp {
    fn check_invariants(&self) -> Result<(), InvariantViolation> {
        self.state.check_invariants()?;
        self.engine.audio_gate.check_sync_with_playback(&self.state)?;
        let tray_snapshot = spot_core::TrayMenuSnapshot::from_playback_state(&self.state);
        tray_snapshot.check_invariants()?;
        tray_snapshot.check_sync_with_playback(&self.state)?;
        if let Some(tray) = &self.tray {
            if let Some(live_snap) = tray.snapshot() {
                live_snap.check_invariants()?;
                live_snap.check_sync_with_playback(&self.state)?;
            }
        }
        self.nav_history.check_invariants()?;
        self.recent_searches.check_invariants()?;
        self.recently_listened.check_invariants()?;
        self.update_catalog.check_invariants()?;
        self.spectrum.check_invariants()?;
        if let Some(show) = &self.selected_show {
            show.check_invariants()?;
        }
        Ok(())
    }
}

impl SpotApp {
    pub fn new() -> (Self, Task<Message>) {
        #[cfg(test)]
        // SAFETY: Test-only isolation so unit tests never touch user credentials or recent searches.
        unsafe {
            std::env::set_var("SPOT_TEST_CACHE_DIR", "1");
        }
        let (analyzer, sample_producer) = SpectrumAnalyzer::new();
        let engine = spawn_engine(sample_producer);
        let mut state = PlaybackState::mockup_default();

        // Discover real CoreAudio output devices immediately on startup while preserving INV-P6
        let discovered = spot_audio::enumerate_output_devices();
        merge_devices_verified(&mut state, discovered);
        tracing::info!(
            "[UI] SpotApp initialized (selected_device='{}', is_default={})",
            state.selected_device.name,
            state.selected_device.is_default
        );

        let recent_searches = load_recent_searches();
        let recently_listened = load_recently_listened();
        let update_catalog = updater::load_update_catalog();
        let artwork = ArtworkCache::new();
        let sidebar_app_icon = app_icon::sidebar_badge_handle();
        let dock_icon_installed = app_icon::install_macos_dock_icon();
        let tray = SpotTray::try_new(&state, &artwork);
        let tray_init_attempted = tray.is_some();
        let podcast_directory = spot_spotify::web_api::featured_podcast_shows();

        let mut app = Self {
            state,
            active_nav: NavSection::NowPlaying,
            nav_history: NavHistoryMachine::new(StageRoute::Section(NavSection::NowPlaying), 32),
            analyzer,
            spectrum: SpectrumFrame::calm_preview(0.0),
            engine,
            artwork,
            sidebar_app_icon,
            dock_icon_installed,
            tray,
            tray_init_attempted,
            keyboard_modifiers: iced::keyboard::Modifiers::empty(),
            space_key_held: false,
            text_input_edited_this_event: false,
            pre_backspace_search_query: None,
            last_cmd_backspace_applied: false,
            scrubbing_position: None,
            search_query: String::new(),
            recent_searches,
            recently_listened,
            recently_listened_expanded: false,
            update_catalog,
            update_busy: false,
            update_status_note: None,
            search_artists: Vec::new(),
            search_albums: Vec::new(),
            search_shows: Vec::new(),
            search_episodes: Vec::new(),
            search_results: Vec::new(),
            podcast_directory,
            podcast_filter_query: String::new(),
            selected_artist: None,
            selected_album: None,
            selected_show: None,
            artist_bio_expanded: false,
            artist_top_tracks_expanded: false,
            show_bio_expanded: false,
            show_track_details_banner: false,
        };
        debug_assert!(app.check_invariants().is_ok());

        let initial_artwork_tasks = app.spawn_missing_artwork_tasks();
        #[cfg(not(test))]
        let startup_tasks = Task::batch([
            initial_artwork_tasks,
            Task::perform(
                async {
                    updater::fetch_latest_main_release()
                        .await
                        .map_err(|e| e.to_string())
                },
                Message::MainUpdateChecked,
            ),
        ]);
        #[cfg(test)]
        let startup_tasks = initial_artwork_tasks;
        (app, startup_tasks)
    }

    fn current_route(&self) -> &StageRoute {
        self.nav_history.current()
    }

    fn can_go_back(&self) -> bool {
        self.nav_history.can_go_back()
    }

    fn can_go_forward(&self) -> bool {
        self.nav_history.can_go_forward()
    }

    fn push_route(&mut self, route: StageRoute) {
        if let StageRoute::Section(sec) = &route {
            self.active_nav = *sec;
        }
        self.nav_history.push(route);
        debug_assert!(self.nav_history.check_invariants().is_ok());
        let trail: Vec<String> = self
            .nav_history
            .entries()
            .iter()
            .take(self.nav_history.index() + 1)
            .map(|r| r.breadcrumb_label())
            .collect();
        tracing::info!(
            "[Nav] Route pushed (index={}/{}) -> {}",
            self.nav_history.index(),
            self.nav_history.entries().len(),
            trail.join(" › ")
        );
    }

    fn activate_current_route(&mut self) {
        let route = self.current_route().clone();
        let trail: Vec<String> = self
            .nav_history
            .entries()
            .iter()
            .take(self.nav_history.index() + 1)
            .map(|r| r.breadcrumb_label())
            .collect();
        tracing::info!(
            "[Nav] Activated history route (index={}/{}) -> {}",
            self.nav_history.index(),
            self.nav_history.entries().len(),
            trail.join(" › ")
        );

        match route {
            StageRoute::Section(sec) => {
                self.active_nav = sec;
                if sec == NavSection::Search
                    && !self.search_query.trim().is_empty()
                    && self.search_artists.is_empty()
                {
                    let _ = self
                        .engine
                        .command_tx
                        .send(PlayerCommand::SearchCatalog(self.search_query.clone()));
                }
            }
            StageRoute::Artist { id_or_uri, label } => {
                self.artist_bio_expanded = false;
                self.artist_top_tracks_expanded = false;
                let already_loaded = self.selected_artist.as_ref().is_some_and(|d| {
                    d.artist.spotify_uri.eq_ignore_ascii_case(&id_or_uri)
                        || d.artist.id.eq_ignore_ascii_case(&id_or_uri)
                        || d.artist.name.eq_ignore_ascii_case(&id_or_uri)
                        || (!label.is_empty() && d.artist.name.eq_ignore_ascii_case(&label))
                });
                if !already_loaded {
                    let _ = self
                        .engine
                        .command_tx
                        .send(PlayerCommand::SelectArtist(id_or_uri));
                }
            }
            StageRoute::Album {
                id_or_uri, label, ..
            } => {
                let already_loaded = self.selected_album.as_ref().is_some_and(|d| {
                    !d.tracks.is_empty()
                        && (d.album.spotify_uri.eq_ignore_ascii_case(&id_or_uri)
                            || d.album.id.eq_ignore_ascii_case(&id_or_uri)
                            || d.album.title.eq_ignore_ascii_case(&id_or_uri)
                            || (!label.is_empty() && d.album.title.eq_ignore_ascii_case(&label)))
                });
                if !already_loaded {
                    let _ = self
                        .engine
                        .command_tx
                        .send(PlayerCommand::SelectAlbum(id_or_uri));
                }
            }
            StageRoute::Show { id_or_uri, label } => {
                self.show_bio_expanded = false;
                let already_loaded = self.selected_show.as_ref().is_some_and(|d| {
                    !d.episodes.is_empty()
                        && (d.show.spotify_uri.eq_ignore_ascii_case(&id_or_uri)
                            || d.show.id.eq_ignore_ascii_case(&id_or_uri)
                            || d.show.title.eq_ignore_ascii_case(&id_or_uri)
                            || (!label.is_empty() && d.show.title.eq_ignore_ascii_case(&label)))
                });
                if !already_loaded {
                    let _ = self
                        .engine
                        .command_tx
                        .send(PlayerCommand::SelectShow(id_or_uri));
                }
            }
        }
    }

    fn resolve_show_meta(&self, show_id_or_uri: &str) -> (String, String, Option<PodcastShowSummary>) {
        for show in self.search_shows.iter().chain(self.podcast_directory.iter()) {
            if show.spotify_uri.eq_ignore_ascii_case(show_id_or_uri)
                || show.id.eq_ignore_ascii_case(show_id_or_uri)
                || show.title.eq_ignore_ascii_case(show_id_or_uri)
            {
                return (show.title.clone(), show.publisher.clone(), Some(show.clone()));
            }
        }
        if let Some(detail) = &self.selected_show {
            if detail.show.spotify_uri.eq_ignore_ascii_case(show_id_or_uri)
                || detail.show.id.eq_ignore_ascii_case(show_id_or_uri)
                || detail.show.title.eq_ignore_ascii_case(show_id_or_uri)
            {
                return (
                    detail.show.title.clone(),
                    detail.show.publisher.clone(),
                    Some(detail.show.clone()),
                );
            }
        }
        if show_id_or_uri.starts_with("spotify:show:") {
            ("Podcast".to_string(), "Spotify Podcasts".to_string(), None)
        } else {
            (
                show_id_or_uri.to_string(),
                "Spotify Podcasts".to_string(),
                None,
            )
        }
    }

    fn resolve_artist_label(&self, artist_id_or_name: &str) -> String {
        if let Some(found) = self.search_artists.iter().find(|a| {
            a.spotify_uri.eq_ignore_ascii_case(artist_id_or_name)
                || a.id.eq_ignore_ascii_case(artist_id_or_name)
                || a.name.eq_ignore_ascii_case(artist_id_or_name)
        }) {
            return found.name.clone();
        }
        if let Some(detail) = &self.selected_artist {
            if detail
                .artist
                .spotify_uri
                .eq_ignore_ascii_case(artist_id_or_name)
                || detail.artist.id.eq_ignore_ascii_case(artist_id_or_name)
                || detail.artist.name.eq_ignore_ascii_case(artist_id_or_name)
            {
                return detail.artist.name.clone();
            }
        }
        if let Some(alb) = &self.selected_album {
            if alb
                .artist_uri
                .as_deref()
                .is_some_and(|u| u.eq_ignore_ascii_case(artist_id_or_name))
            {
                return alb.album.artist_name.clone();
            }
        }
        if artist_id_or_name.starts_with("spotify:artist:") {
            "Artist".to_string()
        } else {
            artist_id_or_name.to_string()
        }
    }

    fn resolve_album_meta(
        &self,
        album_id_or_uri: &str,
    ) -> (String, Option<String>, Option<String>) {
        if let Some(detail) = &self.selected_artist {
            if let Some(alb) = detail.albums.iter().find(|a| {
                a.spotify_uri.eq_ignore_ascii_case(album_id_or_uri)
                    || a.id.eq_ignore_ascii_case(album_id_or_uri)
                    || a.title.eq_ignore_ascii_case(album_id_or_uri)
            }) {
                return (
                    alb.title.clone(),
                    Some(detail.artist.name.clone()),
                    Some(detail.artist.spotify_uri.clone()),
                );
            }
        }
        if let Some(alb) = self.search_albums.iter().find(|a| {
            a.spotify_uri.eq_ignore_ascii_case(album_id_or_uri)
                || a.id.eq_ignore_ascii_case(album_id_or_uri)
                || a.title.eq_ignore_ascii_case(album_id_or_uri)
        }) {
            let artist_uri = self
                .search_artists
                .iter()
                .find(|ar| ar.name.eq_ignore_ascii_case(&alb.artist_name))
                .map(|ar| ar.spotify_uri.clone());
            return (alb.title.clone(), Some(alb.artist_name.clone()), artist_uri);
        }
        if let Some(detail) = &self.selected_album {
            if detail
                .album
                .spotify_uri
                .eq_ignore_ascii_case(album_id_or_uri)
                || detail.album.title.eq_ignore_ascii_case(album_id_or_uri)
            {
                return (
                    detail.album.title.clone(),
                    Some(detail.album.artist_name.clone()),
                    detail.artist_uri.clone(),
                );
            }
        }
        if let Some(item) = self.recently_listened.entries().iter().find(|it| {
            matches!(&it.kind, RecentListenedKind::Album { album_uri, .. }
                if album_uri.eq_ignore_ascii_case(album_id_or_uri)
                    || it.title.eq_ignore_ascii_case(album_id_or_uri))
        }) {
            return (
                item.title.clone(),
                Some(item.kind.creator_name().to_string()),
                None,
            );
        }
        if album_id_or_uri.starts_with("spotify:album:") {
            ("Album".to_string(), None, None)
        } else {
            (
                album_id_or_uri.to_string(),
                Some(self.state.current_track.artist.clone()),
                None,
            )
        }
    }

    fn record_recently_listened(&mut self, item: RecentListenedItem) {
        if self.recently_listened.record_item(item) {
            debug_assert!(self.recently_listened.check_invariants().is_ok());
            save_recently_listened(self.recently_listened.entries());
        }
    }

    fn record_recently_listened_from_track(&mut self, track: &Track) {
        if track.title.trim().is_empty() || track.title == "Ready to Play" {
            return;
        }
        if track.spotify_uri.starts_with("spotify:episode:") {
            let (show_title, publisher, show_opt) = self.resolve_show_meta(&track.album);
            let show_uri = show_opt
                .as_ref()
                .map(|s| s.spotify_uri.clone())
                .unwrap_or_else(|| format!("spotify:show:{show_title}"));
            let cover = track
                .cover_art_url
                .clone()
                .or_else(|| show_opt.and_then(|s| s.cover_art_url));
            self.record_recently_listened(RecentListenedItem {
                title: show_title,
                subtitle: format!("Podcast · {publisher}"),
                cover_art_url: cover,
                artwork_theme: track.artwork_theme,
                kind: RecentListenedKind::Podcast {
                    show_uri,
                    publisher,
                },
            });
        } else if !track.album.trim().is_empty() {
            let mut album_uri = track.album.clone();
            let mut cover = track.cover_art_url.clone();
            if let Some(detail) = &self.selected_album {
                if detail.album.title.eq_ignore_ascii_case(&track.album) {
                    album_uri = detail.album.spotify_uri.clone();
                    if cover.is_none() {
                        cover = detail.album.cover_art_url.clone();
                    }
                }
            }
            if !album_uri.starts_with("spotify:album:") {
                if let Some(artist_detail) = &self.selected_artist {
                    if let Some(alb) = artist_detail
                        .albums
                        .iter()
                        .find(|a| a.title.eq_ignore_ascii_case(&track.album))
                    {
                        album_uri = alb.spotify_uri.clone();
                        if cover.is_none() {
                            cover = alb.cover_art_url.clone();
                        }
                    }
                }
            }
            if !album_uri.starts_with("spotify:album:") {
                if let Some(alb) = self
                    .search_albums
                    .iter()
                    .find(|a| a.title.eq_ignore_ascii_case(&track.album))
                {
                    album_uri = alb.spotify_uri.clone();
                    if cover.is_none() {
                        cover = alb.cover_art_url.clone();
                    }
                }
            }
            self.record_recently_listened(RecentListenedItem {
                title: track.album.clone(),
                subtitle: format!("Album · {}", track.artist),
                cover_art_url: cover,
                artwork_theme: track.artwork_theme,
                kind: RecentListenedKind::Album {
                    album_uri,
                    artist_name: track.artist.clone(),
                },
            });
        }
    }

    fn record_recent_search_entry(&mut self, entry: RecentSearchEntry) {
        if self.recent_searches.record_entry(entry) {
            debug_assert!(self.recent_searches.check_invariants().is_ok());
            save_recent_searches(self.recent_searches.entries());
        }
    }

    fn record_recent_query_from_search(&mut self, query: &str) {
        if self.recent_searches.record_typing_query(query) {
            debug_assert!(self.recent_searches.check_invariants().is_ok());
            save_recent_searches(self.recent_searches.entries());
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        // 60 FPS frame ticker + global keyboard listener for Spacebar play/pause & Cmd+Backspace whole-word deletion
        Subscription::batch([
            iced::time::every(Duration::from_millis(16)).map(|_| Message::AnimationTick),
            iced::event::listen_with(|event, status, _window| match event {
                iced::Event::Keyboard(iced::keyboard::Event::ModifiersChanged(modifiers)) => {
                    Some(Message::ModifiersChanged(modifiers))
                }
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Backspace),
                    modifiers,
                    ..
                }) if modifiers.command() || modifiers.macos_command() => {
                    Some(Message::SearchDeleteWord)
                }
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key,
                    modifiers,
                    ..
                }) if is_space_keyboard_key(&key) => Some(Message::SpacePressed {
                    status_captured: status == iced::event::Status::Captured,
                    has_command_ctrl_alt: modifiers.command()
                        || modifiers.macos_command()
                        || modifiers.control()
                        || modifiers.alt(),
                }),
                iced::Event::Keyboard(iced::keyboard::Event::KeyReleased { key, .. })
                    if is_space_keyboard_key(&key) =>
                {
                    Some(Message::SpaceReleased)
                }
                _ => None,
            }),
        ])
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::AnimationTick => {
                self.text_input_edited_this_event = false;
                if !self.dock_icon_installed {
                    self.dock_icon_installed = app_icon::install_macos_dock_icon();
                }
                if !self.tray_init_attempted {
                    self.tray_init_attempted = true;
                    self.tray = SpotTray::try_new(&self.state, &self.artwork);
                }

                // Poll native macOS menu bar tray & Now Playing actions
                for action in SpotTray::poll_actions() {
                    match action {
                        TrayAction::OpenNowPlaying => {
                            self.push_route(StageRoute::Section(NavSection::NowPlaying));
                        }
                        TrayAction::Play => {
                            if !self.state.is_playing {
                                self.state.is_playing = true;
                                self.engine.audio_gate.set_playing(true);
                                let _ = self.engine.command_tx.send(PlayerCommand::TogglePlayPause);
                            }
                        }
                        TrayAction::Pause => {
                            if self.state.is_playing {
                                self.state.is_playing = false;
                                self.engine.audio_gate.set_playing(false);
                                let _ = self.engine.command_tx.send(PlayerCommand::TogglePlayPause);
                            }
                        }
                        TrayAction::TogglePlayPause => {
                            self.state.is_playing = !self.state.is_playing;
                            self.engine.audio_gate.set_playing(self.state.is_playing);
                            let _ = self.engine.command_tx.send(PlayerCommand::TogglePlayPause);
                        }
                        TrayAction::NextTrack => {
                            self.state
                                .apply_verified_action(VerifiedTransportAction::NextTrack);
                            self.engine
                                .audio_gate
                                .flush_and_set_playing(self.state.is_playing);
                            self.record_recently_listened_from_track(
                                &self.state.current_track.clone(),
                            );
                            let _ = self.engine.command_tx.send(PlayerCommand::NextTrack);
                        }
                        TrayAction::PreviousTrack => {
                            self.state
                                .apply_verified_action(VerifiedTransportAction::PreviousTrack);
                            self.engine
                                .audio_gate
                                .flush_and_set_playing(self.state.is_playing);
                            self.record_recently_listened_from_track(
                                &self.state.current_track.clone(),
                            );
                            let _ = self.engine.command_tx.send(PlayerCommand::PreviousTrack);
                        }
                        TrayAction::ToggleLike => {
                            self.state
                                .apply_verified_action(VerifiedTransportAction::ToggleLikeCurrent);
                            let _ = self
                                .engine
                                .command_tx
                                .send(PlayerCommand::ToggleLikeCurrent);
                        }
                        TrayAction::SeekSeconds(secs) => {
                            let sanitized = sanitize_position_secs(
                                secs as f32,
                                self.state.current_track.duration_secs,
                            );
                            self.scrubbing_position = None;
                            self.state.position_secs = sanitized;
                            self.engine
                                .audio_gate
                                .flush_and_set_playing(self.state.is_playing);
                            let _ = self
                                .engine
                                .command_tx
                                .send(PlayerCommand::SeekSeconds(sanitized));
                        }
                    }
                }

                let mut needs_art_scan = false;
                // Drain any pending events from the async Spotify + Audio engine
                while let Ok(event) = self.engine.event_rx.try_recv() {
                    if matches!(
                        event,
                        EngineEvent::StateSynced(_)
                            | EngineEvent::TrackChanged(_)
                            | EngineEvent::QueueUpdated(_)
                            | EngineEvent::SearchResults { .. }
                            | EngineEvent::ArtistDetailLoaded(_)
                            | EngineEvent::AlbumDetailLoaded(_)
                    ) {
                        needs_art_scan = true;
                    }
                    self.apply_engine_event(event);
                }

                self.sync_tray();

                // Update 60 FPS spectrum frame from live lock-free audio ringbuffer (or harmonic fallback)
                self.spectrum = self.analyzer.next_frame(self.state.is_playing, 0.016);

                if needs_art_scan {
                    return self.spawn_missing_artwork_tasks();
                }
            }
            Message::SelectNav(nav) => {
                tracing::info!("[UI] SelectNav -> {:?}", nav);
                self.push_route(StageRoute::Section(nav));
                if nav == NavSection::Search && self.search_artists.is_empty() {
                    let _ = self
                        .engine
                        .command_tx
                        .send(PlayerCommand::SearchCatalog(self.search_query.clone()));
                }
            }
            Message::NavigateBack => {
                if self.nav_history.go_back() {
                    self.activate_current_route();
                }
            }
            Message::NavigateForward => {
                if self.nav_history.go_forward() {
                    self.activate_current_route();
                }
            }
            Message::NavigateHistory(idx) => {
                if idx != self.nav_history.index() && self.nav_history.jump_to(idx) {
                    self.activate_current_route();
                }
            }
            Message::TogglePlayPause => {
                self.state.is_playing = !self.state.is_playing;
                self.engine.audio_gate.set_playing(self.state.is_playing);
                tracing::info!(
                    "[UI] TogglePlayPause clicked -> is_playing={} (instant audio gate updated)",
                    self.state.is_playing
                );
                let _ = self.engine.command_tx.send(PlayerCommand::TogglePlayPause);
            }
            Message::NextTrack => {
                tracing::info!("[UI] NextTrack clicked");
                self.state
                    .apply_verified_action(VerifiedTransportAction::NextTrack);
                self.engine
                    .audio_gate
                    .flush_and_set_playing(self.state.is_playing);
                self.record_recently_listened_from_track(&self.state.current_track.clone());
                let _ = self.engine.command_tx.send(PlayerCommand::NextTrack);
            }
            Message::PreviousTrack => {
                tracing::info!("[UI] PreviousTrack clicked");
                self.state
                    .apply_verified_action(VerifiedTransportAction::PreviousTrack);
                self.engine
                    .audio_gate
                    .flush_and_set_playing(self.state.is_playing);
                self.record_recently_listened_from_track(&self.state.current_track.clone());
                let _ = self.engine.command_tx.send(PlayerCommand::PreviousTrack);
            }
            Message::SeekChanged(val) => {
                let sanitized = sanitize_position_secs(val, self.state.current_track.duration_secs);
                self.scrubbing_position = Some(sanitized);
                self.state.position_secs = sanitized;
            }
            Message::SeekReleased => {
                if let Some(pos) = self.scrubbing_position.take() {
                    tracing::info!("[UI] SeekReleased -> {pos:.1}s");
                    self.engine
                        .audio_gate
                        .flush_and_set_playing(self.state.is_playing);
                    let _ = self.engine.command_tx.send(PlayerCommand::SeekSeconds(pos));
                }
            }
            Message::VolumeChanged(vol) => {
                self.state.volume = sanitize_unit_float(vol, self.state.volume);
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::SetVolume(self.state.volume));
            }
            Message::ToggleShuffle => {
                self.state.apply_verified_action(VerifiedTransportAction::ToggleShuffle);
                tracing::info!("[UI] ToggleShuffle -> {}", self.state.shuffle);
                let _ = self.engine.command_tx.send(PlayerCommand::ToggleShuffle);
            }
            Message::CycleRepeat => {
                self.state.apply_verified_action(VerifiedTransportAction::CycleRepeat);
                tracing::info!("[UI] CycleRepeat -> {:?}", self.state.repeat);
                let _ = self.engine.command_tx.send(PlayerCommand::CycleRepeat);
            }
            Message::ToggleLike => {
                self.state.apply_verified_action(VerifiedTransportAction::ToggleLikeCurrent);
                tracing::info!(
                    "[UI] ToggleLike ('{}') -> {}",
                    self.state.current_track.title,
                    self.state.current_track.liked
                );
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::ToggleLikeCurrent);
            }
            Message::SelectQueueTrack(track_id) => {
                tracing::info!("[UI] SelectQueueTrack('{track_id}')");
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::SelectQueueTrack(track_id));
                self.push_route(StageRoute::Section(NavSection::NowPlaying));
            }
            Message::PlaySpecificTrack(track) => {
                tracing::info!(
                    "[UI] PlaySpecificTrack: '{}' by '{}' ({})",
                    track.title,
                    track.artist,
                    track.spotify_uri
                );
                if matches!(self.current_route(), StageRoute::Section(NavSection::Search)) {
                    self.record_recent_search_entry(RecentSearchEntry {
                        label: track.title.clone(),
                        subtitle: format!("Track · {}", track.artist),
                        query: format!("{} {}", track.title, track.artist),
                        kind: RecentSearchKind::Track {
                            uri: track.spotify_uri.clone(),
                            artist_name: track.artist.clone(),
                        },
                    });
                }
                self.record_recently_listened_from_track(&track);
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::PlayTrack(track));
                self.push_route(StageRoute::Section(NavSection::NowPlaying));
            }
            Message::SelectPlaylist(playlist_id) => {
                tracing::info!("[UI] SelectPlaylist('{playlist_id}')");
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::SelectPlaylist(playlist_id));
                self.push_route(StageRoute::Section(NavSection::NowPlaying));
            }
            Message::OpenArtist(artist_id_or_name) => {
                let label = self.resolve_artist_label(&artist_id_or_name);
                tracing::info!(
                    "[UI] OpenArtist('{artist_id_or_name}') -> label='{}'",
                    label
                );
                if matches!(self.current_route(), StageRoute::Section(NavSection::Search))
                    && label != "Artist"
                {
                    self.record_recent_search_entry(RecentSearchEntry {
                        label: label.clone(),
                        subtitle: "Artist".to_string(),
                        query: label.clone(),
                        kind: RecentSearchKind::Artist {
                            uri: artist_id_or_name.clone(),
                        },
                    });
                }
                self.artist_bio_expanded = false;
                self.artist_top_tracks_expanded = false;
                self.push_route(StageRoute::Artist {
                    id_or_uri: artist_id_or_name.clone(),
                    label,
                });
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::SelectArtist(artist_id_or_name));
            }
            Message::CloseArtistView => {
                tracing::info!("[UI] CloseArtistView -> NavigateBack");
                self.artist_bio_expanded = false;
                self.artist_top_tracks_expanded = false;
                if self.nav_history.go_back() {
                    self.activate_current_route();
                } else {
                    self.push_route(StageRoute::Section(NavSection::Search));
                }
            }
            Message::ToggleArtistBio => {
                self.artist_bio_expanded = !self.artist_bio_expanded;
                tracing::info!(
                    "[UI] ToggleArtistBio -> expanded={}",
                    self.artist_bio_expanded
                );
            }
            Message::ToggleArtistTopTracks => {
                self.artist_top_tracks_expanded = !self.artist_top_tracks_expanded;
                tracing::info!(
                    "[UI] ToggleArtistTopTracks -> expanded={}",
                    self.artist_top_tracks_expanded
                );
            }
            Message::OpenAlbum(album_uri) => {
                let (label, artist_name, artist_uri) = self.resolve_album_meta(&album_uri);
                tracing::info!(
                    "[UI] OpenAlbum('{album_uri}') -> label='{}', artist={:?}",
                    label,
                    artist_name
                );
                if matches!(self.current_route(), StageRoute::Section(NavSection::Search))
                    && label != "Album"
                {
                    let artist_str = artist_name
                        .clone()
                        .unwrap_or_else(|| "Album".to_string());
                    self.record_recent_search_entry(RecentSearchEntry {
                        label: label.clone(),
                        subtitle: format!("Album · {artist_str}"),
                        query: label.clone(),
                        kind: RecentSearchKind::Album {
                            uri: album_uri.clone(),
                            artist_name: artist_str,
                        },
                    });
                }
                let already_same = self.selected_album.as_ref().is_some_and(|d| {
                    d.album.spotify_uri.eq_ignore_ascii_case(&album_uri)
                        || d.album.id.eq_ignore_ascii_case(&album_uri)
                        || d.album.title.eq_ignore_ascii_case(&album_uri)
                });
                if !already_same {
                    self.selected_album = None;
                    if let Some(artist_detail) = &self.selected_artist {
                        if let Some(alb) = artist_detail.albums.iter().find(|a| {
                            a.spotify_uri.eq_ignore_ascii_case(&album_uri)
                                || a.id.eq_ignore_ascii_case(&album_uri)
                                || a.title.eq_ignore_ascii_case(&album_uri)
                        }) {
                            self.selected_album = Some(AlbumDetail {
                                album: alb.clone(),
                                artist_uri: Some(artist_detail.artist.spotify_uri.clone()),
                                tracks: Vec::new(),
                            });
                        }
                    }
                    if self.selected_album.is_none() {
                        if let Some(alb) = self.search_albums.iter().find(|a| {
                            a.spotify_uri.eq_ignore_ascii_case(&album_uri)
                                || a.id.eq_ignore_ascii_case(&album_uri)
                                || a.title.eq_ignore_ascii_case(&album_uri)
                        }) {
                            self.selected_album = Some(AlbumDetail {
                                album: alb.clone(),
                                artist_uri: artist_uri.clone(),
                                tracks: Vec::new(),
                            });
                        }
                    }
                    if self.selected_album.is_none() {
                        if let Some(item) = self.recently_listened.entries().iter().find(|it| {
                            matches!(&it.kind, RecentListenedKind::Album { album_uri: u, .. }
                                if u.eq_ignore_ascii_case(&album_uri)
                                    || it.title.eq_ignore_ascii_case(&album_uri))
                        }) {
                            let id = album_uri
                                .strip_prefix("spotify:album:")
                                .unwrap_or(&album_uri)
                                .to_string();
                            self.selected_album = Some(AlbumDetail {
                                album: AlbumSummary {
                                    id,
                                    spotify_uri: item.kind.target_uri().to_string(),
                                    title: item.title.clone(),
                                    artist_name: item.kind.creator_name().to_string(),
                                    release_year: "2024".to_string(),
                                    album_type: "Album".to_string(),
                                    track_count: 0,
                                    cover_art_url: item.cover_art_url.clone(),
                                    artwork_theme: item.artwork_theme,
                                },
                                artist_uri: artist_uri.clone(),
                                tracks: Vec::new(),
                            });
                        }
                    }
                    if self.selected_album.is_none() {
                        self.selected_album = Some(spot_spotify::web_api::fallback_album_detail(
                            &album_uri, None,
                        ));
                    }
                }
                self.push_route(StageRoute::Album {
                    id_or_uri: album_uri.clone(),
                    label,
                    artist_name,
                    artist_uri,
                });
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::SelectAlbum(album_uri));
            }
            Message::PlayAlbum(album_uri) => {
                let (label, artist_name, _) = self.resolve_album_meta(&album_uri);
                tracing::info!("[UI] PlayAlbum('{album_uri}') -> label='{label}'");
                let artist_str = artist_name
                    .clone()
                    .unwrap_or_else(|| "Album".to_string());
                if matches!(self.current_route(), StageRoute::Section(NavSection::Search))
                    && label != "Album"
                {
                    self.record_recent_search_entry(RecentSearchEntry {
                        label: label.clone(),
                        subtitle: format!("Album · {artist_str}"),
                        query: label.clone(),
                        kind: RecentSearchKind::Album {
                            uri: album_uri.clone(),
                            artist_name: artist_str.clone(),
                        },
                    });
                }
                if label != "Album" {
                    let cover = self
                        .selected_album
                        .as_ref()
                        .filter(|d| {
                            d.album.spotify_uri.eq_ignore_ascii_case(&album_uri)
                                || d.album.title.eq_ignore_ascii_case(&label)
                        })
                        .and_then(|d| d.album.cover_art_url.clone())
                        .or_else(|| {
                            self.selected_artist.as_ref().and_then(|ad| {
                                ad.albums
                                    .iter()
                                    .find(|a| {
                                        a.spotify_uri.eq_ignore_ascii_case(&album_uri)
                                            || a.title.eq_ignore_ascii_case(&label)
                                    })
                                    .and_then(|a| a.cover_art_url.clone())
                            })
                        })
                        .or_else(|| {
                            self.search_albums
                                .iter()
                                .find(|a| {
                                    a.spotify_uri.eq_ignore_ascii_case(&album_uri)
                                        || a.title.eq_ignore_ascii_case(&label)
                                })
                                .and_then(|a| a.cover_art_url.clone())
                        });
                    self.record_recently_listened(RecentListenedItem {
                        title: label,
                        subtitle: format!("Album · {artist_str}"),
                        cover_art_url: cover,
                        artwork_theme: spot_core::ArtworkTheme::SunriseMountainLake,
                        kind: RecentListenedKind::Album {
                            album_uri: album_uri.clone(),
                            artist_name: artist_str,
                        },
                    });
                }
                if let Some(detail) = self.selected_album.clone().filter(|d| {
                    !d.tracks.is_empty()
                        && (d.album.spotify_uri.eq_ignore_ascii_case(&album_uri)
                            || d.album.id.eq_ignore_ascii_case(&album_uri)
                            || d.album.title.eq_ignore_ascii_case(&album_uri))
                }) {
                    let active_album = AlbumPlaybackQueue {
                        album_uri: detail.album.spotify_uri.clone(),
                        album_title: detail.album.title.clone(),
                        artist_name: detail.album.artist_name.clone(),
                        tracks: detail.tracks,
                    };
                    self.state
                        .apply_verified_action(VerifiedTransportAction::ActivateAlbum {
                            album: active_album.clone(),
                            artist_uri: detail.artist_uri,
                            artist_albums: vec![active_album],
                            start_track_index: 0,
                            auto_play: true,
                        });
                    self.engine
                        .audio_gate
                        .flush_and_set_playing(self.state.is_playing);
                }
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::PlayAlbum(album_uri));
                self.push_route(StageRoute::Section(NavSection::NowPlaying));
            }
            Message::PlayAlbumTrack(track_idx) => {
                if let Some(detail) = self.selected_album.clone() {
                    if let Some(chosen) = detail.tracks.get(track_idx).cloned() {
                        tracing::info!(
                            "[UI] PlayAlbumTrack({track_idx}) -> '{}' from album '{}'",
                            chosen.title,
                            detail.album.title
                        );
                        self.record_recently_listened(RecentListenedItem {
                            title: detail.album.title.clone(),
                            subtitle: format!(
                                "Album · {} · {}",
                                detail.album.artist_name, detail.album.release_year
                            ),
                            cover_art_url: detail.album.cover_art_url.clone(),
                            artwork_theme: detail.album.artwork_theme,
                            kind: RecentListenedKind::Album {
                                album_uri: detail.album.spotify_uri.clone(),
                                artist_name: detail.album.artist_name.clone(),
                            },
                        });
                        let active_album = AlbumPlaybackQueue {
                            album_uri: detail.album.spotify_uri.clone(),
                            album_title: detail.album.title.clone(),
                            artist_name: detail.album.artist_name.clone(),
                            tracks: detail.tracks.clone(),
                        };
                        self.state
                            .apply_verified_action(VerifiedTransportAction::ActivateAlbum {
                                album: active_album.clone(),
                                artist_uri: detail.artist_uri.clone(),
                                artist_albums: vec![active_album],
                                start_track_index: track_idx,
                                auto_play: true,
                            });
                        self.engine
                            .audio_gate
                            .flush_and_set_playing(self.state.is_playing);
                        let _ =
                            self.engine
                                .command_tx
                                .send(PlayerCommand::PlayAlbumAtTrack {
                                    album_uri: detail.album.spotify_uri.clone(),
                                    track_index: track_idx,
                                    preloaded_album: Some(detail),
                                });
                        self.push_route(StageRoute::Section(NavSection::NowPlaying));
                    }
                }
            }
            Message::PlayArtistTopTracks => {
                if let Some(detail) = &self.selected_artist {
                    if let Some(first) = detail.top_tracks.first().cloned() {
                        tracing::info!(
                            "[UI] PlayArtistTopTracks -> starting with '{}' ({})",
                            first.title,
                            first.spotify_uri
                        );
                        let _ = self
                            .engine
                            .command_tx
                            .send(PlayerCommand::PlayTrack(first));
                        self.push_route(StageRoute::Section(NavSection::NowPlaying));
                    }
                }
            }
            Message::OpenShow(show_uri) => {
                let (label, publisher, summary_opt) = self.resolve_show_meta(&show_uri);
                tracing::info!(
                    "[UI] OpenShow('{show_uri}') -> label='{label}', publisher='{publisher}'"
                );
                if matches!(self.current_route(), StageRoute::Section(NavSection::Search))
                    && label != "Podcast"
                {
                    self.record_recent_search_entry(RecentSearchEntry {
                        label: label.clone(),
                        subtitle: format!("Podcast · {publisher}"),
                        query: label.clone(),
                        kind: RecentSearchKind::Show {
                            uri: show_uri.clone(),
                            publisher: publisher.clone(),
                        },
                    });
                }
                let already_same = self.selected_show.as_ref().is_some_and(|d| {
                    !d.episodes.is_empty()
                        && (d.show.spotify_uri.eq_ignore_ascii_case(&show_uri)
                            || d.show.id.eq_ignore_ascii_case(&show_uri)
                            || d.show.title.eq_ignore_ascii_case(&show_uri))
                });
                if !already_same {
                    self.selected_show = summary_opt.map(|show| PodcastShowDetail {
                        show,
                        episodes: Vec::new(),
                    });
                    if self.selected_show.is_none() {
                        self.selected_show =
                            Some(spot_spotify::web_api::fallback_show_detail(&show_uri));
                    }
                }
                self.show_bio_expanded = false;
                self.push_route(StageRoute::Show {
                    id_or_uri: show_uri.clone(),
                    label,
                });
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::SelectShow(show_uri));
            }
            Message::PlayShow(show_uri) => {
                let (label, publisher, summary_opt) = self.resolve_show_meta(&show_uri);
                tracing::info!("[UI] PlayShow('{show_uri}') -> label='{label}'");
                if matches!(self.current_route(), StageRoute::Section(NavSection::Search))
                    && label != "Podcast"
                {
                    self.record_recent_search_entry(RecentSearchEntry {
                        label: label.clone(),
                        subtitle: format!("Podcast · {publisher}"),
                        query: label.clone(),
                        kind: RecentSearchKind::Show {
                            uri: show_uri.clone(),
                            publisher: publisher.clone(),
                        },
                    });
                }
                if label != "Podcast" {
                    let cover = summary_opt.as_ref().and_then(|s| s.cover_art_url.clone());
                    let theme = summary_opt
                        .as_ref()
                        .map(|s| s.artwork_theme)
                        .unwrap_or(spot_core::ArtworkTheme::SunriseMountainLake);
                    self.record_recently_listened(RecentListenedItem {
                        title: label,
                        subtitle: format!("Podcast · {publisher}"),
                        cover_art_url: cover,
                        artwork_theme: theme,
                        kind: RecentListenedKind::Podcast {
                            show_uri: show_uri.clone(),
                            publisher,
                        },
                    });
                }
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::PlayShow(show_uri));
                self.push_route(StageRoute::Section(NavSection::NowPlaying));
            }
            Message::PlayShowEpisode(ep_idx) => {
                if let Some(detail) = self.selected_show.clone() {
                    if let Some(chosen_ep) = detail.episodes.get(ep_idx) {
                        let chosen_track = chosen_ep.into_playable_track();
                        tracing::info!(
                            "[UI] PlayShowEpisode({ep_idx}) -> '{}' from show '{}' ({})",
                            chosen_track.title,
                            detail.show.title,
                            chosen_track.spotify_uri
                        );
                        self.record_recently_listened(RecentListenedItem {
                            title: detail.show.title.clone(),
                            subtitle: format!("Podcast · {}", detail.show.publisher),
                            cover_art_url: detail.show.cover_art_url.clone(),
                            artwork_theme: detail.show.artwork_theme,
                            kind: RecentListenedKind::Podcast {
                                show_uri: detail.show.spotify_uri.clone(),
                                publisher: detail.show.publisher.clone(),
                            },
                        });
                        let episodes: Vec<Track> = detail
                            .episodes
                            .iter()
                            .map(PodcastEpisode::into_playable_track)
                            .collect();
                        self.state.apply_verified_action(
                            VerifiedTransportAction::ActivatePodcastShow {
                                show_uri: detail.show.spotify_uri.clone(),
                                show_title: detail.show.title.clone(),
                                episodes,
                                start_episode_index: ep_idx,
                                auto_play: true,
                            },
                        );
                        self.engine
                            .audio_gate
                            .flush_and_set_playing(self.state.is_playing);
                        let _ =
                            self.engine
                                .command_tx
                                .send(PlayerCommand::PlayShowAtEpisode {
                                    show_uri: detail.show.spotify_uri.clone(),
                                    episode_index: ep_idx,
                                    preloaded_show: Some(detail),
                                });
                        self.push_route(StageRoute::Section(NavSection::NowPlaying));
                    }
                }
            }
            Message::PlaySearchEpisode(episode) => {
                let track = episode.into_playable_track();
                tracing::info!(
                    "[UI] PlaySearchEpisode -> '{}' ({})",
                    track.title,
                    track.spotify_uri
                );
                if matches!(self.current_route(), StageRoute::Section(NavSection::Search)) {
                    self.record_recent_search_entry(RecentSearchEntry {
                        label: episode.show_title.clone(),
                        subtitle: format!("Podcast · {}", episode.title),
                        query: episode.show_title.clone(),
                        kind: RecentSearchKind::Show {
                            uri: episode.show_uri.clone(),
                            publisher: episode.show_title.clone(),
                        },
                    });
                }
                let (_, publisher, _) = self.resolve_show_meta(&episode.show_uri);
                self.record_recently_listened(RecentListenedItem {
                    title: episode.show_title.clone(),
                    subtitle: format!("Podcast · {publisher}"),
                    cover_art_url: episode.cover_art_url.clone(),
                    artwork_theme: episode.artwork_theme,
                    kind: RecentListenedKind::Podcast {
                        show_uri: episode.show_uri.clone(),
                        publisher,
                    },
                });
                self.state
                    .apply_verified_action(VerifiedTransportAction::PlayTrack(track.clone()));
                self.engine
                    .audio_gate
                    .flush_and_set_playing(self.state.is_playing);
                let _ = self.engine.command_tx.send(PlayerCommand::PlayTrack(track));
                self.push_route(StageRoute::Section(NavSection::NowPlaying));
            }
            Message::ToggleShowBio => {
                self.show_bio_expanded = !self.show_bio_expanded;
            }
            Message::PodcastFilterChanged(query) => {
                self.text_input_edited_this_event = true;
                let trimmed = query.trim().to_string();
                self.podcast_filter_query = query;
                if spot_core::normalize_spotify_show_uri(&trimmed).is_some() {
                    return self.update(Message::OpenShow(trimmed));
                }
            }
            Message::ClearPodcastFilter => {
                self.podcast_filter_query.clear();
            }
            Message::ToggleRecentlyListenedExpanded => {
                self.recently_listened_expanded = !self.recently_listened_expanded;
                tracing::info!(
                    "[UI] ToggleRecentlyListenedExpanded -> expanded={} (showing {}/{} items)",
                    self.recently_listened_expanded,
                    self.recently_listened
                        .visible_entries(self.recently_listened_expanded)
                        .len(),
                    self.recently_listened.len()
                );
            }
            Message::CreateNewPlaylist => {
                let next_idx = self.state.playlists.len() + 1;
                let new_id = format!("custom-playlist-{next_idx}");
                tracing::info!("[UI] CreateNewPlaylist -> '{new_id}'");
                self.state.playlists.push(spot_core::Playlist {
                    id: new_id,
                    spotify_uri: format!("spotify:playlist:custom{next_idx}"),
                    name: format!("Late Night #{next_idx}"),
                    subtitle: "Custom Spot mixtape".into(),
                    track_count: 12,
                    is_playing_context: false,
                    artwork_theme: spot_core::ArtworkTheme::AlpineMist,
                });
            }
            Message::SelectOutputDevice(device) => {
                tracing::info!(
                    "[UI] SelectOutputDevice -> '{}' (id={})",
                    device.name,
                    device.id
                );
                select_device_verified(&mut self.state, device.clone());
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::SelectOutputDevice(device));
            }
            Message::ConnectSpotifyClicked { force_reauth } => {
                tracing::info!("[UI] ConnectSpotifyClicked(force_reauth={force_reauth})");
                let client_id = std::env::var("SPOTIFY_CLIENT_ID").ok();
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::AuthenticateSpotify {
                        client_id,
                        force_reauth,
                    });
            }
            Message::LogoutSpotifyClicked => {
                tracing::info!("[UI] LogoutSpotifyClicked");
                let _ = self.engine.command_tx.send(PlayerCommand::LogoutSpotify);
            }
            Message::ModifiersChanged(modifiers) => {
                self.keyboard_modifiers = modifiers;
            }
            Message::SpacePressed {
                status_captured,
                has_command_ctrl_alt,
            } => {
                let should_toggle = should_toggle_play_pause_on_space_verified(
                    true,
                    has_command_ctrl_alt,
                    self.space_key_held,
                    status_captured,
                    self.text_input_edited_this_event,
                );
                self.space_key_held = true;
                self.text_input_edited_this_event = false;
                if should_toggle {
                    self.state.is_playing = !self.state.is_playing;
                    self.engine.audio_gate.set_playing(self.state.is_playing);
                    tracing::info!(
                        "[UI] Spacebar TogglePlayPause -> is_playing={} (instant audio gate updated)",
                        self.state.is_playing
                    );
                    let _ = self.engine.command_tx.send(PlayerCommand::TogglePlayPause);
                }
            }
            Message::SpaceReleased => {
                self.space_key_held = false;
                self.text_input_edited_this_event = false;
            }
            Message::SearchQueryChanged(query) => {
                self.text_input_edited_this_event = true;
                let prev_query = self.search_query.clone();
                let is_single_char_backspace =
                    prev_query.chars().count() == query.chars().count() + 1
                        && prev_query.starts_with(&query);

                if is_single_char_backspace
                    && (self.keyboard_modifiers.command()
                        || self.keyboard_modifiers.macos_command())
                {
                    let shortened = delete_last_word_verified(&prev_query);
                    tracing::info!(
                        "[UI] Cmd+Backspace in Search: '{}' -> '{}'",
                        prev_query,
                        shortened
                    );
                    self.pre_backspace_search_query = None;
                    self.last_cmd_backspace_applied = true;
                    self.search_query = shortened.clone();
                    if shortened.trim().is_empty() {
                        self.search_artists.clear();
                        self.search_albums.clear();
                        self.search_shows.clear();
                        self.search_episodes.clear();
                        self.search_results.clear();
                    }
                    let _ = self
                        .engine
                        .command_tx
                        .send(PlayerCommand::SearchCatalog(shortened));
                    return text_input::move_cursor_to_end(search_input_id());
                }

                if is_single_char_backspace {
                    self.pre_backspace_search_query = Some(prev_query);
                } else {
                    self.pre_backspace_search_query = None;
                }
                self.last_cmd_backspace_applied = false;

                tracing::info!("[UI] SearchQueryChanged -> '{}'", query);
                self.search_query = query.clone();
                if query.trim().is_empty() {
                    self.search_artists.clear();
                    self.search_albums.clear();
                    self.search_shows.clear();
                    self.search_episodes.clear();
                    self.search_results.clear();
                }
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::SearchCatalog(query));
            }
            Message::SearchDeleteWord => {
                if self.last_cmd_backspace_applied {
                    self.last_cmd_backspace_applied = false;
                    return Task::none();
                }
                if matches!(self.current_route(), StageRoute::Section(NavSection::Search))
                    && !self.search_query.is_empty()
                {
                    let base = self
                        .pre_backspace_search_query
                        .take()
                        .unwrap_or_else(|| self.search_query.clone());
                    let shortened = delete_last_word_verified(&base);
                    tracing::info!(
                        "[UI] SearchDeleteWord (Cmd+Backspace) -> '{}' -> '{}'",
                        base,
                        shortened
                    );
                    self.search_query = shortened.clone();
                    if shortened.trim().is_empty() {
                        self.search_artists.clear();
                        self.search_albums.clear();
                        self.search_shows.clear();
                        self.search_episodes.clear();
                        self.search_results.clear();
                    }
                    let _ = self
                        .engine
                        .command_tx
                        .send(PlayerCommand::SearchCatalog(shortened));
                    return text_input::move_cursor_to_end(search_input_id());
                }
            }
            Message::SubmitSearch => {
                let trimmed = self.search_query.trim().to_string();
                if !trimmed.is_empty() {
                    tracing::info!("[UI] SubmitSearch -> '{trimmed}'");
                    self.record_recent_search_entry(RecentSearchEntry {
                        label: trimmed.clone(),
                        subtitle: "Search".to_string(),
                        query: trimmed.clone(),
                        kind: RecentSearchKind::Query,
                    });
                    let _ = self
                        .engine
                        .command_tx
                        .send(PlayerCommand::SearchCatalog(trimmed));
                }
            }
            Message::ClearSearchQuery => {
                tracing::info!("[UI] ClearSearchQuery");
                self.search_query.clear();
                self.search_artists.clear();
                self.search_albums.clear();
                self.search_shows.clear();
                self.search_episodes.clear();
                self.search_results.clear();
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::SearchCatalog(String::new()));
            }
            Message::SelectRecentSearch(idx) => {
                if let Some(entry) = self.recent_searches.get(idx).cloned() {
                    tracing::info!(
                        "[UI] SelectRecentSearch({idx}) -> '{}' ({:?})",
                        entry.label,
                        entry.kind
                    );
                    self.record_recent_search_entry(entry.clone());
                    match entry.kind {
                        RecentSearchKind::Query | RecentSearchKind::Track { .. } => {
                            self.search_query = entry.query.clone();
                            let _ = self
                                .engine
                                .command_tx
                                .send(PlayerCommand::SearchCatalog(entry.query));
                        }
                        RecentSearchKind::Artist { uri } => {
                            self.search_query = entry.query.clone();
                            let _ = self
                                .engine
                                .command_tx
                                .send(PlayerCommand::SearchCatalog(entry.query));
                            return self.update(Message::OpenArtist(uri));
                        }
                        RecentSearchKind::Album { uri, .. } => {
                            return self.update(Message::OpenAlbum(uri));
                        }
                        RecentSearchKind::Show { uri, .. } => {
                            return self.update(Message::OpenShow(uri));
                        }
                    }
                }
            }
            Message::RemoveRecentSearch(idx) => {
                if let Some(removed) = self.recent_searches.remove_at(idx) {
                    tracing::info!("[UI] RemoveRecentSearch({idx}) -> '{}'", removed.label);
                    save_recent_searches(self.recent_searches.entries());
                }
            }
            Message::ClearRecentSearches => {
                tracing::info!("[UI] ClearRecentSearches");
                self.recent_searches.clear();
                save_recent_searches(self.recent_searches.entries());
            }
            Message::ToggleTrackMenu => {
                self.show_track_details_banner = !self.show_track_details_banner;
            }
            Message::CheckForMainUpdate => {
                if !self.update_busy {
                    self.update_busy = true;
                    self.update_status_note = Some("Checking main releases…".to_string());
                    tracing::info!("[Updater] Checking GitHub releases for latest main push");
                    self.sync_tray();
                    return Task::perform(
                        async {
                            updater::fetch_latest_main_release()
                                .await
                                .map_err(|e| e.to_string())
                        },
                        Message::MainUpdateChecked,
                    );
                }
            }
            Message::MainUpdateChecked(res) => {
                self.update_busy = false;
                match res {
                    Ok(manifest) => {
                        let accepted = self.update_catalog.observe_remote_release(manifest.clone());
                        let _ = updater::save_update_catalog(&self.update_catalog);
                        debug_assert!(self.update_catalog.check_invariants().is_ok());
                        match self.update_catalog.evaluate_availability() {
                            UpdateAvailability::UpdateAvailable {
                                manifest: avail,
                                stored_archive_filename,
                            } => {
                                tracing::info!(
                                    "[Updater] New main build available: #{} ({}) [accepted={}]",
                                    avail.build_number,
                                    avail.short_sha,
                                    accepted
                                );
                                self.update_status_note = Some(if stored_archive_filename.is_some() {
                                    format!("Ready to install · {}", avail.short_sha)
                                } else {
                                    format!("Click to download · {}", avail.short_sha)
                                });
                            }
                            UpdateAvailability::UpToDate {
                                installed_build,
                                installed_short_sha,
                            } => {
                                tracing::info!(
                                    "[Updater] Up to date with main (build #{installed_build}, {installed_short_sha})"
                                );
                                self.update_status_note = Some("Up to date with main".to_string());
                            }
                        }
                    }
                    Err(err) => {
                        tracing::debug!("[Updater] Main update check note: {err}");
                        self.update_status_note = Some("Up to date (local build)".to_string());
                    }
                }
            }
            Message::DownloadMainUpdate(manifest) => {
                if !self.update_busy {
                    self.update_busy = true;
                    self.update_status_note = Some(format!(
                        "Downloading #{} ({})…",
                        manifest.build_number, manifest.short_sha
                    ));
                    tracing::info!(
                        "[Updater] Downloading main build #{} ({}) from {}",
                        manifest.build_number,
                        manifest.short_sha,
                        manifest.download_url
                    );
                    self.sync_tray();
                    return Task::perform(
                        async move {
                            updater::download_and_store_update(manifest)
                                .await
                                .map_err(|e| e.to_string())
                        },
                        Message::MainUpdateDownloaded,
                    );
                }
            }
            Message::MainUpdateDownloaded(res) => {
                self.update_busy = false;
                match res {
                    Ok((entry, path)) => {
                        tracing::info!(
                            "[Updater] Stored verified main update #{} ({}) at {}",
                            entry.manifest.build_number,
                            entry.manifest.short_sha,
                            path.display()
                        );
                        self.update_catalog = updater::load_update_catalog();
                        debug_assert!(self.update_catalog.check_invariants().is_ok());
                        self.update_status_note = Some(format!(
                            "Stored #{} · Click to install",
                            entry.manifest.build_number
                        ));
                    }
                    Err(err) => {
                        tracing::warn!("[Updater] Failed to download main update: {err}");
                        self.update_status_note = Some("Download failed — retry".to_string());
                    }
                }
            }
            Message::InstallStoredMainUpdate(entry) => {
                if !self.update_busy {
                    self.update_busy = true;
                    self.update_status_note = Some(format!(
                        "Installing #{} ({})…",
                        entry.manifest.build_number, entry.manifest.short_sha
                    ));
                    tracing::info!(
                        "[Updater] Installing stored build #{} ({})",
                        entry.manifest.build_number,
                        entry.manifest.short_sha
                    );
                    self.sync_tray();
                    return Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || {
                                updater::install_stored_update_entry(&entry)
                                    .map_err(|e| e.to_string())
                            })
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()))
                        },
                        Message::MainUpdateInstalled,
                    );
                }
            }
            Message::MainUpdateInstalled(res) => {
                self.update_busy = false;
                match res {
                    Ok(installed_path) => {
                        tracing::info!(
                            "[Updater] Installed Spot update to {}",
                            installed_path.display()
                        );
                        self.update_catalog = updater::load_update_catalog();
                        debug_assert!(self.update_catalog.check_invariants().is_ok());
                        self.update_status_note =
                            Some("Installed to /Applications · Relaunch".to_string());
                    }
                    Err(err) => {
                        tracing::warn!("[Updater] Install failed: {err}");
                        self.update_status_note = Some("Install failed — see logs".to_string());
                    }
                }
            }
            Message::ArtworkLoaded(Some((url, decoded))) => {
                tracing::info!("[UI] ArtworkLoaded -> cached texture for {}", url);
                self.artwork.insert_remote(url, decoded);
            }
            Message::ArtworkLoaded(None) => {}
        }
        self.sync_tray();
        Task::none()
    }

    fn sync_tray(&mut self) {
        self.engine.audio_gate.set_playing(self.state.is_playing);
        if let Err(err) = self.engine.audio_gate.check_sync_with_playback(&self.state) {
            panic!(
                "[FORMAL VERIFICATION FAILURE] Instant audio gate invariant violated ({}): {}",
                err.code, err.details
            );
        }
        if let Some(tray) = &mut self.tray {
            tray.sync(&self.state, &self.artwork);
            if let Some(snapshot) = tray.snapshot() {
                if let Err(err) = snapshot.check_sync_with_playback(&self.state) {
                    panic!(
                        "[FORMAL VERIFICATION FAILURE] Tray progress invariant violated ({}): {}",
                        err.code, err.details
                    );
                }
            }
        }
    }

    fn spawn_missing_artwork_tasks(&mut self) -> Task<Message> {
        let mut urls: Vec<String> = Vec::new();
        if let Some(u) = &self.state.current_track.cover_art_url {
            urls.push(u.clone());
        }
        for t in &self.state.queue {
            if let Some(u) = &t.cover_art_url {
                urls.push(u.clone());
            }
        }
        for a in &self.search_artists {
            if let Some(u) = &a.portrait_url {
                urls.push(u.clone());
            }
        }
        for alb in &self.search_albums {
            if let Some(u) = &alb.cover_art_url {
                urls.push(u.clone());
            }
        }
        for show in &self.search_shows {
            if let Some(u) = &show.cover_art_url {
                urls.push(u.clone());
            }
        }
        for ep in &self.search_episodes {
            if let Some(u) = &ep.cover_art_url {
                urls.push(u.clone());
            }
        }
        for show in &self.podcast_directory {
            if let Some(u) = &show.cover_art_url {
                urls.push(u.clone());
            }
        }
        for item in self.recently_listened.entries() {
            if let Some(u) = &item.cover_art_url {
                urls.push(u.clone());
            }
        }
        for t in &self.search_results {
            if let Some(u) = &t.cover_art_url {
                urls.push(u.clone());
            }
        }
        if let Some(detail) = &self.selected_artist {
            if let Some(u) = &detail.artist.portrait_url {
                urls.push(u.clone());
            }
            for alb in &detail.albums {
                if let Some(u) = &alb.cover_art_url {
                    urls.push(u.clone());
                }
            }
            for t in &detail.top_tracks {
                if let Some(u) = &t.cover_art_url {
                    urls.push(u.clone());
                }
            }
        }
        if let Some(detail) = &self.selected_album {
            if let Some(u) = &detail.album.cover_art_url {
                urls.push(u.clone());
            }
            for t in &detail.tracks {
                if let Some(u) = &t.cover_art_url {
                    urls.push(u.clone());
                }
            }
        }
        if let Some(detail) = &self.selected_show {
            if let Some(u) = &detail.show.cover_art_url {
                urls.push(u.clone());
            }
            for ep in &detail.episodes {
                if let Some(u) = &ep.cover_art_url {
                    urls.push(u.clone());
                }
            }
        }

        let mut tasks = Vec::new();
        for url in urls {
            if self.artwork.mark_in_flight(&url) {
                tasks.push(Task::perform(
                    download_and_decode_artwork(url),
                    Message::ArtworkLoaded,
                ));
            }
        }
        if tasks.is_empty() {
            Task::none()
        } else {
            tracing::info!("[UI] Spawning {} background artwork download task(s)", tasks.len());
            Task::batch(tasks)
        }
    }

    fn apply_engine_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::StateSynced(new_state) => {
                tracing::info!(
                    "[UI] EngineEvent::StateSynced (track='{}', playing={}, queue={}, playlists={}, connection={:?})",
                    new_state.current_track.title,
                    new_state.is_playing,
                    new_state.queue.len(),
                    new_state.playlists.len(),
                    new_state.connection
                );
                let scrubbing = self.scrubbing_position;
                self.state = *new_state;
                if let Some(pos) = scrubbing {
                    self.state.position_secs = pos;
                }
            }
            EngineEvent::PositionTick(pos) => {
                if self.scrubbing_position.is_none() {
                    self.state.position_secs =
                        sanitize_position_secs(pos, self.state.current_track.duration_secs);
                }
            }
            EngineEvent::PlayStateChanged(playing) => {
                tracing::info!("[UI] EngineEvent::PlayStateChanged({playing})");
                self.state.is_playing = playing;
            }
            EngineEvent::TrackChanged(track) => {
                tracing::info!(
                    "[UI] EngineEvent::TrackChanged -> '{}' by '{}'",
                    track.title,
                    track.artist
                );
                self.state.current_track = track;
            }
            EngineEvent::QueueUpdated(queue) => {
                tracing::info!("[UI] EngineEvent::QueueUpdated ({} tracks)", queue.len());
                self.state.queue = queue;
            }
            EngineEvent::PlaylistsLoaded(playlists) => {
                tracing::info!(
                    "[UI] EngineEvent::PlaylistsLoaded ({} playlists)",
                    playlists.len()
                );
                self.state.playlists = playlists;
            }
            EngineEvent::DevicesUpdated(devices) => {
                tracing::info!(
                    "[UI] EngineEvent::DevicesUpdated ({} devices)",
                    devices.len()
                );
                merge_devices_verified(&mut self.state, devices);
            }
            EngineEvent::ConnectionUpdated(conn) => {
                tracing::info!("[UI] EngineEvent::ConnectionUpdated -> {:?}", conn);
                self.state.connection = conn;
            }
            EngineEvent::SearchResults {
                query,
                artists,
                albums,
                shows,
                episodes,
                tracks,
            } => {
                tracing::info!(
                    "[UI] EngineEvent::SearchResults(query='{}') -> {} artists, {} albums, {} shows, {} episodes, {} tracks",
                    query,
                    artists.len(),
                    albums.len(),
                    shows.len(),
                    episodes.len(),
                    tracks.len()
                );
                if query.trim().len() >= 2
                    && (!artists.is_empty()
                        || !albums.is_empty()
                        || !shows.is_empty()
                        || !episodes.is_empty()
                        || !tracks.is_empty())
                {
                    self.record_recent_query_from_search(&query);
                }
                self.search_artists = artists;
                self.search_albums = albums;
                self.search_shows = shows;
                self.search_episodes = episodes;
                self.search_results = tracks;
            }
            EngineEvent::ArtistDetailLoaded(detail) => {
                tracing::info!(
                    "[UI] EngineEvent::ArtistDetailLoaded('{}') -> {} albums, {} top tracks",
                    detail.artist.name,
                    detail.albums.len(),
                    detail.top_tracks.len()
                );
                let loaded_name = detail.artist.name.clone();
                let loaded_uri = detail.artist.spotify_uri.clone();
                self.selected_artist = Some(*detail);
                self.artist_bio_expanded = false;
                self.artist_top_tracks_expanded = false;
                if let StageRoute::Artist { id_or_uri, label } = self.nav_history.current_mut() {
                    *id_or_uri = loaded_uri;
                    *label = loaded_name;
                }
            }
            EngineEvent::AlbumDetailLoaded(detail) => {
                tracing::info!(
                    "[UI] EngineEvent::AlbumDetailLoaded('{}' by '{}') -> {} tracks",
                    detail.album.title,
                    detail.album.artist_name,
                    detail.tracks.len()
                );
                let loaded_title = detail.album.title.clone();
                let loaded_uri = detail.album.spotify_uri.clone();
                let loaded_artist_name = detail.album.artist_name.clone();
                let loaded_artist_uri = detail.artist_uri.clone();
                if self.state.is_playing
                    && (self
                        .state
                        .current_track
                        .album
                        .eq_ignore_ascii_case(&detail.album.title)
                        || detail
                            .tracks
                            .iter()
                            .any(|t| t.spotify_uri == self.state.current_track.spotify_uri))
                {
                    self.record_recently_listened(RecentListenedItem {
                        title: detail.album.title.clone(),
                        subtitle: format!(
                            "Album · {} · {}",
                            detail.album.artist_name, detail.album.release_year
                        ),
                        cover_art_url: detail.album.cover_art_url.clone(),
                        artwork_theme: detail.album.artwork_theme,
                        kind: RecentListenedKind::Album {
                            album_uri: detail.album.spotify_uri.clone(),
                            artist_name: detail.album.artist_name.clone(),
                        },
                    });
                }
                self.selected_album = Some(*detail);
                if let StageRoute::Album {
                    id_or_uri,
                    label,
                    artist_name,
                    artist_uri,
                } = self.nav_history.current_mut()
                {
                    *id_or_uri = loaded_uri;
                    *label = loaded_title;
                    *artist_name = Some(loaded_artist_name);
                    if loaded_artist_uri.is_some() {
                        *artist_uri = loaded_artist_uri;
                    }
                }
            }
            EngineEvent::ShowDetailLoaded(detail) => {
                tracing::info!(
                    "[UI] EngineEvent::ShowDetailLoaded('{}' by '{}') -> {} episodes",
                    detail.show.title,
                    detail.show.publisher,
                    detail.episodes.len()
                );
                let loaded_title = detail.show.title.clone();
                let loaded_uri = detail.show.spotify_uri.clone();
                if self.state.is_playing
                    && (self
                        .state
                        .current_track
                        .album
                        .eq_ignore_ascii_case(&detail.show.title)
                        || detail
                            .episodes
                            .iter()
                            .any(|ep| ep.spotify_uri == self.state.current_track.spotify_uri))
                {
                    self.record_recently_listened(RecentListenedItem {
                        title: detail.show.title.clone(),
                        subtitle: format!("Podcast · {}", detail.show.publisher),
                        cover_art_url: detail.show.cover_art_url.clone(),
                        artwork_theme: detail.show.artwork_theme,
                        kind: RecentListenedKind::Podcast {
                            show_uri: detail.show.spotify_uri.clone(),
                            publisher: detail.show.publisher.clone(),
                        },
                    });
                }
                self.selected_show = Some(*detail);
                self.show_bio_expanded = false;
                if let StageRoute::Show { id_or_uri, label } = self.nav_history.current_mut() {
                    *id_or_uri = loaded_uri;
                    *label = loaded_title;
                }
            }
        }
        self.engine.audio_gate.set_playing(self.state.is_playing);
    }

    pub fn view(&self) -> Element<'_, Message> {
        let sidebar = self.view_sidebar();
        let center_stage = match self.current_route() {
            StageRoute::Artist { .. } => self.view_artist_stage(),
            StageRoute::Album { .. } => self.view_album_stage(),
            StageRoute::Show { .. } => self.view_show_stage(),
            StageRoute::Section(NavSection::NowPlaying) => self.view_now_playing_stage(),
            StageRoute::Section(NavSection::Search) => self.view_search_stage(),
            StageRoute::Section(NavSection::Podcasts) => self.view_podcasts_stage(),
            StageRoute::Section(NavSection::Home | NavSection::YourLibrary) => {
                self.view_library_stage()
            }
        };
        let queue_pane = self.view_queue_pane();

        row![sidebar, center_stage, queue_pane]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Column 1: Frosted warm-stone left sidebar.
    fn view_sidebar(&self) -> Element<'_, Message> {
        let brand_header = row![
            image(self.sidebar_app_icon.clone())
                .width(Length::Fixed(34.0))
                .height(Length::Fixed(34.0)),
            column![
                text("Spot")
                    .font(SERIF_FONT)
                    .size(28)
                    .color(palette::INK_PRIMARY),
                text("A natural home\nfor your music")
                    .size(12)
                    .line_height(1.25)
                    .color(palette::INK_SECONDARY),
            ]
            .spacing(2)
        ]
        .spacing(12)
        .align_y(Alignment::Start)
        .padding(Padding {
            top: 4.0,
            right: 8.0,
            bottom: 10.0,
            left: 8.0,
        });

        let nav_items = column![
            self.view_nav_item(NavSection::Home, icons::nav_home()),
            self.view_nav_item(NavSection::Search, icons::nav_search()),
            self.view_nav_item(NavSection::Podcasts, icons::nav_podcasts()),
            self.view_nav_item(NavSection::NowPlaying, icons::nav_now_playing()),
            self.view_nav_item(NavSection::YourLibrary, icons::nav_library()),
        ]
        .spacing(4);

        let playlists_header = row![
            text("Playlists").size(12.5).color(palette::INK_SECONDARY),
            horizontal_space(),
            button(
                svg(icons::plus_icon())
                    .width(Length::Fixed(14.0))
                    .height(Length::Fixed(14.0))
            )
            .on_press(Message::CreateNewPlaylist)
            .padding(4)
            .style(theme::icon_ghost_button)
        ]
        .align_y(Alignment::Center)
        .padding(Padding {
            top: 6.0,
            right: 6.0,
            bottom: 4.0,
            left: 8.0,
        });

        let mut playlist_col = column![].spacing(4);
        for playlist in &self.state.playlists {
            let thumb = image(self.artwork.thumb_handle(playlist.artwork_theme))
                .width(Length::Fixed(32.0))
                .height(Length::Fixed(32.0));

            let active_dot: Element<'_, Message> = if playlist.is_playing_context {
                container(text(""))
                    .width(Length::Fixed(7.0))
                    .height(Length::Fixed(7.0))
                    .style(|_| container::Style {
                        background: Some(iced::Background::Color(palette::EMERALD)),
                        border: iced::Border {
                            radius: 4.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    })
                    .into()
            } else {
                horizontal_space().width(Length::Fixed(7.0)).into()
            };

            let item_row = row![
                thumb,
                text(&playlist.name)
                    .size(13.5)
                    .color(palette::INK_PRIMARY)
                    .width(Length::Fill),
                active_dot
            ]
            .spacing(11)
            .align_y(Alignment::Center);

            playlist_col = playlist_col.push(
                button(item_row)
                    .on_press(Message::SelectPlaylist(playlist.id.clone()))
                    .width(Length::Fill)
                    .padding(Padding::from([5, 8]))
                    .style(theme::subtle_row_button(playlist.is_playing_context)),
            );
        }

        let playlists_scroll = scrollable(playlist_col).height(Length::Fill);

        let conn_label = self.state.connection.display_label();
        let is_live = self.state.connection.is_live_session();
        let dot_color = if is_live {
            palette::EMERALD
        } else {
            iced::Color::from_rgb8(0xD9, 0x8A, 0x3C)
        };
        let status_dot = container(text(""))
            .width(Length::Fixed(6.5))
            .height(Length::Fixed(6.5))
            .style(move |_| container::Style {
                background: Some(iced::Background::Color(dot_color)),
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            });

        let footer_card = button(
            row![
                svg(icons::spotify_badge())
                    .width(Length::Fixed(24.0))
                    .height(Length::Fixed(24.0)),
                column![
                    text(if is_live {
                        "Spotify streaming"
                    } else {
                        "Connect Spotify"
                    })
                    .size(12.5)
                    .color(palette::INK_PRIMARY),
                    row![
                        status_dot,
                        text(conn_label).size(11.0).color(palette::INK_SECONDARY)
                    ]
                    .spacing(5)
                    .align_y(Alignment::Center),
                ]
                .spacing(2)
                .width(Length::Fill),
                text("›").size(18).color(palette::INK_SECONDARY),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .on_press(Message::ConnectSpotifyClicked {
            force_reauth: is_live,
        })
        .width(Length::Fill)
        .padding(Padding::from([8, 8]))
        .style(theme::subtle_row_button(false));

        let update_card = self.view_update_card();

        let content = column![
            vertical_space().height(Length::Fixed(34.0)),
            brand_header,
            vertical_space().height(Length::Fixed(6.0)),
            nav_items,
            vertical_space().height(Length::Fixed(10.0)),
            horizontal_rule(1),
            playlists_header,
            playlists_scroll,
            horizontal_rule(1),
            update_card,
            footer_card,
        ]
        .spacing(6)
        .padding(Padding {
            top: 10.0,
            right: 16.0,
            bottom: 14.0,
            left: 16.0,
        });

        container(content)
            .width(Length::Fixed(246.0))
            .height(Length::Fill)
            .style(theme::sidebar_container)
            .into()
    }

    fn view_update_card(&self) -> Element<'_, Message> {
        let availability = self.update_catalog.evaluate_availability();
        let (headline, subline, has_update, action_msg) = match availability {
            UpdateAvailability::UpdateAvailable {
                manifest,
                stored_archive_filename,
            } => {
                if let Some(stored) = self
                    .update_catalog
                    .find_stored(&manifest.commit_sha)
                    .cloned()
                {
                    let _ = stored_archive_filename;
                    (
                        format!(
                            "Update ready · #{} ({})",
                            manifest.build_number, manifest.short_sha
                        ),
                        self.update_status_note
                            .clone()
                            .unwrap_or_else(|| "Stored · Click to install".to_string()),
                        true,
                        Message::InstallStoredMainUpdate(stored),
                    )
                } else {
                    (
                        format!(
                            "Update available · #{} ({})",
                            manifest.build_number, manifest.short_sha
                        ),
                        self.update_status_note
                            .clone()
                            .unwrap_or_else(|| "Download latest main build".to_string()),
                        true,
                        Message::DownloadMainUpdate(manifest),
                    )
                }
            }
            UpdateAvailability::UpToDate {
                installed_build,
                installed_short_sha,
            } => {
                let stored_count = self.update_catalog.stored_updates().len();
                let default_sub = if stored_count > 0 {
                    format!("{stored_count} stored · Check updates")
                } else {
                    "Check for main updates".to_string()
                };
                (
                    format!("Spot #{installed_build} · {installed_short_sha}"),
                    self.update_status_note.clone().unwrap_or(default_sub),
                    false,
                    Message::CheckForMainUpdate,
                )
            }
        };

        let dot_color = if has_update {
            palette::EMERALD
        } else {
            palette::INK_MUTED
        };
        let update_dot = container(text(""))
            .width(Length::Fixed(6.5))
            .height(Length::Fixed(6.5))
            .style(move |_| container::Style {
                background: Some(iced::Background::Color(dot_color)),
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            });

        button(
            row![
                column![
                    row![
                        update_dot,
                        text(headline).size(11.5).color(palette::INK_PRIMARY),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center),
                    text(subline).size(10.5).color(palette::INK_SECONDARY),
                ]
                .spacing(2)
                .width(Length::Fill),
                text(if has_update { "↓" } else { "↻" })
                    .size(13)
                    .color(if has_update {
                        palette::EMERALD
                    } else {
                        palette::INK_SECONDARY
                    }),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .on_press(action_msg)
        .width(Length::Fill)
        .padding(Padding::from([6, 8]))
        .style(theme::subtle_row_button(has_update))
        .into()
    }

    fn view_nav_item<'a>(
        &self,
        section: NavSection,
        icon_handle: svg::Handle,
    ) -> Element<'a, Message> {
        let is_active = matches!(self.current_route(), StageRoute::Section(sec) if *sec == section);
        let label_text = text(section.label())
            .size(14)
            .color(palette::INK_PRIMARY);

        let content = row![
            svg(icon_handle)
                .width(Length::Fixed(18.0))
                .height(Length::Fixed(18.0)),
            label_text,
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        button(content)
            .on_press(Message::SelectNav(section))
            .width(Length::Fill)
            .padding(Padding::from([9, 12]))
            .style(theme::nav_pill_button(is_active))
            .into()
    }

    /// Lightweight, unboxed contextual back/forward navigation row for drill-down views
    /// (`Artist` and `Album`), avoiding any duplication of the left-hand sidebar nav.
    fn view_navigation_bar(&self) -> Element<'_, Message> {
        let mut nav_row = row![].spacing(8).align_y(Alignment::Center);

        match self.current_route() {
            StageRoute::Album {
                artist_name,
                artist_uri,
                ..
            } => {
                let prev_is_same_artist = matches!(
                    (self.nav_history.previous_entry(), artist_name),
                    (Some(StageRoute::Artist { label, .. }), Some(a_name))
                        if label.eq_ignore_ascii_case(a_name)
                );

                if prev_is_same_artist {
                    let artist_label = artist_name
                        .as_deref()
                        .map(|n| truncate_nav_label(n, 24))
                        .unwrap_or_else(|| "Artist".to_string());
                    nav_row = nav_row.push(
                        button(
                            row![
                                text("←").size(13).color(palette::EMERALD),
                                text(artist_label).size(12.5).color(palette::INK_PRIMARY),
                            ]
                            .spacing(6)
                            .align_y(Alignment::Center),
                        )
                        .on_press(Message::NavigateBack)
                        .padding(Padding::from([6, 12]))
                        .style(theme::nav_pill_button(false)),
                    );
                } else {
                    if self.can_go_back() {
                        let prev_label = self
                            .nav_history
                            .previous_entry()
                            .map(|r| truncate_nav_label(r.label(), 20))
                            .unwrap_or_else(|| "Back".to_string());
                        nav_row = nav_row.push(
                            button(
                                row![
                                    text("←").size(13).color(palette::EMERALD),
                                    text(prev_label).size(12.5).color(palette::INK_PRIMARY),
                                ]
                                .spacing(6)
                                .align_y(Alignment::Center),
                            )
                            .on_press(Message::NavigateBack)
                            .padding(Padding::from([6, 12]))
                            .style(theme::nav_pill_button(false)),
                        );
                    }
                    if let Some(a_name) = artist_name {
                        let target = artist_uri.clone().unwrap_or_else(|| a_name.clone());
                        nav_row = nav_row.push(
                            button(
                                text(format!("{} ›", truncate_nav_label(a_name, 22)))
                                    .size(12.0)
                                    .color(palette::INK_SECONDARY),
                            )
                            .on_press(Message::OpenArtist(target))
                            .padding(Padding::from([6, 12]))
                            .style(theme::subtle_row_button(false)),
                        );
                    }
                }
            }
            StageRoute::Artist { .. } => {
                let back_label = self
                    .nav_history
                    .previous_entry()
                    .map(|r| truncate_nav_label(r.label(), 20))
                    .unwrap_or_else(|| "Search".to_string());
                let back_msg = if self.can_go_back() {
                    Message::NavigateBack
                } else {
                    Message::CloseArtistView
                };
                nav_row = nav_row.push(
                    button(
                        row![
                            text("←").size(13).color(palette::EMERALD),
                            text(back_label).size(12.5).color(palette::INK_PRIMARY),
                        ]
                        .spacing(6)
                        .align_y(Alignment::Center),
                    )
                    .on_press(back_msg)
                    .padding(Padding::from([6, 12]))
                    .style(theme::nav_pill_button(false)),
                );
            }
            StageRoute::Show { .. } => {
                let back_label = self
                    .nav_history
                    .previous_entry()
                    .map(|r| truncate_nav_label(r.label(), 20))
                    .unwrap_or_else(|| "Podcasts".to_string());
                let back_msg = if self.can_go_back() {
                    Message::NavigateBack
                } else {
                    Message::SelectNav(NavSection::Podcasts)
                };
                nav_row = nav_row.push(
                    button(
                        row![
                            text("←").size(13).color(palette::EMERALD),
                            text(back_label).size(12.5).color(palette::INK_PRIMARY),
                        ]
                        .spacing(6)
                        .align_y(Alignment::Center),
                    )
                    .on_press(back_msg)
                    .padding(Padding::from([6, 12]))
                    .style(theme::nav_pill_button(false)),
                );
            }
            StageRoute::Section(_) => {}
        }

        // Only show a forward pill if forward history points to an Artist, Album, or Show sub-page
        if self.can_go_forward() {
            if let Some(
                StageRoute::Artist { label, .. }
                | StageRoute::Album { label, .. }
                | StageRoute::Show { label, .. },
            ) = self.nav_history.next_entry()
            {
                nav_row = nav_row.push(
                    button(
                        row![
                            text(truncate_nav_label(label, 20))
                                .size(12.0)
                                .color(palette::INK_SECONDARY),
                            text("→").size(13).color(palette::INK_MUTED),
                        ]
                        .spacing(5)
                        .align_y(Alignment::Center),
                    )
                    .on_press(Message::NavigateForward)
                    .padding(Padding::from([6, 11]))
                    .style(theme::subtle_row_button(false)),
                );
            }
        }

        nav_row.into()
    }

    /// Column 2: Main "Now Playing" center stage.
    fn view_now_playing_stage(&self) -> Element<'_, Message> {
        let track = &self.state.current_track;

        let hero_art = container(
            image(
                self.artwork
                    .hero_or_url(track.cover_art_url.as_deref(), track.artwork_theme),
            )
            .width(Length::Fixed(352.0))
            .height(Length::Fixed(352.0)),
        )
        .style(theme::album_art_shadow_card);

        let is_episode = track.spotify_uri.starts_with("spotify:episode:");

        let artist_link_btn = button(
            row![
                text(&track.artist)
                    .font(SERIF_FONT)
                    .size(18)
                    .color(palette::INK_SECONDARY),
                text("›").size(15).color(palette::INK_MUTED),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .on_press(if is_episode {
            Message::OpenShow(track.artist.clone())
        } else {
            Message::OpenArtist(track.artist.clone())
        })
        .padding(Padding::from([1, 4]))
        .style(theme::icon_ghost_button);

        let album_link_btn = button(
            row![
                text(if is_episode {
                    "Podcast Episode"
                } else {
                    &track.album
                })
                .size(13.5)
                .color(palette::INK_MUTED),
                text("›").size(13).color(palette::INK_MUTED),
            ]
            .spacing(3)
            .align_y(Alignment::Center),
        )
        .on_press(if is_episode {
            Message::OpenShow(track.album.clone())
        } else {
            Message::OpenAlbum(track.album.clone())
        })
        .padding(Padding::from([1, 4]))
        .style(theme::icon_ghost_button);

        let subtitle_links = row![
            artist_link_btn,
            text("•").size(12).color(palette::INK_MUTED),
            album_link_btn,
        ]
        .spacing(4)
        .align_y(Alignment::Center);

        let title_and_actions = row![
            column![
                text(&track.title)
                    .font(SERIF_FONT)
                    .size(31)
                    .color(palette::INK_PRIMARY),
                subtitle_links,
            ]
            .spacing(2)
            .width(Length::Fill),
            row![
                button(
                    svg(icons::heart_icon(track.liked))
                        .width(Length::Fixed(20.0))
                        .height(Length::Fixed(20.0))
                )
                .on_press(Message::ToggleLike)
                .padding(8)
                .style(theme::icon_ghost_button),
                button(
                    svg(icons::dots_horizontal())
                        .width(Length::Fixed(20.0))
                        .height(Length::Fixed(20.0))
                )
                .on_press(Message::ToggleTrackMenu)
                .padding(8)
                .style(theme::icon_ghost_button),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        ]
        .align_y(Alignment::Center)
        .width(Length::Fixed(430.0));

        let telemetry_badge: Element<'_, Message> = if self.show_track_details_banner {
            container(
                text(format!(
                    "320 kbps Ogg Vorbis • 32-bit F32 DSP • Album: {} • URI: {}",
                    track.album, track.spotify_uri
                ))
                .size(11.5)
                .color(palette::INK_SECONDARY),
            )
            .padding(Padding::from([4, 10]))
            .style(|_| container::Style {
                background: Some(iced::Background::Color(palette::DEVICE_PILL_BG)),
                border: iced::Border {
                    radius: 8.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into()
        } else {
            vertical_space().height(Length::Fixed(0.0)).into()
        };

        let visualizer_row = container(RibbonVisualizer::view(self.spectrum))
            .width(Length::Fixed(430.0))
            .height(Length::Fixed(78.0));

        let duration_f = (track.duration_secs.max(1)) as f32;
        let current_pos = self.state.synced_position_secs().clamp(0.0, duration_f);

        let seek_slider = slider(0.0..=duration_f, current_pos, Message::SeekChanged)
            .on_release(Message::SeekReleased)
            .step(0.25_f32)
            .width(Length::Fixed(430.0))
            .style(theme::seek_slider_style);

        let timestamps_row = row![
            text(format_seconds(self.state.synced_elapsed_secs()))
                .size(11.5)
                .color(palette::INK_MUTED),
            horizontal_space(),
            text(format_seconds(track.duration_secs))
                .size(11.5)
                .color(palette::INK_MUTED),
        ]
        .width(Length::Fixed(430.0));

        let scrubber_block = column![seek_slider, timestamps_row].spacing(4);

        let play_pause_icon = if self.state.is_playing {
            icons::pause_icon()
        } else {
            icons::play_icon()
        };

        let transport_row = row![
            button(
                svg(icons::shuffle_icon(self.state.shuffle))
                    .width(Length::Fixed(18.0))
                    .height(Length::Fixed(18.0))
            )
            .on_press(Message::ToggleShuffle)
            .padding(10)
            .style(theme::icon_ghost_button),
            button(
                svg(icons::prev_icon())
                    .width(Length::Fixed(20.0))
                    .height(Length::Fixed(20.0))
            )
            .on_press(Message::PreviousTrack)
            .padding(10)
            .style(theme::icon_ghost_button),
            button(
                container(
                    svg(play_pause_icon)
                        .width(Length::Fixed(22.0))
                        .height(Length::Fixed(22.0))
                )
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Horizontal::Center)
                .align_y(Vertical::Center)
            )
            .on_press(Message::TogglePlayPause)
            .width(Length::Fixed(56.0))
            .height(Length::Fixed(56.0))
            .style(theme::play_circle_button),
            button(
                svg(icons::next_icon())
                    .width(Length::Fixed(20.0))
                    .height(Length::Fixed(20.0))
            )
            .on_press(Message::NextTrack)
            .padding(10)
            .style(theme::icon_ghost_button),
            button(
                svg(icons::repeat_icon(self.state.repeat != RepeatMode::Off))
                    .width(Length::Fixed(18.0))
                    .height(Length::Fixed(18.0))
            )
            .on_press(Message::CycleRepeat)
            .padding(10)
            .style(theme::icon_ghost_button),
        ]
        .spacing(24)
        .align_y(Alignment::Center);

        let volume_control = row![
            svg(icons::volume_icon())
                .width(Length::Fixed(16.0))
                .height(Length::Fixed(16.0)),
            slider(0.0..=1.0, self.state.volume, Message::VolumeChanged)
                .step(0.01_f32)
                .width(Length::Fixed(165.0))
                .style(theme::volume_slider_style),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let device_picker = container(
            row![
                svg(icons::airplay_speaker_icon())
                    .width(Length::Fixed(15.0))
                    .height(Length::Fixed(15.0)),
                pick_list(
                    self.state.devices.as_slice(),
                    Some(self.state.selected_device.clone()),
                    Message::SelectOutputDevice,
                )
                .text_size(12.0)
                .padding(Padding::from([4, 8]))
                .style(theme::device_pick_list_style),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .padding(Padding {
                top: 2.0,
                right: 4.0,
                bottom: 2.0,
                left: 10.0,
            }),
        )
        .style(|_| container::Style {
            background: Some(iced::Background::Color(palette::DEVICE_PILL_BG)),
            border: iced::Border {
                radius: 16.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });

        let utility_row = row![volume_control, horizontal_space(), device_picker]
            .align_y(Alignment::Center)
            .width(Length::Fixed(450.0));

        let stage_column = column![
            vertical_space(),
            hero_art,
            vertical_space().height(Length::Fixed(18.0)),
            title_and_actions,
            telemetry_badge,
            vertical_space().height(Length::Fixed(4.0)),
            visualizer_row,
            scrubber_block,
            vertical_space().height(Length::Fixed(10.0)),
            transport_row,
            vertical_space().height(Length::Fixed(18.0)),
            utility_row,
            vertical_space(),
        ]
        .align_x(Alignment::Center)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(Padding::from([20, 32]));

        container(stage_column)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center)
            .style(theme::center_stage_container)
            .into()
    }

    fn view_spotify_auth_banner(&self) -> Option<Element<'_, Message>> {
        match &self.state.connection {
            ConnectionStatus::Connected {
                live_session: true, ..
            } => None,

            ConnectionStatus::Connecting { status } => Some(
                container(
                    row![
                        svg(icons::spotify_badge())
                            .width(Length::Fixed(18.0))
                            .height(Length::Fixed(18.0)),
                        text(status)
                            .size(12.5)
                            .color(palette::INK_PRIMARY)
                            .width(Length::Fill),
                        button(text("Retry Browser Login").size(11.5))
                            .on_press(Message::ConnectSpotifyClicked { force_reauth: true })
                            .padding(Padding::from([5, 12]))
                            .style(theme::subtle_row_button(true)),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([9, 12]))
                .style(|_| container::Style {
                    background: Some(iced::Background::Color(palette::DEVICE_PILL_BG)),
                    border: iced::Border {
                        radius: 12.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .into(),
            ),

            _ => Some(
                container(
                    row![
                        svg(icons::spotify_badge())
                            .width(Length::Fixed(18.0))
                            .height(Length::Fixed(18.0)),
                        column![
                            text("Sign in with Spotify Premium")
                                .size(13.0)
                                .color(palette::INK_PRIMARY),
                            text("Opens Spotify PKCE login in your browser for live 320 kbps streaming, artist albums, and cover art.")
                                .size(11.5)
                                .color(palette::INK_SECONDARY),
                        ]
                        .spacing(1)
                        .width(Length::Fill),
                        button(text("Connect Spotify").size(12.0))
                            .on_press(Message::ConnectSpotifyClicked {
                                force_reauth: false
                            })
                            .padding(Padding::from([7, 14]))
                            .style(theme::subtle_row_button(true)),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([10, 14]))
                .style(|_| container::Style {
                    background: Some(iced::Background::Color(palette::SIDEBAR_ACTIVE_PILL)),
                    border: iced::Border {
                        radius: 12.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .into(),
            ),
        }
    }

    fn view_search_stage(&self) -> Element<'_, Message> {
        let header = row![
            text("Search & Discover")
                .font(SERIF_FONT)
                .size(32)
                .color(palette::INK_PRIMARY),
            horizontal_space(),
        ]
        .align_y(Alignment::Center);

        let input = text_input(
            "Search artists, albums, podcasts, or tracks on Spotify...",
            &self.search_query,
        )
        .id(search_input_id())
        .on_input(Message::SearchQueryChanged)
        .on_submit(Message::SubmitSearch)
        .padding(12)
        .size(14)
        .width(Length::Fill);

        let mut search_bar_row = row![input].spacing(8).align_y(Alignment::Center);
        if !self.search_query.is_empty() {
            search_bar_row = search_bar_row.push(
                button(text("Clear").size(12.0).color(palette::INK_SECONDARY))
                    .on_press(Message::ClearSearchQuery)
                    .padding(Padding::from([10, 14]))
                    .style(theme::subtle_row_button(false)),
            );
        }

        let mut content_col = column![].spacing(18);

        // 0. Recently Searched section (persisted across sessions)
        if !self.recent_searches.is_empty() {
            let recent_header = row![
                text("Recently Searched")
                    .font(SERIF_FONT)
                    .size(20)
                    .color(palette::INK_PRIMARY),
                horizontal_space(),
                button(text("Clear All").size(11.5).color(palette::INK_MUTED))
                    .on_press(Message::ClearRecentSearches)
                    .padding(Padding::from([4, 9]))
                    .style(theme::subtle_row_button(false)),
            ]
            .align_y(Alignment::Center);

            let mut chips_grid = column![].spacing(7);
            let indexed_recent: Vec<(usize, &RecentSearchEntry)> = self
                .recent_searches
                .entries()
                .iter()
                .enumerate()
                .take(6)
                .collect();
            for chunk in indexed_recent.chunks(3) {
                let mut r = row![].spacing(8);
                for (idx, entry) in chunk {
                    let badge_color = match entry.kind {
                        RecentSearchKind::Artist { .. }
                        | RecentSearchKind::Album { .. }
                        | RecentSearchKind::Show { .. } => palette::EMERALD,
                        _ => palette::INK_MUTED,
                    };
                    let chip = container(
                        row![
                            button(
                                column![
                                    text(entry.kind.badge_label())
                                        .size(9.5)
                                        .color(badge_color),
                                    text(truncate_nav_label(&entry.label, 22))
                                        .size(13.0)
                                        .color(palette::INK_PRIMARY),
                                ]
                                .spacing(1)
                                .width(Length::Fill),
                            )
                            .on_press(Message::SelectRecentSearch(*idx))
                            .width(Length::Fill)
                            .padding(Padding::from([6, 9]))
                            .style(theme::subtle_row_button(false)),
                            button(text("×").size(13.5).color(palette::INK_MUTED))
                                .on_press(Message::RemoveRecentSearch(*idx))
                                .padding(Padding::from([5, 8]))
                                .style(theme::subtle_row_button(false)),
                        ]
                        .spacing(2)
                        .align_y(Alignment::Center),
                    )
                    .width(Length::FillPortion(1))
                    .style(theme::recent_search_chip_card);

                    r = r.push(chip);
                }
                for _ in chunk.len()..3 {
                    r = r.push(horizontal_space().width(Length::FillPortion(1)));
                }
                chips_grid = chips_grid.push(r);
            }

            content_col = content_col.push(column![recent_header, chips_grid].spacing(7));
        }

        // 1. Artists section
        if !self.search_artists.is_empty() {
            let mut artists_grid = column![].spacing(8);
            for pair in self.search_artists.chunks(2).take(3) {
                let mut r = row![].spacing(10);
                for artist in pair {
                    let portrait = image(
                        self.artwork
                            .thumb_or_url(artist.portrait_url.as_deref(), artist.artwork_theme),
                    )
                    .width(Length::Fixed(52.0))
                    .height(Length::Fixed(52.0));

                    let artist_card = button(
                        row![
                            portrait,
                            column![
                                text(&artist.name)
                                    .font(SERIF_FONT)
                                    .size(18)
                                    .color(palette::INK_PRIMARY),
                                text(artist.subtitle())
                                    .size(11.5)
                                    .color(palette::INK_SECONDARY),
                            ]
                            .spacing(2)
                            .width(Length::Fill),
                            text("Albums ›").size(12).color(palette::INK_MUTED),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    )
                    .on_press(Message::OpenArtist(artist.spotify_uri.clone()))
                    .width(Length::FillPortion(1))
                    .padding(Padding::from([8, 10]))
                    .style(theme::subtle_row_button(false));

                    r = r.push(artist_card);
                }
                if pair.len() == 1 {
                    r = r.push(horizontal_space().width(Length::FillPortion(1)));
                }
                artists_grid = artists_grid.push(r);
            }

            content_col = content_col.push(
                column![
                    text("Artists")
                        .font(SERIF_FONT)
                        .size(22)
                        .color(palette::INK_PRIMARY),
                    artists_grid,
                ]
                .spacing(8),
            );
        }

        // 2. Albums section
        if !self.search_albums.is_empty() {
            let mut albums_grid = column![].spacing(8);
            for pair in self.search_albums.chunks(2).take(3) {
                let mut r = row![].spacing(10);
                for alb in pair {
                    let cover = image(
                        self.artwork
                            .thumb_or_url(alb.cover_art_url.as_deref(), alb.artwork_theme),
                    )
                    .width(Length::Fixed(52.0))
                    .height(Length::Fixed(52.0));

                    let alb_card = row![
                        button(
                            row![
                                cover,
                                column![
                                    text(&alb.title).size(14).color(palette::INK_PRIMARY),
                                    text(format!(
                                        "{} · {} · {}",
                                        alb.artist_name, alb.release_year, alb.album_type
                                    ))
                                    .size(11.5)
                                    .color(palette::INK_SECONDARY),
                                ]
                                .spacing(2)
                                .width(Length::Fill),
                                text("Tracks ›").size(11.5).color(palette::INK_MUTED),
                            ]
                            .spacing(12)
                            .align_y(Alignment::Center),
                        )
                        .on_press(Message::OpenAlbum(alb.spotify_uri.clone()))
                        .width(Length::Fill)
                        .padding(Padding::from([8, 10]))
                        .style(theme::subtle_row_button(false)),
                        button(text("▶").size(12).color(palette::EMERALD))
                            .on_press(Message::PlayAlbum(alb.spotify_uri.clone()))
                            .padding(Padding::from([10, 10]))
                            .style(theme::subtle_row_button(false)),
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center)
                    .width(Length::FillPortion(1));

                    r = r.push(alb_card);
                }
                if pair.len() == 1 {
                    r = r.push(horizontal_space().width(Length::FillPortion(1)));
                }
                albums_grid = albums_grid.push(r);
            }

            content_col = content_col.push(
                column![
                    text("Albums")
                        .font(SERIF_FONT)
                        .size(22)
                        .color(palette::INK_PRIMARY),
                    albums_grid,
                ]
                .spacing(8),
            );
        }

        // 2b. Podcasts section
        if !self.search_shows.is_empty() {
            let mut shows_grid = column![].spacing(8);
            for pair in self.search_shows.chunks(2).take(3) {
                let mut r = row![].spacing(10);
                for show in pair {
                    let cover = image(
                        self.artwork
                            .thumb_or_url(show.cover_art_url.as_deref(), show.artwork_theme),
                    )
                    .width(Length::Fixed(52.0))
                    .height(Length::Fixed(52.0));

                    let show_card = row![
                        button(
                            row![
                                cover,
                                column![
                                    text(&show.title).size(14).color(palette::INK_PRIMARY),
                                    text(format!(
                                        "{} · {} · {} eps",
                                        show.publisher, show.category, show.episode_count
                                    ))
                                    .size(11.5)
                                    .color(palette::INK_SECONDARY),
                                ]
                                .spacing(2)
                                .width(Length::Fill),
                                text("Episodes ›").size(11.5).color(palette::INK_MUTED),
                            ]
                            .spacing(12)
                            .align_y(Alignment::Center),
                        )
                        .on_press(Message::OpenShow(show.spotify_uri.clone()))
                        .width(Length::Fill)
                        .padding(Padding::from([8, 10]))
                        .style(theme::subtle_row_button(false)),
                        button(text("▶").size(12).color(palette::EMERALD))
                            .on_press(Message::PlayShow(show.spotify_uri.clone()))
                            .padding(Padding::from([10, 10]))
                            .style(theme::subtle_row_button(false)),
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center)
                    .width(Length::FillPortion(1));

                    r = r.push(show_card);
                }
                if pair.len() == 1 {
                    r = r.push(horizontal_space().width(Length::FillPortion(1)));
                }
                shows_grid = shows_grid.push(r);
            }

            content_col = content_col.push(
                column![
                    text("Podcasts")
                        .font(SERIF_FONT)
                        .size(22)
                        .color(palette::INK_PRIMARY),
                    shows_grid,
                ]
                .spacing(8),
            );
        }

        // 2c. Podcast Episodes section
        if !self.search_episodes.is_empty() {
            let mut episodes_col = column![].spacing(6);
            for ep in &self.search_episodes {
                let is_current = ep.spotify_uri == self.state.current_track.spotify_uri;
                let row_item = row![
                    button(
                        row![
                            image(
                                self.artwork
                                    .thumb_or_url(ep.cover_art_url.as_deref(), ep.artwork_theme)
                            )
                            .width(Length::Fixed(44.0))
                            .height(Length::Fixed(44.0)),
                            column![
                                text(&ep.title).size(14.0).color(palette::INK_PRIMARY),
                                text(format!(
                                    "{} • {} • {}",
                                    ep.show_title,
                                    ep.release_date,
                                    ep.formatted_duration()
                                ))
                                .size(12.0)
                                .color(palette::INK_SECONDARY),
                            ]
                            .spacing(2)
                            .width(Length::Fill),
                            text("▶ Episode").size(11.5).color(palette::EMERALD),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    )
                    .on_press(Message::PlaySearchEpisode(ep.clone()))
                    .width(Length::Fill)
                    .padding(Padding::from([7, 10]))
                    .style(theme::subtle_row_button(is_current)),
                    button(text("Show ›").size(11.5).color(palette::INK_SECONDARY))
                        .on_press(Message::OpenShow(ep.show_uri.clone()))
                        .padding(Padding::from([8, 10]))
                        .style(theme::subtle_row_button(false)),
                ]
                .spacing(6)
                .align_y(Alignment::Center);

                episodes_col = episodes_col.push(row_item);
            }

            content_col = content_col.push(
                column![
                    text("Podcast Episodes")
                        .font(SERIF_FONT)
                        .size(22)
                        .color(palette::INK_PRIMARY),
                    episodes_col,
                ]
                .spacing(8),
            );
        }

        // 3. Tracks section
        if !self.search_results.is_empty() {
            let mut tracks_col = column![].spacing(6);
            for track in &self.search_results {
                let is_current = track.id == self.state.current_track.id;
                let row_item = row![
                    button(
                        row![
                            image(
                                self.artwork.thumb_or_url(
                                    track.cover_art_url.as_deref(),
                                    track.artwork_theme
                                )
                            )
                            .width(Length::Fixed(44.0))
                            .height(Length::Fixed(44.0)),
                            column![
                                text(&track.title).size(14.5).color(palette::INK_PRIMARY),
                                text(format!("{} • {}", track.artist, track.album))
                                    .size(12.5)
                                    .color(palette::INK_SECONDARY),
                            ]
                            .spacing(2)
                            .width(Length::Fill),
                            text(track.formatted_duration())
                                .size(12.5)
                                .color(palette::INK_MUTED),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    )
                    .on_press(Message::PlaySpecificTrack(track.clone()))
                    .width(Length::Fill)
                    .padding(Padding::from([7, 10]))
                    .style(theme::subtle_row_button(is_current)),
                    button(text("Artist ›").size(11.5).color(palette::INK_SECONDARY))
                        .on_press(Message::OpenArtist(track.artist.clone()))
                        .padding(Padding::from([8, 10]))
                        .style(theme::subtle_row_button(false)),
                ]
                .spacing(6)
                .align_y(Alignment::Center);

                tracks_col = tracks_col.push(row_item);
            }

            content_col = content_col.push(
                column![
                    text("Tracks")
                        .font(SERIF_FONT)
                        .size(22)
                        .color(palette::INK_PRIMARY),
                    tracks_col,
                ]
                .spacing(8),
            );
        } else if self.search_query.trim().is_empty()
            && self.recent_searches.is_empty()
            && self.search_shows.is_empty()
            && self.search_episodes.is_empty()
        {
            content_col = content_col.push(
                text("Search above for artists, albums, podcasts, and tracks on Spotify.")
                    .size(13.5)
                    .color(palette::INK_MUTED),
            );
        }

        let mut top_col = column![vertical_space().height(Length::Fixed(10.0)), header,].spacing(12);
        if let Some(banner) = self.view_spotify_auth_banner() {
            top_col = top_col.push(banner);
        }
        top_col = top_col
            .push(search_bar_row)
            .push(scrollable(content_col).height(Length::Fill))
            .padding(Padding::from([20, 34]));

        container(top_col)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::center_stage_container)
            .into()
    }

    /// Dedicated Podcasts discovery directory stage (`NavSection::Podcasts`).
    fn view_podcasts_stage(&self) -> Element<'_, Message> {
        let header = row![
            column![
                text("Podcasts")
                    .font(SERIF_FONT)
                    .size(32)
                    .color(palette::INK_PRIMARY),
                text("Stream full episodes and show archives over Spotify Premium")
                    .size(13.0)
                    .color(palette::INK_SECONDARY),
            ]
            .spacing(2),
            horizontal_space(),
        ]
        .align_y(Alignment::Center);

        let filter_input = text_input(
            "Filter podcasts by title, host, topic, or paste a spotify:show:... link...",
            &self.podcast_filter_query,
        )
        .on_input(Message::PodcastFilterChanged)
        .padding(11)
        .size(13.5)
        .width(Length::Fill);

        let mut filter_row = row![filter_input].spacing(8).align_y(Alignment::Center);
        if !self.podcast_filter_query.is_empty() {
            filter_row = filter_row.push(
                button(text("Clear").size(12.0).color(palette::INK_SECONDARY))
                    .on_press(Message::ClearPodcastFilter)
                    .padding(Padding::from([9, 14]))
                    .style(theme::subtle_row_button(false)),
            );
        }

        let filtered_shows: Vec<PodcastShowSummary> =
            if self.podcast_filter_query.trim().is_empty() {
                self.podcast_directory.clone()
            } else {
                spot_spotify::web_api::search_podcast_shows(&self.podcast_filter_query)
            };

        let mut grid_col = column![].spacing(14);
        for chunk in filtered_shows.chunks(3) {
            let mut card_row = row![].spacing(14);
            for show in chunk {
                let cover = container(
                    image(
                        self.artwork
                            .hero_or_url(show.cover_art_url.as_deref(), show.artwork_theme),
                    )
                    .width(Length::Fixed(132.0))
                    .height(Length::Fixed(132.0)),
                )
                .style(theme::album_art_shadow_card);

                let card = column![
                    button(
                        column![
                            cover,
                            vertical_space().height(Length::Fixed(4.0)),
                            text(show.category.to_uppercase())
                                .size(9.5)
                                .color(palette::EMERALD),
                            text(truncate_nav_label(&show.title, 28))
                                .size(14.0)
                                .color(palette::INK_PRIMARY),
                            text(format!(
                                "{} • {} eps",
                                truncate_nav_label(&show.publisher, 22),
                                show.episode_count
                            ))
                            .size(11.5)
                            .color(palette::INK_SECONDARY),
                        ]
                        .spacing(3)
                        .align_x(Alignment::Start),
                    )
                    .on_press(Message::OpenShow(show.spotify_uri.clone()))
                    .width(Length::Fill)
                    .padding(Padding::from([10, 10]))
                    .style(theme::subtle_row_button(false)),
                    row![
                        button(text("Episodes ›").size(11.0).color(palette::INK_SECONDARY))
                            .on_press(Message::OpenShow(show.spotify_uri.clone()))
                            .padding(Padding::from([3, 8]))
                            .style(theme::subtle_row_button(false)),
                        horizontal_space(),
                        button(text("▶ Latest").size(11.0).color(palette::EMERALD))
                            .on_press(Message::PlayShow(show.spotify_uri.clone()))
                            .padding(Padding::from([3, 8]))
                            .style(theme::subtle_row_button(true)),
                    ]
                    .align_y(Alignment::Center)
                    .padding(Padding::from([0, 6])),
                ]
                .spacing(2)
                .width(Length::FillPortion(1));

                card_row = card_row.push(card);
            }
            for _ in chunk.len()..3 {
                card_row = card_row.push(horizontal_space().width(Length::FillPortion(1)));
            }
            grid_col = grid_col.push(card_row);
        }

        let mut top_col =
            column![vertical_space().height(Length::Fixed(10.0)), header, filter_row].spacing(14);
        if let Some(banner) = self.view_spotify_auth_banner() {
            top_col = top_col.push(banner);
        }
        top_col = top_col
            .push(scrollable(grid_col).height(Length::Fill))
            .padding(Padding::from([20, 34]));

        container(top_col)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::center_stage_container)
            .into()
    }

    /// Dedicated Podcast Show detail & episode list stage (`StageRoute::Show`).
    fn view_show_stage(&self) -> Element<'_, Message> {
        let Some(detail) = &self.selected_show else {
            return container(
                column![
                    vertical_space().height(Length::Fixed(10.0)),
                    self.view_navigation_bar(),
                    vertical_space().height(Length::Fixed(40.0)),
                    text("Loading podcast episodes & artwork...")
                        .font(SERIF_FONT)
                        .size(28)
                        .color(palette::INK_SECONDARY),
                ]
                .spacing(14)
                .padding(Padding::from([20, 34])),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::center_stage_container)
            .into();
        };

        let cover = container(
            image(self.artwork.hero_or_url(
                detail.show.cover_art_url.as_deref(),
                detail.show.artwork_theme,
            ))
            .width(Length::Fixed(208.0))
            .height(Length::Fixed(208.0)),
        )
        .style(theme::album_art_shadow_card);

        let eyebrow = format!("PODCAST  •  {}", detail.show.category.to_uppercase());
        let stats_line = format!(
            "{} episodes  •  {} loaded  •  Spotify Podcasts",
            detail.show.episode_count.max(detail.episodes.len()),
            detail.episodes.len()
        );

        let (desc_preview, full_desc, has_more_desc) =
            summarize_artist_bio(&detail.show.description, 185);
        let desc_toggle_label = if self.show_bio_expanded {
            "Collapse Description ▴"
        } else {
            "Read Description ▾"
        };

        let mut action_row = row![
            button(text("▶  Play Latest Episode").size(12.5))
                .on_press(Message::PlayShow(detail.show.spotify_uri.clone()))
                .padding(Padding::from([8, 16]))
                .style(theme::subtle_row_button(true)),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        if has_more_desc {
            action_row = action_row.push(
                button(
                    text(desc_toggle_label)
                        .size(12.0)
                        .color(palette::INK_SECONDARY),
                )
                .on_press(Message::ToggleShowBio)
                .padding(Padding::from([7, 13]))
                .style(theme::subtle_row_button(false)),
            );
        }

        let hero_header = row![
            cover,
            column![
                text(eyebrow).size(10.5).color(palette::EMERALD),
                text(&detail.show.title)
                    .font(SERIF_FONT)
                    .size(36)
                    .color(palette::INK_PRIMARY),
                text(&detail.show.publisher)
                    .font(SERIF_FONT)
                    .size(20)
                    .color(palette::INK_SECONDARY),
                text(stats_line).size(12.0).color(palette::INK_MUTED),
                text(desc_preview)
                    .size(12.5)
                    .line_height(1.38)
                    .color(palette::INK_SECONDARY),
                vertical_space().height(Length::Fixed(4.0)),
                action_row,
            ]
            .spacing(5)
            .width(Length::Fill),
        ]
        .spacing(24)
        .align_y(Alignment::Center);

        let expanded_desc_card: Option<Element<'_, Message>> = if self.show_bio_expanded {
            Some(
                container(
                    column![
                        text("About this Podcast")
                            .font(SERIF_FONT)
                            .size(19)
                            .color(palette::INK_PRIMARY),
                        text(full_desc)
                            .size(13.0)
                            .line_height(1.48)
                            .color(palette::INK_SECONDARY),
                    ]
                    .spacing(8),
                )
                .width(Length::Fill)
                .padding(Padding::from([14, 18]))
                .style(|_| container::Style {
                    background: Some(iced::Background::Color(palette::SIDEBAR_ACTIVE_PILL)),
                    border: iced::Border {
                        radius: 12.0.into(),
                        color: palette::DIVIDER,
                        width: 1.0,
                    },
                    ..Default::default()
                })
                .into(),
            )
        } else {
            None
        };

        let mut episodes_col = column![
            row![
                text("Recent Episodes")
                    .font(SERIF_FONT)
                    .size(23)
                    .color(palette::INK_PRIMARY),
                text(format!("{} episodes", detail.episodes.len()))
                    .size(12.5)
                    .color(palette::INK_MUTED),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
        ]
        .spacing(8);

        if detail.episodes.is_empty() {
            episodes_col = episodes_col.push(
                text("Fetching latest episodes from Spotify...")
                    .size(13.5)
                    .color(palette::INK_MUTED),
            );
        }

        for (idx, ep) in detail.episodes.iter().enumerate() {
            let is_current = ep.spotify_uri == self.state.current_track.spotify_uri;
            let (ep_notes_preview, _, _) = summarize_artist_bio(&ep.description, 130);

            let ep_row = button(
                row![
                    image(
                        self.artwork
                            .thumb_or_url(ep.cover_art_url.as_deref(), ep.artwork_theme)
                    )
                    .width(Length::Fixed(52.0))
                    .height(Length::Fixed(52.0)),
                    column![
                        text(&ep.title).size(14.5).color(palette::INK_PRIMARY),
                        text(format!("{}  •  {}", ep.release_date, ep.formatted_duration()))
                            .size(11.5)
                            .color(palette::EMERALD),
                        text(ep_notes_preview)
                            .size(12.0)
                            .color(palette::INK_SECONDARY),
                    ]
                    .spacing(2)
                    .width(Length::Fill),
                    container(
                        text(if is_current && self.state.is_playing {
                            "Playing"
                        } else {
                            "▶ Play"
                        })
                        .size(12.0)
                        .color(palette::EMERALD),
                    )
                    .padding(Padding::from([6, 12])),
                ]
                .spacing(14)
                .align_y(Alignment::Center),
            )
            .on_press(Message::PlayShowEpisode(idx))
            .width(Length::Fill)
            .padding(Padding::from([10, 12]))
            .style(theme::subtle_row_button(is_current));

            episodes_col = episodes_col.push(ep_row);
        }

        let mut stage_col = column![hero_header].spacing(18).padding(Padding {
            top: 0.0,
            right: 16.0,
            bottom: 16.0,
            left: 0.0,
        });
        if let Some(desc_card) = expanded_desc_card {
            stage_col = stage_col.push(desc_card);
        }
        stage_col = stage_col.push(horizontal_rule(1)).push(episodes_col);

        container(
            column![
                vertical_space().height(Length::Fixed(10.0)),
                self.view_navigation_bar(),
                scrollable(stage_col).height(Length::Fill),
            ]
            .spacing(14)
            .padding(Padding::from([20, 34])),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::center_stage_container)
        .into()
    }

    /// Dedicated Artist profile & album discography stage with real cover artwork.
    fn view_artist_stage(&self) -> Element<'_, Message> {
        let Some(detail) = &self.selected_artist else {
            return self.view_search_stage();
        };

        let portrait = container(
            image(self.artwork.hero_or_url(
                detail.artist.portrait_url.as_deref(),
                detail.artist.artwork_theme,
            ))
            .width(Length::Fixed(216.0))
            .height(Length::Fixed(216.0)),
        )
        .style(theme::album_art_shadow_card);

        let followers_text = detail
            .artist
            .followers
            .map(|f| format!("{} followers", format_compact_count(f)))
            .unwrap_or_else(|| {
                format!(
                    "{} Releases  •  {} Popular Tracks",
                    detail.albums.len(),
                    detail.top_tracks.len()
                )
            });

        let genre_line = if detail.artist.genres.is_empty() {
            followers_text
        } else {
            format!("{}  •  {}", detail.artist.subtitle(), followers_text)
        };

        let raw_bio = detail.biography.as_deref().unwrap_or(
            "Explore studio albums, singles, and high-resolution 320 kbps streams on Spot.",
        );
        let (bio_preview, full_bio, has_more_bio) = summarize_artist_bio(raw_bio, 215);

        let mut action_row = row![
            button(text("▶  Play Top Tracks").size(12.5))
                .on_press(Message::PlayArtistTopTracks)
                .padding(Padding::from([8, 16]))
                .style(theme::subtle_row_button(true)),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        if has_more_bio {
            let toggle_label = if self.artist_bio_expanded {
                "Collapse Bio  ▴"
            } else {
                "Read Bio  ▾"
            };
            action_row = action_row.push(
                button(text(toggle_label).size(12.0))
                    .on_press(Message::ToggleArtistBio)
                    .padding(Padding::from([7, 13]))
                    .style(theme::nav_pill_button(self.artist_bio_expanded)),
            );
        }

        let hero_header = row![
            portrait,
            column![
                text("VERIFIED ARTIST").size(10.5).color(palette::EMERALD),
                text(&detail.artist.name)
                    .font(SERIF_FONT)
                    .size(40)
                    .color(palette::INK_PRIMARY),
                text(genre_line).size(13.0).color(palette::INK_SECONDARY),
                vertical_space().height(Length::Fixed(2.0)),
                text(bio_preview).size(13.0).color(palette::INK_MUTED),
                vertical_space().height(Length::Fixed(4.0)),
                action_row,
            ]
            .spacing(5)
            .width(Length::Fill),
        ]
        .spacing(24)
        .align_y(Alignment::Center);

        let expanded_bio_section: Option<Element<'_, Message>> =
            if self.artist_bio_expanded && has_more_bio {
                let mut bio_col = column![
                    text(format!("About {}", detail.artist.name))
                        .font(SERIF_FONT)
                        .size(21)
                        .color(palette::INK_PRIMARY)
                ]
                .spacing(10);

                for paragraph in full_bio.split("\n\n").filter(|p| !p.trim().is_empty()) {
                    bio_col = bio_col.push(
                        text(paragraph.trim().to_string())
                            .size(13.0)
                            .color(palette::INK_SECONDARY),
                    );
                }

                Some(
                    container(bio_col)
                        .width(Length::Fill)
                        .padding(Padding::from([16, 20]))
                        .style(theme::artist_bio_card)
                        .into(),
                )
            } else {
                None
            };

        // Discography — Albums & Singles with artwork (click card to open Album page, or ▶ to play)
        let mut albums_section = column![
            text("Albums & Releases")
                .font(SERIF_FONT)
                .size(23)
                .color(palette::INK_PRIMARY)
        ]
        .spacing(10);

        for chunk in detail.albums.chunks(3) {
            let mut album_row = row![].spacing(14);
            for alb in chunk {
                let cover = container(
                    image(
                        self.artwork
                            .hero_or_url(alb.cover_art_url.as_deref(), alb.artwork_theme),
                    )
                    .width(Length::Fixed(128.0))
                    .height(Length::Fixed(128.0)),
                )
                .style(theme::album_art_shadow_card);

                let card = column![
                    button(
                        column![
                            cover,
                            vertical_space().height(Length::Fixed(4.0)),
                            text(&alb.title).size(13.5).color(palette::INK_PRIMARY),
                            text(format!(
                                "{} • {} ({} tracks)",
                                alb.release_year, alb.album_type, alb.track_count
                            ))
                            .size(11.5)
                            .color(palette::INK_SECONDARY),
                        ]
                        .spacing(3)
                        .align_x(Alignment::Start),
                    )
                    .on_press(Message::OpenAlbum(alb.spotify_uri.clone()))
                    .width(Length::Fill)
                    .padding(Padding::from([10, 10]))
                    .style(theme::subtle_row_button(false)),
                    row![
                        button(text("View Album ›").size(11.0).color(palette::INK_SECONDARY))
                            .on_press(Message::OpenAlbum(alb.spotify_uri.clone()))
                            .padding(Padding::from([3, 8]))
                            .style(theme::subtle_row_button(false)),
                        horizontal_space(),
                        button(text("▶ Play").size(11.0).color(palette::EMERALD))
                            .on_press(Message::PlayAlbum(alb.spotify_uri.clone()))
                            .padding(Padding::from([3, 8]))
                            .style(theme::subtle_row_button(true)),
                    ]
                    .align_y(Alignment::Center)
                    .padding(Padding::from([0, 6])),
                ]
                .spacing(2)
                .width(Length::FillPortion(1));

                album_row = album_row.push(card);
            }
            for _ in chunk.len()..3 {
                album_row = album_row.push(horizontal_space().width(Length::FillPortion(1)));
            }
            albums_section = albums_section.push(album_row);
        }

        // Top Tracks collapsible section (collapsed by default; full-width header so text never wraps)
        let mut stage_col = column![hero_header].spacing(18).padding(Padding {
            top: 0.0,
            right: 16.0,
            bottom: 16.0,
            left: 0.0,
        });
        if let Some(bio_card) = expanded_bio_section {
            stage_col = stage_col.push(bio_card);
        }
        if !detail.top_tracks.is_empty() {
            let toggle_label = if self.artist_top_tracks_expanded {
                "Hide Top Tracks  ▴"
            } else {
                "Show Top Tracks  ▾"
            };

            let top_tracks_header = button(
                row![
                    text("Top Tracks")
                        .font(SERIF_FONT)
                        .size(23)
                        .color(palette::INK_PRIMARY),
                    text(format!("{} tracks", detail.top_tracks.len()))
                        .size(12.5)
                        .color(palette::INK_MUTED),
                    horizontal_space(),
                    container(
                        text(toggle_label)
                            .size(12.0)
                            .color(palette::INK_PRIMARY),
                    )
                    .padding(Padding::from([6, 12]))
                    .style(|_| container::Style {
                        background: Some(iced::Background::Color(palette::SIDEBAR_ACTIVE_PILL)),
                        border: iced::Border {
                            radius: 10.0.into(),
                            color: palette::DIVIDER,
                            width: 1.0,
                        },
                        ..Default::default()
                    }),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .on_press(Message::ToggleArtistTopTracks)
            .width(Length::Fill)
            .padding(Padding::from([6, 8]))
            .style(theme::subtle_row_button(false));

            let mut top_tracks_col = column![top_tracks_header].spacing(6);

            if self.artist_top_tracks_expanded {
                for (idx, track) in detail.top_tracks.iter().enumerate() {
                    let is_current = track.id == self.state.current_track.id;
                    let track_row = row![
                        button(
                            row![
                                text(format!("{:02}", idx + 1))
                                    .size(12)
                                    .color(palette::INK_MUTED)
                                    .width(Length::Fixed(22.0)),
                                image(
                                    self.artwork.thumb_or_url(
                                        track.cover_art_url.as_deref(),
                                        track.artwork_theme
                                    )
                                )
                                .width(Length::Fixed(40.0))
                                .height(Length::Fixed(40.0)),
                                column![
                                    text(&track.title).size(14).color(palette::INK_PRIMARY),
                                    text(&track.album).size(12).color(palette::INK_SECONDARY),
                                ]
                                .spacing(2)
                                .width(Length::Fill),
                                text(track.formatted_duration())
                                    .size(12)
                                    .color(palette::INK_MUTED),
                            ]
                            .spacing(10)
                            .align_y(Alignment::Center),
                        )
                        .on_press(Message::PlaySpecificTrack(track.clone()))
                        .width(Length::Fill)
                        .padding(Padding::from([6, 10]))
                        .style(theme::subtle_row_button(is_current)),
                        button(text("Album ›").size(11.5).color(palette::INK_SECONDARY))
                            .on_press(Message::OpenAlbum(track.album.clone()))
                            .padding(Padding::from([7, 10]))
                            .style(theme::subtle_row_button(false)),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center);

                    top_tracks_col = top_tracks_col.push(track_row);
                }
            }

            stage_col = stage_col
                .push(horizontal_rule(1))
                .push(top_tracks_col);
        }
        stage_col = stage_col
            .push(horizontal_rule(1))
            .push(albums_section);

        let body_scroll = scrollable(stage_col).height(Length::Fill);

        container(
            column![
                vertical_space().height(Length::Fixed(10.0)),
                self.view_navigation_bar(),
                body_scroll,
            ]
            .spacing(14)
            .padding(Padding::from([20, 34])),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::center_stage_container)
        .into()
    }

    /// Dedicated Album detail & tracklist stage with visual navigation back to Artist (or deeper).
    fn view_album_stage(&self) -> Element<'_, Message> {
        let Some(detail) = &self.selected_album else {
            return container(
                column![
                    vertical_space().height(Length::Fixed(10.0)),
                    self.view_navigation_bar(),
                    vertical_space().height(Length::Fixed(40.0)),
                    text("Loading album tracks & artwork...")
                        .font(SERIF_FONT)
                        .size(28)
                        .color(palette::INK_SECONDARY),
                ]
                .spacing(14)
                .padding(Padding::from([20, 34])),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::center_stage_container)
            .into();
        };

        let cover = container(
            image(self.artwork.hero_or_url(
                detail.album.cover_art_url.as_deref(),
                detail.album.artwork_theme,
            ))
            .width(Length::Fixed(208.0))
            .height(Length::Fixed(208.0)),
        )
        .style(theme::album_art_shadow_card);

        let artist_target = detail
            .artist_uri
            .clone()
            .unwrap_or_else(|| detail.album.artist_name.clone());

        let prev_is_same_artist = matches!(
            self.nav_history.previous_entry(),
            Some(StageRoute::Artist { label, .. })
                if label.eq_ignore_ascii_case(&detail.album.artist_name)
        );

        let to_artist_msg = if prev_is_same_artist {
            Message::NavigateBack
        } else {
            Message::OpenArtist(artist_target.clone())
        };

        let total_secs: u32 = detail.tracks.iter().map(|t| t.duration_secs).sum();
        let total_mins = (total_secs / 60).max(1);

        let artist_link_btn = button(
            row![
                text(&detail.album.artist_name)
                    .font(SERIF_FONT)
                    .size(21)
                    .color(palette::INK_PRIMARY),
                text("Artist Page ›").size(12.0).color(palette::EMERALD),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .on_press(to_artist_msg.clone())
        .padding(Padding::from([4, 8]))
        .style(theme::subtle_row_button(false));

        let action_row = row![
            button(text("▶  Play Album").size(12.5))
                .on_press(Message::PlayAlbum(detail.album.spotify_uri.clone()))
                .padding(Padding::from([8, 16]))
                .style(theme::subtle_row_button(true)),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let eyebrow = format!(
            "{}  •  {}",
            detail.album.album_type.to_uppercase(),
            detail.album.release_year
        );
        let stats_line = format!(
            "{} tracks  •  {} min  •  320 kbps Ogg Vorbis",
            detail.tracks.len(),
            total_mins
        );

        let hero_header = row![
            cover,
            column![
                text(eyebrow).size(10.5).color(palette::EMERALD),
                text(&detail.album.title)
                    .font(SERIF_FONT)
                    .size(38)
                    .color(palette::INK_PRIMARY),
                artist_link_btn,
                text(stats_line).size(12.5).color(palette::INK_SECONDARY),
                vertical_space().height(Length::Fixed(6.0)),
                action_row,
            ]
            .spacing(5)
            .width(Length::Fill),
        ]
        .spacing(24)
        .align_y(Alignment::Center);

        let mut tracks_col = column![
            text("Album Tracks")
                .font(SERIF_FONT)
                .size(23)
                .color(palette::INK_PRIMARY)
        ]
        .spacing(6);

        for (idx, track) in detail.tracks.iter().enumerate() {
            let is_current = track.id == self.state.current_track.id;
            let track_row = row![
                button(
                    row![
                        text(format!("{:02}", idx + 1))
                            .size(12)
                            .color(palette::INK_MUTED)
                            .width(Length::Fixed(24.0)),
                        column![
                            text(&track.title).size(14.5).color(palette::INK_PRIMARY),
                            text(&track.artist).size(12.0).color(palette::INK_SECONDARY),
                        ]
                        .spacing(2)
                        .width(Length::Fill),
                        text(track.formatted_duration())
                            .size(12.5)
                            .color(palette::INK_MUTED),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                )
                .on_press(Message::PlayAlbumTrack(idx))
                .width(Length::Fill)
                .padding(Padding::from([7, 10]))
                .style(theme::subtle_row_button(is_current)),
                button(text("Artist ›").size(11.5).color(palette::INK_SECONDARY))
                    .on_press(Message::OpenArtist(track.artist.clone()))
                    .padding(Padding::from([7, 10]))
                    .style(theme::subtle_row_button(false)),
            ]
            .spacing(6)
            .align_y(Alignment::Center);

            tracks_col = tracks_col.push(track_row);
        }

        let stage_col = column![hero_header, horizontal_rule(1), tracks_col]
            .spacing(18)
            .padding(Padding {
                top: 0.0,
                right: 16.0,
                bottom: 16.0,
                left: 0.0,
            });

        container(
            column![
                vertical_space().height(Length::Fixed(10.0)),
                self.view_navigation_bar(),
                scrollable(stage_col).height(Length::Fill),
            ]
            .spacing(14)
            .padding(Padding::from([20, 34])),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::center_stage_container)
        .into()
    }

    fn view_library_stage(&self) -> Element<'_, Message> {
        let heading = if self.active_nav == NavSection::Home {
            "Home"
        } else {
            "Your Library"
        };

        let header = row![
            column![
                text(heading)
                    .font(SERIF_FONT)
                    .size(32)
                    .color(palette::INK_PRIMARY),
                text("Recently-listened albums & podcasts and your playlists")
                    .size(13.0)
                    .color(palette::INK_SECONDARY),
            ]
            .spacing(3),
            horizontal_space(),
        ]
        .align_y(Alignment::Center);

        let visible_recent = self
            .recently_listened
            .visible_entries(self.recently_listened_expanded);
        let total_recent = self.recently_listened.len();
        let extra_count = self.recently_listened.extra_count_beyond_top_ten();

        let status_badge_label = if self.recently_listened_expanded {
            format!("Showing all {total_recent}")
        } else {
            format!(
                "Top {}",
                visible_recent.len().min(RECENTLY_LISTENED_TOP_LIMIT)
            )
        };

        let mut recent_header_row = row![
            text("Recently-listened")
                .font(SERIF_FONT)
                .size(23)
                .color(palette::INK_PRIMARY),
            container(
                text(status_badge_label)
                    .size(11.5)
                    .color(palette::INK_SECONDARY),
            )
            .padding(Padding::from([3, 9]))
            .style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(palette::SIDEBAR_ACTIVE_PILL)),
                border: iced::Border {
                    radius: 10.0.into(),
                    color: palette::DIVIDER,
                    width: 1.0,
                },
                ..Default::default()
            }),
            horizontal_space(),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        if self.recently_listened.has_more_than_top_ten() {
            let toggle_label = if self.recently_listened_expanded {
                "Show Top 10  ▴".to_string()
            } else {
                format!("Show More (+{extra_count})  ▾")
            };
            recent_header_row = recent_header_row.push(
                button(text(toggle_label).size(12.0).color(palette::EMERALD))
                    .on_press(Message::ToggleRecentlyListenedExpanded)
                    .padding(Padding::from([6, 12]))
                    .style(theme::subtle_row_button(false)),
            );
        }

        let mut recent_section = column![recent_header_row].spacing(8);

        for (idx, item) in visible_recent.iter().enumerate() {
            let target_uri = item.kind.target_uri().to_string();
            let (open_msg, play_msg, kind_badge) = match &item.kind {
                RecentListenedKind::Album { .. } => (
                    Message::OpenAlbum(target_uri.clone()),
                    Message::PlayAlbum(target_uri),
                    "ALBUM",
                ),
                RecentListenedKind::Podcast { .. } => (
                    Message::OpenShow(target_uri.clone()),
                    Message::PlayShow(target_uri),
                    "PODCAST",
                ),
            };

            let is_playing_this = self
                .state
                .current_track
                .album
                .eq_ignore_ascii_case(&item.title);

            let row_item = row![
                button(
                    row![
                        text(format!("{:02}", idx + 1))
                            .size(12.0)
                            .color(palette::INK_MUTED)
                            .width(Length::Fixed(24.0)),
                        image(
                            self.artwork
                                .thumb_or_url(item.cover_art_url.as_deref(), item.artwork_theme)
                        )
                        .width(Length::Fixed(52.0))
                        .height(Length::Fixed(52.0)),
                        column![
                            row![
                                container(
                                    text(kind_badge).size(9.5).color(palette::EMERALD),
                                )
                                .padding(Padding::from([2, 6]))
                                .style(|_theme: &Theme| container::Style {
                                    background: Some(iced::Background::Color(
                                        palette::SIDEBAR_ACTIVE_PILL
                                    )),
                                    border: iced::Border {
                                        radius: 6.0.into(),
                                        color: palette::DIVIDER,
                                        width: 1.0,
                                    },
                                    ..Default::default()
                                }),
                                text(&item.title).size(15.0).color(palette::INK_PRIMARY),
                            ]
                            .spacing(8)
                            .align_y(Alignment::Center),
                            text(&item.subtitle)
                                .size(12.5)
                                .color(palette::INK_SECONDARY),
                        ]
                        .spacing(3)
                        .width(Length::Fill),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                )
                .on_press(open_msg)
                .width(Length::Fill)
                .padding(Padding::from([8, 12]))
                .style(theme::subtle_row_button(is_playing_this)),
                button(text("▶  Play").size(12.0).color(palette::INK_PRIMARY))
                    .on_press(play_msg)
                    .padding(Padding::from([10, 14]))
                    .style(theme::subtle_row_button(false)),
            ]
            .spacing(8)
            .align_y(Alignment::Center);

            recent_section = recent_section.push(row_item);
        }

        if self.recently_listened.has_more_than_top_ten() {
            let footer_label = if self.recently_listened_expanded {
                "Collapse to Top 10 Recently-listened  ▴".to_string()
            } else {
                format!(
                    "Show {extra_count} More Recently-listened Albums & Podcasts  ▾"
                )
            };
            recent_section = recent_section.push(
                button(
                    row![
                        horizontal_space(),
                        text(footer_label).size(12.5).color(palette::EMERALD),
                        horizontal_space(),
                    ]
                    .align_y(Alignment::Center),
                )
                .on_press(Message::ToggleRecentlyListenedExpanded)
                .width(Length::Fill)
                .padding(Padding::from([8, 12]))
                .style(theme::subtle_row_button(false)),
            );
        }

        let playlists_header = row![
            text("Playlists")
                .font(SERIF_FONT)
                .size(23)
                .color(palette::INK_PRIMARY),
            container(
                text(format!("{}", self.state.playlists.len()))
                    .size(11.5)
                    .color(palette::INK_SECONDARY),
            )
            .padding(Padding::from([3, 9]))
            .style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(palette::SIDEBAR_ACTIVE_PILL)),
                border: iced::Border {
                    radius: 10.0.into(),
                    color: palette::DIVIDER,
                    width: 1.0,
                },
                ..Default::default()
            }),
            horizontal_space(),
            button(text("+ New Playlist").size(12.0).color(palette::INK_SECONDARY))
                .on_press(Message::CreateNewPlaylist)
                .padding(Padding::from([6, 12]))
                .style(theme::subtle_row_button(false)),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let mut playlist_cards = column![playlists_header].spacing(10);
        if self.state.playlists.is_empty() {
            playlist_cards = playlist_cards.push(
                text("Connect your Spotify account to load your playlists.")
                    .size(13.0)
                    .color(palette::INK_MUTED),
            );
        }
        for playlist in &self.state.playlists {
            let card = button(
                row![
                    image(self.artwork.thumb_handle(playlist.artwork_theme))
                        .width(Length::Fixed(52.0))
                        .height(Length::Fixed(52.0)),
                    column![
                        text(&playlist.name).size(15).color(palette::INK_PRIMARY),
                        text(&playlist.subtitle)
                            .size(12.5)
                            .color(palette::INK_SECONDARY),
                    ]
                    .spacing(3)
                    .width(Length::Fill),
                    text(format!("{} tracks", playlist.track_count))
                        .size(12)
                        .color(palette::INK_MUTED),
                ]
                .spacing(14)
                .align_y(Alignment::Center),
            )
            .on_press(Message::SelectPlaylist(playlist.id.clone()))
            .width(Length::Fill)
            .padding(Padding::from([10, 12]))
            .style(theme::subtle_row_button(playlist.is_playing_context));

            playlist_cards = playlist_cards.push(card);
        }

        let library_body = column![recent_section, horizontal_rule(1), playlist_cards]
            .spacing(22)
            .padding(Padding {
                top: 0.0,
                right: 16.0,
                bottom: 20.0,
                left: 0.0,
            });

        let mut top_col = column![
            vertical_space().height(Length::Fixed(10.0)),
            self.view_navigation_bar(),
            header,
        ]
        .spacing(12);
        if let Some(banner) = self.view_spotify_auth_banner() {
            top_col = top_col.push(banner);
        }
        top_col = top_col
            .push(scrollable(library_body).height(Length::Fill))
            .padding(Padding::from([20, 34]));

        container(top_col)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::center_stage_container)
            .into()
    }

    /// Column 3: Right "Up Next" queue pane.
    fn view_queue_pane(&self) -> Element<'_, Message> {
        let header = text("Up Next")
            .font(SERIF_FONT)
            .size(25)
            .color(palette::INK_PRIMARY);

        let context_subtitle: Option<String> = match &self.state.context {
            PlaybackContext::Album {
                artist_name,
                artist_albums,
                current_album_index,
                exhausted,
                ..
            } => {
                if let Some(active_album) = artist_albums.get(*current_album_index) {
                    if *exhausted {
                        Some(format!("Finished all albums by {artist_name}"))
                    } else if artist_albums.len() > 1 {
                        Some(format!(
                            "Album {} of {} · {}",
                            current_album_index + 1,
                            artist_albums.len(),
                            active_album.album_title
                        ))
                    } else {
                        Some(format!("From {}", active_album.album_title))
                    }
                } else {
                    None
                }
            }
            PlaybackContext::Playlist { playlist_id } => {
                let pl_name = self
                    .state
                    .playlists
                    .iter()
                    .find(|p| p.id == *playlist_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| playlist_id.clone());
                Some(format!("Playlist · {pl_name}"))
            }
            PlaybackContext::PodcastShow { show_title, .. } => {
                Some(format!("Podcast · {show_title}"))
            }
            PlaybackContext::None => None,
        };

        let mut header_col = column![header].spacing(2);
        if let Some(sub) = context_subtitle {
            header_col = header_col.push(text(sub).size(12.5).color(palette::INK_SECONDARY));
        }

        let mut list = column![].spacing(10);
        if self.state.queue.is_empty() {
            list = list.push(
                container(
                    text("Select a playlist, album, or track to start your queue.")
                        .size(12.5)
                        .color(palette::INK_MUTED),
                )
                .padding(Padding::from([8, 22])),
            );
        }
        for track in &self.state.queue {
            let is_current = track.id == self.state.current_track.id;

            let leading_indicator: Element<'_, Message> = if is_current {
                svg(icons::active_bars())
                    .width(Length::Fixed(14.0))
                    .height(Length::Fixed(14.0))
                    .into()
            } else {
                horizontal_space().width(Length::Fixed(14.0)).into()
            };

            let cover = image(
                self.artwork
                    .thumb_or_url(track.cover_art_url.as_deref(), track.artwork_theme),
            )
            .width(Length::Fixed(46.0))
            .height(Length::Fixed(46.0));

            let meta = column![
                text(&track.title).size(14).color(palette::INK_PRIMARY),
                text(&track.artist).size(12.5).color(palette::INK_SECONDARY),
            ]
            .spacing(2)
            .width(Length::Fill);

            let duration = text(track.formatted_duration())
                .size(12.5)
                .color(palette::INK_MUTED);

            let row_btn = button(
                row![leading_indicator, cover, meta, duration]
                    .spacing(10)
                    .align_y(Alignment::Center),
            )
            .on_press(Message::SelectQueueTrack(track.id.clone()))
            .width(Length::Fill)
            .padding(Padding::from([6, 8]))
            .style(theme::subtle_row_button(false));

            list = list.push(row_btn);
        }

        if let Some(next_album) = self.state.next_artist_album_queue() {
            list = list.push(
                container(
                    column![
                        text("NEXT ALBUM BY ARTIST")
                            .size(10.5)
                            .color(palette::INK_MUTED),
                        text(&next_album.album_title)
                            .size(13)
                            .color(palette::INK_SECONDARY),
                    ]
                    .spacing(2),
                )
                .padding(Padding::from([10, 22])),
            );
        } else if self.state.is_discography_exhausted() {
            list = list.push(
                container(
                    text("All albums by this artist have finished playing.")
                        .size(12)
                        .color(palette::INK_MUTED),
                )
                .padding(Padding::from([10, 22])),
            );
        }

        let content = column![
            vertical_space().height(Length::Fixed(34.0)),
            container(header_col).padding(Padding {
                top: 0.0,
                right: 0.0,
                bottom: 8.0,
                left: 22.0,
            }),
            scrollable(list).height(Length::Fill),
        ]
        .spacing(10)
        .padding(Padding {
            top: 14.0,
            right: 20.0,
            bottom: 20.0,
            left: 14.0,
        });

        container(content)
            .width(Length::Fixed(295.0))
            .height(Length::Fill)
            .style(theme::queue_pane_container)
            .into()
    }
}

fn is_space_keyboard_key(key: &iced::keyboard::Key) -> bool {
    match key {
        iced::keyboard::Key::Named(iced::keyboard::key::Named::Space) => true,
        iced::keyboard::Key::Character(c) => c.as_str() == " ",
        _ => false,
    }
}

fn truncate_nav_label(label: &str, max_chars: usize) -> String {
    let trimmed = label.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn recent_searches_file_path() -> PathBuf {
    spot_spotify::auth::AppCachePaths::resolve()
        .root_dir
        .join("recent_searches.json")
}

fn load_recent_searches() -> RecentSearchStore {
    if cfg!(test) {
        return RecentSearchStore::new(10);
    }
    let path = recent_searches_file_path();
    let Ok(bytes) = std::fs::read(&path) else {
        return RecentSearchStore::new(10);
    };
    let raw = serde_json::from_slice::<Vec<RecentSearchEntry>>(&bytes).unwrap_or_default();
    RecentSearchStore::from_loaded(raw, 10)
}

fn save_recent_searches(entries: &[RecentSearchEntry]) {
    if cfg!(test) {
        return;
    }
    let path = recent_searches_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(entries) {
        let _ = std::fs::write(&path, json);
    }
}

fn recently_listened_file_path() -> PathBuf {
    spot_spotify::auth::AppCachePaths::resolve()
        .root_dir
        .join("recently_listened.json")
}

fn load_recently_listened() -> RecentListenedStore {
    let mut store = if cfg!(test) {
        RecentListenedStore::new(24)
    } else {
        let path = recently_listened_file_path();
        match std::fs::read(&path) {
            Ok(bytes) => {
                let raw =
                    serde_json::from_slice::<Vec<RecentListenedItem>>(&bytes).unwrap_or_default();
                RecentListenedStore::from_loaded(raw, 24)
            }
            Err(_) => RecentListenedStore::new(24),
        }
    };
    for seed in spot_spotify::web_api::default_recently_listened_items() {
        store.append_if_absent(seed);
    }
    store
}

fn save_recently_listened(entries: &[RecentListenedItem]) {
    if cfg!(test) {
        return;
    }
    let path = recently_listened_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(entries) {
        let _ = std::fs::write(&path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_artist_bio_strips_html_and_truncates_cleanly() {
        let mastodon_raw = r#"One of the template bands of the New Wave of American Heavy Metal, Atlanta's Mastodon inject brainy, psychedelic prog into a volatile rhythmic alloy of sludge, stoner, and thrash metal. They broke into the mainstream with 2004's <a href="spotify:album:abc">Leviathan</a>, a critically acclaimed concept album based on <a href="spotify:artist:674k1zGzVqfmAcYyeOtMh7">Herman Melville</a>'s Moby Dick."#;
        let (preview, full, has_more) = summarize_artist_bio(mastodon_raw, 215);
        assert!(has_more);
        assert!(!preview.contains("<a "));
        assert!(!preview.contains("</a>"));
        assert!(!full.contains("<a "));
        assert!(full.contains("Herman Melville's Moby Dick."));
        assert!(preview.chars().count() <= 220);
    }

    #[test]
    fn recent_query_coalesces_incremental_typing_and_deduplicates() {
        let mut recent = RecentSearchStore::new(10);
        assert!(recent.record_typing_query("ma"));
        assert!(recent.record_typing_query("mast"));
        assert!(recent.record_typing_query("mastodon"));
        assert_eq!(recent.len(), 1);
        assert_eq!(recent.entries()[0].label, "mastodon");
        recent.assert_invariants();

        recent.record_entry(RecentSearchEntry {
            label: "Mastodon".into(),
            subtitle: "Progressive Metal".into(),
            query: "mastodon".into(),
            kind: RecentSearchKind::Artist {
                uri: "spotify:artist:1DvfQQ39HxvCJ3GvfeIFuT".into(),
            },
        });
        assert_eq!(recent.len(), 2);
        assert!(matches!(
            recent.entries()[0].kind,
            RecentSearchKind::Artist { .. }
        ));
        recent.assert_invariants();
    }

    #[test]
    fn artwork_cache_generates_all_themes() {
        let cache = ArtworkCache::new();
        let _hero = cache.hero_handle(spot_core::ArtworkTheme::SunriseMountainLake);
        let _thumb = cache.thumb_handle(spot_core::ArtworkTheme::GoldenCanyon);
    }

    #[test]
    fn app_initializes_and_handles_multi_level_navigation_and_playback_messages() {
        let (mut app, _) = SpotApp::new();
        app.assert_invariants();
        assert!(app.state.queue.is_empty());
        assert_eq!(app.active_nav, NavSection::NowPlaying);

        let _ = app.update(Message::SelectNav(NavSection::Search));
        assert_eq!(app.active_nav, NavSection::Search);
        app.assert_invariants();

        let _ = app.update(Message::ToggleArtistBio);
        assert!(app.artist_bio_expanded);
        let _ = app.update(Message::ToggleArtistTopTracks);
        assert!(app.artist_top_tracks_expanded);
        let _ = app.update(Message::OpenArtist("Mastodon".into()));
        assert!(!app.artist_bio_expanded);
        assert!(!app.artist_top_tracks_expanded);
        assert!(matches!(app.current_route(), StageRoute::Artist { .. }));
        app.assert_invariants();

        // Navigate deeper from Artist -> Album -> Back to Artist -> Forward to Album
        let _ = app.update(Message::OpenAlbum("Marrow Deep".into()));
        assert!(matches!(app.current_route(), StageRoute::Album { .. }));
        assert!(app.selected_album.is_some());
        app.assert_invariants();

        let _ = app.update(Message::NavigateBack);
        assert!(matches!(app.current_route(), StageRoute::Artist { .. }));
        app.assert_invariants();

        let _ = app.update(Message::NavigateForward);
        assert!(matches!(app.current_route(), StageRoute::Album { .. }));
        app.assert_invariants();

        let _ = app.update(Message::ToggleLike);
        assert!(app.state.current_track.liked);
        app.assert_invariants();

        let _ = app.update(Message::AnimationTick);
        app.assert_invariants();
    }

    #[test]
    fn cmd_backspace_deletes_whole_word_and_connected_status_cleans_up_banner() {
        let (mut app, _) = SpotApp::new();
        let _ = app.update(Message::SelectNav(NavSection::Search));
        let _ = app.update(Message::SearchQueryChanged("mastodon leviathan".into()));
        assert_eq!(app.search_query, "mastodon leviathan");

        // Simulate Cmd+Backspace when ModifiersChanged(LOGO) is active and text_input emits 1-char backspace
        let _ = app.update(Message::ModifiersChanged(iced::keyboard::Modifiers::LOGO));
        let _ = app.update(Message::SearchQueryChanged("mastodon leviatha".into()));
        assert_eq!(app.search_query, "mastodon ");
        // Subsequent SearchDeleteWord in same frame is deduplicated
        let _ = app.update(Message::SearchDeleteWord);
        assert_eq!(app.search_query, "mastodon ");

        // Second Cmd+Backspace deletes "mastodon " -> ""
        let _ = app.update(Message::SearchDeleteWord);
        assert_eq!(app.search_query, "");

        // Verify connected account formatting & top banner removal
        app.state.connection = ConnectionStatus::Connected {
            username: "315okpb22i4itwoz3ccxlrvz7xci".into(),
            is_premium: true,
            live_session: true,
        };
        assert_eq!(app.state.connection.display_label(), "Connected • Premium");
        assert!(app.view_spotify_auth_banner().is_none());
    }

    #[test]
    fn podcasts_directory_show_detail_and_episode_playback_preserve_invariants() {
        let (mut app, _) = SpotApp::new();
        assert!(!app.podcast_directory.is_empty());

        // Navigate to Podcasts section
        let _ = app.update(Message::SelectNav(NavSection::Podcasts));
        assert_eq!(app.active_nav, NavSection::Podcasts);
        let _ = app.view();
        app.assert_invariants();

        // Filter podcasts and open a show
        let _ = app.update(Message::PodcastFilterChanged("Huberman".into()));
        let first_show = app.podcast_directory[0].clone();
        let _ = app.update(Message::OpenShow(first_show.spotify_uri.clone()));
        assert!(matches!(app.current_route(), StageRoute::Show { .. }));
        app.assert_invariants();

        // Simulate ShowDetailLoaded with playable episodes
        let sample_ep = PodcastEpisode {
            id: "2MAi0BvDc6GTFvKFPXnkCL".into(),
            spotify_uri: "spotify:episode:2MAi0BvDc6GTFvKFPXnkCL".into(),
            title: "Episode #1: Deep Focus & Neuroplasticity".into(),
            show_title: first_show.title.clone(),
            show_uri: first_show.spotify_uri.clone(),
            description: "Detailed show notes on focus and sleep.".into(),
            release_date: "2025-10-14".into(),
            duration_secs: 4520,
            cover_art_url: first_show.cover_art_url.clone(),
            artwork_theme: first_show.artwork_theme,
        };
        app.apply_engine_event(EngineEvent::ShowDetailLoaded(Box::new(PodcastShowDetail {
            show: first_show.clone(),
            episodes: vec![sample_ep],
        })));
        app.assert_invariants();

        // Expand/collapse show description and render show stage
        let _ = app.update(Message::ToggleShowBio);
        assert!(app.show_bio_expanded);
        let _ = app.view();

        // Play episode 0 from the show page
        let _ = app.update(Message::PlayShowEpisode(0));
        assert_eq!(
            app.state.current_track.spotify_uri,
            "spotify:episode:2MAi0BvDc6GTFvKFPXnkCL"
        );
        assert!(app.state.is_playing);
        assert_eq!(app.state.current_track.formatted_duration(), "1:15:20");
        app.assert_invariants();
    }

    #[test]
    fn your_library_shows_top_10_recently_listened_and_expands_to_more_than_10() {
        let (mut app, _) = SpotApp::new();
        assert!(app.recently_listened.len() > RECENTLY_LISTENED_TOP_LIMIT);
        assert!(app.recently_listened.has_more_than_top_ten());
        assert!(!app.recently_listened_expanded);
        assert_eq!(
            app.recently_listened
                .visible_entries(app.recently_listened_expanded)
                .len(),
            RECENTLY_LISTENED_TOP_LIMIT
        );

        // Navigate to Your Library and render collapsed Top 10 + Playlists
        let _ = app.update(Message::SelectNav(NavSection::YourLibrary));
        assert_eq!(app.active_nav, NavSection::YourLibrary);
        let _ = app.view();
        app.assert_invariants();

        // Expand Recently-listened to show more than 10
        let _ = app.update(Message::ToggleRecentlyListenedExpanded);
        assert!(app.recently_listened_expanded);
        assert_eq!(
            app.recently_listened
                .visible_entries(app.recently_listened_expanded)
                .len(),
            app.recently_listened.len()
        );
        assert!(
            app.recently_listened
                .visible_entries(app.recently_listened_expanded)
                .len()
                > RECENTLY_LISTENED_TOP_LIMIT
        );
        let _ = app.view();
        app.assert_invariants();

        // Playing an item promotes it to #1 in Recently-listened
        let item_at_12 = app.recently_listened.entries()[12].clone();
        let target_uri = item_at_12.kind.target_uri().to_string();
        match &item_at_12.kind {
            RecentListenedKind::Album { .. } => {
                let _ = app.update(Message::PlayAlbum(target_uri.clone()));
            }
            RecentListenedKind::Podcast { .. } => {
                let _ = app.update(Message::PlayShow(target_uri.clone()));
            }
        }
        assert_eq!(
            app.recently_listened.entries()[0].kind.target_uri(),
            target_uri
        );
        app.assert_invariants();

        // Collapse back to Top 10
        let _ = app.update(Message::ToggleRecentlyListenedExpanded);
        assert!(!app.recently_listened_expanded);
        assert_eq!(
            app.recently_listened
                .visible_entries(app.recently_listened_expanded)
                .len(),
            RECENTLY_LISTENED_TOP_LIMIT
        );
        app.assert_invariants();
    }

    #[test]
    fn album_up_next_isolation_and_artist_discography_progression() {
        use spot_core::ArtworkTheme;
        let (mut app, _) = SpotApp::new();

        // 1. Start with an active playlist ("Jarvis Vibes") containing playlist tracks.
        let playlist_tracks = vec![
            Track {
                id: "pl-1".into(),
                spotify_uri: "spotify:track:pl1".into(),
                title: "King Of The Dancehall".into(),
                artist: "Beenie Man".into(),
                album: "Jarvis Vibes".into(),
                duration_secs: 213,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::SunriseMountainLake,
            },
            Track {
                id: "pl-2".into(),
                spotify_uri: "spotify:track:pl2".into(),
                title: "King".into(),
                artist: "Years & Years".into(),
                album: "Jarvis Vibes".into(),
                duration_secs: 215,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::EmeraldRing,
            },
        ];
        app.state.playlists.push(spot_core::Playlist {
            id: "pl-jarvis".into(),
            spotify_uri: "spotify:playlist:jarvis".into(),
            name: "Jarvis Vibes".into(),
            subtitle: "Playlist".into(),
            track_count: 2,
            is_playing_context: false,
            artwork_theme: ArtworkTheme::SunriseMountainLake,
        });
        app.state
            .apply_verified_action(VerifiedTransportAction::ActivatePlaylist {
                playlist_id: "pl-jarvis".into(),
                tracks: playlist_tracks,
                auto_play: true,
            });
        app.engine.audio_gate.set_playing(app.state.is_playing);
        assert_eq!(app.state.active_playlist_id.as_deref(), Some("pl-jarvis"));
        app.assert_invariants();

        // 2. Load album "Run The Jewels 2" and click track 0 ("Jeopardy").
        let rtj2_tracks = vec![
            Track {
                id: "rtj2-1".into(),
                spotify_uri: "spotify:track:rtj2_1".into(),
                title: "Jeopardy".into(),
                artist: "Run The Jewels".into(),
                album: "Run The Jewels 2".into(),
                duration_secs: 201,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::VioletAura,
            },
            Track {
                id: "rtj2-2".into(),
                spotify_uri: "spotify:track:rtj2_2".into(),
                title: "Oh My Darling Don't Cry".into(),
                artist: "Run The Jewels".into(),
                album: "Run The Jewels 2".into(),
                duration_secs: 204,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::VioletAura,
            },
        ];
        let rtj3_tracks = vec![Track {
            id: "rtj3-1".into(),
            spotify_uri: "spotify:track:rtj3_1".into(),
            title: "Legend Has It".into(),
            artist: "Run The Jewels".into(),
            album: "Run The Jewels 3".into(),
            duration_secs: 205,
            liked: false,
            cover_art_url: None,
            artwork_theme: ArtworkTheme::TwilightHorizon,
        }];
        let rtj2_detail = spot_core::AlbumDetail {
            album: spot_core::AlbumSummary {
                id: "rtj2".into(),
                spotify_uri: "spotify:album:rtj2".into(),
                title: "Run The Jewels 2".into(),
                artist_name: "Run The Jewels".into(),
                release_year: "2014".into(),
                album_type: "Album".into(),
                track_count: 2,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::VioletAura,
            },
            artist_uri: Some("spotify:artist:rtj".into()),
            tracks: rtj2_tracks.clone(),
        };
        app.apply_engine_event(EngineEvent::AlbumDetailLoaded(Box::new(rtj2_detail)));

        let _ = app.update(Message::PlayAlbumTrack(0));

        // Up Next must strictly contain "Run The Jewels 2" tracks, NOT "Jarvis Vibes" playlist tracks!
        assert!(app.state.active_playlist_id.is_none());
        assert_eq!(app.state.queue, rtj2_tracks);
        assert_eq!(app.state.current_track.title, "Jeopardy");
        app.assert_invariants();

        // Hydrate the artist's discography (RTJ2 -> RTJ3)
        app.state
            .apply_verified_action(VerifiedTransportAction::SetArtistDiscography {
                artist_name: "Run The Jewels".into(),
                artist_uri: Some("spotify:artist:rtj".into()),
                albums: vec![
                    spot_core::AlbumPlaybackQueue {
                        album_uri: "spotify:album:rtj2".into(),
                        album_title: "Run The Jewels 2".into(),
                        artist_name: "Run The Jewels".into(),
                        tracks: rtj2_tracks,
                    },
                    spot_core::AlbumPlaybackQueue {
                        album_uri: "spotify:album:rtj3".into(),
                        album_title: "Run The Jewels 3".into(),
                        artist_name: "Run The Jewels".into(),
                        tracks: rtj3_tracks.clone(),
                    },
                ],
            });
        app.assert_invariants();

        // NextTrack advances to track 2 of Run The Jewels 2
        let _ = app.update(Message::NextTrack);
        assert_eq!(app.state.current_track.title, "Oh My Darling Don't Cry");
        assert!(app.state.is_playing);
        app.assert_invariants();

        // NextTrack at end of Run The Jewels 2 transitions to Run The Jewels 3!
        let _ = app.update(Message::NextTrack);
        assert_eq!(app.state.current_track.title, "Legend Has It");
        assert_eq!(app.state.queue, rtj3_tracks);
        assert!(app.state.is_playing);
        app.assert_invariants();

        // NextTrack at end of final album in artist discography stops playback completely!
        let _ = app.update(Message::NextTrack);
        assert!(!app.state.is_playing);
        assert!(app.state.is_discography_exhausted());
        app.assert_invariants();
        let _ = app.view();
    }

    #[test]
    fn tray_and_player_progress_indicators_are_always_in_sync() {
        use spot_core::{ArtworkTheme, TrayMenuSnapshot};
        let (mut app, _) = SpotApp::new();

        // Load podcast episode "MAGA's Midterm Defectors" (duration 27:42 = 1662s)
        app.state.current_track = Track {
            id: "ep-daily".into(),
            spotify_uri: "spotify:episode:2MAi0BvDc6GTFvKFPXnkCL".into(),
            title: "MAGA's Midterm Defectors".into(),
            artist: "The Daily".into(),
            album: "The Daily".into(),
            duration_secs: 1662,
            liked: false,
            cover_art_url: None,
            artwork_theme: ArtworkTheme::SunriseMountainLake,
        };
        app.state.is_playing = true;
        app.apply_engine_event(EngineEvent::PositionTick(972.0)); // 16:12
        app.assert_invariants();

        let snap = TrayMenuSnapshot::from_playback_state(&app.state);
        assert!(snap.check_sync_with_playback(&app.state).is_ok());
        assert_eq!(app.state.synced_elapsed_secs(), 972);
        assert_eq!(snap.elapsed_secs, 972);
        assert_eq!(snap.remaining_secs, 690); // -11:30
        assert!(snap.progress_label.contains("16:12 / 27:42"));
        assert!(snap.progress_label.contains("(-11:30)"));

        // Seek via UI slider to 08:43 (523s) and release
        let _ = app.update(Message::SeekChanged(523.0));
        app.assert_invariants();
        let snap_after_scrub = TrayMenuSnapshot::from_playback_state(&app.state);
        assert!(snap_after_scrub.check_sync_with_playback(&app.state).is_ok());
        assert_eq!(app.state.synced_elapsed_secs(), 523);
        assert_eq!(snap_after_scrub.elapsed_secs, 523);
        assert_eq!(snap_after_scrub.remaining_secs, 1139);
        assert!(snap_after_scrub.progress_label.contains("8:43 / 27:42"));

        let _ = app.update(Message::SeekReleased);
        app.assert_invariants();

        // Pause playback -> playback_rate_milli must drop to 0 so macOS Now Playing does not drift
        let _ = app.update(Message::TogglePlayPause);
        assert!(!app.state.is_playing);
        app.assert_invariants();
        let snap_paused = TrayMenuSnapshot::from_playback_state(&app.state);
        assert_eq!(snap_paused.playback_rate_milli, 0);
        assert_eq!(snap_paused.elapsed_secs, app.state.synced_elapsed_secs());
    }

    #[test]
    fn spacebar_toggles_play_pause_instantly_and_ignores_focused_text_inputs() {
        let (mut app, _) = SpotApp::new();
        assert!(!app.state.is_playing);
        assert!(!app.engine.audio_gate.is_playing());
        app.assert_invariants();

        // 1. Pressing Space on NowPlaying (status_captured = false) immediately unpauses both UI and audio_gate
        let _ = app.update(Message::SpacePressed {
            status_captured: false,
            has_command_ctrl_alt: false,
        });
        assert!(app.state.is_playing);
        assert!(app.engine.audio_gate.is_playing());
        assert!(app.space_key_held);
        app.assert_invariants();

        // 2. OS auto-repeat while Space is still held down does NOT rapid-toggle play/pause
        let _ = app.update(Message::SpacePressed {
            status_captured: false,
            has_command_ctrl_alt: false,
        });
        assert!(app.state.is_playing);
        assert!(app.engine.audio_gate.is_playing());
        app.assert_invariants();

        // Release Space
        let _ = app.update(Message::SpaceReleased);
        assert!(!app.space_key_held);

        // 3. Pressing Space even when mouse cursor hovers over a slider (status_captured = true,
        //    but text_input_edited_this_event = false) still pauses immediately!
        let _ = app.update(Message::SpacePressed {
            status_captured: true,
            has_command_ctrl_alt: false,
        });
        let _ = app.update(Message::SpaceReleased);
        assert!(!app.state.is_playing);
        assert!(!app.engine.audio_gate.is_playing());
        app.assert_invariants();

        // 4. Typing a space into the focused Search input emits SearchQueryChanged first,
        //    then SpacePressed { status_captured: true }. Must NOT toggle play/pause!
        let _ = app.update(Message::SelectNav(NavSection::Search));
        let _ = app.update(Message::SearchQueryChanged("run ".into()));
        let _ = app.update(Message::SpacePressed {
            status_captured: true,
            has_command_ctrl_alt: false,
        });
        let _ = app.update(Message::SpaceReleased);
        assert!(!app.state.is_playing);
        assert!(!app.engine.audio_gate.is_playing());
        assert_eq!(app.search_query, "run ");
        app.assert_invariants();

        // 5. Typing a space into the focused Podcast filter input also must NOT toggle play/pause!
        let _ = app.update(Message::SelectNav(NavSection::Podcasts));
        let _ = app.update(Message::PodcastFilterChanged("the daily ".into()));
        let _ = app.update(Message::SpacePressed {
            status_captured: true,
            has_command_ctrl_alt: false,
        });
        let _ = app.update(Message::SpaceReleased);
        assert!(!app.state.is_playing);
        assert!(!app.engine.audio_gate.is_playing());
        assert_eq!(app.podcast_filter_query, "the daily ");
        app.assert_invariants();
    }

    #[test]
    fn main_update_detection_storage_and_integrity_preserve_invariants() {
        let (mut app, _) = SpotApp::new();
        app.assert_invariants();

        let next_build = app.update_catalog.installed_build_number() + 10;
        let fake_sha = "abcdef1234567890abcdef1234567890abcdef12";
        let fake_digest = updater::sha256_hex_bytes(b"spot-macos-zip-payload");

        let manifest = UpdateReleaseManifest {
            version: "0.1.0".into(),
            build_number: next_build,
            commit_sha: fake_sha.into(),
            short_sha: "abcdef1".into(),
            tag: format!("main-b{next_build}-abcdef1"),
            commit_title: "Add main update storage".into(),
            published_at: "2026-10-19T23:30:00Z".into(),
            asset_name: "Spot-macos.zip".into(),
            download_url: "https://github.com/cartermp/spot/releases/download/test/Spot-macos.zip"
                .into(),
            sha256: fake_digest.clone(),
            size_bytes: 22,
        };

        let _ = app.update(Message::MainUpdateChecked(Ok(manifest.clone())));
        assert!(matches!(
            app.update_catalog.evaluate_availability(),
            UpdateAvailability::UpdateAvailable { .. }
        ));
        app.assert_invariants();

        // Corrupted SHA-256 must be rejected by UpdateCatalogStore (INV-U2)
        assert!(app
            .update_catalog
            .record_downloaded_update(
                manifest.clone(),
                "0000000000000000000000000000000000000000000000000000000000000000"
            )
            .is_err());
        app.assert_invariants();

        // Valid SHA-256 stores cleanly and transitions after install (INV-U3 & INV-U4)
        let (stored, _) = app
            .update_catalog
            .record_downloaded_update(manifest, &fake_digest)
            .expect("valid digest must store");
        assert!(app.update_catalog.find_stored("abcdef1").is_some());
        app.assert_invariants();

        app.update_catalog
            .mark_installed(stored.manifest.build_number, &stored.manifest.commit_sha);
        assert!(matches!(
            app.update_catalog.evaluate_availability(),
            UpdateAvailability::UpToDate { .. }
        ));
        app.assert_invariants();
        let _ = app.view();
    }
}
