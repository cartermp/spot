use iced::widget::image;
use iced::window;

/// Renders the signature Spot macOS squircle application icon into an RGBA8 pixel buffer
/// of `size × size` (e.g. `1024×1024` for `.icns`, `512×512` for the macOS Dock, `128×128` for sidebar badge).
///
/// Aesthetic design:
/// - **Apple HIG Continuous Squircle (`|x|^4.35 + |y|^4.35 = R^4.35`)** in warm brushed
///   champagne-alabaster ceramic (`#FBF8F2` -> `#DFD5C3`) with a specular top rim and soft
///   ambient drop shadow.
/// - **Recessed Warm-Brass Chamfer & Obsidian Studio Medallion** with subtle emerald acoustic glow.
/// - **"Spot" the Music Dog**: A sculpted cream-ivory terrier pup with floppy espresso ears,
///   a signature dark eye-spot patch, glossy expressive eyes with catchlights, a button nose,
///   emerald collar with a gold tag, and studio emerald headphones.
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
            let squircle_norm =
                (ax.powf(squircle_exp) + ay.powf(squircle_exp)).powf(1.0 / squircle_exp);
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
                let top_light =
                    ((-((nx + 0.14).powi(2) + (ny + 0.24).powi(2)) * 3.8).exp()).clamp(0.0, 1.0);

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
                let medallion_r = 0.324f32;
                let bezel_outer_r = 0.338f32;

                if r >= medallion_r && r < bezel_outer_r + 0.020 {
                    let bezel_t =
                        ((bezel_outer_r - r) / (bezel_outer_r - medallion_r)).clamp(0.0, 1.0);
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
                    // Deep warm obsidian-forest studio background inside the medallion
                    let disc_diag = ((nx + ny) / (2.0 * medallion_r) + 0.5).clamp(0.0, 1.0);
                    let mut disc = lerp3([26.0, 34.0, 32.0], [12.0, 16.0, 15.0], disc_diag);

                    // Soft radial emerald studio backlight behind Spot
                    let emerald_glow = (-(r / (medallion_r * 0.78)).powi(2) * 1.35).exp();
                    disc = lerp3(disc, [22.0, 74.0, 46.0], emerald_glow * 0.50);

                    // Inner recessed shadow around the medallion rim
                    let rim_d = (medallion_r - r).max(0.0);
                    if rim_d < 0.025 {
                        let inner_shadow = (1.0 - rim_d / 0.025).powi(2) * 0.48;
                        disc = lerp3(disc, [6.0, 8.0, 10.0], inner_shadow);
                    }

                    // Normalized coordinates inside the medallion: u, v in [-1, 1]
                    let u = nx / medallion_r;
                    let v = ny / medallion_r;
                    let aa = (1.85 / n) / medallion_r;

                    // --- 3A. Studio Emerald Headphone Band Arc (curving over the crown) ---
                    let hb_dy = v - (-0.04);
                    let hb_r = (u * u + hb_dy * hb_dy).sqrt();
                    let hb_radius = 0.485f32;
                    let hb_half_thick = 0.044f32;
                    if v < -0.10 {
                        let hb_dist = (hb_r - hb_radius).abs() - hb_half_thick;
                        let hb_cov = smooth_cov(hb_dist, aa);
                        if hb_cov > 0.0 {
                            let crown_t = ((v + 0.56) / 0.50).clamp(0.0, 1.0);
                            let mut hb_col = lerp3(
                                [98.0, 248.0, 158.0], // luminous mint-emerald crown
                                [20.0, 150.0, 75.0],  // deep emerald sides
                                crown_t,
                            );
                            let tube_spec =
                                (1.0 - ((hb_r - (hb_radius - 0.012)).abs() / hb_half_thick))
                                    .clamp(0.0, 1.0)
                                    .powi(2);
                            hb_col = lerp3(hb_col, [235.0, 255.0, 242.0], tube_spec * 0.42);
                            disc = lerp3(disc, hb_col, hb_cov);
                        }
                    }

                    // --- 3B. Floppy Espresso Terrier Ears (Left & Right) ---
                    let ear_l_d = sd_rotated_ellipse(u + 0.40, v + 0.08, 0.165, 0.305, 0.42);
                    let ear_r_d = sd_rotated_ellipse(u - 0.40, v + 0.08, 0.165, 0.305, -0.42);
                    let ear_d = ear_l_d.min(ear_r_d);
                    let ear_cov = smooth_cov(ear_d, aa);
                    if ear_cov > 0.0 {
                        let ear_shade = ((v + 0.35) / 0.65).clamp(0.0, 1.0);
                        let mut ear_col = lerp3(
                            [56.0, 48.0, 43.0], // warm roasted espresso top
                            [26.0, 22.0, 20.0], // deep charcoal-espresso tip
                            ear_shade,
                        );
                        // Soft specular highlight near top outer curve of each ear
                        let ear_hi = ((-((v + 0.24).powi(2)) * 28.0).exp()) * 0.22;
                        ear_col = lerp3(ear_col, [110.0, 96.0, 86.0], ear_hi);
                        disc = lerp3(disc, ear_col, ear_cov);
                    }

                    // --- 3C. Emerald Collar & Warm Gold Medal Tag at Neck Base ---
                    let collar_d = sd_rotated_ellipse(u, v - 0.39, 0.27, 0.075, 0.0);
                    let collar_cov = smooth_cov(collar_d, aa);
                    if collar_cov > 0.0 {
                        let collar_t = ((u + 0.27) / 0.54).clamp(0.0, 1.0);
                        let collar_col = lerp3(
                            [42.0, 214.0, 115.0],
                            [16.0, 132.0, 64.0],
                            (collar_t - 0.35).abs() * 1.4,
                        );
                        disc = lerp3(disc, collar_col, collar_cov);
                    }

                    // --- 3D. Sculpted Cream-Ivory Dog Head & Plush Cheeks ---
                    let d_cranium = sd_ellipse(u, v + 0.07, 0.385, 0.355);
                    let d_cheeks = sd_ellipse(u, v - 0.13, 0.355, 0.245);
                    let head_d = smin(d_cranium, d_cheeks, 0.11);

                    // Soft drop shadow cast by head onto ears & collar
                    if head_d > 0.0 && head_d < 0.065 {
                        let sh = (1.0 - head_d / 0.065).powi(2) * 0.35;
                        disc = lerp3(disc, [8.0, 10.0, 10.0], sh);
                    }

                    let head_cov = smooth_cov(head_d, aa);
                    if head_cov > 0.0 {
                        // Warm cream-alabaster 3D fur shading
                        let fur_diag = ((u * 0.7 + v) * 0.5 + 0.5).clamp(0.0, 1.0);
                        let mut fur_col = lerp3(
                            [253.0, 250.0, 243.0], // bright warm cream crown
                            [225.0, 215.0, 198.0], // warm biscuit lower cheek shadow
                            fur_diag,
                        );
                        let crown_glow =
                            ((-((u + 0.10).powi(2) + (v + 0.24).powi(2)) * 6.5).exp()) * 0.35;
                        fur_col = blend_screen(fur_col, [255.0, 254.0, 250.0], crown_glow);

                        // --- 3E. Signature "Spot" Eye Patch (Left Eye) ---
                        let spot_d = sd_rotated_ellipse(u + 0.155, v + 0.055, 0.148, 0.172, -0.26);
                        let spot_cov = smooth_cov(spot_d, aa);
                        if spot_cov > 0.0 {
                            let spot_t = ((v + 0.22) / 0.34).clamp(0.0, 1.0);
                            let spot_col = lerp3(
                                [48.0, 42.0, 38.0], // warm dark espresso
                                [24.0, 21.0, 19.0], // rich ink charcoal
                                spot_t,
                            );
                            fur_col = lerp3(fur_col, spot_col, spot_cov);
                        }

                        // --- 3F. Raised Plush Muzzle Pad & Happy Smile + Coral Tongue ---
                        let muzzle_d = sd_ellipse(u, v - 0.145, 0.215, 0.150);
                        let muzzle_cov = smooth_cov(muzzle_d, aa * 1.4);
                        if muzzle_cov > 0.0 {
                            let muz_t = ((v - 0.02) / 0.26).clamp(0.0, 1.0);
                            let muz_col = lerp3(
                                [255.0, 253.0, 248.0],
                                [236.0, 228.0, 214.0],
                                muz_t,
                            );
                            fur_col = lerp3(fur_col, muz_col, muzzle_cov * 0.92);
                        }

                        // Cheerful Coral-Rose Tongue Peek beneath the smile line
                        let tongue_d = sd_ellipse(u, v - 0.225, 0.058, 0.052);
                        if v > 0.185 {
                            let tongue_cov = smooth_cov(tongue_d, aa);
                            if tongue_cov > 0.0 {
                                let tg_t = ((v - 0.185) / 0.09).clamp(0.0, 1.0);
                                let tg_col = lerp3(
                                    [218.0, 92.0, 90.0],
                                    [246.0, 138.0, 132.0],
                                    tg_t,
                                );
                                fur_col = lerp3(fur_col, tg_col, tongue_cov);
                            }
                        }

                        // Friendly "W" Puppy Smile Curves (left and right arcs meeting under the nose)
                        let smile_l_r = ((u + 0.055).powi(2) + (v - 0.155).powi(2)).sqrt();
                        let smile_r_r = ((u - 0.055).powi(2) + (v - 0.155).powi(2)).sqrt();
                        if v >= 0.145 && u.abs() <= 0.118 {
                            let arc_d = (smile_l_r - 0.055).abs().min((smile_r_r - 0.055).abs()) - 0.012;
                            let smile_cov = smooth_cov(arc_d, aa);
                            fur_col = lerp3(fur_col, [32.0, 28.0, 25.0], smile_cov * 0.90);
                        }
                        // Vertical philtrum line from nose to smile center
                        if u.abs() < 0.014 && v > 0.11 && v < 0.165 {
                            let phil_cov = smooth_cov(u.abs() - 0.010, aa);
                            fur_col = lerp3(fur_col, [32.0, 28.0, 25.0], phil_cov * 0.85);
                        }

                        // --- 3G. Expressive Glossy Eyes with Specular Catchlights ---
                        // Left eye (inside the dark Spot patch: warm cream-amber outer iris ring so it pops!)
                        let eye_l_d = ((u + 0.148).powi(2) + (v + 0.052).powi(2)).sqrt();
                        let eye_l_ring_cov = smooth_cov(eye_l_d - 0.064, aa);
                        if eye_l_ring_cov > 0.0 {
                            fur_col = lerp3(fur_col, [246.0, 238.0, 224.0], eye_l_ring_cov);
                        }
                        let eye_l_pupil_cov = smooth_cov(eye_l_d - 0.049, aa);
                        if eye_l_pupil_cov > 0.0 {
                            fur_col = lerp3(fur_col, [18.0, 16.0, 15.0], eye_l_pupil_cov);
                        }
                        let catch_l_d = ((u + 0.164).powi(2) + (v + 0.068).powi(2)).sqrt() - 0.017;
                        let catch_l_cov = smooth_cov(catch_l_d, aa);
                        if catch_l_cov > 0.0 {
                            fur_col = lerp3(fur_col, [255.0, 255.0, 255.0], catch_l_cov);
                        }

                        // Right eye (on the cream fur)
                        let eye_r_d = ((u - 0.148).powi(2) + (v + 0.052).powi(2)).sqrt();
                        let eye_r_cov = smooth_cov(eye_r_d - 0.052, aa);
                        if eye_r_cov > 0.0 {
                            fur_col = lerp3(fur_col, [20.0, 18.0, 17.0], eye_r_cov);
                        }
                        let catch_r_d = ((u - 0.132).powi(2) + (v + 0.068).powi(2)).sqrt() - 0.017;
                        let catch_r_cov = smooth_cov(catch_r_d, aa);
                        if catch_r_cov > 0.0 {
                            fur_col = lerp3(fur_col, [255.0, 255.0, 255.0], catch_r_cov);
                        }

                        // --- 3H. Sculpted Glossy Button Nose ---
                        let nose_d = sd_ellipse(u, v - 0.078, 0.088, 0.060);
                        let nose_cov = smooth_cov(nose_d, aa);
                        if nose_cov > 0.0 {
                            let nose_t = ((v - 0.02) / 0.12).clamp(0.0, 1.0);
                            let mut nose_col = lerp3(
                                [44.0, 40.0, 38.0],
                                [16.0, 14.0, 13.0],
                                nose_t,
                            );
                            // Crisp top bridge studio highlight on the nose
                            let nose_spec_d = sd_ellipse(u + 0.015, v - 0.052, 0.044, 0.016);
                            let nose_spec_cov = smooth_cov(nose_spec_d, aa * 1.2);
                            nose_col = lerp3(nose_col, [210.0, 206.0, 200.0], nose_spec_cov * 0.55);
                            fur_col = lerp3(fur_col, nose_col, nose_cov);
                        }

                        disc = lerp3(disc, fur_col, head_cov);
                    }

                    // --- 3I. Gold Collar Medal Tag (hanging in front of the collar) ---
                    let tag_d = (u * u + (v - 0.455).powi(2)).sqrt() - 0.052;
                    let tag_cov = smooth_cov(tag_d, aa);
                    if tag_cov > 0.0 {
                        let tag_t = ((u + v - 0.40) / 0.12).clamp(0.0, 1.0);
                        let tag_col = lerp3(
                            [252.0, 224.0, 132.0], // bright warm gold
                            [204.0, 154.0, 54.0],  // rich burnished gold
                            tag_t,
                        );
                        disc = lerp3(disc, tag_col, tag_cov);
                    }

                    // --- 3J. Studio Emerald Headphone Earcups (Hugging Both Cheeks) ---
                    let cup_l_d = sd_rotated_ellipse(u + 0.445, v - 0.02, 0.095, 0.168, 0.10);
                    let cup_r_d = sd_rotated_ellipse(u - 0.445, v - 0.02, 0.095, 0.168, -0.10);
                    let cup_d = cup_l_d.min(cup_r_d);
                    let cup_cov = smooth_cov(cup_d, aa);
                    if cup_cov > 0.0 {
                        let cup_vt = ((v + 0.18) / 0.34).clamp(0.0, 1.0);
                        let mut cup_col = lerp3(
                            [86.0, 244.0, 148.0], // bright mint-emerald top
                            [16.0, 136.0, 66.0],  // deep forest emerald bottom
                            cup_vt,
                        );
                        // Inner dark cushion rim closest to the head
                        let inner_pad_l =
                            sd_rotated_ellipse(u + 0.385, v - 0.02, 0.036, 0.145, 0.10);
                        let inner_pad_r =
                            sd_rotated_ellipse(u - 0.385, v - 0.02, 0.036, 0.145, -0.10);
                        let pad_cov = smooth_cov(inner_pad_l.min(inner_pad_r), aa);
                        cup_col = lerp3(cup_col, [28.0, 34.0, 32.0], pad_cov * 0.85);
                        disc = lerp3(disc, cup_col, cup_cov);
                    }

                    // 4. Delicate Top-Hemisphere Studio Glass Lens Reflection
                    let lens_y = ny + 0.26 * medallion_r;
                    let lens_r = (nx * nx * 0.82 + lens_y * lens_y).sqrt();
                    if ny < -0.02 * medallion_r && lens_r < medallion_r * 0.88 {
                        let glass = ((1.0 - lens_r / (medallion_r * 0.88)).clamp(0.0, 1.0))
                            * ((-ny / medallion_r).clamp(0.0, 1.0))
                            * 0.12;
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

fn sd_ellipse(px: f32, py: f32, rx: f32, ry: f32) -> f32 {
    let nx = px / rx.max(1e-5);
    let ny = py / ry.max(1e-5);
    let r = (nx * nx + ny * ny).sqrt();
    (r - 1.0) * rx.min(ry)
}

fn sd_rotated_ellipse(px: f32, py: f32, rx: f32, ry: f32, angle_rad: f32) -> f32 {
    let (s, c) = angle_rad.sin_cos();
    let rx_p = px * c - py * s;
    let ry_p = px * s + py * c;
    sd_ellipse(rx_p, ry_p, rx, ry)
}

fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = (0.5 + 0.5 * (b - a) / k.max(1e-5)).clamp(0.0, 1.0);
    lerp(b, a, h) - k * h * (1.0 - h)
}

fn smooth_cov(dist: f32, feather: f32) -> f32 {
    ((feather - dist) / (2.0 * feather.max(1e-5))).clamp(0.0, 1.0)
}

/// Exports the rendered Spot macOS squircle dog icon as a PNG file of `size × size`.
pub fn export_icon_png(path: impl AsRef<std::path::Path>, size: u32) -> Result<(), String> {
    let rgba = render_macos_app_icon_rgba(size);
    let img = ::image::RgbaImage::from_raw(size, size, rgba)
        .ok_or_else(|| "failed to construct RgbaImage buffer".to_string())?;
    if let Some(parent) = path.as_ref().parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    img.save(path.as_ref()).map_err(|e| e.to_string())
}

/// Returns an `iced::window::Icon` (`256×256` RGBA) for the Spot window.
pub fn window_icon() -> Option<window::Icon> {
    let size = 256u32;
    let rgba = render_macos_app_icon_rgba(size);
    window::icon::from_rgba(rgba, size, size).ok()
}

/// Returns a high-DPI `image::Handle` (`128×128` RGBA) for the Spot dog icon in the sidebar.
pub fn sidebar_badge_handle() -> image::Handle {
    let size = 128u32;
    let rgba = render_macos_app_icon_rgba(size);
    image::Handle::from_rgba(size, size, rgba)
}

/// Installs the high-resolution `512×512` Spot dog squircle icon directly onto
/// `NSApplication::sharedApplication().setApplicationIconImage(...)` so the macOS Dock
/// and `Cmd+Tab` App Switcher immediately display the custom Spot dog icon even when
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

    tracing::info!("[AppIcon] Installed 512x512 Retina Spot dog icon on macOS Dock & App Switcher");
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
        // Center pixel (128, 128) inside the dog face should be fully opaque
        let center_idx = (128 * 256 + 128) * 4;
        assert_eq!(rgba[center_idx + 3], 255);
        // Forehead pixel (128, 106) is warm cream fur (high R/G/B)
        let forehead_idx = (106 * 256 + 128) * 4;
        assert!(rgba[forehead_idx] > 210 && rgba[forehead_idx + 1] > 205);
        assert!(window_icon().is_some());

        if let Ok(path) = std::env::var("SPOT_EXPORT_ICON_PATH")
            .or_else(|_| std::env::var("GROOVE_EXPORT_ICON_PATH"))
        {
            export_icon_png(&path, 1024).expect("saved PNG icon");
        }
    }
}
