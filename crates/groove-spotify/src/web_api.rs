use std::time::Duration;

use groove_core::{
    AlbumSummary, ArtistDetail, ArtistSummary, ArtworkTheme, PlaybackState, Playlist, Track,
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
            if let Some(sess) = session {
                info!("[Search] Empty query with live session -> fetching featured catalog via spclient");
                if let Some(featured) = search_via_spclient(sess, "Tycho Bonobo ODESZA").await {
                    return featured;
                }
            }
            info!("[Search] Empty query -> returning default featured artists/albums/tracks");
            return CatalogSearchResults {
                artists: all_curated_artists(),
                albums: all_curated_albums(),
                tracks: all_curated_tracks().into_iter().take(10).collect(),
            };
        }

        if let Some(sess) = session {
            if let Some(results) = search_via_spclient(sess, trimmed).await {
                return results;
            }
        }

        self.search_catalog(trimmed).await
    }

    /// Searches artists, albums, and tracks via `/v1/search?type=artist,album,track&q=...`
    /// when a bearer token is available, falling back to curated catalog search.
    pub async fn search_catalog(&self, query: &str) -> CatalogSearchResults {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return CatalogSearchResults {
                artists: all_curated_artists(),
                albums: all_curated_albums(),
                tracks: all_curated_tracks().into_iter().take(10).collect(),
            };
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

    /// Fetches all playable tracks for an album (and attaches its cover artwork URL)
    /// using `librespot_metadata::Album` or Spotify Web API `/v1/albums/{id}`.
    pub async fn fetch_album_tracks(
        &self,
        session: Option<&Session>,
        album_id_or_uri: &str,
    ) -> Vec<Track> {
        let trimmed = album_id_or_uri.trim();
        info!(
            "[AlbumTracks] Fetching tracks for album '{}' (has_live_session={})",
            trimmed,
            session.is_some()
        );

        // 1. Try native `librespot_metadata::Album::get` over `session.spclient()`.
        if let (Some(sess), Some(uri_str)) = (session, normalize_album_uri(trimmed)) {
            if let Ok(album_uri) = SpotifyUri::from_uri(&uri_str) {
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
                        info!(
                            "[spclient] Loaded {} tracks for album '{}' in {:?} (cover={:?})",
                            tracks.len(),
                            album_title,
                            t0.elapsed(),
                            cover_url
                        );
                        return tracks;
                    }
                }
            }
        }

        // 2. Try Spotify Web API `GET /v1/albums/{id}`.
        if let (Some(token), Some(uri_str)) =
            (self.available_web_token(), normalize_album_uri(trimmed))
        {
            let raw_id = uri_str.strip_prefix("spotify:album:").unwrap_or(&uri_str);
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
                                return mapped;
                            }
                        }
                    }
                }
            }
        }

        // 3. Fallback to curated album tracks.
        curated_tracks_for_album(trimmed)
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

fn pick_best_image_url(images: &[ApiImage]) -> Option<String> {
    images
        .first()
        .map(|img| img.url.clone())
        .filter(|u| !u.trim().is_empty())
}

fn extract_librespot_image_url(images: &librespot_metadata::image::Images) -> Option<String> {
    // Prefer LARGE or DEFAULT image if present, or fall back to first image
    let chosen = images
        .iter()
        .find(|img| {
            matches!(
                img.size,
                librespot_metadata::image::ImageSize::LARGE
                    | librespot_metadata::image::ImageSize::DEFAULT
            )
        })
        .or_else(|| images.first())?;
    let hex = chosen.id.to_base16().ok()?;
    if hex.is_empty() {
        None
    } else {
        Some(format!("https://i.scdn.co/image/{hex}"))
    }
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
        .or_else(|| extract_librespot_image_url(&meta_artist.portraits));

    let biography = meta_artist
        .biographies
        .first()
        .map(|b| b.text.trim().to_string())
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
    let spotify_uri = t
        .id
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

/// Curated artists available out-of-the-box and in search.
pub fn all_curated_artists() -> Vec<ArtistSummary> {
    vec![
        ArtistSummary {
            id: "artist-lume".into(),
            spotify_uri: "spotify:artist:lume".into(),
            name: "Lume".into(),
            genres: vec!["Ambient Electronic".into(), "Nordic Post-Rock".into()],
            followers: Some(418_000),
            portrait_url: None,
            artwork_theme: ArtworkTheme::SunriseMountainLake,
        },
        ArtistSummary {
            id: "artist-kairo".into(),
            spotify_uri: "spotify:artist:kairo".into(),
            name: "Kairo".into(),
            genres: vec!["Downtempo".into(), "Warm Analog Synth".into()],
            followers: Some(692_400),
            portrait_url: None,
            artwork_theme: ArtworkTheme::GoldenCanyon,
        },
        ArtistSummary {
            id: "artist-valle".into(),
            spotify_uri: "spotify:artist:valle".into(),
            name: "Vallé".into(),
            genres: vec!["Neo-Classical".into(), "Atmospheric Piano".into()],
            followers: Some(312_900),
            portrait_url: None,
            artwork_theme: ArtworkTheme::MoonlitClouds,
        },
        ArtistSummary {
            id: "artist-nyra".into(),
            spotify_uri: "spotify:artist:nyra".into(),
            name: "Nyra".into(),
            genres: vec!["Dream Pop".into(), "Ethereal Indie".into()],
            followers: Some(254_100),
            portrait_url: None,
            artwork_theme: ArtworkTheme::MistyArch,
        },
        ArtistSummary {
            id: "artist-caspian".into(),
            spotify_uri: "spotify:artist:caspian".into(),
            name: "Caspian".into(),
            genres: vec!["Cinematic Post-Rock".into(), "Instrumental".into()],
            followers: Some(530_800),
            portrait_url: None,
            artwork_theme: ArtworkTheme::CoastalIsland,
        },
        ArtistSummary {
            id: "artist-nils-aether".into(),
            spotify_uri: "spotify:artist:nils-aether".into(),
            name: "Nils & Aether".into(),
            genres: vec!["Modular Synthesis".into(), "Modern Classical".into()],
            followers: Some(389_000),
            portrait_url: None,
            artwork_theme: ArtworkTheme::AlpineMist,
        },
    ]
}

/// Curated albums across the featured artists.
pub fn all_curated_albums() -> Vec<AlbumSummary> {
    let mut out = Vec::new();
    for artist in all_curated_artists() {
        let detail = curated_artist_detail(&artist.name);
        out.extend(detail.albums);
    }
    out
}

pub fn search_curated_artists(query: &str) -> Vec<ArtistSummary> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return all_curated_artists();
    }
    let mut matches: Vec<ArtistSummary> = all_curated_artists()
        .into_iter()
        .filter(|a| {
            a.name.to_ascii_lowercase().contains(&q)
                || a.genres.iter().any(|g| g.to_ascii_lowercase().contains(&q))
        })
        .collect();

    // If the user searched for any artist name not in the 6 presets, synthesize a dynamic profile
    // so searching for any artist name in offline/demo mode still displays an interactive artist card!
    if matches.is_empty() && !q.is_empty() {
        let display_name = query
            .trim()
            .split_whitespace()
            .map(|w| {
                let mut chars = w.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        matches.push(ArtistSummary {
            id: format!("artist-{}", q.replace(' ', "-")),
            spotify_uri: format!("spotify:artist:{}", q.replace(' ', "-")),
            name: display_name,
            genres: vec!["Featured Artist".into(), "High-Res Stream".into()],
            followers: Some(185_000),
            portrait_url: None,
            artwork_theme: ArtworkTheme::TwilightHorizon,
        });
    }
    matches
}

pub fn search_curated_albums(query: &str) -> Vec<AlbumSummary> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return all_curated_albums();
    }
    all_curated_albums()
        .into_iter()
        .filter(|alb| {
            alb.title.to_ascii_lowercase().contains(&q)
                || alb.artist_name.to_ascii_lowercase().contains(&q)
        })
        .collect()
}

/// Returns a complete `ArtistDetail` (bio, top tracks, and full album discography) for any artist.
pub fn curated_artist_detail(artist_key: &str) -> ArtistDetail {
    let key = artist_key
        .trim()
        .strip_prefix("spotify:artist:")
        .unwrap_or(artist_key.trim())
        .strip_prefix("artist-")
        .unwrap_or(artist_key.trim())
        .to_ascii_lowercase();

    match key.as_str() {
        "lume" | "artist-lume" => ArtistDetail {
            artist: all_curated_artists().remove(0),
            biography: Some(
                "Sculpting widescreen Nordic electronics, warm tape loops, and felt-piano horizons inspired by alpine lakes at first light."
                    .into(),
            ),
            top_tracks: vec![
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
                    id: "fjord-lanterns".into(),
                    spotify_uri: "spotify:track:2takcwOaAZWiXQijPHIx7B".into(),
                    title: "Fjord Lanterns".into(),
                    artist: "Lume".into(),
                    album: "Solstice".into(),
                    duration_secs: 3 * 60 + 54,
                    liked: false,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::DawnReflection,
                },
                Track {
                    id: "glacier-bloom".into(),
                    spotify_uri: "spotify:track:1301WleyT98MSxVHPZCA6M".into(),
                    title: "Glacier Bloom".into(),
                    artist: "Lume".into(),
                    album: "Northbound".into(),
                    duration_secs: 4 * 60 + 16,
                    liked: true,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::AlpineMist,
                },
                Track {
                    id: "auroral-drift".into(),
                    spotify_uri: "spotify:track:4cOdK2wGLETKBW3PvgPWqT".into(),
                    title: "Auroral Drift".into(),
                    artist: "Lume".into(),
                    album: "Glass Horizon".into(),
                    duration_secs: 5 * 60 + 11,
                    liked: false,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::TwilightHorizon,
                },
            ],
            albums: vec![
                AlbumSummary {
                    id: "album-solstice".into(),
                    spotify_uri: "spotify:album:solstice".into(),
                    title: "Solstice".into(),
                    artist_name: "Lume".into(),
                    release_year: "2025".into(),
                    album_type: "Album".into(),
                    track_count: 9,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::SunriseMountainLake,
                },
                AlbumSummary {
                    id: "album-northbound".into(),
                    spotify_uri: "spotify:album:northbound".into(),
                    title: "Northbound".into(),
                    artist_name: "Lume".into(),
                    release_year: "2023".into(),
                    album_type: "Album".into(),
                    track_count: 8,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::AlpineMist,
                },
                AlbumSummary {
                    id: "album-glass-horizon".into(),
                    spotify_uri: "spotify:album:glass-horizon".into(),
                    title: "Glass Horizon".into(),
                    artist_name: "Lume".into(),
                    release_year: "2022".into(),
                    album_type: "EP".into(),
                    track_count: 5,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::DawnReflection,
                },
            ],
        },

        "kairo" | "artist-kairo" => ArtistDetail {
            artist: all_curated_artists().remove(1),
            biography: Some(
                "Analog polysynths, dusty Rhodes chords, and late-night downtempo grooves crafted between Tokyo and Lisbon."
                    .into(),
            ),
            top_tracks: vec![
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
                    id: "velvet-meridian".into(),
                    spotify_uri: "spotify:track:6PQ88X9TkUIAUIZJHW2upE".into(),
                    title: "Velvet Meridian".into(),
                    artist: "Kairo".into(),
                    album: "Sunburst Tape".into(),
                    duration_secs: 4 * 60 + 5,
                    liked: false,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::WarmDusk,
                },
            ],
            albums: vec![
                AlbumSummary {
                    id: "album-afterglow".into(),
                    spotify_uri: "spotify:album:afterglow".into(),
                    title: "Afterglow".into(),
                    artist_name: "Kairo".into(),
                    release_year: "2024".into(),
                    album_type: "Album".into(),
                    track_count: 10,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::GoldenCanyon,
                },
                AlbumSummary {
                    id: "album-sunburst-tape".into(),
                    spotify_uri: "spotify:album:sunburst-tape".into(),
                    title: "Sunburst Tape".into(),
                    artist_name: "Kairo".into(),
                    release_year: "2023".into(),
                    album_type: "Album".into(),
                    track_count: 8,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::WarmDusk,
                },
                AlbumSummary {
                    id: "album-shibuya-rain".into(),
                    spotify_uri: "spotify:album:shibuya-rain".into(),
                    title: "Shibuya Rain".into(),
                    artist_name: "Kairo".into(),
                    release_year: "2021".into(),
                    album_type: "Single".into(),
                    track_count: 3,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::EmeraldRing,
                },
            ],
        },

        "valle" | "vallé" | "artist-valle" => ArtistDetail {
            artist: all_curated_artists().remove(2),
            biography: Some(
                "Intimate nocturnal chamber works blending upright piano, cello harmonics, and subtle modular textures."
                    .into(),
            ),
            top_tracks: vec![
                Track {
                    id: "echoes-in-bloom".into(),
                    spotify_uri: "spotify:track:0VjIjW4GlUZAMYd2vXMi3b".into(),
                    title: "Echoes in Bloom".into(),
                    artist: "Vallé".into(),
                    album: "Nocturne".into(),
                    duration_secs: 4 * 60 + 3,
                    liked: false,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::MoonlitClouds,
                },
                Track {
                    id: "lunar-etude".into(),
                    spotify_uri: "spotify:track:4iV5W9uYEdYUVa79Axb7Rh".into(),
                    title: "Lunar Étude No. 3".into(),
                    artist: "Vallé".into(),
                    album: "Nocturne".into(),
                    duration_secs: 3 * 60 + 48,
                    liked: true,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::TwilightHorizon,
                },
            ],
            albums: vec![
                AlbumSummary {
                    id: "album-nocturne".into(),
                    spotify_uri: "spotify:album:nocturne".into(),
                    title: "Nocturne".into(),
                    artist_name: "Vallé".into(),
                    release_year: "2024".into(),
                    album_type: "Album".into(),
                    track_count: 8,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::MoonlitClouds,
                },
                AlbumSummary {
                    id: "album-clair-obscur".into(),
                    spotify_uri: "spotify:album:clair-obscur".into(),
                    title: "Clair-Obscur".into(),
                    artist_name: "Vallé".into(),
                    release_year: "2022".into(),
                    album_type: "Album".into(),
                    track_count: 7,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::VioletAura,
                },
            ],
        },

        _ => {
            let matching_artist = search_curated_artists(artist_key)
                .into_iter()
                .next()
                .unwrap_or_else(|| all_curated_artists().remove(0));
            let name = matching_artist.name.clone();
            let mut top_tracks = search_curated_tracks(&name);
            if top_tracks.is_empty() {
                top_tracks = PlaybackState::mockup_default()
                    .queue
                    .into_iter()
                    .take(4)
                    .map(|mut t| {
                        t.artist = name.clone();
                        t
                    })
                    .collect();
            }
            let albums = vec![
                AlbumSummary {
                    id: format!("album-{}-horizons", name.to_ascii_lowercase().replace(' ', "-")),
                    spotify_uri: format!(
                        "spotify:album:{}-horizons",
                        name.to_ascii_lowercase().replace(' ', "-")
                    ),
                    title: format!("{name} — Horizons"),
                    artist_name: name.clone(),
                    release_year: "2025".into(),
                    album_type: "Album".into(),
                    track_count: 8,
                    cover_art_url: None,
                    artwork_theme: matching_artist.artwork_theme,
                },
                AlbumSummary {
                    id: format!("album-{}-sessions", name.to_ascii_lowercase().replace(' ', "-")),
                    spotify_uri: format!(
                        "spotify:album:{}-sessions",
                        name.to_ascii_lowercase().replace(' ', "-")
                    ),
                    title: "Late Night Sessions".into(),
                    artist_name: name,
                    release_year: "2023".into(),
                    album_type: "EP".into(),
                    track_count: 5,
                    cover_art_url: None,
                    artwork_theme: ArtworkTheme::TwilightHorizon,
                },
            ];
            ArtistDetail {
                artist: matching_artist,
                biography: Some(
                    "Atmospheric compositions and high-dynamic-range studio recordings.".into(),
                ),
                top_tracks,
                albums,
            }
        }
    }
}

/// Returns curated tracks for a given album ID or URI when offline.
pub fn curated_tracks_for_album(album_key: &str) -> Vec<Track> {
    let q = album_key
        .trim()
        .strip_prefix("spotify:album:")
        .unwrap_or(album_key.trim())
        .strip_prefix("album-")
        .unwrap_or(album_key.trim())
        .to_ascii_lowercase();

    for artist in all_curated_artists() {
        let detail = curated_artist_detail(&artist.name);
        if let Some(alb) = detail.albums.iter().find(|a| {
            a.id.to_ascii_lowercase().contains(&q)
                || a.title.to_ascii_lowercase().contains(&q)
                || a.spotify_uri.to_ascii_lowercase().contains(&q)
        }) {
            let mut tracks = detail.top_tracks;
            for (idx, t) in tracks.iter_mut().enumerate() {
                t.album = alb.title.clone();
                t.artwork_theme = if idx == 0 {
                    alb.artwork_theme
                } else {
                    track_theme_for_index(idx)
                };
            }
            return tracks;
        }
    }

    PlaybackState::mockup_default().queue
}
