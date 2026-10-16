use std::time::Duration;

use groove_core::{
    AlbumDetail, AlbumSummary, ArtistDetail, ArtistSummary, ArtworkTheme, PlaybackState, Playlist,
    Track,
};
use librespot_core::{Session, SpotifyUri};
use librespot_metadata::{
    Album as LibrespotAlbum, Artist as LibrespotArtist, Metadata, Track as LibrespotTrack,
};
use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;
use tracing::{info, warn};

const SPOTIFY_API_BASE: &str = "https://api.spotify.com/v1";

/// Combined catalog search results for artists, albums, and tracks.
#[derive(Debug, Clone, Default)]
pub struct CatalogSearchResults {
    pub artists: Vec<ArtistSummary>,
    pub albums: Vec<AlbumSummary>,
    pub tracks: Vec<Track>,
}

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
    artists: Option<ApiArtistsPage>,
    albums: Option<ApiAlbumsPage>,
    tracks: Option<ApiTracksPage>,
}

#[derive(Debug, Deserialize)]
struct ApiArtistsPage {
    #[serde(default)]
    items: Vec<ApiArtistFull>,
}

#[derive(Debug, Deserialize)]
struct ApiAlbumsPage {
    #[serde(default)]
    items: Vec<ApiAlbumFull>,
}

#[derive(Debug, Deserialize)]
struct ApiTracksPage {
    #[serde(default)]
    items: Vec<ApiTrackItem>,
}

#[derive(Debug, Deserialize)]
struct ApiArtistFull {
    id: String,
    uri: Option<String>,
    name: String,
    #[serde(default)]
    genres: Vec<String>,
    followers: Option<ApiFollowers>,
    #[serde(default)]
    images: Vec<ApiImage>,
}

#[derive(Debug, Deserialize)]
struct ApiFollowers {
    total: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ApiAlbumFull {
    id: String,
    uri: Option<String>,
    name: String,
    album_type: Option<String>,
    release_date: Option<String>,
    total_tracks: Option<usize>,
    #[serde(default)]
    artists: Vec<ApiArtist>,
    #[serde(default)]
    images: Vec<ApiImage>,
    tracks: Option<ApiAlbumTracksPage>,
}

#[derive(Debug, Deserialize)]
struct ApiAlbumTracksPage {
    #[serde(default)]
    items: Vec<ApiTrackItem>,
}

#[derive(Debug, Deserialize)]
struct ApiTopTracksResponse {
    #[serde(default)]
    tracks: Vec<ApiTrackItem>,
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

/// Async Spotify catalog client using `librespot`'s native `spclient` protocol first (zero Web API
/// rate limits), with optional Spotify Web API support and curated local catalog fallback.
#[derive(Clone)]
pub struct SpotifyWebClient {
    http: Client,
    access_token: Option<String>,
    rate_limited_until: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
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

        Self {
            http,
            access_token,
            rate_limited_until: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn set_access_token(&mut self, access_token: Option<String>) {
        self.access_token = access_token.filter(|t| !t.trim().is_empty());
    }

    pub fn access_token(&self) -> Option<&str> {
        self.access_token.as_deref()
    }

    fn is_web_api_rate_limited(&self) -> bool {
        if let Ok(guard) = self.rate_limited_until.lock() {
            if let Some(until) = *guard {
                if std::time::Instant::now() < until {
                    return true;
                }
            }
        }
        false
    }

    fn note_web_api_rate_limit(&self, retry_after_secs: u64) {
        let secs = retry_after_secs.clamp(15, 300);
        if let Ok(mut guard) = self.rate_limited_until.lock() {
            *guard = Some(std::time::Instant::now() + Duration::from_secs(secs));
        }
        info!(
            "[WebAPI] Rate-limited (429); backing off public Web API for {secs}s and using native spclient"
        );
    }

    fn available_web_token(&self) -> Option<&str> {
        if self.is_web_api_rate_limited() {
            None
        } else {
            self.access_token.as_deref()
        }
    }

    /// Fetches the current user's Spotify profile (`GET /v1/me`).
    pub async fn fetch_user_profile(&self) -> Result<SpotifyUserProfile, WebApiError> {
        let token = self
            .available_web_token()
            .ok_or(WebApiError::MissingToken)?;

        let url = format!("{SPOTIFY_API_BASE}/me");
        let resp = self.http.get(url).bearer_auth(token).send().await?;
        let status = resp.status();
        if status.as_u16() == 429 {
            let retry = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(60);
            self.note_web_api_rate_limit(retry);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(WebApiError::ApiStatus {
                status: status.as_u16(),
                body,
            });
        }

        Ok(resp.json::<SpotifyUserProfile>().await?)
    }

    /// Fetches the user's playlists via `spclient` rootlist or `/v1/me/playlists`, or returns
    /// the curated Groove playlists when offline.
    pub async fn fetch_user_playlists_with_session(
        &self,
        session: Option<&Session>,
    ) -> Vec<Playlist> {
        if let Some(sess) = session {
            if let Some(playlists) = fetch_playlists_via_spclient(sess).await {
                if !playlists.is_empty() {
                    info!(
                        "[spclient] Loaded {} user playlists from Spotify rootlist",
                        playlists.len()
                    );
                    return playlists;
                }
            }
            info!("[spclient] User rootlist has no custom playlists yet; using Groove curated playlists");
            return PlaybackState::mockup_default().playlists;
        }
        self.fetch_user_playlists().await
    }

    /// Fetches the user's playlists from `/v1/me/playlists`, or returns the curated Groove playlists
    /// when offline or without an OAuth bearer token.
    pub async fn fetch_user_playlists(&self) -> Vec<Playlist> {
        if let Some(token) = self.available_web_token() {
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
                    if resp.status().as_u16() == 429 {
                        let retry = resp
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(60);
                        self.note_web_api_rate_limit(retry);
                    } else {
                        warn!(
                            "Spotify /v1/me/playlists returned {}; using curated playlists",
                            resp.status()
                        );
                    }
                }
                Err(err) => {
                    warn!("Spotify /v1/me/playlists failed ({err}); using curated playlists");
                }
            }
        }

        PlaybackState::mockup_default().playlists
    }

    /// Fetches tracks for a given playlist ID using `spclient` first when a live session is available.
    pub async fn fetch_playlist_tracks_with_session(
        &self,
        session: Option<&Session>,
        playlist_id: &str,
    ) -> Vec<Track> {
        if let Some(sess) = session {
            let candidate_uri = if playlist_id.starts_with("spotify:playlist:") {
                Some(playlist_id.to_string())
            } else if playlist_id.len() == 22 && playlist_id.chars().all(|c| c.is_ascii_alphanumeric()) {
                Some(format!("spotify:playlist:{playlist_id}"))
            } else {
                None
            };
            if let Some(uri_str) = candidate_uri {
                if let Ok(pl_uri) = SpotifyUri::from_uri(&uri_str) {
                    if let Some(tracks) = fetch_playlist_tracks_via_spclient(sess, &pl_uri).await {
                        if !tracks.is_empty() {
                            return tracks;
                        }
                    }
                }
            }
        }
        self.fetch_playlist_tracks(playlist_id).await
    }

    /// Fetches tracks for a given playlist ID. When a live bearer token is present and the playlist
    /// is not one of the built-in curated slugs (or if live fetch succeeds), returns the live tracks;
    /// otherwise returns the rich curated track list for that playlist.
    pub async fn fetch_playlist_tracks(&self, playlist_id: &str) -> Vec<Track> {
        let api_playlist_id = playlist_id
            .strip_prefix("spotify:playlist:")
            .unwrap_or(playlist_id);
        let is_base62_id = api_playlist_id.len() == 22
            && api_playlist_id.chars().all(|c| c.is_ascii_alphanumeric());

        if is_base62_id {
            if let Some(token) = self.available_web_token() {
                let url = format!("{SPOTIFY_API_BASE}/playlists/{api_playlist_id}/tracks?limit=50");
                if let Ok(resp) = self.http.get(url).bearer_auth(token).send().await {
                    if resp.status().as_u16() == 429 {
                        let retry = resp
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(60);
                        self.note_web_api_rate_limit(retry);
                    } else if resp.status().is_success() {
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
        }

        curated_tracks_for_playlist(playlist_id)
    }

    /// Searches artists, albums, and tracks using `session.spclient()` first (zero Web API rate limits!),
    /// then optional Web API `/v1/search`, falling back to curated catalog search.
    pub async fn search_catalog_with_session(
        &self,
        session: Option<&Session>,
        query: &str,
    ) -> CatalogSearchResults {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return CatalogSearchResults::default();
        }

        if let Some(sess) = session {
            if let Some(results) = search_via_spclient(sess, trimmed).await {
                return results;
            }
        }

        self.search_catalog(trimmed).await
    }

    /// Searches artists, albums, and tracks via `/v1/search?type=artist,album,track&q=...`
    /// when a bearer token is available.
    pub async fn search_catalog(&self, query: &str) -> CatalogSearchResults {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return CatalogSearchResults::default();
        }

        if let Some(token) = self.available_web_token() {
            let req = self
                .http
                .get(format!("{SPOTIFY_API_BASE}/search"))
                .bearer_auth(token)
                .query(&[
                    ("type", "artist,album,track"),
                    ("limit", "10"),
                    ("q", trimmed),
                ]);

            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(parsed) = resp.json::<SearchResponse>().await {
                        let artists: Vec<ArtistSummary> = parsed
                            .artists
                            .map(|p| {
                                p.items
                                    .into_iter()
                                    .enumerate()
                                    .map(|(idx, item)| map_api_artist(item, idx))
                                    .collect()
                            })
                            .unwrap_or_default();

                        let albums: Vec<AlbumSummary> = parsed
                            .albums
                            .map(|p| {
                                p.items
                                    .into_iter()
                                    .enumerate()
                                    .map(|(idx, item)| map_api_album(item, idx))
                                    .collect()
                            })
                            .unwrap_or_default();

                        let tracks: Vec<Track> = parsed
                            .tracks
                            .map(|p| {
                                p.items
                                    .into_iter()
                                    .enumerate()
                                    .map(|(idx, item)| map_api_track(item, idx))
                                    .collect()
                            })
                            .unwrap_or_default();

                        if !artists.is_empty() || !albums.is_empty() || !tracks.is_empty() {
                            return CatalogSearchResults {
                                artists,
                                albums,
                                tracks,
                            };
                        }
                    }
                }
                Ok(resp) => {
                    if resp.status().as_u16() == 429 {
                        let retry = resp
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(60);
                        self.note_web_api_rate_limit(retry);
                    } else {
                        warn!(
                            "Spotify Web API search returned status {}; falling back to local catalog search",
                            resp.status()
                        );
                    }
                }
                Err(err) => {
                    warn!(
                        "Spotify Web API search error ({err}); falling back to local catalog search"
                    );
                }
            }
        }

        CatalogSearchResults {
            artists: search_curated_artists(trimmed),
            albums: search_curated_albums(trimmed),
            tracks: search_curated_tracks(trimmed),
        }
    }

    /// Searches tracks via `/v1/search?type=track&q=...` when an OAuth bearer token is available,
    /// falling back to instant local search across all curated playlist tracks.
    pub async fn search_tracks(&self, query: &str) -> Vec<Track> {
        self.search_catalog(query).await.tracks
    }

    /// Fetches an artist's full profile, top tracks, and albums (with cover artwork URLs)
    /// using `librespot_metadata` (`spclient`) first, then Spotify Web API (`/v1/artists`),
    /// falling back to the curated artist database when offline.
    pub async fn fetch_artist_detail(
        &self,
        session: Option<&Session>,
        artist_id_or_uri: &str,
    ) -> ArtistDetail {
        let trimmed = artist_id_or_uri.trim();
        info!(
            "[ArtistDetail] Fetching artist detail for '{}' (has_live_session={})",
            trimmed,
            session.is_some()
        );

        // Check if this is a human-readable artist name (e.g. clicking "Lume" in Now Playing)
        // and resolve it to a Spotify Artist URI via spclient/catalog search if not a curated artist.
        let mut resolved_uri = normalize_artist_uri(trimmed);
        if resolved_uri.is_none() && (session.is_some() || self.available_web_token().is_some()) {
            let clean_query = trimmed
                .strip_prefix("spotify:artist:")
                .unwrap_or(trimmed)
                .strip_prefix("artist-")
                .unwrap_or(trimmed)
                .replace('-', " ");
            let search = self
                .search_catalog_with_session(session, &clean_query)
                .await;
            if let Some(first) = search.artists.first() {
                resolved_uri = normalize_artist_uri(&first.spotify_uri)
                    .or_else(|| normalize_artist_uri(&first.id));
                info!(
                    "[ArtistDetail] Resolved artist name '{}' (query='{}') -> URI {:?}",
                    trimmed, clean_query, resolved_uri
                );
            }
        }

        // 1. Try native `librespot_metadata::Artist` + `Album` over `session.spclient()` first!
        if let (Some(sess), Some(uri_str)) = (session, resolved_uri.as_deref()) {
            if let Ok(artist_uri) = SpotifyUri::from_uri(uri_str) {
                if let Some(detail) = fetch_artist_via_spclient(sess, &artist_uri).await {
                    return detail;
                }
            }
        }

        // 2. Try Spotify Web API `/v1/artists/{id}`, `/v1/artists/{id}/albums`, `/v1/artists/{id}/top-tracks`.
        if let (Some(token), Some(uri_str)) =
            (self.available_web_token(), resolved_uri.as_deref())
        {
            let raw_id = uri_str.strip_prefix("spotify:artist:").unwrap_or(uri_str);
            let artist_url = format!("{SPOTIFY_API_BASE}/artists/{raw_id}");
            let albums_url = format!(
                "{SPOTIFY_API_BASE}/artists/{raw_id}/albums?include_groups=album,single&limit=12"
            );
            let top_url = format!("{SPOTIFY_API_BASE}/artists/{raw_id}/top-tracks?market=from_token");

            let (artist_res, albums_res, top_res) = tokio::join!(
                self.http.get(&artist_url).bearer_auth(token).send(),
                self.http.get(&albums_url).bearer_auth(token).send(),
                self.http.get(&top_url).bearer_auth(token).send(),
            );

            if let Ok(resp) = artist_res {
                if resp.status().as_u16() == 429 {
                    self.note_web_api_rate_limit(60);
                } else if resp.status().is_success() {
                    if let Ok(api_artist) = resp.json::<ApiArtistFull>().await {
                        let artist_summary = map_api_artist(api_artist, 0);
                        let albums = match albums_res {
                            Ok(r) if r.status().is_success() => r
                                .json::<ApiAlbumsPage>()
                                .await
                                .map(|page| {
                                    page.items
                                        .into_iter()
                                        .enumerate()
                                        .map(|(idx, a)| map_api_album(a, idx))
                                        .collect()
                                })
                                .unwrap_or_default(),
                            _ => Vec::new(),
                        };
                        let top_tracks = match top_res {
                            Ok(r) if r.status().is_success() => r
                                .json::<ApiTopTracksResponse>()
                                .await
                                .map(|page| {
                                    page.tracks
                                        .into_iter()
                                        .enumerate()
                                        .map(|(idx, t)| map_api_track(t, idx))
                                        .collect()
                                })
                                .unwrap_or_default(),
                            _ => Vec::new(),
                        };

                        return ArtistDetail {
                            artist: artist_summary,
                            biography: None,
                            top_tracks,
                            albums,
                        };
                    }
                }
            }
        }

        // 3. Fallback to curated artist profile & discography.
        info!(
            "[ArtistDetail] Using curated artist profile for '{}'",
            trimmed
        );
        curated_artist_detail(trimmed)
    }

    /// Fetches full album detail (metadata, parent artist URI, cover art, and tracklist)
    /// using `librespot_metadata::Album` (`spclient`) or Spotify Web API `/v1/albums/{id}`.
    pub async fn fetch_album_detail(
        &self,
        session: Option<&Session>,
        album_id_or_uri: &str,
    ) -> AlbumDetail {
        let trimmed = album_id_or_uri.trim();
        info!(
            "[AlbumDetail] Fetching album detail for '{}' (has_live_session={})",
            trimmed,
            session.is_some()
        );

        let mut resolved_uri = normalize_album_uri(trimmed);
        if resolved_uri.is_none() && (session.is_some() || self.available_web_token().is_some()) {
            let clean_query = trimmed
                .strip_prefix("spotify:album:")
                .unwrap_or(trimmed)
                .replace('-', " ");
            let search = self
                .search_catalog_with_session(session, &clean_query)
                .await;
            if let Some(first) = search.albums.first() {
                resolved_uri = normalize_album_uri(&first.spotify_uri)
                    .or_else(|| normalize_album_uri(&first.id));
                info!(
                    "[AlbumDetail] Resolved album name '{}' (query='{}') -> URI {:?}",
                    trimmed, clean_query, resolved_uri
                );
            }
        }

        // 1. Try native `librespot_metadata::Album::get` over `session.spclient()`.
        if let (Some(sess), Some(uri_str)) = (session, resolved_uri.as_deref()) {
            if let Ok(album_uri) = SpotifyUri::from_uri(uri_str) {
                let t0 = std::time::Instant::now();
                if let Ok(meta_album) = LibrespotAlbum::get(sess, &album_uri).await {
                    let cover_url = extract_librespot_image_url(&meta_album.covers)
                        .or_else(|| extract_librespot_image_url(&meta_album.cover_group));
                    let album_title = meta_album.name.clone();
                    let default_artist = meta_album
                        .artists
                        .first()
                        .map(|a| a.name.clone())
                        .unwrap_or_else(|| "Artist".to_string());
                    let artist_uri = meta_album
                        .artists
                        .first()
                        .and_then(|a| a.id.to_uri().ok());
                    let release_year = meta_album.date.year().to_string();
                    let album_type = match meta_album.album_type {
                        librespot_metadata::album::AlbumType::SINGLE => "Single".to_string(),
                        librespot_metadata::album::AlbumType::COMPILATION => {
                            "Compilation".to_string()
                        }
                        librespot_metadata::album::AlbumType::EP => "EP".to_string(),
                        _ => "Album".to_string(),
                    };

                    let track_uris: Vec<SpotifyUri> =
                        meta_album.tracks().take(24).cloned().collect();
                    let mut set = tokio::task::JoinSet::new();
                    for (idx, t_uri) in track_uris.into_iter().enumerate() {
                        let s = sess.clone();
                        set.spawn(async move {
                            LibrespotTrack::get(&s, &t_uri).await.ok().map(|t| (idx, t))
                        });
                    }
                    let mut indexed_tracks = Vec::new();
                    while let Some(Ok(Some((idx, t)))) = set.join_next().await {
                        indexed_tracks.push((idx, t));
                    }
                    indexed_tracks.sort_by_key(|(idx, _)| *idx);

                    let tracks: Vec<Track> = indexed_tracks
                        .into_iter()
                        .map(|(idx, t)| {
                            map_librespot_track(
                                &t,
                                idx,
                                Some(&album_title),
                                Some(&default_artist),
                                cover_url.clone(),
                            )
                        })
                        .collect();

                    if !tracks.is_empty() {
                        let alb_id = uri_str
                            .strip_prefix("spotify:album:")
                            .unwrap_or(uri_str)
                            .to_string();
                        info!(
                            "[spclient] Loaded album detail '{}' by '{}' ({} tracks, artist_uri={:?}) in {:?}",
                            album_title,
                            default_artist,
                            tracks.len(),
                            artist_uri,
                            t0.elapsed()
                        );
                        return AlbumDetail {
                            album: AlbumSummary {
                                id: alb_id,
                                spotify_uri: uri_str.to_string(),
                                title: album_title,
                                artist_name: default_artist,
                                release_year,
                                album_type,
                                track_count: tracks.len(),
                                cover_art_url: cover_url,
                                artwork_theme: ArtworkTheme::SunriseMountainLake,
                            },
                            artist_uri,
                            tracks,
                        };
                    }
                }
            }
        }

        // 2. Try Spotify Web API `GET /v1/albums/{id}`.
        if let (Some(token), Some(uri_str)) =
            (self.available_web_token(), resolved_uri.as_deref())
        {
            let raw_id = uri_str.strip_prefix("spotify:album:").unwrap_or(uri_str);
            let url = format!("{SPOTIFY_API_BASE}/albums/{raw_id}");
            if let Ok(resp) = self.http.get(url).bearer_auth(token).send().await {
                if resp.status().as_u16() == 429 {
                    self.note_web_api_rate_limit(60);
                } else if resp.status().is_success() {
                    if let Ok(alb) = resp.json::<ApiAlbumFull>().await {
                        let cover = pick_best_image_url(&alb.images);
                        let album_name = alb.name.clone();
                        let fallback_artist = alb
                            .artists
                            .iter()
                            .map(|a| a.name.clone())
                            .collect::<Vec<_>>()
                            .join(", ");
                        if let Some(page) = alb.tracks {
                            let mapped: Vec<Track> = page
                                .items
                                .into_iter()
                                .enumerate()
                                .map(|(idx, mut item)| {
                                    if item.artists.is_empty() && !fallback_artist.is_empty() {
                                        item.artists.push(ApiArtist {
                                            name: fallback_artist.clone(),
                                        });
                                    }
                                    let mut track = map_api_track(item, idx);
                                    track.album = album_name.clone();
                                    if track.cover_art_url.is_none() {
                                        track.cover_art_url = cover.clone();
                                    }
                                    track
                                })
                                .collect();
                            if !mapped.is_empty() {
                                return AlbumDetail {
                                    album: AlbumSummary {
                                        id: raw_id.to_string(),
                                        spotify_uri: uri_str.to_string(),
                                        title: album_name,
                                        artist_name: if fallback_artist.is_empty() {
                                            "Artist".to_string()
                                        } else {
                                            fallback_artist
                                        },
                                        release_year: alb
                                            .release_date
                                            .unwrap_or_else(|| "2024".to_string())
                                            .chars()
                                            .take(4)
                                            .collect(),
                                        album_type: alb
                                            .album_type
                                            .unwrap_or_else(|| "Album".to_string()),
                                        track_count: mapped.len(),
                                        cover_art_url: cover,
                                        artwork_theme: ArtworkTheme::SunriseMountainLake,
                                    },
                                    artist_uri: None,
                                    tracks: mapped,
                                };
                            }
                        }
                    }
                }
            }
        }

        // 3. Fallback to curated album tracks.
        fallback_album_detail(trimmed, resolved_uri)
    }

    /// Fetches all playable tracks for an album (and attaches its cover artwork URL)
    /// using `librespot_metadata::Album` or Spotify Web API `/v1/albums/{id}`.
    pub async fn fetch_album_tracks(
        &self,
        session: Option<&Session>,
        album_id_or_uri: &str,
    ) -> Vec<Track> {
        self.fetch_album_detail(session, album_id_or_uri)
            .await
            .tracks
    }
}

pub fn fallback_album_detail(trimmed: &str, resolved_uri: Option<String>) -> AlbumDetail {
    let tracks = curated_tracks_for_album(trimmed);
    let first = tracks.first();
    let title = first
        .map(|t| t.album.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| trimmed.to_string());
    let artist_name = first
        .map(|t| t.artist.clone())
        .unwrap_or_else(|| "Lume".to_string());
    let cover_art_url = first.and_then(|t| t.cover_art_url.clone());
    let artwork_theme = first
        .map(|t| t.artwork_theme)
        .unwrap_or(ArtworkTheme::SunriseMountainLake);

    AlbumDetail {
        album: AlbumSummary {
            id: trimmed.to_string(),
            spotify_uri: resolved_uri.unwrap_or_else(|| format!("spotify:album:{trimmed}")),
            title,
            artist_name: artist_name.clone(),
            release_year: "2024".to_string(),
            album_type: "Album".to_string(),
            track_count: tracks.len(),
            cover_art_url,
            artwork_theme,
        },
        artist_uri: Some(artist_name),
        tracks,
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

/// Fallback track list when offline or before a live Spotify session is connected.
pub fn curated_tracks_for_playlist(_playlist_key: &str) -> Vec<Track> {
    Vec::new()
}

pub fn all_curated_tracks() -> Vec<Track> {
    Vec::new()
}

pub fn search_curated_tracks(_query: &str) -> Vec<Track> {
    Vec::new()
}

fn pick_best_image_url(images: &[ApiImage]) -> Option<String> {
    images
        .first()
        .map(|img| img.url.clone())
        .filter(|u| !u.trim().is_empty())
}

fn extract_librespot_image_url(images: &librespot_metadata::image::Images) -> Option<String> {
    // Prefer XLARGE or LARGE high-res image first, then largest pixel area, then DEFAULT/first
    let chosen = images
        .iter()
        .find(|img| matches!(img.size, librespot_metadata::image::ImageSize::XLARGE))
        .or_else(|| {
            images
                .iter()
                .find(|img| matches!(img.size, librespot_metadata::image::ImageSize::LARGE))
        })
        .or_else(|| {
            images
                .iter()
                .filter(|img| img.width > 0 && img.height > 0)
                .max_by_key(|img| img.width.saturating_mul(img.height))
        })
        .or_else(|| {
            images
                .iter()
                .find(|img| matches!(img.size, librespot_metadata::image::ImageSize::DEFAULT))
        })
        .or_else(|| images.first())?;
    let hex = chosen.id.to_base16().ok()?;
    if hex.is_empty() {
        None
    } else {
        Some(format!("https://i.scdn.co/image/{hex}"))
    }
}

/// Strips inline HTML tags (such as `<a href="spotify:artist:...">...</a>`) and decodes HTML
/// entities from Spotify artist biographies while preserving clean paragraph breaks.
pub fn sanitize_spotify_biography(raw: &str) -> String {
    let mut stripped = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '<' {
            let mut tag = String::new();
            for tc in chars.by_ref() {
                if tc == '>' {
                    break;
                }
                tag.push(tc);
            }
            let tag_lower = tag.trim().to_ascii_lowercase();
            if tag_lower.starts_with("br")
                || tag_lower == "/p"
                || tag_lower.starts_with("p ")
                || tag_lower == "p"
            {
                stripped.push('\n');
            }
        } else {
            stripped.push(ch);
        }
    }

    let decoded = stripped
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&ndash;", "–")
        .replace("&mdash;", "—")
        .replace("&hellip;", "…")
        .replace("&#8211;", "–")
        .replace("&#8212;", "—")
        .replace("&#8217;", "’")
        .replace("&#8220;", "“")
        .replace("&#8221;", "”");

    let mut paragraphs = Vec::new();
    for line in decoded.lines() {
        let collapsed: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !collapsed.is_empty() {
            paragraphs.push(collapsed);
        }
    }
    paragraphs.join("\n\n")
}

fn normalize_artist_uri(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if let Some(id) = trimmed.strip_prefix("spotify:artist:") {
        if id.len() == 22 && id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Some(trimmed.to_string());
        }
    }
    if trimmed.len() == 22 && trimmed.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Some(format!("spotify:artist:{trimmed}"));
    }
    None
}

fn normalize_album_uri(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if let Some(id) = trimmed.strip_prefix("spotify:album:") {
        if id.len() == 22 && id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Some(trimmed.to_string());
        }
    }
    if trimmed.len() == 22 && trimmed.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Some(format!("spotify:album:{trimmed}"));
    }
    None
}

fn encode_spotify_search_query(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 8);
    let mut prev_space = false;
    for ch in query.trim().chars() {
        if ch.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push('+');
                prev_space = true;
            }
        } else {
            prev_space = false;
            match ch {
                '%' => out.push_str("%25"),
                '?' => out.push_str("%3F"),
                '#' => out.push_str("%23"),
                '&' => out.push_str("%26"),
                '/' => out.push_str("%2F"),
                '+' => out.push_str("%2B"),
                _ => out.push(ch),
            }
        }
    }
    out
}

async fn search_via_spclient(session: &Session, query: &str) -> Option<CatalogSearchResults> {
    let t0 = std::time::Instant::now();
    let encoded = encode_spotify_search_query(query);
    if encoded.is_empty() {
        return None;
    }

    let search_uri = format!("spotify:search:{encoded}");
    info!("[spclient] Resolving search context '{search_uri}'...");

    let ctx = match session.spclient().get_context(&search_uri).await {
        Ok(ctx) => ctx,
        Err(err) => {
            warn!("[spclient] get_context('{search_uri}') failed: {err}");
            return None;
        }
    };

    let mut track_uris: Vec<SpotifyUri> = Vec::new();
    for page in &ctx.pages {
        for track in &page.tracks {
            if let Some(uri_str) = track.uri.as_deref() {
                if let Ok(uri) = SpotifyUri::from_uri(uri_str) {
                    if !track_uris.contains(&uri) {
                        track_uris.push(uri);
                        if track_uris.len() >= 10 {
                            break;
                        }
                    }
                }
            }
        }
        if track_uris.len() >= 10 {
            break;
        }
    }

    if track_uris.is_empty() {
        warn!("[spclient] Search '{query}' returned 0 track URIs in context");
        return None;
    }

    // Fetch all matched tracks in parallel over spclient HTTP/2
    let mut track_set = tokio::task::JoinSet::new();
    for (idx, t_uri) in track_uris.into_iter().enumerate() {
        let sess = session.clone();
        track_set.spawn(async move {
            LibrespotTrack::get(&sess, &t_uri)
                .await
                .ok()
                .map(|t| (idx, t))
        });
    }

    let mut indexed_tracks = Vec::new();
    while let Some(Ok(Some((idx, t)))) = track_set.join_next().await {
        indexed_tracks.push((idx, t));
    }
    indexed_tracks.sort_by_key(|(idx, _)| *idx);

    if indexed_tracks.is_empty() {
        return None;
    }

    let query_lower = query.trim().to_ascii_lowercase();
    let mut unique_artist_uris: Vec<(SpotifyUri, String, bool)> = Vec::new();
    let mut albums: Vec<AlbumSummary> = Vec::new();
    let mut tracks: Vec<Track> = Vec::with_capacity(indexed_tracks.len());

    for (idx, meta_track) in &indexed_tracks {
        let cover_url = extract_librespot_image_url(&meta_track.album.covers)
            .or_else(|| extract_librespot_image_url(&meta_track.album.cover_group));

        // Collect unique artists from track
        for art in meta_track.artists.iter() {
            if !unique_artist_uris
                .iter()
                .any(|(existing_uri, _, _)| existing_uri == &art.id)
            {
                let name_matches = art.name.to_ascii_lowercase().contains(&query_lower);
                unique_artist_uris.push((art.id.clone(), art.name.clone(), name_matches));
            }
        }

        // Collect unique albums directly from the track's embedded album metadata
        if let Ok(alb_uri_str) = meta_track.album.id.to_uri() {
            if !albums.iter().any(|a| a.spotify_uri == alb_uri_str) && albums.len() < 8 {
                let alb_id = alb_uri_str
                    .strip_prefix("spotify:album:")
                    .unwrap_or(&alb_uri_str)
                    .to_string();
                let artist_name = meta_track
                    .artists
                    .first()
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| "Artist".to_string());
                let release_year = meta_track.album.date.year().to_string();
                let album_type = match meta_track.album.album_type {
                    librespot_metadata::album::AlbumType::SINGLE => "Single".to_string(),
                    librespot_metadata::album::AlbumType::COMPILATION => "Compilation".to_string(),
                    librespot_metadata::album::AlbumType::EP => "EP".to_string(),
                    _ => "Album".to_string(),
                };
                albums.push(AlbumSummary {
                    id: alb_id,
                    spotify_uri: alb_uri_str,
                    title: meta_track.album.name.clone(),
                    artist_name,
                    release_year,
                    album_type,
                    track_count: 10,
                    cover_art_url: cover_url.clone(),
                    artwork_theme: track_theme_for_index(albums.len()),
                });
            }
        }

        tracks.push(map_librespot_track(
            meta_track,
            *idx,
            None,
            None,
            cover_url,
        ));
    }

    // Prioritize artists whose name matches the search query
    unique_artist_uris.sort_by_key(|(_, _, matches)| !*matches);
    unique_artist_uris.truncate(6);

    // Fetch artist portraits in parallel via LibrespotArtist::get
    let mut artist_set = tokio::task::JoinSet::new();
    for (idx, (art_uri, fallback_name, _)) in unique_artist_uris.into_iter().enumerate() {
        let sess = session.clone();
        artist_set.spawn(async move {
            let meta_res = LibrespotArtist::get(&sess, &art_uri).await.ok();
            (idx, art_uri, fallback_name, meta_res)
        });
    }

    let mut indexed_artists = Vec::new();
    while let Some(Ok((idx, art_uri, fallback_name, meta_opt))) = artist_set.join_next().await {
        let uri_str = art_uri
            .to_uri()
            .unwrap_or_else(|_| format!("spotify:artist:art-{idx}"));
        let id = uri_str
            .strip_prefix("spotify:artist:")
            .unwrap_or(&uri_str)
            .to_string();

        if let Some(meta_artist) = meta_opt {
            let portrait_url = extract_librespot_image_url(&meta_artist.portrait_group)
                .or_else(|| extract_librespot_image_url(&meta_artist.portraits));
            indexed_artists.push((
                idx,
                ArtistSummary {
                    id,
                    spotify_uri: uri_str,
                    name: meta_artist.name,
                    genres: Vec::new(),
                    followers: None,
                    portrait_url,
                    artwork_theme: track_theme_for_index(idx),
                },
            ));
        } else {
            indexed_artists.push((
                idx,
                ArtistSummary {
                    id,
                    spotify_uri: uri_str,
                    name: fallback_name,
                    genres: Vec::new(),
                    followers: None,
                    portrait_url: None,
                    artwork_theme: track_theme_for_index(idx),
                },
            ));
        }
    }
    indexed_artists.sort_by_key(|(idx, _)| *idx);
    let artists: Vec<ArtistSummary> = indexed_artists.into_iter().map(|(_, a)| a).collect();

    info!(
        "[spclient] Search '{}' completed in {:?} -> {} artists, {} albums, {} tracks",
        query,
        t0.elapsed(),
        artists.len(),
        albums.len(),
        tracks.len()
    );

    Some(CatalogSearchResults {
        artists,
        albums,
        tracks,
    })
}

async fn fetch_playlists_via_spclient(session: &Session) -> Option<Vec<Playlist>> {
    use librespot_metadata::Playlist as LibrespotPlaylist;
    use protobuf::Message;

    let t0 = std::time::Instant::now();
    let bytes = session.spclient().get_rootlist(0, Some(20)).await.ok()?;
    let msg = <LibrespotPlaylist as Metadata>::Message::parse_from_bytes(&bytes).ok()?;

    let mut playlist_uris: Vec<SpotifyUri> = msg
        .contents
        .items
        .iter()
        .filter_map(|item| SpotifyUri::from_uri(item.uri()).ok())
        .filter(|uri| matches!(uri, SpotifyUri::Playlist { .. }))
        .take(12)
        .collect();
    playlist_uris.dedup();
    info!(
        "[spclient] Rootlist returned {} raw items ({} playlist URIs) in {:?}",
        msg.contents.items.len(),
        playlist_uris.len(),
        t0.elapsed()
    );

    if playlist_uris.is_empty() {
        return None;
    }

    let mut set = tokio::task::JoinSet::new();
    for (idx, pl_uri) in playlist_uris.into_iter().enumerate() {
        let sess = session.clone();
        set.spawn(async move {
            LibrespotPlaylist::get(&sess, &pl_uri)
                .await
                .ok()
                .map(|pl| (idx, pl_uri, pl))
        });
    }

    let mut indexed = Vec::new();
    while let Some(Ok(Some((idx, pl_uri, pl)))) = set.join_next().await {
        let uri_str = pl_uri
            .to_uri()
            .unwrap_or_else(|_| format!("spotify:playlist:pl-{idx}"));
        let id = uri_str
            .strip_prefix("spotify:playlist:")
            .unwrap_or(&uri_str)
            .to_string();
        let subtitle = if pl.attributes.description.trim().is_empty() {
            "Spotify Playlist".to_string()
        } else {
            pl.attributes.description.trim().to_string()
        };
        let track_count = pl.length.max(0) as usize;
        indexed.push((
            idx,
            Playlist {
                id,
                spotify_uri: uri_str,
                name: pl.name().to_string(),
                subtitle,
                track_count,
                is_playing_context: idx == 0,
                artwork_theme: playlist_theme_for_index(idx),
            },
        ));
    }
    indexed.sort_by_key(|(idx, _)| *idx);
    let playlists: Vec<Playlist> = indexed.into_iter().map(|(_, p)| p).collect();
    info!(
        "[spclient] Fetched {} playlists from rootlist in {:?}",
        playlists.len(),
        t0.elapsed()
    );
    if playlists.is_empty() {
        None
    } else {
        Some(playlists)
    }
}

async fn fetch_playlist_tracks_via_spclient(
    session: &Session,
    playlist_uri: &SpotifyUri,
) -> Option<Vec<Track>> {
    use librespot_metadata::Playlist as LibrespotPlaylist;

    let t0 = std::time::Instant::now();
    let pl = LibrespotPlaylist::get(session, playlist_uri).await.ok()?;
    let track_uris: Vec<SpotifyUri> = pl
        .tracks()
        .filter(|u| matches!(u, SpotifyUri::Track { .. }))
        .take(30)
        .cloned()
        .collect();

    if track_uris.is_empty() {
        return None;
    }

    let mut set = tokio::task::JoinSet::new();
    for (idx, t_uri) in track_uris.into_iter().enumerate() {
        let sess = session.clone();
        set.spawn(async move {
            LibrespotTrack::get(&sess, &t_uri)
                .await
                .ok()
                .map(|t| (idx, t))
        });
    }

    let mut indexed = Vec::new();
    while let Some(Ok(Some((idx, t)))) = set.join_next().await {
        let cover = extract_librespot_image_url(&t.album.covers)
            .or_else(|| extract_librespot_image_url(&t.album.cover_group));
        indexed.push((idx, map_librespot_track(&t, idx, None, None, cover)));
    }
    indexed.sort_by_key(|(idx, _)| *idx);
    let tracks: Vec<Track> = indexed.into_iter().map(|(_, t)| t).collect();
    info!(
        "[spclient] Fetched {} tracks for playlist '{}' in {:?}",
        tracks.len(),
        pl.name(),
        t0.elapsed()
    );
    Some(tracks)
}

async fn fetch_artist_via_spclient(
    session: &Session,
    artist_uri: &SpotifyUri,
) -> Option<ArtistDetail> {
    let t0 = std::time::Instant::now();
    let meta_artist = match LibrespotArtist::get(session, artist_uri).await {
        Ok(a) => a,
        Err(err) => {
            warn!("[spclient] LibrespotArtist::get({artist_uri:?}) failed: {err}");
            return None;
        }
    };
    let uri_str = artist_uri.to_uri().ok()?;
    let id = uri_str
        .strip_prefix("spotify:artist:")
        .unwrap_or(&uri_str)
        .to_string();

    let portrait_url = extract_librespot_image_url(&meta_artist.portrait_group)
        .or_else(|| extract_librespot_image_url(&meta_artist.portraits))
        .or_else(|| {
            meta_artist.biographies.iter().find_map(|b| {
                extract_librespot_image_url(&b.portraits)
                    .or_else(|| b.portrait_group.iter().find_map(extract_librespot_image_url))
            })
        });

    let biography = meta_artist
        .biographies
        .first()
        .map(|b| sanitize_spotify_biography(&b.text))
        .filter(|s| !s.is_empty());

    let artist_name = meta_artist.name.clone();
    let artist_summary = ArtistSummary {
        id,
        spotify_uri: uri_str,
        name: artist_name.clone(),
        genres: Vec::new(),
        followers: None,
        portrait_url: portrait_url.clone(),
        artwork_theme: ArtworkTheme::SunriseMountainLake,
    };

    // Fetch up to 10 albums + singles concurrently via `LibrespotAlbum::get`
    let mut album_uris: Vec<SpotifyUri> = meta_artist
        .albums_current()
        .chain(meta_artist.singles_current())
        .take(10)
        .cloned()
        .collect();
    album_uris.dedup();

    let top_track_uris: Vec<SpotifyUri> = meta_artist
        .top_tracks
        .iter()
        .flat_map(|tt| tt.tracks.iter())
        .take(8)
        .cloned()
        .collect();

    let mut album_set = tokio::task::JoinSet::new();
    for (idx, alb_uri) in album_uris.into_iter().enumerate() {
        let sess = session.clone();
        album_set.spawn(async move {
            LibrespotAlbum::get(&sess, &alb_uri)
                .await
                .ok()
                .map(|alb| (idx, alb))
        });
    }

    let mut track_set = tokio::task::JoinSet::new();
    for (idx, t_uri) in top_track_uris.into_iter().enumerate() {
        let sess = session.clone();
        track_set.spawn(async move {
            LibrespotTrack::get(&sess, &t_uri)
                .await
                .ok()
                .map(|t| (idx, t))
        });
    }

    let mut indexed_albums = Vec::new();
    while let Some(Ok(Some((idx, alb)))) = album_set.join_next().await {
        let alb_uri_str = alb
            .id
            .to_uri()
            .unwrap_or_else(|_| format!("spotify:album:alb-{idx}"));
        let alb_id = alb_uri_str
            .strip_prefix("spotify:album:")
            .unwrap_or(&alb_uri_str)
            .to_string();
        let cover_art_url = extract_librespot_image_url(&alb.covers)
            .or_else(|| extract_librespot_image_url(&alb.cover_group));
        let release_year = alb.date.year().to_string();
        let album_type = match alb.album_type {
            librespot_metadata::album::AlbumType::SINGLE => "Single".to_string(),
            librespot_metadata::album::AlbumType::COMPILATION => "Compilation".to_string(),
            librespot_metadata::album::AlbumType::EP => "EP".to_string(),
            _ => "Album".to_string(),
        };
        let track_count = alb.tracks().count().max(1);
        indexed_albums.push((
            idx,
            AlbumSummary {
                id: alb_id,
                spotify_uri: alb_uri_str,
                title: alb.name,
                artist_name: artist_name.clone(),
                release_year,
                album_type,
                track_count,
                cover_art_url,
                artwork_theme: track_theme_for_index(idx),
            },
        ));
    }
    indexed_albums.sort_by_key(|(idx, _)| *idx);
    let albums: Vec<AlbumSummary> = indexed_albums.into_iter().map(|(_, a)| a).collect();

    let mut indexed_tracks = Vec::new();
    while let Some(Ok(Some((idx, t)))) = track_set.join_next().await {
        let cover = extract_librespot_image_url(&t.album.covers)
            .or_else(|| extract_librespot_image_url(&t.album.cover_group))
            .or_else(|| portrait_url.clone());
        indexed_tracks.push((
            idx,
            map_librespot_track(&t, idx, None, Some(&artist_name), cover),
        ));
    }
    indexed_tracks.sort_by_key(|(idx, _)| *idx);
    let top_tracks: Vec<Track> = indexed_tracks.into_iter().map(|(_, t)| t).collect();

    info!(
        "[spclient] Loaded artist '{}' in {:?}: {} albums, {} top tracks, portrait={:?}",
        artist_name,
        t0.elapsed(),
        albums.len(),
        top_tracks.len(),
        portrait_url
    );

    Some(ArtistDetail {
        artist: artist_summary,
        biography,
        top_tracks,
        albums,
    })
}

fn map_librespot_track(
    t: &LibrespotTrack,
    idx: usize,
    album_override: Option<&str>,
    artist_override: Option<&str>,
    cover_art_url: Option<String>,
) -> Track {
    let playable_uri = if t.files.is_empty() {
        t.alternatives.first().unwrap_or(&t.id)
    } else {
        &t.id
    };
    let spotify_uri = playable_uri
        .to_uri()
        .unwrap_or_else(|_| format!("spotify:track:spclient-{idx}"));
    let id = spotify_uri
        .strip_prefix("spotify:track:")
        .unwrap_or(&spotify_uri)
        .to_string();
    let artist = if !t.artists.is_empty() {
        t.artists
            .iter()
            .map(|a| a.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        artist_override.unwrap_or("Unknown Artist").to_string()
    };
    let album = album_override
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if t.album.name.is_empty() {
                "Single".to_string()
            } else {
                t.album.name.clone()
            }
        });
    let duration_secs = ((t.duration.max(1000) + 500) / 1000) as u32;

    Track {
        id,
        spotify_uri,
        title: t.name.clone(),
        artist,
        album,
        duration_secs,
        liked: false,
        cover_art_url,
        artwork_theme: track_theme_for_index(idx),
    }
}

fn map_api_artist(item: ApiArtistFull, idx: usize) -> ArtistSummary {
    let spotify_uri = item
        .uri
        .unwrap_or_else(|| format!("spotify:artist:{}", item.id));
    let portrait_url = pick_best_image_url(&item.images);
    let followers = item.followers.and_then(|f| f.total);

    ArtistSummary {
        id: item.id,
        spotify_uri,
        name: item.name,
        genres: item.genres,
        followers,
        portrait_url,
        artwork_theme: track_theme_for_index(idx),
    }
}

fn map_api_album(item: ApiAlbumFull, idx: usize) -> AlbumSummary {
    let spotify_uri = item
        .uri
        .unwrap_or_else(|| format!("spotify:album:{}", item.id));
    let artist_name = if item.artists.is_empty() {
        "Spotify Artist".to_string()
    } else {
        item.artists
            .into_iter()
            .map(|a| a.name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let release_year = item
        .release_date
        .as_deref()
        .and_then(|d| d.split('-').next())
        .filter(|y| !y.is_empty())
        .unwrap_or("2024")
        .to_string();
    let album_type = match item
        .album_type
        .as_deref()
        .unwrap_or("album")
        .to_ascii_lowercase()
        .as_str()
    {
        "single" => "Single".to_string(),
        "compilation" => "Compilation".to_string(),
        "ep" => "EP".to_string(),
        _ => "Album".to_string(),
    };
    let cover_art_url = pick_best_image_url(&item.images);

    AlbumSummary {
        id: item.id,
        spotify_uri,
        title: item.name,
        artist_name,
        release_year,
        album_type,
        track_count: item.total_tracks.unwrap_or(8).max(1),
        cover_art_url,
        artwork_theme: track_theme_for_index(idx),
    }
}

pub fn all_curated_artists() -> Vec<ArtistSummary> {
    Vec::new()
}

pub fn all_curated_albums() -> Vec<AlbumSummary> {
    Vec::new()
}

pub fn search_curated_artists(_query: &str) -> Vec<ArtistSummary> {
    Vec::new()
}

pub fn search_curated_albums(_query: &str) -> Vec<AlbumSummary> {
    Vec::new()
}

/// Returns an empty `ArtistDetail` placeholder when offline.
pub fn curated_artist_detail(artist_key: &str) -> ArtistDetail {
    let name = artist_key
        .trim()
        .strip_prefix("spotify:artist:")
        .unwrap_or(artist_key.trim())
        .to_string();
    ArtistDetail {
        artist: ArtistSummary {
            id: name.clone(),
            spotify_uri: format!("spotify:artist:{name}"),
            name,
            genres: Vec::new(),
            followers: None,
            portrait_url: None,
            artwork_theme: ArtworkTheme::SunriseMountainLake,
        },
        biography: None,
        top_tracks: Vec::new(),
        albums: Vec::new(),
    }
}

/// Returns empty tracks for a given album ID or URI when offline.
pub fn curated_tracks_for_album(_album_key: &str) -> Vec<Track> {
    Vec::new()
}
