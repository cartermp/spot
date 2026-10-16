pub mod auth;
pub mod sink;
pub mod web_api;

use std::{sync::Arc, time::Duration};

use flume::{Receiver, Sender};
use groove_audio::{enumerate_output_devices, AudioSampleProducer};
use groove_core::{
    advance_queue_verified, merge_devices_verified, resolve_playable_track_candidate,
    sanitize_position_secs, sanitize_unit_float, select_device_verified, AlbumPlaybackQueue,
    AudioDevice, ConnectionStatus, EngineEvent, FormalInvariant, PlaybackContext, PlaybackState,
    PlayerCommand, Track, VerifiedTransportAction,
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

    // Merge real macOS CoreAudio output devices if available (skipped in unit tests to avoid CoreAudio HAL lock contention).
    if !cfg!(test) {
        merge_output_devices(&mut state, enumerate_output_devices());
    }

    // Reflect cached credentials if already stored on disk, and auto-connect in background
    // (disabled during unit tests so tests remain fast, offline, and deterministic).
    let has_cached_creds = !cfg!(test) && auth::cached_credentials().is_some();
    if has_cached_creds {
        info!("[Engine] Found cached Spotify credentials; reconnecting in background...");
        state.connection = ConnectionStatus::Connecting {
            status: "Reconnecting Spotify session...".to_string(),
        };
    } else {
        info!("[Engine] Starting in preview mode (no cached credentials or running tests)");
    }

    let volume_scaler = AtomicVolumeScaler::new(state.volume);
    let mut web_client = SpotifyWebClient::default();
    let mut live_session: Option<Session> = None;
    let mut player: Option<Arc<Player>> = None;
    let mut player_events: Option<PlayerEventChannel> = None;
    let mut player_has_loaded_track = false;
    let mut search_abort_handle: Option<tokio::task::AbortHandle> = None;
    let mut artist_abort_handle: Option<tokio::task::AbortHandle> = None;
    let (auth_result_tx, auth_result_rx) =
        flume::unbounded::<Result<auth::AuthenticatedSession, auth::SpotifyAuthError>>();

    if has_cached_creds {
        let tx = auth_result_tx.clone();
        tokio::spawn(async move {
            match auth::try_connect_cached(None).await {
                Ok(Some(session)) => {
                    let _ = tx.send(Ok(session));
                }
                Ok(None) => {
                    warn!("[Engine] Cached credentials did not yield an active session");
                }
                Err(err) => {
                    warn!("[Engine] Auto-reconnect with cached credentials failed: {err}");
                    let _ = tx.send(Err(err));
                }
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
                    &mut player_has_loaded_track,
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
                    &mut player_has_loaded_track,
                    &mut search_abort_handle,
                    &mut artist_abort_handle,
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
                    &live_session,
                    &mut player_has_loaded_track,
                );
            }

            _ = tick_interval.tick() => {
                if state.is_playing {
                    state.position_secs += 0.1;
                    let duration = state.current_track.duration_secs as f32;
                    if duration > 0.0 && state.position_secs >= duration {
                        advance_queue_track(&mut state, true, true);
                        sync_player_after_advance(
                            &state,
                            &player,
                            &live_session,
                            &mut player_has_loaded_track,
                        );
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
    live_session: &Option<Session>,
    player_has_loaded_track: &mut bool,
) {
    match event {
        PlayerEvent::Playing {
            track_id,
            position_ms,
            ..
        } => {
            info!(
                "[LibrespotPlayer] Event: Playing track={track_id:?} at position_ms={position_ms}"
            );
            state.is_playing = true;
            state.position_secs =
                sanitize_position_secs(position_ms as f32 / 1000.0, state.current_track.duration_secs);
            let _ = event_tx.send(EngineEvent::PlayStateChanged(true));
        }
        PlayerEvent::Paused {
            track_id,
            position_ms,
            ..
        } => {
            info!(
                "[LibrespotPlayer] Event: Paused track={track_id:?} at position_ms={position_ms}"
            );
            state.is_playing = false;
            state.position_secs =
                sanitize_position_secs(position_ms as f32 / 1000.0, state.current_track.duration_secs);
            let _ = event_tx.send(EngineEvent::PlayStateChanged(false));
        }
        PlayerEvent::PositionChanged { position_ms, .. }
        | PlayerEvent::PositionCorrection { position_ms, .. }
        | PlayerEvent::Seeked { position_ms, .. } => {
            state.position_secs =
                sanitize_position_secs(position_ms as f32 / 1000.0, state.current_track.duration_secs);
            let _ = event_tx.send(EngineEvent::PositionTick(state.position_secs));
        }
        PlayerEvent::EndOfTrack { track_id, .. } => {
            info!("[LibrespotPlayer] Event: EndOfTrack ({track_id:?}) -> advancing queue");
            advance_queue_track(state, true, true);
            sync_player_after_advance(state, player, live_session, player_has_loaded_track);
            let _ = event_tx.send(EngineEvent::TrackChanged(state.current_track.clone()));
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }
        PlayerEvent::Loading {
            track_id,
            position_ms,
            ..
        } => {
            info!(
                "[LibrespotPlayer] Event: Loading track={track_id:?} at position_ms={position_ms}"
            );
        }
        PlayerEvent::Unavailable { track_id, .. } => {
            warn!("[LibrespotPlayer] Event: Track unavailable ({track_id:?})");
        }
        other => {
            debug!("[LibrespotPlayer] Event: {other:?}");
        }
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
    player_has_loaded_track: &mut bool,
) {
    match auth_res {
        Ok(auth_session) => {
            info!(
                "[Engine] Applying authenticated Spotify session for user='{}' (has_token={})",
                auth_session.username,
                auth_session.access_token.is_some()
            );
            web_client.set_access_token(auth_session.access_token.clone());
            let username = auth_session.username.clone();
            let is_premium = true;

            let user_playlists = web_client
                .fetch_user_playlists_with_session(Some(&auth_session.session))
                .await;
            if !user_playlists.is_empty() {
                state.playlists = user_playlists;
                let active_pl_id = match &state.context {
                    PlaybackContext::Playlist { playlist_id } => Some(playlist_id.clone()),
                    _ => None,
                };
                for pl in &mut state.playlists {
                    pl.is_playing_context = active_pl_id.as_deref() == Some(pl.id.as_str());
                }
                state.active_playlist_id = active_pl_id;
                let _ = event_tx.send(EngineEvent::PlaylistsLoaded(state.playlists.clone()));
            }

            if let Some(old_player) = player.take() {
                info!("[Engine] Stopping previous librespot Player instance before replacement");
                old_player.stop();
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
            *player_has_loaded_track = false;

            // If the user already selected a track and started playback, load it into the new player.
            if !state.current_track.id.is_empty() && state.is_playing {
                load_track_on_player(
                    &new_player,
                    Some(&auth_session.session),
                    &state.current_track,
                    state.is_playing,
                    state.position_secs,
                );
                *player_has_loaded_track = true;
            }

            *live_session = Some(auth_session.session.clone());
            *player = Some(new_player);
            *player_events = Some(new_events);

            state.connection = ConnectionStatus::Connected {
                username,
                is_premium,
                live_session: true,
            };
            info!("[Engine] Spotify Premium engine connected and player ready");
        }
        Err(err) => {
            warn!("[Engine] Spotify authentication failed: {err}");
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
    player_has_loaded_track: &mut bool,
    search_abort_handle: &mut Option<tokio::task::AbortHandle>,
    artist_abort_handle: &mut Option<tokio::task::AbortHandle>,
    auth_result_tx: &Sender<Result<auth::AuthenticatedSession, auth::SpotifyAuthError>>,
) {
    match cmd {
        PlayerCommand::TogglePlayPause => {
            let was_exhausted = state.is_discography_exhausted();
            let has_loaded = player.is_some() && *player_has_loaded_track;
            state.apply_verified_action(VerifiedTransportAction::TogglePlayPause {
                has_loaded_track: has_loaded,
            });
            info!(
                "[Engine] Command: TogglePlayPause -> is_playing={} (track='{}', uri='{}', was_exhausted={was_exhausted})",
                state.is_playing, state.current_track.title, state.current_track.spotify_uri
            );
            if let Some(active_player) = player.as_ref() {
                if state.is_playing {
                    if !*player_has_loaded_track || was_exhausted {
                        load_track_on_player(
                            active_player,
                            live_session.as_ref(),
                            &state.current_track,
                            true,
                            state.position_secs,
                        );
                        *player_has_loaded_track = true;
                    } else {
                        active_player.play();
                    }
                } else if *player_has_loaded_track {
                    active_player.pause();
                }
            }
            let _ = event_tx.send(EngineEvent::PlayStateChanged(state.is_playing));
            if was_exhausted {
                let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
            }
        }

        PlayerCommand::Play => {
            let was_exhausted = state.is_discography_exhausted();
            state.apply_verified_action(VerifiedTransportAction::Play);
            info!(
                "[Engine] Command: Play (track='{}', uri='{}', was_exhausted={was_exhausted})",
                state.current_track.title, state.current_track.spotify_uri
            );
            if let Some(active_player) = player.as_ref() {
                if !*player_has_loaded_track || was_exhausted {
                    load_track_on_player(
                        active_player,
                        live_session.as_ref(),
                        &state.current_track,
                        true,
                        state.position_secs,
                    );
                    *player_has_loaded_track = true;
                } else {
                    active_player.play();
                }
            }
            let _ = event_tx.send(EngineEvent::PlayStateChanged(true));
            if was_exhausted {
                let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
            }
        }

        PlayerCommand::Pause => {
            state.apply_verified_action(VerifiedTransportAction::Pause);
            info!("[Engine] Command: Pause");
            if let Some(active_player) = player.as_ref() {
                if *player_has_loaded_track {
                    active_player.pause();
                }
            }
            let _ = event_tx.send(EngineEvent::PlayStateChanged(false));
        }

        PlayerCommand::NextTrack => {
            advance_queue_track(state, true, false);
            info!(
                "[Engine] Command: NextTrack -> '{}' by '{}' (uri={}, is_playing={}, exhausted={})",
                state.current_track.title,
                state.current_track.artist,
                state.current_track.spotify_uri,
                state.is_playing,
                state.is_discography_exhausted()
            );
            sync_player_after_advance(state, player, live_session, player_has_loaded_track);
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::PreviousTrack => {
            advance_queue_track(state, false, false);
            info!(
                "[Engine] Command: PreviousTrack -> '{}' by '{}' (uri={}, is_playing={})",
                state.current_track.title,
                state.current_track.artist,
                state.current_track.spotify_uri,
                state.is_playing
            );
            sync_player_after_advance(state, player, live_session, player_has_loaded_track);
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::SeekSeconds(pos) => {
            state.apply_verified_action(VerifiedTransportAction::SeekSeconds(pos));
            let clamped = state.position_secs;
            info!("[Engine] Command: SeekSeconds -> {clamped:.1}s");
            if let Some(active_player) = player.as_ref() {
                active_player.seek((clamped * 1000.0) as u32);
            }
            let _ = event_tx.send(EngineEvent::PositionTick(state.position_secs));
        }

        PlayerCommand::SetVolume(vol) => {
            let clamped = sanitize_unit_float(vol, state.volume);
            state.apply_verified_action(VerifiedTransportAction::SetVolume(clamped));
            volume_scaler.set(clamped);
            info!("[Engine] Command: SetVolume -> {:.0}%", clamped * 100.0);
            if let Some(active_player) = player.as_ref() {
                let raw_vol = (clamped as f64 * u16::MAX as f64).round() as u16;
                active_player.emit_volume_changed_event(raw_vol);
            }
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::ToggleShuffle => {
            state.apply_verified_action(VerifiedTransportAction::ToggleShuffle);
            info!("[Engine] Command: ToggleShuffle -> {}", state.shuffle);
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::CycleRepeat => {
            state.apply_verified_action(VerifiedTransportAction::CycleRepeat);
            info!("[Engine] Command: CycleRepeat -> {:?}", state.repeat);
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::ToggleLikeCurrent => {
            state.apply_verified_action(VerifiedTransportAction::ToggleLikeCurrent);
            info!(
                "[Engine] Command: ToggleLikeCurrent ('{}') -> liked={}",
                state.current_track.title, state.current_track.liked
            );
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::SelectQueueTrack(track_id) => {
            let prev_id = state.current_track.id.clone();
            state.apply_verified_action(VerifiedTransportAction::SelectQueueTrack(track_id.clone()));
            if state.current_track.id == track_id
                || state.current_track.spotify_uri == track_id
                || state.current_track.id != prev_id
            {
                info!(
                    "[Engine] Command: SelectQueueTrack -> '{}' by '{}' ({})",
                    state.current_track.title,
                    state.current_track.artist,
                    state.current_track.spotify_uri
                );
                if let Some(active_player) = player.as_ref() {
                    load_track_on_player(
                        active_player,
                        live_session.as_ref(),
                        &state.current_track,
                        true,
                        0.0,
                    );
                    *player_has_loaded_track = true;
                }
            }
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::PlayTrack(track) => {
            info!(
                "[Engine] Command: PlayTrack -> '{}' by '{}' (uri={}, cover={:?})",
                track.title, track.artist, track.spotify_uri, track.cover_art_url
            );
            let was_in_active_context = matches!(
                state.context,
                PlaybackContext::Playlist { .. }
                    | PlaybackContext::Album { .. }
                    | PlaybackContext::PodcastShow { .. }
            ) && state.queue.iter().any(|t| {
                t.id == track.id
                    || (!track.spotify_uri.is_empty() && t.spotify_uri == track.spotify_uri)
            });

            state.apply_verified_action(VerifiedTransportAction::PlayTrack(track.clone()));
            if let Some(active_player) = player.as_ref() {
                load_track_on_player(
                    active_player,
                    live_session.as_ref(),
                    &state.current_track,
                    true,
                    0.0,
                );
                *player_has_loaded_track = true;
            }
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));

            // When playing a track from Search or Top Tracks on a live session outside the
            // current context, hydrate its parent album into `Up Next` and load the artist's
            // discography so advancing moves through the album and then the artist's albums.
            if !was_in_active_context
                && live_session.is_some()
                && !track.spotify_uri.starts_with("spotify:episode:")
            {
                if let Some(parent_album_uri) = web_client
                    .resolve_track_parent_album_uri(live_session.as_ref(), &track)
                    .await
                {
                    let detail = web_client
                        .fetch_album_detail(live_session.as_ref(), &parent_album_uri)
                        .await;
                    if !detail.tracks.is_empty() {
                        let start_idx = detail
                            .tracks
                            .iter()
                            .position(|t| {
                                t.id == track.id
                                    || (!track.spotify_uri.is_empty()
                                        && t.spotify_uri == track.spotify_uri)
                                    || t.title.eq_ignore_ascii_case(&track.title)
                            })
                            .unwrap_or(0);
                        let active_album = AlbumPlaybackQueue {
                            album_uri: detail.album.spotify_uri.clone(),
                            album_title: detail.album.title.clone(),
                            artist_name: detail.album.artist_name.clone(),
                            tracks: detail.tracks.clone(),
                        };
                        state.apply_verified_action(VerifiedTransportAction::ActivateAlbum {
                            album: active_album.clone(),
                            artist_uri: detail.artist_uri.clone(),
                            artist_albums: vec![active_album.clone()],
                            start_track_index: start_idx,
                            auto_play: true,
                        });
                        let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));

                        let artist_lookup = detail
                            .artist_uri
                            .as_deref()
                            .unwrap_or(&detail.album.artist_name);
                        let discography = web_client
                            .fetch_artist_discography_queues(
                                live_session.as_ref(),
                                artist_lookup,
                                &active_album,
                            )
                            .await;
                        if !discography.is_empty() {
                            state.apply_verified_action(
                                VerifiedTransportAction::SetArtistDiscography {
                                    artist_name: detail.album.artist_name.clone(),
                                    artist_uri: detail.artist_uri.clone(),
                                    albums: discography,
                                },
                            );
                            let _ =
                                event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
                        }
                    }
                }
            }
        }

        PlayerCommand::SelectPlaylist(playlist_id) => {
            info!("[Engine] Command: SelectPlaylist('{playlist_id}')");
            ensure_fresh_web_token(web_client, live_session).await;
            let resolved_key = state
                .playlists
                .iter()
                .find(|p| {
                    p.id == playlist_id
                        || p.spotify_uri == playlist_id
                        || p.name.eq_ignore_ascii_case(&playlist_id)
                })
                .map(|p| p.id.clone())
                .unwrap_or_else(|| playlist_id.clone());

            let tracks = web_client
                .fetch_playlist_tracks_with_session(live_session.as_ref(), &resolved_key)
                .await;
            let had_tracks = !tracks.is_empty();
            state.apply_verified_action(VerifiedTransportAction::ActivatePlaylist {
                playlist_id: resolved_key,
                tracks,
                auto_play: true,
            });
            if had_tracks {
                if let Some(active_player) = player.as_ref() {
                    load_track_on_player(
                        active_player,
                        live_session.as_ref(),
                        &state.current_track,
                        true,
                        0.0,
                    );
                    *player_has_loaded_track = true;
                }
            }
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::SelectArtist(artist_id_or_uri) => {
            info!("[Engine] Command: SelectArtist('{artist_id_or_uri}')");
            if let Some(prev) = artist_abort_handle.take() {
                prev.abort();
            }
            let client = web_client.clone();
            let sess = live_session.clone();
            let tx = event_tx.clone();
            let jh = tokio::spawn(async move {
                let detail = client
                    .fetch_artist_detail(sess.as_ref(), &artist_id_or_uri)
                    .await;
                info!(
                    "[Engine] ArtistDetailLoaded for '{}' ({} albums, {} top tracks)",
                    detail.artist.name,
                    detail.albums.len(),
                    detail.top_tracks.len()
                );
                let _ = tx.send(EngineEvent::ArtistDetailLoaded(Box::new(detail)));
            });
            *artist_abort_handle = Some(jh.abort_handle());
        }

        PlayerCommand::SelectAlbum(album_id_or_uri) => {
            info!("[Engine] Command: SelectAlbum('{album_id_or_uri}')");
            let client = web_client.clone();
            let sess = live_session.clone();
            let tx = event_tx.clone();
            tokio::spawn(async move {
                let detail = client
                    .fetch_album_detail(sess.as_ref(), &album_id_or_uri)
                    .await;
                info!(
                    "[Engine] AlbumDetailLoaded for '{}' by '{}' ({} tracks, artist_uri={:?})",
                    detail.album.title,
                    detail.album.artist_name,
                    detail.tracks.len(),
                    detail.artist_uri
                );
                let _ = tx.send(EngineEvent::AlbumDetailLoaded(Box::new(detail)));
            });
        }

        PlayerCommand::PlayAlbum(album_id_or_uri) => {
            info!("[Engine] Command: PlayAlbum('{album_id_or_uri}')");
            activate_album_playback(
                &album_id_or_uri,
                0,
                None,
                state,
                event_tx,
                web_client,
                live_session,
                player,
                player_has_loaded_track,
            )
            .await;
        }

        PlayerCommand::PlayAlbumAtTrack {
            album_uri,
            track_index,
            preloaded_album,
        } => {
            info!(
                "[Engine] Command: PlayAlbumAtTrack(album='{}', track_index={})",
                album_uri, track_index
            );
            activate_album_playback(
                &album_uri,
                track_index,
                preloaded_album,
                state,
                event_tx,
                web_client,
                live_session,
                player,
                player_has_loaded_track,
            )
            .await;
        }

        PlayerCommand::SelectShow(show_id_or_uri) => {
            info!("[Engine] Command: SelectShow('{show_id_or_uri}')");
            let client = web_client.clone();
            let sess = live_session.clone();
            let tx = event_tx.clone();
            tokio::spawn(async move {
                let detail = client
                    .fetch_show_detail(sess.as_ref(), &show_id_or_uri)
                    .await;
                info!(
                    "[Engine] ShowDetailLoaded for '{}' by '{}' ({} episodes)",
                    detail.show.title,
                    detail.show.publisher,
                    detail.episodes.len()
                );
                let _ = tx.send(EngineEvent::ShowDetailLoaded(Box::new(detail)));
            });
        }

        PlayerCommand::PlayShow(show_id_or_uri) => {
            info!("[Engine] Command: PlayShow('{show_id_or_uri}')");
            activate_show_playback(
                &show_id_or_uri,
                0,
                None,
                state,
                event_tx,
                web_client,
                live_session,
                player,
                player_has_loaded_track,
            )
            .await;
        }

        PlayerCommand::PlayShowAtEpisode {
            show_uri,
            episode_index,
            preloaded_show,
        } => {
            info!(
                "[Engine] Command: PlayShowAtEpisode(show='{}', episode_index={})",
                show_uri, episode_index
            );
            activate_show_playback(
                &show_uri,
                episode_index,
                preloaded_show,
                state,
                event_tx,
                web_client,
                live_session,
                player,
                player_has_loaded_track,
            )
            .await;
        }

        PlayerCommand::SelectOutputDevice(device) => {
            info!(
                "[Engine] Command: SelectOutputDevice -> '{}' (id={}, is_default={})",
                device.name, device.id, device.is_default
            );
            select_device_verified(state, device.clone());

            if let Some(session) = live_session.as_ref() {
                if let Some(old_player) = player.take() {
                    old_player.stop();
                }
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
                if *player_has_loaded_track || state.current_track.id != "higher-ground" {
                    load_track_on_player(
                        &new_player,
                        Some(session),
                        &state.current_track,
                        state.is_playing,
                        state.position_secs,
                    );
                    *player_has_loaded_track = true;
                }
                *player = Some(new_player);
                *player_events = Some(new_events);
            }

            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::AuthenticateSpotify {
            client_id,
            force_reauth,
        } => {
            info!(
                "[Engine] Command: AuthenticateSpotify (force_reauth={force_reauth}, custom_client_id={})",
                client_id.is_some()
            );
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
            info!("[Engine] Command: LogoutSpotify -> clearing credentials and stopping player");
            auth::clear_cached_credentials();
            if let Some(active_player) = player.take() {
                active_player.stop();
            }
            *player_events = None;
            *live_session = None;
            *player_has_loaded_track = false;
            web_client.set_access_token(None);
            state.connection = ConnectionStatus::Disconnected { reason: None };
            let _ = event_tx.send(EngineEvent::ConnectionUpdated(state.connection.clone()));
            let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
        }

        PlayerCommand::SearchCatalog(query) => {
            if let Some(prev) = search_abort_handle.take() {
                info!(
                    "[Engine] Cancelling previous in-flight search task; new query='{}'",
                    query
                );
                prev.abort();
            }

            let client = web_client.clone();
            let sess = live_session.clone();
            let tx = event_tx.clone();
            let jh = tokio::spawn(async move {
                // Debounce live network search by 140ms when typing non-empty queries on a live session
                if sess.is_some() && !query.trim().is_empty() {
                    tokio::time::sleep(Duration::from_millis(140)).await;
                }
                info!(
                    "[Engine] Running SearchCatalog for query='{}' (live_session={})",
                    query,
                    sess.is_some()
                );
                let results = client
                    .search_catalog_with_session(sess.as_ref(), &query)
                    .await;
                info!(
                    "[Engine] SearchResults ready for query='{}': {} artists, {} albums, {} shows, {} episodes, {} tracks",
                    query,
                    results.artists.len(),
                    results.albums.len(),
                    results.shows.len(),
                    results.episodes.len(),
                    results.tracks.len()
                );
                let _ = tx.send(EngineEvent::SearchResults {
                    query,
                    artists: results.artists,
                    albums: results.albums,
                    shows: results.shows,
                    episodes: results.episodes,
                    tracks: results.tracks,
                });
            });
            *search_abort_handle = Some(jh.abort_handle());
        }

        PlayerCommand::RefreshDevices => {
            info!("[Engine] Command: RefreshDevices");
            merge_output_devices(state, enumerate_output_devices());
            let _ = event_tx.send(EngineEvent::DevicesUpdated(state.devices.clone()));
        }
    }
}

fn sync_player_after_advance(
    state: &PlaybackState,
    player: &Option<Arc<Player>>,
    live_session: &Option<Session>,
    player_has_loaded_track: &mut bool,
) {
    if let Some(active_player) = player.as_ref() {
        if state.is_discography_exhausted() || (!state.is_playing && state.position_secs == 0.0) {
            if state.is_discography_exhausted() {
                info!(
                    "[Engine] Artist discography exhausted -> stopping playback (playing nothing)"
                );
                active_player.stop();
                *player_has_loaded_track = false;
            } else {
                load_track_on_player(
                    active_player,
                    live_session.as_ref(),
                    &state.current_track,
                    false,
                    0.0,
                );
                *player_has_loaded_track = true;
            }
        } else {
            load_track_on_player(
                active_player,
                live_session.as_ref(),
                &state.current_track,
                state.is_playing,
                0.0,
            );
            *player_has_loaded_track = true;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn activate_album_playback(
    album_id_or_uri: &str,
    start_track_index: usize,
    preloaded_album: Option<groove_core::AlbumDetail>,
    state: &mut PlaybackState,
    event_tx: &Sender<EngineEvent>,
    web_client: &mut SpotifyWebClient,
    live_session: &mut Option<Session>,
    player: &mut Option<Arc<Player>>,
    player_has_loaded_track: &mut bool,
) {
    let detail = match preloaded_album.filter(|d| !d.tracks.is_empty()) {
        Some(preloaded) => preloaded,
        None => {
            ensure_fresh_web_token(web_client, live_session).await;
            let fetched = web_client
                .fetch_album_detail(live_session.as_ref(), album_id_or_uri)
                .await;
            let _ = event_tx.send(EngineEvent::AlbumDetailLoaded(Box::new(fetched.clone())));
            fetched
        }
    };
    let tracks = detail.tracks.clone();

    if !tracks.is_empty() {
        let clamped_idx = start_track_index.min(tracks.len() - 1);
        let active_album = AlbumPlaybackQueue {
            album_uri: detail.album.spotify_uri.clone(),
            album_title: detail.album.title.clone(),
            artist_name: detail.album.artist_name.clone(),
            tracks: tracks.clone(),
        };

        state.apply_verified_action(VerifiedTransportAction::ActivateAlbum {
            album: active_album.clone(),
            artist_uri: detail.artist_uri.clone(),
            artist_albums: vec![active_album.clone()],
            start_track_index: clamped_idx,
            auto_play: true,
        });

        info!(
            "[Engine] Activated album '{}' by '{}' ({} tracks, starting at index {} '{}')",
            detail.album.title,
            detail.album.artist_name,
            tracks.len(),
            clamped_idx,
            state.current_track.title
        );

        if let Some(active_player) = player.as_ref() {
            load_track_on_player(
                active_player,
                live_session.as_ref(),
                &state.current_track,
                true,
                0.0,
            );
            *player_has_loaded_track = true;
        }
        let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));

        // Fetch the artist's ordered discography so advancing past the end of this album
        // automatically plays the next album by this artist, and stops once all albums finish.
        if live_session.is_some() {
            let artist_lookup = detail
                .artist_uri
                .as_deref()
                .unwrap_or(&detail.album.artist_name);
            let discography = web_client
                .fetch_artist_discography_queues(
                    live_session.as_ref(),
                    artist_lookup,
                    &active_album,
                )
                .await;
            if !discography.is_empty() {
                let total_albums = discography.len();
                state.apply_verified_action(VerifiedTransportAction::SetArtistDiscography {
                    artist_name: detail.album.artist_name.clone(),
                    artist_uri: detail.artist_uri.clone(),
                    albums: discography,
                });
                info!(
                    "[Engine] Loaded artist discography for '{}' ({total_albums} albums queued for sequential progression)",
                    detail.album.artist_name
                );
                let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn activate_show_playback(
    show_id_or_uri: &str,
    start_episode_index: usize,
    preloaded_show: Option<groove_core::PodcastShowDetail>,
    state: &mut PlaybackState,
    event_tx: &Sender<EngineEvent>,
    web_client: &mut SpotifyWebClient,
    live_session: &mut Option<Session>,
    player: &mut Option<Arc<Player>>,
    player_has_loaded_track: &mut bool,
) {
    let detail = match preloaded_show.filter(|d| !d.episodes.is_empty()) {
        Some(preloaded) => preloaded,
        None => {
            let fetched = web_client
                .fetch_show_detail(live_session.as_ref(), show_id_or_uri)
                .await;
            let _ = event_tx.send(EngineEvent::ShowDetailLoaded(Box::new(fetched.clone())));
            fetched
        }
    };
    let tracks: Vec<Track> = detail
        .episodes
        .iter()
        .map(|ep| ep.into_playable_track())
        .collect();
    let show_uri = detail.show.spotify_uri.clone();
    let show_title = detail.show.title.clone();

    if !tracks.is_empty() {
        let clamped_idx = start_episode_index.min(tracks.len() - 1);
        state.apply_verified_action(VerifiedTransportAction::ActivatePodcastShow {
            show_uri,
            show_title: show_title.clone(),
            episodes: tracks.clone(),
            start_episode_index: clamped_idx,
            auto_play: true,
        });

        info!(
            "[Engine] Activated podcast show '{}' ({} episodes, starting at index {} '{}')",
            show_title,
            tracks.len(),
            clamped_idx,
            state.current_track.title
        );

        if let Some(active_player) = player.as_ref() {
            load_track_on_player(
                active_player,
                live_session.as_ref(),
                &state.current_track,
                true,
                0.0,
            );
            *player_has_loaded_track = true;
        }
        let _ = event_tx.send(EngineEvent::StateSynced(Box::new(state.clone())));
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

fn load_track_on_player(
    player: &Arc<Player>,
    session: Option<&Session>,
    track: &Track,
    start_playing: bool,
    position_secs: f32,
) {
    match SpotifyUri::from_uri(&track.spotify_uri) {
        Ok(uri) if uri.is_playable() => {
            let position_ms = (position_secs.max(0.0) * 1000.0) as u32;
            let player_clone = Arc::clone(player);
            let sess_clone = session.cloned();
            let title = track.title.clone();
            let raw_uri_str = track.spotify_uri.clone();

            tokio::spawn(async move {
                let mut resolved_uri = uri;
                if let Some(sess) = sess_clone {
                    use librespot_metadata::{
                        Episode as LibrespotEpisode, Metadata, Track as LibrespotTrack,
                    };
                    if matches!(resolved_uri, SpotifyUri::Episode { .. }) {
                        if let Ok(meta_ep) = LibrespotEpisode::get(&sess, &resolved_uri).await {
                            info!(
                                "[LibrespotPlayer] Podcast episode '{}' ({}) verified with {} audio files",
                                title,
                                raw_uri_str,
                                meta_ep.audio.0.len()
                            );
                        }
                    } else if let Ok(meta) = LibrespotTrack::get(&sess, &resolved_uri).await {
                        let primary_files = meta.files.len();
                        let mut alt_candidates: Vec<(String, SpotifyUri, usize)> = Vec::new();
                        if primary_files == 0 {
                            for alt_uri in meta.alternatives.iter() {
                                if let Ok(alt_meta) = LibrespotTrack::get(&sess, alt_uri).await {
                                    alt_candidates.push((
                                        format!("{alt_uri:?}"),
                                        alt_uri.clone(),
                                        alt_meta.files.len(),
                                    ));
                                    if !alt_meta.files.is_empty() {
                                        break;
                                    }
                                }
                            }
                        }
                        let chosen = {
                            let alt_refs: Vec<(&str, usize)> = alt_candidates
                                .iter()
                                .map(|(s, _, count)| (s.as_str(), *count))
                                .collect();
                            resolve_playable_track_candidate(&raw_uri_str, primary_files, &alt_refs)
                                .map(|(label, files)| (label.to_string(), files))
                        };
                        if let Some((chosen_label, chosen_files)) = chosen {
                            if primary_files == 0 {
                                if let Some((_, chosen_uri, _)) = alt_candidates
                                    .into_iter()
                                    .find(|(label, _, _)| *label == chosen_label)
                                {
                                    info!(
                                        "[LibrespotPlayer] Track '{}' ({}) had 0 direct files; resolved to playable alternative {:?} ({} files)",
                                        title, raw_uri_str, chosen_uri, chosen_files
                                    );
                                    resolved_uri = chosen_uri;
                                }
                            } else {
                                info!(
                                    "[LibrespotPlayer] Track '{}' ({}) verified with {} direct audio files",
                                    title, raw_uri_str, chosen_files
                                );
                            }
                        }
                    }
                }
                info!(
                    "[LibrespotPlayer] Loading track '{}' ({:?}) start_playing={} position_ms={}",
                    title, resolved_uri, start_playing, position_ms
                );
                player_clone.load(resolved_uri, start_playing, position_ms);
            });
        }
        Ok(_) => {
            warn!(
                "Spotify URI '{}' is not a playable track/episode",
                track.spotify_uri
            );
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
    merge_devices_verified(state, detected);
    debug_assert!(state.check_invariants().is_ok());
    info!(
        "[Audio] Active output device selected: '{}' (id={}, is_default={})",
        state.selected_device.name, state.selected_device.id, state.selected_device.is_default
    );
}

fn advance_queue_track(state: &mut PlaybackState, forward: bool, auto_ended: bool) {
    advance_queue_verified(state, forward, auto_ended);
    debug_assert!(state.check_invariants().is_ok());
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

    fn sample_track(id: &str, title: &str, duration_secs: u32) -> Track {
        Track {
            id: id.into(),
            spotify_uri: format!("spotify:track:{id}"),
            title: title.into(),
            artist: "Test Artist".into(),
            album: "Test Album".into(),
            duration_secs,
            liked: false,
            cover_art_url: None,
            artwork_theme: groove_core::ArtworkTheme::SunriseMountainLake,
        }
    }

    #[test]
    fn play_pause_commands_toggle_playback_state() {
        let (_analyzer, producer) = SpectrumAnalyzer::new();
        let handle = spawn_engine(producer);

        // Initial StateSynced should arrive immediately with is_playing == false and empty queue.
        let initial = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) => Some(*s),
            _ => None,
        });
        assert!(!initial.is_playing);
        assert!(initial.queue.is_empty());

        // Play -> true
        handle.command_tx.send(PlayerCommand::Play).unwrap();
        let is_playing = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::PlayStateChanged(playing) => Some(playing),
            _ => None,
        });
        assert!(is_playing);

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
    }

    #[test]
    fn next_and_previous_track_commands_navigate_queue() {
        let (_analyzer, producer) = SpectrumAnalyzer::new();
        let handle = spawn_engine(producer);

        let _ = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) => Some(*s),
            _ => None,
        });

        let album_one = groove_core::AlbumDetail {
            album: groove_core::AlbumSummary {
                id: "rtj2".into(),
                spotify_uri: "spotify:album:rtj2".into(),
                title: "Run The Jewels 2".into(),
                artist_name: "Run The Jewels".into(),
                release_year: "2014".into(),
                album_type: "Album".into(),
                track_count: 2,
                cover_art_url: None,
                artwork_theme: groove_core::ArtworkTheme::SunriseMountainLake,
            },
            artist_uri: Some("spotify:artist:rtj".into()),
            tracks: vec![
                sample_track("t1", "Jeopardy", 201),
                sample_track("t2", "Oh My Darling Don't Cry", 204),
            ],
        };

        handle
            .command_tx
            .send(PlayerCommand::PlayAlbumAtTrack {
                album_uri: "spotify:album:rtj2".into(),
                track_index: 0,
                preloaded_album: Some(album_one),
            })
            .unwrap();
        let after_start = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) if s.current_track.id == "t1" => Some(*s),
            _ => None,
        });
        assert_eq!(after_start.queue.len(), 2);
        assert!(after_start.active_playlist_id.is_none());
        assert!(after_start.check_invariants().is_ok());

        // NextTrack moves to track 2 within the same album
        handle.command_tx.send(PlayerCommand::NextTrack).unwrap();
        let after_next = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) if s.current_track.id == "t2" => Some(*s),
            _ => None,
        });
        assert_eq!(after_next.current_track.id, "t2");
        assert_eq!(after_next.position_secs, 0.0);
        assert!(after_next.is_playing);
        assert!(after_next.check_invariants().is_ok());

        // PreviousTrack moves back to track 1 within the same album
        handle
            .command_tx
            .send(PlayerCommand::PreviousTrack)
            .unwrap();
        let after_prev = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) if s.current_track.id == "t1" => Some(*s),
            _ => None,
        });
        assert_eq!(after_prev.current_track.id, "t1");
        assert_eq!(after_prev.position_secs, 0.0);

        // Advance to end of album (t1 -> t2 -> exhausted silence when single album in discography)
        handle.command_tx.send(PlayerCommand::NextTrack).unwrap();
        let _ = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) if s.current_track.id == "t2" => Some(*s),
            _ => None,
        });
        handle.command_tx.send(PlayerCommand::NextTrack).unwrap();
        let after_exhausted = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) if !s.is_playing => Some(*s),
            _ => None,
        });
        assert!(!after_exhausted.is_playing);
        assert!(after_exhausted.is_discography_exhausted());
        assert!(after_exhausted.check_invariants().is_ok());
    }

    #[test]
    fn playlist_selection_switches_context() {
        let (_analyzer, producer) = SpectrumAnalyzer::new();
        let handle = spawn_engine(producer);

        let _ = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) => Some(*s),
            _ => None,
        });

        handle
            .command_tx
            .send(PlayerCommand::SelectPlaylist("custom-playlist".into()))
            .unwrap();

        let synced = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) => Some(*s),
            _ => None,
        });

        assert_eq!(synced.active_playlist_id.as_deref(), Some("custom-playlist"));
    }

    #[test]
    fn seek_seconds_clamps_to_track_duration_and_emits_position_tick() {
        let (_analyzer, producer) = SpectrumAnalyzer::new();
        let handle = spawn_engine(producer);

        let _ = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) => Some(*s),
            _ => None,
        });

        handle
            .command_tx
            .send(PlayerCommand::PlayTrack(sample_track("t1", "Track One", 240)))
            .unwrap();
        let _ = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) if s.current_track.id == "t1" => Some(*s),
            _ => None,
        });

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

        // Seek past track duration -> clamps to 240.0
        handle
            .command_tx
            .send(PlayerCommand::SeekSeconds(900.0))
            .unwrap();
        let clamped_pos = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::PositionTick(p) if (p - 240.0).abs() < 0.05 => Some(p),
            _ => None,
        });
        assert!((clamped_pos - 240.0).abs() < 0.01);
    }

    #[test]
    fn artist_detail_fallback_loads_cleanly() {
        let (_analyzer, producer) = SpectrumAnalyzer::new();
        let handle = spawn_engine(producer);

        let _ = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::StateSynced(s) => Some(*s),
            _ => None,
        });

        handle
            .command_tx
            .send(PlayerCommand::SelectArtist("Killer Mike".into()))
            .unwrap();
        let detail = recv_until(&handle.event_rx, |evt| match evt {
            EngineEvent::ArtistDetailLoaded(d) => Some(*d),
            _ => None,
        });
        assert_eq!(detail.artist.name, "Killer Mike");
    }
}

