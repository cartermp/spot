# Spot

Personal macOS Spotify Premium player written in Rust (`iced` + `librespot` 320 kbps Ogg Vorbis + CoreAudio).

## Quick Start

```bash
# Build and install Spot.app to /Applications (plus ~/.local/bin/spot)
make install

# Or build dist/Spot.app without installing
make package

# Or run directly from source
make run
```

## Shortcuts

| Key | Action |
| --- | --- |
| `Space` | Instant play / pause (when search isn't focused) |
| `Cmd + Backspace` | Delete previous word in the search bar |

## Where Data Lives

Cached credentials, encrypted audio chunks, recent searches, and recently-listened history are stored in:

```text
~/Library/Application Support/Spot/
```

## Crates

- `crates/spot-app` — `iced` 0.13 (`wgpu` Metal) UI, macOS menu bar tray, and procedural dog icon (`spot` binary)
- `crates/spot-spotify` — `librespot` 320 kbps streaming, OAuth PKCE login, `spclient` catalog/podcast client, and instant-gate `TeeSink`
- `crates/spot-audio` — Lock-free PCM ring buffer, 32-band `realfft` spectrum analyzer, and `cpal` CoreAudio device discovery
- `crates/spot-core` — Domain types, playback queue/discography state machine, and formally verified transitions
- `crates/spot-verify` — Exhaustive model-checking (`stateright`) & property verification suite (`spot-verify` binary)

## Verification & Tests

```bash
make verify   # Run all 18 formal proofs & model checks
make test     # Run unit/property tests + formal suite
```
