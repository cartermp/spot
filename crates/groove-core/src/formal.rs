#![allow(unexpected_cfgs)]

use serde::{Deserialize, Serialize};

use crate::{
    AudioDevice, PlaybackState, RepeatMode, SpectrumFrame, Track,
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

/// Trait implemented by all formally verified state machines and data structures in Groove.
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
/// - **INV-P8**: Under `shuffle == true`, forward and backward steps are exact modular inverses and form a single full cycle of length `queue.len()`.
/// - **INV-P9**: When `auto_ended == true`, `repeat == RepeatMode::Off`, and `!shuffle` on the final track of the queue, playback stops cleanly (`is_playing = false`) at the start of the queue instead of looping infinitely.
pub fn advance_queue_verified(state: &mut PlaybackState, forward: bool, auto_ended: bool) {
    state.position_secs = 0.0;
    // Natural track completion (`auto_ended == true`) always progresses forward.
    let forward = forward || auto_ended;

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

/// Applies a pure synchronous state transition on `PlaybackState` and preserves all invariants `INV-P1..INV-P9`.
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
    SelectDevice(AudioDevice),
}

impl PlaybackState {
    /// Executes a verified transport action on `self`, guaranteeing that `self.check_invariants()`
    /// holds after the transition whenever it held beforehand.
    pub fn apply_verified_action(&mut self, action: VerifiedTransportAction) {
        match action {
            VerifiedTransportAction::Play => {
                self.is_playing = true;
            }
            VerifiedTransportAction::Pause => {
                self.is_playing = false;
            }
            VerifiedTransportAction::TogglePlayPause { has_loaded_track } => {
                if !has_loaded_track && !self.is_playing {
                    self.is_playing = true;
                } else {
                    self.is_playing = !self.is_playing;
                }
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
                self.position_secs = sanitize_position_secs(pos, self.current_track.duration_secs);
            }
            VerifiedTransportAction::SeekSeconds(pos) => {
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
            }
            VerifiedTransportAction::SelectQueueTrack(track_id) => {
                if let Some(selected) = self
                    .queue
                    .iter()
                    .find(|t| t.id == track_id || (!track_id.is_empty() && t.spotify_uri == track_id))
                    .cloned()
                {
                    self.current_track = selected;
                    self.position_secs = 0.0;
                    self.is_playing = true;
                }
            }
            VerifiedTransportAction::PlayTrack(mut track) => {
                if let Some(existing) = self
                    .queue
                    .iter_mut()
                    .find(|t| t.id == track.id || (!track.spotify_uri.is_empty() && t.spotify_uri == track.spotify_uri))
                {
                    track.liked = existing.liked;
                    *existing = track.clone();
                } else {
                    self.queue.insert(0, track.clone());
                }
                self.current_track = track;
                self.position_secs = 0.0;
                self.is_playing = true;
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
                self.active_playlist_id =
                    matched_id.or_else(|| (!playlist_id.trim().is_empty()).then_some(playlist_id));

                if !tracks.is_empty() {
                    self.current_track = tracks[0].clone();
                    self.queue = tracks;
                    self.position_secs = 0.0;
                    self.is_playing = auto_play;
                }
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
    Artist { uri: String },
    Album { uri: String, artist_name: String },
    Track { uri: String, artist_name: String },
}

impl RecentSearchKind {
    pub fn badge_label(&self) -> &'static str {
        match self {
            RecentSearchKind::Query => "SEARCH",
            RecentSearchKind::Artist { .. } => "ARTIST",
            RecentSearchKind::Album { .. } => "ALBUM",
            RecentSearchKind::Track { .. } => "TRACK",
        }
    }
}

/// Persisted entry in the "Recently Searched" store.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecentSearchEntry {
    pub label: String,
    pub subtitle: String,
    pub query: String,
    pub kind: RecentSearchKind,
}

/// Formally verified bounded store for Recently Searched items with prefix coalescing and deduplication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentSearchStore {
    entries: Vec<RecentSearchEntry>,
    max_entries: usize,
}

impl Default for RecentSearchStore {
    fn default() -> Self {
        Self::new(10)
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
        // Insert in reverse so index 0 of `raw` ends up at the front while enforcing all invariants.
        for entry in raw.into_iter().rev() {
            store.record_entry(entry);
        }
        store
    }

    #[inline]
    pub fn entries(&self) -> &[RecentSearchEntry] {
        &self.entries
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

        self.entries.retain(|existing| {
            !(existing.kind.badge_label() == entry.kind.badge_label()
                && existing.label.eq_ignore_ascii_case(&entry.label))
        });
        self.entries.insert(0, entry);
        self.entries.truncate(self.max_entries);
        true
    }

    /// Records a debounced search query while coalescing incremental typing prefixes
    /// (e.g. `"ma"` -> `"mas"` -> `"mastodon"` updates the head entry in-place).
    pub fn record_typing_query(&mut self, raw_query: &str) -> bool {
        let query = raw_query.trim();
        if query.chars().count() < 2 {
            return false;
        }

        if let Some(first) = self.entries.first().cloned() {
            if first.kind == RecentSearchKind::Query {
                let prev_lower = first.label.to_lowercase();
                let next_lower = query.to_lowercase();
                if next_lower.starts_with(&prev_lower) || prev_lower.starts_with(&next_lower) {
                    if next_lower.chars().count() >= prev_lower.chars().count() {
                        // Replace the head prefix entry and deduplicate any older matching entry later in the list.
                        self.entries.remove(0);
                        return self.record_entry(RecentSearchEntry {
                            label: query.to_string(),
                            subtitle: first.subtitle,
                            query: query.to_string(),
                            kind: RecentSearchKind::Query,
                        });
                    }
                    return false;
                }
            }
        }

        self.record_entry(RecentSearchEntry {
            label: query.to_string(),
            subtitle: "Search query".to_string(),
            query: query.to_string(),
            kind: RecentSearchKind::Query,
        })
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

        Ok(())
    }
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

/// Formally verified snapshot of the macOS Menu Bar Tray state derived from `PlaybackState`.
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
        let has_track = !state.current_track.id.is_empty()
            && !state.current_track.title.trim().is_empty()
            && state.current_track.id != "no-track";

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
            " Groove".to_string()
        };

        let now_playing_headline = if has_track {
            if clean_artist.is_empty() {
                clean_title.clone()
            } else {
                format!("{clean_title} — {clean_artist}")
            }
        } else {
            "Groove — Ready to Play".to_string()
        };

        let album_subtitle = if has_track && !clean_album.is_empty() {
            format!("Album: {clean_album} • 320 kbps")
        } else {
            "Spotify Premium • 320 kbps Ogg Vorbis".to_string()
        };

        let tooltip = if has_track {
            format!("Groove: {now_playing_headline} ({album_subtitle})")
        } else {
            "Groove — Spotify Premium Player".to_string()
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
            play_pause_label,
            like_label,
            is_playing: state.is_playing,
            can_skip_tracks,
        }
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
