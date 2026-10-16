mod artwork;
mod icons;
mod theme;
mod visualizer;

use artwork::{download_and_decode_artwork, ArtworkCache};
use groove_audio::SpectrumAnalyzer;
use groove_core::{
    format_compact_count, format_seconds, AlbumSummary, ArtistDetail, ArtistSummary, AudioDevice,
    ConnectionStatus, EngineEvent, NavSection, PlaybackState, PlayerCommand, RepeatMode,
    SpectrumFrame, Track,
};
use groove_spotify::{
    web_api::{all_curated_albums, all_curated_artists, curated_artist_detail},
    spawn_engine, EngineHandle,
};
use iced::alignment::{Horizontal, Vertical};
use iced::widget::{
    button, column, container, horizontal_rule, horizontal_space, image, pick_list, row,
    scrollable, slider, svg, text, text_input, vertical_space,
};
use iced::window;
use iced::{Alignment, Element, Length, Padding, Size, Subscription, Task, Theme};
use std::time::Duration;
use theme::{palette, INSTRUMENT_SERIF_BYTES, SERIF_FONT};
use visualizer::RibbonVisualizer;

pub fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
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
        platform_specific,
        ..Default::default()
    };

    iced::application("Groove", GrooveApp::update, GrooveApp::view)
        .window(window_settings)
        .font(INSTRUMENT_SERIF_BYTES)
        .subscription(GrooveApp::subscription)
        .theme(|_| Theme::Light)
        .antialiasing(true)
        .run_with(GrooveApp::new)
}

#[derive(Debug, Clone)]
pub enum Message {
    AnimationTick,
    SelectNav(NavSection),
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
    PlayAlbum(String),
    PlayArtistTopTracks,
    CreateNewPlaylist,
    SelectOutputDevice(AudioDevice),
    ConnectSpotifyClicked { force_reauth: bool },
    LogoutSpotifyClicked,
    SearchQueryChanged(String),
    ToggleTrackMenu,
    ArtworkLoaded(Option<(String, image::Handle)>),
}

pub struct GrooveApp {
    state: PlaybackState,
    active_nav: NavSection,
    analyzer: SpectrumAnalyzer,
    spectrum: SpectrumFrame,
    engine: EngineHandle,
    artwork: ArtworkCache,
    scrubbing_position: Option<f32>,
    search_query: String,
    search_artists: Vec<ArtistSummary>,
    search_albums: Vec<AlbumSummary>,
    search_results: Vec<Track>,
    selected_artist: Option<ArtistDetail>,
    viewing_artist: bool,
    show_track_details_banner: bool,
}

impl GrooveApp {
    pub fn new() -> (Self, Task<Message>) {
        let (analyzer, sample_producer) = SpectrumAnalyzer::new();
        let engine = spawn_engine(sample_producer);
        let mut state = PlaybackState::mockup_default();

        // Discover real CoreAudio output devices immediately on startup
        let discovered = groove_audio::enumerate_output_devices();
        if !discovered.is_empty() {
            if let Some(def) = discovered.iter().find(|d| d.is_default).cloned() {
                state.selected_device = def;
            } else if let Some(first) = discovered.first().cloned() {
                state.selected_device = first;
            }
            state.devices = discovered;
        }
        tracing::info!(
            "[UI] GrooveApp initialized (selected_device='{}', is_default={})",
            state.selected_device.name,
            state.selected_device.is_default
        );

        let search_results = state.queue.clone();
        let search_artists = all_curated_artists();
        let search_albums = all_curated_albums();
        let selected_artist = Some(curated_artist_detail("Lume"));

        (
            Self {
                state,
                active_nav: NavSection::NowPlaying,
                analyzer,
                spectrum: SpectrumFrame::calm_preview(0.0),
                engine,
                artwork: ArtworkCache::new(),
                scrubbing_position: None,
                search_query: String::new(),
                search_artists,
                search_albums,
                search_results,
                selected_artist,
                viewing_artist: false,
                show_track_details_banner: false,
            },
            Task::none(),
        )
    }

    pub fn subscription(&self) -> Subscription<Message> {
        // 60 FPS frame ticker driving both the RealFFT ribbon visualizer and non-blocking EngineEvent drain
        iced::time::every(Duration::from_millis(16)).map(|_| Message::AnimationTick)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::AnimationTick => {
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
                    ) {
                        needs_art_scan = true;
                    }
                    self.apply_engine_event(event);
                }

                // Update 60 FPS spectrum frame from live lock-free audio ringbuffer (or harmonic fallback)
                self.spectrum = self.analyzer.next_frame(self.state.is_playing, 0.016);

                if needs_art_scan {
                    return self.spawn_missing_artwork_tasks();
                }
            }
            Message::SelectNav(nav) => {
                tracing::info!("[UI] SelectNav -> {:?}", nav);
                self.viewing_artist = false;
                self.active_nav = nav;
                if nav == NavSection::Search && self.search_artists.is_empty() {
                    let _ = self
                        .engine
                        .command_tx
                        .send(PlayerCommand::SearchCatalog(self.search_query.clone()));
                }
            }
            Message::TogglePlayPause => {
                self.state.is_playing = !self.state.is_playing;
                tracing::info!("[UI] TogglePlayPause clicked -> is_playing={}", self.state.is_playing);
                let _ = self.engine.command_tx.send(PlayerCommand::TogglePlayPause);
            }
            Message::NextTrack => {
                tracing::info!("[UI] NextTrack clicked");
                let _ = self.engine.command_tx.send(PlayerCommand::NextTrack);
            }
            Message::PreviousTrack => {
                tracing::info!("[UI] PreviousTrack clicked");
                let _ = self.engine.command_tx.send(PlayerCommand::PreviousTrack);
            }
            Message::SeekChanged(val) => {
                self.scrubbing_position = Some(val);
                self.state.position_secs = val;
            }
            Message::SeekReleased => {
                if let Some(pos) = self.scrubbing_position.take() {
                    tracing::info!("[UI] SeekReleased -> {pos:.1}s");
                    let _ = self.engine.command_tx.send(PlayerCommand::SeekSeconds(pos));
                }
            }
            Message::VolumeChanged(vol) => {
                self.state.volume = vol.clamp(0.0, 1.0);
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::SetVolume(self.state.volume));
            }
            Message::ToggleShuffle => {
                self.state.shuffle = !self.state.shuffle;
                tracing::info!("[UI] ToggleShuffle -> {}", self.state.shuffle);
                let _ = self.engine.command_tx.send(PlayerCommand::ToggleShuffle);
            }
            Message::CycleRepeat => {
                self.state.repeat = self.state.repeat.next();
                tracing::info!("[UI] CycleRepeat -> {:?}", self.state.repeat);
                let _ = self.engine.command_tx.send(PlayerCommand::CycleRepeat);
            }
            Message::ToggleLike => {
                self.state.current_track.liked = !self.state.current_track.liked;
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
                self.viewing_artist = false;
                self.active_nav = NavSection::NowPlaying;
            }
            Message::PlaySpecificTrack(track) => {
                tracing::info!(
                    "[UI] PlaySpecificTrack: '{}' by '{}' ({})",
                    track.title,
                    track.artist,
                    track.spotify_uri
                );
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::PlayTrack(track));
                self.viewing_artist = false;
                self.active_nav = NavSection::NowPlaying;
            }
            Message::SelectPlaylist(playlist_id) => {
                tracing::info!("[UI] SelectPlaylist('{playlist_id}')");
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::SelectPlaylist(playlist_id));
                self.viewing_artist = false;
                self.active_nav = NavSection::NowPlaying;
            }
            Message::OpenArtist(artist_id_or_name) => {
                tracing::info!("[UI] OpenArtist('{artist_id_or_name}')");
                self.viewing_artist = true;
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::SelectArtist(artist_id_or_name));
            }
            Message::CloseArtistView => {
                tracing::info!("[UI] CloseArtistView");
                self.viewing_artist = false;
            }
            Message::PlayAlbum(album_uri) => {
                tracing::info!("[UI] PlayAlbum('{album_uri}')");
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::PlayAlbum(album_uri));
                self.viewing_artist = false;
                self.active_nav = NavSection::NowPlaying;
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
                        self.viewing_artist = false;
                        self.active_nav = NavSection::NowPlaying;
                    }
                }
            }
            Message::CreateNewPlaylist => {
                let next_idx = self.state.playlists.len() + 1;
                let new_id = format!("custom-playlist-{next_idx}");
                tracing::info!("[UI] CreateNewPlaylist -> '{new_id}'");
                self.state.playlists.push(groove_core::Playlist {
                    id: new_id,
                    spotify_uri: format!("spotify:playlist:custom{next_idx}"),
                    name: format!("Late Night #{next_idx}"),
                    subtitle: "Custom Groove mixtape".into(),
                    track_count: 12,
                    is_playing_context: false,
                    artwork_theme: groove_core::ArtworkTheme::AlpineMist,
                });
            }
            Message::SelectOutputDevice(device) => {
                tracing::info!("[UI] SelectOutputDevice -> '{}' (id={})", device.name, device.id);
                self.state.selected_device = device.clone();
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
            Message::SearchQueryChanged(query) => {
                tracing::info!("[UI] SearchQueryChanged -> '{}'", query);
                self.search_query = query.clone();
                let _ = self
                    .engine
                    .command_tx
                    .send(PlayerCommand::SearchCatalog(query));
            }
            Message::ToggleTrackMenu => {
                self.show_track_details_banner = !self.show_track_details_banner;
            }
            Message::ArtworkLoaded(Some((url, handle))) => {
                tracing::info!("[UI] ArtworkLoaded -> cached texture for {}", url);
                self.artwork.insert_remote(url, handle);
            }
            Message::ArtworkLoaded(None) => {}
        }
        Task::none()
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
                    self.state.position_secs = pos;
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
                self.state.devices = devices;
            }
            EngineEvent::ConnectionUpdated(conn) => {
                tracing::info!("[UI] EngineEvent::ConnectionUpdated -> {:?}", conn);
                self.state.connection = conn;
            }
            EngineEvent::SearchResults {
                query,
                artists,
                albums,
                tracks,
            } => {
                tracing::info!(
                    "[UI] EngineEvent::SearchResults(query='{}') -> {} artists, {} albums, {} tracks",
                    query,
                    artists.len(),
                    albums.len(),
                    tracks.len()
                );
                self.search_artists = artists;
                self.search_albums = albums;
                self.search_results = tracks;
            }
            EngineEvent::ArtistDetailLoaded(detail) => {
                tracing::info!(
                    "[UI] EngineEvent::ArtistDetailLoaded('{}') -> {} albums, {} top tracks",
                    detail.artist.name,
                    detail.albums.len(),
                    detail.top_tracks.len()
                );
                self.selected_artist = Some(*detail);
                self.viewing_artist = true;
            }
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let sidebar = self.view_sidebar();
        let center_stage = if self.viewing_artist {
            self.view_artist_stage()
        } else {
            match self.active_nav {
                NavSection::NowPlaying => self.view_now_playing_stage(),
                NavSection::Search => self.view_search_stage(),
                NavSection::Home | NavSection::YourLibrary => self.view_library_stage(),
            }
        };
        let queue_pane = self.view_queue_pane();

        row![sidebar, center_stage, queue_pane]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Column 1: Frosted warm-stone left sidebar matching the Groove mockup.
    fn view_sidebar(&self) -> Element<'_, Message> {
        let brand_header = row![
            svg(icons::groove_logo())
                .width(Length::Fixed(30.0))
                .height(Length::Fixed(30.0)),
            column![
                text("Groove")
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

    fn view_nav_item<'a>(
        &self,
        section: NavSection,
        icon_handle: svg::Handle,
    ) -> Element<'a, Message> {
        let is_active = self.active_nav == section && !self.viewing_artist;
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

    /// Column 2: Main "Now Playing" center stage matching the Groove mockup.
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
        .on_press(Message::OpenArtist(track.artist.clone()))
        .padding(Padding::from([1, 4]))
        .style(theme::icon_ghost_button);

        let title_and_actions = row![
            column![
                text(&track.title)
                    .font(SERIF_FONT)
                    .size(31)
                    .color(palette::INK_PRIMARY),
                artist_link_btn,
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
        let current_pos = self.state.position_secs.clamp(0.0, duration_f);

        let seek_slider = slider(0.0..=duration_f, current_pos, Message::SeekChanged)
            .on_release(Message::SeekReleased)
            .step(0.25_f32)
            .width(Length::Fixed(430.0))
            .style(theme::seek_slider_style);

        let timestamps_row = row![
            text(format_seconds(current_pos as u32))
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
        .padding(Padding::from([24, 32]));

        container(stage_column)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center)
            .style(theme::center_stage_container)
            .into()
    }

    fn view_spotify_auth_banner(&self) -> Element<'_, Message> {
        match &self.state.connection {
            ConnectionStatus::Connected {
                username,
                live_session: true,
                ..
            } => container(
                row![
                    svg(icons::spotify_badge())
                        .width(Length::Fixed(18.0))
                        .height(Length::Fixed(18.0)),
                    text(format!(
                        "Spotify Premium active as {username} • 320 kbps Ogg Vorbis + live catalog"
                    ))
                    .size(12.0)
                    .color(palette::INK_SECONDARY)
                    .width(Length::Fill),
                    button(text("Re-auth").size(11.5))
                        .on_press(Message::ConnectSpotifyClicked { force_reauth: true })
                        .padding(Padding::from([4, 10]))
                        .style(theme::subtle_row_button(true)),
                    button(text("Sign Out").size(11.5))
                        .on_press(Message::LogoutSpotifyClicked)
                        .padding(Padding::from([4, 10]))
                        .style(theme::subtle_row_button(false)),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([8, 12]))
            .style(|_| container::Style {
                background: Some(iced::Background::Color(palette::SIDEBAR_ACTIVE_PILL)),
                border: iced::Border {
                    radius: 12.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into(),

            ConnectionStatus::Connecting { status } => container(
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

            _ => container(
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
        }
    }

    fn view_search_stage(&self) -> Element<'_, Message> {
        let header = row![
            text("Search & Discover")
                .font(SERIF_FONT)
                .size(32)
                .color(palette::INK_PRIMARY),
            horizontal_space(),
            button(text("Now Playing").size(12.5))
                .on_press(Message::SelectNav(NavSection::NowPlaying))
                .padding(Padding::from([6, 12]))
                .style(theme::nav_pill_button(true)),
        ]
        .align_y(Alignment::Center);

        let input = text_input(
            "Search artists, albums, or tracks on Spotify...",
            &self.search_query,
        )
        .on_input(Message::SearchQueryChanged)
        .padding(12)
        .size(14);

        let mut content_col = column![].spacing(18);

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

                    let alb_card = button(
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
                            text("Play").size(11.5).color(palette::INK_MUTED),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    )
                    .on_press(Message::PlayAlbum(alb.spotify_uri.clone()))
                    .width(Length::FillPortion(1))
                    .padding(Padding::from([8, 10]))
                    .style(theme::subtle_row_button(false));

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

        // 3. Tracks section
        let mut tracks_col = column![].spacing(6);
        let list = if self.search_query.trim().is_empty() && self.search_results.is_empty() {
            &self.state.queue
        } else {
            &self.search_results
        };

        for track in list {
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

        container(
            column![
                vertical_space().height(Length::Fixed(16.0)),
                header,
                self.view_spotify_auth_banner(),
                input,
                scrollable(content_col).height(Length::Fill),
            ]
            .spacing(14)
            .padding(Padding::from([24, 34])),
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

        let nav_bar = row![
            button(text("← Back").size(12.5))
                .on_press(Message::CloseArtistView)
                .padding(Padding::from([6, 12]))
                .style(theme::subtle_row_button(true)),
            horizontal_space(),
            button(text("Now Playing").size(12.5))
                .on_press(Message::SelectNav(NavSection::NowPlaying))
                .padding(Padding::from([6, 12]))
                .style(theme::nav_pill_button(true)),
        ]
        .align_y(Alignment::Center);

        let portrait = container(
            image(self.artwork.hero_or_url(
                detail.artist.portrait_url.as_deref(),
                detail.artist.artwork_theme,
            ))
            .width(Length::Fixed(116.0))
            .height(Length::Fixed(116.0)),
        )
        .style(theme::album_art_shadow_card);

        let followers_text = detail
            .artist
            .followers
            .map(|f| format!("{} followers", format_compact_count(f)))
            .unwrap_or_else(|| "Verified Artist".to_string());

        let genre_line = if detail.artist.genres.is_empty() {
            followers_text
        } else {
            format!("{}  •  {}", detail.artist.subtitle(), followers_text)
        };

        let bio_text = detail.biography.as_deref().unwrap_or(
            "Explore studio albums, singles, and high-resolution 320 kbps streams on Groove.",
        );

        let hero_header = row![
            portrait,
            column![
                text(&detail.artist.name)
                    .font(SERIF_FONT)
                    .size(36)
                    .color(palette::INK_PRIMARY),
                text(genre_line).size(12.5).color(palette::INK_SECONDARY),
                text(bio_text).size(12.5).color(palette::INK_MUTED),
                row![
                    button(text("▶  Play Top Tracks").size(12.5))
                        .on_press(Message::PlayArtistTopTracks)
                        .padding(Padding::from([7, 14]))
                        .style(theme::subtle_row_button(true)),
                ]
                .spacing(8),
            ]
            .spacing(5)
            .width(Length::Fill),
        ]
        .spacing(18)
        .align_y(Alignment::Center);

        // Discography — Albums & Singles with artwork
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

                let card = button(
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
                .on_press(Message::PlayAlbum(alb.spotify_uri.clone()))
                .width(Length::FillPortion(1))
                .padding(Padding::from([10, 10]))
                .style(theme::subtle_row_button(false));

                album_row = album_row.push(card);
            }
            for _ in chunk.len()..3 {
                album_row = album_row.push(horizontal_space().width(Length::FillPortion(1)));
            }
            albums_section = albums_section.push(album_row);
        }

        // Popular Tracks section
        let mut top_tracks_col = column![
            text("Popular Tracks")
                .font(SERIF_FONT)
                .size(23)
                .color(palette::INK_PRIMARY)
        ]
        .spacing(6);

        for (idx, track) in detail.top_tracks.iter().enumerate() {
            let is_current = track.id == self.state.current_track.id;
            let track_btn = button(
                row![
                    text(format!("{:02}", idx + 1))
                        .size(12)
                        .color(palette::INK_MUTED)
                        .width(Length::Fixed(22.0)),
                    image(
                        self.artwork
                            .thumb_or_url(track.cover_art_url.as_deref(), track.artwork_theme)
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
            .style(theme::subtle_row_button(is_current));

            top_tracks_col = top_tracks_col.push(track_btn);
        }

        let body_scroll = scrollable(
            column![
                hero_header,
                horizontal_rule(1),
                albums_section,
                horizontal_rule(1),
                top_tracks_col,
            ]
            .spacing(18),
        )
        .height(Length::Fill);

        container(
            column![
                vertical_space().height(Length::Fixed(16.0)),
                nav_bar,
                body_scroll,
            ]
            .spacing(14)
            .padding(Padding::from([24, 34])),
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
            text(heading)
                .font(SERIF_FONT)
                .size(32)
                .color(palette::INK_PRIMARY),
            horizontal_space(),
            button(text("Open Now Playing").size(12.5))
                .on_press(Message::SelectNav(NavSection::NowPlaying))
                .padding(Padding::from([6, 12]))
                .style(theme::nav_pill_button(true)),
        ]
        .align_y(Alignment::Center);

        let mut featured_artists_row = row![].spacing(10);
        for artist in self.search_artists.iter().take(3) {
            let portrait = image(
                self.artwork
                    .thumb_or_url(artist.portrait_url.as_deref(), artist.artwork_theme),
            )
            .width(Length::Fixed(48.0))
            .height(Length::Fixed(48.0));

            let artist_btn = button(
                row![
                    portrait,
                    column![
                        text(&artist.name)
                            .font(SERIF_FONT)
                            .size(17)
                            .color(palette::INK_PRIMARY),
                        text("View Albums ›")
                            .size(11.5)
                            .color(palette::INK_SECONDARY),
                    ]
                    .spacing(2)
                    .width(Length::Fill),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .on_press(Message::OpenArtist(artist.spotify_uri.clone()))
            .width(Length::FillPortion(1))
            .padding(Padding::from([8, 10]))
            .style(theme::subtle_row_button(false));

            featured_artists_row = featured_artists_row.push(artist_btn);
        }

        let mut playlist_cards = column![
            text("Featured Artists")
                .font(SERIF_FONT)
                .size(22)
                .color(palette::INK_PRIMARY),
            featured_artists_row,
            vertical_space().height(Length::Fixed(4.0)),
            text("Playlists")
                .font(SERIF_FONT)
                .size(22)
                .color(palette::INK_PRIMARY),
        ]
        .spacing(10);
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

        container(
            column![
                vertical_space().height(Length::Fixed(16.0)),
                header,
                self.view_spotify_auth_banner(),
                scrollable(playlist_cards).height(Length::Fill),
            ]
            .spacing(14)
            .padding(Padding::from([24, 34])),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::center_stage_container)
        .into()
    }

    /// Column 3: Right "Up Next" queue pane matching the Groove mockup.
    fn view_queue_pane(&self) -> Element<'_, Message> {
        let header = text("Up Next")
            .font(SERIF_FONT)
            .size(25)
            .color(palette::INK_PRIMARY);

        let mut list = column![].spacing(10);
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

        let content = column![
            vertical_space().height(Length::Fixed(34.0)),
            container(header).padding(Padding {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artwork_cache_generates_all_themes() {
        let cache = ArtworkCache::new();
        let _hero = cache.hero_handle(groove_core::ArtworkTheme::SunriseMountainLake);
        let _thumb = cache.thumb_handle(groove_core::ArtworkTheme::GoldenCanyon);
    }

    #[test]
    fn app_initializes_and_handles_navigation_and_playback_messages() {
        let (mut app, _) = GrooveApp::new();
        assert_eq!(app.state.current_track.title, "Higher Ground");
        assert_eq!(app.active_nav, NavSection::NowPlaying);

        let _ = app.update(Message::SelectNav(NavSection::Search));
        assert_eq!(app.active_nav, NavSection::Search);

        let _ = app.update(Message::ToggleLike);
        assert!(app.state.current_track.liked);

        let _ = app.update(Message::AnimationTick);
        for &band in &app.spectrum.bands {
            assert!(band.is_finite() && (0.0..=1.0).contains(&band));
        }
    }
}
