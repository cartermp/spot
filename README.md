# Groove

*A natural home for your music.*

**Groove** is a native, low-resource Spotify Premium desktop player written in 100% Rust using [`librespot` 0.8](https://github.com/librespot-org/librespot) for 320 kbps Ogg Vorbis streaming and [`iced` 0.13](https://github.com/iced-rs/iced) (`wgpu` Metal backend) for a hardware-accelerated 60–120 FPS interface.

## Architecture

- **`crates/groove-core`**: Shared domain types (`Track`, `Playlist`, `PlaybackState`, `AudioDevice`, `SpectrumFrame`), command/event channels (`PlayerCommand`, `EngineEvent`), and default mockup state.
- **`crates/groove-audio`**: Lock-free `rtrb` SPSC PCM sample ring buffer, 2048-point Hann-windowed `realfft` spectrum analyzer (32 logarithmic bands with exponential attack/release smoothing), and `cpal` CoreAudio output device enumeration.
- **`crates/groove-spotify`**: Async `tokio` engine integrating `librespot-core`, `librespot-oauth` (PKCE login + persistent credential & 1 GB encrypted audio chunk caching in `~/Library/Application Support/Groove`), custom `TeeSink` wrapping `librespot-playback` (`Bitrate::Bitrate320`), and a `reqwest` Spotify Web API client.
- **`crates/groove-app`**: Native macOS GUI featuring a transparent full-size content titlebar with inset traffic lights, bundled `Instrument Serif` editorial typography, procedural high-DPI artwork rendering, and a multi-layered 3D contour ribbon wave visualizer.

## Running Groove

```bash
cargo run --release -p groove-app
```

## Connecting Spotify Premium

1. Click the **Spotify streaming** status card in the bottom-left of the sidebar (or set `SPOTIFY_CLIENT_ID=<your_client_id>` from the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard) with redirect URI `http://127.0.0.1:8898/login`).
2. Complete the one-time OAuth 2.0 PKCE login in your browser.
3. Groove caches your reusable `librespot` credentials in `~/Library/Application Support/Groove/credentials` so subsequent launches connect automatically.
