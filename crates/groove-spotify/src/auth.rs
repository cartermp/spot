use std::fs;
use std::path::PathBuf;

use librespot_core::{
    authentication::Credentials,
    cache::Cache,
    config::SessionConfig,
    session::Session,
};
use librespot_oauth::OAuthClientBuilder;
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
    #[error("failed to initialize Groove cache directory: {0}")]
    CacheInit(String),

    #[error("Spotify OAuth PKCE flow failed: {0}")]
    OAuth(String),

    #[error("Spotify session connection failed: {0}")]
    SessionConnect(String),

    #[error("blocking OAuth task panicked: {0}")]
    TaskJoin(String),
}

/// Resolved directories under the user's application data folder (`.../Groove`).
#[derive(Debug, Clone)]
pub struct AppCachePaths {
    pub root_dir: PathBuf,
    pub credentials_dir: PathBuf,
    pub audio_cache_dir: PathBuf,
}

impl AppCachePaths {
    /// Returns the Groove application data directories:
    /// `dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("Groove")`
    /// with `credentials` and `audio_cache` subdirectories.
    pub fn resolve() -> Self {
        let root_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Groove");
        let credentials_dir = root_dir.join("credentials");
        let audio_cache_dir = root_dir.join("audio_cache");
        Self {
            root_dir,
            credentials_dir,
            audio_cache_dir,
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

/// Opens the `librespot_core::cache::Cache` backed by Groove's `credentials` and
/// 1 GiB `audio_cache` directories.
pub fn open_cache() -> Result<(Cache, AppCachePaths), SpotifyAuthError> {
    let paths = AppCachePaths::resolve();
    let _ = fs::create_dir_all(&paths.credentials_dir);
    let _ = fs::create_dir_all(&paths.audio_cache_dir);

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

/// Builds a `SessionConfig` using Groove's app data directory and optional custom client ID.
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

    Ok(Some(AuthenticatedSession {
        session,
        username,
        access_token: None,
    }))
}

/// Authenticates with Spotify Premium and connects a live `librespot_core::Session`.
///
/// 1. First checks `cache.credentials()` for reusable cached credentials and tries connecting.
/// 2. If no cached credentials exist (or cached connection fails), runs the interactive OAuth 2.0
///    PKCE browser login on `tokio::task::spawn_blocking`, exchanges the code for an access token,
///    constructs `Credentials::with_access_token`, and connects & persists the `Session`.
pub async fn authenticate_and_connect(
    client_id: Option<&str>,
) -> Result<AuthenticatedSession, SpotifyAuthError> {
    let (cache, paths) = open_cache()?;
    let session_config = build_session_config(&paths, client_id);

    // 1. Try cached credentials first if present.
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
                info!("Connected to Spotify Premium from cache as '{username}'");
                return Ok(AuthenticatedSession {
                    session: cached_session,
                    username,
                    access_token: None,
                });
            }
            Err(err) => {
                warn!("Cached Spotify credentials failed ({err}); falling back to OAuth PKCE flow");
            }
        }
    }

    // 2. Run OAuth 2.0 PKCE flow on blocking thread pool.
    let resolved_client_id = session_config.client_id.clone();
    let redirect_uri = DEFAULT_REDIRECT_URI.to_string();

    info!("Starting Spotify OAuth PKCE login (redirect_uri={redirect_uri})");
    let token = tokio::task::spawn_blocking(move || {
        let scopes: Vec<&str> = SPOTIFY_OAUTH_SCOPES.to_vec();
        let oauth_client = OAuthClientBuilder::new(&resolved_client_id, &redirect_uri, scopes)
            .open_in_browser()
            .with_custom_message(
                "<html><body style=\"background:#12131a;color:#f5f5f7;font-family:-apple-system,sans-serif;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;\"><div><h2>Connected to Groove</h2><p>You can close this browser tab and return to Groove.</p></div></body></html>",
            )
            .build()
            .map_err(|e| SpotifyAuthError::OAuth(e.to_string()))?;

        oauth_client
            .get_access_token()
            .map_err(|e| SpotifyAuthError::OAuth(e.to_string()))
    })
    .await
    .map_err(|e| SpotifyAuthError::TaskJoin(e.to_string()))??;

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

    info!("Connected to Spotify Premium via OAuth PKCE as '{username}'");
    Ok(AuthenticatedSession {
        session,
        username,
        access_token: Some(access_token),
    })
}
