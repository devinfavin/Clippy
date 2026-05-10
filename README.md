# Clippy

**A focused video clip editor for Windows.** Open a recording, mark the moments you want, and export — as MP4, MP3, or GIF — in seconds.

Built for the OBS → Discord workflow: drag a recording onto the window, set your regions, and ship.

<!-- HERO: Insert a short screen recording or GIF showing the full open → trim → export flow here -->
<!-- Suggested: ~15s recording at 1280×720, showing file open → timeline scrub → F/Shift+F region marks → Ctrl+E export → status strip "Exported" -->

---

## Features

### Open anything, instantly

Drop a video file onto the window, drag it onto the `.exe`, or use **Ctrl+O**. MP4/H.264/AAC files play immediately — no re-encode on load. MKV and other containers remux in the background while you work, so you can start trimming before the background job finishes.

<!-- SCREENSHOT: The app with a video loaded, showing the player, timeline waveform, and region chips -->

---

### Region-based editing

Press **F** to mark an in-point at the playhead, **Shift+F** to mark the out-point. Regions appear as color-coded bands on the timeline and as chips above it. Build as many as you need — each is independent.

<!-- GIF: Pressing F and Shift+F to mark two regions, chips appearing, dragging a region edge to nudge the boundary -->

- Drag the edges of a region band on the timeline to fine-tune in/out after the fact
- Jump to any region instantly with number keys (**1–9**)
- Delete a region by clicking the **×** on its chip

---

### Per-region crop

Press **Shift+C** (or click **Crop** on a region chip) to open the crop overlay. Draw a rectangle over the video, lock to a preset aspect ratio, and confirm. The crop is baked in at export — the rest of the frame is discarded.

<!-- GIF: Opening the crop overlay on a region, drawing a selection with an aspect lock active, confirming and seeing the crop badge appear on the chip -->

| Aspect lock | Use case |
|-------------|----------|
| 16:9 | Standard widescreen |
| 9:16 | TikTok / Reels / Shorts |
| 1:1 | Square posts |
| 4:3 | Retro / webcam |
| Free | Any shape |
| Source | Match the source video |

---

### Per-region speed

Click the speed badge on a region chip (shown as **1×** by default) to change playback rate: **0.25×, 0.5×, 1×, 2×, 4×**. Audio is pitch-corrected automatically at export using the `atempo` filter.

<!-- SCREENSHOT: A region chip with the speed picker popover open, showing the five presets -->

---

### Multi-track audio mixer

If your source has multiple audio tracks (SteelSeries Sonar, OBS multi-track, or any multi-audio container), the mixer panel appears below the video. Each track gets a color-coded row with a volume slider and a mute button.

<!-- SCREENSHOT: The track mixer with 3–4 colored rows (Game, Mic, Discord), one muted, sliders at different levels -->

- **Click a track's colored dot** to recolor it — the same color appears on the row stripe, the slider thumb, and that track's waveform layer on the timeline
- **Click a track's label** to rename it inline (turn "Track 2" into "Discord")
- **The mixer follows the playhead:** inside a region, you're editing that region's audio mix; outside, you're editing the source default — you hear the mix change live as the playhead crosses a region boundary

---

### Waveform + keyframe visualization

The timeline shows a layered waveform for every audio track in its palette color. Faint vertical ticks mark every video keyframe — useful for knowing exactly where a stream-copy cut will land.

<!-- SCREENSHOT: Timeline close-up showing overlapping colored waveform layers and thin white keyframe tick marks -->

---

### Export to MP4, MP3, or GIF

Open the export panel with **Ctrl+E**.

<!-- SCREENSHOT: The export dialog with format tabs (MP4 / MP3 / GIF), size-limit picker, and normalize checkbox -->

**MP4** — Hardware-accelerated encode using the best encoder your GPU supports (NVENC → AMF → QSV → libx264 software fallback). Export regions as separate clips or stitched into one file. Optional size target (10 MB, 50 MB, 500 MB, or none) for Discord's upload limits.

**MP3** — Audio-only, 192 kbps. All regions are stitched in timeline order.

**GIF** — Silent looping animation at 15 fps. Resolution presets: Small (480 px wide), Medium (960 px), Large (1280 px), or source width.

**Normalize loudness** — One checkbox to bring quiet game audio up to a Discord-friendly level (~−16 LUFS target) without touching the per-track mixer.

After export finishes, the status strip shows **Open folder** and **Copy path** buttons so you can get the file out immediately.

<!-- SCREENSHOT: The status strip in "export done" state showing filename, file size, and the two action buttons -->

---

### Frame capture

Press **Shift+S** to copy the current frame to the clipboard. Press again within 5 seconds to write it as a PNG file to disk instead.

<!-- SCREENSHOT: Status strip showing the "Frame copied to clipboard — click Save Frame again to write a PNG" confirmation -->

---

### Project persistence

Regions, crops, speeds, audio mix, track colors, and track renames all auto-save per source file. Reopen the same recording later and everything is exactly where you left it.

---

### Keyboard shortcuts — rebindable

Every action is bound to a key. Click any shortcut label in the footer to open the keybind editor and remap it to whatever you prefer.

<!-- SCREENSHOT: The keybind editor overlay showing all actions with their current bindings -->

Default bindings:

| Action | Default key |
|--------|-------------|
| Play / pause | Space |
| Frame back | ← |
| Frame forward | → |
| Jump to start | Home |
| Jump to end | End |
| Set in-point | F |
| Set out-point | Shift+F |
| Open crop overlay | Shift+C |
| Copy frame / save PNG | Shift+S |
| Export | Ctrl+E |
| Open file | Ctrl+O |
| Jump to region 1–9 | 1–9 |

---

### Tips modal

Hit the **?** button in the top bar to pull up a curated list of things that aren't obvious from just looking at the UI.

<!-- SCREENSHOT: The tips modal open, showing a few tip entries -->

---

## Installation

**Recommended:** Download `Clippy_0.1.0_x64-setup.exe` from the [Releases](../../releases) page and run the installer. It bundles the FFmpeg sidecars and handles WebView2 if needed.

**Portable:** Download `clippy.exe` from Releases, put it anywhere on your machine, and run it. FFmpeg/FFprobe must be on `PATH` or in the same directory as the exe.

### System requirements

| | Minimum |
|--|---------|
| OS | Windows 10 x64 |
| Runtime | WebView2 (pre-installed on Windows 11; auto-installed on Windows 10 by the NSIS installer) |
| GPU | Any (for playback). NVIDIA / AMD / Intel GPU recommended for hardware-accelerated export. |

---

## Usage

1. **Open a video** — drag it onto the window, drag it onto `clippy.exe`, or press **Ctrl+O**
2. **Scrub** — drag the timeline or use ← / → to find your moments
3. **Set regions** — press **F** at the start of a clip, **Shift+F** at the end; repeat for every clip you want
4. **Adjust per-region** — crop (**Shift+C**), speed (click the badge on the chip), audio mix (the mixer tracks your playhead automatically)
5. **Export** — **Ctrl+E**, pick format + size limit, go

---

## Technical notes

- **No cloud. No telemetry.** Everything runs locally. FFmpeg/FFprobe are bundled as sidecars and never touch the network.
- **Proxy cache** lives at `%APPDATA%\com.devin.clippy\proxies\`. Files older than 30 days are pruned automatically. You can clear it manually via the link in the app footer.
- **Export is two-stage:** each region is stream-copied (or re-encoded only when a crop or non-unity speed is applied) into a clean intermediate file, then the intermediates are concatenated. This keeps export fast and lossless when possible.
- **Audio preview uses the WebAudio API.** Waveform rendering and live mix preview run inside the WebView — no extra process.
- **Hardware encoder waterfall:** the backend tries NVENC, then AMF, then QSV, then falls back to libx264. The first one that initializes wins.

---

## Building from source

**Prerequisites:** [Rust](https://rustup.rs/), [Node.js 20+](https://nodejs.org/), [pnpm](https://pnpm.io/). FFmpeg and FFprobe binaries need to be placed in `clippy/src-tauri/binaries/` using Tauri's sidecar naming convention (`ffmpeg-x86_64-pc-windows-msvc.exe` / `ffprobe-x86_64-pc-windows-msvc.exe`).

```sh
git clone https://github.com/yourname/clippy
cd clippy/clippy
pnpm install
pnpm tauri dev        # dev mode with hot reload
pnpm tauri build      # production → target/release/bundle/nsis/
```

---

## License

MIT — see [LICENSE](LICENSE).
