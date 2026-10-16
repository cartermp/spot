use iced::widget::image;
use iced::window;

/// Renders the signature Groove macOS squircle application icon into an RGBA8 pixel buffer
/// of `size × size` (e.g. `512×512` for the macOS Dock / `.icns`, `128×128` for sidebar badge).
///
/// Aesthetic design:
/// - **Apple HIG Continuous Squircle (`|x|^4.35 + |y|^4.35 = R^4.35`)** in warm brushed
///   champagne-alabaster ceramic (`#FBF8F2` -> `#DFD5C3`) with a specular top rim and soft
///   ambient drop shadow.
/// - **Recessed Warm-Brass Chamfer & Obsidian Vinyl Medallion** with anisotropic diagonal
///   satin micro-grooves.
/// - **Luminous Emerald & Warm Amber Acoustic Ribbon Wave** sweeping behind 5 sculpted
///   emerald-jade harmonic soundbars with top specular highlights and soft glass lens sheen.
pub fn render_macos_app_icon_rgba(size: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    let n = size.max(16) as f32;

    // Apple HIG squircle occupies ~82.4% of the canvas with room for the Dock drop shadow.
    let half_span = 0.412f32;
    let squircle_exp = 4.35f32;
    let feather = (1.35 / n).max(0.0025);

    for py in 0..size {
        let ny = (py as f32 + 0.5) / n - 0.5;
        for px in 0..size {
            let nx = (px as f32 + 0.5) / n - 0.5;

            // Squircle signed-distance approximation: d_sq = (|nx/a|^p + |ny/a|^p)^(1/p)
            let ax = (nx.abs() / half_span).max(1e-6);
            let ay = (ny.abs() / half_span).max(1e-6);
            let squircle_norm = (ax.powf(squircle_exp) + ay.powf(squircle_exp)).powf(1.0 / squircle_exp);
            let squircle_dist = (squircle_norm - 1.0) * half_span;

            // Soft multi-layer Dock drop shadow (shifted slightly downward)
            let sh_nx = nx / (half_span * 1.02);
            let sh_ny = (ny - 0.022) / (half_span * 1.02);
            let sh_norm = (sh_nx.abs().powf(squircle_exp) + sh_ny.abs().powf(squircle_exp))
                .powf(1.0 / squircle_exp);
            let shadow_alpha = if squircle_dist > 0.0 {
                let d = (sh_norm - 1.0).max(0.0);
                ((-d * d * 52.0).exp() * 0.34).clamp(0.0, 0.34)
            } else {
                0.0
            };

            let body_cov = ((feather - squircle_dist) / (2.0 * feather)).clamp(0.0, 1.0);
            if body_cov <= 0.0 && shadow_alpha <= 0.002 {
                continue;
            }

            let mut rgb = [18.0f32, 20.0, 24.0];
            let mut out_alpha = shadow_alpha;

            if body_cov > 0.0 {
                // 1. Warm Brushed Alabaster-Champagne Ceramic Squircle Chassis
                let diag = ((nx + ny) / (2.0 * half_span) + 0.5).clamp(0.0, 1.0);
                let top_light = ((-((nx + 0.14).powi(2) + (ny + 0.24).powi(2)) * 3.8).exp())
                    .clamp(0.0, 1.0);

                let mut body = lerp3(
                    [252.0, 249.0, 242.0], // warm ivory top-left
                    [219.0, 210.0, 194.0], // champagne stone bottom-right
                    diag,
                );
                body = blend_screen(body, [255.0, 253.0, 248.0], top_light * 0.38);

                // Subtle fine horizontal brushed grain on the ceramic squircle
                let grain = ((ny * 260.0).sin() * (nx * 47.0).cos()) * 1.6;
                body[0] = (body[0] + grain).clamp(0.0, 255.0);
                body[1] = (body[1] + grain).clamp(0.0, 255.0);
                body[2] = (body[2] + grain).clamp(0.0, 255.0);

                // Squircle outer rim specular highlight (top edge) & warm bottom bevel
                let edge_band = (1.0 - (squircle_dist.abs() / 0.016)).clamp(0.0, 1.0);
                if ny < 0.0 {
                    let rim_strength = edge_band * (-ny / half_span).clamp(0.0, 1.0) * 0.78;
                    body = lerp3(body, [255.0, 255.0, 255.0], rim_strength);
                } else {
                    let bevel_strength = edge_band * (ny / half_span).clamp(0.0, 1.0) * 0.45;
                    body = lerp3(body, [178.0, 166.0, 148.0], bevel_strength);
                }

                // 2. Central Recessed Obsidian Acoustic Medallion & Warm Brass Chamfer Ring
                let r = (nx * nx + ny * ny).sqrt();
                let theta = ny.atan2(nx);
                let medallion_r = 0.318f32;
                let bezel_outer_r = 0.332f32;

                // Soft recessed shadow just outside the bezel
                if r >= medallion_r && r < bezel_outer_r + 0.020 {
                    let bezel_t = ((bezel_outer_r - r) / (bezel_outer_r - medallion_r)).clamp(0.0, 1.0);
                    let brass_sheen = (0.5 + 0.5 * (theta + 0.85).cos()).powi(2);
                    let brass_color = lerp3(
                        [164.0, 142.0, 108.0],
                        [238.0, 219.0, 184.0],
                        brass_sheen,
                    );
                    let ring_cov = ((bezel_outer_r + feather - r) / (2.0 * feather)).clamp(0.0, 1.0)
                        * ((r - medallion_r + feather) / (2.0 * feather)).clamp(0.0, 1.0);
                    body = lerp3(body, brass_color, ring_cov * (0.65 + 0.35 * bezel_t));
                }

                let disc_cov = ((medallion_r + feather - r) / (2.0 * feather)).clamp(0.0, 1.0);
                if disc_cov > 0.0 {
                    // Deep warm obsidian-onyx studio vinyl base
                    let disc_diag = ((nx + ny) / (2.0 * medallion_r) + 0.5).clamp(0.0, 1.0);
                    let mut disc = lerp3([30.0, 35.0, 42.0], [14.0, 16.0, 20.0], disc_diag);

                    // Ambient emerald studio glow near the center of the medallion
                    let emerald_glow = (-(r / (medallion_r * 0.72)).powi(2) * 1.45).exp();
                    disc = lerp3(disc, [18.0, 56.0, 38.0], emerald_glow * 0.42);

                    // Anisotropic diagonal satin sheen + concentric vinyl micro-grooves
                    if r > 0.055 && r < medallion_r - 0.014 {
                        let sheen_lobe = (2.0 * (theta - 0.68)).cos().max(0.0).powi(3);
                        let groove_wave = 0.5 + 0.5 * (r * 390.0).sin();
                        let groove_alpha = (0.04 + 0.14 * sheen_lobe) * groove_wave;
                        disc = lerp3(disc, [110.0, 132.0, 124.0], groove_alpha);
                    }

                    // Inner recessed shadow around the top-left of the medallion rim
                    let rim_d = (medallion_r - r).max(0.0);
                    if rim_d < 0.025 {
                        let inner_shadow = (1.0 - rim_d / 0.025).powi(2) * 0.48;
                        disc = lerp3(disc, [6.0, 8.0, 10.0], inner_shadow);
                    }

                    // 3. Sweeping Translucent Acoustic Silk Ribbons across the medallion
                    let u = (nx / medallion_r).clamp(-1.0, 1.0);
                    let v = ny / medallion_r;
                    let env = (1.0 - u * u).max(0.0).powf(0.65);

                    // Warm Sunset-Gold harmonic under-ribbon
                    let gold_center = (u * 2.7 - 0.35).sin() * 0.16 * env + 0.04;
                    let gold_dist = (v - gold_center).abs();
                    let gold_band = (-(gold_dist / (0.095 * env + 0.018)).powi(2) * 1.8).exp() * env;
                    disc = lerp3(disc, [242.0, 162.0, 84.0], (gold_band * 0.46).clamp(0.0, 0.55));

                    // Lush Emerald-Jade primary acoustic ribbon
                    let em_center = -(u * 2.5 + 0.20).sin() * 0.18 * env;
                    let em_dist = (v - em_center).abs();
                    let em_band = (-(em_dist / (0.105 * env + 0.020)).powi(2) * 1.7).exp() * env;
                    disc = lerp3(disc, [26.0, 208.0, 104.0], (em_band * 0.58).clamp(0.0, 0.68));

                    // 4. Five Sculpted Harmonic Soundbars (Groove Brand Mark)
                    // Normalized coordinates inside the medallion: u in [-1, 1], v in [-1, 1]
                    let bar_specs: [(f32, f32); 5] = [
                        (-0.46, 0.25),
                        (-0.23, 0.46),
                        (0.00, 0.66),
                        (0.23, 0.42),
                        (0.46, 0.23),
                    ];
                    let bar_half_w = 0.058f32;
                    let bar_feath = (2.2 / n) / medallion_r;

                    for (bx, bh) in bar_specs {
                        let dx = (u - bx).abs();
                        let dy = v.abs();
                        // Capsule SDF: segment of half-height (bh - bar_half_w) with radius bar_half_w
                        let seg_h = (bh - bar_half_w).max(0.01);
                        let qy = (dy - seg_h).max(0.0);
                        let cap_dist = (dx * dx + qy * qy).sqrt() - bar_half_w;

                        // Soft emerald halo around each bar
                        if cap_dist > 0.0 && cap_dist < 0.09 {
                            let halo = (-(cap_dist / 0.048).powi(2)).exp() * 0.26;
                            disc = blend_screen(disc, [32.0, 224.0, 114.0], halo);
                        }

                        let bar_cov = ((bar_feath - cap_dist) / (2.0 * bar_feath)).clamp(0.0, 1.0);
                        if bar_cov > 0.0 {
                            // Vertical luminous emerald-to-jade gradient along the bar
                            let vt = ((v / bh) * 0.5 + 0.5).clamp(0.0, 1.0);
                            let mut bar_rgb = if vt < 0.5 {
                                lerp3(
                                    [98.0, 248.0, 158.0], // bright mint-emerald crown
                                    [26.0, 204.0, 98.0],  // vibrant studio emerald center
                                    vt * 2.0,
                                )
                            } else {
                                lerp3(
                                    [26.0, 204.0, 98.0],
                                    [15.0, 134.0, 64.0], // deep rich forest emerald base
                                    (vt - 0.5) * 2.0,
                                )
                            };

                            // Subtle horizontal cylindrical highlight across the bar width
                            let cyl = (1.0 - (dx / bar_half_w).powi(2)).max(0.0).sqrt();
                            bar_rgb = blend_screen(bar_rgb, [215.0, 255.0, 230.0], cyl * 0.22);

                            // Crisp specular cap highlight near the top rounded tip
                            let top_tip_d = ((u - bx).powi(2) + (v + seg_h).powi(2)).sqrt();
                            if top_tip_d < bar_half_w {
                                let tip_glow = (1.0 - top_tip_d / bar_half_w).powi(2) * 0.38;
                                bar_rgb = lerp3(bar_rgb, [255.0, 255.0, 255.0], tip_glow);
                            }

                            disc = lerp3(disc, bar_rgb, bar_cov);
                        }
                    }

                    // 5. Delicate Top-Hemisphere Studio Glass Lens Reflection
                    let lens_y = ny + 0.26 * medallion_r;
                    let lens_r = (nx * nx * 0.82 + lens_y * lens_y).sqrt();
                    if ny < -0.02 * medallion_r && lens_r < medallion_r * 0.88 {
                        let glass = ((1.0 - lens_r / (medallion_r * 0.88)).clamp(0.0, 1.0))
                            * ((-ny / medallion_r).clamp(0.0, 1.0))
                            * 0.14;
                        disc = blend_screen(disc, [255.0, 255.0, 255.0], glass);
                    }

                    body = lerp3(body, disc, disc_cov);
                }

                // Composite squircle body over the ambient drop shadow
                let inv = 1.0 - body_cov;
                out_alpha = body_cov + shadow_alpha * inv;
                if out_alpha > 1e-5 {
                    rgb[0] = (body[0] * body_cov + rgb[0] * shadow_alpha * inv) / out_alpha;
                    rgb[1] = (body[1] * body_cov + rgb[1] * shadow_alpha * inv) / out_alpha;
                    rgb[2] = (body[2] * body_cov + rgb[2] * shadow_alpha * inv) / out_alpha;
                }
            }

            let idx = ((py * size + px) * 4) as usize;
            pixels[idx] = rgb[0].round().clamp(0.0, 255.0) as u8;
            pixels[idx + 1] = rgb[1].round().clamp(0.0, 255.0) as u8;
            pixels[idx + 2] = rgb[2].round().clamp(0.0, 255.0) as u8;
            pixels[idx + 3] = (out_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }

    pixels
}

/// Returns an `iced::window::Icon` (`256×256` RGBA) for the Groove window.
pub fn window_icon() -> Option<window::Icon> {
    let size = 256u32;
    let rgba = render_macos_app_icon_rgba(size);
    window::icon::from_rgba(rgba, size, size).ok()
}

/// Returns a high-DPI `image::Handle` (`128×128` RGBA) for the Groove brand icon in the sidebar.
pub fn sidebar_badge_handle() -> image::Handle {
    let size = 128u32;
    let rgba = render_macos_app_icon_rgba(size);
    image::Handle::from_rgba(size, size, rgba)
}

/// Installs the high-resolution `512×512` Groove squircle icon directly onto
/// `NSApplication::sharedApplication().setApplicationIconImage(...)` so the macOS Dock
/// and `Cmd+Tab` App Switcher immediately display the custom Groove icon even when
/// launched via `cargo run`.
#[cfg(target_os = "macos")]
pub fn install_macos_dock_icon() -> bool {
    use objc2::rc::Retained;
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{
        NSApplication, NSBitmapFormat, NSBitmapImageRep, NSDeviceRGBColorSpace, NSImage,
    };
    use objc2_foundation::NSSize;

    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };

    let width = 512u32;
    let height = 512u32;
    let rgba = render_macos_app_icon_rgba(width);

    unsafe {
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
            return false;
        };

        let size = NSSize::new(256.0, 256.0);
        let ns_image: Retained<NSImage> = NSImage::initWithSize(NSImage::alloc(), size);
        ns_image.addRepresentation(&bitmap_rep);
        ns_image.setTemplate(false);

        let app = NSApplication::sharedApplication(mtm);
        app.setApplicationIconImage(Some(&ns_image));
    }

    tracing::info!("[AppIcon] Installed 512x512 Retina squircle icon on macOS Dock & App Switcher");
    true
}

#[cfg(not(target_os = "macos"))]
pub fn install_macos_dock_icon() -> bool {
    false
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t)]
}

fn blend_screen(base: [f32; 3], top: [f32; 3], opacity: f32) -> [f32; 3] {
    let mut out = [0.0; 3];
    let o = opacity.clamp(0.0, 1.0);
    for i in 0..3 {
        let b = base[i] / 255.0;
        let t = top[i] / 255.0;
        let screened = 1.0 - (1.0 - b) * (1.0 - t);
        out[i] = lerp(base[i], screened * 255.0, o);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_app_icon_renders_valid_squircle_rgba_and_window_icon() {
        let rgba = render_macos_app_icon_rgba(256);
        assert_eq!(rgba.len(), 256 * 256 * 4);

        // Corner pixel (0, 0) outside the squircle should be transparent
        assert_eq!(rgba[3], 0);
        // Center pixel (128, 128) inside the acoustic medallion should be fully opaque
        let center_idx = (128 * 256 + 128) * 4;
        assert_eq!(rgba[center_idx + 3], 255);
        assert!(window_icon().is_some());

        if std::env::var("GROOVE_EXPORT_ICON_PATH").is_ok() {
            let path = std::env::var("GROOVE_EXPORT_ICON_PATH").unwrap();
            let hi_rgba = render_macos_app_icon_rgba(1024);
            let img = ::image::RgbaImage::from_raw(1024, 1024, hi_rgba)
                .expect("valid 1024x1024 RGBA buffer");
            img.save(&path).expect("saved PNG icon");
        }
    }
}
