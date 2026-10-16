use serde::{Deserialize, Serialize};

pub mod formal;
pub use formal::{
    advance_queue_verified, coprime_shuffle_step, delete_last_word_verified,
    episode_to_playable_track_verified, format_connected_account_label, gcd_usize,
    is_opaque_spotify_user_id, merge_devices_verified, normalize_album_playback_queue,
    normalize_spotify_episode_uri, normalize_spotify_show_uri, resolve_playable_track_candidate,
    sanitize_pcm_sample, sanitize_position_secs, sanitize_spotify_biography, sanitize_unit_float,
    select_device_verified, summarize_artist_bio, FormalInvariant, InvariantViolation,
    NavHistoryMachine, NavRouteTarget, RecentListenedItem, RecentListenedKind,
    RecentListenedStore, RecentSearchEntry, RecentSearchKind, RecentSearchStore,
    TrayMenuSnapshot, VerifiedTransportAction, RECENTLY_LISTENED_TOP_LIMIT,
};

/// Number of logarithmically spaced frequency bands used by the ribbon visualizer.
pub const SPECTRUM_BANDS: usize = 32;

/// Primary navigation sections in the left sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NavSection {
    Home,
    Search,
    Podcasts,
    NowPlaying,
    YourLibrary,
}

impl NavSection {
    pub fn label(self) -> &'static str {
        match self {
            NavSection::Home => "Home",
            NavSection::Search => "Search",
            NavSection::Podcasts => "Podcasts",
            NavSection::NowPlaying => "Now Playing",
            NavSection::YourLibrary => "Your Library",
        }
    }
}

/// Repeat mode for transport controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepeatMode {
    Off,
    Context,
    Track,
}

impl RepeatMode {
    pub fn next(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::Context,
            RepeatMode::Context => RepeatMode::Track,
            RepeatMode::Track => RepeatMode::Off,
        }
    }
}

/// Visual theme seed for procedural/cached artwork generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArtworkTheme {
    /// Warm peach moon / turquoise mountain lake ("Higher Ground - Lume")
    SunriseMountainLake,
    /// Warm amber dusk canyon peaks ("Golden Hours - Kairo")
    GoldenCanyon,
    /// Lavender / indigo crescent moon & clouds ("Echoes in Bloom - Vallé")
    MoonlitClouds,
    /// Moody dark slate cave & silhouette ("Still Here - Nyra")
    MistyArch,
    /// Golden coastal island reflection ("Letters to You - Caspian")
    CoastalIsland,
    /// Deep blue twilight horizon ("Chill Focus")
    TwilightHorizon,
    /// Warm sunrise over calm sea ("Morning Light")
    DawnReflection,
    /// Teal alpine mist ("Deep Work")
    AlpineMist,
    /// Warm autumn dusk ("Indie Favourites")
    WarmDusk,
    /// Emerald neon ring ("On Repeat")
    EmeraldRing,
    /// Violet-magenta gradient ("Discover Weekly")
    VioletAura,
}

/// Metadata for a single playable track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub spotify_uri: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: u32,
    pub liked: bool,
    pub cover_art_url: Option<String>,
    pub artwork_theme: ArtworkTheme,
}

impl Track {
    pub fn formatted_duration(&self) -> String {
        format_seconds(self.duration_secs)
    }
}

/// User playlist summary shown in the left sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub spotify_uri: String,
    pub name: String,
    pub subtitle: String,
    pub track_count: usize,
    pub is_playing_context: bool,
    pub artwork_theme: ArtworkTheme,
}

/// Summary card for an artist in Search or Artist view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistSummary {
    pub id: String,
    pub spotify_uri: String,
    pub name: String,
    pub genres: Vec<String>,
    pub followers: Option<u64>,
    pub portrait_url: Option<String>,
    pub artwork_theme: ArtworkTheme,
}

impl ArtistSummary {
    pub fn subtitle(&self) -> String {
        if !self.genres.is_empty() {
            self.genres
                .iter()
                .take(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(" · ")
        } else if let Some(followers) = self.followers {
            format!("{} followers", format_compact_count(followers))
        } else {
            "Artist".to_string()
        }
    }
}

/// Summary card for an album or single belonging to an artist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumSummary {
    pub id: String,
    pub spotify_uri: String,
    pub title: String,
    pub artist_name: String,
    pub release_year: String,
    pub album_type: String,
    pub track_count: usize,
    pub cover_art_url: Option<String>,
    pub artwork_theme: ArtworkTheme,
}

/// Full album detail including metadata, parent artist reference, and tracklist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumDetail {
    pub album: AlbumSummary,
    pub artist_uri: Option<String>,
    pub tracks: Vec<Track>,
}

/// Full artist profile including portrait, bio, top tracks, and discography albums.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistDetail {
    pub artist: ArtistSummary,
    pub biography: Option<String>,
    pub top_tracks: Vec<Track>,
    pub albums: Vec<AlbumSummary>,
}

/// Summary card for a podcast show (`spotify:show:<id>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodcastShowSummary {
    pub id: String,
    pub spotify_uri: String,
    pub title: String,
    pub publisher: String,
    pub description: String,
    pub category: String,
    pub episode_count: usize,
    pub cover_art_url: Option<String>,
    pub artwork_theme: ArtworkTheme,
}

/// Metadata for a single playable podcast episode (`spotify:episode:<id>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodcastEpisode {
    pub id: String,
    pub spotify_uri: String,
    pub title: String,
    pub show_title: String,
    pub show_uri: String,
    pub description: String,
    pub release_date: String,
    pub duration_secs: u32,
    pub cover_art_url: Option<String>,
    pub artwork_theme: ArtworkTheme,
}

impl PodcastEpisode {
    pub fn formatted_duration(&self) -> String {
        format_episode_duration(self.duration_secs)
    }

    pub fn into_playable_track(&self) -> Track {
        episode_to_playable_track_verified(self)
    }
}

/// Full podcast show detail including show metadata, description, and episode list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodcastShowDetail {
    pub show: PodcastShowSummary,
    pub episodes: Vec<PodcastEpisode>,
}

/// Formats podcast episode duration in human-readable form (e.g., `42 min`, `1 hr 18 min`).
pub fn format_episode_duration(total_secs: u32) -> String {
    let total_mins = (total_secs.max(60)) / 60;
    let hours = total_mins / 60;
    let mins = total_mins % 60;
    if hours > 0 {
        if mins == 0 {
            format!("{hours} hr")
        } else {
            format!("{hours} hr {mins} min")
        }
    } else {
        format!("{total_mins} min")
    }
}

/// Formats follower counts compactly (e.g. `1.4M`, `284K`).
pub fn format_compact_count(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}K", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

/// Local CoreAudio output device or Spotify Connect remote target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub is_spotify_connect: bool,
}

impl std::fmt::Display for AudioDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

/// Current Spotify session connection state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    /// Connected to Spotify Premium (or ready with cached session).
    Connected {
        username: String,
        is_premium: bool,
        live_session: bool,
    },
    /// Currently running OAuth PKCE browser login or connecting session.
    Connecting { status: String },
    /// Disconnected or awaiting login; includes last error if any.
    Disconnected { reason: Option<String> },
}

impl ConnectionStatus {
    pub fn display_label(&self) -> String {
        match self {
            ConnectionStatus::Connected {
                username,
                is_premium,
                live_session: true,
            } => format_connected_account_label(username, *is_premium),
            ConnectionStatus::Connected {
                username,
                is_premium,
                live_session: false,
            } => format_connected_account_label(username, *is_premium),
            ConnectionStatus::Connecting { status } => status.clone(),
            ConnectionStatus::Disconnected {
                reason: Some(reason),
            } => format!("Login failed ({reason})"),
            ConnectionStatus::Disconnected { reason: None } => {
                "Sign in with Spotify Premium".to_string()
            }
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, ConnectionStatus::Connected { .. })
    }

    pub fn is_live_session(&self) -> bool {
        matches!(
            self,
            ConnectionStatus::Connected {
                live_session: true,
                ..
            }
        )
    }
}

/// Real-time smoothed FFT spectrum snapshot passed from `groove-audio` to the GPU ribbon visualizer.
#[derive(Debug, Clone, Copy)]
pub struct SpectrumFrame {
    /// 32 logarithmically spaced frequency bands normalized to `0.0..=1.0`.
    pub bands: [f32; SPECTRUM_BANDS],
    /// Smoothed RMS energy (`0.0..=1.0`).
    pub rms_energy: f32,
    /// Bass energy envelope (`0.0..=1.0`).
    pub bass_energy: f32,
    /// Midrange energy envelope (`0.0..=1.0`).
    pub mid_energy: f32,
    /// High/presence energy envelope (`0.0..=1.0`).
    pub high_energy: f32,
    /// Monotonically advancing wave phase in radians.
    pub phase: f32,
}

impl Default for SpectrumFrame {
    fn default() -> Self {
        Self::calm_preview(0.0)
    }
}

impl SpectrumFrame {
    /// Generates an organic, studio-grade multi-band spectrum envelope matching the mockup's wave shape
    /// when audio is paused or running in preview mode.
    /// Total over all IEEE-754 `f32` values (`NaN`, `±Inf`, negative phases).
    pub fn calm_preview(phase: f32) -> Self {
        let max_phase = 20.0 * std::f32::consts::TAU;
        let clean_phase = if phase.is_finite() {
            phase.rem_euclid(max_phase)
        } else {
            0.0
        };
        let mut bands = [0.0f32; SPECTRUM_BANDS];
        for (i, band) in bands.iter_mut().enumerate() {
            let x = i as f32 / (SPECTRUM_BANDS - 1) as f32;
            let envelope = (x * std::f32::consts::PI).sin().powi(2);
            let wave1 = (x * 6.5 + clean_phase * 1.1).sin() * 0.28;
            let wave2 = (x * 13.0 - clean_phase * 0.7).cos() * 0.16;
            let center_boost = (-((x - 0.52) * 4.2).powi(2)).exp() * 0.38;
            *band = sanitize_unit_float(
                ((0.36 + wave1 + wave2 + center_boost) * envelope).clamp(0.02, 1.0),
                0.02,
            );
        }
        Self {
            bands,
            rms_energy: 0.58,
            bass_energy: 0.62,
            mid_energy: 0.74,
            high_energy: 0.48,
            phase: clean_phase,
        }
    }
}

/// Ordered tracklist of a single album inside an artist's discography playback sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumPlaybackQueue {
    pub album_uri: String,
    pub album_title: String,
    pub artist_name: String,
    pub tracks: Vec<Track>,
}

/// Formal playback context governing `Up Next` (`state.queue`) and end-of-context progression.
///
/// - **Playlist**: Only when `context` is `PlaybackContext::Playlist` does `active_playlist_id`
///   equal `Some(playlist_id)` and does advancing move through a playlist.
/// - **Album**: `Up Next` (`state.queue`) consists exclusively of the tracks of
///   `artist_albums[current_album_index]`. When that album finishes, advancing transitions to
///   `artist_albums[current_album_index + 1]` (the next album by that artist). Once all albums
///   in `artist_albums` have been played through, `exhausted` becomes `true` and `is_playing`
///   becomes `false` (plays nothing).
/// - **PodcastShow**: `Up Next` (`state.queue`) consists of the episodes of the active show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PlaybackContext {
    #[default]
    None,
    Playlist {
        playlist_id: String,
    },
    Album {
        artist_name: String,
        artist_uri: Option<String>,
        artist_albums: Vec<AlbumPlaybackQueue>,
        current_album_index: usize,
        exhausted: bool,
    },
    PodcastShow {
        show_uri: String,
        show_title: String,
    },
}

/// Complete observable state of the player.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackState {
    pub current_track: Track,
    pub queue: Vec<Track>,
    pub playlists: Vec<Playlist>,
    pub active_playlist_id: Option<String>,
    #[serde(default)]
    pub context: PlaybackContext,
    pub is_playing: bool,
    pub position_secs: f32,
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub devices: Vec<AudioDevice>,
    pub selected_device: AudioDevice,
    pub connection: ConnectionStatus,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self::mockup_default()
    }
}

impl PlaybackState {
    /// Constructs a clean initial player state without template/placeholder music.
    pub fn mockup_default() -> Self {
        let default_device = AudioDevice {
            id: "system-default".into(),
            name: "System Default Output".into(),
            is_default: true,
            is_spotify_connect: false,
        };

        Self {
            current_track: Track {
                id: String::new(),
                spotify_uri: String::new(),
                title: "Ready to Play".into(),
                artist: "Select a playlist, album, or track".into(),
                album: String::new(),
                duration_secs: 0,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::SunriseMountainLake,
            },
            queue: Vec::new(),
            playlists: Vec::new(),
            active_playlist_id: None,
            context: PlaybackContext::None,
            is_playing: false,
            position_secs: 0.0,
            volume: 0.45,
            shuffle: false,
            repeat: RepeatMode::Off,
            devices: vec![default_device.clone()],
            selected_device: default_device,
            connection: ConnectionStatus::Disconnected { reason: None },
        }
    }

    /// Returns `true` if `context` is `PlaybackContext::Album` and all albums by the artist
    /// have been played through (`exhausted == true`).
    pub fn is_discography_exhausted(&self) -> bool {
        matches!(
            self.context,
            PlaybackContext::Album {
                exhausted: true,
                ..
            }
        )
    }

    /// Returns the currently active `AlbumPlaybackQueue` when playing an album.
    pub fn active_album_queue(&self) -> Option<&AlbumPlaybackQueue> {
        match &self.context {
            PlaybackContext::Album {
                artist_albums,
                current_album_index,
                ..
            } => artist_albums.get(*current_album_index),
            _ => None,
        }
    }

    /// Returns the next album by the current artist when playing an album, if one exists.
    pub fn next_artist_album_queue(&self) -> Option<&AlbumPlaybackQueue> {
        match &self.context {
            PlaybackContext::Album {
                artist_albums,
                current_album_index,
                exhausted: false,
                ..
            } => artist_albums.get(current_album_index + 1),
            _ => None,
        }
    }

    /// Returns `true` if `current_track` represents an active playable track or episode.
    pub fn has_active_track(&self) -> bool {
        !self.current_track.id.is_empty()
            && !self.current_track.title.trim().is_empty()
            && self.current_track.id != "no-track"
    }

    /// Canonical sanitized playback position in `[0.0, current_track.duration_secs]`.
    pub fn synced_position_secs(&self) -> f32 {
        sanitize_position_secs(self.position_secs, self.current_track.duration_secs)
    }

    /// Canonical integer elapsed seconds shown in both the main player scrubber and the macOS tray.
    pub fn synced_elapsed_secs(&self) -> u32 {
        (self.synced_position_secs().floor() as u32).min(self.current_track.duration_secs)
    }

    /// Canonical integer remaining seconds (`duration_secs - elapsed_secs`).
    pub fn synced_remaining_secs(&self) -> u32 {
        self.current_track
            .duration_secs
            .saturating_sub(self.synced_elapsed_secs())
    }

    /// Canonical progress ratio in parts-per-thousand (`0..=1000`).
    pub fn synced_progress_permille(&self) -> u16 {
        let dur = self.current_track.duration_secs;
        if dur == 0 {
            0
        } else {
            (((self.synced_elapsed_secs() as u64) * 1000) / (dur as u64)).min(1000) as u16
        }
    }

    /// Canonical playback rate in milli-units (`1000` = `1.0x` when actively playing a track, `0` = `0.0x` when paused/idle).
    pub fn synced_playback_rate_milli(&self) -> u16 {
        if self.is_playing && self.has_active_track() {
            1000
        } else {
            0
        }
    }
}

/// Commands dispatched from the UI thread to the async Spotify + Audio engine.
#[derive(Debug, Clone)]
pub enum PlayerCommand {
    TogglePlayPause,
    Play,
    Pause,
    NextTrack,
    PreviousTrack,
    SeekSeconds(f32),
    SetVolume(f32),
    ToggleShuffle,
    CycleRepeat,
    ToggleLikeCurrent,
    SelectQueueTrack(String),
    PlayTrack(Track),
    SelectPlaylist(String),
    SelectArtist(String),
    SelectAlbum(String),
    PlayAlbum(String),
    PlayAlbumAtTrack {
        album_uri: String,
        track_index: usize,
        preloaded_album: Option<AlbumDetail>,
    },
    SelectShow(String),
    PlayShow(String),
    PlayShowAtEpisode {
        show_uri: String,
        episode_index: usize,
        preloaded_show: Option<PodcastShowDetail>,
    },
    SelectOutputDevice(AudioDevice),
    AuthenticateSpotify {
        client_id: Option<String>,
        force_reauth: bool,
    },
    LogoutSpotify,
    SearchCatalog(String),
    RefreshDevices,
}

/// Events emitted by the Spotify + Audio engine back to the UI thread.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    StateSynced(Box<PlaybackState>),
    PositionTick(f32),
    PlayStateChanged(bool),
    TrackChanged(Track),
    QueueUpdated(Vec<Track>),
    PlaylistsLoaded(Vec<Playlist>),
    DevicesUpdated(Vec<AudioDevice>),
    ConnectionUpdated(ConnectionStatus),
    SearchResults {
        query: String,
        artists: Vec<ArtistSummary>,
        albums: Vec<AlbumSummary>,
        shows: Vec<PodcastShowSummary>,
        episodes: Vec<PodcastEpisode>,
        tracks: Vec<Track>,
    },
    ArtistDetailLoaded(Box<ArtistDetail>),
    AlbumDetailLoaded(Box<AlbumDetail>),
    ShowDetailLoaded(Box<PodcastShowDetail>),
}

/// Formats seconds as `M:SS` (e.g., `2:17`, `4:28`) or `H:MM:SS` for long podcast episodes.
pub fn format_seconds(total_secs: u32) -> String {
    if total_secs >= 3600 {
        let hours = total_secs / 3600;
        let mins = (total_secs % 3600) / 60;
        let secs = total_secs % 60;
        format!("{hours}:{mins:02}:{secs:02}")
    } else {
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{mins}:{secs:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_mockup_timestamps_accurately() {
        assert_eq!(format_seconds(137), "2:17");
        assert_eq!(format_seconds(268), "4:28");
        assert_eq!(format_seconds(196), "3:16");
    }

    #[test]
    fn mockup_default_matches_design() {
        let state = PlaybackState::mockup_default();
        assert_eq!(state.current_track.title, "Ready to Play");
        assert!(state.queue.is_empty());
        assert!(state.playlists.is_empty());
        assert!(!state.is_playing);
    }
}
