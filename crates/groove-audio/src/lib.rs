use std::f32::consts::{PI, TAU};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait};
use groove_core::{AudioDevice, PlaybackState, SpectrumFrame, SPECTRUM_BANDS};
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use rtrb::{Consumer, Producer, RingBuffer};

/// Capacity of the lock-free SPSC ring buffer (~370 ms of mono audio at 44.1 kHz).
const RING_BUFFER_CAPACITY: usize = 16_384;

/// Sliding window and real FFT size (2048 samples -> ~21.53 Hz bin resolution at 44.1 kHz).
const FFT_SIZE: usize = 2048;

/// Maximum number of mono samples drained from the ring buffer per UI frame.
const MAX_DRAIN_SAMPLES: usize = 4096;

/// Lowest FFT bin included in the 32-band visualizer (~21.5 Hz at 44.1 kHz).
const MIN_FFT_BIN: usize = 1;

/// Highest FFT bin included in the 32-band visualizer (~19.4 kHz at 44.1 kHz).
const MAX_FFT_BIN: usize = 900;

/// Number of consecutive frames without incoming PCM samples before transitioning
/// from live FFT analysis back to procedural harmonic preview synthesis.
const LIVE_AUDIO_HOLD_FRAMES: u32 = 15;

/// Exponential smoothing coefficient when a band's amplitude is rising.
const ATTACK_COEFF: f32 = 0.38;

/// Exponential smoothing coefficient when a band's amplitude is falling.
const RELEASE_COEFF: f32 = 0.09;

/// Dynamic range floor in decibels mapped to `0.0` in normalized band output.
const DB_FLOOR: f32 = -65.0;

/// Non-blocking PCM sample producer shared with the audio decoding/output callback thread.
///
/// Uses `try_lock()` on an `rtrb::Producer<f32>` so the audio thread never blocks even if
/// cloned producers are accessed concurrently.
#[derive(Clone)]
pub struct AudioSampleProducer {
    producer: Arc<Mutex<Producer<f32>>>,
}

impl std::fmt::Debug for AudioSampleProducer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioSampleProducer").finish_non_exhaustive()
    }
}

impl AudioSampleProducer {
    /// Pushes interleaved PCM f32 samples (e.g. stereo 2-channel from librespot) by downmixing to mono into the lock-free ring buffer.
    pub fn push_interleaved(&self, samples: &[f32], channels: usize) {
        if samples.is_empty() || channels == 0 {
            return;
        }

        let Ok(mut prod) = self.producer.try_lock() else {
            return;
        };

        if channels == 1 {
            for &sample in samples {
                let clean = if sample.is_finite() {
                    sample.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                if prod.push(clean).is_err() {
                    break;
                }
            }
        } else {
            for frame in samples.chunks(channels) {
                let mut sum = 0.0f32;
                for &sample in frame {
                    if sample.is_finite() {
                        sum += sample;
                    }
                }
                let mono = (sum / frame.len() as f32).clamp(-1.0, 1.0);
                if prod.push(mono).is_err() {
                    break;
                }
            }
        }
    }

    /// Pushes mono PCM f32 samples into the lock-free ring buffer.
    pub fn push_mono(&self, samples: &[f32]) {
        self.push_interleaved(samples, 1);
    }

    /// Pushes i16 interleaved samples (converted to -1.0..=1.0 f32) into the ring buffer.
    pub fn push_interleaved_i16(&self, samples: &[i16], channels: usize) {
        if samples.is_empty() || channels == 0 {
            return;
        }

        let Ok(mut prod) = self.producer.try_lock() else {
            return;
        };

        const I16_SCALE: f32 = 1.0 / 32768.0;

        if channels == 1 {
            for &sample in samples {
                let mono = (sample as f32 * I16_SCALE).clamp(-1.0, 1.0);
                if prod.push(mono).is_err() {
                    break;
                }
            }
        } else {
            for frame in samples.chunks(channels) {
                let mut sum = 0.0f32;
                for &sample in frame {
                    sum += sample as f32 * I16_SCALE;
                }
                let mono = (sum / frame.len() as f32).clamp(-1.0, 1.0);
                if prod.push(mono).is_err() {
                    break;
                }
            }
        }
    }
}

/// Real-time 2048-point Hann-windowed FFT spectrum analyzer and procedural harmonic fallback.
pub struct SpectrumAnalyzer {
    consumer: Consumer<f32>,
    window_buf: Vec<f32>,
    window_cursor: usize,
    hann_window: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    fft_input: Vec<f32>,
    fft_output: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    bin_edges: [usize; SPECTRUM_BANDS + 1],
    smoothed_bands: [f32; SPECTRUM_BANDS],
    phase: f32,
    frames_since_live_audio: u32,
}

impl std::fmt::Debug for SpectrumAnalyzer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpectrumAnalyzer")
            .field("window_cursor", &self.window_cursor)
            .field("phase", &self.phase)
            .field("frames_since_live_audio", &self.frames_since_live_audio)
            .finish_non_exhaustive()
    }
}

impl Default for SpectrumAnalyzer {
    fn default() -> Self {
        Self::new().0
    }
}

impl SpectrumAnalyzer {
    /// Creates a new `SpectrumAnalyzer` paired with a lock-free `AudioSampleProducer`.
    pub fn new() -> (Self, AudioSampleProducer) {
        let (producer, consumer) = RingBuffer::<f32>::new(RING_BUFFER_CAPACITY);

        let hann_window: Vec<f32> = (0..FFT_SIZE)
            .map(|n| 0.5 * (1.0 - (2.0 * PI * n as f32 / (FFT_SIZE - 1) as f32).cos()))
            .collect();

        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let fft_input = fft.make_input_vec();
        let fft_output = fft.make_output_vec();
        let fft_scratch = fft.make_scratch_vec();

        let bin_edges = compute_log_bin_edges();

        let analyzer = Self {
            consumer,
            window_buf: vec![0.0; FFT_SIZE],
            window_cursor: 0,
            hann_window,
            fft,
            fft_input,
            fft_output,
            fft_scratch,
            bin_edges,
            smoothed_bands: [0.0; SPECTRUM_BANDS],
            phase: 0.0,
            frames_since_live_audio: u32::MAX,
        };

        let sample_producer = AudioSampleProducer {
            producer: Arc::new(Mutex::new(producer)),
        };

        (analyzer, sample_producer)
    }

    /// Advances the spectrum analyzer by `dt_secs` and returns a smoothed 32-band `SpectrumFrame`.
    pub fn next_frame(&mut self, is_playing: bool, dt_secs: f32) -> SpectrumFrame {
        let dt = if dt_secs.is_finite() {
            dt_secs.clamp(0.0, 0.5)
        } else {
            1.0 / 60.0
        };

        if is_playing {
            self.phase += dt * 2.2;
        } else {
            self.phase += dt * 0.25;
        }
        // Wrap at an exact common period (20 * TAU) so fractional phase multipliers remain continuous.
        if self.phase > 20.0 * TAU {
            self.phase -= 20.0 * TAU;
        }

        // Drain up to 4096 mono samples from the ring buffer into the 2048-sample sliding window.
        let mut drained = 0usize;
        while drained < MAX_DRAIN_SAMPLES {
            match self.consumer.pop() {
                Ok(sample) => {
                    let clean = if sample.is_finite() {
                        sample.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    };
                    self.window_buf[self.window_cursor] = clean;
                    self.window_cursor = (self.window_cursor + 1) % FFT_SIZE;
                    drained += 1;
                }
                Err(_) => break,
            }
        }

        if drained > 0 {
            self.frames_since_live_audio = 0;
        } else {
            self.frames_since_live_audio = self.frames_since_live_audio.saturating_add(1);
        }

        let live_audio_active = self.frames_since_live_audio <= LIVE_AUDIO_HOLD_FRAMES;

        let target_bands = if is_playing && live_audio_active {
            self.compute_fft_bands()
        } else if is_playing {
            synthesize_playing_envelope(self.phase)
        } else {
            synthesize_resting_envelope(self.phase)
        };

        for (smoothed, &target) in self.smoothed_bands.iter_mut().zip(target_bands.iter()) {
            let clamped_target = target.clamp(0.0, 1.0);
            let coeff = if clamped_target > *smoothed {
                ATTACK_COEFF
            } else {
                RELEASE_COEFF
            };
            *smoothed = (*smoothed + (clamped_target - *smoothed) * coeff).clamp(0.0, 1.0);
        }

        let sum_sq: f32 = self.smoothed_bands.iter().map(|&b| b * b).sum();
        let rms_energy = (sum_sq / SPECTRUM_BANDS as f32).sqrt().clamp(0.0, 1.0);
        let bass_energy = band_group_energy(&self.smoothed_bands[0..6]);
        let mid_energy = band_group_energy(&self.smoothed_bands[6..20]);
        let high_energy = band_group_energy(&self.smoothed_bands[20..32]);

        SpectrumFrame {
            bands: self.smoothed_bands,
            rms_energy,
            bass_energy,
            mid_energy,
            high_energy,
            phase: self.phase,
        }
    }

    /// Runs the 2048-point Hann-windowed real FFT and maps bins `1..=900` into 32 normalized dB bands.
    fn compute_fft_bands(&mut self) -> [f32; SPECTRUM_BANDS] {
        for n in 0..FFT_SIZE {
            let idx = (self.window_cursor + n) % FFT_SIZE;
            self.fft_input[n] = self.window_buf[idx] * self.hann_window[n];
        }

        if let Err(err) = self.fft.process_with_scratch(
            &mut self.fft_input,
            &mut self.fft_output,
            &mut self.fft_scratch,
        ) {
            tracing::warn!("RealFFT processing error: {err}");
            return [0.0; SPECTRUM_BANDS];
        }

        // Coherent gain normalization for a 2048-point Hann-windowed real FFT.
        let norm_factor = 4.0 / FFT_SIZE as f32;
        let max_valid_bin = self.fft_output.len().saturating_sub(1).min(MAX_FFT_BIN);

        let mut bands = [0.0f32; SPECTRUM_BANDS];
        for (band_idx, band_val) in bands.iter_mut().enumerate() {
            let start_bin = self.bin_edges[band_idx].clamp(MIN_FFT_BIN, max_valid_bin);
            let end_bin_exclusive = if band_idx + 1 == SPECTRUM_BANDS {
                max_valid_bin + 1
            } else {
                self.bin_edges[band_idx + 1]
                    .max(start_bin + 1)
                    .min(max_valid_bin + 1)
            };

            let mut peak_mag = 0.0f32;
            let mut sum_sq = 0.0f32;
            let count = end_bin_exclusive.saturating_sub(start_bin).max(1);

            for bin in start_bin..end_bin_exclusive {
                let c = self.fft_output[bin];
                let mag = (c.re * c.re + c.im * c.im).sqrt() * norm_factor;
                if mag > peak_mag {
                    peak_mag = mag;
                }
                sum_sq += mag * mag;
            }

            let rms_mag = (sum_sq / count as f32).sqrt();
            let combined_mag = peak_mag * 0.8 + rms_mag * 0.2;
            let db = 20.0 * combined_mag.max(1e-6).log10();
            *band_val = ((db - DB_FLOOR) / -DB_FLOOR).clamp(0.0, 1.0);
        }

        bands
    }
}

/// Precomputes 33 strictly increasing logarithmic bin boundaries across bins `1..=900`.
fn compute_log_bin_edges() -> [usize; SPECTRUM_BANDS + 1] {
    let mut edges = [MIN_FFT_BIN; SPECTRUM_BANDS + 1];
    let ratio = MAX_FFT_BIN as f32 / MIN_FFT_BIN as f32;

    for k in 1..=SPECTRUM_BANDS {
        let t = k as f32 / SPECTRUM_BANDS as f32;
        let raw = (MIN_FFT_BIN as f32 * ratio.powf(t)).round() as usize;
        let prev = edges[k - 1];
        let remaining_bands = SPECTRUM_BANDS - k;
        let max_allowed = MAX_FFT_BIN.saturating_sub(remaining_bands);
        edges[k] = raw.max(prev + 1).min(max_allowed);
    }

    edges[SPECTRUM_BANDS] = MAX_FFT_BIN;
    edges
}

/// Synthesizes a rich, musical 32-band harmonic spectrum envelope when playing in preview/demo mode.
fn synthesize_playing_envelope(phase: f32) -> [f32; SPECTRUM_BANDS] {
    let mut bands = [0.0f32; SPECTRUM_BANDS];
    let sub_bass_pulse = (phase * 1.35).sin().abs().powf(1.4) * 0.26;

    for (i, band) in bands.iter_mut().enumerate() {
        let x = i as f32 / (SPECTRUM_BANDS - 1) as f32;
        let window_arch = (x * PI).sin().max(0.0).powf(1.35);

        let bass_swell = (-((x - 0.12) * 5.5).powi(2)).exp() * (0.34 + sub_bass_pulse);
        let warm_mid_harmonic =
            (-((x - 0.48) * 3.8).powi(2)).exp() * (0.42 + 0.16 * (phase * 0.9 + x * 4.0).sin());
        let presence_shimmer =
            (-((x - 0.78) * 6.0).powi(2)).exp() * (0.20 + 0.10 * (phase * 1.7 - x * 9.0).cos());

        let travelling_wave_1 = (x * 6.5 + phase * 1.15).sin() * 0.22;
        let travelling_wave_2 = (x * 13.0 - phase * 0.75).cos() * 0.14;

        let combined = (0.38 + bass_swell + warm_mid_harmonic + presence_shimmer)
            + (travelling_wave_1 + travelling_wave_2) * window_arch;

        *band = (combined * (0.45 + 0.55 * window_arch)).clamp(0.08, 0.98);
    }

    bands
}

/// Synthesizes a calm, low-amplitude resting wave (`0.08..0.22`) when playback is paused.
fn synthesize_resting_envelope(phase: f32) -> [f32; SPECTRUM_BANDS] {
    let mut bands = [0.0f32; SPECTRUM_BANDS];

    for (i, band) in bands.iter_mut().enumerate() {
        let x = i as f32 / (SPECTRUM_BANDS - 1) as f32;
        let arch = (x * PI).sin();
        let breath = (x * 4.5 + phase * 0.8).sin() * 0.035 + (x * 9.0 - phase * 0.5).cos() * 0.02;
        *band = (0.08 + 0.10 * arch + breath).clamp(0.08, 0.22);
    }

    bands
}

/// Computes an expressive energy metric for a frequency region combining mean and peak band levels.
fn band_group_energy(slice: &[f32]) -> f32 {
    if slice.is_empty() {
        return 0.0;
    }
    let mean = slice.iter().copied().sum::<f32>() / slice.len() as f32;
    let peak = slice.iter().copied().fold(0.0f32, f32::max);
    (mean * 0.55 + peak * 0.45).clamp(0.0, 1.0)
}

/// Converts a human-readable device name into a stable kebab-case identifier.
fn slugify_device_id(name: &str) -> String {
    let mut id = String::with_capacity(name.len());
    let mut prev_dash = false;

    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !id.is_empty() {
            id.push('-');
            prev_dash = true;
        }
    }

    while id.ends_with('-') {
        id.pop();
    }

    if id.is_empty() {
        "audio-output-device".to_string()
    } else {
        id
    }
}

/// Enumerates local CoreAudio / system output devices via `cpal`, deduplicating by name and
/// ensuring the mockup fallback devices are always available.
pub fn enumerate_output_devices() -> Vec<AudioDevice> {
    let mut devices: Vec<AudioDevice> = Vec::new();

    let host = cpal::default_host();
    let default_name = host
        .default_output_device()
        .and_then(|device| device.name().ok())
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());

    if let Ok(output_devices) = host.output_devices() {
        for device in output_devices {
            if let Ok(raw_name) = device.name() {
                let name = raw_name.trim().to_string();
                if name.is_empty() {
                    continue;
                }
                if devices
                    .iter()
                    .any(|existing| existing.name.eq_ignore_ascii_case(&name))
                {
                    continue;
                }

                let is_default = default_name
                    .as_deref()
                    .map(|def| def.eq_ignore_ascii_case(&name))
                    .unwrap_or(false);

                devices.push(AudioDevice {
                    id: slugify_device_id(&name),
                    name,
                    is_default,
                    is_spotify_connect: false,
                });
            }
        }
    }

    // Append fallback devices from PlaybackState::mockup_default().devices if missing
    // (e.g. if "MacBook Pro Speakers" is not present or on headless environments).
    let fallback_devices = PlaybackState::mockup_default().devices;
    let has_default_already = devices.iter().any(|d| d.is_default);

    for fallback in fallback_devices {
        if !devices
            .iter()
            .any(|existing| existing.name.eq_ignore_ascii_case(&fallback.name))
        {
            devices.push(AudioDevice {
                id: fallback.id,
                name: fallback.name,
                is_default: !has_default_already && fallback.is_default,
                is_spotify_connect: fallback.is_spotify_connect,
            });
        }
    }

    if !devices.is_empty() && !devices.iter().any(|d| d.is_default) {
        devices[0].is_default = true;
    }

    tracing::info!(
        "Enumerated {} audio output devices (default={:?}): {:?}",
        devices.len(),
        default_name,
        devices.iter().map(|d| (&d.name, d.is_default)).collect::<Vec<_>>()
    );

    devices
}

/// Validates a requested output device name against live `cpal` host output devices.
/// Returns `Some(exact_cpal_name)` only if `cpal` has a matching output device, or `None`
/// to use the system default output device (preventing `rodio::open` from panicking on
/// virtual/fallback device names).
pub fn resolve_cpal_output_device_name(requested: Option<&str>) -> Option<String> {
    let host = cpal::default_host();
    let default_dev_name = host
        .default_output_device()
        .and_then(|d| d.name().ok());

    let Some(req) = requested.map(str::trim).filter(|s| !s.is_empty()) else {
        tracing::info!(
            "Using system default CoreAudio output device: {:?}",
            default_dev_name
        );
        return None;
    };

    if let Ok(mut output_devices) = host.output_devices() {
        if let Some(matched) = output_devices.find_map(|d| {
            let raw_name = d.name().ok()?;
            if raw_name == req || raw_name.trim().eq_ignore_ascii_case(req) {
                Some(raw_name)
            } else {
                None
            }
        }) {
            tracing::info!(
                "Resolved requested audio device {:?} -> live cpal device {:?}",
                req,
                matched
            );
            return Some(matched);
        }
    }

    tracing::warn!(
        "Requested audio device {:?} is not a live cpal output device; falling back to system default {:?}",
        req,
        default_dev_name
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn producer_is_clone_send_and_sync() {
        fn assert_traits<T: Clone + Send + Sync>() {}
        assert_traits::<AudioSampleProducer>();
    }

    #[test]
    fn push_interleaved_downmixes_and_fft_detects_100hz_and_440hz() {
        let (mut analyzer, producer) = SpectrumAnalyzer::new();
        let sample_rate = 44_100.0f32;
        let num_frames = FFT_SIZE * 2;

        // Put 100 Hz on the left channel and 440 Hz on the right channel to verify stereo downmixing.
        let mut stereo_samples = Vec::with_capacity(num_frames * 2);
        for n in 0..num_frames {
            let t = n as f32 / sample_rate;
            let left_100hz = (TAU * 100.0 * t).sin() * 0.85;
            let right_440hz = (TAU * 440.0 * t).sin() * 0.85;
            stereo_samples.push(left_100hz);
            stereo_samples.push(right_440hz);
        }

        producer.push_interleaved(&stereo_samples, 2);

        // Run several frames within the live-audio hold window so attack smoothing converges.
        let mut frame = SpectrumFrame::default();
        for _ in 0..8 {
            frame = analyzer.next_frame(true, 1.0 / 60.0);
        }

        // Verify all bands are normalized in 0.0..=1.0.
        for &b in &frame.bands {
            assert!(
                (0.0..=1.0).contains(&b),
                "band value out of normalized range: {b}"
            );
        }

        let bass_peak = frame.bands[0..6].iter().copied().fold(0.0f32, f32::max);
        let mid_peak = frame.bands[6..20].iter().copied().fold(0.0f32, f32::max);
        let high_peak = frame.bands[20..32].iter().copied().fold(0.0f32, f32::max);

        assert!(
            bass_peak > 0.5,
            "expected strong 100 Hz peak in bass bands (0..6), got {bass_peak}"
        );
        assert!(
            mid_peak > 0.5,
            "expected strong 440 Hz peak in mid bands (6..20), got {mid_peak}"
        );
        assert!(
            high_peak < 0.15,
            "expected minimal energy in high bands (20..32), got {high_peak}"
        );
        assert!(
            frame.bass_energy > frame.high_energy,
            "bass_energy ({}) should exceed high_energy ({})",
            frame.bass_energy,
            frame.high_energy
        );
        assert!(
            frame.mid_energy > frame.high_energy,
            "mid_energy ({}) should exceed high_energy ({})",
            frame.mid_energy,
            frame.high_energy
        );
    }

    #[test]
    fn push_interleaved_i16_and_mono_work() {
        let (mut analyzer, producer) = SpectrumAnalyzer::new();
        let sample_rate = 44_100.0f32;

        let mut i16_stereo = Vec::with_capacity(FFT_SIZE * 2);
        for n in 0..FFT_SIZE {
            let t = n as f32 / sample_rate;
            let s = ((TAU * 440.0 * t).sin() * 24_000.0) as i16;
            i16_stereo.push(s);
            i16_stereo.push(s);
        }

        producer.push_interleaved_i16(&i16_stereo, 2);
        let frame = analyzer.next_frame(true, 1.0 / 60.0);
        assert!(frame.mid_energy > 0.1);

        let mono: Vec<f32> = (0..FFT_SIZE)
            .map(|n| (TAU * 100.0 * (n as f32 / sample_rate)).sin() * 0.8)
            .collect();
        producer.push_mono(&mono);
        let frame2 = analyzer.next_frame(true, 1.0 / 60.0);
        assert!(frame2.bass_energy > 0.1);
    }

    #[test]
    fn preview_and_paused_modes_behave_smoothly() {
        let (mut analyzer, _producer) = SpectrumAnalyzer::new();

        let mut playing_frame = SpectrumFrame::default();
        for _ in 0..10 {
            playing_frame = analyzer.next_frame(true, 1.0 / 60.0);
        }
        assert!(playing_frame.phase > 0.0);
        assert!(playing_frame.rms_energy > 0.2);

        let mut paused_frame = playing_frame;
        for _ in 0..60 {
            paused_frame = analyzer.next_frame(false, 1.0 / 60.0);
        }
        assert!(paused_frame.rms_energy < playing_frame.rms_energy);
        for &b in &paused_frame.bands {
            assert!((0.07..=0.25).contains(&b), "unexpected resting band: {b}");
        }
    }

    #[test]
    fn enumerate_output_devices_includes_unique_devices_and_fallbacks() {
        let devices = enumerate_output_devices();
        assert!(!devices.is_empty(), "device list should never be empty");
        assert!(
            devices
                .iter()
                .any(|d| d.name.eq_ignore_ascii_case("MacBook Pro Speakers")),
            "expected MacBook Pro Speakers in device list: {devices:?}"
        );
        assert_eq!(
            devices.iter().filter(|d| d.is_default).count(),
            1,
            "expected exactly one default output device"
        );

        for (i, a) in devices.iter().enumerate() {
            for b in devices.iter().skip(i + 1) {
                assert!(
                    !a.name.eq_ignore_ascii_case(&b.name),
                    "duplicate device name found: {}",
                    a.name
                );
            }
        }
    }
}

