use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

use groove_audio::AudioSampleProducer;
use librespot_playback::{
    audio_backend::{self, Sink, SinkResult},
    config::AudioFormat,
    convert::Converter,
    decoder::AudioPacket,
    mixer::VolumeGetter,
};
use tracing::warn;

/// Default initial scratch capacity in stereo `f32` samples (2048 stereo frames = 4096 samples).
const INITIAL_SCRATCH_CAPACITY: usize = 4096;

/// Lock-free atomic volume scaler shared between the engine control loop, `librespot` mixer,
/// and `TeeSink`.
#[derive(Debug, Clone)]
pub struct AtomicVolumeScaler {
    volume_bits: Arc<AtomicU32>,
}

impl Default for AtomicVolumeScaler {
    fn default() -> Self {
        Self::new(0.45)
    }
}

impl AtomicVolumeScaler {
    pub fn new(initial_volume: f32) -> Self {
        let clamped = initial_volume.clamp(0.0, 1.0);
        Self {
            volume_bits: Arc::new(AtomicU32::new(clamped.to_bits())),
        }
    }

    #[inline]
    pub fn set(&self, volume: f32) {
        let clamped = volume.clamp(0.0, 1.0);
        self.volume_bits.store(clamped.to_bits(), Ordering::Relaxed);
    }

    #[inline]
    pub fn get(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0)
    }
}

impl VolumeGetter for AtomicVolumeScaler {
    #[inline]
    fn attenuation_factor(&self) -> f64 {
        self.get() as f64
    }
}

/// Real-time audio tee sink that taps decoded 320 kbps PCM frames into `groove_audio`'s
/// lock-free ring buffer for the 120fps GPU spectrum visualizer before forwarding audio
/// to the underlying `rodio` CoreAudio output backend.
pub struct TeeSink {
    inner: Option<Box<dyn Sink>>,
    sample_producer: AudioSampleProducer,
    volume_scaler: AtomicVolumeScaler,
    scratch_f32: Vec<f32>,
}

impl TeeSink {
    /// Creates a new `TeeSink` wrapping the default `librespot_playback` audio backend (`rodio`)
    /// and tapping stereo `f32` samples into `sample_producer`.
    pub fn new(
        device_name: Option<String>,
        format: AudioFormat,
        sample_producer: AudioSampleProducer,
        volume_scaler: AtomicVolumeScaler,
    ) -> Self {
        let inner = match audio_backend::find(None) {
            Some(builder) => Some(builder(device_name, format)),
            None => {
                warn!("No librespot audio backend found; TeeSink will tap visualizer samples only");
                None
            }
        };

        Self {
            inner,
            sample_producer,
            volume_scaler,
            scratch_f32: Vec::with_capacity(INITIAL_SCRATCH_CAPACITY),
        }
    }

    /// Creates a `TeeSink` around an explicit inner `Sink` implementation (useful for testing).
    pub fn with_inner(
        inner: Option<Box<dyn Sink>>,
        sample_producer: AudioSampleProducer,
        volume_scaler: AtomicVolumeScaler,
    ) -> Self {
        Self {
            inner,
            sample_producer,
            volume_scaler,
            scratch_f32: Vec::with_capacity(INITIAL_SCRATCH_CAPACITY),
        }
    }
}

impl Sink for TeeSink {
    fn start(&mut self) -> SinkResult<()> {
        if let Some(inner) = self.inner.as_mut() {
            inner.start()?;
        }
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        if let Some(inner) = self.inner.as_mut() {
            inner.stop()?;
        }
        Ok(())
    }

    fn write(&mut self, mut packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let gain = self.volume_scaler.get();

        match &mut packet {
            AudioPacket::Samples(samples) => {
                if !samples.is_empty() {
                    // Reuse pre-allocated scratch buffer (zero heap allocations on steady-state hot path).
                    self.scratch_f32.clear();
                    self.scratch_f32
                        .extend(samples.iter().map(|&sample| (sample as f32) * gain));
                    self.sample_producer.push_interleaved(&self.scratch_f32, 2);

                    // Apply atomic volume scaling to the outgoing playback samples.
                    let gain_f64 = gain as f64;
                    if (gain_f64 - 1.0).abs() > f64::EPSILON {
                        for sample in samples.iter_mut() {
                            *sample *= gain_f64;
                        }
                    }
                }
            }
            AudioPacket::Raw(raw_bytes) => {
                if !raw_bytes.is_empty() {
                    // Interpret raw PCM as signed 16-bit little-endian stereo frames for the visualizer tap.
                    self.scratch_f32.clear();
                    let norm = (1.0 / 32768.0) * gain;
                    self.scratch_f32.extend(
                        raw_bytes
                            .chunks_exact(2)
                            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 * norm),
                    );
                    if !self.scratch_f32.is_empty() {
                        self.sample_producer.push_interleaved(&self.scratch_f32, 2);
                    }
                }
            }
        }

        if let Some(inner) = self.inner.as_mut() {
            inner.write(packet, converter)?;
        }

        Ok(())
    }
}
