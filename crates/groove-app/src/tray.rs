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

/// High-level user action triggered from the macOS menu bar tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenNowPlaying,
    TogglePlayPause,
    NextTrack,
    PreviousTrack,
    ToggleLike,
}

/// Manages the native macOS system tray (`NSStatusItem` + `NSMenu`) displaying
/// the current playing track, rounded album art cover, and transport controls.
pub struct GrooveTray {
    tray: TrayIcon,
    menu: Menu,
    now_playing_item: MenuItem,
    album_item: MenuItem,
    play_pause_item: MenuItem,
    next_track_item: MenuItem,
    prev_track_item: MenuItem,
    like_item: MenuItem,
    last_snapshot: Option<TrayMenuSnapshot>,
    last_cover_key: Option<String>,
}

impl GrooveTray {
    /// Creates the macOS status bar item and dropdown menu on the main thread.
    pub fn try_new(state: &PlaybackState, artwork: &ArtworkCache) -> Option<Self> {
        #[cfg(target_os = "macos")]
        let _mtm = objc2::MainThreadMarker::new()?;

        let snapshot = TrayMenuSnapshot::from_playback_state(state);
        snapshot.assert_invariants();

        let menu = Menu::new();

        let now_playing_item = MenuItem::with_id(
            MenuId::new(ID_NOW_PLAYING),
            format!("♫  {}", snapshot.now_playing_headline),
            true,
            None,
        );
        let album_item = MenuItem::new(&snapshot.album_subtitle, false, None);
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

        let cover_key = Self::cover_key(state, artwork);
        tracing::info!(
            "[Tray] Initialized macOS menu bar tray: title='{}', track='{}'",
            snapshot.status_bar_title,
            snapshot.now_playing_headline
        );

        Some(Self {
            tray,
            menu,
            now_playing_item,
            album_item,
            play_pause_item,
            next_track_item,
            prev_track_item,
            like_item,
            last_snapshot: Some(snapshot),
            last_cover_key: Some(cover_key),
        })
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

    /// Synchronizes the macOS menu bar tray text and album cover art whenever
    /// playback state or decoded artwork changes.
    pub fn sync(&mut self, state: &PlaybackState, artwork: &ArtworkCache) {
        let snapshot = TrayMenuSnapshot::from_playback_state(state);
        snapshot.assert_invariants();

        if self.last_snapshot.as_ref() != Some(&snapshot) {
            self.now_playing_item
                .set_text(format!("♫  {}", snapshot.now_playing_headline));
            self.album_item.set_text(&snapshot.album_subtitle);
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

            tracing::info!(
                "[Tray] Updated menu bar status: '{}' | '{}'",
                snapshot.status_bar_title,
                snapshot.now_playing_headline
            );
            self.last_snapshot = Some(snapshot);
        }

        let cover_key = Self::cover_key(state, artwork);
        if self.last_cover_key.as_ref() != Some(&cover_key) {
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
        }
    }

    /// Non-blockingly drains any pending clicks on tray menu items.
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
