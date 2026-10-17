use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use cpal::traits::{DeviceTrait, HostTrait};
use groove_audio::AudioSampleProducer;
use groove_core::{
    sanitize_pcm_sample, sanitize_unit_float, step_gated_stereo_frame_verified, InstantAudioGate,
    INSTANT_PAUSE_RAMP_FRAMES, INSTANT_SINK_MAX_BUFFERED_FRAMES,
};
use librespot_playback::{
    audio_backend::{Sink, SinkError, SinkResult},
    config::AudioFormat,
    convert::Converter,
    decoder::AudioPacket,
    mixer::VolumeGetter,
};
use tracing::{info, warn};

/// Default initial scratch capacity in stereo `f32` samples (2048 stereo frames = 4096 samples).
const INITIAL_SCRATCH_CAPACITY: usize = 4096;

/// Target sample rate for Spotify Ogg Vorbis streams (44.1 kHz stereo).
const SPOTIFY_SAMPLE_RATE: u32 = 44_100;

/// Number of channels for Spotify stereo streams.
const SPOTIFY_CHANNELS: u16 = 2;

/// Preferred low-latency CoreAudio hardware buffer size in frames (`256` frames = `~5.3 ms`).
const LOW_LATENCY_BUFFER_FRAMES: u32 = 256;

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
        let clamped = sanitize_unit_float(initial_volume, 0.45);
        Self {
            volume_bits: Arc::new(AtomicU32::new(clamped.to_bits())),
        }
    }

    #[inline]
    pub fn set(&self, volume: f32) {
        let clamped = sanitize_unit_float(volume, self.get());
        self.volume_bits.store(clamped.to_bits(), Ordering::Relaxed);
    }

    #[inline]
    pub fn get(&self) -> f32 {
        sanitize_unit_float(f32::from_bits(self.volume_bits.load(Ordering::Relaxed)), 0.45)
    }
}

impl VolumeGetter for AtomicVolumeScaler {
    #[inline]
    fn attenuation_factor(&self) -> f64 {
        self.get() as f64
    }
}

struct SharedAudioQueueState {
    frames: VecDeque<(f32, f32)>,
    last_epoch: u64,
}

/// Real-time stereo frame queue shared between `TeeSink::write` (decoder thread) and
/// `InstantGatedSource` (CoreAudio hardware callback thread).
pub struct SharedAudioQueue {
    state: Mutex<SharedAudioQueueState>,
    audio_gate: InstantAudioGate,
}

impl SharedAudioQueue {
    pub fn new(audio_gate: InstantAudioGate) -> Arc<Self> {
        let initial_epoch = audio_gate.flush_epoch();
        Arc::new(Self {
            state: Mutex::new(SharedAudioQueueState {
                frames: VecDeque::with_capacity(INSTANT_SINK_MAX_BUFFERED_FRAMES * 2),
                last_epoch: initial_epoch,
            }),
            audio_gate,
        })
    }

    pub fn push_frames(&self, incoming: &[(f32, f32)], epoch: u64) -> usize {
        let Ok(mut guard) = self.state.lock() else {
            return 0;
        };
        if guard.last_epoch != epoch {
            guard.frames.clear();
            guard.last_epoch = epoch;
        }
        guard.frames.extend(incoming.iter().copied());
        let len = guard.frames.len();
        self.audio_gate.set_buffered_frames(len);
        len
    }

    #[inline]
    pub fn pop_batch_nonblocking(
        &self,
        local: &mut VecDeque<(f32, f32)>,
        max_frames: usize,
        epoch: u64,
    ) {
        let Ok(mut guard) = self.state.try_lock() else {
            return;
        };
        if guard.last_epoch != epoch {
            guard.frames.clear();
            guard.last_epoch = epoch;
            self.audio_gate.set_buffered_frames(0);
            return;
        }
        let count = guard.frames.len().min(max_frames);
        for _ in 0..count {
            if let Some(frame) = guard.frames.pop_front() {
                local.push_back(frame);
            }
        }
        self.audio_gate
            .set_buffered_frames(guard.frames.len().saturating_add(local.len()));
    }

    #[inline]
    pub fn clear_nonblocking(&self, epoch: u64) {
        if let Ok(mut guard) = self.state.try_lock() {
            guard.frames.clear();
            guard.last_epoch = epoch;
        }
        self.audio_gate.set_buffered_frames(0);
    }

    #[inline]
    pub fn len_frames(&self) -> usize {
        self.audio_gate.buffered_frames()
    }
}

/// Persistent real-time `rodio::Source` connected directly to the CoreAudio `OutputStream` mixer.
///
/// Checks `InstantAudioGate::is_playing()` on every single stereo frame (`0.022 ms` at `44.1 kHz`),
/// ramping down to exact digital silence in `INSTANT_PAUSE_RAMP_FRAMES` (`32` frames = `0.725 ms`)
/// without draining or losing buffered audio frames while paused.
pub struct InstantGatedSource {
    queue: Arc<SharedAudioQueue>,
    audio_gate: InstantAudioGate,
    volume_scaler: AtomicVolumeScaler,
    sample_producer: AudioSampleProducer,
    local_frames: VecDeque<(f32, f32)>,
    pending_right: Option<f32>,
    ramp_step: u8,
    last_epoch: u64,
}

impl InstantGatedSource {
    pub fn new(
        queue: Arc<SharedAudioQueue>,
        audio_gate: InstantAudioGate,
        volume_scaler: AtomicVolumeScaler,
        sample_producer: AudioSampleProducer,
    ) -> Self {
        let last_epoch = audio_gate.flush_epoch();
        Self {
            queue,
            audio_gate,
            volume_scaler,
            sample_producer,
            local_frames: VecDeque::with_capacity(512),
            pending_right: None,
            ramp_step: 0,
            last_epoch,
        }
    }
}

impl rodio::Source for InstantGatedSource {
    #[inline]
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    #[inline]
    fn channels(&self) -> rodio::ChannelCount {
        SPOTIFY_CHANNELS
    }

    #[inline]
    fn sample_rate(&self) -> rodio::SampleRate {
        SPOTIFY_SAMPLE_RATE
    }

    #[inline]
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

impl Iterator for InstantGatedSource {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        // INV-A3: Always complete the Right channel of the current stereo frame before
        // evaluating any gate/epoch transition on the next stereo frame boundary.
        if let Some(right) = self.pending_right.take() {
            return Some(right);
        }

        let epoch = self.audio_gate.flush_epoch();
        if epoch != self.last_epoch {
            self.last_epoch = epoch;
            self.local_frames.clear();
            self.queue.clear_nonblocking(epoch);
            self.ramp_step = 0;
        }

        let is_playing = self.audio_gate.is_playing();

        // Fast path when paused after the 0.725ms micro-ramp: output exact 0.0 and freeze queue.
        if !is_playing && self.ramp_step == 0 {
            self.pending_right = Some(0.0);
            return Some(0.0);
        }

        if self.local_frames.is_empty() {
            self.queue
                .pop_batch_nonblocking(&mut self.local_frames, 256, epoch);
        }

        let Some(&(raw_l, raw_r)) = self.local_frames.front() else {
            self.ramp_step = 0;
            self.pending_right = Some(0.0);
            return Some(0.0);
        };

        let (out_l, out_r, next_ramp, consume) = step_gated_stereo_frame_verified(
            raw_l,
            raw_r,
            is_playing,
            self.ramp_step,
            INSTANT_PAUSE_RAMP_FRAMES,
            self.volume_scaler.get(),
        );
        self.ramp_step = next_ramp;

        if consume && self.local_frames.pop_front().is_some() {
            self.audio_gate.decrement_buffered_frames(1);
            self.sample_producer.push_interleaved(&[out_l, out_r], 2);
        }

        self.pending_right = Some(out_r);
        Some(out_l)
    }
}

fn open_instant_output_stream(
    device_name: Option<&str>,
    format: AudioFormat,
    queue: Arc<SharedAudioQueue>,
    audio_gate: InstantAudioGate,
    volume_scaler: AtomicVolumeScaler,
    sample_producer: AudioSampleProducer,
) -> Result<rodio::OutputStream, String> {
    let host = cpal::default_host();
    let cpal_device = match device_name {
        Some(target_name) => host
            .output_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.name().ok().is_some_and(|n| n == target_name))
            .or_else(|| host.default_output_device())
            .ok_or_else(|| format!("Audio device '{target_name}' not available"))?,
        None => host
            .default_output_device()
            .ok_or_else(|| "No default audio output device available".to_string())?,
    };

    let actual_name = cpal_device
        .name()
        .unwrap_or_else(|_| "[unknown device]".to_string());
    let default_config = cpal_device
        .default_output_config()
        .map_err(|e| e.to_string())?;
    let supported = cpal_device
        .supported_output_configs()
        .ok()
        .and_then(|mut cfgs| {
            cfgs.find(|c| c.channels() == SPOTIFY_CHANNELS)
                .and_then(|c| {
                    c.try_with_sample_rate(cpal::SampleRate(SPOTIFY_SAMPLE_RATE))
                        .or_else(|| c.try_with_sample_rate(default_config.sample_rate()))
                })
        })
        .unwrap_or(default_config);

    let sample_format = match format {
        AudioFormat::F64 => cpal::SampleFormat::F64,
        AudioFormat::F32 => cpal::SampleFormat::F32,
        AudioFormat::S32 => cpal::SampleFormat::I32,
        AudioFormat::S24 | AudioFormat::S24_3 => cpal::SampleFormat::I24,
        AudioFormat::S16 => cpal::SampleFormat::I16,
    };

    let mut low_latency_stream_cfg = supported.config();
    low_latency_stream_cfg.buffer_size = cpal::BufferSize::Fixed(LOW_LATENCY_BUFFER_FRAMES);

    let mut stream = match rodio::OutputStreamBuilder::default()
        .with_device(cpal_device.clone())
        .with_config(&low_latency_stream_cfg)
        .with_sample_format(sample_format)
        .open_stream()
    {
        Ok(s) => {
            info!(
                "[InstantSink] Opened low-latency CoreAudio stream on '{}' ({} Hz, {} frames = {:.1}ms buffer)",
                actual_name,
                low_latency_stream_cfg.sample_rate.0,
                LOW_LATENCY_BUFFER_FRAMES,
                (LOW_LATENCY_BUFFER_FRAMES as f32 * 1000.0)
                    / (low_latency_stream_cfg.sample_rate.0.max(1) as f32)
            );
            s
        }
        Err(_) => match rodio::OutputStreamBuilder::default()
            .with_device(cpal_device.clone())
            .with_config(&supported.config())
            .with_sample_format(sample_format)
            .open_stream()
        {
            Ok(s) => {
                info!(
                    "[InstantSink] Opened CoreAudio stream on '{}' ({} Hz, default buffer)",
                    actual_name,
                    supported.sample_rate().0
                );
                s
            }
            Err(e) => {
                warn!(
                    "[InstantSink] Primary config failed on '{}': {e}; falling back to device default",
                    actual_name
                );
                rodio::OutputStreamBuilder::from_device(cpal_device)
                    .map_err(|err| err.to_string())?
                    .open_stream_or_fallback()
                    .map_err(|err| err.to_string())?
            }
        },
    };

    stream.log_on_drop(false);
    stream.mixer().add(InstantGatedSource::new(
        queue,
        audio_gate,
        volume_scaler,
        sample_producer,
    ));
    Ok(stream)
}

/// Real-time audio sink with sub-millisecond hardware pause gating (`InstantAudioGate`)
/// and lock-free PCM ring-buffer tapping for the 60/120fps GPU spectrum visualizer.
///
/// Unlike `librespot-playback`'s stock `RodioSink` (which calls `rodio_sink.sleep_until_end()`
/// inside `Sink::stop()` and plays 26 buffered chunks = ~500ms of audio after pressing pause),
/// `TeeSink` gates audio directly at the CoreAudio sample callback level in `0.725 ms` (`32` frames)
/// with zero sample loss.
pub struct TeeSink {
    stream: Option<rodio::OutputStream>,
    inner: Option<Box<dyn Sink>>,
    shared_queue: Arc<SharedAudioQueue>,
    audio_gate: InstantAudioGate,
    sample_producer: AudioSampleProducer,
    volume_scaler: AtomicVolumeScaler,
    scratch_f32: Vec<f32>,
    scratch_frames: Vec<(f32, f32)>,
}

impl TeeSink {
    /// Creates a new `TeeSink` backed by a low-latency `InstantGatedSource` CoreAudio stream.
    pub fn new(
        device_name: Option<String>,
        format: AudioFormat,
        sample_producer: AudioSampleProducer,
        volume_scaler: AtomicVolumeScaler,
        audio_gate: InstantAudioGate,
    ) -> Self {
        let verified_device =
            groove_audio::resolve_cpal_output_device_name(device_name.as_deref());
        info!(
            "Opening Instant TeeSink with format={format:?}, requested_device={device_name:?}, verified_device={verified_device:?}"
        );
        let shared_queue = SharedAudioQueue::new(audio_gate.clone());
        let stream = match open_instant_output_stream(
            verified_device.as_deref(),
            format,
            Arc::clone(&shared_queue),
            audio_gate.clone(),
            volume_scaler.clone(),
            sample_producer.clone(),
        ) {
            Ok(s) => Some(s),
            Err(err) => {
                warn!("Could not open Instant CoreAudio stream ({err}); TeeSink will tap visualizer samples only");
                None
            }
        };

        Self {
            stream,
            inner: None,
            shared_queue,
            audio_gate,
            sample_producer,
            volume_scaler,
            scratch_f32: Vec::with_capacity(INITIAL_SCRATCH_CAPACITY),
            scratch_frames: Vec::with_capacity(INITIAL_SCRATCH_CAPACITY / 2),
        }
    }

    /// Creates a `TeeSink` around an explicit inner `Sink` implementation (useful for unit testing).
    pub fn with_inner(
        inner: Option<Box<dyn Sink>>,
        sample_producer: AudioSampleProducer,
        volume_scaler: AtomicVolumeScaler,
    ) -> Self {
        let audio_gate = InstantAudioGate::new(true);
        let shared_queue = SharedAudioQueue::new(audio_gate.clone());
        Self {
            stream: None,
            inner,
            shared_queue,
            audio_gate,
            sample_producer,
            volume_scaler,
            scratch_f32: Vec::with_capacity(INITIAL_SCRATCH_CAPACITY),
            scratch_frames: Vec::with_capacity(INITIAL_SCRATCH_CAPACITY / 2),
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
        // Immediately gate hardware audio output without calling `sleep_until_end()`.
        self.audio_gate.set_playing(false);
        if let Some(inner) = self.inner.as_mut() {
            inner.stop()?;
        }
        Ok(())
    }

    fn write(&mut self, mut packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        if self.stream.is_some() {
            self.scratch_frames.clear();
            match &packet {
                AudioPacket::Samples(samples) => {
                    for chunk in samples.chunks(2) {
                        let l = sanitize_pcm_sample(chunk[0] as f32);
                        let r = sanitize_pcm_sample(*chunk.get(1).unwrap_or(&chunk[0]) as f32);
                        self.scratch_frames.push((l, r));
                    }
                }
                AudioPacket::Raw(raw_bytes) => {
                    let i16_samples: Vec<i16> = raw_bytes
                        .chunks_exact(2)
                        .map(|b| i16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    let norm = 1.0 / 32768.0;
                    for chunk in i16_samples.chunks(2) {
                        let l = sanitize_pcm_sample(chunk[0] as f32 * norm);
                        let r = sanitize_pcm_sample(
                            *chunk.get(1).unwrap_or(&chunk[0]) as f32 * norm,
                        );
                        self.scratch_frames.push((l, r));
                    }
                }
            }

            if !self.scratch_frames.is_empty() {
                let epoch = self.audio_gate.flush_epoch();
                self.shared_queue.push_frames(&self.scratch_frames, epoch);

                // Tight ~92ms backpressure loop that wakes immediately (< 2ms) when paused or flushed,
                // so librespot's player thread never blocks when processing Pause/Seek/Load commands.
                while self.shared_queue.len_frames() > INSTANT_SINK_MAX_BUFFERED_FRAMES
                    && self.audio_gate.is_playing()
                    && self.audio_gate.flush_epoch() == epoch
                {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }

            return Ok(());
        }

        // Fallback / unit-test path when constructed via `TeeSink::with_inner`.
        let gain = self.volume_scaler.get();

        match &mut packet {
            AudioPacket::Samples(samples) => {
                if !samples.is_empty() {
                    self.scratch_f32.clear();
                    self.scratch_f32
                        .extend(samples.iter().map(|&sample| (sample as f32) * gain));
                    self.sample_producer.push_interleaved(&self.scratch_f32, 2);

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
            inner
                .write(packet, converter)
                .map_err(|e| SinkError::OnWrite(e.to_string()))?;
        }

        Ok(())
    }
}
