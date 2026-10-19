use std::fs;
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use librespot_core::{
    authentication::Credentials,
    cache::Cache,
    config::SessionConfig,
    session::Session,
};
use librespot_oauth::OAuthClientBuilder;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn};

/// Default Spotify desktop/keymaster Client ID used by librespot when the user has not
/// supplied a custom Spotify Developer Client ID.
pub const DEFAULT_SPOTIFY_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";

/// Local loopback callback URI used for the OAuth 2.0 PKCE login listener.
pub const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:8898/login";

/// Maximum encrypted audio cache size on disk (1 GiB).
pub const AUDIO_CACHE_LIMIT_BYTES: u64 = 1024 * 1024 * 1024;

/// OAuth 2.0 scopes requested during Spotify Premium PKCE login.
pub const SPOTIFY_OAUTH_SCOPES: &[&str] = &[
    "streaming",
    "user-read-private",
    "user-read-email",
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-library-read",
    "user-library-modify",
    "user-read-playback-state",
    "user-modify-playback-state",
];

#[derive(Debug, Error)]
pub enum SpotifyAuthError {
    #[error("failed to initialize Spot cache directory: {0}")]
    CacheInit(String),

    #[error("Spotify OAuth PKCE flow failed: {0}")]
    OAuth(String),

    #[error("Spotify session connection failed: {0}")]
    SessionConnect(String),

    #[error("blocking OAuth task panicked: {0}")]
    TaskJoin(String),
}

/// Resolved directories under the user's application data folder (`.../Spot`).
#[derive(Debug, Clone)]
pub struct AppCachePaths {
    pub root_dir: PathBuf,
    pub credentials_dir: PathBuf,
    pub audio_cache_dir: PathBuf,
    pub image_cache_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedOAuthToken {
    access_token: String,
    refresh_token: String,
    expires_in_secs: u64,
    saved_at_unix_secs: u64,
}

impl AppCachePaths {
    /// Returns the Spot application data directories:
    /// `dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("Spot")`
    /// with `credentials`, `audio_cache`, and `image_cache` subdirectories.
    /// Automatically migrates any existing credentials and history from legacy `Groove` data dir.
    pub fn resolve() -> Self {
        let is_test = std::env::var("SPOT_TEST_CACHE_DIR").is_ok()
            || std::env::var("GROOVE_TEST_CACHE_DIR").is_ok()
            || cfg!(test);
        let root_dir = if is_test {
            std::env::temp_dir().join("spot-test-cache")
        } else {
            let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
            let spot_dir = base.join("Spot");
            let legacy_groove_dir = base.join("Groove");
            migrate_legacy_cache_if_needed(&legacy_groove_dir, &spot_dir);
            spot_dir
        };
        let credentials_dir = root_dir.join("credentials");
        let audio_cache_dir = root_dir.join("audio_cache");
        let image_cache_dir = root_dir.join("image_cache");
        Self {
            root_dir,
            credentials_dir,
            audio_cache_dir,
            image_cache_dir,
        }
    }

    pub fn oauth_token_path(&self) -> PathBuf {
        self.credentials_dir.join("oauth_token.json")
    }

    pub fn save_oauth_token(&self, access_token: &str, refresh_token: &str, expires_at: Instant) {
        let expires_in_secs = expires_at
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::from_secs(3600))
            .as_secs();
        let saved_at_unix_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let payload = PersistedOAuthToken {
            access_token: access_token.to_string(),
            refresh_token: refresh_token.to_string(),
            expires_in_secs,
            saved_at_unix_secs,
        };
        if let Ok(json) = serde_json::to_string_pretty(&payload) {
            let _ = fs::write(self.oauth_token_path(), json);
        }
    }

    pub fn load_valid_oauth_token(&self) -> Option<String> {
        let raw = fs::read_to_string(self.oauth_token_path()).ok()?;
        let parsed: PersistedOAuthToken = serde_json::from_str(&raw).ok()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now + 90 < parsed.saved_at_unix_secs + parsed.expires_in_secs {
            Some(parsed.access_token)
        } else {
            None
        }
    }
}

/// Active authenticated `librespot` session and optional Web API bearer token.
#[derive(Clone)]
pub struct AuthenticatedSession {
    pub session: Session,
    pub username: String,
    pub access_token: Option<String>,
}

/// Seamlessly migrates existing credentials and user history from `~/Library/Application Support/Groove`
/// to `~/Library/Application Support/Spot` on first launch after renaming.
fn migrate_legacy_cache_if_needed(legacy_dir: &std::path::Path, spot_dir: &std::path::Path) {
    if !legacy_dir.exists() {
        return;
    }
    let spot_creds = spot_dir.join("credentials").join("credentials.json");
    let legacy_creds = legacy_dir.join("credentials").join("credentials.json");
    if !spot_creds.exists() && legacy_creds.exists() {
        let _ = fs::create_dir_all(spot_dir.join("credentials"));
        let _ = fs::copy(&legacy_creds, &spot_creds);
    }

    for rel in [
        "credentials/oauth_token.json",
        "recent_searches.json",
        "recently_listened.json",
    ] {
        let src = legacy_dir.join(rel);
        let dst = spot_dir.join(rel);
        if !dst.exists() && src.exists() {
            if let Some(parent) = dst.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::copy(&src, &dst);
        }
    }
}

/// Opens the `librespot_core::cache::Cache` backed by Spot's `credentials` and
/// 1 GiB `audio_cache` directories.
pub fn open_cache() -> Result<(Cache, AppCachePaths), SpotifyAuthError> {
    let paths = AppCachePaths::resolve();
    let _ = fs::create_dir_all(&paths.credentials_dir);
    let _ = fs::create_dir_all(&paths.audio_cache_dir);
    let _ = fs::create_dir_all(&paths.image_cache_dir);

    let creds_dir = paths.credentials_dir.clone();
    let audio_cache_dir = paths.audio_cache_dir.clone();

    let cache = Cache::new(
        Some(creds_dir.as_path()),
        None,
        Some(audio_cache_dir.as_path()),
        Some(AUDIO_CACHE_LIMIT_BYTES),
    )
    .map_err(|e| SpotifyAuthError::CacheInit(e.to_string()))?;

    Ok((cache, paths))
}

/// Checks whether reusable Spotify credentials are already stored in the local cache.
pub fn cached_credentials() -> Option<Credentials> {
    let (cache, _) = open_cache().ok()?;
    cache.credentials()
}

/// Clears stored credentials and OAuth tokens on disk so the user can sign out or switch accounts.
pub fn clear_cached_credentials() {
    let paths = AppCachePaths::resolve();
    let _ = fs::remove_file(paths.credentials_dir.join("credentials.json"));
    let _ = fs::remove_file(paths.oauth_token_path());
}

/// Obtains a fresh Spotify Web API Bearer token from an active `Session` via `login5`,
/// falling back to any valid persisted OAuth token on disk.
pub async fn obtain_session_access_token(
    session: &Session,
    paths: &AppCachePaths,
) -> Option<String> {
    match session.login5().auth_token().await {
        Ok(token) if !token.access_token.is_empty() => {
            info!("Obtained fresh Spotify Web API token via login5");
            return Some(token.access_token);
        }
        Ok(_) => {}
        Err(err) => {
            warn!("login5 token lookup returned: {err}; checking persisted OAuth token");
        }
    }
    paths.load_valid_oauth_token()
}

/// Releases any previous unfinished loopback listener on `127.0.0.1:8898` before starting a new OAuth dance.
fn release_stale_loopback_listener() {
    let addr: SocketAddr = "127.0.0.1:8898".parse().expect("valid socket addr");
    if let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(80)) {
        let _ = stream.write_all(b"GET /cancel HTTP/1.1\r\nHost: 127.0.0.1:8898\r\n\r\n");
        let _ = stream.flush();
        std::thread::sleep(Duration::from_millis(40));
    }
}

/// Builds a `SessionConfig` using Spot's app data directory and optional custom client ID.
fn build_session_config(paths: &AppCachePaths, client_id: Option<&str>) -> SessionConfig {
    let mut config = SessionConfig::default();
    if let Some(custom_id) = client_id.map(str::trim).filter(|s| !s.is_empty()) {
        config.client_id = custom_id.to_owned();
    } else if let Ok(env_id) = std::env::var("SPOTIFY_CLIENT_ID") {
        let trimmed = env_id.trim();
        if !trimmed.is_empty() {
            config.client_id = trimmed.to_owned();
        }
    }
    let tmp_dir = paths.root_dir.join("tmp");
    let _ = fs::create_dir_all(&tmp_dir);
    config.tmp_dir = tmp_dir;
    config
}

/// Attempts a non-interactive connection using cached credentials if available on disk.
pub async fn try_connect_cached(
    client_id: Option<&str>,
) -> Result<Option<AuthenticatedSession>, SpotifyAuthError> {
    let (cache, paths) = open_cache()?;
    let Some(credentials) = cache.credentials() else {
        return Ok(None);
    };

    info!("Found cached Spotify credentials; attempting non-interactive session connect");
    let session_config = build_session_config(&paths, client_id);
    let session = Session::new(session_config, Some(cache));

    session
        .connect(credentials.clone(), false)
        .await
        .map_err(|e| SpotifyAuthError::SessionConnect(e.to_string()))?;

    let username = {
        let session_user = session.username();
        if !session_user.is_empty() && session_user != "UNKNOWN" {
            session_user
        } else {
            credentials
                .username
                .unwrap_or_else(|| "Spotify Premium".to_string())
        }
    };

    let access_token = obtain_session_access_token(&session, &paths).await;

    Ok(Some(AuthenticatedSession {
        session,
        username,
        access_token,
    }))
}

/// Authenticates with Spotify Premium and connects a live `librespot_core::Session`.
///
/// 1. When `force_reauth` is false, checks `cache.credentials()` for reusable cached credentials and tries connecting.
/// 2. Otherwise (or if cached connection fails), runs the interactive OAuth 2.0
///    PKCE browser login on `tokio::task::spawn_blocking`, exchanges the code for an access token,
///    constructs `Credentials::with_access_token`, and connects & persists the `Session`.
pub async fn authenticate_and_connect(
    client_id: Option<&str>,
    force_reauth: bool,
) -> Result<AuthenticatedSession, SpotifyAuthError> {
    let (cache, paths) = open_cache()?;
    let session_config = build_session_config(&paths, client_id);

    // 1. Try cached credentials first if present and not forcing browser re-auth.
    if !force_reauth {
        if let Some(cached_creds) = cache.credentials() {
            info!("Attempting Spotify connection using cached credentials");
            let cached_session = Session::new(session_config.clone(), Some(cache.clone()));
            match cached_session.connect(cached_creds.clone(), false).await {
                Ok(()) => {
                    let username = {
                        let u = cached_session.username();
                        if !u.is_empty() && u != "UNKNOWN" {
                            u
                        } else {
                            cached_creds
                                .username
                                .unwrap_or_else(|| "Spotify Premium".to_string())
                        }
                    };
                    let access_token = obtain_session_access_token(&cached_session, &paths).await;
                    info!("Connected to Spotify Premium from cache as '{username}'");
                    return Ok(AuthenticatedSession {
                        session: cached_session,
                        username,
                        access_token,
                    });
                }
                Err(err) => {
                    warn!("Cached Spotify credentials failed ({err}); falling back to OAuth PKCE flow");
                }
            }
        }
    }

    // 2. Run OAuth 2.0 PKCE flow on blocking thread pool.
    let resolved_client_id = session_config.client_id.clone();
    let redirect_uri = DEFAULT_REDIRECT_URI.to_string();

    info!("Starting Spotify OAuth PKCE login (redirect_uri={redirect_uri})");
    let token = tokio::task::spawn_blocking(move || {
        release_stale_loopback_listener();
        let scopes: Vec<&str> = SPOTIFY_OAUTH_SCOPES.to_vec();
        let oauth_client = OAuthClientBuilder::new(&resolved_client_id, &redirect_uri, scopes)
            .open_in_browser()
            .with_custom_message(
                "<!doctype html><html><body style=\"background:#F4F2EE;color:#191918;font-family:-apple-system,BlinkMacSystemFont,sans-serif;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;\"><div style=\"background:#FBF9F6;padding:40px 48px;border-radius:20px;box-shadow:0 12px 32px rgba(0,0,0,0.06);text-align:center;max-width:420px;\"><h2 style=\"margin:0 0 12px;font-size:24px;\"><span style=\"display:inline-block;width:11px;height:11px;border-radius:50%;background:#48A868;margin-right:8px;\"></span>Connected to Spot</h2><p style=\"color:#7C7A75;margin:0;line-height:1.5;\">Your Spotify Premium session is active. You can close this browser tab and return to Spot.</p></div></body></html>",
            )
            .build()
            .map_err(|e| SpotifyAuthError::OAuth(e.to_string()))?;

        oauth_client
            .get_access_token()
            .map_err(|e| SpotifyAuthError::OAuth(e.to_string()))
    })
    .await
    .map_err(|e| SpotifyAuthError::TaskJoin(e.to_string()))??;

    paths.save_oauth_token(&token.access_token, &token.refresh_token, token.expires_at);
    let access_token = token.access_token.clone();
    let credentials = Credentials::with_access_token(token.access_token);

    let session = Session::new(session_config, Some(cache));
    session
        .connect(credentials, true)
        .await
        .map_err(|e| SpotifyAuthError::SessionConnect(e.to_string()))?;

    let username = {
        let u = session.username();
        if !u.is_empty() && u != "UNKNOWN" {
            u
        } else {
            "Spotify Premium".to_string()
        }
    };

    let resolved_token = obtain_session_access_token(&session, &paths)
        .await
        .or(Some(access_token));

    info!("Connected to Spotify Premium via OAuth PKCE as '{username}'");
    Ok(AuthenticatedSession {
        session,
        username,
        access_token: resolved_token,
    })
}
