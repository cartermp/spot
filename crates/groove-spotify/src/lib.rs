pub mod auth;
pub mod sink;
pub mod web_api;

use std::{sync::Arc, time::Duration};

use flume::{Receiver, Sender};
use groove_audio::{enumerate_output_devices, AudioSampleProducer};
use groove_core::{
    AudioDevice, ConnectionStatus, EngineEvent, PlaybackState, PlayerCommand, RepeatMode, Track,
};
use librespot_core::{session::Session, SpotifyUri};
use librespot_playback::{
    config::{AudioFormat, Bitrate, PlayerConfig},
    mixer::NoOpVolume,
    player::{Player, PlayerEvent, PlayerEventChannel},
};
use tracing::{debug, info, warn};

use crate::{
    sink::{AtomicVolumeScaler, TeeSink},
    web_api::SpotifyWebClient,
};

pub struct EngineHandle {
    pub command_tx: Sender<PlayerCommand>,
    pub event_rx: Receiver<EngineEvent>,
}

/// Spawns the asynchronous Spotify Premium + Audio transport engine on a dedicated
/// multi-threaded Tokio runtime thread and returns the bidirectional `flume` channel handle.
pub fn spawn_engine(sample_producer: AudioSampleProducer) -> EngineHandle {
    let (command_tx, command_rx) = flume::unbounded();
    let (event_tx, event_rx) = flume::unbounded();

    std::thread::Builder::new()
        .name("groove-spotify-engine".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build Tokio runtime for groove-spotify");

            runtime.block_on(run_engine_loop(command_rx, event_tx, sample_producer));
        })
        .expect("failed to spawn groove-spotify-engine thread");

    EngineHandle {
        command_tx,
        event_rx,
    }
}

async fn run_engine_loop(
    command_rx: Receiver<PlayerCommand>,
    event_tx: Sender<EngineEvent>,
    sample_producer: AudioSampleProducer,
) {
    let mut state = PlaybackState::mockup_default();

    // Merge real macOS CoreAudio output devices if available.
    merge_output_devices(&mut state, enumerate_output_devices());

    // Reflect cached credentials if already stored on disk, and auto-connect in background.
    let has_cached_creds = auth::cached_credentials().is_some();
    if has_cached_creds {
        state.connection = ConnectionStatus::Connecting {
            status: "Reconnecting Spotify session...".to_string(),
        };
    }

    let volume_scaler = AtomicVolumeScaler::new(state.volume);
    let mut web_client = SpotifyWebClient::default();
    let mut live_session: Option<Session> = None;
    let mut player: Option<Arc<Player>> = None;
    let mut player_events: Option<PlayerEventChannel> = None;
    let (auth_result_tx, auth_result_rx) =
        flume::unbounded::<Result<auth::AuthenticatedSession, auth::SpotifyAuthError>>();

    if has_cached_creds {
        let tx = auth_result_tx.clone();
        tokio::spawn(async move {
            if let Ok(Some(session)) = auth::try_connect_cached(None).await {
                let _ = tx.send(Ok(session));
            }
        });
    }

    // Emit initial full state snapshot immediately on startup.
    let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));

    let mut tick_interval = tokio::time::interval(Duration::from_millis(100));
    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Consume the immediate initial tick so the first 100ms increment fires after 100ms.
    tick_interval.tick().await;

    loop {
        tokio::select! {
            biased;

            Ok(auth_res) = auth_result_rx.recv_async() => {
                apply_auth_result(
                    auth_res,
                    &mut state,
                    &event_tx,
                    &sample_producer,
                    &volume_scaler,
                    &mut web_client,
                    &mut live_session,
                    &mut player,
                    &mut player_events,
                )
                .await;
            }

            maybe_cmd = command_rx.recv_async() => {
                let Ok(cmd) = maybe_cmd else {
                    debug!("Engine command channel closed; shutting down groove-spotify loop");
                    break;
                };

                handle_command(
                    cmd,
                    &mut state,
                    &event_tx,
                    &sample_producer,
                    &volume_scaler,
                    &mut web_client,
                    &mut live_session,
                    &mut player,
                    &mut player_events,
                    &auth_result_tx,
                )
                .await;
            }

            Some(player_evt) = recv_player_event(&mut player_events) => {
                handle_librespot_event(
                    player_evt,
                    &mut state,
                    &event_tx,
                    &player,
                );
            }

            _ = tick_interval.tick() => {
                if state.is_playing {
                    state.position_secs += 0.1;
                    let duration = state.current_track.duration_secs as f32;
                    if duration > 0.0 && state.position_secs >= duration {
                        advance_queue_track(&mut state, true, true);
                        if let Some(active_player) = player.as_ref() {
                            load_track_on_player(
                                active_player,
                                &state.current_track,
                                state.is_playing,
                                0.0,
                            );
                        }
                        let _ = event_tx.send(EngineEvent::TrackChanged(state.current_track.clone()));
                        let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
                    } else {
                        let _ = event_tx.send(EngineEvent::PositionTick(state.position_secs));
                    }
                }
            }
        }
    }
}

async fn recv_player_event(channel: &mut Option<PlayerEventChannel>) -> Option<PlayerEvent> {
    match channel.as_mut() {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

fn handle_librespot_event(
    event: PlayerEvent,
    state: &mut PlaybackState,
    event_tx: &Sender<EngineEvent>,
    player: &Option<Arc<Player>>,
) {
    match event {
        PlayerEvent::Playing { position_ms, .. } => {
            state.is_playing = true;
            state.position_secs = position_ms as f32 / 1000.0;
            let _ = event_tx.send(EngineEvent::PlayStateChanged(true));
        }
        PlayerEvent::Paused { position_ms, .. } => {
            state.is_playing = false;
            state.position_secs = position_ms as f32 / 1000.0;
            let _ = event_tx.send(EngineEvent::PlayStateChanged(false));
        }
        PlayerEvent::PositionChanged { position_ms, .. }
        | PlayerEvent::PositionCorrection { position_ms, .. }
        | PlayerEvent::Seeked { position_ms, .. } => {
            state.position_secs = position_ms as f32 / 1000.0;
            let _ = event_tx.send(EngineEvent::PositionTick(state.position_secs));
        }
        PlayerEvent::EndOfTrack { .. } => {
            advance_queue_track(state, true, true);
            if let Some(active_player) = player.as_ref() {
                load_track_on_player(
                    active_player,
                    &state.current_track,
                    state.is_playing,
                    0.0,
                );
            }
            let _ = event_tx.send(EngineEvent::TrackChanged(state.current_track.clone()));
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
async fn apply_auth_result(
    auth_res: Result<auth::AuthenticatedSession, auth::SpotifyAuthError>,
    state: &mut PlaybackState,
    event_tx: &Sender<EngineEvent>,
    sample_producer: &AudioSampleProducer,
    volume_scaler: &AtomicVolumeScaler,
    web_client: &mut SpotifyWebClient,
    live_session: &mut Option<Session>,
    player: &mut Option<Arc<Player>>,
    player_events: &mut Option<PlayerEventChannel>,
) {
    match auth_res {
        Ok(auth_session) => {
            web_client.set_access_token(auth_session.access_token.clone());
            let mut username = auth_session.username.clone();
            let mut is_premium = true;

            if web_client.access_token().is_some() {
                if let Ok(profile) = web_client.fetch_user_profile().await {
                    username = profile.display_username();
                    is_premium = profile.is_premium();
                }
                let user_playlists = web_client.fetch_user_playlists().await;
                if !user_playlists.is_empty() {
                    state.playlists = user_playlists;
                    let _ = event_tx.send(EngineEvent::PlaylistsLoaded(state.playlists.clone()));
                }
            }

            let device_arg = if state.selected_device.is_default {
                None
            } else {
                Some(state.selected_device.name.clone())
            };
            let (new_player, new_events) = create_librespot_player(
                auth_session.session.clone(),
                device_arg,
                sample_producer.clone(),
                volume_scaler.clone(),
            );
            load_track_on_player(
                &new_player,
                &state.current_track,
                state.is_playing,
                state.position_secs,
            );

            *live_session = Some(auth_session.session);
            *player = Some(new_player);
            *player_events = Some(new_events);

            state.connection = ConnectionStatus::Connected {
                username,
                is_premium,
                live_session: true,
            };
            info!("Spotify Premium engine connected and player initialized");
        }
        Err(err) => {
            warn!("Spotify authentication failed: {err}");
            state.connection = ConnectionStatus::Disconnected {
                reason: Some(err.to_string()),
            };
        }
    }

    let _ = event_tx.send(EngineEvent::ConnectionUpdated(state.connection.clone()));
    let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
}

async fn ensure_fresh_web_token(
    web_client: &mut SpotifyWebClient,
    live_session: &Option<Session>,
) {
    if web_client.access_token().is_none() {
        if let Some(sess) = live_session.as_ref() {
            let paths = auth::AppCachePaths::resolve();
            if let Some(tok) = auth::obtain_session_access_token(sess, &paths).await {
                web_client.set_access_token(Some(tok));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_command(
    cmd: PlayerCommand,
    state: &mut PlaybackState,
    event_tx: &Sender<EngineEvent>,
    sample_producer: &AudioSampleProducer,
    volume_scaler: &AtomicVolumeScaler,
    web_client: &mut SpotifyWebClient,
    live_session: &mut Option<Session>,
    player: &mut Option<Arc<Player>>,
    player_events: &mut Option<PlayerEventChannel>,
    auth_result_tx: &Sender<Result<auth::AuthenticatedSession, auth::SpotifyAuthError>>,
) {
    match cmd {
        PlayerCommand::TogglePlayPause => {
            state.is_playing = !state.is_playing;
            if let Some(active_player) = player.as_ref() {
                if state.is_playing {
                    active_player.play();
                } else {
                    active_player.pause();
                }
            }
            let _ = event_tx.send(EngineEvent::PlayStateChanged(state.is_playing));
        }

        PlayerCommand::Play => {
            state.is_playing = true;
            if let Some(active_player) = player.as_ref() {
                active_player.play();
            }
            let _ = event_tx.send(EngineEvent::PlayStateChanged(true));
        }

        PlayerCommand::Pause => {
            state.is_playing = false;
            if let Some(active_player) = player.as_ref() {
                active_player.pause();
            }
            let _ = event_tx.send(EngineEvent::PlayStateChanged(false));
        }

        PlayerCommand::NextTrack => {
            advance_queue_track(state, true, false);
            if let Some(active_player) = player.as_ref() {
                load_track_on_player(
                    active_player,
                    &state.current_track,
                    state.is_playing,
                    0.0,
                );
            }
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::PreviousTrack => {
            advance_queue_track(state, false, false);
            if let Some(active_player) = player.as_ref() {
                load_track_on_player(
                    active_player,
                    &state.current_track,
                    state.is_playing,
                    0.0,
                );
            }
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::SeekSeconds(pos) => {
            let max_dur = state.current_track.duration_secs as f32;
            let clamped = if max_dur > 0.0 {
                pos.clamp(0.0, max_dur)
            } else {
                pos.max(0.0)
            };
            state.position_secs = clamped;
            if let Some(active_player) = player.as_ref() {
                active_player.seek((clamped * 1000.0) as u32);
            }
            let _ = event_tx.send(EngineEvent::PositionTick(state.position_secs));
        }

        PlayerCommand::SetVolume(vol) => {
            let clamped = vol.clamp(0.0, 1.0);
            state.volume = clamped;
            volume_scaler.set(clamped);
            if let Some(active_player) = player.as_ref() {
                let raw_vol = (clamped as f64 * u16::MAX as f64).round() as u16;
                active_player.emit_volume_changed_event(raw_vol);
            }
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::ToggleShuffle => {
            state.shuffle = !state.shuffle;
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::CycleRepeat => {
            state.repeat = state.repeat.next();
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::ToggleLikeCurrent => {
            state.current_track.liked = !state.current_track.liked;
            let liked = state.current_track.liked;
            let current_id = state.current_track.id.clone();
            for track in &mut state.queue {
                if track.id == current_id {
                    track.liked = liked;
                }
            }
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::SelectQueueTrack(track_id) => {
            if let Some(selected) = state
                .queue
                .iter()
                .find(|t| t.id == track_id || t.spotify_uri == track_id)
                .cloned()
            {
                state.current_track = selected;
                state.position_secs = 0.0;
                state.is_playing = true;
                if let Some(active_player) = player.as_ref() {
                    load_track_on_player(active_player, &state.current_track, true, 0.0);
                }
            }
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::PlayTrack(track) => {
            if !state
                .queue
                .iter()
                .any(|t| t.id == track.id || t.spotify_uri == track.spotify_uri)
            {
                state.queue.insert(0, track.clone());
            }
            state.current_track = track;
            state.position_secs = 0.0;
            state.is_playing = true;
            if let Some(active_player) = player.as_ref() {
                load_track_on_player(active_player, &state.current_track, true, 0.0);
            }
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::SelectPlaylist(playlist_id) => {
            ensure_fresh_web_token(web_client, live_session).await;
            let mut resolved_key = playlist_id.clone();
            for playlist in &mut state.playlists {
                let is_match = playlist.id == playlist_id
                    || playlist.spotify_uri == playlist_id
                    || playlist.name.eq_ignore_ascii_case(&playlist_id);
                playlist.is_playing_context = is_match;
                if is_match {
                    resolved_key = playlist.id.clone();
                }
            }
            state.active_playlist_id = Some(resolved_key.clone());

            let tracks = web_client.fetch_playlist_tracks(&resolved_key).await;
            if !tracks.is_empty() {
                state.current_track = tracks[0].clone();
                state.queue = tracks;
                state.position_secs = 0.0;
                state.is_playing = true;
                if let Some(active_player) = player.as_ref() {
                    load_track_on_player(active_player, &state.current_track, true, 0.0);
                }
            }
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::SelectArtist(artist_id_or_uri) => {
            ensure_fresh_web_token(web_client, live_session).await;
            let detail = web_client
                .fetch_artist_detail(live_session.as_ref(), &artist_id_or_uri)
                .await;
            let _ = event_tx.send(EngineEvent::ArtistDetailLoaded(Box::new(detail)));
        }

        PlayerCommand::PlayAlbum(album_id_or_uri) => {
            ensure_fresh_web_token(web_client, live_session).await;
            let tracks = web_client
                .fetch_album_tracks(live_session.as_ref(), &album_id_or_uri)
                .await;
            if !tracks.is_empty() {
                state.current_track = tracks[0].clone();
                state.queue = tracks;
                state.position_secs = 0.0;
                state.is_playing = true;
                if let Some(active_player) = player.as_ref() {
                    load_track_on_player(active_player, &state.current_track, true, 0.0);
                }
                let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
            }
        }

        PlayerCommand::SelectOutputDevice(device) => {
            if !state.devices.iter().any(|d| d.id == device.id) {
                state.devices.push(device.clone());
            }
            state.selected_device = device.clone();

            if let Some(session) = live_session.as_ref() {
                let device_arg = if device.is_default {
                    None
                } else {
                    Some(device.name.clone())
                };
                let (new_player, new_events) = create_librespot_player(
                    session.clone(),
                    device_arg,
                    sample_producer.clone(),
                    volume_scaler.clone(),
                );
                load_track_on_player(
                    &new_player,
                    &state.current_track,
                    state.is_playing,
                    state.position_secs,
                );
                *player = Some(new_player);
                *player_events = Some(new_events);
            }

            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::AuthenticateSpotify {
            client_id,
            force_reauth,
        } => {
            state.connection = ConnectionStatus::Connecting {
                status: "Opening Spotify login in browser...".to_string(),
            };
            let _ = event_tx.send(EngineEvent::ConnectionUpdated(state.connection.clone()));

            let tx = auth_result_tx.clone();
            tokio::spawn(async move {
                let res = auth::authenticate_and_connect(client_id.as_deref(), force_reauth).await;
                let _ = tx.send(res);
            });
        }

        PlayerCommand::LogoutSpotify => {
            auth::clear_cached_credentials();
            if let Some(active_player) = player.take() {
                active_player.stop();
            }
            *player_events = None;
            *live_session = None;
            web_client.set_access_token(None);
            state.connection = ConnectionStatus::Disconnected { reason: None };
            let _ = event_tx.send(EngineEvent::ConnectionUpdated(state.connection.clone()));
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::SearchCatalog(query) => {
            ensure_fresh_web_token(web_client, live_session).await;
            let results = web_client.search_catalog(&query).await;
            let _ = event_tx.send(EngineEvent::SearchResults {
                query,
                artists: results.artists,
                albums: results.albums,
                tracks: results.tracks,
            });
        }

        PlayerCommand::RefreshDevices => {
            merge_output_devices(state, enumerate_output_devices());
            let _ = event_tx.send(EngineEvent::DevicesUpdated(state.devices.clone()));
        }
    }
}

fn create_librespot_player(
    session: Session,
    device_name: Option<String>,
    sample_producer: AudioSampleProducer,
    volume_scaler: AtomicVolumeScaler,
) -> (Arc<Player>, PlayerEventChannel) {
    let player_config = PlayerConfig {
        bitrate: Bitrate::Bitrate320,
        gapless: true,
        position_update_interval: Some(Duration::from_millis(200)),
        ..PlayerConfig::default()
    };

    let player = Player::new(player_config, session, Box::new(NoOpVolume), move || {
        Box::new(TeeSink::new(
            device_name,
            AudioFormat::F32,
            sample_producer,
            volume_scaler,
        ))
    });
    let events = player.get_player_event_channel();
    (player, events)
}

fn load_track_on_player(player: &Player, track: &Track, start_playing: bool, position_secs: f32) {
    match SpotifyUri::from_uri(&track.spotify_uri) {
        Ok(uri) if uri.is_playable() => {
            let position_ms = (position_secs.max(0.0) * 1000.0) as u32;
            player.load(uri, start_playing, position_ms);
        }
        Ok(_) => {
            warn!("Spotify URI '{}' is not a playable track/episode", track.spotify_uri);
        }
        Err(err) => {
            warn!(
                "Could not parse Spotify URI '{}' for librespot playback: {err}",
                track.spotify_uri
            );
        }
    }
}

fn merge_output_devices(state: &mut PlaybackState, detected: Vec<AudioDevice>) {
    if detected.is_empty() {
        return;
    }

    let current_id = state.selected_device.id.clone();
    state.devices = detected;

    if let Some(existing) = state.devices.iter().find(|d| d.id == current_id).cloned() {
        state.selected_device = existing;
    } else if let Some(default_dev) = state.devices.iter().find(|d| d.is_default).cloned() {
        state.selected_device = default_dev;
    } else if let Some(first) = state.devices.first().cloned() {
        state.selected_device = first;
    }
}

fn advance_queue_track(state: &mut PlaybackState, forward: bool, auto_ended: bool) {
    if state.queue.is_empty() {
        state.position_secs = 0.0;
        return;
    }

    if auto_ended && state.repeat == RepeatMode::Track {
        state.position_secs = 0.0;
        return;
    }

    let len = state.queue.len();
    let current_idx = state
        .queue
        .iter()
        .position(|t| t.id == state.current_track.id)
        .unwrap_or(0);

    let next_idx = if state.shuffle && len > 1 {
        let step = if len > 2 { 2 } else { 1 };
        (current_idx + step) % len
    } else if forward {
        (current_idx + 1) % len
    } else if current_idx == 0 {
        len - 1
    } else {
        current_idx - 1
    };

    state.current_track = state.queue[next_idx].clone();
    state.position_secs = 0.0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use groove_audio::SpectrumAnalyzer;

    /// Helper that waits for the next matching event from `event_rx` (ignoring background `PositionTick`s
    /// when waiting for a specific state event).
    fn recv_until<F, T>(rx: &Receiver<EngineEvent>, mut pred: F) -> T
    where
        F: FnMut(EngineEvent) -> Option<T>,
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if let Ok(evt) = rx.recv_timeout(Duration::from_millis(250)) {
                if let Some(val) = pred(evt) {
                    return val;
                }
            }
        }
        panic!("timed out waiting for expected EngineEvent");
    }

    #[test]
    fn play_pause_commands_toggle_playback_state() {
        let (_analyzer, producer) = SpectrumAnalyzer::new();
        let handle = spawn_engine(producer);

        // Initial StateSynced should arrive immediately with is_playing == true.
        let initial = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) => Some(*s),
            _ => None,
        });
        assert!(initial.is_playing);
        assert_eq!(initial.current_track.id, "higher-ground");

        // TogglePlayPause -> false
        handle
            .command_tx
            .send(PlayerCommand::TogglePlayPause)
            .unwrap();
        let is_playing = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::PlayStateChanged(playing) => Some(playing),
            _ => None,
        });
        assert!(!is_playing);

        // Play -> true
        handle.command_tx.send(PlayerCommand::Play).unwrap();
        let is_playing = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::PlayStateChanged(playing) => Some(playing),
            _ => None,
        });
        assert!(is_playing);

        // Pause -> false
        handle.command_tx.send(PlayerCommand::Pause).unwrap();
        let is_playing = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::PlayStateChanged(playing) => Some(playing),
            _ => None,
        });
        assert!(!is_playing);
    }

    #[test]
    fn next_and_previous_track_commands_navigate_queue() {
        let (_analyzer, producer) = SpectrumAnalyzer::new();
        let handle = spawn_engine(producer);

        // Drain initial StateSynced
        let _ = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) => Some(*s),
            _ => None,
        });

        // Pause first so timer ticks don't race position assertions
        handle.command_tx.send(PlayerCommand::Pause).unwrap();
        let _ = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::PlayStateChanged(playing) => Some(playing),
            _ => None,
        });

        // NextTrack -> "golden-hours" at position 0.0
        handle.command_tx.send(PlayerCommand::NextTrack).unwrap();
        let after_next = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) => Some(*s),
            _ => None,
        });
        assert_eq!(after_next.current_track.id, "golden-hours");
        assert_eq!(after_next.position_secs, 0.0);

        // PreviousTrack -> back to "higher-ground" at position 0.0
        handle
            .command_tx
            .send(PlayerCommand::PreviousTrack)
            .unwrap();
        let after_prev = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) => Some(*s),
            _ => None,
        });
        assert_eq!(after_prev.current_track.id, "higher-ground");
        assert_eq!(after_prev.position_secs, 0.0);
    }

    #[test]
    fn playlist_selection_switches_context_and_loads_curated_tracks() {
        let (_analyzer, producer) = SpectrumAnalyzer::new();
        let handle = spawn_engine(producer);

        // Drain initial StateSynced
        let _ = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) => Some(*s),
            _ => None,
        });

        handle
            .command_tx
            .send(PlayerCommand::SelectPlaylist("deep-work".into()))
            .unwrap();

        let synced = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) => Some(*s),
            _ => None,
        });

        assert_eq!(synced.active_playlist_id.as_deref(), Some("deep-work"));
        assert_eq!(synced.current_track.id, "glass-architecture");
        assert_eq!(synced.position_secs, 0.0);
        assert!(synced.is_playing);
        assert!(
            synced
                .playlists
                .iter()
                .find(|p| p.id == "deep-work")
                .unwrap()
                .is_playing_context
        );
        assert!(
            !synced
                .playlists
                .iter()
                .find(|p| p.id == "chill-focus")
                .unwrap()
                .is_playing_context
        );
    }

    #[test]
    fn seek_seconds_clamps_to_track_duration_and_emits_position_tick() {
        let (_analyzer, producer) = SpectrumAnalyzer::new();
        let handle = spawn_engine(producer);

        let initial = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) => Some(*s),
            _ => None,
        });
        let max_dur = initial.current_track.duration_secs as f32;

        // Pause so only our explicit SeekSeconds emits PositionTick
        handle.command_tx.send(PlayerCommand::Pause).unwrap();
        let _ = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::PlayStateChanged(_) => Some(()),
            _ => None,
        });

        // Seek to 95.5s
        handle
            .command_tx
            .send(PlayerCommand::SeekSeconds(95.5))
            .unwrap();
        let pos = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::PositionTick(p) if (p - 95.5).abs() < 0.05 => Some(p),
            _ => None,
        });
        assert!((pos - 95.5).abs() < 0.01);

        // Seek past track duration -> clamps to duration_secs
        handle
            .command_tx
            .send(PlayerCommand::SeekSeconds(max_dur + 500.0))
            .unwrap();
        let clamped_pos = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::PositionTick(p) if (p - max_dur).abs() < 0.05 => Some(p),
            _ => None,
        });
        assert!((clamped_pos - max_dur).abs() < 0.01);
    }

    #[test]
    fn artist_search_and_detail_fetch_albums() {
        let (_analyzer, producer) = SpectrumAnalyzer::new();
        let handle = spawn_engine(producer);

        let _ = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) => Some(*s),
            _ => None,
        });

        handle
            .command_tx
            .send(PlayerCommand::SearchCatalog("Lume".into()))
            .unwrap();
        let (artists, albums, tracks) = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::SearchResults {
                artists,
                albums,
                tracks,
                ..
            } => Some((artists, albums, tracks)),
            _ => None,
        });
        assert!(!artists.is_empty());
        assert_eq!(artists[0].name, "Lume");
        assert!(!albums.is_empty());
        assert!(!tracks.is_empty());

        handle
            .command_tx
            .send(PlayerCommand::SelectArtist("Lume".into()))
            .unwrap();
        let detail = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::ArtistDetailLoaded(d) => Some(*d),
            _ => None,
        });
        assert_eq!(detail.artist.name, "Lume");
        assert!(!detail.albums.is_empty());
        assert!(!detail.top_tracks.is_empty());
    }
}

