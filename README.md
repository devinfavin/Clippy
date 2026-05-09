# Clippy

A focused desktop video clip editor for Discord-style sharing. Built for the
specific workflow of "take an OBS recording, pull 3-5 highlight clips out of it,
send to Discord."

## What it does

- Loads MP4 / MKV / MOV / WebM / M4V / AVI sources. MP4 with H.264/AAC plays
  directly (0 s load); MKV is remuxed in seconds; rare codec mismatches fall
  back to a re-encode.
- Smooth scrubbing on long files via a localhost HTTP media server with proper
  byte-range support (Tauri's `asset://` chokes on multi-GB media).
- Waveform on the timeline, multi-region selection with draggable edges, loop
  playback per region, customizable keybinds, persisted across launches.
- Export options: per-region separate clips, stitched into one, or
  size-targeted (10/50/500 MB) with re-encoding via the best available
  hardware encoder (NVENC → AMF → QSV → libx264 cascade).
- Source preparation strategy is decided up front: direct play / remux /
  encode. The strategy badge in the top bar tells you which one applied.

## Build

Prerequisites:

- Rust stable (MSVC toolchain on Windows)
- Node 22+ and pnpm 11+
- Visual Studio 2022 with the C++ build tools (for MSVC linker)

```powershell
# install JS deps
pnpm install

# dev (live reload, debug build)
pnpm tauri dev

# production build (NSIS + MSI installer + portable exe)
pnpm tauri build
```

Output of `pnpm tauri build` lands in:

```
clippy\src-tauri\target\release\
  clippy.exe                                  # portable
  bundle\
    nsis\Clippy_<version>_x64-setup.exe       # NSIS installer
    msi\Clippy_<version>_x64_en-US.msi        # MSI installer
```

## Where things live at runtime

- **Proxy / waveform cache**: `%APPDATA%\com.devin.clippy\proxies\`
  (one `.remux.mp4` per remuxed source, one `.proxy.mp4` per encoded source,
  one `.wave.f32` per source for the waveform). Safe to delete; will be
  regenerated on next open.
- **Keybinds**: `localStorage` inside the app (key `clippy.keybinds.v1`).

## Project layout

```
src/                     # React + TypeScript frontend
  App.tsx                # main component + keyboard dispatch
  ExportModal.tsx        # export dialog (mode, size, estimate)
  ExportToast.tsx        # post-export bottom-right toast
  keybinds.ts            # binding types, defaults, format/match/capture
  formatters.ts          # fmtTime / fmtEta / fmtMb / size estimates
  types.ts               # shared type defs + size presets
  App.css                # tokenized design system

src-tauri/               # Rust backend
  src/lib.rs             # all Tauri commands + the localhost media server
  binaries/              # bundled ffmpeg + ffprobe sidecars (Windows)
  capabilities/          # Tauri permission scopes
  tauri.conf.json        # window/bundle config

tools/make_icon.py       # regenerate the source app icon
```

## Notable design decisions

- **Single-source clip workflow.** No multi-track timeline, no transitions, no
  effects. Everything is built around "select regions out of one source,
  export." The export modal is the only place re-encoding happens.
- **Stream-copy by default.** No-limit exports never re-encode; size-limited
  exports cascade through the available hardware encoders before falling back
  to libx264.
- **Boundary precision.** Cuts snap to source keyframes (~1 s with OBS
  keyframe interval = 1). Frame-accurate cuts would require a re-encode pass
  on the boundaries; not implemented.
- **HTTP server for media playback.** A random-port localhost server with a
  per-session token serves only files registered via `register_file_url`.
  Drops Tauri's `asset://` for media because Chromium plays HTTP files much
  more reliably for large videos.

## Out of scope

Things deliberately left out (some considered, then rejected):

- Multi-track timeline, transitions, effects, color grading, titles
- Subtitle editing
- Per-region audio volume adjustment
- Cloud sync, accounts, project files
- Plugin system
- Auto-updater
- Code signing (unsigned binary triggers Windows SmartScreen the first time
  — click "More info → Run anyway")
