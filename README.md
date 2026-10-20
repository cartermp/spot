# Spot

Personal macOS Spotify Premium player written in Rust (`iced` + `librespot` 320 kbps Ogg Vorbis + CoreAudio).

## Quick Start

```bash
# Build and install Spot.app to /Applications (plus ~/.local/bin/spot)
make install

# Or build dist/Spot.app without installing
make package

#Or run directly from source
make run
```

## Updates (Every Push to `main`)

Every push to `main` runs `.github/workflows/release.yml`, which executes the 19-proof formal verification suite, packages `Spot.app` into `Spot-macos.zip` + `spot-update-manifest.json` (with SHA-256 digest and build number), and publishes a downloadable GitHub Release.

Downloaded updates are verified by SHA-256 and stored locally in `~/Library/Application Support/Spot/updates/archives/` (retaining the 10 most recent `main` builds for instant rollback):

```bash
# Check if a newer `main` push is available
spot --check-update     # or: make check-update

# Download, verify SHA-256, store locally, and install to /Applications/Spot.app
spot --update           # or: make update

# Download & store latest `main` build without installing yet
spot --download-update

# List locally stored update archives or roll back to a previous build
spot --list-updates     # or: make list-updates
spot --rollback <sha_or_build_number>
```

You can also check, download, and install `main` updates directly from the **Spot #\<build\>** badge in the bottom-left sidebar.

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
