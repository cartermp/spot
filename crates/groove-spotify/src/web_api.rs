use std::time::Duration;

use groove_core::{ArtworkTheme, PlaybackState, Playlist, Track};
use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;
use tracing::warn;

const SPOTIFY_API_BASE: &str = "https://api.spotify.com/v1";

#[derive(Debug, Error)]
pub enum WebApiError {
    #[error("missing Spotify OAuth bearer token")]
    MissingToken,

    #[error("Spotify Web API HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Spotify Web API returned status {status}: {body}")]
    ApiStatus { status: u16, body: String },
}

/// Authenticated user profile returned from `GET /v1/me`.
#[derive(Debug, Clone, Deserialize)]
pub struct SpotifyUserProfile {
    pub id: String,
    pub display_name: Option<String>,
    pub product: Option<String>,
}

impl SpotifyUserProfile {
    pub fn display_username(&self) -> String {
        self.display_name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&self.id)
            .to_string()
    }

    pub fn is_premium(&self) -> bool {
        self.product
            .as_deref()
            .map(|p| p.eq_ignore_ascii_case("premium"))
            .unwrap_or(true)
    }
}

#[derive(Debug, Deserialize)]
struct PlaylistsResponse {
    #[serde(default)]
    items: Vec<ApiPlaylistItem>,
}

#[derive(Debug, Deserialize)]
struct ApiPlaylistItem {
    id: String,
    uri: Option<String>,
    name: String,
    description: Option<String>,
    tracks: Option<ApiPlaylistTracksRef>,
}

#[derive(Debug, Deserialize)]
struct ApiPlaylistTracksRef {
    #[serde(default)]
    total: usize,
}

#[derive(Debug, Deserialize)]
struct PlaylistTracksResponse {
    #[serde(default)]
    items: Vec<ApiPlaylistTrackWrapper>,
}

#[derive(Debug, Deserialize)]
struct ApiPlaylistTrackWrapper {
    track: Option<ApiTrackItem>,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    tracks: Option<ApiTracksPage>,
}

#[derive(Debug, Deserialize)]
struct ApiTracksPage {
    #[serde(default)]
    items: Vec<ApiTrackItem>,
}

#[derive(Debug, Deserialize)]
struct ApiTrackItem {
    id: Option<String>,
    uri: Option<String>,
    name: String,
    #[serde(default)]
    artists: Vec<ApiArtist>,
    album: Option<ApiAlbum>,
    #[serde(default)]
    duration_ms: u64,
}

#[derive(Debug, Deserialize)]
struct ApiArtist {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ApiAlbum {
    name: String,
    #[serde(default)]
    images: Vec<ApiImage>,
}

#[derive(Debug, Deserialize)]
struct ApiImage {
    url: String,
}

/// Async Spotify Web API client with curated local catalog fallback so playlists, track queues,
/// and catalog search work immediately out-of-the-box as well as with a live OAuth bearer token.
#[derive(Clone)]
pub struct SpotifyWebClient {
    http: Client,
    access_token: Option<String>,
}

impl Default for SpotifyWebClient {
    fn default() -> Self {
        Self::new(None)
    }
}

impl SpotifyWebClient {
    pub fn new(access_token: Option<String>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self { http, access_token }
    }

    pub fn set_access_token(&mut self, access_token: Option<String>) {
        self.access_token = access_token.filter(|t| !t.trim().is_empty());
    }

    pub fn access_token(&self) -> Option<&str> {
        self.access_token.as_deref()
    }

    /// Fetches the current user's Spotify profile (`GET /v1/me`).
    pub async fn fetch_user_profile(&self) -> Result<SpotifyUserProfile, WebApiError> {
        let token = self
            .access_token
            .as_deref()
            .ok_or(WebApiError::MissingToken)?;

        let url = format!("{SPOTIFY_API_BASE}/me");
        let resp = self.http.get(url).bearer_auth(token).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(WebApiError::ApiStatus {
                status: status.as_u16(),
                body,
            });
        }

        Ok(resp.json::<SpotifyUserProfile>().await?)
    }

    /// Fetches the user's playlists from `/v1/me/playlists`, or returns the curated Groove playlists
    /// when offline or without an OAuth bearer token.
    pub async fn fetch_user_playlists(&self) -> Vec<Playlist> {
        if let Some(token) = self.access_token.as_deref() {
            let url = format!("{SPOTIFY_API_BASE}/me/playlists?limit=20");
            match self.http.get(url).bearer_auth(token).send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(parsed) = resp.json::<PlaylistsResponse>().await {
                        if !parsed.items.is_empty() {
                            return parsed
                                .items
                                .into_iter()
                                .enumerate()
                                .map(|(idx, item)| {
                                    let uri = item.uri.unwrap_or_else(|| {
                                        format!("spotify:playlist:{}", item.id)
                                    });
                                    let subtitle = item
                                        .description
                                        .filter(|d| !d.trim().is_empty())
                                        .unwrap_or_else(|| "Spotify Playlist".to_string());
                                    Playlist {
                                        id: item.id,
                                        spotify_uri: uri,
                                        name: item.name,
                                        subtitle,
                                        track_count: item
                                            .tracks
                                            .map(|t| t.total)
                                            .unwrap_or(0),
                                        is_playing_context: idx == 0,
                                        artwork_theme: playlist_theme_for_index(idx),
                                    }
                                })
                                .collect();
                        }
                    }
                }
                Ok(resp) => {
                    warn!("Spotify /v1/me/playlists returned {}; using curated playlists", resp.status());
                }
                Err(err) => {
                    warn!("Spotify /v1/me/playlists failed ({err}); using curated playlists");
                }
            }
        }

        PlaybackState::mockup_default().playlists
    }

    /// Fetches tracks for a given playlist ID. When a live bearer token is present and the playlist
    /// is not one of the built-in curated slugs (or if live fetch succeeds), returns the live tracks;
    /// otherwise returns the rich curated track list for that playlist.
    pub async fn fetch_playlist_tracks(&self, playlist_id: &str) -> Vec<Track> {
        if let Some(token) = self.access_token.as_deref() {
            // Extract raw Spotify playlist ID if passed as a `spotify:playlist:...` URI.
            let api_playlist_id = playlist_id
                .strip_prefix("spotify:playlist:")
                .unwrap_or(playlist_id);

            // Only query Web API if it looks like a real Spotify ID or if not a local slug,
            // though we still fall back to curated tracks on any error.
            let url = format!("{SPOTIFY_API_BASE}/playlists/{api_playlist_id}/tracks?limit=50");
            if let Ok(resp) = self.http.get(url).bearer_auth(token).send().await {
                if resp.status().is_success() {
                    if let Ok(parsed) = resp.json::<PlaylistTracksResponse>().await {
                        let mapped: Vec<Track> = parsed
                            .items
                            .into_iter()
                            .filter_map(|w| w.track)
                            .enumerate()
                            .map(|(idx, item)| map_api_track(item, idx))
                            .collect();
                        if !mapped.is_empty() {
                            return mapped;
                        }
                    }
                }
            }
        }

        curated_tracks_for_playlist(playlist_id)
    }

    /// Searches tracks via `/v1/search?type=track&q=...` when an OAuth bearer token is available,
    /// falling back to instant local search across all curated playlist tracks.
    pub async fn search_tracks(&self, query: &str) -> Vec<Track> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return all_curated_tracks().into_iter().take(10).collect();
        }

        if let Some(token) = self.access_token.as_deref() {
            let req = self
                .http
                .get(format!("{SPOTIFY_API_BASE}/search"))
                .bearer_auth(token)
                .query(&[("type", "track"), ("limit", "20"), ("q", trimmed)]);

            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(parsed) = resp.json::<SearchResponse>().await {
                        if let Some(page) = parsed.tracks {
                            let tracks: Vec<Track> = page
                                .items
                                .into_iter()
                                .enumerate()
                                .map(|(idx, item)| map_api_track(item, idx))
                                .collect();
                            if !tracks.is_empty() {
                                return tracks;
                            }
                        }
                    }
                }
                Ok(resp) => {
                    warn!(
                        "Spotify Web API search returned status {}; falling back to local catalog search",
                        resp.status()
                    );
                }
                Err(err) => {
                    warn!("Spotify Web API search error ({err}); falling back to local catalog search");
                }
            }
        }

        search_curated_tracks(trimmed)
    }
}

fn map_api_track(item: ApiTrackItem, idx: usize) -> Track {
    let id = item
        .id
        .clone()
        .unwrap_or_else(|| format!("spotify-track-{idx}"));
    let spotify_uri = item
        .uri
        .unwrap_or_else(|| format!("spotify:track:{id}"));
    let artist = if item.artists.is_empty() {
        "Unknown Artist".to_string()
    } else {
        item.artists
            .into_iter()
            .map(|a| a.name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let (album, cover_art_url) = match item.album {
        Some(alb) => {
            let cover = alb.images.into_iter().next().map(|img| img.url);
            (alb.name, cover)
        }
        None => ("Single".to_string(), None),
    };
    let duration_secs = ((item.duration_ms + 500) / 1000).max(1) as u32;

    Track {
        id,
        spotify_uri,
        title: item.name,
        artist,
        album,
        duration_secs,
        liked: false,
        cover_art_url,
        artwork_theme: track_theme_for_index(idx),
    }
}

fn track_theme_for_index(idx: usize) -> ArtworkTheme {
    const THEMES: &[ArtworkTheme] = &[
        ArtworkTheme::SunriseMountainLake,
        ArtworkTheme::GoldenCanyon,
        ArtworkTheme::MoonlitClouds,
        ArtworkTheme::MistyArch,
        ArtworkTheme::CoastalIsland,
        ArtworkTheme::TwilightHorizon,
        ArtworkTheme::DawnReflection,
        ArtworkTheme::AlpineMist,
        ArtworkTheme::WarmDusk,
        ArtworkTheme::EmeraldRing,
        ArtworkTheme::VioletAura,
    ];
    THEMES[idx % THEMES.len()]
}

fn playlist_theme_for_index(idx: usize) -> ArtworkTheme {
    const THEMES: &[ArtworkTheme] = &[
        ArtworkTheme::TwilightHorizon,
        ArtworkTheme::DawnReflection,
        ArtworkTheme::AlpineMist,
        ArtworkTheme::WarmDusk,
        ArtworkTheme::EmeraldRing,
        ArtworkTheme::VioletAura,
    ];
    THEMES[idx % THEMES.len()]
}

/// Returns the curated track list for any of Groove's 6 built-in playlists:
/// `Chill Focus`, `Morning Light`, `Deep Work`, `Indie Favourites`, `On Repeat`, `Discover Weekly`.
pub fn curated_tracks_for_playlist(playlist_key: &str) -> Vec<Track> {
    let normalized = playlist_key.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "chill-focus"
        | "chill focus"
        | "spotify:playlist:37i9dQZF1DWZeKCadgRdKQ"
        | "37i9dqzf1dwzekcadgrdkq" => PlaybackState::mockup_default().queue,

        "morning-light"
        | "morning light"
        | "spotify:playlist:37i9dQZF1DXdd3gw5QVjt9"
        | "37i9dqzf1dxdd3gw5qvjt9" => vec![
            Track {
                id: "sunlit-veranda".into(),
                spotify_uri: "spotify:track:6habFhsOp2NvshLv26DqMb".into(),
                title: "Sunlit Veranda".into(),
                artist: "Sora & The Pines".into(),
                album: "Daybreak Letters".into(),
                duration_secs: 3 * 60 + 44,
                liked: true,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::DawnReflection,
            },
            Track {
                id: "amber-tea".into(),
                spotify_uri: "spotify:track:2takcwOaAZWiXQijPHIx7B".into(),
                title: "Amber Tea".into(),
                artist: "Juniper Vale".into(),
                album: "Early Mist".into(),
                duration_secs: 3 * 60 + 12,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::SunriseMountainLake,
            },
            Track {
                id: "first-breeze".into(),
                spotify_uri: "spotify:track:1301WleyT98MSxVHPZCA6M".into(),
                title: "First Breeze".into(),
                artist: "Linden".into(),
                album: "Open Windows".into(),
                duration_secs: 4 * 60 + 8,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::CoastalIsland,
            },
            Track {
                id: "paper-lanterns".into(),
                spotify_uri: "spotify:track:3AJwUDP919kvQ9QcozQPxg".into(),
                title: "Paper Lanterns".into(),
                artist: "Mira Sol".into(),
                album: "Halcyon".into(),
                duration_secs: 3 * 60 + 55,
                liked: true,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::GoldenCanyon,
            },
            Track {
                id: "meadowlark".into(),
                spotify_uri: "spotify:track:0VjIjW4GlUZAMYd2vXMi3b".into(),
                title: "Meadowlark".into(),
                artist: "Alder".into(),
                album: "Wildflower".into(),
                duration_secs: 4 * 60 + 19,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::WarmDusk,
            },
        ],

        "deep-work"
        | "deep work"
        | "spotify:playlist:37i9dQZF1DWZqd5JICZI0u"
        | "37i9dqzf1dwzqd5jiczi0u" => vec![
            Track {
                id: "glass-architecture".into(),
                spotify_uri: "spotify:track:5ChkMS8OtdzJeqyybCc9R5".into(),
                title: "Glass Architecture".into(),
                artist: "Nils & Aether".into(),
                album: "Substructures".into(),
                duration_secs: 5 * 60 + 18,
                liked: true,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::AlpineMist,
            },
            Track {
                id: "monolith-iv".into(),
                spotify_uri: "spotify:track:1BxfuPKGuaTgP7aM0Bbdwr".into(),
                title: "Monolith IV".into(),
                artist: "Klangwerk".into(),
                album: "Tape Studies".into(),
                duration_secs: 4 * 60 + 47,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::TwilightHorizon,
            },
            Track {
                id: "quiet-calculus".into(),
                spotify_uri: "spotify:track:4iV5W9uYEdYUVa79Axb7Rh".into(),
                title: "Quiet Calculus".into(),
                artist: "Helios Ensemble".into(),
                album: "Axioms".into(),
                duration_secs: 6 * 60 + 4,
                liked: true,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::MistyArch,
            },
            Track {
                id: "analog-rain".into(),
                spotify_uri: "spotify:track:7fix27I8s5efT0fD9J2k4N".into(),
                title: "Analog Rain".into(),
                artist: "Vespera".into(),
                album: "Modular Rooms".into(),
                duration_secs: 4 * 60 + 32,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::MoonlitClouds,
            },
            Track {
                id: "northern-observatory".into(),
                spotify_uri: "spotify:track:2Fxmhks0bxGSBdJ92vM42m".into(),
                title: "Northern Observatory".into(),
                artist: "Svalbard".into(),
                album: "Permafrost".into(),
                duration_secs: 5 * 60 + 40,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::EmeraldRing,
            },
        ],

        "indie-favourites"
        | "indie favourites"
        | "spotify:playlist:37i9dQZF1DX2Nc3B70tvx0"
        | "37i9dqzf1dx2nc3b70tvx0" => vec![
            Track {
                id: "velvet-skyline".into(),
                spotify_uri: "spotify:track:0e7ipj03S05BNilyu5bRzt".into(),
                title: "Velvet Skyline".into(),
                artist: "The Pale Arcades".into(),
                album: "Late Cinema".into(),
                duration_secs: 3 * 60 + 38,
                liked: true,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::WarmDusk,
            },
            Track {
                id: "polaroid-summer".into(),
                spotify_uri: "spotify:track:6PQ88X9TkUIAUIZJHW2upE".into(),
                title: "Polaroid Summer".into(),
                artist: "Clover Club".into(),
                album: "Tidal Wave".into(),
                duration_secs: 3 * 60 + 24,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::GoldenCanyon,
            },
            Track {
                id: "silver-bicycle".into(),
                spotify_uri: "spotify:track:3ee8Jmje8o58CHK66QrVC2".into(),
                title: "Silver Bicycle".into(),
                artist: "Marlowe".into(),
                album: "Postcards".into(),
                duration_secs: 4 * 60 + 1,
                liked: true,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::CoastalIsland,
            },
            Track {
                id: "neon-laundromat".into(),
                spotify_uri: "spotify:track:5HCyWlXZPP0y6Gqq8TgA20".into(),
                title: "Neon Laundromat".into(),
                artist: "Sunday Static".into(),
                album: "VHS Dreams".into(),
                duration_secs: 3 * 60 + 15,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::VioletAura,
            },
            Track {
                id: "harbour-lights".into(),
                spotify_uri: "spotify:track:1r9xUipOqoNwggBpENDsvJ".into(),
                title: "Harbour Lights".into(),
                artist: "Sable Sister".into(),
                album: "North Pier".into(),
                duration_secs: 4 * 60 + 22,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::SunriseMountainLake,
            },
        ],

        "on-repeat"
        | "on repeat"
        | "spotify:playlist:37i9dQZF1EpzZ5fP2nJ1sR"
        | "37i9dqzf1epzz5fp2nj1sr" => vec![
            Track {
                id: "midnight-transit".into(),
                spotify_uri: "spotify:track:7KXjTSCq5nL1LoYtL7XAwS".into(),
                title: "Midnight Transit".into(),
                artist: "Kairo".into(),
                album: "Afterglow".into(),
                duration_secs: 3 * 60 + 49,
                liked: true,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::EmeraldRing,
            },
            Track {
                id: "higher-ground".into(),
                spotify_uri: "spotify:track:4uLU6hMCjMI75M1A2tKUQC".into(),
                title: "Higher Ground".into(),
                artist: "Lume".into(),
                album: "Solstice".into(),
                duration_secs: 4 * 60 + 28,
                liked: true,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::SunriseMountainLake,
            },
            Track {
                id: "ultraviolet-drift".into(),
                spotify_uri: "spotify:track:2plbrEY59IikOBgBGLjaoe".into(),
                title: "Ultraviolet Drift".into(),
                artist: "Nyx & Orion".into(),
                album: "Parallax".into(),
                duration_secs: 3 * 60 + 33,
                liked: true,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::VioletAura,
            },
            Track {
                id: "golden-hours".into(),
                spotify_uri: "spotify:track:5FVd6KXrgO9B3JPmC8OPst".into(),
                title: "Golden Hours".into(),
                artist: "Kairo".into(),
                album: "Afterglow".into(),
                duration_secs: 3 * 60 + 16,
                liked: true,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::GoldenCanyon,
            },
            Track {
                id: "saltwater-echo".into(),
                spotify_uri: "spotify:track:6UelLqGlWMcVH1E5c4H7lY".into(),
                title: "Saltwater Echo".into(),
                artist: "Caspian".into(),
                album: "Far Shores".into(),
                duration_secs: 4 * 60 + 42,
                liked: true,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::CoastalIsland,
            },
        ],

        "discover-weekly"
        | "discover weekly"
        | "spotify:playlist:37i9dQZEVXcJZyENOWUFo7"
        | "37i9dqzevxjzyenowufo7" => vec![
            Track {
                id: "aurora-protocol".into(),
                spotify_uri: "spotify:track:4cOdK2wGLETKBW3PvgPWqT".into(),
                title: "Aurora Protocol".into(),
                artist: "Celeste Vane".into(),
                album: "Telemetry".into(),
                duration_secs: 4 * 60 + 14,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::VioletAura,
            },
            Track {
                id: "prism-ferry".into(),
                spotify_uri: "spotify:track:0pqnGHJpmpxLKifKRmU6WP".into(),
                title: "Prism Ferry".into(),
                artist: "Opaline".into(),
                album: "Glass Harbour".into(),
                duration_secs: 3 * 60 + 52,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::MoonlitClouds,
            },
            Track {
                id: "cloudbreak-ridge".into(),
                spotify_uri: "spotify:track:3ZCTVFBt2Brf31RLEnCkWJ".into(),
                title: "Cloudbreak Ridge".into(),
                artist: "Fjordline".into(),
                album: "High Altitudes".into(),
                duration_secs: 4 * 60 + 36,
                liked: true,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::AlpineMist,
            },
            Track {
                id: "saffron-horizon".into(),
                spotify_uri: "spotify:track:1dGr1c8CrMLDpV6mPbImSI".into(),
                title: "Saffron Horizon".into(),
                artist: "Aurelia".into(),
                album: "Mirage".into(),
                duration_secs: 3 * 60 + 27,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::DawnReflection,
            },
            Track {
                id: "ghost-frequencies".into(),
                spotify_uri: "spotify:track:5QO79kh1waicV47BqGRL3g".into(),
                title: "Ghost Frequencies".into(),
                artist: "Mireille".into(),
                album: "Night Signals".into(),
                duration_secs: 4 * 60 + 51,
                liked: false,
                cover_art_url: None,
                artwork_theme: ArtworkTheme::TwilightHorizon,
            },
        ],

        _ => PlaybackState::mockup_default().queue,
    }
}

/// Returns all unique curated tracks across every built-in Groove playlist.
pub fn all_curated_tracks() -> Vec<Track> {
    let playlist_ids = [
        "chill-focus",
        "morning-light",
        "deep-work",
        "indie-favourites",
        "on-repeat",
        "discover-weekly",
    ];

    let mut seen_ids = std::collections::HashSet::new();
    let mut all = Vec::new();
    for pid in playlist_ids {
        for track in curated_tracks_for_playlist(pid) {
            if seen_ids.insert(track.id.clone()) {
                all.push(track);
            }
        }
    }
    all
}

/// Performs instant case-insensitive local search across all curated tracks by title, artist, or album.
pub fn search_curated_tracks(query: &str) -> Vec<Track> {
    let trimmed = query.trim().to_ascii_lowercase();
    let catalog = all_curated_tracks();
    if trimmed.is_empty() {
        return catalog;
    }

    catalog
        .into_iter()
        .filter(|track| {
            track.title.to_ascii_lowercase().contains(&trimmed)
                || track.artist.to_ascii_lowercase().contains(&trimmed)
                || track.album.to_ascii_lowercase().contains(&trimmed)
        })
        .collect()
}
