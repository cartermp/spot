use serde::{Deserialize, Serialize};

/// Number of logarithmically spaced frequency bands used by the ribbon visualizer.
pub const SPECTRUM_BANDS: usize = 32;

/// Primary navigation sections in the left sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NavSection {
    Home,
    Search,
    NowPlaying,
    YourLibrary,
}

impl NavSection {
    pub fn label(self) -> &'static str {
        match self {
            NavSection::Home => "Home",
            NavSection::Search => "Search",
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
            ConnectionStatus::Connected { username, .. } => format!("Connected as {username}"),
            ConnectionStatus::Connecting { status } => status.clone(),
            ConnectionStatus::Disconnected {
                reason: Some(reason),
            } => format!("Offline ({reason})"),
            ConnectionStatus::Disconnected { reason: None } => "Click to connect Premium".to_string(),
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, ConnectionStatus::Connected { .. })
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
    pub fn calm_preview(phase: f32) -> Self {
        let mut bands = [0.0f32; SPECTRUM_BANDS];
        for (i, band) in bands.iter_mut().enumerate() {
            let x = i as f32 / (SPECTRUM_BANDS - 1) as f32;
            let envelope = (x * std::f32::consts::PI).sin().powi(2);
            let wave1 = (x * 6.5 + phase * 1.1).sin() * 0.28;
            let wave2 = (x * 13.0 - phase * 0.7).cos() * 0.16;
            let center_boost = (-((x - 0.52) * 4.2).powi(2)).exp() * 0.38;
            *band = ((0.36 + wave1 + wave2 + center_boost) * envelope).clamp(0.02, 1.0);
        }
        Self {
            bands,
            rms_energy: 0.58,
            bass_energy: 0.62,
            mid_energy: 0.74,
            high_energy: 0.48,
            phase,
        }
    }
}

/// Complete observable state of the player.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackState {
    pub current_track: Track,
    pub queue: Vec<Track>,
    pub playlists: Vec<Playlist>,
    pub active_playlist_id: Option<String>,
    pub is_playing: bool,
    pub position_secs: f32,
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub devices: Vec<AudioDevice>,
    pub selected_device: AudioDevice,
    pub connection: ConnectionStatus,
}

impl PlaybackState {
    /// Constructs the initial player state matching the Groove design mockup.
    pub fn mockup_default() -> Self {
        let queue = vec![
            Track {
                id: "higher-ground".into(),
                spotify_uri: "spotify:track:4uLU6hMCjMI75M1A2tKUQC".into(),
                title: "Higher Ground".into(),
                artist: "Lume".into(),
                album: "Solstice".into(),
                duration_secs: 4 * 60 + 28, // 4:28
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::SunriseMountainLake,
            },
            Track {
                id: "golden-hours".into(),
                spotify_uri: "spotify:track:5FVd6KXrgO9B3JPmC8OPst".into(),
                title: "Golden Hours".into(),
                artist: "Kairo".into(),
                album: "Afterglow".into(),
                duration_secs: 3 * 60 + 16, // 3:16
                liked: true,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::GoldenCanyon,
            },
            Track {
                id: "echoes-in-bloom".into(),
                spotify_uri: "spotify:track:0VjIjW4GlUZAMYd2vXMi3b".into(),
                title: "Echoes in Bloom".into(),
                artist: "Vallé".into(),
                album: "Nocturne".into(),
                duration_secs: 4 * 60 + 3, // 4:03
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::MoonlitClouds,
            },
            Track {
                id: "still-here".into(),
                spotify_uri: "spotify:track:7qiZfU4dY1lWllzX7mPBI3".into(),
                title: "Still Here".into(),
                artist: "Nyra".into(),
                album: "Quiet Tides".into(),
                duration_secs: 3 * 60 + 41, // 3:41
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::MistyArch,
            },
            Track {
                id: "letters-to-you".into(),
                spotify_uri: "spotify:track:3n3Ppam7vgaVa1iaRUc9Lp".into(),
                title: "Letters to You".into(),
                artist: "Caspian".into(),
                album: "Far Shores".into(),
                duration_secs: 5 * 60 + 2, // 5:02
                liked: true,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::CoastalIsland,
            },
        ];

        let playlists = vec![
            Playlist {
                id: "chill-focus".into(),
                spotify_uri: "spotify:playlist:37i9dQZF1DWZeKCadgRdKQ".into(),
                name: "Chill Focus".into(),
                subtitle: "Ambient & post-rock for deep flow".into(),
                track_count: 42,
                is_playing_context: true,
                artwork_theme: ArtworkTheme::TwilightHorizon,
            },
            Playlist {
                id: "morning-light".into(),
                spotify_uri: "spotify:playlist:37i9dQZF1DXdd3gw5QVjt9".into(),
                name: "Morning Light".into(),
                subtitle: "Warm acoustic & folktronica".into(),
                track_count: 28,
                is_playing_context: false,
                artwork_theme: ArtworkTheme::DawnReflection,
            },
            Playlist {
                id: "deep-work".into(),
                spotify_uri: "spotify:playlist:37i9dQZF1DWZqd5JICZI0u".into(),
                name: "Deep Work".into(),
                subtitle: "Minimalist synth & neo-classical".into(),
                track_count: 64,
                is_playing_context: false,
                artwork_theme: ArtworkTheme::AlpineMist,
            },
            Playlist {
                id: "indie-favourites".into(),
                spotify_uri: "spotify:playlist:37i9dQZF1DX2Nc3B70tvx0".into(),
                name: "Indie Favourites".into(),
                subtitle: "Handpicked indie & dream pop".into(),
                track_count: 51,
                is_playing_context: false,
                artwork_theme: ArtworkTheme::WarmDusk,
            },
            Playlist {
                id: "on-repeat".into(),
                spotify_uri: "spotify:playlist:37i9dQZF1EpzZ5fP2nJ1sR".into(),
                name: "On Repeat".into(),
                subtitle: "Songs you can't put down".into(),
                track_count: 30,
                is_playing_context: false,
                artwork_theme: ArtworkTheme::EmeraldRing,
            },
            Playlist {
                id: "discover-weekly".into(),
                spotify_uri: "spotify:playlist:37i9dQZEVXcJZyENOWUFo7".into(),
                name: "Discover Weekly".into(),
                subtitle: "Your weekly mixtape of fresh music".into(),
                track_count: 30,
                is_playing_context: false,
                artwork_theme: ArtworkTheme::VioletAura,
            },
        ];

        let default_device = AudioDevice {
            id: "macbook-pro-speakers".into(),
            name: "MacBook Pro Speakers".into(),
            is_default: true,
            is_spotify_connect: false,
        };

        let devices = vec![
            default_device.clone(),
            AudioDevice {
                id: "airpods-pro".into(),
                name: "AirPods Pro".into(),
                is_default: false,
                is_spotify_connect: false,
            },
            AudioDevice {
                id: "studio-display-speakers".into(),
                name: "Studio Display Speakers".into(),
                is_default: false,
                is_spotify_connect: false,
            },
        ];

        Self {
            current_track: queue[0].clone(),
            queue,
            playlists,
            active_playlist_id: Some("chill-focus".into()),
            is_playing: true,
            position_secs: 2.0 * 60.0 + 17.0, // 2:17 from mockup
            volume: 0.45,
            shuffle: false,
            repeat: RepeatMode::Off,
            devices,
            selected_device: default_device,
            connection: ConnectionStatus::Connected {
                username: "Alex".into(),
                is_premium: true,
                live_session: false,
            },
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
    SelectPlaylist(String),
    SelectOutputDevice(AudioDevice),
    AuthenticateSpotify { client_id: Option<String> },
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
    SearchResults { query: String, tracks: Vec<Track> },
}

/// Formats seconds as `M:SS` (e.g., `2:17`, `4:28`).
pub fn format_seconds(total_secs: u32) -> String {
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{mins}:{secs:02}")
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
        assert_eq!(state.current_track.title, "Higher Ground");
        assert_eq!(state.current_track.artist, "Lume");
        assert_eq!(state.queue.len(), 5);
        assert_eq!(state.playlists.len(), 6);
        assert_eq!(state.position_secs as u32, 137);
    }
}
