use crate::artwork::ArtworkCache;
use groove_core::{FormalInvariant, PlaybackState, TrayMenuSnapshot};
use tray_icon::menu::{ContextMenu, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

const ID_NOW_PLAYING: &str = "groove.tray.now_playing";
const ID_PLAY_PAUSE: &str = "groove.tray.play_pause";
const ID_NEXT_TRACK: &str = "groove.tray.next_track";
const ID_PREV_TRACK: &str = "groove.tray.prev_track";
const ID_TOGGLE_LIKE: &str = "groove.tray.toggle_like";
const ID_OPEN_WINDOW: &str = "groove.tray.open_window";

/// High-level user action triggered from the macOS menu bar tray or Now Playing center.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenNowPlaying,
    Play,
    Pause,
    TogglePlayPause,
    NextTrack,
    PreviousTrack,
    ToggleLike,
    SeekSeconds(u32),
}

/// Manages the native macOS system tray (`NSStatusItem` + `NSMenu`) and macOS
/// `MPNowPlayingInfoCenter` / `MPRemoteCommandCenter`, displaying the current
/// playing track, synchronized progress indicator, rounded album art cover, and
/// transport controls.
pub struct GrooveTray {
    tray: TrayIcon,
    menu: Menu,
    now_playing_item: MenuItem,
    album_item: MenuItem,
    progress_item: MenuItem,
    play_pause_item: MenuItem,
    next_track_item: MenuItem,
    prev_track_item: MenuItem,
    like_item: MenuItem,
    last_snapshot: Option<TrayMenuSnapshot>,
    last_cover_key: Option<String>,
    last_cover_rgba: (u32, u32, Vec<u8>),
}

impl GrooveTray {
    /// Creates the macOS status bar item, dropdown menu, and Now Playing center on the main thread.
    pub fn try_new(state: &PlaybackState, artwork: &ArtworkCache) -> Option<Self> {
        #[cfg(target_os = "macos")]
        let _mtm = objc2::MainThreadMarker::new()?;

        init_macos_remote_command_center();

        let snapshot = TrayMenuSnapshot::from_playback_state(state);
        snapshot.assert_invariants();
        if let Err(err) = snapshot.check_sync_with_playback(state) {
            panic!(
                "[FORMAL VERIFICATION FAILURE] TrayMenuSnapshot::check_sync_with_playback violated ({}): {}",
                err.code, err.details
            );
        }

        let menu = Menu::new();

        let now_playing_item = MenuItem::with_id(
            MenuId::new(ID_NOW_PLAYING),
            format!("♫  {}", snapshot.now_playing_headline),
            true,
            None,
        );
        let album_item = MenuItem::new(&snapshot.album_subtitle, false, None);
        let progress_item = MenuItem::new(format!("⏱  {}", snapshot.progress_label), false, None);
        let play_pause_item = MenuItem::with_id(
            MenuId::new(ID_PLAY_PAUSE),
            format!(
                "{}  {}",
                if snapshot.is_playing { "⏸" } else { "▶" },
                snapshot.play_pause_label
            ),
            true,
            None,
        );
        let next_track_item = MenuItem::with_id(
            MenuId::new(ID_NEXT_TRACK),
            "⏭  Next Track",
            snapshot.can_skip_tracks,
            None,
        );
        let prev_track_item = MenuItem::with_id(
            MenuId::new(ID_PREV_TRACK),
            "⏮  Previous Track",
            snapshot.can_skip_tracks,
            None,
        );
        let like_item = MenuItem::with_id(
            MenuId::new(ID_TOGGLE_LIKE),
            &snapshot.like_label,
            true,
            None,
        );
        let open_window_item = MenuItem::with_id(
            MenuId::new(ID_OPEN_WINDOW),
            "Open Groove Player",
            true,
            None,
        );

        let _ = menu.append(&now_playing_item);
        let _ = menu.append(&album_item);
        let _ = menu.append(&progress_item);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&play_pause_item);
        let _ = menu.append(&next_track_item);
        let _ = menu.append(&prev_track_item);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&like_item);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&open_window_item);

        let (w, h, rgba) = artwork.tray_cover_rgba(
            state.current_track.cover_art_url.as_deref(),
            state.current_track.artwork_theme,
        );
        let icon = Icon::from_rgba(rgba.clone(), w, h).ok()?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu.clone()))
            .with_icon(icon)
            .with_title(&snapshot.status_bar_title)
            .with_tooltip(&snapshot.tooltip)
            .build()
            .map_err(|err| {
                tracing::warn!("[Tray] Failed to build macOS tray icon: {err}");
                err
            })
            .ok()?;

        set_macos_menu_cover_art(&menu, &rgba, w, h, 64.0);
        sync_macos_now_playing_center(&snapshot, state, &rgba, w, h);

        let cover_key = Self::cover_key(state, artwork);
        tracing::info!(
            "[Tray] Initialized macOS menu bar tray & Now Playing center: title='{}', track='{}', progress='{}'",
            snapshot.status_bar_title,
            snapshot.now_playing_headline,
            snapshot.progress_label
        );

        Some(Self {
            tray,
            menu,
            now_playing_item,
            album_item,
            progress_item,
            play_pause_item,
            next_track_item,
            prev_track_item,
            like_item,
            last_snapshot: Some(snapshot),
            last_cover_key: Some(cover_key),
            last_cover_rgba: (w, h, rgba),
        })
    }

    /// Returns the most recently synchronized formal tray snapshot for invariant checking.
    pub fn snapshot(&self) -> Option<&TrayMenuSnapshot> {
        self.last_snapshot.as_ref()
    }

    fn cover_key(state: &PlaybackState, artwork: &ArtworkCache) -> String {
        let url = state
            .current_track
            .cover_art_url
            .as_deref()
            .unwrap_or("")
            .trim();
        let has_remote = !url.is_empty() && artwork.has_remote_cached(url);
        format!(
            "{}:{}:{:?}",
            url, has_remote, state.current_track.artwork_theme
        )
    }

    /// Synchronizes the macOS menu bar tray text, progress indicator, Now Playing
    /// center, and album cover art whenever playback state or decoded artwork changes.
    pub fn sync(&mut self, state: &PlaybackState, artwork: &ArtworkCache) {
        let snapshot = TrayMenuSnapshot::from_playback_state(state);
        snapshot.assert_invariants();
        if let Err(err) = snapshot.check_sync_with_playback(state) {
            panic!(
                "[FORMAL VERIFICATION FAILURE] TrayMenuSnapshot::check_sync_with_playback violated ({}): {}",
                err.code, err.details
            );
        }

        let cover_key = Self::cover_key(state, artwork);
        let cover_changed = self.last_cover_key.as_ref() != Some(&cover_key);
        if cover_changed {
            let (w, h, rgba) = artwork.tray_cover_rgba(
                state.current_track.cover_art_url.as_deref(),
                state.current_track.artwork_theme,
            );
            if let Ok(icon) = Icon::from_rgba(rgba.clone(), w, h) {
                let _ = self.tray.set_icon(Some(icon));
            }
            set_macos_menu_cover_art(&self.menu, &rgba, w, h, 64.0);
            tracing::info!("[Tray] Updated macOS tray & menu album art cover ({w}x{h} RGBA)");
            self.last_cover_key = Some(cover_key);
            self.last_cover_rgba = (w, h, rgba);
        }

        let snapshot_changed = self.last_snapshot.as_ref() != Some(&snapshot);
        if snapshot_changed {
            let track_or_state_changed = self
                .last_snapshot
                .as_ref()
                .map(|prev| {
                    prev.now_playing_headline != snapshot.now_playing_headline
                        || prev.is_playing != snapshot.is_playing
                })
                .unwrap_or(true);

            self.now_playing_item
                .set_text(format!("♫  {}", snapshot.now_playing_headline));
            self.album_item.set_text(&snapshot.album_subtitle);
            self.progress_item
                .set_text(format!("⏱  {}", snapshot.progress_label));
            self.play_pause_item.set_text(format!(
                "{}  {}",
                if snapshot.is_playing { "⏸" } else { "▶" },
                snapshot.play_pause_label
            ));
            self.next_track_item.set_enabled(snapshot.can_skip_tracks);
            self.prev_track_item.set_enabled(snapshot.can_skip_tracks);
            self.like_item.set_text(&snapshot.like_label);

            self.tray.set_title(Some(&snapshot.status_bar_title));
            let _ = self.tray.set_tooltip(Some(&snapshot.tooltip));

            if track_or_state_changed {
                tracing::info!(
                    "[Tray] Updated menu bar status: '{}' | '{}' | '{}'",
                    snapshot.status_bar_title,
                    snapshot.now_playing_headline,
                    snapshot.progress_label
                );
            }
            self.last_snapshot = Some(snapshot.clone());
        }

        if snapshot_changed || cover_changed {
            let (w, h, ref rgba) = self.last_cover_rgba;
            sync_macos_now_playing_center(&snapshot, state, rgba, w, h);
        }
    }

    /// Non-blockingly drains any pending clicks on tray menu items or macOS
    /// `MPRemoteCommandCenter` media controls.
    pub fn poll_actions() -> Vec<TrayAction> {
        let mut actions = Vec::new();
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let id = event.id().0.as_str();
            tracing::info!("[Tray] Menu item clicked: id={id}");
            match id {
                ID_NOW_PLAYING | ID_OPEN_WINDOW => {
                    activate_macos_app();
                    actions.push(TrayAction::OpenNowPlaying);
                }
                ID_PLAY_PAUSE => actions.push(TrayAction::TogglePlayPause),
                ID_NEXT_TRACK => actions.push(TrayAction::NextTrack),
                ID_PREV_TRACK => actions.push(TrayAction::PreviousTrack),
                ID_TOGGLE_LIKE => actions.push(TrayAction::ToggleLike),
                _ => {}
            }
        }
        if let Some(rx) = remote_command_receiver() {
            while let Ok(action) = rx.try_recv() {
                tracing::info!("[Tray] macOS Now Playing remote command received: {action:?}");
                actions.push(action);
            }
        }
        actions
    }
}

/// Attaches a large (`display_pt × display_pt`, e.g. `64×64` pt) rounded-corner album art `NSImage`
/// to the first item (`Now Playing`) of the native macOS `NSMenu`.
#[cfg(target_os = "macos")]
fn set_macos_menu_cover_art(
    menu: &Menu,
    rgba: &[u8],
    width: u32,
    height: u32,
    display_pt: f64,
) {
    use objc2::rc::Retained;
    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSBitmapFormat, NSBitmapImageRep, NSDeviceRGBColorSpace, NSImage, NSMenu};
    use objc2_foundation::NSSize;

    if rgba.len() != (width * height * 4) as usize || width == 0 || height == 0 {
        return;
    }

    unsafe {
        let ns_menu_ptr = menu.ns_menu() as *mut NSMenu;
        if ns_menu_ptr.is_null() {
            return;
        }
        let ns_menu = &*ns_menu_ptr;
        if ns_menu.numberOfItems() <= 0 {
            return;
        }
        let Some(first_item) = ns_menu.itemAtIndex(0) else {
            return;
        };

        let mut planes: [*mut u8; 1] = [rgba.as_ptr() as *mut u8];
        let Some(bitmap_rep) = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            planes.as_mut_ptr(),
            width as isize,
            height as isize,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat::empty(),
            (width * 4) as isize,
            32,
        ) else {
            return;
        };

        let size = NSSize::new(display_pt, display_pt);
        let image: Retained<NSImage> = NSImage::initWithSize(NSImage::alloc(), size);
        image.addRepresentation(&bitmap_rep);
        image.setTemplate(false);
        first_item.setImage(Some(&image));
    }
}

#[cfg(not(target_os = "macos"))]
fn set_macos_menu_cover_art(
    _menu: &Menu,
    _rgba: &[u8],
    _width: u32,
    _height: u32,
    _display_pt: f64,
) {
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn activate_macos_app() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        app.activateIgnoringOtherApps(true);
    }
}

#[cfg(not(target_os = "macos"))]
fn activate_macos_app() {}

#[cfg(target_os = "macos")]
#[link(name = "MediaPlayer", kind = "framework")]
extern "C" {}

#[cfg(target_os = "macos")]
static REMOTE_COMMAND_RX: std::sync::OnceLock<flume::Receiver<TrayAction>> =
    std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
fn remote_command_receiver() -> Option<&'static flume::Receiver<TrayAction>> {
    REMOTE_COMMAND_RX.get()
}

#[cfg(not(target_os = "macos"))]
fn remote_command_receiver() -> Option<&'static flume::Receiver<TrayAction>> {
    None
}

#[cfg(target_os = "macos")]
fn claim_macos_now_playing_application() {
    use std::ffi::CString;

    unsafe {
        let Ok(framework_path) = CString::new(
            "/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote",
        ) else {
            return;
        };
        let handle = libc::dlopen(framework_path.as_ptr(), libc::RTLD_LAZY);
        if handle.is_null() {
            return;
        }
        let Ok(sym_name) = CString::new("MRMediaRemoteSetCanBeNowPlayingApplication") else {
            return;
        };
        let sym = libc::dlsym(handle, sym_name.as_ptr());
        if !sym.is_null() {
            let func: unsafe extern "C" fn(u8) = std::mem::transmute(sym);
            func(1);
        }
    }
}

#[cfg(target_os = "macos")]
fn init_macos_remote_command_center() {
    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use std::ptr::NonNull;

    let _ = REMOTE_COMMAND_RX.get_or_init(|| {
        let (tx, rx) = flume::unbounded::<TrayAction>();

        claim_macos_now_playing_application();

        unsafe {
            let Some(cls) = AnyClass::get(c"MPRemoteCommandCenter") else {
                return rx;
            };
            let cc: *mut AnyObject = msg_send![cls, sharedCommandCenter];
            if cc.is_null() {
                return rx;
            }

            let register_simple = |cmd: *mut AnyObject, action: TrayAction| {
                if cmd.is_null() {
                    return;
                }
                let _: () = msg_send![cmd, setEnabled: Bool::YES];
                let tx_clone = tx.clone();
                let block = RcBlock::new(move |_event: NonNull<AnyObject>| -> isize {
                    let _ = tx_clone.send(action);
                    0 // MPRemoteCommandHandlerStatusSuccess
                });
                let _: *mut AnyObject = msg_send![cmd, addTargetWithHandler: &*block];
                std::mem::forget(block);
            };

            let play_cmd: *mut AnyObject = msg_send![cc, playCommand];
            register_simple(play_cmd, TrayAction::Play);

            let pause_cmd: *mut AnyObject = msg_send![cc, pauseCommand];
            register_simple(pause_cmd, TrayAction::Pause);

            let toggle_cmd: *mut AnyObject = msg_send![cc, togglePlayPauseCommand];
            register_simple(toggle_cmd, TrayAction::TogglePlayPause);

            let next_cmd: *mut AnyObject = msg_send![cc, nextTrackCommand];
            register_simple(next_cmd, TrayAction::NextTrack);

            let prev_cmd: *mut AnyObject = msg_send![cc, previousTrackCommand];
            register_simple(prev_cmd, TrayAction::PreviousTrack);

            let seek_cmd: *mut AnyObject = msg_send![cc, changePlaybackPositionCommand];
            if !seek_cmd.is_null() {
                let _: () = msg_send![seek_cmd, setEnabled: Bool::YES];
                let tx_seek = tx.clone();
                let block = RcBlock::new(move |event: NonNull<AnyObject>| -> isize {
                    let pos_secs: f64 = msg_send![event.as_ptr(), positionTime];
                    if pos_secs.is_finite() && pos_secs >= 0.0 {
                        let _ = tx_seek.send(TrayAction::SeekSeconds(pos_secs.round() as u32));
                    }
                    0 // MPRemoteCommandHandlerStatusSuccess
                });
                let _: *mut AnyObject = msg_send![seek_cmd, addTargetWithHandler: &*block];
                std::mem::forget(block);
            }
        }

        tracing::info!(
            "[Tray] Registered macOS MPRemoteCommandCenter handlers (play, pause, toggle, next, prev, seek)"
        );
        rx
    });
}

#[cfg(not(target_os = "macos"))]
fn init_macos_remote_command_center() {}

#[cfg(target_os = "macos")]
fn sync_macos_now_playing_center(
    snapshot: &TrayMenuSnapshot,
    state: &PlaybackState,
    rgba: &[u8],
    width: u32,
    height: u32,
) {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, ProtocolObject};
    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSBitmapFormat, NSBitmapImageRep, NSDeviceRGBColorSpace, NSImage};
    use objc2_foundation::{NSCopying, NSDictionary, NSNumber, NSSize, NSString};

    claim_macos_now_playing_application();

    unsafe {
        let Some(center_cls) = AnyClass::get(c"MPNowPlayingInfoCenter") else {
            return;
        };
        let center: *mut AnyObject = msg_send![center_cls, defaultCenter];
        if center.is_null() {
            return;
        }

        if !state.has_active_track() {
            let nil_dict: Option<&NSDictionary<NSString, AnyObject>> = None;
            let _: () = msg_send![center, setNowPlayingInfo: nil_dict];
            // MPNowPlayingPlaybackStateStopped = 3
            let _: () = msg_send![center, setPlaybackState: 3usize];
            return;
        }

        let mut keys: Vec<Retained<NSString>> = Vec::with_capacity(9);
        let mut vals: Vec<Retained<AnyObject>> = Vec::with_capacity(9);

        let mut push_entry = |k: &str, v: Retained<AnyObject>| {
            keys.push(NSString::from_str(k));
            vals.push(v);
        };

        push_entry(
            "title",
            Retained::into_super(Retained::into_super(NSString::from_str(
                state.current_track.title.trim(),
            ))),
        );
        push_entry(
            "artist",
            Retained::into_super(Retained::into_super(NSString::from_str(
                state.current_track.artist.trim(),
            ))),
        );
        push_entry(
            "albumTitle",
            Retained::into_super(Retained::into_super(NSString::from_str(
                state.current_track.album.trim(),
            ))),
        );
        push_entry(
            "playbackDuration",
            Retained::into_super(Retained::into_super(NSNumber::new_f64(
                snapshot.duration_secs as f64,
            )))
            .into(),
        );
        push_entry(
            "MPNowPlayingInfoPropertyElapsedPlaybackTime",
            Retained::into_super(Retained::into_super(NSNumber::new_f64(
                snapshot.elapsed_secs as f64,
            )))
            .into(),
        );
        push_entry(
            "MPNowPlayingInfoPropertyPlaybackRate",
            Retained::into_super(Retained::into_super(NSNumber::new_f64(
                snapshot.playback_rate_f64(),
            )))
            .into(),
        );
        push_entry(
            "MPNowPlayingInfoPropertyDefaultPlaybackRate",
            Retained::into_super(Retained::into_super(NSNumber::new_f64(1.0))).into(),
        );
        // MPNowPlayingInfoMediaTypeAudio = 1
        push_entry(
            "MPNowPlayingInfoPropertyMediaType",
            Retained::into_super(Retained::into_super(NSNumber::new_u64(1))).into(),
        );

        // Attach album/podcast cover artwork to MPNowPlayingInfoCenter
        if rgba.len() == (width * height * 4) as usize && width > 0 && height > 0 {
            let mut planes: [*mut u8; 1] = [rgba.as_ptr() as *mut u8];
            if let Some(bitmap_rep) = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                planes.as_mut_ptr(),
                width as isize,
                height as isize,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                NSBitmapFormat::empty(),
                (width * 4) as isize,
                32,
            ) {
                let size = NSSize::new(width as f64, height as f64);
                let image: Retained<NSImage> = NSImage::initWithSize(NSImage::alloc(), size);
                image.addRepresentation(&bitmap_rep);
                image.setTemplate(false);

                if let Some(art_cls) = AnyClass::get(c"MPMediaItemArtwork") {
                    let alloc_art: *mut AnyObject = msg_send![art_cls, alloc];
                    let art_ptr: *mut AnyObject = msg_send![alloc_art, initWithImage: &*image];
                    if let Some(art_retained) = Retained::from_raw(art_ptr) {
                        push_entry("artwork", art_retained);
                    }
                }
            }
        }

        let key_refs: Vec<&ProtocolObject<dyn NSCopying>> = keys
            .iter()
            .map(|k| ProtocolObject::from_ref(&**k))
            .collect();
        let val_refs: Vec<&AnyObject> = vals.iter().map(|v| &**v).collect();
        let dict = NSDictionary::from_slices(&key_refs, &val_refs);

        let _: () = msg_send![center, setNowPlayingInfo: &*dict];
        // MPNowPlayingPlaybackStatePlaying = 1, MPNowPlayingPlaybackStatePaused = 2
        let pb_state: usize = if snapshot.is_playing { 1 } else { 2 };
        let _: () = msg_send![center, setPlaybackState: pb_state];
    }
}

#[cfg(not(target_os = "macos"))]
fn sync_macos_now_playing_center(
    _snapshot: &TrayMenuSnapshot,
    _state: &PlaybackState,
    _rgba: &[u8],
    _width: u32,
    _height: u32,
) {
}
