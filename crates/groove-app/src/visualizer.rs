use groove_core::{SpectrumFrame, SPECTRUM_BANDS};
use iced::mouse;
use iced::widget::canvas::gradient::Linear;
use iced::widget::canvas::{self, Frame, Geometry, LineCap, LineJoin, Path, Program, Stroke};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Theme};

/// Renders the signature multi-layered emerald/mint 3D contour ribbon wave visualizer
/// seen in the Groove "Now Playing" mockup.
pub struct RibbonVisualizer {
    pub frame: SpectrumFrame,
}

impl RibbonVisualizer {
    pub fn new(frame: SpectrumFrame) -> Self {
        Self { frame }
    }

    pub fn view<'a, Message: 'a>(frame: SpectrumFrame) -> Element<'a, Message> {
        canvas::Canvas::new(Self::new(frame))
            .width(Length::Fill)
            .height(Length::Fixed(78.0))
            .into()
    }
}

/// Evaluates the interpolated spectrum amplitude at normalized horizontal coordinate `x` in `0.0..=1.0`.
fn sample_spectrum(bands: &[f32; SPECTRUM_BANDS], x: f32) -> f32 {
    let scaled = (x.clamp(0.0, 1.0) * (SPECTRUM_BANDS - 1) as f32).max(0.0);
    let idx0 = (scaled.floor() as usize).min(SPECTRUM_BANDS - 1);
    let idx1 = (idx0 + 1).min(SPECTRUM_BANDS - 1);
    let frac = scaled - idx0 as f32;
    let smooth_frac = frac * frac * (3.0 - 2.0 * frac);
    bands[idx0] * (1.0 - smooth_frac) + bands[idx1] * smooth_frac
}

/// Evaluates a single wave ribbon's height at normalized `x` (`0.0..=1.0`) for ribbon layer `layer_idx`.
fn wave_displacement(frame: &SpectrumFrame, x: f32, layer_idx: usize, contour_t: f32) -> f32 {
    // Taper smoothly at the left and right edges like the mockup
    let edge_envelope = (x * std::f32::consts::PI).sin().max(0.0).powf(1.35);

    // Sample the real-time FFT spectrum with a slight spatial offset per ribbon
    let spec = sample_spectrum(&frame.bands, (x * 0.88 + 0.06).clamp(0.0, 1.0));

    let phase = frame.phase;
    let layer_f = layer_idx as f32;

    // 4 primary architectural peaks matching the mockup silhouette, modulated by live FFT energy
    let peak1 = (-((x - 0.19) * 8.5).powi(2)).exp() * (0.42 + frame.bass_energy * 0.38);
    let peak2 = (-((x - 0.36) * 9.0).powi(2)).exp() * (0.36 + frame.mid_energy * 0.34);
    let peak3 = (-((x - 0.56) * 7.2).powi(2)).exp() * (0.64 + frame.mid_energy * 0.42);
    let peak4 = (-((x - 0.81) * 8.0).powi(2)).exp() * (0.35 + frame.high_energy * 0.32);

    // Travelling harmonic ripples for the 3D ribbon twist
    let carrier1 = (x * 11.5 - phase * 1.15 + layer_f * 0.65 + contour_t * 1.35).sin() * 0.14;
    let carrier2 = (x * 21.0 + phase * 0.82 - layer_f * 0.90 - contour_t * 1.80).cos() * 0.08;

    let base_arch = peak1 + peak2 + peak3 + peak4 + spec * 0.34;
    let ribbon_spread = (contour_t - 0.5) * 0.28 * (0.4 + base_arch);

    ((base_arch + carrier1 + carrier2 + ribbon_spread) * edge_envelope).clamp(-0.18, 1.15)
}

impl<Message> Program<Message> for RibbonVisualizer {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let w = bounds.width;
        let h = bounds.height;
        let baseline_y = h * 0.84;
        let max_amp = h * 0.68;
        let steps = 96usize;

        // 1. Draw 3 translucent gradient-filled wave bodies (back-to-front)
        let ribbon_layers = [
            // Back mint-silver ribbon
            (
                0usize,
                0.85f32,
                Color::from_rgba(0.55, 0.84, 0.76, 0.24),
                Color::from_rgba(0.75, 0.92, 0.86, 0.02),
            ),
            // Mid emerald-teal ribbon
            (
                1usize,
                0.50f32,
                Color::from_rgba(0.11, 0.68, 0.53, 0.32),
                Color::from_rgba(0.11, 0.68, 0.53, 0.03),
            ),
            // Front deep forest-emerald ribbon
            (
                2usize,
                0.20f32,
                Color::from_rgba(0.06, 0.52, 0.39, 0.38),
                Color::from_rgba(0.06, 0.52, 0.39, 0.02),
            ),
        ];

        for &(layer_idx, contour_t, top_color, bottom_color) in &ribbon_layers {
            let fill_path = Path::new(|builder| {
                builder.move_to(Point::new(0.0, baseline_y));
                for i in 0..=steps {
                    let x_norm = i as f32 / steps as f32;
                    let disp = wave_displacement(&self.frame, x_norm, layer_idx, contour_t);
                    let px = x_norm * w;
                    let py = (baseline_y - disp * max_amp).clamp(2.0, h - 2.0);
                    builder.line_to(Point::new(px, py));
                }
                builder.line_to(Point::new(w, baseline_y));
                builder.close();
            });

            let gradient = Linear::new(Point::new(0.0, h * 0.12), Point::new(0.0, baseline_y))
                .add_stop(0.0, top_color)
                .add_stop(1.0, bottom_color);

            frame.fill(&fill_path, gradient);
        }

        // 2. Draw 16 fine 3D topological contour lines sweeping across the ribbons
        let num_contours = 16usize;
        for c in 0..num_contours {
            let contour_t = c as f32 / (num_contours - 1) as f32;
            let layer_idx = c % 3;

            let path = Path::new(|builder| {
                for i in 0..=steps {
                    let x_norm = i as f32 / steps as f32;
                    let disp = wave_displacement(&self.frame, x_norm, layer_idx, contour_t);
                    let px = x_norm * w;
                    let py = (baseline_y - disp * max_amp).clamp(2.0, h - 2.0);
                    if i == 0 {
                        builder.move_to(Point::new(px, py));
                    } else {
                        builder.line_to(Point::new(px, py));
                    }
                }
            });

            // Alternate between crisp emerald ridges and luminous white-mint highlights
            let is_ridge_crest = c == 7 || c == 11 || c == 15;
            let stroke_color = if is_ridge_crest {
                Color::from_rgba(0.08, 0.58, 0.43, 0.58)
            } else if c % 2 == 0 {
                Color::from_rgba(0.14, 0.69, 0.54, 0.28)
            } else {
                Color::from_rgba(0.88, 0.98, 0.94, 0.45)
            };

            let width = if is_ridge_crest { 1.35 } else { 0.85 };

            frame.stroke(
                &path,
                Stroke {
                    style: stroke_color.into(),
                    width,
                    line_cap: LineCap::Round,
                    line_join: LineJoin::Round,
                    ..Stroke::default()
                },
            );
        }

        vec![frame.into_geometry()]
    }
}
