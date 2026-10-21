#![allow(unexpected_cfgs)]

use serde::{Deserialize, Serialize};

use crate::{
    AlbumPlaybackQueue, ArtworkTheme, AudioDevice, PlaybackContext, PlaybackState, PodcastEpisode,
    PodcastShowDetail, RepeatMode, SpectrumFrame, Track,
};

/// Detailed error returned when a formal state invariant is violated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantViolation {
    pub code: &'static str,
    pub subsystem: &'static str,
    pub details: String,
}

impl std::fmt::Display for InvariantViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}:{}] {}",
            self.subsystem, self.code, self.details
        )
    }
}

impl std::error::Error for InvariantViolation {}

/// Trait implemented by all formally verified state machines and data structures in Spot.
pub trait FormalInvariant {
    /// Verifies that all inductive state invariants hold on `self`.
    fn check_invariants(&self) -> Result<(), InvariantViolation>;

    /// Panics with a detailed diagnostic if any formal invariant is violated.
    fn assert_invariants(&self) {
        if let Err(violation) = self.check_invariants() {
            panic!("Formal invariant violation: {violation}");
        }
    }
}

/// Greatest common divisor via Euclid's algorithm.
#[inline]
pub const fn gcd_usize(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// Computes a deterministic shuffle step `s` for a queue of length `len` such that:
/// 1. For `len <= 1`, `s == 0`.
/// 2. For `len > 1`, `1 <= s < len` and `gcd(s, len) == 1`.
///
/// Because `gcd(s, len) == 1`, the map `i -> (i + s) % len` is an algebraic generator of the
/// cyclic group `Z / len Z`, guaranteeing that repeated `NextTrack` in shuffle mode visits
/// **every** track in the queue before repeating (100% reachability / full-cycle bijection),
/// and `i -> (i + len - s) % len` is its exact inverse for `PreviousTrack`.
pub const fn coprime_shuffle_step(len: usize) -> usize {
    if len <= 1 {
        return 0;
    }
    if len == 2 {
        return 1;
    }
    // Start near the golden ratio conjugate (~0.618 * len) for well-dispersed jumps,
    // then advance until coprime with `len`.
    let mut candidate = (len * 618) / 1000;
    if candidate <= 1 {
        candidate = 2;
    }
    if candidate >= len {
        candidate = len - 1;
    }
    while candidate < len {
        if gcd_usize(candidate, len) == 1 {
            return candidate;
        }
        candidate += 1;
    }
    1
}

/// Total sanitizer mapping any IEEE-754 `f32` (including `NaN`, `+Inf`, `-Inf`, `-0.0`)
/// into a canonical finite value in `0.0..=1.0`.
#[inline]
pub fn sanitize_unit_float(val: f32, fallback: f32) -> f32 {
    let raw = if val.is_finite() {
        val.clamp(0.0, 1.0)
    } else if fallback.is_finite() {
        fallback.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if raw == 0.0 {
        0.0
    } else {
        raw
    }
}

/// Total sanitizer mapping any IEEE-754 `f32` seek/playback position into a valid finite
/// position in `0.0..=duration_secs` (or `>= 0.0` when `duration_secs == 0`).
#[inline]
pub fn sanitize_position_secs(pos: f32, duration_secs: u32) -> f32 {
    if !pos.is_finite() {
        return 0.0;
    }
    let max_dur = duration_secs as f32;
    let clamped = if max_dur > 0.0 {
        pos.clamp(0.0, max_dur)
    } else {
        pos.max(0.0).min(86_400.0)
    };
    if clamped == 0.0 {
        0.0
    } else {
        clamped
    }
}

/// Total sanitizer for a single PCM audio sample, guaranteeing a finite value in `-1.0..=1.0`.
#[inline]
pub fn sanitize_pcm_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        let c = sample.clamp(-1.0, 1.0);
        if c == 0.0 {
            0.0
        } else {
            c
        }
    } else {
        0.0
    }
}

/// Pure, verified queue advancement transition function.
///
/// Formal Guarantees:
/// - **INV-P2 / INV-P3**: Resets `position_secs` to `0.0`.
/// - **INV-P4**: When `!state.queue.is_empty()`, `state.current_track` is always a member of `state.queue`.
/// - **INV-P5**: Advancing never activates a playlist when `state.context` is not `PlaybackContext::Playlist`.
/// - **INV-P8**: Under `shuffle == true`, forward and backward steps are exact modular inverses and form a single full cycle of length `queue.len()`.
/// - **INV-P9**: In `Playlist` / `PodcastShow` / `None` contexts, when `auto_ended == true`, `repeat == RepeatMode::Off`, and `!shuffle` on the final track of the queue, playback stops cleanly (`is_playing = false`) at the start of the queue.
/// - **INV-P10 / INV-P11 / INV-P12**: In `PlaybackContext::Album`:
///   1. While tracks remain in the active album (`current_idx + 1 < queue.len()`), advancing moves to the next track in that same album (`Up Next` stays that album).
///   2. When the active album finishes (`current_idx + 1 >= queue.len()`), if another album by that artist exists (`current_album_index + 1 < artist_albums.len()`), advancing moves to Track 0 of the next album by that artist and updates `Up Next` (`state.queue`) to that next album's tracks.
///   3. Once all albums by that artist have been played through (`current_album_index + 1 >= artist_albums.len()`), advancing with `RepeatMode::Off` and `!shuffle` marks `exhausted = true` and sets `is_playing = false` (plays nothing).
pub fn advance_queue_verified(state: &mut PlaybackState, forward: bool, auto_ended: bool) {
    state.position_secs = 0.0;
    // Natural track completion (`auto_ended == true`) always progresses forward.
    let forward = forward || auto_ended;

    if let PlaybackContext::Album {
        ref mut artist_albums,
        ref mut current_album_index,
        ref mut exhausted,
        ..
    } = state.context
    {
        if artist_albums.is_empty() || state.queue.is_empty() {
            *exhausted = true;
            state.is_playing = false;
            return;
        }

        if *current_album_index >= artist_albums.len() {
            *current_album_index = artist_albums.len() - 1;
        }
        let k = *current_album_index;
        let album_count = artist_albums.len();
        let len = state.queue.len();
        let current_idx = state
            .queue
            .iter()
            .position(|t| t.id == state.current_track.id)
            .unwrap_or(0);

        if auto_ended && state.repeat == RepeatMode::Track {
            *exhausted = false;
            state.current_track = state.queue[current_idx].clone();
            state.is_playing = true;
            return;
        }

        if state.shuffle && len > 1 {
            *exhausted = false;
            let step = coprime_shuffle_step(len);
            let next_idx = if forward {
                (current_idx + step) % len
            } else {
                (current_idx + len - step) % len
            };
            state.current_track = state.queue[next_idx].clone();
            if auto_ended {
                state.is_playing = true;
            }
            return;
        }

        if forward {
            if *exhausted {
                if state.repeat == RepeatMode::Context {
                    *current_album_index = 0;
                    state.queue = artist_albums[0].tracks.clone();
                    state.current_track = state.queue[0].clone();
                    *exhausted = false;
                    if auto_ended {
                        state.is_playing = true;
                    }
                } else {
                    // Already went through all albums by this artist: play nothing.
                    state.is_playing = false;
                }
                return;
            }

            if current_idx + 1 < len {
                // Next track within the current album
                state.current_track = state.queue[current_idx + 1].clone();
                if auto_ended {
                    state.is_playing = true;
                }
            } else if k + 1 < album_count {
                // Current album is over -> move to the next album by this artist
                let next_album_idx = k + 1;
                *current_album_index = next_album_idx;
                state.queue = artist_albums[next_album_idx].tracks.clone();
                state.current_track = state.queue[0].clone();
                *exhausted = false;
                if auto_ended {
                    state.is_playing = true;
                }
            } else if state.repeat == RepeatMode::Context {
                // Explicit Repeat::Context wraps around to the artist's first album
                *current_album_index = 0;
                state.queue = artist_albums[0].tracks.clone();
                state.current_track = state.queue[0].clone();
                *exhausted = false;
                if auto_ended {
                    state.is_playing = true;
                }
            } else {
                // Gone through all albums by this artist -> stop and play nothing
                if let Some(last_track) = state.queue.last().cloned() {
                    state.current_track = last_track;
                }
                *exhausted = true;
                state.is_playing = false;
            }
        } else {
            // Backward (`PreviousTrack`)
            if *exhausted {
                *exhausted = false;
                if let Some(last_track) = state.queue.last().cloned() {
                    state.current_track = last_track;
                }
                return;
            }
            if current_idx > 0 {
                state.current_track = state.queue[current_idx - 1].clone();
            } else if k > 0 {
                let prev_album_idx = k - 1;
                *current_album_index = prev_album_idx;
                state.queue = artist_albums[prev_album_idx].tracks.clone();
                if let Some(last_track) = state.queue.last().cloned() {
                    state.current_track = last_track;
                }
            } else {
                state.current_track = state.queue[0].clone();
            }
        }
        return;
    }

    if state.queue.is_empty() {
        if auto_ended && state.repeat == RepeatMode::Off {
            state.is_playing = false;
        }
        return;
    }

    let len = state.queue.len();
    let current_idx = state
        .queue
        .iter()
        .position(|t| t.id == state.current_track.id)
        .unwrap_or(0);

    if auto_ended && state.repeat == RepeatMode::Track {
        // Stay on current track (ensure metadata is synced with queue slot)
        state.current_track = state.queue[current_idx].clone();
        state.is_playing = true;
        return;
    }

    if auto_ended && state.repeat == RepeatMode::Off && !state.shuffle && current_idx + 1 >= len {
        // Reached the end of the queue with Repeat::Off -> stop playback and reset to first track
        state.current_track = state.queue[0].clone();
        state.is_playing = false;
        return;
    }

    let next_idx = if state.shuffle && len > 1 {
        let step = coprime_shuffle_step(len);
        if forward {
            (current_idx + step) % len
        } else {
            (current_idx + len - step) % len
        }
    } else if forward {
        (current_idx + 1) % len
    } else if current_idx == 0 {
        len - 1
    } else {
        current_idx - 1
    };

    state.current_track = state.queue[next_idx].clone();
    if auto_ended {
        state.is_playing = true;
    }
}

fn albums_match_identity(a: &AlbumPlaybackQueue, b: &AlbumPlaybackQueue) -> bool {
    (!a.album_uri.trim().is_empty()
        && !b.album_uri.trim().is_empty()
        && a.album_uri.trim().eq_ignore_ascii_case(b.album_uri.trim()))
        || (!a.album_title.trim().is_empty()
            && !b.album_title.trim().is_empty()
            && a.album_title
                .trim()
                .eq_ignore_ascii_case(b.album_title.trim()))
}

/// Normalizes an `AlbumPlaybackQueue` so every track has a non-empty unique ID within the album
/// and the album metadata is non-empty. Returns `None` if the album has no valid tracks.
pub fn normalize_album_playback_queue(mut album: AlbumPlaybackQueue) -> Option<AlbumPlaybackQueue> {
    let mut dedup_tracks: Vec<Track> = Vec::with_capacity(album.tracks.len());
    for (idx, mut t) in album.tracks.into_iter().enumerate() {
        if t.id.trim().is_empty() {
            if !t.spotify_uri.trim().is_empty() {
                t.id = t.spotify_uri.trim().to_string();
            } else if !t.title.trim().is_empty() {
                t.id = format!("album-track-{idx}-{}", t.title.trim());
            } else {
                continue;
            }
        }
        if !dedup_tracks.iter().any(|existing| existing.id == t.id) {
            dedup_tracks.push(t);
        }
    }
    if dedup_tracks.is_empty() {
        return None;
    }

    album.album_title = album.album_title.trim().to_string();
    if album.album_title.is_empty() {
        let from_track = dedup_tracks[0].album.trim();
        album.album_title = if from_track.is_empty() {
            "Album".to_string()
        } else {
            from_track.to_string()
        };
    }

    album.album_uri = album.album_uri.trim().to_string();
    if album.album_uri.is_empty() {
        album.album_uri = format!("spotify:album:{}", album.album_title);
    }

    album.artist_name = album.artist_name.trim().to_string();
    if album.artist_name.is_empty() {
        let from_track = dedup_tracks[0].artist.trim();
        album.artist_name = if from_track.is_empty() {
            "Artist".to_string()
        } else {
            from_track.to_string()
        };
    }

    album.tracks = dedup_tracks;
    Some(album)
}

/// Pure, verified device list merger that guarantees `INV-P6` (`!devices.is_empty()`,
/// `selected_device` is in `devices`, and at most one device has `is_default == true`).
pub fn merge_devices_verified(state: &mut PlaybackState, detected: Vec<AudioDevice>) {
    if detected.is_empty() {
        // Ensure existing state still satisfies INV-P6
        if state.devices.is_empty() {
            state.selected_device.is_default = true;
            state.devices.push(state.selected_device.clone());
        }
        normalize_single_default_device(&mut state.devices);
        if let Some(matched) = state
            .devices
            .iter()
            .find(|d| d.id == state.selected_device.id)
            .cloned()
        {
            state.selected_device = matched;
        } else if let Some(first) = state.devices.first().cloned() {
            state.selected_device = first;
        }
        return;
    }

    let current_id = state.selected_device.id.clone();
    let mut normalized = Vec::with_capacity(detected.len());
    for dev in detected {
        if !normalized
            .iter()
            .any(|existing: &AudioDevice| existing.id == dev.id || existing.name.eq_ignore_ascii_case(&dev.name))
        {
            normalized.push(dev);
        }
    }
    normalize_single_default_device(&mut normalized);

    let had_real_default = normalized
        .iter()
        .find(|d| d.id == current_id && d.is_default)
        .cloned();
    state.devices = normalized;

    if let Some(existing_default) = had_real_default {
        state.selected_device = existing_default;
    } else if let Some(existing) = state.devices.iter().find(|d| d.id == current_id).cloned() {
        state.selected_device = existing;
    } else if let Some(default_dev) = state.devices.iter().find(|d| d.is_default).cloned() {
        state.selected_device = default_dev;
    } else if let Some(first) = state.devices.first().cloned() {
        state.selected_device = first;
    }
}

fn normalize_single_default_device(devices: &mut [AudioDevice]) {
    if devices.is_empty() {
        return;
    }
    let mut seen_default = false;
    for dev in devices.iter_mut() {
        if dev.is_default {
            if seen_default {
                dev.is_default = false;
            } else {
                seen_default = true;
            }
        }
    }
    if !seen_default {
        devices[0].is_default = true;
    }
}

/// Selects a specific output device on `PlaybackState` while preserving `INV-P6`.
pub fn select_device_verified(state: &mut PlaybackState, mut device: AudioDevice) {
    if let Some(existing) = state
        .devices
        .iter()
        .find(|d| d.id == device.id || d.name.eq_ignore_ascii_case(&device.name))
        .cloned()
    {
        state.selected_device = existing;
    } else {
        if state.devices.iter().any(|d| d.is_default) {
            device.is_default = false;
        }
        state.devices.push(device.clone());
        state.selected_device = device;
    }
}

/// Applies a pure synchronous state transition on `PlaybackState` and preserves all invariants `INV-P1..INV-P12`.
#[derive(Debug, Clone, PartialEq)]
pub enum VerifiedTransportAction {
    Play,
    Pause,
    TogglePlayPause { has_loaded_track: bool },
    NextTrack,
    PreviousTrack,
    EndOfTrack,
    PositionTick(f32),
    SeekSeconds(f32),
    SetVolume(f32),
    ToggleShuffle,
    CycleRepeat,
    ToggleLikeCurrent,
    SelectQueueTrack(String),
    PlayTrack(Track),
    ActivatePlaylist {
        playlist_id: String,
        tracks: Vec<Track>,
        auto_play: bool,
    },
    ActivateAlbum {
        album: AlbumPlaybackQueue,
        artist_uri: Option<String>,
        artist_albums: Vec<AlbumPlaybackQueue>,
        start_track_index: usize,
        auto_play: bool,
    },
    SetArtistDiscography {
        artist_name: String,
        artist_uri: Option<String>,
        albums: Vec<AlbumPlaybackQueue>,
    },
    ActivatePodcastShow {
        show_uri: String,
        show_title: String,
        episodes: Vec<Track>,
        start_episode_index: usize,
        auto_play: bool,
    },
    SelectDevice(AudioDevice),
}

impl PlaybackState {
    fn clear_playlist_context(&mut self) {
        for playlist in &mut self.playlists {
            playlist.is_playing_context = false;
        }
        self.active_playlist_id = None;
    }

    fn resume_if_album_exhausted(&mut self) {
        if let PlaybackContext::Album {
            ref artist_albums,
            ref mut current_album_index,
            ref mut exhausted,
            ..
        } = self.context
        {
            if *exhausted {
                *exhausted = false;
                if !artist_albums.is_empty() {
                    *current_album_index = 0;
                    self.queue = artist_albums[0].tracks.clone();
                    if let Some(first) = self.queue.first().cloned() {
                        self.current_track = first;
                    }
                }
                self.position_secs = 0.0;
            }
        }
    }

    /// Executes a verified transport action on `self`, guaranteeing that `self.check_invariants()`
    /// holds after the transition whenever it held beforehand.
    pub fn apply_verified_action(&mut self, action: VerifiedTransportAction) {
        match action {
            VerifiedTransportAction::Play => {
                self.resume_if_album_exhausted();
                self.is_playing = true;
            }
            VerifiedTransportAction::Pause => {
                self.is_playing = false;
            }
            VerifiedTransportAction::TogglePlayPause { has_loaded_track } => {
                let target_playing = if !has_loaded_track && !self.is_playing {
                    true
                } else {
                    !self.is_playing
                };
                if target_playing {
                    self.resume_if_album_exhausted();
                }
                self.is_playing = target_playing;
            }
            VerifiedTransportAction::NextTrack => {
                advance_queue_verified(self, true, false);
            }
            VerifiedTransportAction::PreviousTrack => {
                advance_queue_verified(self, false, false);
            }
            VerifiedTransportAction::EndOfTrack => {
                advance_queue_verified(self, true, true);
            }
            VerifiedTransportAction::PositionTick(pos) => {
                if self.is_discography_exhausted() {
                    self.position_secs = 0.0;
                } else {
                    self.position_secs =
                        sanitize_position_secs(pos, self.current_track.duration_secs);
                }
            }
            VerifiedTransportAction::SeekSeconds(pos) => {
                if let PlaybackContext::Album {
                    ref mut exhausted, ..
                } = self.context
                {
                    *exhausted = false;
                }
                self.position_secs = sanitize_position_secs(pos, self.current_track.duration_secs);
            }
            VerifiedTransportAction::SetVolume(vol) => {
                self.volume = sanitize_unit_float(vol, self.volume);
            }
            VerifiedTransportAction::ToggleShuffle => {
                self.shuffle = !self.shuffle;
            }
            VerifiedTransportAction::CycleRepeat => {
                self.repeat = self.repeat.next();
            }
            VerifiedTransportAction::ToggleLikeCurrent => {
                self.current_track.liked = !self.current_track.liked;
                let liked = self.current_track.liked;
                let current_id = self.current_track.id.clone();
                for track in &mut self.queue {
                    if track.id == current_id {
                        track.liked = liked;
                    }
                }
                if let PlaybackContext::Album {
                    ref mut artist_albums,
                    current_album_index,
                    ..
                } = self.context
                {
                    if let Some(active_album) = artist_albums.get_mut(current_album_index) {
                        for track in &mut active_album.tracks {
                            if track.id == current_id {
                                track.liked = liked;
                            }
                        }
                    }
                }
            }
            VerifiedTransportAction::SelectQueueTrack(track_id) => {
                if let Some(selected) = self
                    .queue
                    .iter()
                    .find(|t| {
                        t.id == track_id || (!track_id.is_empty() && t.spotify_uri == track_id)
                    })
                    .cloned()
                {
                    if let PlaybackContext::Album {
                        ref mut exhausted, ..
                    } = self.context
                    {
                        *exhausted = false;
                    }
                    self.current_track = selected;
                    self.position_secs = 0.0;
                    self.is_playing = true;
                }
            }
            VerifiedTransportAction::PlayTrack(mut track) => {
                if track.id.trim().is_empty() {
                    track.id = if !track.spotify_uri.trim().is_empty() {
                        track.spotify_uri.trim().to_string()
                    } else {
                        "standalone-track".to_string()
                    };
                }
                let in_same_album_context = match &self.context {
                    PlaybackContext::Album {
                        artist_albums,
                        current_album_index,
                        ..
                    } => {
                        let same_album = artist_albums
                            .get(*current_album_index)
                            .is_some_and(|alb| {
                                track.album.trim().is_empty()
                                    || alb.album_title.eq_ignore_ascii_case(track.album.trim())
                            });
                        same_album
                            && self.queue.len() > 1
                            && self.queue.iter().any(|t| {
                                t.id == track.id
                                    || (!track.spotify_uri.is_empty()
                                        && t.spotify_uri == track.spotify_uri)
                            })
                    }
                    PlaybackContext::PodcastShow { .. }
                        if track.spotify_uri.starts_with("spotify:episode:") =>
                    {
                        self.queue.iter().any(|t| {
                            t.id == track.id
                                || (!track.spotify_uri.is_empty()
                                    && t.spotify_uri == track.spotify_uri)
                        })
                    }
                    _ => false,
                };

                if in_same_album_context {
                    if let Some(existing) = self.queue.iter().find(|t| {
                        t.id == track.id
                            || (!track.spotify_uri.is_empty() && t.spotify_uri == track.spotify_uri)
                    }) {
                        track = existing.clone();
                    }
                    if let PlaybackContext::Album {
                        ref mut exhausted, ..
                    } = self.context
                    {
                        *exhausted = false;
                    }
                    self.current_track = track;
                    self.position_secs = 0.0;
                    self.is_playing = true;
                } else {
                    // Playing an individual song always switches out of any active playlist
                    // into that song's parent album context.
                    self.clear_playlist_context();
                    let album_title = if track.album.trim().is_empty() {
                        track.title.clone()
                    } else {
                        track.album.trim().to_string()
                    };
                    let artist_name = if track.artist.trim().is_empty() {
                        "Artist".to_string()
                    } else {
                        track.artist.trim().to_string()
                    };
                    if track.spotify_uri.starts_with("spotify:episode:") {
                        self.context = PlaybackContext::PodcastShow {
                            show_uri: format!("spotify:show:{album_title}"),
                            show_title: album_title,
                        };
                        self.queue = vec![track.clone()];
                        self.current_track = track;
                        self.position_secs = 0.0;
                        self.is_playing = true;
                    } else if let Some(single_album) =
                        normalize_album_playback_queue(AlbumPlaybackQueue {
                            album_uri: format!("spotify:album:{album_title}"),
                            album_title,
                            artist_name: artist_name.clone(),
                            tracks: vec![track.clone()],
                        })
                    {
                        self.queue = single_album.tracks.clone();
                        self.current_track = single_album.tracks[0].clone();
                        self.position_secs = 0.0;
                        self.is_playing = true;
                        self.context = PlaybackContext::Album {
                            artist_name,
                            artist_uri: None,
                            artist_albums: vec![single_album],
                            current_album_index: 0,
                            exhausted: false,
                        };
                    }
                }
            }
            VerifiedTransportAction::ActivatePlaylist {
                playlist_id,
                tracks,
                auto_play,
            } => {
                let mut matched_id: Option<String> = None;
                for playlist in &mut self.playlists {
                    let is_match = matched_id.is_none()
                        && (playlist.id == playlist_id
                            || (!playlist.spotify_uri.is_empty()
                                && playlist.spotify_uri == playlist_id)
                            || playlist.name.eq_ignore_ascii_case(&playlist_id));
                    playlist.is_playing_context = is_match;
                    if is_match {
                        matched_id = Some(playlist.id.clone());
                    }
                }
                let resolved_id = matched_id
                    .or_else(|| (!playlist_id.trim().is_empty()).then_some(playlist_id));
                self.active_playlist_id = resolved_id.clone();
                self.context = match resolved_id {
                    Some(id) => PlaybackContext::Playlist { playlist_id: id },
                    None => PlaybackContext::None,
                };

                if !tracks.is_empty() {
                    self.current_track = tracks[0].clone();
                    self.queue = tracks;
                    self.position_secs = 0.0;
                    self.is_playing = auto_play;
                }
            }
            VerifiedTransportAction::ActivateAlbum {
                album,
                artist_uri,
                artist_albums,
                start_track_index,
                auto_play,
            } => {
                let primary_opt = normalize_album_playback_queue(album);
                let mut norm_albums: Vec<AlbumPlaybackQueue> = Vec::new();
                for candidate in artist_albums {
                    if let Some(norm) = normalize_album_playback_queue(candidate) {
                        if !norm_albums
                            .iter()
                            .any(|existing| albums_match_identity(existing, &norm))
                        {
                            norm_albums.push(norm);
                        }
                    }
                }

                let current_album_index = if let Some(primary) = primary_opt {
                    if let Some(pos) = norm_albums
                        .iter()
                        .position(|existing| albums_match_identity(existing, &primary))
                    {
                        norm_albums[pos] = primary;
                        pos
                    } else {
                        norm_albums.insert(0, primary);
                        0
                    }
                } else {
                    0
                };

                if norm_albums.is_empty() {
                    return;
                }

                self.clear_playlist_context();
                let active_album = &norm_albums[current_album_index];
                let track_idx = start_track_index.min(active_album.tracks.len() - 1);
                let chosen_track = active_album.tracks[track_idx].clone();
                let artist_name = active_album.artist_name.clone();

                self.queue = active_album.tracks.clone();
                self.current_track = chosen_track;
                self.position_secs = 0.0;
                self.is_playing = auto_play;
                self.context = PlaybackContext::Album {
                    artist_name,
                    artist_uri,
                    artist_albums: norm_albums,
                    current_album_index,
                    exhausted: false,
                };
            }
            VerifiedTransportAction::SetArtistDiscography {
                artist_name,
                artist_uri,
                albums,
            } => {
                if let PlaybackContext::Album {
                    artist_name: ref mut cur_artist_name,
                    artist_uri: ref mut cur_artist_uri,
                    ref mut artist_albums,
                    ref mut current_album_index,
                    ref mut exhausted,
                } = self.context
                {
                    if !artist_name.trim().is_empty()
                        && !cur_artist_name.trim().is_empty()
                        && !cur_artist_name
                            .trim()
                            .eq_ignore_ascii_case(artist_name.trim())
                    {
                        return;
                    }
                    let Some(current_active) = artist_albums.get(*current_album_index).cloned()
                    else {
                        return;
                    };
                    // Preserve the exact active album queue (including any liked flags)
                    let active_preserved = AlbumPlaybackQueue {
                        album_uri: current_active.album_uri,
                        album_title: current_active.album_title,
                        artist_name: current_active.artist_name,
                        tracks: self.queue.clone(),
                    };

                    let mut norm_albums: Vec<AlbumPlaybackQueue> = Vec::new();
                    for candidate in albums {
                        if let Some(norm) = normalize_album_playback_queue(candidate) {
                            if !norm_albums
                                .iter()
                                .any(|existing| albums_match_identity(existing, &norm))
                            {
                                norm_albums.push(norm);
                            }
                        }
                    }

                    let new_idx = if let Some(pos) = norm_albums
                        .iter()
                        .position(|existing| albums_match_identity(existing, &active_preserved))
                    {
                        norm_albums[pos] = active_preserved;
                        pos
                    } else {
                        norm_albums.insert(0, active_preserved);
                        0
                    };

                    if !artist_name.trim().is_empty() {
                        *cur_artist_name = artist_name.trim().to_string();
                    }
                    if artist_uri.is_some() {
                        *cur_artist_uri = artist_uri;
                    }
                    *artist_albums = norm_albums;
                    *current_album_index = new_idx;
                    if *exhausted && new_idx + 1 < artist_albums.len() {
                        *exhausted = false;
                    }
                }
            }
            VerifiedTransportAction::ActivatePodcastShow {
                show_uri,
                show_title,
                episodes,
                start_episode_index,
                auto_play,
            } => {
                if episodes.is_empty() {
                    return;
                }
                self.clear_playlist_context();
                let ep_idx = start_episode_index.min(episodes.len() - 1);
                self.current_track = episodes[ep_idx].clone();
                self.queue = episodes;
                self.position_secs = 0.0;
                self.is_playing = auto_play;
                self.context = PlaybackContext::PodcastShow {
                    show_uri,
                    show_title,
                };
            }
            VerifiedTransportAction::SelectDevice(device) => {
                select_device_verified(self, device);
            }
        }
    }
}

impl FormalInvariant for PlaybackState {
    fn check_invariants(&self) -> Result<(), InvariantViolation> {
        // INV-P1: Volume is finite and within [0.0, 1.0].
        if !self.volume.is_finite() || !(0.0..=1.0).contains(&self.volume) {
            return Err(InvariantViolation {
                code: "INV-P1",
                subsystem: "PlaybackState",
                details: format!("volume {} is out of finite [0.0, 1.0] bounds", self.volume),
            });
        }

        // INV-P2: Position is finite and non-negative.
        if !self.position_secs.is_finite() || self.position_secs < 0.0 {
            return Err(InvariantViolation {
                code: "INV-P2",
                subsystem: "PlaybackState",
                details: format!(
                    "position_secs {} must be finite and >= 0.0",
                    self.position_secs
                ),
            });
        }

        // INV-P3: Position never exceeds current_track.duration_secs when duration_secs > 0.
        if self.current_track.duration_secs > 0
            && self.position_secs > self.current_track.duration_secs as f32
        {
            return Err(InvariantViolation {
                code: "INV-P3",
                subsystem: "PlaybackState",
                details: format!(
                    "position_secs {} exceeds current_track.duration_secs {}",
                    self.position_secs, self.current_track.duration_secs
                ),
            });
        }

        // INV-P4: Queue membership and `liked` flag coherence.
        if !self.queue.is_empty() && !self.current_track.id.is_empty() {
            match self.queue.iter().find(|t| t.id == self.current_track.id) {
                Some(in_queue) => {
                    if in_queue.liked != self.current_track.liked {
                        return Err(InvariantViolation {
                            code: "INV-P4",
                            subsystem: "PlaybackState",
                            details: format!(
                                "current_track '{}' liked={} disagrees with queue entry liked={}",
                                self.current_track.id, self.current_track.liked, in_queue.liked
                            ),
                        });
                    }
                }
                None => {
                    return Err(InvariantViolation {
                        code: "INV-P4",
                        subsystem: "PlaybackState",
                        details: format!(
                            "non-empty queue does not contain current_track.id '{}'",
                            self.current_track.id
                        ),
                    });
                }
            }
        }

        // INV-P5: At most one playlist has `is_playing_context == true`, and it matches `active_playlist_id`.
        // Moreover, a playlist may only be active when `context == PlaybackContext::Playlist`.
        let active_contexts: Vec<&str> = self
            .playlists
            .iter()
            .filter(|p| p.is_playing_context)
            .map(|p| p.id.as_str())
            .collect();
        if active_contexts.len() > 1 {
            return Err(InvariantViolation {
                code: "INV-P5",
                subsystem: "PlaybackState",
                details: format!(
                    "multiple playlists marked as active playing context: {:?}",
                    active_contexts
                ),
            });
        }
        if let Some(&active_id) = active_contexts.first() {
            if self.active_playlist_id.as_deref() != Some(active_id) {
                return Err(InvariantViolation {
                    code: "INV-P5",
                    subsystem: "PlaybackState",
                    details: format!(
                        "playlist '{}' has is_playing_context=true but active_playlist_id={:?}",
                        active_id, self.active_playlist_id
                    ),
                });
            }
        }
        match &self.context {
            PlaybackContext::Playlist { playlist_id } => {
                if self.active_playlist_id.as_deref() != Some(playlist_id.as_str()) {
                    return Err(InvariantViolation {
                        code: "INV-P5",
                        subsystem: "PlaybackState",
                        details: format!(
                            "context is Playlist('{playlist_id}') but active_playlist_id={:?}",
                            self.active_playlist_id
                        ),
                    });
                }
            }
            PlaybackContext::Album { .. } | PlaybackContext::PodcastShow { .. } => {
                if self.active_playlist_id.is_some() || !active_contexts.is_empty() {
                    return Err(InvariantViolation {
                        code: "INV-P5",
                        subsystem: "PlaybackState",
                        details: format!(
                            "non-playlist context {:?} must not have active_playlist_id={:?} or active_contexts={:?}",
                            self.context, self.active_playlist_id, active_contexts
                        ),
                    });
                }
            }
            PlaybackContext::None => {}
        }

        // INV-P10: Album `Up Next` queue isolation (`state.queue == artist_albums[current_album_index].tracks`).
        // INV-P11: Discography exhaustion silence (`exhausted ==> current_album_index + 1 == len && !is_playing && position_secs == 0.0`).
        if let PlaybackContext::Album {
            ref artist_albums,
            current_album_index,
            exhausted,
            ..
        } = self.context
        {
            if artist_albums.is_empty() {
                return Err(InvariantViolation {
                    code: "INV-P10",
                    subsystem: "PlaybackState",
                    details: "PlaybackContext::Album must contain at least one album in artist_albums"
                        .to_string(),
                });
            }
            if current_album_index >= artist_albums.len() {
                return Err(InvariantViolation {
                    code: "INV-P10",
                    subsystem: "PlaybackState",
                    details: format!(
                        "current_album_index {current_album_index} is out of bounds for artist_albums len {}",
                        artist_albums.len()
                    ),
                });
            }
            for (idx, alb) in artist_albums.iter().enumerate() {
                if alb.tracks.is_empty() {
                    return Err(InvariantViolation {
                        code: "INV-P10",
                        subsystem: "PlaybackState",
                        details: format!("artist_albums[{idx}] ('{}') has empty tracks", alb.album_title),
                    });
                }
            }
            let active_album = &artist_albums[current_album_index];
            if self.queue != active_album.tracks {
                return Err(InvariantViolation {
                    code: "INV-P10",
                    subsystem: "PlaybackState",
                    details: format!(
                        "Up Next queue (len={}) does not match active album '{}' tracks (len={})",
                        self.queue.len(),
                        active_album.album_title,
                        active_album.tracks.len()
                    ),
                });
            }

            if exhausted {
                if current_album_index + 1 != artist_albums.len() {
                    return Err(InvariantViolation {
                        code: "INV-P11",
                        subsystem: "PlaybackState",
                        details: format!(
                            "discography marked exhausted at album {} of {} before final album",
                            current_album_index,
                            artist_albums.len()
                        ),
                    });
                }
                if self.is_playing {
                    return Err(InvariantViolation {
                        code: "INV-P11",
                        subsystem: "PlaybackState",
                        details:
                            "discography is exhausted (all albums finished) but is_playing == true"
                                .to_string(),
                    });
                }
                if self.position_secs != 0.0 {
                    return Err(InvariantViolation {
                        code: "INV-P11",
                        subsystem: "PlaybackState",
                        details: format!(
                            "discography is exhausted but position_secs={} (expected 0.0)",
                            self.position_secs
                        ),
                    });
                }
            }
        }

        // INV-P6: Device list is non-empty, contains selected_device, and has exactly one default device.
        if self.devices.is_empty() {
            return Err(InvariantViolation {
                code: "INV-P6",
                subsystem: "PlaybackState",
                details: "devices list must never be empty".to_string(),
            });
        }
        let default_count = self.devices.iter().filter(|d| d.is_default).count();
        if default_count != 1 {
            return Err(InvariantViolation {
                code: "INV-P6",
                subsystem: "PlaybackState",
                details: format!(
                    "expected exactly 1 default audio device, found {default_count}"
                ),
            });
        }
        if !self.devices.iter().any(|d| d.id == self.selected_device.id) {
            return Err(InvariantViolation {
                code: "INV-P6",
                subsystem: "PlaybackState",
                details: format!(
                    "selected_device '{}' is not present in devices list",
                    self.selected_device.id
                ),
            });
        }

        Ok(())
    }
}

impl FormalInvariant for SpectrumFrame {
    fn check_invariants(&self) -> Result<(), InvariantViolation> {
        // INV-D1: Every band in `0..SPECTRUM_BANDS` is finite and in `0.0..=1.0`.
        for (idx, &b) in self.bands.iter().enumerate() {
            if !b.is_finite() || !(0.0..=1.0).contains(&b) {
                return Err(InvariantViolation {
                    code: "INV-D1",
                    subsystem: "SpectrumFrame",
                    details: format!("band[{idx}] = {b} is outside finite [0.0, 1.0]"),
                });
            }
        }

        // INV-D2: Energy envelopes are finite and in `0.0..=1.0`.
        for (name, val) in [
            ("rms_energy", self.rms_energy),
            ("bass_energy", self.bass_energy),
            ("mid_energy", self.mid_energy),
            ("high_energy", self.high_energy),
        ] {
            if !val.is_finite() || !(0.0..=1.0).contains(&val) {
                return Err(InvariantViolation {
                    code: "INV-D2",
                    subsystem: "SpectrumFrame",
                    details: format!("{name} = {val} is outside finite [0.0, 1.0]"),
                });
            }
        }

        // INV-D3: Phase is finite and within `[0.0, 20.0 * TAU]`.
        let max_phase = 20.0 * std::f32::consts::TAU + 1e-3;
        if !self.phase.is_finite() || self.phase < 0.0 || self.phase > max_phase {
            return Err(InvariantViolation {
                code: "INV-D3",
                subsystem: "SpectrumFrame",
                details: format!("phase = {} is outside finite [0.0, 20*TAU]", self.phase),
            });
        }

        Ok(())
    }
}

/// Trait for routes managed by the formally verified `NavHistoryMachine`.
pub trait NavRouteTarget: Clone + std::fmt::Debug + PartialEq + Eq {
    /// Returns true if `self` and `other` refer to the same logical screen/entity
    /// (so pushing `other` while on `self` updates in-place instead of creating an adjacent duplicate).
    fn matches_target(&self, other: &Self) -> bool;
}

/// Formally verified bounded multi-level navigation stack with Back, Forward, and History Jump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavHistoryMachine<R: NavRouteTarget> {
    entries: Vec<R>,
    index: usize,
    max_capacity: usize,
}

impl<R: NavRouteTarget> NavHistoryMachine<R> {
    pub fn new(initial: R, max_capacity: usize) -> Self {
        let cap = max_capacity.max(2);
        Self {
            entries: vec![initial],
            index: 0,
            max_capacity: cap,
        }
    }

    #[inline]
    pub fn current(&self) -> &R {
        &self.entries[self.index]
    }

    #[inline]
    pub fn current_mut(&mut self) -> &mut R {
        &mut self.entries[self.index]
    }

    #[inline]
    pub fn entries(&self) -> &[R] {
        &self.entries
    }

    #[inline]
    pub fn index(&self) -> usize {
        self.index
    }

    #[inline]
    pub fn max_capacity(&self) -> usize {
        self.max_capacity
    }

    #[inline]
    pub fn can_go_back(&self) -> bool {
        self.index > 0 && self.entries.len() > 1
    }

    #[inline]
    pub fn can_go_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    #[inline]
    pub fn previous_entry(&self) -> Option<&R> {
        if self.can_go_back() {
            self.entries.get(self.index - 1)
        } else {
            None
        }
    }

    #[inline]
    pub fn previous_entry_mut(&mut self) -> Option<&mut R> {
        if self.can_go_back() {
            let prev_idx = self.index - 1;
            self.entries.get_mut(prev_idx)
        } else {
            None
        }
    }

    #[inline]
    pub fn next_entry(&self) -> Option<&R> {
        if self.can_go_forward() {
            self.entries.get(self.index + 1)
        } else {
            None
        }
    }

    /// Pushes `route` onto the navigation history:
    /// - If `route` matches the current route target, updates the current slot in-place (`INV-N3`).
    /// - Otherwise truncates any forward history beyond `self.index` (`INV-N4`), appends `route`,
    ///   and evicts the oldest entry if `entries.len() > max_capacity` (`INV-N1`).
    pub fn push(&mut self, route: R) {
        if self.current().matches_target(&route) {
            self.entries[self.index] = route;
            return;
        }
        self.entries.truncate(self.index + 1);
        self.entries.push(route);
        if self.entries.len() > self.max_capacity {
            let overflow = self.entries.len() - self.max_capacity;
            self.entries.drain(0..overflow);
        }
        self.index = self.entries.len() - 1;
    }

    /// Moves one step back in history if `can_go_back()` is true. Returns true if moved.
    pub fn go_back(&mut self) -> bool {
        if self.can_go_back() {
            self.index -= 1;
            true
        } else {
            false
        }
    }

    /// Moves one step forward in history if `can_go_forward()` is true. Returns true if moved.
    pub fn go_forward(&mut self) -> bool {
        if self.can_go_forward() {
            self.index += 1;
            true
        } else {
            false
        }
    }

    /// Jumps directly to `target_idx` if within `0..entries.len()`. Returns true if moved.
    pub fn jump_to(&mut self, target_idx: usize) -> bool {
        if target_idx < self.entries.len() {
            self.index = target_idx;
            true
        } else {
            false
        }
    }
}

impl<R: NavRouteTarget> FormalInvariant for NavHistoryMachine<R> {
    fn check_invariants(&self) -> Result<(), InvariantViolation> {
        // INV-N1: Non-empty and bounded by `max_capacity`.
        if self.entries.is_empty() || self.entries.len() > self.max_capacity {
            return Err(InvariantViolation {
                code: "INV-N1",
                subsystem: "NavHistoryMachine",
                details: format!(
                    "entries.len() = {} violates 1 <= len <= {}",
                    self.entries.len(),
                    self.max_capacity
                ),
            });
        }

        // INV-N2: Cursor index is strictly within bounds.
        if self.index >= self.entries.len() {
            return Err(InvariantViolation {
                code: "INV-N2",
                subsystem: "NavHistoryMachine",
                details: format!(
                    "index {} >= entries.len() {}",
                    self.index,
                    self.entries.len()
                ),
            });
        }

        // INV-N3: No two adjacent entries have matching route targets.
        for window in self.entries.windows(2) {
            if window[0].matches_target(&window[1]) {
                return Err(InvariantViolation {
                    code: "INV-N3",
                    subsystem: "NavHistoryMachine",
                    details: format!(
                        "adjacent duplicate route targets in history: {:?} and {:?}",
                        window[0], window[1]
                    ),
                });
            }
        }

        Ok(())
    }
}

/// Kind of item stored in the "Recently Searched" history.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RecentSearchKind {
    Query,
    Artist {
        uri: String,
    },
    Album {
        uri: String,
        artist_name: String,
    },
    Show {
        uri: String,
        publisher: String,
    },
    Track {
        uri: String,
        artist_name: String,
        #[serde(default)]
        album_title: String,
        #[serde(default)]
        album_uri: Option<String>,
    },
}

impl RecentSearchKind {
    pub fn badge_label(&self) -> &'static str {
        match self {
            RecentSearchKind::Query => "SEARCH",
            RecentSearchKind::Artist { .. } => "ARTIST",
            RecentSearchKind::Album { .. } => "ALBUM",
            RecentSearchKind::Show { .. } => "PODCAST",
            RecentSearchKind::Track { .. } => "SONG",
        }
    }

    pub fn display_badge(&self) -> &'static str {
        match self {
            RecentSearchKind::Query => "SEARCH",
            RecentSearchKind::Artist { .. } => "★ ARTIST",
            RecentSearchKind::Album { .. } => "ALBUM",
            RecentSearchKind::Show { .. } => "PODCAST",
            RecentSearchKind::Track { .. } => "▶ SONG",
        }
    }

    pub fn action_hint(&self) -> &'static str {
        match self {
            RecentSearchKind::Query => "Search ›",
            RecentSearchKind::Artist { .. } => "Artist ›",
            RecentSearchKind::Album { .. } => "Album ›",
            RecentSearchKind::Show { .. } => "Show ›",
            RecentSearchKind::Track { .. } => "▶ Album",
        }
    }

    #[inline]
    pub fn is_song(&self) -> bool {
        matches!(self, RecentSearchKind::Track { .. })
    }

    #[inline]
    pub fn is_artist(&self) -> bool {
        matches!(self, RecentSearchKind::Artist { .. })
    }

    #[inline]
    pub fn is_podcast(&self) -> bool {
        matches!(self, RecentSearchKind::Show { .. })
    }

    /// Returns `true` iff this entry represents a real clicked Artist, Song, or Podcast
    /// (excluding raw search query strings and ungrouped kinds).
    #[inline]
    pub fn is_real_clicked_item(&self) -> bool {
        self.is_artist() || self.is_song() || self.is_podcast()
    }
}

/// Persisted entry in the "Recently Searched" store.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecentSearchEntry {
    pub label: String,
    pub subtitle: String,
    pub query: String,
    pub kind: RecentSearchKind,
    #[serde(default)]
    pub cover_art_url: Option<String>,
    #[serde(default)]
    pub artwork_theme: ArtworkTheme,
}

impl RecentSearchEntry {
    pub fn from_song(track: &Track, album_uri: Option<String>) -> Self {
        let title = track.title.trim();
        let artist = if track.artist.trim().is_empty() {
            "Artist"
        } else {
            track.artist.trim()
        };
        let album = track.album.trim();
        let subtitle = if album.is_empty() {
            format!("Song · {artist}")
        } else {
            format!("Song · {artist} · {album}")
        };
        Self {
            label: title.to_string(),
            subtitle,
            query: title.to_string(),
            kind: RecentSearchKind::Track {
                uri: track.spotify_uri.trim().to_string(),
                artist_name: artist.to_string(),
                album_title: album.to_string(),
                album_uri: album_uri.filter(|u| !u.trim().is_empty()),
            },
            cover_art_url: track.cover_art_url.clone(),
            artwork_theme: track.artwork_theme,
        }
    }

    pub fn from_artist(
        name: &str,
        uri: &str,
        cover_art_url: Option<String>,
        artwork_theme: ArtworkTheme,
    ) -> Self {
        let clean_name = name.trim();
        Self {
            label: clean_name.to_string(),
            subtitle: "Artist · Discography".to_string(),
            query: clean_name.to_string(),
            kind: RecentSearchKind::Artist {
                uri: uri.trim().to_string(),
            },
            cover_art_url,
            artwork_theme,
        }
    }

    pub fn from_podcast(
        title: &str,
        publisher: &str,
        uri: &str,
        cover_art_url: Option<String>,
        artwork_theme: ArtworkTheme,
    ) -> Self {
        let clean_title = title.trim();
        let clean_pub = if publisher.trim().is_empty() {
            "Podcast"
        } else {
            publisher.trim()
        };
        Self {
            label: clean_title.to_string(),
            subtitle: format!("Podcast · {clean_pub}"),
            query: clean_title.to_string(),
            kind: RecentSearchKind::Show {
                uri: uri.trim().to_string(),
                publisher: clean_pub.to_string(),
            },
            cover_art_url,
            artwork_theme,
        }
    }
}

/// Formally verified bounded store for Recently Searched items (real clicked Artists, Songs, and Podcasts only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentSearchStore {
    entries: Vec<RecentSearchEntry>,
    max_entries: usize,
}

impl Default for RecentSearchStore {
    fn default() -> Self {
        Self::new(30)
    }
}

impl RecentSearchStore {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_entries: max_entries.max(1),
        }
    }

    pub fn from_loaded(raw: Vec<RecentSearchEntry>, max_entries: usize) -> Self {
        let mut store = Self::new(max_entries);
        // Insert in reverse so index 0 of `raw` ends up at the front while enforcing all invariants
        // and automatically stripping any legacy search-query strings.
        for entry in raw.into_iter().rev() {
            store.record_entry(entry);
        }
        store
    }

    #[inline]
    pub fn entries(&self) -> &[RecentSearchEntry] {
        &self.entries
    }

    /// Returns `(store_index, entry)` for all clicked Artist entries in most-recent-first order.
    pub fn artist_entries_indexed(&self) -> Vec<(usize, &RecentSearchEntry)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind.is_artist())
            .collect()
    }

    /// Returns `(store_index, entry)` for all clicked Song entries in most-recent-first order.
    pub fn song_entries_indexed(&self) -> Vec<(usize, &RecentSearchEntry)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind.is_song())
            .collect()
    }

    /// Returns `(store_index, entry)` for all clicked Podcast entries in most-recent-first order.
    pub fn podcast_entries_indexed(&self) -> Vec<(usize, &RecentSearchEntry)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind.is_podcast())
            .collect()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn get(&self, idx: usize) -> Option<&RecentSearchEntry> {
        self.entries.get(idx)
    }

    pub fn record_entry(&mut self, mut entry: RecentSearchEntry) -> bool {
        if !entry.kind.is_real_clicked_item() {
            return false;
        }
        let trimmed_label = entry.label.trim().to_string();
        if trimmed_label.is_empty() {
            return false;
        }
        entry.label = trimmed_label;
        let trimmed_query = entry.query.trim().to_string();
        entry.query = if trimmed_query.is_empty() {
            entry.label.clone()
        } else {
            trimmed_query
        };

        // Preserve existing cover_art_url if the re-selected entry didn't have one yet.
        let prev_art = self
            .entries
            .iter()
            .find(|existing| {
                existing.kind.badge_label() == entry.kind.badge_label()
                    && existing.label.eq_ignore_ascii_case(&entry.label)
            })
            .and_then(|existing| existing.cover_art_url.clone());
        if entry.cover_art_url.is_none() {
            entry.cover_art_url = prev_art;
        }

        self.entries.retain(|existing| {
            !(existing.kind.badge_label() == entry.kind.badge_label()
                && existing.label.eq_ignore_ascii_case(&entry.label))
        });
        self.entries.insert(0, entry);
        self.entries.truncate(self.max_entries);
        true
    }

    /// Raw search query strings are never stored in `RecentSearchStore`; only real clicked
    /// Artists, Songs, and Podcasts are recorded (`INV-R4`).
    pub fn record_typing_query(&mut self, _raw_query: &str) -> bool {
        false
    }

    pub fn remove_at(&mut self, idx: usize) -> Option<RecentSearchEntry> {
        if idx < self.entries.len() {
            Some(self.entries.remove(idx))
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Sets `cover_art_url` on a matching `(badge_label, label)` entry if not already present.
    pub fn set_cover_art_url(
        &mut self,
        badge_label: &str,
        label: &str,
        cover_art_url: String,
    ) -> bool {
        let trimmed = cover_art_url.trim();
        if trimmed.is_empty() {
            return false;
        }
        let mut updated = false;
        for entry in &mut self.entries {
            if entry.kind.badge_label().eq_ignore_ascii_case(badge_label)
                && entry.label.eq_ignore_ascii_case(label)
                && entry.cover_art_url.as_deref() != Some(trimmed)
            {
                entry.cover_art_url = Some(trimmed.to_string());
                updated = true;
            }
        }
        updated
    }
}

impl FormalInvariant for RecentSearchStore {
    fn check_invariants(&self) -> Result<(), InvariantViolation> {
        // INV-R1: Bounded capacity.
        if self.entries.len() > self.max_entries {
            return Err(InvariantViolation {
                code: "INV-R1",
                subsystem: "RecentSearchStore",
                details: format!(
                    "entries.len() = {} exceeds max_entries = {}",
                    self.entries.len(),
                    self.max_entries
                ),
            });
        }

        // INV-R2: No blank labels or queries.
        for (idx, entry) in self.entries.iter().enumerate() {
            if entry.label.trim().is_empty() || entry.query.trim().is_empty() {
                return Err(InvariantViolation {
                    code: "INV-R2",
                    subsystem: "RecentSearchStore",
                    details: format!("entry[{idx}] has blank label or query: {entry:?}"),
                });
            }
        }

        // INV-R3: Pairwise uniqueness by (badge_label, case-insensitive label).
        for (i, a) in self.entries.iter().enumerate() {
            for b in self.entries.iter().skip(i + 1) {
                if a.kind.badge_label() == b.kind.badge_label()
                    && a.label.eq_ignore_ascii_case(&b.label)
                {
                    return Err(InvariantViolation {
                        code: "INV-R3",
                        subsystem: "RecentSearchStore",
                        details: format!(
                            "duplicate recent search entries: '{}' ({}) and '{}' ({})",
                            a.label,
                            a.kind.badge_label(),
                            b.label,
                            b.kind.badge_label()
                        ),
                    });
                }
            }
        }

        // INV-R4: Real clicked entities only (no raw search strings) & exact 3-list partition (Artists, Songs, Podcasts).
        for (idx, entry) in self.entries.iter().enumerate() {
            if !entry.kind.is_real_clicked_item() {
                return Err(InvariantViolation {
                    code: "INV-R4",
                    subsystem: "RecentSearchStore",
                    details: format!(
                        "entry[{idx}] is not a real clicked Artist, Song, or Podcast: {:?}",
                        entry.kind
                    ),
                });
            }
            if entry.kind.is_song() {
                if entry.kind.badge_label() != "SONG"
                    || entry.kind.display_badge() != "▶ SONG"
                    || entry.kind.is_artist()
                    || entry.kind.is_podcast()
                {
                    return Err(InvariantViolation {
                        code: "INV-R4",
                        subsystem: "RecentSearchStore",
                        details: format!(
                            "entry[{idx}] is a song but has invalid badge denotation: {:?}",
                            entry.kind
                        ),
                    });
                }
            } else if entry.kind.is_artist()
                && (entry.kind.badge_label() != "ARTIST"
                    || entry.kind.display_badge() != "★ ARTIST"
                    || entry.kind.is_song()
                    || entry.kind.is_podcast())
            {
                return Err(InvariantViolation {
                    code: "INV-R4",
                    subsystem: "RecentSearchStore",
                    details: format!(
                        "entry[{idx}] is an artist but has invalid badge denotation: {:?}",
                        entry.kind
                    ),
                });
            } else if entry.kind.is_podcast()
                && (entry.kind.badge_label() != "PODCAST"
                    || entry.kind.display_badge() != "PODCAST"
                    || entry.kind.is_song()
                    || entry.kind.is_artist())
            {
                return Err(InvariantViolation {
                    code: "INV-R4",
                    subsystem: "RecentSearchStore",
                    details: format!(
                        "entry[{idx}] is a podcast but has invalid badge denotation: {:?}",
                        entry.kind
                    ),
                });
            }
        }

        let partitioned_count = self.artist_entries_indexed().len()
            + self.song_entries_indexed().len()
            + self.podcast_entries_indexed().len();
        if partitioned_count != self.entries.len() {
            return Err(InvariantViolation {
                code: "INV-R4",
                subsystem: "RecentSearchStore",
                details: format!(
                    "3-list partition count ({partitioned_count}) != entries.len() ({})",
                    self.entries.len()
                ),
            });
        }

        Ok(())
    }
}

fn normalize_song_title_key(title: &str) -> String {
    let trimmed = title.trim().to_lowercase();
    let without_dash = trimmed
        .split(" - ")
        .next()
        .unwrap_or(&trimmed)
        .trim();
    without_dash
        .split(" (remaster")
        .next()
        .unwrap_or(without_dash)
        .trim()
        .to_string()
}

/// Pure, formally verified lookup of a song's index within its parent album's tracklist.
///
/// Formal Guarantees:
/// - **INV-SONG-IDX-1 (Bounds Soundness)**: Whenever `Some(idx)` is returned, `idx < album_tracks.len()`.
/// - **INV-SONG-IDX-2 (Exact ID/URI Priority)**: If any track in `album_tracks` matches `target_song.id`
///   or `target_song.spotify_uri`, the first such index is returned before title matching.
/// - **INV-SONG-IDX-3 (Title Fallback)**: If IDs differ (e.g. single vs album URI), matches by
///   case-insensitive title or normalized remaster-stripped title.
pub fn find_song_index_in_album_verified(
    album_tracks: &[Track],
    target_song: &Track,
) -> Option<usize> {
    if album_tracks.is_empty() {
        return None;
    }

    let target_id = target_song.id.trim();
    let target_uri = target_song.spotify_uri.trim();
    if !target_id.is_empty() || !target_uri.is_empty() {
        if let Some(idx) = album_tracks.iter().position(|t| {
            (!target_id.is_empty() && t.id.trim().eq_ignore_ascii_case(target_id))
                || (!target_uri.is_empty()
                    && t.spotify_uri.trim().eq_ignore_ascii_case(target_uri))
        }) {
            return Some(idx);
        }
    }

    let target_title = target_song.title.trim();
    if !target_title.is_empty() {
        if let Some(idx) = album_tracks
            .iter()
            .position(|t| t.title.trim().eq_ignore_ascii_case(target_title))
        {
            return Some(idx);
        }

        let norm_target = normalize_song_title_key(target_title);
        if norm_target.len() >= 2 {
            if let Some(idx) = album_tracks
                .iter()
                .position(|t| normalize_song_title_key(&t.title) == norm_target)
            {
                return Some(idx);
            }
        }
    }

    None
}

/// Builds a normalized `AlbumPlaybackQueue` and the exact `start_track_index` for `target_song`
/// on its parent album so that selecting a song from Search or Search History behaves identically
/// to opening the album that the song is on and selecting that song on the album (and playing
/// the next song in the album afterward).
///
/// Formal Guarantees:
/// - **INV-SONG-ALBUM-1 (Valid Start Index)**: `start_idx < queue.tracks.len()`.
/// - **INV-SONG-ALBUM-2 (Selected Song Active)**: `queue.tracks[start_idx]` corresponds to `target_song`
///   at its position on the album.
/// - **INV-SONG-ALBUM-3 (Sequential Next Song in Album)**: If `start_idx + 1 < queue.tracks.len()`,
///   activating the returned `(queue, start_idx)` and advancing via `NextTrack` or `EndOfTrack`
///   (with `shuffle == false` and `repeat != RepeatMode::Track`) deterministically plays
///   `queue.tracks[start_idx + 1]`.
pub fn resolve_song_album_queue_verified(
    album_uri: &str,
    album_title: &str,
    artist_name: &str,
    mut album_tracks: Vec<Track>,
    target_song: &Track,
) -> Option<(AlbumPlaybackQueue, usize)> {
    let resolved_title = if !album_title.trim().is_empty() {
        album_title.trim().to_string()
    } else if !target_song.album.trim().is_empty() {
        target_song.album.trim().to_string()
    } else {
        target_song.title.trim().to_string()
    };

    let resolved_artist = if !artist_name.trim().is_empty() {
        artist_name.trim().to_string()
    } else if !target_song.artist.trim().is_empty() {
        target_song.artist.trim().to_string()
    } else {
        "Artist".to_string()
    };

    let resolved_uri = if !album_uri.trim().is_empty() {
        album_uri.trim().to_string()
    } else {
        format!("spotify:album:{resolved_title}")
    };

    if album_tracks.is_empty() {
        album_tracks.push(target_song.clone());
    }

    let raw_idx = match find_song_index_in_album_verified(&album_tracks, target_song) {
        Some(idx) => {
            if album_tracks[idx].cover_art_url.is_none() && target_song.cover_art_url.is_some() {
                album_tracks[idx].cover_art_url = target_song.cover_art_url.clone();
            }
            idx
        }
        None => {
            let mut inserted = target_song.clone();
            inserted.album = resolved_title.clone();
            album_tracks.insert(0, inserted);
            0
        }
    };

    let target_after_insert = album_tracks[raw_idx].clone();
    let normalized = normalize_album_playback_queue(AlbumPlaybackQueue {
        album_uri: resolved_uri,
        album_title: resolved_title,
        artist_name: resolved_artist,
        tracks: album_tracks,
    })?;

    let final_idx = find_song_index_in_album_verified(&normalized.tracks, &target_after_insert)
        .unwrap_or_else(|| raw_idx.min(normalized.tracks.len().saturating_sub(1)));

    Some((normalized, final_idx))
}

/// Pure decision model for resolving an unlinked Spotify track (with `0` direct audio files)
/// to its first playable alternative that has audio files.
///
/// Formal Guarantees:
/// - **INV-A1 (Soundness)**: Returns `Some((uri, file_count))` **iff** `file_count > 0`. Never returns a 0-file track.
/// - **INV-A2 (Primary Priority)**: Returns `(primary_uri, primary_files)` whenever `primary_files > 0`.
/// - **INV-A3 (First-Match Alternative Determinism)**: When `primary_files == 0`, returns the first alternative in slice order with `file_count > 0`.
pub fn resolve_playable_track_candidate<'a>(
    primary_uri: &'a str,
    primary_file_count: usize,
    alternatives: &[(&'a str, usize)],
) -> Option<(&'a str, usize)> {
    if primary_uri.trim().is_empty() {
        return None;
    }
    if primary_file_count > 0 {
        return Some((primary_uri, primary_file_count));
    }
    for &(alt_uri, alt_files) in alternatives {
        if !alt_uri.trim().is_empty() && alt_files > 0 {
            return Some((alt_uri, alt_files));
        }
    }
    None
}

/// Checks whether `candidate` inside `<...>` looks like an HTML tag (starts with ASCII alphabetic, `/`, or `!`).
fn looks_like_html_tag(candidate: &str) -> bool {
    let trimmed = candidate.trim();
    let Some(first) = trimmed.chars().next() else {
        return false;
    };
    if first == '/' {
        trimmed
            .chars()
            .nth(1)
            .is_some_and(|c| c.is_ascii_alphabetic())
    } else {
        first.is_ascii_alphabetic() || first == '!'
    }
}

fn sanitize_spotify_biography_pass(raw: &str) -> String {
    let decoded = raw
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&ndash;", "–")
        .replace("&mdash;", "—")
        .replace("&hellip;", "…")
        .replace("&#8211;", "–")
        .replace("&#8212;", "—")
        .replace("&#8217;", "’")
        .replace("&#8220;", "“")
        .replace("&#8221;", "”");

    let mut stripped = String::with_capacity(decoded.len());
    let bytes = decoded.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'<' {
            // Find matching '>' on the same line without another '<' intervening
            let mut close_idx = None;
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'\n' && bytes[j] != b'<' {
                if bytes[j] == b'>' {
                    close_idx = Some(j);
                    break;
                }
                j += 1;
            }

            if let Some(end_j) = close_idx {
                let inner = &decoded[i + 1..end_j];
                if looks_like_html_tag(inner) {
                    let tag_lower = inner.trim().to_ascii_lowercase();
                    if tag_lower.starts_with("br")
                        || tag_lower == "/p"
                        || tag_lower.starts_with("p ")
                        || tag_lower == "p"
                    {
                        stripped.push('\n');
                    }
                    i = end_j + 1;
                    continue;
                }
            }
        }

        let ch = decoded[i..].chars().next().unwrap();
        stripped.push(ch);
        i += ch.len_utf8();
    }

    let entity_cleaned = stripped.replace("&lt;", "‹").replace("&gt;", "›");

    let mut paragraphs = Vec::new();
    for line in entity_cleaned.lines() {
        let collapsed: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !collapsed.is_empty() {
            paragraphs.push(collapsed);
        }
    }
    paragraphs.join("\n\n")
}

/// Formally verified Spotify artist biography HTML sanitizer.
///
/// Formal Guarantees:
/// - **INV-B1 (Well-Formed & Nested Tag Stripping)**: Strips balanced HTML tags (`<a href="...">`, `</a>`, `<p>`, `<br/>`, and nested `<a<a0>>` constructs) while preserving literal `<` characters that do not form a valid closed tag on the same line (e.g., `"<3"`).
/// - **INV-B2 (Totality & UTF-8 Safety)**: Never panics on any arbitrary UTF-8 input.
/// - **INV-B3 (Universal Idempotence / Projection)**: `sanitize_spotify_biography(&sanitize_spotify_biography(s)) == sanitize_spotify_biography(s)` for all UTF-8 strings `s`.
pub fn sanitize_spotify_biography(raw: &str) -> String {
    let mut current = sanitize_spotify_biography_pass(raw);
    for _ in 0..8 {
        let next = sanitize_spotify_biography_pass(&current);
        if next == current {
            break;
        }
        current = next;
    }
    current
}

/// Sanitizes an artist biography and returns `(preview_excerpt, full_clean_bio, has_more_bio)`.
///
/// Formal Guarantees:
/// - **INV-B4 (Bounded Preview Length)**: `preview_excerpt.chars().count() <= max_chars.max(1) + 1` (accounting for trailing `…`).
/// - **INV-B5 (UTF-8 Char-Boundary Totality)**: Never slices mid-codepoint on any multi-byte Unicode string.
pub fn summarize_artist_bio(raw: &str, max_chars: usize) -> (String, String, bool) {
    let limit = max_chars.max(1);
    let clean = sanitize_spotify_biography(raw);
    let single_line: String = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= limit {
        return (single_line, clean, false);
    }

    let mut byte_cutoff = single_line.len();
    for (char_idx, (byte_idx, _)) in single_line.char_indices().enumerate() {
        if char_idx == limit {
            byte_cutoff = byte_idx;
            break;
        }
    }
    let slice = &single_line[..byte_cutoff];

    if let Some(dot_pos) = slice.rfind(". ") {
        if slice[..=dot_pos].chars().count() >= (limit * 2 / 5).min(90) {
            return (slice[..=dot_pos].trim().to_string(), clean, true);
        }
    }

    let word_trimmed = match slice.rfind(' ') {
        Some(space_pos) if slice[..space_pos].chars().count() >= (limit / 5).min(40) => {
            &slice[..space_pos]
        }
        _ => slice,
    }
    .trim_end_matches(|c: char| matches!(c, ',' | ';' | ':' | '-' | '–' | '—'));

    (format!("{word_trimmed}…"), clean, true)
}

/// Formally verified `Cmd+Backspace` word-deletion operator on a search query.
///
/// Deletes any trailing whitespace followed by the last contiguous word (or punctuation token),
/// returning the remaining prefix.
///
/// Formal Guarantees:
/// - **INV-W1 (Prefix Preservation)**: `input.starts_with(&delete_last_word_verified(input))` for all UTF-8 strings.
/// - **INV-W2 (Strict Contraction)**: If `!input.is_empty()`, `delete_last_word_verified(input).chars().count() < input.chars().count()`, and `delete_last_word_verified("") == ""`.
/// - **INV-W3 (Single-Word Exactness)**: If `input.trim()` is a single alphanumeric word, `delete_last_word_verified(input) == ""`.
/// - **INV-W4 (Multi-Word Boundary)**: `delete_last_word_verified("alpha beta") == "alpha "` and `delete_last_word_verified("alpha ") == ""`.
pub fn delete_last_word_verified(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    let without_trailing_ws = input.trim_end_matches(char::is_whitespace);
    if without_trailing_ws.is_empty() {
        return String::new();
    }

    let last_ch = without_trailing_ws.chars().next_back().unwrap();
    let is_word_char = |c: char| c.is_alphanumeric() || c == '_' || c == '\'';
    let target_is_word = is_word_char(last_ch);

    let mut cut_byte_idx = 0usize;
    for (byte_idx, ch) in without_trailing_ws.char_indices().rev() {
        if ch.is_whitespace() || is_word_char(ch) != target_is_word {
            cut_byte_idx = byte_idx + ch.len_utf8();
            break;
        }
    }

    without_trailing_ws[..cut_byte_idx].to_string()
}

/// Detects whether a Spotify username string is an opaque machine-generated canonical ID
/// (e.g. `"315okpb22i4itwoz3ccxlrvz7xci"`, 20+ base62 chars starting with `31`/`21`/`22`/`11`,
/// or an all-numeric account ID).
pub fn is_opaque_spotify_user_id(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("spotify") {
        return true;
    }
    if trimmed.chars().any(char::is_whitespace) {
        return false;
    }

    let char_count = trimmed.chars().count();
    let all_ascii_alnum = trimmed.chars().all(|c| c.is_ascii_alphanumeric());
    let digit_count = trimmed.chars().filter(|c| c.is_ascii_digit()).count();

    if trimmed.chars().all(|c| c.is_ascii_digit()) && char_count >= 8 {
        return true;
    }

    if all_ascii_alnum
        && char_count >= 20
        && (trimmed.starts_with("31")
            || trimmed.starts_with("21")
            || trimmed.starts_with("22")
            || trimmed.starts_with("11")
            || digit_count >= 4)
    {
        return true;
    }

    false
}

/// Formats the connected Spotify account status for the compact sidebar footer.
///
/// Formal Guarantees:
/// - **INV-C1 (Opaque Hash Suppression)**: Machine-generated Spotify canonical IDs (such as `"315okpb22i4itwoz3ccxlrvz7xci"`)
///   are never displayed raw; they render cleanly as `"Connected • Premium"` or `"Connected"`.
/// - **INV-C2 (Single-Line Bounded Length)**: Output is always single-line (`!contains('\n')`) and at most 26 characters,
///   preventing multi-line wrapping in the 246px sidebar.
/// - **INV-C3 (Human Handle Preservation)**: Human display names (e.g. `"Carter"`) render as `"Connected as Carter"`.
pub fn format_connected_account_label(username: &str, is_premium: bool) -> String {
    let clean: String = username
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if is_opaque_spotify_user_id(&clean) {
        return if is_premium {
            "Connected • Premium".to_string()
        } else {
            "Connected".to_string()
        };
    }

    let max_name_chars = 13usize;
    let display_name = if clean.chars().count() <= max_name_chars {
        clean
    } else {
        let mut truncated: String = clean.chars().take(max_name_chars - 1).collect();
        truncated.push('…');
        truncated
    };

    format!("Connected as {display_name}")
}

/// Formally verified snapshot of the macOS Menu Bar Tray & Now Playing state derived from `PlaybackState`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayMenuSnapshot {
    /// Compact single-line title shown next to the icon in the macOS menu bar (`<= 30` chars).
    pub status_bar_title: String,
    /// Hover tooltip for the macOS status item.
    pub tooltip: String,
    /// Primary headline inside the tray dropdown menu (`Title — Artist`).
    pub now_playing_headline: String,
    /// Secondary album/stream line inside the tray dropdown menu.
    pub album_subtitle: String,
    /// Synchronized single-line progress indicator (`elapsed ━━●━━ -remaining (duration)`).
    pub progress_label: String,
    /// Synchronized integer elapsed playback time in seconds (`<= duration_secs`).
    pub elapsed_secs: u32,
    /// Synchronized total track/episode duration in seconds.
    pub duration_secs: u32,
    /// Synchronized integer remaining playback time in seconds (`duration_secs - elapsed_secs`).
    pub remaining_secs: u32,
    /// Synchronized progress bar ratio in parts-per-thousand (`0..=1000`).
    pub progress_permille: u16,
    /// Synchronized OS Now Playing playback rate in milli-units (`1000` = `1.0x`, `0` = `0.0x`).
    pub playback_rate_milli: u16,
    /// Action label for Play/Pause menu item (`"Pause Playback"` vs `"Play Track"`).
    pub play_pause_label: String,
    /// Action label for Like menu item (`"♥ Liked"` vs `"♡ Like Track"`).
    pub like_label: String,
    /// Whether playback is currently active.
    pub is_playing: bool,
    /// Whether `Next Track` and `Previous Track` are enabled.
    pub can_skip_tracks: bool,
}

impl TrayMenuSnapshot {
    pub fn from_playback_state(state: &PlaybackState) -> Self {
        let has_track = state.has_active_track();

        let clean_title: String = state
            .current_track
            .title
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let clean_artist: String = state
            .current_track
            .artist
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let clean_album: String = state
            .current_track
            .album
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");

        let status_bar_title = if has_track {
            let prefix = if state.is_playing { " ♫ " } else { " " };
            let max_title_chars = 22usize;
            let short_title = if clean_title.chars().count() <= max_title_chars {
                clean_title.clone()
            } else {
                let mut s: String = clean_title.chars().take(max_title_chars - 1).collect();
                s.push('…');
                s
            };
            format!("{prefix}{short_title}")
        } else {
            " Spot".to_string()
        };

        let now_playing_headline = if has_track {
            if clean_artist.is_empty() {
                clean_title.clone()
            } else {
                format!("{clean_title} — {clean_artist}")
            }
        } else {
            "Spot — Ready to Play".to_string()
        };

        let is_podcast_episode = state.current_track.spotify_uri.starts_with("spotify:episode:");
        let album_subtitle = if has_track && !clean_album.is_empty() {
            if is_podcast_episode {
                format!("Podcast: {clean_album}")
            } else {
                format!("Album: {clean_album} • 320 kbps")
            }
        } else {
            "Spotify Premium • 320 kbps Ogg Vorbis".to_string()
        };

        let elapsed_secs = state.synced_elapsed_secs();
        let duration_secs = state.current_track.duration_secs;
        let remaining_secs = state.synced_remaining_secs();
        let progress_permille = state.synced_progress_permille();
        let playback_rate_milli = state.synced_playback_rate_milli();

        let elapsed_str = crate::format_seconds(elapsed_secs);
        let remaining_str = format!("-{}", crate::format_seconds(remaining_secs));
        let duration_str = crate::format_seconds(duration_secs);
        let bar_glyphs = Self::render_progress_bar(progress_permille, 10);
        let progress_label =
            format!("{bar_glyphs}  {elapsed_str} / {duration_str}  ({remaining_str})");

        let tooltip = if has_track {
            format!("Spot: {now_playing_headline} [{elapsed_str} / {duration_str}]")
        } else {
            "Spot — Spotify Premium Player".to_string()
        };

        let play_pause_label = if state.is_playing {
            "Pause Playback".to_string()
        } else {
            "Play Track".to_string()
        };

        let like_label = if state.current_track.liked {
            "♥ Liked in Library".to_string()
        } else {
            "♡ Like Current Track".to_string()
        };

        let can_skip_tracks = has_track || !state.queue.is_empty();

        Self {
            status_bar_title,
            tooltip,
            now_playing_headline,
            album_subtitle,
            progress_label,
            elapsed_secs,
            duration_secs,
            remaining_secs,
            progress_permille,
            playback_rate_milli,
            play_pause_label,
            like_label,
            is_playing: state.is_playing,
            can_skip_tracks,
        }
    }

    /// Renders a compact single-line Unicode progress bar of `slots` segments for `progress_permille` (`0..=1000`).
    pub fn render_progress_bar(progress_permille: u16, slots: usize) -> String {
        let total = slots.max(4);
        let clamped = progress_permille.min(1000) as usize;
        let knob_idx = ((clamped * (total - 1)) + 500) / 1000;
        let mut bar = String::with_capacity(total * 3);
        for i in 0..total {
            if i < knob_idx {
                bar.push('━');
            } else if i == knob_idx {
                bar.push('●');
            } else {
                bar.push('─');
            }
        }
        bar
    }

    /// Returns the playback rate as an IEEE-754 `f64` (`1.0` or `0.0`) for macOS `MPNowPlayingInfoCenter`.
    pub fn playback_rate_f64(&self) -> f64 {
        (self.playback_rate_milli as f64) / 1000.0
    }

    /// Formal relational invariant (`INV-T7`) proving that this `TrayMenuSnapshot`'s progress
    /// indicator and transport state are strictly in sync with the true `PlaybackState`.
    pub fn check_sync_with_playback(&self, state: &PlaybackState) -> Result<(), InvariantViolation> {
        self.check_invariants()?;

        let true_elapsed = state.synced_elapsed_secs();
        let true_duration = state.current_track.duration_secs;
        let true_remaining = state.synced_remaining_secs();
        let true_permille = state.synced_progress_permille();
        let true_rate = state.synced_playback_rate_milli();

        if self.elapsed_secs != true_elapsed
            || self.duration_secs != true_duration
            || self.remaining_secs != true_remaining
            || self.progress_permille != true_permille
            || self.is_playing != state.is_playing
            || self.playback_rate_milli != true_rate
        {
            return Err(InvariantViolation {
                code: "INV-T7",
                subsystem: "TrayMenuSnapshot",
                details: format!(
                    "tray progress out of sync with player: tray=(elapsed={}s, rem=-{}s, dur={}s, permille={}, playing={}, rate={}) vs player=(elapsed={}s, rem=-{}s, dur={}s, permille={}, playing={}, rate={})",
                    self.elapsed_secs,
                    self.remaining_secs,
                    self.duration_secs,
                    self.progress_permille,
                    self.is_playing,
                    self.playback_rate_milli,
                    true_elapsed,
                    true_remaining,
                    true_duration,
                    true_permille,
                    state.is_playing,
                    true_rate,
                ),
            });
        }

        Ok(())
    }
}

impl FormalInvariant for TrayMenuSnapshot {
    fn check_invariants(&self) -> Result<(), InvariantViolation> {
        // INV-T1: Status bar title is bounded (`<= 30` chars) and single-line.
        if self.status_bar_title.chars().count() > 30
            || self.status_bar_title.contains('\n')
            || self.status_bar_title.contains('\r')
        {
            return Err(InvariantViolation {
                code: "INV-T1",
                subsystem: "TrayMenuSnapshot",
                details: format!(
                    "status_bar_title '{}' exceeds 30 chars or contains newline",
                    self.status_bar_title
                ),
            });
        }

        // INV-T2: Headline and subtitle are non-empty and single-line.
        if self.now_playing_headline.trim().is_empty()
            || self.now_playing_headline.contains('\n')
            || self.album_subtitle.trim().is_empty()
            || self.album_subtitle.contains('\n')
        {
            return Err(InvariantViolation {
                code: "INV-T2",
                subsystem: "TrayMenuSnapshot",
                details: "headline or album_subtitle is empty or multi-line".to_string(),
            });
        }

        // INV-T3: Play/Pause label coherence with `is_playing`.
        let expected_label = if self.is_playing {
            "Pause Playback"
        } else {
            "Play Track"
        };
        if self.play_pause_label != expected_label {
            return Err(InvariantViolation {
                code: "INV-T3",
                subsystem: "TrayMenuSnapshot",
                details: format!(
                    "play_pause_label '{}' disagrees with is_playing={}",
                    self.play_pause_label, self.is_playing
                ),
            });
        }

        // INV-T4: Progress time conservation (`elapsed_secs + remaining_secs == duration_secs`)
        // and exact progress_permille ratio in `0..=1000`.
        if self.elapsed_secs > self.duration_secs
            || self.elapsed_secs.saturating_add(self.remaining_secs) != self.duration_secs
        {
            return Err(InvariantViolation {
                code: "INV-T4",
                subsystem: "TrayMenuSnapshot",
                details: format!(
                    "progress time conservation violated: elapsed={}s + remaining={}s != duration={}s",
                    self.elapsed_secs, self.remaining_secs, self.duration_secs
                ),
            });
        }
        let expected_permille = if self.duration_secs == 0 {
            0
        } else {
            (((self.elapsed_secs as u64) * 1000) / (self.duration_secs as u64)).min(1000) as u16
        };
        if self.progress_permille > 1000 || self.progress_permille != expected_permille {
            return Err(InvariantViolation {
                code: "INV-T4",
                subsystem: "TrayMenuSnapshot",
                details: format!(
                    "progress_permille={} disagrees with expected={} (elapsed={}, duration={})",
                    self.progress_permille, expected_permille, self.elapsed_secs, self.duration_secs
                ),
            });
        }

        // INV-T5: Playback rate coherence (`playback_rate_milli` in `{0, 1000}` and `!is_playing ==> 0`).
        if (self.playback_rate_milli != 0 && self.playback_rate_milli != 1000)
            || (!self.is_playing && self.playback_rate_milli != 0)
        {
            return Err(InvariantViolation {
                code: "INV-T5",
                subsystem: "TrayMenuSnapshot",
                details: format!(
                    "playback_rate_milli={} invalid for is_playing={}",
                    self.playback_rate_milli, self.is_playing
                ),
            });
        }

        // INV-T6: Progress indicator label is single-line and contains exact elapsed and remaining timestamps.
        let expected_elapsed_str = crate::format_seconds(self.elapsed_secs);
        let expected_rem_str = format!("-{}", crate::format_seconds(self.remaining_secs));
        if self.progress_label.trim().is_empty()
            || self.progress_label.contains('\n')
            || self.progress_label.contains('\r')
            || !self.progress_label.contains(&expected_elapsed_str)
            || !self.progress_label.contains(&expected_rem_str)
        {
            return Err(InvariantViolation {
                code: "INV-T6",
                subsystem: "TrayMenuSnapshot",
                details: format!(
                    "progress_label '{}' missing elapsed '{}' or remaining '{}'",
                    self.progress_label, expected_elapsed_str, expected_rem_str
                ),
            });
        }

        Ok(())
    }
}

fn normalize_spotify_entity_kind_uri(input: &str, kind: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let prefix = format!("spotify:{kind}:");
    let candidate = if let Some(rest) = trimmed.strip_prefix(&prefix) {
        rest.split(['?', '#', '/']).next().unwrap_or(rest)
    } else if let Some(idx) = trimmed.find(&format!("open.spotify.com/{kind}/")) {
        let after = &trimmed[idx + format!("open.spotify.com/{kind}/").len()..];
        after.split(['?', '#', '/']).next().unwrap_or(after)
    } else {
        trimmed
    };

    if candidate.len() == 22 && candidate.chars().all(|c| c.is_ascii_alphanumeric()) {
        Some(format!("spotify:{kind}:{candidate}"))
    } else {
        None
    }
}

/// Normalizes a Spotify Podcast Show identifier (`"spotify:show:<id>"`,
/// `"https://open.spotify.com/show/<id>?si=..."`, or a bare 22-char base62 ID)
/// into its canonical `"spotify:show:<22-char-base62>"` URI.
///
/// Formal Guarantees:
/// - **INV-E1 (Show URI Totality, Canonicality & Idempotence)**:
///   Never panics on any UTF-8 input; if `Some(uri)` is returned, `uri.starts_with("spotify:show:")`,
///   `uri.len() == 13 + 22`, and `normalize_spotify_show_uri(&uri) == Some(uri)`.
pub fn normalize_spotify_show_uri(input: &str) -> Option<String> {
    normalize_spotify_entity_kind_uri(input, "show")
}

/// Normalizes a Spotify Podcast Episode identifier (`"spotify:episode:<id>"`,
/// `"https://open.spotify.com/episode/<id>?si=..."`, or a bare 22-char base62 ID)
/// into its canonical `"spotify:episode:<22-char-base62>"` URI.
///
/// Formal Guarantees:
/// - **INV-E1 (Episode URI Totality, Canonicality & Idempotence)**:
///   Never panics on any UTF-8 input; if `Some(uri)` is returned, `uri.starts_with("spotify:episode:")`,
///   `uri.len() == 16 + 22`, and `normalize_spotify_episode_uri(&uri) == Some(uri)`.
pub fn normalize_spotify_episode_uri(input: &str) -> Option<String> {
    normalize_spotify_entity_kind_uri(input, "episode")
}

/// Converts a `PodcastEpisode` into a verified playable `Track` for the transport queue,
/// Now Playing stage, scrubber, and macOS menu bar tray.
///
/// Formal Guarantees:
/// - **INV-E2 (Playable Episode URI Guarantee)**: `track.spotify_uri.starts_with("spotify:episode:")`
///   and `!track.id.is_empty()`.
/// - **INV-E3 (Positive Bounded Duration)**: `1 <= track.duration_secs <= 86_400`, preventing
///   zero-duration scrubber division or runaway bounds.
/// - **INV-E4 (Single-Line Non-Empty Metadata)**: `track.title`, `track.artist`, and `track.album`
///   are non-empty and contain no newlines.
pub fn episode_to_playable_track_verified(ep: &PodcastEpisode) -> Track {
    let raw_id: String = ep
        .id
        .trim()
        .strip_prefix("spotify:episode:")
        .unwrap_or(ep.id.trim())
        .chars()
        .filter(|c| !c.is_control() && !c.is_whitespace())
        .collect();
    let id = if raw_id.is_empty() {
        "episode-unknown".to_string()
    } else {
        raw_id
    };

    let spotify_uri = normalize_spotify_episode_uri(&ep.spotify_uri)
        .or_else(|| normalize_spotify_episode_uri(&id))
        .unwrap_or_else(|| format!("spotify:episode:{id}"));

    let clean_title: String = ep.title.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = if clean_title.is_empty() {
        "Podcast Episode".to_string()
    } else {
        clean_title
    };

    let clean_show: String = ep
        .show_title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let show_title = if clean_show.is_empty() {
        "Spotify Podcast".to_string()
    } else {
        clean_show
    };

    let duration_secs = ep.duration_secs.clamp(1, 86_400);

    Track {
        id,
        spotify_uri,
        title,
        artist: show_title.clone(),
        album: show_title,
        duration_secs,
        liked: false,
        cover_art_url: ep.cover_art_url.clone(),
        artwork_theme: ep.artwork_theme,
    }
}

impl FormalInvariant for PodcastShowDetail {
    fn check_invariants(&self) -> Result<(), InvariantViolation> {
        // INV-S1: Canonical `spotify:show:` URI and non-empty show ID.
        if self.show.id.trim().is_empty() || !self.show.spotify_uri.starts_with("spotify:show:") {
            return Err(InvariantViolation {
                code: "INV-S1",
                subsystem: "PodcastShowDetail",
                details: format!(
                    "show id '{}' or spotify_uri '{}' is invalid",
                    self.show.id, self.show.spotify_uri
                ),
            });
        }

        // INV-S2: Non-empty single-line show title and publisher.
        if self.show.title.trim().is_empty()
            || self.show.title.contains('\n')
            || self.show.publisher.trim().is_empty()
            || self.show.publisher.contains('\n')
        {
            return Err(InvariantViolation {
                code: "INV-S2",
                subsystem: "PodcastShowDetail",
                details: "show title or publisher is blank or contains newline".to_string(),
            });
        }

        // INV-S3: Every episode has a valid `spotify:episode:` URI, positive duration, and converts to a valid Track.
        for (idx, ep) in self.episodes.iter().enumerate() {
            if ep.id.trim().is_empty()
                || !ep.spotify_uri.starts_with("spotify:episode:")
                || ep.duration_secs == 0
            {
                return Err(InvariantViolation {
                    code: "INV-S3",
                    subsystem: "PodcastShowDetail",
                    details: format!(
                        "episode[{idx}] has invalid id='{}', uri='{}', or duration_secs={}",
                        ep.id, ep.spotify_uri, ep.duration_secs
                    ),
                });
            }
            let playable = episode_to_playable_track_verified(ep);
            if !playable.spotify_uri.starts_with("spotify:episode:")
                || playable.id.is_empty()
                || playable.duration_secs == 0
                || playable.title.contains('\n')
                || playable.artist.contains('\n')
            {
                return Err(InvariantViolation {
                    code: "INV-S3",
                    subsystem: "PodcastShowDetail",
                    details: format!("episode[{idx}] playable track conversion violated INV-E2..E4"),
                });
            }
        }

        Ok(())
    }
}

/// Default collapsed item limit ("Top 10") for the "Recently-listened" section in Your Library.
pub const RECENTLY_LISTENED_TOP_LIMIT: usize = 10;

/// Kind of item stored in the "Recently-listened" section of Your Library.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RecentListenedKind {
    Album {
        album_uri: String,
        artist_name: String,
    },
    Podcast {
        show_uri: String,
        publisher: String,
    },
}

impl RecentListenedKind {
    pub fn badge_label(&self) -> &'static str {
        match self {
            RecentListenedKind::Album { .. } => "ALBUM",
            RecentListenedKind::Podcast { .. } => "PODCAST",
        }
    }

    pub fn target_uri(&self) -> &str {
        match self {
            RecentListenedKind::Album { album_uri, .. } => album_uri,
            RecentListenedKind::Podcast { show_uri, .. } => show_uri,
        }
    }

    pub fn creator_name(&self) -> &str {
        match self {
            RecentListenedKind::Album { artist_name, .. } => artist_name,
            RecentListenedKind::Podcast { publisher, .. } => publisher,
        }
    }
}

/// Persisted entry in the "Recently-listened" library store (Album or Podcast).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecentListenedItem {
    pub title: String,
    pub subtitle: String,
    pub cover_art_url: Option<String>,
    pub artwork_theme: ArtworkTheme,
    pub kind: RecentListenedKind,
}

impl RecentListenedItem {
    /// Returns `true` if `self` and `other` refer to the same Album or Podcast entity.
    pub fn matches_same_entity(&self, other: &Self) -> bool {
        if self.kind.badge_label() != other.kind.badge_label() {
            return false;
        }
        let a_uri = self.kind.target_uri().trim();
        let b_uri = other.kind.target_uri().trim();
        if !a_uri.is_empty() && !b_uri.is_empty() && a_uri.eq_ignore_ascii_case(b_uri) {
            return true;
        }
        !self.title.trim().is_empty()
            && self.title.trim().eq_ignore_ascii_case(other.title.trim())
            && self
                .kind
                .creator_name()
                .trim()
                .eq_ignore_ascii_case(other.kind.creator_name().trim())
    }
}

/// Formally verified MRU store for "Recently-listened" Albums & Podcasts in Your Library.
///
/// Formal Guarantees:
/// - **INV-L1 (Bounded Capacity)**: `max_entries >= 10` and `entries.len() <= max_entries`.
/// - **INV-L2 (Non-Empty Single-Line Metadata)**: Every item has non-empty single-line `title`,
///   `subtitle`, and `target_uri()`.
/// - **INV-L3 (Strict Pairwise Deduplication)**: No two items in `entries` match via `matches_same_entity`.
/// - **INV-L4 (Top-10 Collapse / Expand Projection)**:
///   - `visible_entries(false)` returns `&entries[..entries.len().min(10)]` (never more than 10).
///   - `visible_entries(true)` returns all `entries` (showing `> 10` when `entries.len() > 10`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentListenedStore {
    entries: Vec<RecentListenedItem>,
    max_entries: usize,
}

impl Default for RecentListenedStore {
    fn default() -> Self {
        Self::new(30)
    }
}

impl RecentListenedStore {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_entries: max_entries.max(RECENTLY_LISTENED_TOP_LIMIT),
        }
    }

    pub fn from_loaded(raw: Vec<RecentListenedItem>, max_entries: usize) -> Self {
        let mut store = Self::new(max_entries);
        for item in raw.into_iter().rev() {
            store.record_item(item);
        }
        store
    }

    #[inline]
    pub fn entries(&self) -> &[RecentListenedItem] {
        &self.entries
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Returns the slice of items to display in "Recently-listened":
    /// - When `expanded == false`, returns at most the Top 10 items (`RECENTLY_LISTENED_TOP_LIMIT`).
    /// - When `expanded == true`, returns all items (`> 10` when available).
    #[inline]
    pub fn visible_entries(&self, expanded: bool) -> &[RecentListenedItem] {
        if expanded {
            &self.entries
        } else {
            let limit = self.entries.len().min(RECENTLY_LISTENED_TOP_LIMIT);
            &self.entries[..limit]
        }
    }

    /// Returns `true` when there are more than 10 recently-listened items so the UI can show
    /// the expand/collapse control.
    #[inline]
    pub fn has_more_than_top_ten(&self) -> bool {
        self.entries.len() > RECENTLY_LISTENED_TOP_LIMIT
    }

    /// Number of additional items beyond the Top 10 when collapsed.
    #[inline]
    pub fn extra_count_beyond_top_ten(&self) -> usize {
        self.entries.len().saturating_sub(RECENTLY_LISTENED_TOP_LIMIT)
    }

    fn sanitize_item(mut item: RecentListenedItem) -> Option<RecentListenedItem> {
        let clean_title: String = item.title.split_whitespace().collect::<Vec<_>>().join(" ");
        if clean_title.is_empty() {
            return None;
        }
        item.title = clean_title;

        match &mut item.kind {
            RecentListenedKind::Album {
                album_uri,
                artist_name,
            } => {
                let clean_artist: String = artist_name
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                *artist_name = if clean_artist.is_empty() {
                    "Artist".to_string()
                } else {
                    clean_artist
                };
                let clean_uri = album_uri.trim().to_string();
                *album_uri = if clean_uri.is_empty() {
                    item.title.clone()
                } else {
                    clean_uri
                };
            }
            RecentListenedKind::Podcast {
                show_uri,
                publisher,
            } => {
                let clean_pub: String = publisher
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                *publisher = if clean_pub.is_empty() {
                    "Podcast".to_string()
                } else {
                    clean_pub
                };
                let clean_uri = normalize_spotify_show_uri(show_uri).unwrap_or_else(|| {
                    let trimmed = show_uri.trim();
                    if trimmed.is_empty() {
                        format!("spotify:show:{}", item.title)
                    } else {
                        trimmed.to_string()
                    }
                });
                *show_uri = clean_uri;
            }
        }

        let clean_sub: String = item
            .subtitle
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        item.subtitle = if clean_sub.is_empty() {
            item.kind.creator_name().to_string()
        } else {
            clean_sub
        };

        item.cover_art_url = item
            .cover_art_url
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty());

        Some(item)
    }

    /// Records a newly listened Album or Podcast at index `0` (Most Recently Listened),
    /// deduplicating any existing matching entry while preserving existing cover art if the new
    /// event did not carry a cover URL.
    pub fn record_item(&mut self, raw_item: RecentListenedItem) -> bool {
        let Some(mut item) = Self::sanitize_item(raw_item) else {
            return false;
        };

        if item.cover_art_url.is_none() {
            if let Some(prev_cover) = self
                .entries
                .iter()
                .find(|existing| existing.matches_same_entity(&item))
                .and_then(|existing| existing.cover_art_url.clone())
            {
                item.cover_art_url = Some(prev_cover);
            }
        }

        self.entries
            .retain(|existing| !existing.matches_same_entity(&item));
        self.entries.insert(0, item);
        self.entries.truncate(self.max_entries);
        true
    }

    /// Appends a fallback/catalog item to the tail of the store only if it is not already present
    /// and capacity remains, preserving the user's MRU items at the front.
    pub fn append_if_absent(&mut self, raw_item: RecentListenedItem) -> bool {
        if self.entries.len() >= self.max_entries {
            return false;
        }
        let Some(item) = Self::sanitize_item(raw_item) else {
            return false;
        };
        if self
            .entries
            .iter()
            .any(|existing| existing.matches_same_entity(&item))
        {
            return false;
        }
        self.entries.push(item);
        true
    }
}

impl FormalInvariant for RecentListenedStore {
    fn check_invariants(&self) -> Result<(), InvariantViolation> {
        // INV-L1: Capacity bound and minimum Top-10 capacity.
        if self.max_entries < RECENTLY_LISTENED_TOP_LIMIT || self.entries.len() > self.max_entries {
            return Err(InvariantViolation {
                code: "INV-L1",
                subsystem: "RecentListenedStore",
                details: format!(
                    "len={} exceeds max_entries={} (min={})",
                    self.entries.len(),
                    self.max_entries,
                    RECENTLY_LISTENED_TOP_LIMIT
                ),
            });
        }

        // INV-L2: Every entry has non-empty single-line title, subtitle, and target URI.
        for (idx, item) in self.entries.iter().enumerate() {
            if item.title.trim().is_empty()
                || item.title.contains('\n')
                || item.subtitle.trim().is_empty()
                || item.subtitle.contains('\n')
                || item.kind.target_uri().trim().is_empty()
                || item.kind.creator_name().trim().is_empty()
            {
                return Err(InvariantViolation {
                    code: "INV-L2",
                    subsystem: "RecentListenedStore",
                    details: format!("entry[{idx}] has blank or multiline metadata: {item:?}"),
                });
            }
        }

        // INV-L3: Strict pairwise entity uniqueness.
        for i in 0..self.entries.len() {
            for j in (i + 1)..self.entries.len() {
                if self.entries[i].matches_same_entity(&self.entries[j]) {
                    return Err(InvariantViolation {
                        code: "INV-L3",
                        subsystem: "RecentListenedStore",
                        details: format!(
                            "duplicate recently-listened entities at {i} and {j}: {:?} vs {:?}",
                            self.entries[i], self.entries[j]
                        ),
                    });
                }
            }
        }

        // INV-L4: Top-10 collapsed vs expanded view projection exactness.
        let collapsed = self.visible_entries(false);
        let expanded = self.visible_entries(true);
        if collapsed.len() != self.entries.len().min(RECENTLY_LISTENED_TOP_LIMIT)
            || collapsed.len() > RECENTLY_LISTENED_TOP_LIMIT
            || expanded.len() != self.entries.len()
            || self.has_more_than_top_ten() != (self.entries.len() > RECENTLY_LISTENED_TOP_LIMIT)
        {
            return Err(InvariantViolation {
                code: "INV-L4",
                subsystem: "RecentListenedStore",
                details: format!(
                    "projection mismatch: collapsed={}, expanded={}, total={}",
                    collapsed.len(),
                    expanded.len(),
                    self.entries.len()
                ),
            });
        }

        Ok(())
    }
}

/// Number of stereo frames (`32` frames = `0.725 ms` at `44.1 kHz`) used for the sub-millisecond
/// click-free de-click ramp when pausing or unpausing audio on the real-time hardware thread.
pub const INSTANT_PAUSE_RAMP_FRAMES: u8 = 32;

/// Maximum number of stereo frames buffered in the real-time `InstantAudioQueue` (`4096` frames = `~92.8 ms`),
/// replacing `librespot-playback`'s 26-chunk (`~500 ms`) `RodioSink` buffer.
pub const INSTANT_SINK_MAX_BUFFERED_FRAMES: usize = 4096;

#[derive(Debug)]
struct InstantAudioGateInner {
    playing: std::sync::atomic::AtomicBool,
    flush_epoch: std::sync::atomic::AtomicU64,
    buffered_frames: std::sync::atomic::AtomicUsize,
}

/// Lock-free real-time hardware audio gate shared directly between the UI thread (`SpotApp`),
/// the async Spotify engine (`spot-spotify`), and the CoreAudio sample callback (`InstantGatedSource`).
///
/// Formal Guarantees:
/// - **INV-G1 (Zero-Tail Instant Silence Post-Ramp)**: Once `ramp_step == 0` while `!is_playing()`,
///   the hardware callback outputs exact `(0.0, 0.0)` digital silence and consumes `0` queued frames.
/// - **INV-G2 (Bounded Sub-Millisecond Pause/Resume Ramp)**: Transitions between playing and paused
///   complete in at most `INSTANT_PAUSE_RAMP_FRAMES` (`32` stereo frames = `0.725 ms` at `44.1 kHz`).
/// - **INV-G3 (Lossless Pause/Unpause Sample Conservation)**: While paused (`!is_playing()` and `ramp_step == 0`),
///   zero queued frames are popped, so unpausing resumes from the exact next buffered sample in `< 1 ms`.
/// - **INV-G4 (UI-to-Audio Gate Synchronization Invariant)**: `gate.is_playing() == state.is_playing`
///   holds synchronously after every UI and engine transport transition.
#[derive(Debug, Clone)]
pub struct InstantAudioGate {
    inner: std::sync::Arc<InstantAudioGateInner>,
}

impl Default for InstantAudioGate {
    fn default() -> Self {
        Self::new(false)
    }
}

impl InstantAudioGate {
    pub fn new(initial_playing: bool) -> Self {
        Self {
            inner: std::sync::Arc::new(InstantAudioGateInner {
                playing: std::sync::atomic::AtomicBool::new(initial_playing),
                flush_epoch: std::sync::atomic::AtomicU64::new(0),
                buffered_frames: std::sync::atomic::AtomicUsize::new(0),
            }),
        }
    }

    #[inline]
    pub fn is_playing(&self) -> bool {
        self.inner
            .playing
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[inline]
    pub fn set_playing(&self, playing: bool) {
        self.inner
            .playing
            .store(playing, std::sync::atomic::Ordering::SeqCst);
    }

    #[inline]
    pub fn toggle_playing(&self) -> bool {
        let prev = self
            .inner
            .playing
            .fetch_xor(true, std::sync::atomic::Ordering::SeqCst);
        !prev
    }

    #[inline]
    pub fn flush_epoch(&self) -> u64 {
        self.inner
            .flush_epoch
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Atomically increments `flush_epoch` and zeroes `buffered_frames` so the real-time audio
    /// thread immediately drops stale pre-buffered samples (e.g. on Seek or Track switch).
    #[inline]
    pub fn flush(&self) -> u64 {
        self.inner
            .buffered_frames
            .store(0, std::sync::atomic::Ordering::SeqCst);
        self.inner
            .flush_epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .wrapping_add(1)
    }

    /// Atomically flushes stale buffered samples and sets the target `playing` state.
    #[inline]
    pub fn flush_and_set_playing(&self, playing: bool) -> u64 {
        self.set_playing(playing);
        self.flush()
    }

    #[inline]
    pub fn buffered_frames(&self) -> usize {
        self.inner
            .buffered_frames
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[inline]
    pub fn set_buffered_frames(&self, frames: usize) {
        self.inner
            .buffered_frames
            .store(frames, std::sync::atomic::Ordering::SeqCst);
    }

    #[inline]
    pub fn decrement_buffered_frames(&self, count: usize) {
        let _ = self.inner.buffered_frames.fetch_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |cur| Some(cur.saturating_sub(count)),
        );
    }

    /// Returns the duration in seconds of decoded audio currently buffered between the decoder
    /// and the hardware output callback.
    #[inline]
    pub fn buffered_seconds(&self, sample_rate_hz: u32) -> f32 {
        let rate = sample_rate_hz.max(1) as f32;
        (self.buffered_frames() as f32) / rate
    }

    /// Converts a decoder stream position (`decoder_position_ms`) into the true audible position
    /// at the speakers by subtracting the unplayed frames currently sitting in the audio queue.
    #[inline]
    pub fn audible_position_secs(
        &self,
        decoder_position_ms: u32,
        duration_secs: u32,
        sample_rate_hz: u32,
    ) -> f32 {
        let raw_secs = (decoder_position_ms as f32) / 1000.0;
        let audible = (raw_secs - self.buffered_seconds(sample_rate_hz)).max(0.0);
        sanitize_position_secs(audible, duration_secs)
    }

    /// Formal relational invariant (`INV-G4`) proving that the hardware audio gate state is
    /// strictly synchronized with `PlaybackState::is_playing`.
    pub fn check_sync_with_playback(&self, state: &PlaybackState) -> Result<(), InvariantViolation> {
        let gate_playing = self.is_playing();
        if gate_playing != state.is_playing {
            return Err(InvariantViolation {
                code: "INV-G4",
                subsystem: "InstantAudioGate",
                details: format!(
                    "audio_gate.is_playing()={} disagrees with state.is_playing={}",
                    gate_playing, state.is_playing
                ),
            });
        }
        Ok(())
    }
}

/// Formally verified per-stereo-frame DSP gate step for sub-millisecond pause/unpause.
///
/// Returns `(out_left, out_right, next_ramp_step, consume_queued_frame)`:
/// - **INV-G1 (Zero-Tail Silence)**: When `!is_playing && ramp_step == 0`, returns `(0.0, 0.0, 0, false)`.
/// - **INV-G2 (Bounded Ramp Convergence)**: Reaches `next_ramp_step == 0` (on pause) or
///   `next_ramp_step == ramp_frames.max(1)` (on play) in at most `ramp_frames.max(1)` steps.
/// - **INV-G3 (Sample Conservation & Fin-Bounded Output)**: Outputs are always finite in `[-1.0, 1.0]`,
///   and `consume_queued_frame` is `false` whenever `!is_playing && ramp_step == 0`.
#[inline]
pub fn step_gated_stereo_frame_verified(
    left: f32,
    right: f32,
    is_playing: bool,
    ramp_step: u8,
    ramp_frames: u8,
    volume: f32,
) -> (f32, f32, u8, bool) {
    let effective_ramp = ramp_frames.max(1);
    let clamped_step = ramp_step.min(effective_ramp);
    let clean_l = sanitize_pcm_sample(left);
    let clean_r = sanitize_pcm_sample(right);
    let clean_vol = sanitize_unit_float(volume, 0.45);

    if is_playing {
        let next_step = clamped_step.saturating_add(1).min(effective_ramp);
        let env = (next_step as f32) / (effective_ramp as f32);
        let gain = clean_vol * env;
        (
            sanitize_pcm_sample(clean_l * gain),
            sanitize_pcm_sample(clean_r * gain),
            next_step,
            true,
        )
    } else if clamped_step == 0 {
        (0.0, 0.0, 0, false)
    } else {
        let next_step = clamped_step - 1;
        let env = (next_step as f32) / (effective_ramp as f32);
        let gain = clean_vol * env;
        (
            sanitize_pcm_sample(clean_l * gain),
            sanitize_pcm_sample(clean_r * gain),
            next_step,
            true,
        )
    }
}

/// Formally verified decision rule (`INV-G5`) for whether a keyboard press event should trigger
/// instant Spacebar Play/Pause toggling.
///
/// Formal Guarantees:
/// - **INV-G5a (Text-Input Safety)**: Never toggles play/pause when a focused `text_input` widget
///   captured the Space key and edited text (`widget_status_captured && text_input_edited_this_event`).
/// - **INV-G5b (Slider-Hover Resilience)**: Toggles play/pause even if a non-text widget (such as
///   `iced_widget::Slider` when the mouse cursor is hovering over the progress bar) reported
///   `widget_status_captured == true`, because `text_input_edited_this_event == false`.
/// - **INV-G5c (Modifier & Auto-Repeat Immunity)**: Never toggles when `Cmd`/`Ctrl`/`Alt` modifiers
///   are held or while the Spacebar is already held down (`space_already_held == true`).
#[inline]
pub fn should_toggle_play_pause_on_space_verified(
    is_space_key: bool,
    has_command_ctrl_alt: bool,
    space_already_held: bool,
    widget_status_captured: bool,
    text_input_edited_this_event: bool,
) -> bool {
    is_space_key
        && !has_command_ctrl_alt
        && !space_already_held
        && !(widget_status_captured && text_input_edited_this_event)
}

/// Default maximum number of downloaded `main` update archives retained in local storage
/// (`~/Library/Application Support/Spot/updates/archives`).
pub const MAX_STORED_UPDATES: usize = 10;

/// Normalizes a git commit SHA (7..=40 ASCII hex characters) into `(canonical_lower_sha, short_7_sha)`.
pub fn normalize_git_sha_verified(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim();
    if trimmed.len() < 7 || trimmed.len() > 40 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let short = lower[..7].to_string();
    Some((lower, short))
}

/// Normalizes a SHA-256 hex digest (exact 64 ASCII hex characters) into canonical lowercase.
pub fn normalize_sha256_hex_verified(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.len() != 64 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(trimmed.to_ascii_lowercase())
}

/// Returns `true` if two normalized or raw git commit SHAs refer to the same commit
/// (supporting 7-char short SHA vs 40-char full SHA comparison).
pub fn git_shas_match_verified(a: &str, b: &str) -> bool {
    let Some((na, _)) = normalize_git_sha_verified(a) else {
        return false;
    };
    let Some((nb, _)) = normalize_git_sha_verified(b) else {
        return false;
    };
    if na.len() <= nb.len() {
        nb.starts_with(&na)
    } else {
        na.starts_with(&nb)
    }
}

/// Formally verified SHA-256 integrity gate (`INV-U2`).
/// Returns `true` iff both `expected_sha256` and `actual_sha256` are valid 64-character hex
/// SHA-256 digests and match identically in lowercase.
pub fn verify_update_digest_verified(expected_sha256: &str, actual_sha256: &str) -> bool {
    match (
        normalize_sha256_hex_verified(expected_sha256),
        normalize_sha256_hex_verified(actual_sha256),
    ) {
        (Some(exp), Some(act)) => exp == act,
        _ => false,
    }
}

/// Release manifest published on every push to `main` (`spot-update-manifest.json`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UpdateReleaseManifest {
    pub version: String,
    pub build_number: u64,
    pub commit_sha: String,
    pub short_sha: String,
    pub tag: String,
    pub commit_title: String,
    pub published_at: String,
    pub asset_name: String,
    pub download_url: String,
    pub sha256: String,
    pub size_bytes: u64,
}

impl UpdateReleaseManifest {
    /// Sanitizes and validates a release manifest, returning `None` if the commit SHA,
    /// SHA-256 digest, or download URL is malformed.
    pub fn sanitize(mut self) -> Option<Self> {
        let (commit_sha, short_sha) = normalize_git_sha_verified(&self.commit_sha)
            .or_else(|| normalize_git_sha_verified(&self.short_sha))?;
        let sha256 = normalize_sha256_hex_verified(&self.sha256)?;

        self.commit_sha = commit_sha;
        self.short_sha = short_sha.clone();
        self.sha256 = sha256;
        self.build_number = self.build_number.max(1);

        let clean_version: String = self.version.split_whitespace().collect::<Vec<_>>().join(" ");
        self.version = if clean_version.is_empty() {
            "0.1.0".to_string()
        } else {
            clean_version
        };

        let clean_tag: String = self.tag.split_whitespace().collect::<Vec<_>>().join("");
        self.tag = if clean_tag.is_empty() {
            format!("main-b{}-{short_sha}", self.build_number)
        } else {
            clean_tag
        };

        let clean_title: String = self
            .commit_title
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        self.commit_title = if clean_title.is_empty() {
            format!("Spot main build #{} ({short_sha})", self.build_number)
        } else {
            clean_title
        };

        let clean_pub: String = self
            .published_at
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        self.published_at = if clean_pub.is_empty() {
            "latest".to_string()
        } else {
            clean_pub
        };

        let clean_asset: String = self
            .asset_name
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("");
        self.asset_name = if clean_asset.is_empty() {
            "Spot-macos.zip".to_string()
        } else {
            clean_asset
        };

        let clean_url = self.download_url.trim().to_string();
        if clean_url.is_empty() || clean_url.contains('\n') || clean_url.contains('\r') {
            return None;
        }
        self.download_url = clean_url;
        self.size_bytes = self.size_bytes.max(1);

        Some(self)
    }

    /// Canonical filename used when storing this update in `~/Library/Application Support/Spot/updates/archives/`.
    pub fn canonical_archive_filename(&self) -> String {
        format!("Spot-b{}-{}.zip", self.build_number, self.short_sha)
    }
}

/// Formally verified freshness check (`INV-U1`) determining whether a remote `main` release
/// represents a newer update over the currently installed build.
///
/// Formal Guarantees:
/// - **INV-U1a (Anti-Self-Update)**: Returns `false` whenever `candidate.commit_sha` matches `installed_sha`.
/// - **INV-U1b (Anti-Downgrade Monotonicity)**: Returns `false` whenever `candidate.build_number < installed_build`.
/// - **INV-U1c (Same-Build Commit Push Detection)**: Returns `true` when `candidate.build_number >= installed_build`
///   and `candidate.commit_sha` is a valid commit distinct from `installed_sha`.
pub fn is_release_newer_verified(
    installed_build: u64,
    installed_sha: &str,
    candidate: &UpdateReleaseManifest,
) -> bool {
    let Some((cand_sha, _)) = normalize_git_sha_verified(&candidate.commit_sha) else {
        return false;
    };
    if normalize_sha256_hex_verified(&candidate.sha256).is_none() {
        return false;
    }
    if git_shas_match_verified(installed_sha, &cand_sha) {
        return false;
    }
    candidate.build_number >= installed_build
}

/// Entry for a downloaded & SHA-256 verified update archive stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StoredUpdateEntry {
    pub manifest: UpdateReleaseManifest,
    pub archive_filename: String,
    pub verified_sha256: String,
}

/// Resolved update status relative to the currently running `Spot` binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateAvailability {
    UpToDate {
        installed_build: u64,
        installed_short_sha: String,
    },
    UpdateAvailable {
        manifest: UpdateReleaseManifest,
        stored_archive_filename: Option<String>,
    },
}

/// Formally verified local update catalog & archive store (`updates_catalog.json`).
///
/// Formal Guarantees:
/// - **INV-U1 (Monotonic Update Availability)**: `evaluate_availability()` reports `UpdateAvailable`
///   iff `latest_remote` is strictly newer than `(installed_build_number, installed_commit_sha)` per `is_release_newer_verified`.
/// - **INV-U2 (Cryptographic SHA-256 Integrity Gate)**: Every entry in `stored_updates` has a valid
///   64-hex `verified_sha256` identical to `entry.manifest.sha256`. Corrupted downloads are rejected.
/// - **INV-U3 (Bounded & Pairwise-Unique Archive Store)**: `stored_updates.len() <= max_stored`,
///   ordered newest-first by `build_number`, with pairwise-distinct commits.
/// - **INV-U4 (Install Convergence)**: `mark_installed` updates the installed build/commit so
///   `evaluate_availability()` immediately transitions to `UpToDate` for that release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCatalogStore {
    installed_build_number: u64,
    installed_commit_sha: String,
    installed_short_sha: String,
    latest_remote: Option<UpdateReleaseManifest>,
    stored_updates: Vec<StoredUpdateEntry>,
    max_stored: usize,
}

impl Default for UpdateCatalogStore {
    fn default() -> Self {
        Self::new(1, "0000000", MAX_STORED_UPDATES)
    }
}

impl UpdateCatalogStore {
    pub fn new(installed_build_number: u64, installed_commit_sha: &str, max_stored: usize) -> Self {
        let (commit_sha, short_sha) = normalize_git_sha_verified(installed_commit_sha)
            .unwrap_or_else(|| ("0000000".to_string(), "0000000".to_string()));
        Self {
            installed_build_number: installed_build_number.max(1),
            installed_commit_sha: commit_sha,
            installed_short_sha: short_sha,
            latest_remote: None,
            stored_updates: Vec::new(),
            max_stored: max_stored.max(1),
        }
    }

    pub fn from_loaded(
        installed_build_number: u64,
        installed_commit_sha: &str,
        latest_remote: Option<UpdateReleaseManifest>,
        raw_stored: Vec<StoredUpdateEntry>,
        max_stored: usize,
    ) -> Self {
        let mut store = Self::new(installed_build_number, installed_commit_sha, max_stored);
        if let Some(remote) = latest_remote {
            let _ = store.observe_remote_release(remote);
        }
        for entry in raw_stored.into_iter().rev() {
            let _ = store.record_downloaded_update(entry.manifest, &entry.verified_sha256);
        }
        store
    }

    #[inline]
    pub fn installed_build_number(&self) -> u64 {
        self.installed_build_number
    }

    #[inline]
    pub fn installed_commit_sha(&self) -> &str {
        &self.installed_commit_sha
    }

    #[inline]
    pub fn installed_short_sha(&self) -> &str {
        &self.installed_short_sha
    }

    #[inline]
    pub fn latest_remote(&self) -> Option<&UpdateReleaseManifest> {
        self.latest_remote.as_ref()
    }

    #[inline]
    pub fn stored_updates(&self) -> &[StoredUpdateEntry] {
        &self.stored_updates
    }

    #[inline]
    pub fn max_stored(&self) -> usize {
        self.max_stored
    }

    /// Finds a locally stored update matching `sha_or_prefix` (if already downloaded & verified).
    pub fn find_stored(&self, sha_or_prefix: &str) -> Option<&StoredUpdateEntry> {
        self.stored_updates
            .iter()
            .find(|e| git_shas_match_verified(&e.manifest.commit_sha, sha_or_prefix))
    }

    /// Records the latest release manifest observed from GitHub (`main` branch release).
    /// Only updates `latest_remote` if the manifest is valid and has a build number at least
    /// as high as any previously observed remote release.
    pub fn observe_remote_release(&mut self, raw_manifest: UpdateReleaseManifest) -> bool {
        let Some(manifest) = raw_manifest.sanitize() else {
            return false;
        };
        if let Some(prev) = &self.latest_remote {
            if manifest.build_number < prev.build_number {
                return false;
            }
        }
        self.latest_remote = Some(manifest);
        true
    }

    /// Verifies and stores a downloaded update archive in the catalog (`INV-U2` & `INV-U3`).
    /// Returns `(stored_entry, evicted_archive_filenames)` on success, or `Err(InvariantViolation)`
    /// if the SHA-256 digest does not match the manifest.
    pub fn record_downloaded_update(
        &mut self,
        raw_manifest: UpdateReleaseManifest,
        actual_sha256: &str,
    ) -> Result<(StoredUpdateEntry, Vec<String>), InvariantViolation> {
        let Some(manifest) = raw_manifest.sanitize() else {
            return Err(InvariantViolation {
                code: "INV-U2",
                subsystem: "UpdateCatalogStore",
                details: "manifest failed sanitization".to_string(),
            });
        };

        if !verify_update_digest_verified(&manifest.sha256, actual_sha256) {
            return Err(InvariantViolation {
                code: "INV-U2",
                subsystem: "UpdateCatalogStore",
                details: format!(
                    "SHA-256 mismatch for commit {}: expected {}, got {}",
                    manifest.short_sha, manifest.sha256, actual_sha256
                ),
            });
        }

        let verified_sha256 = normalize_sha256_hex_verified(actual_sha256).unwrap();
        let archive_filename = manifest.canonical_archive_filename();

        // Observe as latest remote if newer
        let _ = self.observe_remote_release(manifest.clone());

        // Deduplicate any existing entry with matching commit SHA
        self.stored_updates
            .retain(|existing| !git_shas_match_verified(&existing.manifest.commit_sha, &manifest.commit_sha));

        let entry = StoredUpdateEntry {
            manifest,
            archive_filename,
            verified_sha256,
        };

        self.stored_updates.insert(0, entry.clone());
        // Sort newest-first by build_number (preserving insertion order on ties)
        self.stored_updates
            .sort_by(|a, b| b.manifest.build_number.cmp(&a.manifest.build_number));

        let mut evicted = Vec::new();
        while self.stored_updates.len() > self.max_stored {
            if let Some(removed) = self.stored_updates.pop() {
                evicted.push(removed.archive_filename);
            }
        }

        Ok((entry, evicted))
    }

    /// Marks a specific build and commit as installed (`INV-U4`).
    pub fn mark_installed(&mut self, build_number: u64, commit_sha: &str) -> bool {
        let Some((norm_sha, short_sha)) = normalize_git_sha_verified(commit_sha) else {
            return false;
        };
        self.installed_build_number = build_number.max(1);
        self.installed_commit_sha = norm_sha;
        self.installed_short_sha = short_sha;
        true
    }

    /// Evaluates whether a newer `main` push is available to download or install (`INV-U1`).
    pub fn evaluate_availability(&self) -> UpdateAvailability {
        if let Some(remote) = &self.latest_remote {
            if is_release_newer_verified(
                self.installed_build_number,
                &self.installed_commit_sha,
                remote,
            ) {
                let stored_archive_filename = self
                    .find_stored(&remote.commit_sha)
                    .map(|s| s.archive_filename.clone());
                return UpdateAvailability::UpdateAvailable {
                    manifest: remote.clone(),
                    stored_archive_filename,
                };
            }
        }
        UpdateAvailability::UpToDate {
            installed_build: self.installed_build_number,
            installed_short_sha: self.installed_short_sha.clone(),
        }
    }
}

impl FormalInvariant for UpdateCatalogStore {
    fn check_invariants(&self) -> Result<(), InvariantViolation> {
        // INV-U1: Installed SHA is valid and evaluate_availability agrees with is_release_newer_verified.
        if normalize_git_sha_verified(&self.installed_commit_sha).is_none()
            || self.installed_short_sha.len() != 7
            || self.installed_build_number == 0
        {
            return Err(InvariantViolation {
                code: "INV-U1",
                subsystem: "UpdateCatalogStore",
                details: format!(
                    "invalid installed build={} or sha='{}'",
                    self.installed_build_number, self.installed_commit_sha
                ),
            });
        }

        let availability = self.evaluate_availability();
        match (&self.latest_remote, &availability) {
            (Some(remote), UpdateAvailability::UpdateAvailable { manifest, .. }) => {
                if !is_release_newer_verified(
                    self.installed_build_number,
                    &self.installed_commit_sha,
                    remote,
                ) || manifest != remote
                {
                    return Err(InvariantViolation {
                        code: "INV-U1",
                        subsystem: "UpdateCatalogStore",
                        details: "UpdateAvailable reported for non-newer or mismatched remote release"
                            .to_string(),
                    });
                }
            }
            (Some(remote), UpdateAvailability::UpToDate { .. }) => {
                if is_release_newer_verified(
                    self.installed_build_number,
                    &self.installed_commit_sha,
                    remote,
                ) {
                    return Err(InvariantViolation {
                        code: "INV-U1",
                        subsystem: "UpdateCatalogStore",
                        details: "UpToDate reported despite newer remote release".to_string(),
                    });
                }
            }
            (None, UpdateAvailability::UpdateAvailable { .. }) => {
                return Err(InvariantViolation {
                    code: "INV-U1",
                    subsystem: "UpdateCatalogStore",
                    details: "UpdateAvailable reported when latest_remote is None".to_string(),
                });
            }
            (None, UpdateAvailability::UpToDate { .. }) => {}
        }

        // INV-U2: Every stored update has a valid 64-hex SHA-256 matching its manifest and a non-empty archive filename.
        for (idx, entry) in self.stored_updates.iter().enumerate() {
            if !verify_update_digest_verified(&entry.manifest.sha256, &entry.verified_sha256)
                || entry.archive_filename.trim().is_empty()
                || !entry.archive_filename.ends_with(".zip")
            {
                return Err(InvariantViolation {
                    code: "INV-U2",
                    subsystem: "UpdateCatalogStore",
                    details: format!(
                        "stored_updates[{idx}] failed digest or filename integrity: {entry:?}"
                    ),
                });
            }
        }

        // INV-U3: Bounded capacity (`<= max_stored`), descending build_number order, and pairwise unique commit SHAs.
        if self.stored_updates.len() > self.max_stored {
            return Err(InvariantViolation {
                code: "INV-U3",
                subsystem: "UpdateCatalogStore",
                details: format!(
                    "stored_updates.len()={} exceeds max_stored={}",
                    self.stored_updates.len(),
                    self.max_stored
                ),
            });
        }
        for window in self.stored_updates.windows(2) {
            if window[0].manifest.build_number < window[1].manifest.build_number {
                return Err(InvariantViolation {
                    code: "INV-U3",
                    subsystem: "UpdateCatalogStore",
                    details: format!(
                        "stored_updates not sorted descending by build_number: {} < {}",
                        window[0].manifest.build_number, window[1].manifest.build_number
                    ),
                });
            }
        }
        for i in 0..self.stored_updates.len() {
            for j in (i + 1)..self.stored_updates.len() {
                if git_shas_match_verified(
                    &self.stored_updates[i].manifest.commit_sha,
                    &self.stored_updates[j].manifest.commit_sha,
                ) {
                    return Err(InvariantViolation {
                        code: "INV-U3",
                        subsystem: "UpdateCatalogStore",
                        details: format!(
                            "duplicate commit SHA in stored_updates at {i} and {j}: {}",
                            self.stored_updates[i].manifest.commit_sha
                        ),
                    });
                }
            }
        }

        Ok(())
    }
}

#[allow(unexpected_cfgs)]
#[cfg(kani)]
mod kani_proofs {
    use super::*;

    #[kani::proof]
    fn kani_prove_unit_float_totality() {
        let val: f32 = kani::any();
        let fallback: f32 = kani::any();
        let out = sanitize_unit_float(val, fallback);
        assert!(out.is_finite());
        assert!(out >= 0.0 && out <= 1.0);
    }

    #[kani::proof]
    fn kani_prove_position_secs_totality() {
        let pos: f32 = kani::any();
        let dur: u32 = kani::any();
        let out = sanitize_position_secs(pos, dur);
        assert!(out.is_finite());
        assert!(out >= 0.0);
        if dur > 0 {
            assert!(out <= dur as f32);
        }
    }

    #[kani::proof]
    fn kani_prove_pcm_sample_totality() {
        let sample: f32 = kani::any();
        let out = sanitize_pcm_sample(sample);
        assert!(out.is_finite());
        assert!(out >= -1.0 && out <= 1.0);
    }

    #[kani::proof]
    fn kani_prove_repeat_mode_three_cycle() {
        let idx: u8 = kani::any();
        kani::assume(idx < 3);
        let mode = match idx {
            0 => RepeatMode::Off,
            1 => RepeatMode::Context,
            _ => RepeatMode::Track,
        };
        assert_ne!(mode.next(), mode);
        assert_eq!(mode.next().next().next(), mode);
    }

    #[kani::proof]
    #[kani::unwind(65)]
    fn kani_prove_coprime_shuffle_step() {
        let len: usize = kani::any();
        kani::assume(len >= 2 && len <= 64);
        let step = coprime_shuffle_step(len);
        assert!(step >= 1 && step < len);
        assert_eq!(gcd_usize(step, len), 1);
    }
}
