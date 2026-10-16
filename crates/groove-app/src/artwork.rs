use groove_core::ArtworkTheme;
use iced::widget::image;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// Decoded artwork bundle containing both a `380x380` GPU texture handle and a `96x96`
/// rounded-corner RGBA buffer for the macOS system tray & menu cover art.
#[derive(Clone, Debug)]
pub struct DecodedArtwork {
    pub handle: image::Handle,
    pub tray_rgba_96: Vec<u8>,
}

/// Caches procedurally rendered and remotely fetched rounded album/artist artwork handles
/// so they are rendered as GPU textures at 120 FPS with zero runtime overhead.
pub struct ArtworkCache {
    hero: HashMap<ArtworkTheme, image::Handle>,
    thumb: HashMap<ArtworkTheme, image::Handle>,
    tray_theme_rgba: HashMap<ArtworkTheme, Vec<u8>>,
    remote: HashMap<String, image::Handle>,
    remote_tray_rgba: HashMap<String, Vec<u8>>,
    in_flight: HashSet<String>,
}

impl Default for ArtworkCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ArtworkCache {
    pub fn new() -> Self {
        let themes = [
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

        let mut hero = HashMap::new();
        let mut thumb = HashMap::new();
        let mut tray_theme_rgba = HashMap::new();

        for theme in themes {
            hero.insert(theme, render_artwork(theme, 360, 18.0));
            thumb.insert(theme, render_artwork(theme, 72, 12.0));
            tray_theme_rgba.insert(theme, render_artwork_rgba(theme, 96, 16.0));
        }

        Self {
            hero,
            thumb,
            tray_theme_rgba,
            remote: HashMap::new(),
            remote_tray_rgba: HashMap::new(),
            in_flight: HashSet::new(),
        }
    }

    pub fn hero_handle(&self, theme: ArtworkTheme) -> image::Handle {
        self.hero
            .get(&theme)
            .cloned()
            .unwrap_or_else(|| render_artwork(theme, 360, 18.0))
    }

    pub fn thumb_handle(&self, theme: ArtworkTheme) -> image::Handle {
        self.thumb
            .get(&theme)
            .cloned()
            .unwrap_or_else(|| render_artwork(theme, 72, 12.0))
    }

    /// Returns a downloaded remote cover/portrait handle if cached, or falls back to the hero theme.
    pub fn hero_or_url(&self, url: Option<&str>, theme: ArtworkTheme) -> image::Handle {
        if let Some(u) = url {
            if let Some(handle) = self.remote.get(u) {
                return handle.clone();
            }
        }
        self.hero_handle(theme)
    }

    /// Returns a downloaded remote cover/portrait handle if cached, or falls back to the thumbnail theme.
    pub fn thumb_or_url(&self, url: Option<&str>, theme: ArtworkTheme) -> image::Handle {
        if let Some(u) = url {
            if let Some(handle) = self.remote.get(u) {
                return handle.clone();
            }
        }
        self.thumb_handle(theme)
    }

    /// Marks a remote artwork URL as in-flight if not already cached or downloading.
    /// Returns `true` if a new download task should be spawned.
    pub fn mark_in_flight(&mut self, url: &str) -> bool {
        let trimmed = url.trim();
        if trimmed.is_empty() || self.remote.contains_key(trimmed) {
            return false;
        }
        self.in_flight.insert(trimmed.to_string())
    }

    /// Returns `true` if the remote cover URL is already decoded in the cache.
    pub fn has_remote_cached(&self, url: &str) -> bool {
        self.remote_tray_rgba.contains_key(url.trim())
    }

    /// Inserts a decoded remote artwork bundle into the cache.
    pub fn insert_remote(&mut self, url: String, decoded: DecodedArtwork) {
        self.in_flight.remove(&url);
        self.remote_tray_rgba
            .insert(url.clone(), decoded.tray_rgba_96);
        self.remote.insert(url, decoded.handle);
    }

    /// Returns a `96x96` rounded RGBA byte buffer for the macOS tray icon and Now Playing menu card.
    pub fn tray_cover_rgba(
        &self,
        url: Option<&str>,
        theme: ArtworkTheme,
    ) -> (u32, u32, Vec<u8>) {
        if let Some(u) = url {
            if let Some(rgba) = self.remote_tray_rgba.get(u.trim()) {
                return (96, 96, rgba.clone());
            }
        }
        let rgba = self
            .tray_theme_rgba
            .get(&theme)
            .cloned()
            .unwrap_or_else(|| render_artwork_rgba(theme, 96, 16.0));
        (96, 96, rgba)
    }
}

/// Downloads an album cover or artist portrait from `url` (using an on-disk cache under
/// `~/Library/Application Support/Groove/image_cache`), decodes it, and applies anti-aliased
/// rounded corners.
pub async fn download_and_decode_artwork(url: String) -> Option<(String, DecodedArtwork)> {
    let trimmed = url.trim().to_string();
    if trimmed.is_empty() {
        return None;
    }

    let disk_path = disk_cache_path_for_url(&trimmed);
    let raw_bytes = if let Some(existing) = disk_path
        .as_ref()
        .and_then(|p| fs::read(p).ok())
        .filter(|b| !b.is_empty())
    {
        tracing::info!(
            "[Artwork] Loaded {} bytes from disk cache for {}",
            existing.len(),
            trimmed
        );
        existing
    } else {
        tracing::info!("[Artwork] Downloading remote image: {}", trimmed);
        let resp = match reqwest::get(&trimmed).await {
            Ok(r) => r,
            Err(err) => {
                tracing::warn!("[Artwork] HTTP error downloading {}: {err}", trimmed);
                return None;
            }
        };
        if !resp.status().is_success() {
            tracing::warn!(
                "[Artwork] Non-success HTTP status {} for {}",
                resp.status(),
                trimmed
            );
            return None;
        }
        let bytes = resp.bytes().await.ok()?.to_vec();
        tracing::info!(
            "[Artwork] Downloaded {} bytes from CDN for {}",
            bytes.len(),
            trimmed
        );
        if let Some(path) = &disk_path {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(path, &bytes);
        }
        bytes
    };

    let url_clone = trimmed.clone();
    let decoded = tokio::task::spawn_blocking(move || decode_artwork_bundle(&raw_bytes))
        .await
        .ok()??;

    tracing::info!(
        "[Artwork] Decoded GPU texture (380x380 + 96x96 tray cover) for {}",
        url_clone
    );
    Some((url_clone, decoded))
}

fn disk_cache_path_for_url(url: &str) -> Option<PathBuf> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    let hash = hasher.finish();
    let base = dirs::data_dir()?.join("Groove").join("image_cache");
    Some(base.join(format!("art_v2_{hash:016x}.img")))
}

fn decode_artwork_bundle(bytes: &[u8]) -> Option<DecodedArtwork> {
    let dyn_img = ::image::load_from_memory(bytes).ok()?;
    let hero_rgba = resize_and_round_rgba(&dyn_img, 380, 20.0);
    let tray_rgba_96 = resize_and_round_rgba(&dyn_img, 96, 16.0);
    Some(DecodedArtwork {
        handle: image::Handle::from_rgba(380, 380, hero_rgba),
        tray_rgba_96,
    })
}

fn resize_and_round_rgba(
    dyn_img: &::image::DynamicImage,
    target_size: u32,
    corner_radius: f32,
) -> Vec<u8> {
    let resized = dyn_img.resize_to_fill(
        target_size,
        target_size,
        ::image::imageops::FilterType::Triangle,
    );
    let mut rgba = resized.to_rgba8().into_raw();
    let s = target_size as f32;
    let half = s * 0.5;

    for py in 0..target_size {
        for px in 0..target_size {
            let dx = (px as f32 + 0.5 - half).abs() - (half - corner_radius);
            let dy = (py as f32 + 0.5 - half).abs() - (half - corner_radius);
            let dist = (dx.max(0.0).hypot(dy.max(0.0)) + dx.min(dy).min(0.0)) - corner_radius;
            let alpha = (0.5 - dist).clamp(0.0, 1.0);
            let idx = ((py * target_size + px) * 4 + 3) as usize;
            rgba[idx] = ((rgba[idx] as f32 / 255.0) * alpha * 255.0) as u8;
        }
    }

    rgba
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

fn lerp_rgb(c1: [f32; 3], c2: [f32; 3], t: f32) -> [f32; 3] {
    [
        lerp(c1[0], c2[0], t),
        lerp(c1[1], c2[1], t),
        lerp(c1[2], c2[2], t),
    ]
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Renders the artwork theme with signed-distance rounded corners and landscapes matching the mockup.
fn render_artwork(theme: ArtworkTheme, size: u32, corner_radius: f32) -> image::Handle {
    let pixels = render_artwork_rgba(theme, size, corner_radius);
    image::Handle::from_rgba(size, size, pixels)
}

fn render_artwork_rgba(theme: ArtworkTheme, size: u32, corner_radius: f32) -> Vec<u8> {
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    let s = size as f32;

    for py in 0..size {
        for px in 0..size {
            let u = (px as f32 + 0.5) / s;
            let v = (py as f32 + 0.5) / s;

            // Rounded rectangle SDF for crisp anti-aliased corners
            let half = s * 0.5;
            let dx = (px as f32 + 0.5 - half).abs() - (half - corner_radius);
            let dy = (py as f32 + 0.5 - half).abs() - (half - corner_radius);
            let dist = (dx.max(0.0).hypot(dy.max(0.0)) + dx.min(dy).min(0.0)) - corner_radius;
            let alpha = (0.5 - dist).clamp(0.0, 1.0);

            if alpha <= 0.0 {
                continue;
            }

            let rgb = match theme {
                ArtworkTheme::SunriseMountainLake => paint_sunrise_mountain_lake(u, v),
                ArtworkTheme::GoldenCanyon => paint_golden_canyon(u, v),
                ArtworkTheme::MoonlitClouds => paint_moonlit_clouds(u, v),
                ArtworkTheme::MistyArch => paint_misty_arch(u, v),
                ArtworkTheme::CoastalIsland => paint_coastal_island(u, v),
                ArtworkTheme::TwilightHorizon => paint_twilight_horizon(u, v),
                ArtworkTheme::DawnReflection => paint_dawn_reflection(u, v),
                ArtworkTheme::AlpineMist => paint_alpine_mist(u, v),
                ArtworkTheme::WarmDusk => paint_warm_dusk(u, v),
                ArtworkTheme::EmeraldRing => paint_emerald_ring(u, v),
                ArtworkTheme::VioletAura => paint_violet_aura(u, v),
            };

            let idx = ((py * size + px) * 4) as usize;
            pixels[idx] = (rgb[0].clamp(0.0, 1.0) * 255.0) as u8;
            pixels[idx + 1] = (rgb[1].clamp(0.0, 1.0) * 255.0) as u8;
            pixels[idx + 2] = (rgb[2].clamp(0.0, 1.0) * 255.0) as u8;
            pixels[idx + 3] = (alpha * 255.0) as u8;
        }
    }

    pixels
}

/// Paints the hero cover from the mockup ("Higher Ground — Lume"):
/// Teal-blue sky transitioning to warm peach horizon, a large luminous sun/moon at (0.53, 0.37),
/// layered turquoise fjords/mountains, a dark cliff promontory on the left with a lone hiker silhouette,
/// and golden-peach water ripples on the lake below.
fn paint_sunrise_mountain_lake(u: f32, v: f32) -> [f32; 3] {
    let teal_top = [0.29, 0.49, 0.56];
    let mist_mid = [0.76, 0.84, 0.82];
    let peach_glow = [0.98, 0.85, 0.76];
    let warm_core = [1.0, 0.95, 0.90];

    // Sky gradient
    let mut col = if v < 0.45 {
        lerp_rgb(teal_top, mist_mid, v / 0.45)
    } else {
        lerp_rgb(mist_mid, peach_glow, (v - 0.45) / 0.25)
    };

    // Large glowing sun/moon disc centered at (0.53, 0.36), radius 0.23
    let sun_dist = ((u - 0.53).powi(2) + (v - 0.36).powi(2)).sqrt();
    let halo = (-sun_dist * 4.5).exp() * 0.45;
    col = lerp_rgb(col, peach_glow, halo);
    if sun_dist < 0.235 {
        let edge = smoothstep(0.235, 0.228, sun_dist);
        let disc_col = lerp_rgb(warm_core, [0.99, 0.82, 0.72], (v - 0.14) / 0.44);
        col = lerp_rgb(col, disc_col, edge);
    }

    // Distant hazy mountain range
    let far_mtn = 0.55
        - 0.14 * (u * 5.2 + 0.4).sin().abs()
        - 0.08 * ((u - 0.18).abs() * -9.0).exp()
        - 0.12 * ((u - 0.88).abs() * -7.0).exp();
    if v > far_mtn && v < 0.67 {
        let fog = ((v - far_mtn) / 0.15).clamp(0.0, 1.0);
        let mtn_col = lerp_rgb([0.36, 0.55, 0.60], [0.64, 0.76, 0.77], fog);
        col = lerp_rgb(col, mtn_col, smoothstep(far_mtn - 0.004, far_mtn + 0.004, v));
    }

    // Mid-ground sharp fjord cliffs left & right framing the lake
    let left_peak = 0.39 + (u - 0.19).powi(2) * 3.6 + 0.025 * (u * 28.0).sin();
    let right_peak = 0.34 + (u - 0.92).powi(2) * 3.8 + 0.02 * (u * 22.0).cos();
    let mid_mtn = left_peak.min(right_peak);
    if v > mid_mtn && v < 0.67 {
        let mtn_shade = lerp_rgb([0.14, 0.33, 0.40], [0.27, 0.47, 0.53], (v - 0.35) / 0.32);
        col = lerp_rgb(col, mtn_shade, smoothstep(mid_mtn - 0.003, mid_mtn + 0.003, v));
    }

    // Calm alpine lake below v = 0.66
    if v >= 0.66 {
        let water_t = (v - 0.66) / 0.34;
        let base_water = lerp_rgb([0.52, 0.68, 0.72], [0.10, 0.25, 0.33], water_t);
        // Sun reflection column around u = 0.55
        let refl_width = 0.18 + water_t * 0.14;
        let refl_x = (-((u - 0.55) / refl_width).powi(2)).exp();
        let ripple = 0.5 + 0.5 * ((v * 190.0) + (u * 35.0).sin() * 2.5).sin();
        let refl_strength = refl_x * (0.55 + 0.45 * ripple) * (1.0 - water_t * 0.25);
        col = lerp_rgb(base_water, [0.99, 0.79, 0.62], refl_strength);
    }

    // Foreground dark rocky promontory on the left (u < 0.56, v > cliff_profile)
    let cliff_top = if u < 0.33 {
        0.64 - 0.06 * (-((u - 0.18) * 9.0).powi(2)).exp() + 0.008 * (u * 40.0).sin()
    } else {
        0.64 + (u - 0.33) * 1.45
    };
    if v > cliff_top {
        let rock_light = (-((u - 0.28).powi(2) + (v - 0.70).powi(2)) * 18.0).exp() * 0.15;
        let rock_col = [0.06 + rock_light, 0.13 + rock_light, 0.16 + rock_light];
        col = lerp_rgb(col, rock_col, smoothstep(cliff_top - 0.003, cliff_top + 0.003, v));
    }

    // Lone standing figure silhouette at u = 0.175, v = 0.525..0.585
    let body_dx = (u - 0.175).abs();
    if v > 0.532 && v < 0.585 && body_dx < 0.008 {
        col = [0.05, 0.10, 0.13];
    }
    let head_dist = ((u - 0.175).powi(2) + (v - 0.522).powi(2)).sqrt();
    if head_dist < 0.008 {
        col = [0.05, 0.10, 0.13];
    }

    col
}

fn paint_golden_canyon(u: f32, v: f32) -> [f32; 3] {
    let sky = lerp_rgb([0.92, 0.73, 0.62], [0.98, 0.86, 0.74], v);
    let ridge1 = 0.42 + (u - 0.30).abs() * 0.75;
    let ridge2 = 0.48 + (u - 0.72).abs() * 0.85;
    let mut col = sky;
    if v > ridge1 {
        col = [0.38, 0.22, 0.20];
    }
    if v > ridge2 {
        col = [0.22, 0.13, 0.14];
    }
    col
}

fn paint_moonlit_clouds(u: f32, v: f32) -> [f32; 3] {
    let mut col = lerp_rgb([0.46, 0.52, 0.69], [0.76, 0.75, 0.86], v);
    let d1 = ((u - 0.48).powi(2) + (v - 0.42).powi(2)).sqrt();
    let d2 = ((u - 0.54).powi(2) + (v - 0.39).powi(2)).sqrt();
    if d1 < 0.18 && d2 > 0.15 {
        col = [0.96, 0.94, 0.98];
    }
    let cloud = 0.68 + 0.05 * (u * 10.0).sin();
    if v > cloud {
        col = lerp_rgb(col, [0.23, 0.27, 0.42], (v - cloud) / 0.32);
    }
    col
}

fn paint_misty_arch(u: f32, v: f32) -> [f32; 3] {
    let glow = (-((u - 0.52).powi(2) + (v - 0.58).powi(2)) * 9.0).exp();
    let mut col = lerp_rgb([0.16, 0.21, 0.25], [0.78, 0.68, 0.56], glow);
    let arch_dist = ((u - 0.52).powi(2) * 1.4 + (v - 0.62).powi(2)).sqrt();
    if arch_dist > 0.34 {
        col = [0.10, 0.13, 0.16];
    }
    col
}

fn paint_coastal_island(u: f32, v: f32) -> [f32; 3] {
    let mut col = if v < 0.56 {
        lerp_rgb([0.62, 0.78, 0.88], [0.98, 0.87, 0.68], v / 0.56)
    } else {
        lerp_rgb([0.85, 0.76, 0.58], [0.22, 0.46, 0.56], (v - 0.56) / 0.44)
    };
    let island = 0.42 + (u - 0.25).powi(2) * 2.8;
    if v > island && v < 0.58 && u < 0.62 {
        col = [0.24, 0.33, 0.28];
    }
    col
}

fn paint_twilight_horizon(u: f32, v: f32) -> [f32; 3] {
    let glow = (-((u - 0.5).powi(2) + (v - 0.55).powi(2)) * 8.0).exp();
    lerp_rgb([0.11, 0.23, 0.36], [0.68, 0.82, 0.88], glow)
}

fn paint_dawn_reflection(u: f32, v: f32) -> [f32; 3] {
    let sun = (-((u - 0.5).powi(2) + (v - 0.48).powi(2)) * 10.0).exp();
    lerp_rgb([0.24, 0.31, 0.42], [0.96, 0.72, 0.46], sun)
}

fn paint_alpine_mist(_u: f32, v: f32) -> [f32; 3] {
    lerp_rgb([0.22, 0.42, 0.48], [0.72, 0.84, 0.86], v)
}

fn paint_warm_dusk(_u: f32, v: f32) -> [f32; 3] {
    lerp_rgb([0.38, 0.28, 0.32], [0.90, 0.66, 0.50], v)
}

fn paint_emerald_ring(u: f32, v: f32) -> [f32; 3] {
    let d = ((u - 0.5).powi(2) + (v - 0.5).powi(2)).sqrt();
    let ring = (-((d - 0.26) * 18.0).powi(2)).exp();
    lerp_rgb([0.06, 0.14, 0.14], [0.22, 0.86, 0.64], ring)
}

fn paint_violet_aura(u: f32, v: f32) -> [f32; 3] {
    lerp_rgb([0.36, 0.28, 0.68], [0.84, 0.52, 0.76], (u + v) * 0.5)
}
