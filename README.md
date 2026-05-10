# Clippy

**A focused video clip editor for Windows.** Open a recording, mark the moments you want, and export as MP4, MP3, or GIF in seconds.

For editing a clip to send to buddies on discord, the options I had for video editing tools felt like cutting a steak with either a plastic knife or a chainsaw. The tools were either too complicated, or too simple. Clippy is the sweet middle ground.

![Clippy workflow — open a recording, mark regions, export a clip](docs/HERO.gif)

---

## Features

### Open anything, instantly

Drop a video file onto the window, drag it onto the `.exe`, or use **Ctrl+O**. MP4/H.264/AAC files play immediately — no re-encode on load. MKV and other containers remux in the background while you work, so you can start trimming before the background job finishes.

![App with a video loaded — player, waveform timeline, and region chips](docs/OpenAnythingScreenshot.png)

---

### Region-based editing

Press **F** to mark an in-point at the playhead, **Shift+F** to mark the out-point. Regions appear as color-coded bands on the timeline and as chips above it. Each is independent, in editing and exporting. 

![Marking in/out points with F and Shift+F, region chips appearing on the timeline](docs/RegionBasedEditing.gif)

- Drag the edges of a region band on the timeline to fine-tune in/out after the fact
- Jump to any region instantly with number keys (**1–9**)
- Delete a region by clicking the **×** on its chip
- Each region has independent speed, audio, and crop controls when exported as individual clips
- Make each regions edits match for a singular stitched clip

---

### Per-region crop

Press **Shift+C** (or click **Crop** on a region chip) to open the crop overlay. Draw a rectangle over the video, lock to a preset aspect ratio, and confirm. The crop is baked in at export — the rest of the frame is discarded.

![Opening the crop overlay, drawing a selection with aspect lock, confirming](docs/PerRegionCrop.gif)

---

### Per-region speed

Click the speed badge on a region chip (shown as **1×** by default) to change playback rate: **0.25×, 0.5×, 1×, 2×, 4×**. Audio is pitch-corrected automatically at export using the `atempo` filter.

![Region chip with speed picker popover open](docs/PerRegionSpeed.png)

---

### Multi-track audio mixer

If your source has multiple audio tracks (SteelSeries Sonar, OBS multi-track, or any multi-audio container), the mixer panel appears below the video. Each track gets a color-coded row with a volume slider and a mute button.

![Multi-track audio mixer with color-coded rows, mute buttons, and volume sliders](docs/Multi-TrackAudio.jpg)

- **Click a track's colored dot** to recolor it — the same color appears on the row stripe, the slider thumb, and that track's waveform layer on the timeline
- **Click a track's label** to rename it inline (turn "Track 2" into "Discord")
- **The mixer follows the playhead:** inside a region, you're editing that region's audio mix; outside, you're editing the source default — you hear the mix change live as the playhead crosses a region boundary

---

### Export to MP4, MP3, or GIF 

Open the export panel with **Ctrl+E** or the **Export** button in the top right.

![Export dialog showing format tabs, size-limit picker, and normalize option](docs/ExportDialog.png)

**MP4** — Hardware-accelerated encode using the best encoder your GPU supports (NVENC → AMF → QSV → libx264 software fallback). Export regions as separate clips or stitched into one file. Optional size target (10 MB, 50 MB, 500 MB, or none) for Discord's upload limits.

**MP3** — Audio-only, 192 kbps. All regions are stitched in timeline order.

**GIF** — Silent looping animation at 15 fps. Resolution presets: Small (480 px wide), Medium (960 px), Large (1280 px), or source width.

**X Clips or Stitched** — Export as many files as you selected, or stitch them together into one 

**Normalize loudness** — One checkbox to bring quiet game audio up to a Discord-friendly level (~−16 LUFS target) without touching the per-track mixer.

---

### Keyboard shortcuts — rebindable

Every action is bound to a key. Click any shortcut label in the footer to open the keybind editor and remap it to whatever you prefer.

![Keybind editor showing all actions and their current bindings](docs/KeyboardShortcuts.png)

---

### Tips modal

Hit the **?** button in the top bar to pull up a curated list of things that aren't obvious from just looking at the UI.

![Tips modal](docs/Tips.png)

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

**Prerequisites:** [Rust](https://rustup.rs/), [Node.js 20+](https://nodejs.org/), [pnpm](https://pnpm.io/), and FFmpeg/FFprobe.

**FFmpeg sidecars** are not included in this repo (LGPL). Download a Windows x64 build from [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) (the `ffmpeg-release-essentials` zip is sufficient), then copy and rename the two executables into `clippy/src-tauri/binaries/`:

```
ffmpeg.exe  →  clippy/src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe
ffprobe.exe →  clippy/src-tauri/binaries/ffprobe-x86_64-pc-windows-msvc.exe
```

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
