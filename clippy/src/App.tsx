import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  cropsEqual,
  GIF_DEFAULT_RESOLUTION,
  newRegionId,
  SIZE_PRESETS,
  resolveRegionColor,
  resolveTrackColor,
  REGION_COLORS,
  trackMixIsDefault,
  trackMixToBackend,
  type Crop,
  type ExportFormat,
  type ExportMode,
  type ExportProgress,
  type GifResolution,
  type Phase,
  type ProjectState,
  type ProxyProgress,
  type ProxyResult,
  type Region,
  type SizeLimit,
  type TrackColorOverrides,
  type TrackMix,
  type TrackNameOverrides,
  type VideoInfo,
} from "./types";
import { fmtTime } from "./formatters";
import {
  ACTION_LABELS,
  captureKeybind,
  DEFAULT_KEYBINDS,
  formatKeybind,
  loadKeybinds,
  matchesBinding,
  saveKeybinds,
  type ActionId,
  type Keybind,
  type Keybinds,
} from "./keybinds";
import { ExportModal } from "./ExportModal";
import { CropOverlay } from "./CropOverlay";
import { CropIndicator } from "./CropIndicator";
import { SpeedPicker } from "./SpeedPicker";
import { TrackMixer } from "./TrackMixer";
import { useAudioTracks } from "./useAudioTracks";
import { StatusStrip, type StatusContent } from "./StatusStrip";
import { ColorPicker } from "./ColorPicker";
import { TipsModal } from "./TipsModal";
import { OnboardingHint } from "./OnboardingHint";
import "./App.css";

/** "#rrggbb" + alpha → "rgba(r, g, b, a)". Used for waveform fill colors so we
 *  can blend the per-track palette colors with translucency. */
function hexWithAlpha(hex: string, alpha: number): string {
  const m = hex.match(/^#([0-9a-f]{6})$/i);
  if (!m) return hex;
  const r = parseInt(m[1].slice(0, 2), 16);
  const g = parseInt(m[1].slice(2, 4), 16);
  const b = parseInt(m[1].slice(4, 6), 16);
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

export default function App() {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const timelineRef = useRef<HTMLDivElement | null>(null);

  const [srcPath, setSrcPath] = useState<string | null>(null);
  const [proxyPath, setProxyPath] = useState<string | null>(null);
  const [info, setInfo] = useState<VideoInfo | null>(null);
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  const [currentTime, setCurrentTime] = useState(0);
  const [isPlaying, setIsPlaying] = useState(false);
  // Committed regions (each has both in and out), and a draft pair for the
  // region currently being built. As soon as draft has both endpoints with
  // in < out, it auto-commits and the draft clears.
  const [regions, setRegions] = useState<Region[]>([]);
  const [draftIn, setDraftIn] = useState<number | null>(null);
  const [draftOut, setDraftOut] = useState<number | null>(null);
  const [isScrubbing, setIsScrubbing] = useState(false);
  const [proxyEncoder, setProxyEncoder] = useState<string | null>(null);

  // Keybinds
  const [keybinds, setKeybinds] = useState<Keybinds>(() => loadKeybinds());
  const [keybindsOpen, setKeybindsOpen] = useState(false);
  const [listeningAction, setListeningAction] = useState<ActionId | null>(null);
  useEffect(() => { saveKeybinds(keybinds); }, [keybinds]);

  // Tips modal — opened by the "?" button in the topbar.
  const [tipsOpen, setTipsOpen] = useState(false);

  // Export modal state
  const [exportOpen, setExportOpen] = useState(false);
  const [exportSize, setExportSize] = useState<SizeLimit>(SIZE_PRESETS[0]);
  const [exportMode, setExportMode] = useState<ExportMode>("separate");
  const [exportFormat, setExportFormat] = useState<ExportFormat>("mp4");
  const [exportNormalize, setExportNormalize] = useState(false);
  // Per-track mute + volume for multi-audio sources. Persisted with project.
  const [trackMix, setTrackMix] = useState<TrackMix>({});
  // Per-track color overrides (palette-slot per stream-index). User picks via
  // ColorPicker on the mixer's dot. Persisted with project.
  const [trackColors, setTrackColors] = useState<TrackColorOverrides>({});
  // Per-track user-renamed labels (click track name in the mixer to edit).
  const [trackNames, setTrackNames] = useState<TrackNameOverrides>({});
  const [exportGifResolution, setExportGifResolution] =
    useState<GifResolution>(GIF_DEFAULT_RESOLUTION);
  // Post-export toast: list of files just produced.
  const [lastExport, setLastExport] = useState<{ paths: string[] } | null>(null);

  // Waveforms: one Float32Array of peak amplitudes per audio track, keyed by
  // track index. Single-track sources end up with a single entry at key 0.
  const [waveforms, setWaveforms] = useState<Map<number, Float32Array>>(new Map());
  const waveformIdRef = useRef(0);
  const waveCanvasRef = useRef<HTMLCanvasElement | null>(null);

  // Keyframe timestamps (seconds) for the source video. Drawn as faint ticks
  // on the timeline so the user can see where stream-copy cuts will snap to.
  const [keyframes, setKeyframes] = useState<Float32Array | null>(null);
  const keyframeCanvasRef = useRef<HTMLCanvasElement | null>(null);

  // Convert the file path to an http://127.0.0.1:PORT/vid?... URL via the
  // in-process media server. Chromium plays HTTP files much more reliably than
  // asset:// for large videos.
  const [proxySrc, setProxySrc] = useState<string | null>(null);
  useEffect(() => {
    if (!proxyPath) {
      setProxySrc(null);
      return;
    }
    let cancelled = false;
    invoke<string>("register_file_url", { path: proxyPath })
      .then((url) => {
        if (!cancelled) setProxySrc(url);
      })
      .catch((err) => {
        if (!cancelled) {
          console.error("[clippy] register_file_url failed:", err);
          setPhase({ kind: "error", message: `Couldn't register file URL: ${err}` });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [proxyPath]);

  const duration = info?.duration_secs ?? 0;
  const fps = info?.fps && info.fps > 0 ? info.fps : 30;
  const frameSecs = 1 / fps;

  // Pending seek target: while a seek is in flight, store the latest target
  // and apply it on 'seeked' so we don't pile up seek requests.
  const pendingSeekRef = useRef<number | null>(null);
  // Whether to keep the video muted during a scrub drag.
  const wasMutedRef = useRef<boolean>(false);
  // Whether playback was active when a scrub drag started, so we can resume on release.
  const wasPlayingRef = useRef<boolean>(false);

  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    const onSeeked = () => {
      if (pendingSeekRef.current != null) {
        const target = pendingSeekRef.current;
        pendingSeekRef.current = null;
        try { v.currentTime = target; } catch {}
      }
    };
    v.addEventListener("seeked", onSeeked);
    return () => {
      v.removeEventListener("seeked", onSeeked);
    };
  }, [proxyPath]);

  // ---- file open + proxy pipeline ----
  const loadFile = useCallback(async (selected: string) => {
    try {
      // Invalidate any in-flight waveform extraction.
      waveformIdRef.current += 1;
      setWaveforms(new Map());
      setKeyframes(null);

      // Stop any active loop, since regions are about to clear.
      loopingRegionIdRef.current = null;
      setLoopingRegionId(null);

      setSrcPath(selected);
      setProxyPath(null);
      setInfo(null);
      setRegions([]);
      setDraftIn(null);
      setDraftOut(null);
      setCurrentTime(0);
      setTrackMix({});
      setTrackColors({});
      setTrackNames({});
      setPhase({ kind: "probing" });

      const probed = await invoke<VideoInfo>("probe_video", { path: selected });
      setInfo(probed);

      setPhase({ kind: "proxying", progress: 0, eta: null });
      const result = await invoke<ProxyResult>("generate_proxy", {
        path: selected,
        info: probed,
      });
      setProxyPath(result.play_path);
      setProxyEncoder(result.strategy);
      setPhase({ kind: "ready" });

      // Restore previously-saved regions for this source (regions, crops,
      // speeds). Sidecar JSON keyed by the same fingerprint as the cache.
      try {
        const saved = await invoke<ProjectState | null>("load_project", {
          srcPath: selected,
        });
        if (saved && Array.isArray(saved.regions)) {
          // Re-run sort so any out-of-order entries are tidied.
          const sorted = [...saved.regions].sort((a, b) => a.inSecs - b.inSecs);
          setRegions(sorted);
        }
        if (saved && saved.trackMix && typeof saved.trackMix === "object") {
          setTrackMix(saved.trackMix as TrackMix);
        }
        if (saved && saved.trackColors && typeof saved.trackColors === "object") {
          setTrackColors(saved.trackColors as TrackColorOverrides);
        }
        if (saved && saved.trackNames && typeof saved.trackNames === "object") {
          setTrackNames(saved.trackNames as TrackNameOverrides);
        }
      } catch (err) {
        console.warn("[clippy] load_project failed:", err);
      }

      // Kick off waveform extraction per audio track in parallel. Doesn't block
      // ready state; bins arrive over time and the canvas redraws as they land.
      const waveId = ++waveformIdRef.current;
      const tracksForWave = probed.audio_tracks.length > 0
        ? probed.audio_tracks.map((t) => t.index)
        : [0];
      for (const idx of tracksForWave) {
        invoke<number[]>("extract_waveform", {
          path: selected,
          info: probed,
          trackIndex: idx,
        })
          .then((bins) => {
            if (waveId !== waveformIdRef.current) return;
            setWaveforms((prev) => {
              const next = new Map(prev);
              next.set(idx, new Float32Array(bins));
              return next;
            });
          })
          .catch((err) => {
            if (waveId !== waveformIdRef.current) return;
            console.error(`[clippy] waveform extract (track ${idx}) failed:`, err);
          });
      }

      // Probe keyframes in parallel — non-blocking, draws ticks when ready.
      invoke<number[]>("probe_keyframes", { path: selected })
        .then((times) => {
          if (waveId !== waveformIdRef.current) return;
          setKeyframes(new Float32Array(times));
        })
        .catch((err) => {
          if (waveId !== waveformIdRef.current) return;
          console.warn("[clippy] probe_keyframes failed:", err);
        });
    } catch (e) {
      setPhase({ kind: "error", message: String(e) });
    }
  }, []);

  const handleOpen = useCallback(async () => {
    const selected = await openDialog({
      multiple: false,
      directory: false,
      filters: [
        { name: "Video", extensions: ["mkv", "mp4", "mov", "webm", "m4v", "avi"] },
      ],
    });
    if (!selected || typeof selected !== "string") return;
    await loadFile(selected);
  }, [loadFile]);

  // Suppress the default Chromium context menu globally — for a focused
  // desktop app the "Save video as / Inspect / Reload" entries are just noise.
  useEffect(() => {
    const onCtx = (e: MouseEvent) => e.preventDefault();
    window.addEventListener("contextmenu", onCtx);
    return () => window.removeEventListener("contextmenu", onCtx);
  }, []);

  // If Clippy was launched with a file argument (Windows "Open with" → Clippy,
  // or a video dropped on Clippy.exe), auto-load it on first mount. The backend
  // takes-and-clears its stored value so a hot-reload during dev won't re-open.
  useEffect(() => {
    invoke<string | null>("get_initial_path")
      .then((p) => {
        if (p) loadFile(p);
      })
      .catch((err) => console.error("[clippy] get_initial_path failed:", err));
    // loadFile is stable (useCallback with empty deps), but we intentionally
    // run this only once at mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Window-level drag-and-drop: drop a video file on the window to open it.
  const [isDraggingFile, setIsDraggingFile] = useState(false);
  useEffect(() => {
    const VIDEO_EXTS = ["mkv", "mp4", "mov", "webm", "m4v", "avi"];
    const isVideoPath = (p: string) =>
      VIDEO_EXTS.includes(p.split(".").pop()?.toLowerCase() ?? "");
    let unlisten: UnlistenFn | null = null;
    import("@tauri-apps/api/webview").then(({ getCurrentWebview }) => {
      getCurrentWebview()
        .onDragDropEvent((event) => {
          const p = event.payload as { type: string; paths?: string[] };
          if (p.type === "enter" || p.type === "over") {
            const hasVideo = (p.paths ?? []).some(isVideoPath);
            setIsDraggingFile(hasVideo);
          } else if (p.type === "drop") {
            setIsDraggingFile(false);
            const first = (p.paths ?? []).find(isVideoPath);
            if (first) loadFile(first);
          } else {
            setIsDraggingFile(false);
          }
        })
        .then((u) => (unlisten = u));
    });
    return () => {
      unlisten?.();
    };
  }, [loadFile]);

  // proxy progress events
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    listen<ProxyProgress>("proxy:progress", (event) => {
      setPhase((p) =>
        p.kind === "proxying"
          ? { kind: "proxying", progress: event.payload.progress, eta: event.payload.eta_secs }
          : p
      );
    }).then((u) => (unlisten = u));
    return () => {
      unlisten?.();
    };
  }, []);

  // export progress events
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    listen<ExportProgress>("export:progress", (event) => {
      setPhase((p) =>
        p.kind === "exporting"
          ? { ...p, progress: event.payload.progress }
          : p
      );
    }).then((u) => (unlisten = u));
    return () => {
      unlisten?.();
    };
  }, []);

  // ---- transport controls ----
  const playPause = useCallback(() => {
    const v = videoRef.current;
    if (!v) return;
    if (v.paused) v.play();
    else v.pause();
  }, []);

  // Debounced save of the project state (regions, crops, speeds, track mix,
  // track colors, track names) to a sidecar JSON next to the proxy cache.
  useEffect(() => {
    if (!srcPath) return;
    const handle = window.setTimeout(() => {
      const state: ProjectState = { version: 1, regions, trackMix, trackColors, trackNames };
      invoke("save_project", { srcPath, state }).catch((err) =>
        console.warn("[clippy] save_project failed:", err)
      );
    }, 600);
    return () => window.clearTimeout(handle);
  }, [srcPath, regions, trackMix, trackColors, trackNames]);

  // Loop playback state (declared up here so seek/scrubTo can reference it
  // for the auto-stop-when-outside check below). The actual loop logic is
  // further down with the rest of the region helpers.
  const [loopingRegionId, setLoopingRegionId] = useState<string | null>(null);
  const loopingRegionIdRef = useRef<string | null>(null);
  const regionsRef = useRef<Region[]>([]);
  useEffect(() => { regionsRef.current = regions; }, [regions]);

  // If the user moves the playhead outside the currently-looping region,
  // stop looping. The playback wrap (timeupdate handler) sets currentTime
  // directly and doesn't go through these helpers, so it stays in-bounds.
  const maybeStopLoopOutsideRegion = useCallback((t: number) => {
    const loopId = loopingRegionIdRef.current;
    if (!loopId) return;
    const r = regionsRef.current.find((x) => x.id === loopId);
    // Tiny tolerance so floating-point boundary lands don't kill the loop.
    if (!r || t < r.inSecs - 0.001 || t > r.outSecs + 0.001) {
      loopingRegionIdRef.current = null;
      setLoopingRegionId(null);
    }
  }, []);

  const seek = useCallback(
    (t: number) => {
      const v = videoRef.current;
      if (!v) return;
      const clamped = Math.max(0, Math.min(duration, t));
      v.currentTime = clamped;
      pendingSeekRef.current = null;
      setCurrentTime(clamped);
      maybeStopLoopOutsideRegion(clamped);
    },
    [duration, maybeStopLoopOutsideRegion]
  );

  // Scrub-friendly seek: updates the playhead immediately, and either dispatches
  // the seek now or queues it for the next 'seeked' event so we don't pile up.
  const scrubTo = useCallback(
    (t: number) => {
      const v = videoRef.current;
      if (!v) return;
      const clamped = Math.max(0, Math.min(duration, t));
      setCurrentTime(clamped);
      if (v.seeking) {
        pendingSeekRef.current = clamped;
      } else {
        pendingSeekRef.current = null;
        try { v.currentTime = clamped; } catch {}
      }
      maybeStopLoopOutsideRegion(clamped);
    },
    [duration, maybeStopLoopOutsideRegion]
  );

  const stepFrames = useCallback(
    (n: number) => {
      const v = videoRef.current;
      if (!v) return;
      v.pause();
      seek(v.currentTime + n * frameSecs);
    },
    [seek, frameSecs]
  );

  // New regions auto-inherit a crop from the existing regions, so once you've
  // cropped one you don't have to re-crop every subsequent region. Picks the
  // most recently-added region's crop (last in the array). Returns undefined
  // when no existing region has a crop.
  const inheritedCrop = useCallback((existing: Region[]): Crop | undefined => {
    for (let i = existing.length - 1; i >= 0; i--) {
      if (existing[i].crop) return existing[i].crop;
    }
    return undefined;
  }, []);

  // Same idea for per-region audio mix. Snapshot from the most recent region
  // that has one so the user's tweaks carry forward without having to re-set
  // the mix on each new clip.
  const inheritedMix = useCallback((existing: Region[]): TrackMix | undefined => {
    for (let i = existing.length - 1; i >= 0; i--) {
      const m = existing[i].mix;
      if (m) return { ...m };
    }
    return undefined;
  }, []);

  // Set draft in. If draft out is already set later than t, auto-commit a region.
  const setIn = useCallback(() => {
    const t = videoRef.current?.currentTime ?? 0;
    if (draftOut != null && t < draftOut) {
      setRegions((r) =>
        [
          ...r,
          {
            id: newRegionId(),
            inSecs: t,
            outSecs: draftOut,
            crop: inheritedCrop(r),
            mix: inheritedMix(r),
          },
        ].sort((a, b) => a.inSecs - b.inSecs)
      );
      setDraftIn(null);
      setDraftOut(null);
    } else {
      setDraftIn(t);
      // If the existing draft out is now <= the new in, drop it.
      if (draftOut != null && t >= draftOut) setDraftOut(null);
    }
  }, [draftOut, inheritedCrop, inheritedMix]);

  // Set draft out. If draft in is already set earlier than t, auto-commit a region.
  const setOut = useCallback(() => {
    const t = videoRef.current?.currentTime ?? 0;
    if (draftIn != null && t > draftIn) {
      setRegions((r) =>
        [
          ...r,
          {
            id: newRegionId(),
            inSecs: draftIn,
            outSecs: t,
            crop: inheritedCrop(r),
            mix: inheritedMix(r),
          },
        ].sort((a, b) => a.inSecs - b.inSecs)
      );
      setDraftIn(null);
      setDraftOut(null);
    } else {
      setDraftOut(t);
      if (draftIn != null && t <= draftIn) setDraftIn(null);
    }
  }, [draftIn, inheritedCrop, inheritedMix]);

  const deleteRegion = useCallback((id: string) => {
    setRegions((r) => r.filter((x) => x.id !== id));
  }, []);

  // Crop edit mode: which region's crop are we editing? null = not editing.
  const [cropEditingRegionId, setCropEditingRegionId] = useState<string | null>(null);
  const wasPlayingBeforeCropRef = useRef(false);
  const startCropEdit = useCallback((regionId: string) => {
    const r = regions.find((x) => x.id === regionId);
    if (!r) return;
    const v = videoRef.current;
    if (v) {
      wasPlayingBeforeCropRef.current = !v.paused;
      if (!v.paused) v.pause();
    }
    seek(r.inSecs);
    setCropEditingRegionId(regionId);
  }, [regions, seek]);
  const finishCropEdit = useCallback((newCrop: Crop | undefined) => {
    if (!cropEditingRegionId) return;
    setRegions((rs) =>
      rs.map((r) => (r.id === cropEditingRegionId ? { ...r, crop: newCrop } : r))
    );
    setCropEditingRegionId(null);
    if (wasPlayingBeforeCropRef.current) {
      videoRef.current?.play().catch(() => {});
    }
  }, [cropEditingRegionId]);
  const cancelCropEdit = useCallback(() => {
    setCropEditingRegionId(null);
    if (wasPlayingBeforeCropRef.current) {
      videoRef.current?.play().catch(() => {});
    }
  }, []);
  // Propagate the just-edited crop to every region. Closes the overlay.
  // Used to make stitched export trivially possible (all regions must share
  // the same crop, and matching them by hand is awkward).
  const applyCropToAllRegions = useCallback((newCrop: Crop) => {
    setRegions((rs) => rs.map((r) => ({ ...r, crop: newCrop })));
    setCropEditingRegionId(null);
    if (wasPlayingBeforeCropRef.current) {
      videoRef.current?.play().catch(() => {});
    }
  }, []);
  const editingRegion = cropEditingRegionId
    ? regions.find((r) => r.id === cropEditingRegionId)
    : null;

  // Active cropped region for the read-only indicator: first region whose crop
  // is set AND contains the playhead. Re-evaluates each render — cheap.
  const activeCroppedRegion = regions.find(
    (r) => r.crop && currentTime >= r.inSecs && currentTime <= r.outSecs
  );

  // Region containing the playhead (first match, sorted by inSecs already).
  // Drives the audio-mix context: while inside, the mixer edits that region's
  // mix and WebAudio plays it; outside, both fall back to the source default.
  const playheadRegion = regions.find(
    (r) => currentTime >= r.inSecs && currentTime <= r.outSecs
  );
  const playheadRegionIndex = playheadRegion ? regions.indexOf(playheadRegion) : -1;
  const effectiveMix: TrackMix = playheadRegion?.mix ?? trackMix;
  // Stable callback that writes back to the right slice of state — region
  // override if inside one, otherwise the source default.
  const setEffectiveMix = useCallback(
    (next: TrackMix) => {
      if (playheadRegion) {
        const id = playheadRegion.id;
        setRegions((rs) => rs.map((r) => (r.id === id ? { ...r, mix: next } : r)));
      } else {
        setTrackMix(next);
      }
    },
    [playheadRegion]
  );

  // Multi-track audio: extract each track on file load and feed them through
  // WebAudio so the mixer's sliders/mutes affect playback in real time. Hook
  // is a no-op for single-track sources. Effective mix changes as the playhead
  // crosses region boundaries — you literally hear the per-region mix kick in.
  useAudioTracks({
    videoElement: videoRef.current,
    srcPath,
    tracks: info?.audio_tracks ?? [],
    mix: effectiveMix,
  });

  // For the floating loop badge in the player corner.
  const loopingRegion = loopingRegionId
    ? regions.find((r) => r.id === loopingRegionId)
    : null;
  const loopingRegionIndex = loopingRegion ? regions.indexOf(loopingRegion) : -1;

  const clearAllRegions = useCallback(() => {
    setRegions([]);
    setDraftIn(null);
    setDraftOut(null);
  }, []);

  // Region edge dragging: track which region+edge is currently being resized.
  // Using state for the cursor styling, ref for the move handler closure.
  const [resizingEdge, setResizingEdge] = useState<{ regionId: string; edge: "in" | "out" } | null>(null);
  const resizingEdgeRef = useRef<{ regionId: string; edge: "in" | "out" } | null>(null);

  // If targetId is given, toggle loop on that specific region; otherwise pick
  // the region under the playhead (or nearest if none contain it).
  const toggleLoopRegion = useCallback((targetId?: string) => {
    // If we're already looping AND the user is asking to start the same region
    // (or any region with no specific target), stop looping.
    if (loopingRegionIdRef.current && (targetId === undefined || targetId === loopingRegionIdRef.current)) {
      loopingRegionIdRef.current = null;
      setLoopingRegionId(null);
      return;
    }
    let target: Region | undefined;
    if (targetId !== undefined) {
      target = regions.find((r) => r.id === targetId);
    } else {
      const t = videoRef.current?.currentTime ?? 0;
      target = regions.find((r) => r.inSecs <= t && t <= r.outSecs);
      if (!target) {
        target = [...regions].sort((a, b) =>
          Math.abs(a.inSecs - t) - Math.abs(b.inSecs - t)
        )[0];
      }
    }
    if (!target) return;
    loopingRegionIdRef.current = target.id;
    setLoopingRegionId(target.id);
    seek(target.inSecs);
    videoRef.current?.play().catch(() => {});
  }, [regions, seek]);

  // Stop looping if the looped region is deleted.
  useEffect(() => {
    if (loopingRegionId && !regions.some((r) => r.id === loopingRegionId)) {
      loopingRegionIdRef.current = null;
      setLoopingRegionId(null);
    }
  }, [regions, loopingRegionId]);

  const startEdgeResize = (
    e: React.PointerEvent<HTMLDivElement>,
    regionId: string,
    edge: "in" | "out"
  ) => {
    e.stopPropagation();
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    resizingEdgeRef.current = { regionId, edge };
    setResizingEdge({ regionId, edge });
  };

  // Coalesce rapid pointermove events into one update per animation frame so
  // we don't thrash setRegions/setCurrentTime when the mouse fires at 200+ Hz.
  const edgeRafRef = useRef<number | null>(null);
  const edgePendingTimeRef = useRef<number | null>(null);

  const flushEdgeResize = useCallback(() => {
    edgeRafRef.current = null;
    const ctx = resizingEdgeRef.current;
    const t = edgePendingTimeRef.current;
    edgePendingTimeRef.current = null;
    if (!ctx || t == null) return;
    setRegions((rs) =>
      rs
        .map((r) => {
          if (r.id !== ctx.regionId) return r;
          if (ctx.edge === "in") {
            const newIn = Math.max(0, Math.min(t, r.outSecs - 0.05));
            return { ...r, inSecs: newIn };
          } else {
            const newOut = Math.max(r.inSecs + 0.05, Math.min(t, duration));
            return { ...r, outSecs: newOut };
          }
        })
        .sort((a, b) => a.inSecs - b.inSecs)
    );
    scrubTo(t);
  }, [duration, scrubTo]);

  const onEdgeResizeMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!resizingEdgeRef.current) return;
    edgePendingTimeRef.current = timeFromPointer(e.clientX);
    if (edgeRafRef.current == null) {
      edgeRafRef.current = requestAnimationFrame(flushEdgeResize);
    }
  };

  const onEdgeResizeUp = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!resizingEdgeRef.current) return;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    // Apply any final pending position immediately so we don't end on a stale frame.
    if (edgeRafRef.current != null) {
      cancelAnimationFrame(edgeRafRef.current);
      edgeRafRef.current = null;
      flushEdgeResize();
    }
    resizingEdgeRef.current = null;
    setResizingEdge(null);
  };

  // ---- export ----
  // Build the list of clips that should be exported. Uses committed regions
  // when present, otherwise falls back to the draft pair (drafts have no
  // crop/speed/mix — those are per-region only).
  const collectClips = useCallback((): Array<{
    inSecs: number;
    outSecs: number;
    crop?: Crop;
    speed?: number;
    mix?: TrackMix;
  }> => {
    if (regions.length > 0) {
      return regions.map((r) => ({
        inSecs: r.inSecs,
        outSecs: r.outSecs,
        crop: r.crop,
        speed: r.speed,
        mix: r.mix,
      }));
    }
    if (draftIn != null && draftOut != null && draftIn < draftOut) {
      return [{ inSecs: draftIn, outSecs: draftOut }];
    }
    return [];
  }, [regions, draftIn, draftOut]);

  // Open the export modal. The modal's Export button is what actually runs the
  // save dialog + ffmpeg.
  const openExport = useCallback(() => {
    if (!srcPath) return;
    if (collectClips().length === 0) {
      setPhase({ kind: "error", message: "No regions to export. Set in/out points first." });
      return;
    }
    // Default mode based on what's available: stitch only makes sense for >1 clip.
    if (collectClips().length <= 1) {
      setExportMode("separate");
    }
    setExportOpen(true);
  }, [srcPath, collectClips]);

  const runExport = useCallback(async () => {
    if (!srcPath) return;
    const clips = collectClips();
    if (clips.length === 0) {
      setPhase({ kind: "error", message: "No regions to export." });
      return;
    }
    setExportOpen(false);

    const baseName = srcPath.split(/[\\/]/).pop()?.replace(/\.[^.]+$/, "") ?? "clip";
    const isStitched = exportMode === "stitched" && clips.length > 1;
    const isAudio = exportFormat === "mp3";
    const isGif = exportFormat === "gif";
    const sized = exportFormat === "mp4" && exportSize.kind === "mb";
    const ext = isAudio ? "mp3" : isGif ? "gif" : "mp4";
    const filterName = isAudio ? "MP3" : isGif ? "GIF" : "MP4";

    // GIF target width: resolved here so the backend doesn't need to know about
    // the "source" preset. Source = the actual source width (no upscale ever).
    const gifTargetWidth = isGif
      ? exportGifResolution.kind === "source"
        ? info?.width ?? 1280
        : exportGifResolution.w
      : null;

    const totalTracks = info?.audio_tracks.length ?? 0;
    const totalAudioTracks = totalTracks > 0 ? totalTracks : null;
    // Convert a TrackMix → backend payload. Returns null when the mix is at
    // defaults so the backend takes the fast single-track path.
    const mixToPayload = (m: TrackMix | undefined): Array<{ index: number; volume: number }> | null => {
      if (totalTracks < 1) return null;
      const eff = m ?? trackMix;
      if (trackMixIsDefault(eff, totalTracks)) return null;
      return trackMixToBackend(eff, totalTracks);
    };
    // Default mix payload (used when a region has no override or for non-region
    // single-clip exports).
    const defaultMixPayload = mixToPayload(trackMix);

    // Backend RegionExport — includes the region's own mix (or null fallback).
    const toRegionExport = (c: { inSecs: number; outSecs: number; crop?: Crop; speed?: number; mix?: TrackMix }) => ({
      in_secs: c.inSecs,
      out_secs: c.outSecs,
      crop: c.crop ?? null,
      speed: c.speed ?? null,
      mix: mixToPayload(c.mix),
    });

    if (isStitched) {
      // Stitched + non-audio needs uniform crop (different output dims would
      // require visible scale+pad). Stitched + sized OR stitched + GIF + audio
      // additionally need uniform speed (single-pass paths).
      const firstCrop = clips[0].crop;
      const allCropMatch = clips.every((c) => cropsEqual(c.crop, firstCrop));
      const allSpeedMatch = clips.every((c) => (c.speed ?? 1) === (clips[0].speed ?? 1));
      if (!isAudio && !allCropMatch) {
        setPhase({
          kind: "error",
          message: "Stitched export requires every region to have the same crop. Use the ✂ button → Apply to all.",
        });
        return;
      }
      if ((sized || isAudio) && !allSpeedMatch) {
        setPhase({
          kind: "error",
          message: "Size-targeted or audio stitched export needs uniform speed across regions.",
        });
        return;
      }

      const suggested = sized
        ? `${baseName}_stitched_${exportSize.mb}mb.${ext}`
        : `${baseName}_stitched.${ext}`;
      const dest = await saveDialog({
        defaultPath: suggested,
        filters: [{ name: filterName, extensions: [ext] }],
      });
      if (!dest) return;

      setPhase({ kind: "exporting", progress: 0, current: 1, total: 1 });
      try {
        const regionPayload = clips.map(toRegionExport);
        if (isGif) {
          await invoke("export_concat_gif", {
            srcPath,
            regions: regionPayload,
            outputPath: dest,
            targetWidth: gifTargetWidth,
          });
        } else if (isAudio) {
          await invoke("export_concat_audio", {
            srcPath,
            regions: regionPayload,
            outputPath: dest,
            normalize: exportNormalize,
            trackMix: defaultMixPayload,
            totalAudioTracks,
          });
        } else if (sized) {
          await invoke("export_concat_sized", {
            srcPath,
            regions: regionPayload,
            outputPath: dest,
            targetSizeMb: exportSize.mb,
            normalize: exportNormalize,
            trackMix: defaultMixPayload,
            totalAudioTracks,
          });
        } else {
          await invoke("export_concat", {
            srcPath,
            regions: regionPayload,
            outputPath: dest,
            normalize: exportNormalize,
            trackMix: defaultMixPayload,
            totalAudioTracks,
          });
        }
        setPhase({ kind: "ready" });
        setLastExport({ paths: [dest] });
      } catch (e) {
        setPhase({ kind: "error", message: String(e) });
      }
      return;
    }

    // Separate clips (or single clip)
    const sizeSuffix = sized ? `_${exportSize.mb}mb` : "";
    const suggested =
      clips.length === 1
        ? `${baseName}_clip_${Math.round(clips[0].inSecs)}-${Math.round(clips[0].outSecs)}${sizeSuffix}.${ext}`
        : `${baseName}_clip_1${sizeSuffix}.${ext}`;
    const firstDest = await saveDialog({
      defaultPath: suggested,
      filters: [{ name: filterName, extensions: [ext] }],
    });
    if (!firstDest) return;

    let outputPaths: string[];
    if (clips.length === 1) {
      outputPaths = [firstDest];
    } else {
      const re = new RegExp(`^(.*?)(?:_\\d+)?\\.${ext}$`, "i");
      const m = firstDest.match(re);
      const base = m ? m[1] : firstDest.replace(new RegExp(`\\.${ext}$`, "i"), "");
      outputPaths = clips.map((_, idx) => `${base}_${idx + 1}.${ext}`);
    }

    try {
      for (let i = 0; i < clips.length; i++) {
        setPhase({ kind: "exporting", progress: 0, current: i + 1, total: clips.length });
        const c = clips[i];
        if (isGif) {
          await invoke("export_clip_gif", {
            srcPath,
            inSecs: c.inSecs,
            outSecs: c.outSecs,
            outputPath: outputPaths[i],
            crop: c.crop ?? null,
            speed: c.speed ?? null,
            targetWidth: gifTargetWidth,
          });
        } else if (isAudio) {
          await invoke("export_clip_audio", {
            srcPath,
            inSecs: c.inSecs,
            outSecs: c.outSecs,
            outputPath: outputPaths[i],
            speed: c.speed ?? null,
            normalize: exportNormalize,
            trackMix: mixToPayload(c.mix),
            totalAudioTracks,
          });
        } else if (sized) {
          await invoke("export_clip_sized", {
            srcPath,
            inSecs: c.inSecs,
            outSecs: c.outSecs,
            outputPath: outputPaths[i],
            targetSizeMb: exportSize.mb,
            crop: c.crop ?? null,
            speed: c.speed ?? null,
            normalize: exportNormalize,
            trackMix: mixToPayload(c.mix),
            totalAudioTracks,
          });
        } else {
          await invoke("export_clip", {
            srcPath,
            inSecs: c.inSecs,
            outSecs: c.outSecs,
            outputPath: outputPaths[i],
            crop: c.crop ?? null,
            speed: c.speed ?? null,
            normalize: exportNormalize,
            trackMix: mixToPayload(c.mix),
            totalAudioTracks,
          });
        }
      }
      setPhase({ kind: "ready" });
      setLastExport({ paths: outputPaths });
    } catch (e) {
      setPhase({ kind: "error", message: String(e) });
    }
  }, [srcPath, collectClips, exportMode, exportSize, exportFormat, exportNormalize, exportGifResolution, info, trackMix]);

  // Alias for the keybind dispatcher.
  const handleExport = openExport;

  // Crop the region containing the playhead (or the nearest if none contain it).
  // Wired to the cropRegion keybind.
  const cropCurrentRegion = useCallback(() => {
    if (regions.length === 0) return;
    const t = videoRef.current?.currentTime ?? 0;
    const inside = regions.find((r) => r.inSecs <= t && t <= r.outSecs);
    const target =
      inside ??
      [...regions].sort((a, b) =>
        Math.abs(a.inSecs - t) - Math.abs(b.inSecs - t)
      )[0];
    if (target) startCropEdit(target.id);
  }, [regions, startCropEdit]);

  // Save Frame is two-stage:
  //   • First click  → copy current frame to clipboard (paste straight into
  //     Discord/Slack — covers the 90% case).
  //   • Click again within ~5 s → open the save dialog and write a PNG.
  // Resets back to "copy" mode if the user moves the playhead substantially or
  // the timeout elapses, so a stale "save" mode never surprises them.
  const [frameAction, setFrameAction] = useState<"copy" | "save">("copy");
  const [frameToast, setFrameToast] = useState<string | null>(null);
  const frameModeAtRef = useRef<number>(0);
  const frameTimerRef = useRef<number | null>(null);
  const clearFrameMode = useCallback(() => {
    setFrameAction("copy");
    setFrameToast(null);
    if (frameTimerRef.current) {
      window.clearTimeout(frameTimerRef.current);
      frameTimerRef.current = null;
    }
  }, []);
  const saveCurrentFrame = useCallback(async () => {
    if (!srcPath || !info) return;
    const t = videoRef.current?.currentTime ?? 0;

    // If we're in "save" mode but the playhead has moved, reset to copy.
    const moved = Math.abs(t - frameModeAtRef.current) > 0.05;
    const mode = !moved && frameAction === "save" ? "save" : "copy";

    if (mode === "copy") {
      try {
        await invoke("copy_frame_to_clipboard", {
          srcPath,
          timeSecs: t,
          width: info.width,
          height: info.height,
        });
        frameModeAtRef.current = t;
        setFrameAction("save");
        setFrameToast("Copied to clipboard — click again to save as PNG");
        if (frameTimerRef.current) window.clearTimeout(frameTimerRef.current);
        frameTimerRef.current = window.setTimeout(clearFrameMode, 5000);
      } catch (e) {
        setPhase({ kind: "error", message: `Couldn't copy frame: ${e}` });
      }
      return;
    }

    // Second click — open save dialog and write a PNG.
    const baseName = srcPath.split(/[\\/]/).pop()?.replace(/\.[^.]+$/, "") ?? "frame";
    const suggested = `${baseName}_frame_${t.toFixed(2).replace(".", "_")}s.png`;
    try {
      const dest = await saveDialog({
        defaultPath: suggested,
        filters: [{ name: "PNG", extensions: ["png"] }],
      });
      clearFrameMode();
      if (!dest) return;
      await invoke("export_frame_png", { srcPath, timeSecs: t, outputPath: dest });
      setLastExport({ paths: [dest] });
    } catch (e) {
      setPhase({ kind: "error", message: String(e) });
    }
  }, [srcPath, info, frameAction, clearFrameMode]);

  // ---- keyboard shortcuts ----
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Don't fire app shortcuts while the keybind editor is capturing.
      if (listeningAction != null) return;
      // openFile is allowed even before a video is loaded; everything else needs a ready/exporting phase.
      const tag = (e.target as HTMLElement | null)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;

      const dispatch = (action: ActionId) => {
        switch (action) {
          case "openFile": handleOpen(); break;
          case "playPause": playPause(); break;
          case "frameBack": stepFrames(-1); break;
          case "frameForward": stepFrames(1); break;
          case "jumpStart": seek(0); break;
          case "jumpEnd": seek(duration); break;
          case "setIn": setIn(); break;
          case "setOut": setOut(); break;
          case "export": handleExport(); break;
          case "loopRegion": toggleLoopRegion(); break;
          case "cropRegion": cropCurrentRegion(); break;
          case "saveFrame": saveCurrentFrame(); break;
          default: {
            // Region jumps: jumpRegion1..jumpRegion9 → seek to that region's in.
            const m = action.match(/^jumpRegion(\d)$/);
            if (m) {
              const idx = parseInt(m[1], 10) - 1;
              const r = regions[idx];
              if (r) seek(r.inSecs);
            }
            break;
          }
        }
      };

      for (const action of Object.keys(keybinds) as ActionId[]) {
        if (matchesBinding(e, keybinds[action])) {
          if (action !== "openFile" && phase.kind !== "ready" && phase.kind !== "exporting") return;
          e.preventDefault();
          dispatch(action);
          return;
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [keybinds, listeningAction, phase, handleOpen, playPause, stepFrames, seek, duration, setIn, setOut, handleExport, toggleLoopRegion, regions, cropCurrentRegion, saveCurrentFrame]);

  // Capture next keypress when listening to bind a new shortcut.
  useEffect(() => {
    if (!listeningAction) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape" && !e.ctrlKey && !e.shiftKey && !e.altKey) {
        setListeningAction(null);
        return;
      }
      const captured = captureKeybind(e);
      if (!captured) return;
      setKeybinds((prev) => ({ ...prev, [listeningAction]: captured }));
      setListeningAction(null);
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true } as AddEventListenerOptions);
  }, [listeningAction]);

  // Esc closes the keybinds modal (when not listening).
  useEffect(() => {
    if (!keybindsOpen || listeningAction) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setKeybindsOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [keybindsOpen, listeningAction]);

  // ---- timeline interaction ----
  const timeFromPointer = useCallback(
    (clientX: number) => {
      const el = timelineRef.current;
      if (!el || !duration) return 0;
      const rect = el.getBoundingClientRect();
      const x = Math.max(0, Math.min(rect.width, clientX - rect.left));
      return (x / rect.width) * duration;
    },
    [duration]
  );

  // High-DPI mice fire pointermove at 200-1000Hz; running scrubTo on every
  // event re-renders the entire App tree per pointer tick. Coalesce to a
  // single rAF flush so we never run more than one scrub per frame.
  const scrubXRef = useRef<number | null>(null);
  const scrubRafRef = useRef<number | null>(null);
  const flushScrub = useCallback(() => {
    scrubRafRef.current = null;
    const x = scrubXRef.current;
    if (x == null) return;
    scrubXRef.current = null;
    scrubTo(timeFromPointer(x));
  }, [scrubTo, timeFromPointer]);

  const onTimelinePointerDown = (e: React.PointerEvent) => {
    if (phase.kind !== "ready" && phase.kind !== "exporting") return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    setIsScrubbing(true);
    const v = videoRef.current;
    if (v) {
      wasMutedRef.current = v.muted;
      wasPlayingRef.current = !v.paused;
      v.muted = true;
      if (!v.paused) v.pause();
    }
    const t = timeFromPointer(e.clientX);
    scrubTo(t);
  };
  const onTimelinePointerMove = (e: React.PointerEvent) => {
    if (!(e.buttons & 1)) return;
    if (!isScrubbing) return;
    scrubXRef.current = e.clientX;
    if (scrubRafRef.current == null) {
      scrubRafRef.current = requestAnimationFrame(flushScrub);
    }
  };
  const onTimelinePointerUp = (e: React.PointerEvent) => {
    if (!isScrubbing) return;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    setIsScrubbing(false);
    // Drop any pending coalesced scrub — the seek() below is the final value.
    if (scrubRafRef.current != null) {
      cancelAnimationFrame(scrubRafRef.current);
      scrubRafRef.current = null;
    }
    scrubXRef.current = null;
    const v = videoRef.current;
    if (v) v.muted = wasMutedRef.current;
    // Apply final position, then restore play state if it was playing before scrub.
    const t = timeFromPointer(e.clientX);
    seek(t);
    if (v && wasPlayingRef.current) {
      v.play().catch(() => {});
    }
  };

  // Draw all per-track waveforms layered onto the canvas (and redraw on
  // container resize). Each track uses its palette color; additive blend so
  // simultaneously-loud tracks brighten where they overlap. Drawing centered
  // on the mid-line — bar heights = max amplitude in that x's bin slice.
  useEffect(() => {
    const canvas = waveCanvasRef.current;
    const container = timelineRef.current;
    if (!canvas || !container) return;

    const draw = () => {
      const cssW = canvas.clientWidth;
      const cssH = canvas.clientHeight;
      if (cssW === 0 || cssH === 0) return;
      const dpr = window.devicePixelRatio || 1;
      if (canvas.width !== Math.round(cssW * dpr) || canvas.height !== Math.round(cssH * dpr)) {
        canvas.width = Math.round(cssW * dpr);
        canvas.height = Math.round(cssH * dpr);
      }
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.scale(dpr, dpr);
      ctx.clearRect(0, 0, cssW, cssH);
      if (waveforms.size === 0) return;

      const mid = cssH / 2;
      // Sort by index so colors are deterministic and the layering doesn't
      // depend on Map insertion order (extraction parallelism).
      const entries = [...waveforms.entries()].sort((a, b) => a[0] - b[0]);

      // Build the per-x effective mix. For each pixel of timeline width, find
      // which region's mix applies (or fall back to the source default).
      // Pre-computed once so the inner draw loop stays cheap.
      const dur = duration > 0 ? duration : 1;
      const xMixes: TrackMix[] = new Array(cssW);
      // Sorted regions are an invariant of setRegions; binary search overkill
      // for typical N <= ~10.
      for (let x = 0; x < cssW; x++) {
        const t = (x / cssW) * dur;
        let m: TrackMix = trackMix;
        for (const r of regions) {
          if (t >= r.inSecs && t <= r.outSecs) {
            m = r.mix ?? trackMix;
            break;
          }
        }
        xMixes[x] = m;
      }

      // Two-pass render:
      //   Pass 1 (source-over, grey, low alpha): muted tracks at original
      //     amplitude — visible as ghost so the user knows what's there.
      //   Pass 2 (lighter, track color): active tracks scaled by per-x volume
      //     so 50% slider → half-height bars, 158% → ~1.6× taller. Heights
      //     clamp to slightly past the track strip so 200% reads as "loud"
      //     without escaping the canvas.
      const maxBar = cssH * 0.92; // hard cap (canvas-bound)
      const unityBar = cssH * 0.55; // bar height at volume == 1.0

      ctx.globalCompositeOperation = "source-over";
      ctx.fillStyle = "rgba(140, 144, 154, 0.16)";
      for (const [idx, bins] of entries) {
        const N = bins.length;
        if (N === 0) continue;
        for (let x = 0; x < cssW; x++) {
          if (!xMixes[x][idx]?.muted) continue;
          const binStart = Math.floor((x / cssW) * N);
          const binEnd = Math.max(binStart + 1, Math.floor(((x + 1) / cssW) * N));
          let max = 0;
          for (let i = binStart; i < binEnd && i < N; i++) {
            const v = bins[i];
            if (v > max) max = v;
          }
          const h = Math.min(max * unityBar, maxBar);
          if (h < 1) continue;
          ctx.fillRect(x, mid - h / 2, 1, h);
        }
      }

      // Average active-count for alpha — heuristic: count active in the
      // middle x to avoid scanning all xMixes again.
      const midMix = xMixes[Math.floor(cssW / 2)] ?? trackMix;
      const activeCount = entries.filter(([i]) => !midMix[i]?.muted).length;
      const baseAlpha = activeCount <= 1 ? 0.85 : 0.55;
      ctx.globalCompositeOperation = "lighter";
      for (const [idx, bins] of entries) {
        const N = bins.length;
        if (N === 0) continue;
        ctx.fillStyle = hexWithAlpha(resolveTrackColor(idx, trackColors), baseAlpha);
        for (let x = 0; x < cssW; x++) {
          const m = xMixes[x][idx];
          if (m?.muted) continue;
          const vol = m?.volume ?? 1;
          if (vol <= 0) continue;
          const binStart = Math.floor((x / cssW) * N);
          const binEnd = Math.max(binStart + 1, Math.floor(((x + 1) / cssW) * N));
          let max = 0;
          for (let i = binStart; i < binEnd && i < N; i++) {
            const v = bins[i];
            if (v > max) max = v;
          }
          const h = Math.min(max * vol * unityBar, maxBar);
          if (h < 1) continue;
          ctx.fillRect(x, mid - h / 2, 1, h);
        }
      }
      ctx.globalCompositeOperation = "source-over";
    };

    draw();
    const ro = new ResizeObserver(draw);
    ro.observe(container);
    return () => ro.disconnect();
  }, [waveforms, trackMix, regions, duration, trackColors]);

  // Draw keyframe ticks. Faint vertical hairlines at every video keyframe so
  // the user can see where stream-copy cuts will snap. Sits below the wave
  // canvas in the stacking order — the waveform additively blends on top so
  // these ticks don't compete visually during playback.
  useEffect(() => {
    const canvas = keyframeCanvasRef.current;
    const container = timelineRef.current;
    if (!canvas || !container) return;

    const draw = () => {
      const cssW = canvas.clientWidth;
      const cssH = canvas.clientHeight;
      if (cssW === 0 || cssH === 0) return;
      const dpr = window.devicePixelRatio || 1;
      if (canvas.width !== Math.round(cssW * dpr) || canvas.height !== Math.round(cssH * dpr)) {
        canvas.width = Math.round(cssW * dpr);
        canvas.height = Math.round(cssH * dpr);
      }
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.scale(dpr, dpr);
      ctx.clearRect(0, 0, cssW, cssH);
      if (!keyframes || keyframes.length === 0 || duration <= 0) return;

      // Hairline ticks. Color is intentionally low-contrast — these are
      // navigational hints, not decoration. Use crisp 1px integer x.
      ctx.fillStyle = "rgba(255, 255, 255, 0.10)";
      let lastX = -2;
      for (let i = 0; i < keyframes.length; i++) {
        const t = keyframes[i];
        if (t < 0 || t > duration) continue;
        const x = Math.round((t / duration) * cssW);
        // Skip duplicates at the same pixel — many keyframes can collapse
        // into one column on a wide source / narrow timeline.
        if (x === lastX) continue;
        lastX = x;
        ctx.fillRect(x, 0, 1, cssH);
      }
    };

    draw();
    const ro = new ResizeObserver(draw);
    ro.observe(container);
    return () => ro.disconnect();
  }, [keyframes, duration]);

  const playheadPct = duration > 0 ? (currentTime / duration) * 100 : 0;
  const draftInPct = draftIn != null && duration > 0 ? (draftIn / duration) * 100 : null;
  const draftOutPct = draftOut != null && duration > 0 ? (draftOut / duration) * 100 : null;

  const exportableCount =
    regions.length > 0
      ? regions.length
      : draftIn != null && draftOut != null && draftIn < draftOut
      ? 1
      : 0;

  // Compute the single status to surface in the strip. Highest-priority
  // state wins. Order, top to bottom: error > work-in-flight > export-done
  // confirmation > frame-copied confirmation > scrubbing > looping > nothing.
  // Single status surface for the app.
  //
  // Memoized so the StatusStrip's prop reference is stable across renders
  // that don't actually change what the strip should show — without this,
  // every timeupdate (60 Hz) during normal playback creates a fresh object
  // and the strip re-renders even though the displayed content is identical.
  const statusContent = useMemo<StatusContent | null>(() => {
    if (phase.kind === "error") {
      return {
        kind: "error",
        message: phase.message,
        onDismiss: () => setPhase({ kind: srcPath ? "ready" : "idle" }),
      };
    }
    if (phase.kind === "probing") {
      return { kind: "phase", label: "Reading file metadata…" };
    }
    if (phase.kind === "proxying") {
      return {
        kind: "phase",
        label: `Preparing source · ${(phase.progress * 100).toFixed(0)}%`,
        progress: phase.progress,
        etaSecs: phase.eta,
      };
    }
    if (phase.kind === "exporting") {
      const label =
        phase.total > 1
          ? `Exporting clip ${phase.current}/${phase.total}`
          : `Exporting`;
      return { kind: "phase", label, progress: phase.progress };
    }
    if (lastExport) {
      return {
        kind: "export-done",
        paths: lastExport.paths,
        onDismiss: () => setLastExport(null),
      };
    }
    if (frameToast) {
      return { kind: "frame-copied", onDismiss: clearFrameMode };
    }
    if (isScrubbing) {
      return { kind: "scrubbing", time: currentTime };
    }
    if (loopingRegion) {
      return {
        kind: "looping",
        regionIndex: loopingRegionIndex,
        regionColor: resolveRegionColor(loopingRegion, loopingRegionIndex),
        onStop: () => toggleLoopRegion(loopingRegion.id),
      };
    }
    return null;
  }, [
    phase,
    srcPath,
    lastExport,
    frameToast,
    isScrubbing,
    currentTime,
    loopingRegion,
    loopingRegionIndex,
    clearFrameMode,
    toggleLoopRegion,
  ]);

  return (
    <div className="app">
      <OnboardingHint />
      <header className="topbar">
        <div className="brand" title="Clippy">
          <div className="brand-mark" aria-hidden />
          <span className="brand-name">Clippy</span>
        </div>
        <button className="ghost" onClick={handleOpen} title={formatKeybind(keybinds.openFile)}>
          Open video…
        </button>
        <div className="filemeta">
          {srcPath ? (
            <>
              <span className="filename" title={srcPath}>
                {srcPath.split(/[\\/]/).pop()}
              </span>
              {info && (
                <span className="dim">
                  {info.width}×{info.height} · {info.fps.toFixed(2)}fps · {info.video_codec}
                  {info.audio_codec ? `/${info.audio_codec}` : ""} · {fmtTime(info.duration_secs)}
                  {proxyEncoder && (
                    <span className="encoder-badge" title="Source preparation strategy">
                      {" "}· {proxyEncoder}
                    </span>
                  )}
                </span>
              )}
            </>
          ) : (
            <span className="dim">No file loaded</span>
          )}
        </div>
        <button
          className="help-button"
          onClick={() => setTipsOpen(true)}
          title="Tips & shortcuts"
          aria-label="Tips and shortcuts"
        >
          ?
        </button>
        <button
          className="btn primary"
          onClick={openExport}
          disabled={phase.kind !== "ready" || !srcPath || exportableCount === 0}
          title={formatKeybind(keybinds.export)}
        >
          {exportableCount > 1 ? `Export ${exportableCount} regions…` : "Export…"}
        </button>
      </header>

      <main className="stage">
        {proxySrc ? (
          <video
            ref={videoRef}
            src={proxySrc}
            className="video"
            onPlay={() => setIsPlaying(true)}
            onPause={() => setIsPlaying(false)}
            onTimeUpdate={(e) => {
              if (isScrubbing) return;
              if (pendingSeekRef.current != null) return;
              const t = e.currentTarget.currentTime;
              setCurrentTime(t);
              // Loop wrap: if we're past the loop region's out, jump back to in.
              const loopId = loopingRegionIdRef.current;
              if (loopId) {
                const r = regionsRef.current.find((x) => x.id === loopId);
                if (r && t >= r.outSecs - 0.02) {
                  try { e.currentTarget.currentTime = r.inSecs; } catch {}
                }
              }
            }}
            onEnded={() => setIsPlaying(false)}
            onClick={playPause}
          />
        ) : (
          <div
            className="placeholder"
            onClick={phase.kind === "idle" || phase.kind === "error" ? handleOpen : undefined}
            role={phase.kind === "idle" || phase.kind === "error" ? "button" : undefined}
          >
            {phase.kind === "idle" ? (
              <>
                <div className="placeholder-icon">▷</div>
                <div className="placeholder-title">Drop a video here</div>
                <div className="placeholder-hint">
                  or press <kbd>{formatKeybind(keybinds.openFile)}</kbd> to open a file
                </div>
                <div className="placeholder-formats">
                  mp4 · mkv · mov · webm · m4v · avi
                </div>
              </>
            ) : phase.kind === "error" ? (
              <>
                <div className="placeholder-title">Couldn't load that file.</div>
                <div className="placeholder-hint">Click to try another</div>
              </>
            ) : (
              <div className="placeholder-title">Loading…</div>
            )}
          </div>
        )}
      </main>

      {/* Bottom zone — status strip (consolidated) + transport (always) +
          tool slots (conditional, in fixed-height containers so adding the
          first region or loading a multi-track source doesn't shift earlier
          layout). */}
      <section className="shell-bottom">
      <StatusStrip content={statusContent} />
      <section className="transport">
        <div className="time-readout">
          <span className="mono">{fmtTime(currentTime)}</span>
          <span className="dim mono"> / {fmtTime(duration)}</span>
        </div>

        <div className="transport-buttons">
          <button onClick={() => seek(0)} title="Home">⏮</button>
          <button onClick={() => stepFrames(-1)} title=",">◀</button>
          <button onClick={playPause} title="Space" className="play">
            {isPlaying ? "⏸" : "▶"}
          </button>
          <button onClick={() => stepFrames(1)} title=".">▶</button>
          <button onClick={() => seek(duration)} title="End">⏭</button>
        </div>

        <div className="mark-buttons">
          <button onClick={setIn} title={formatKeybind(keybinds.setIn)}>Set In</button>
          <button onClick={setOut} title={formatKeybind(keybinds.setOut)}>Set Out</button>
          <button onClick={clearAllRegions} title="Remove all regions and the current draft">
            Clear all
          </button>
          <button
            onClick={saveCurrentFrame}
            disabled={!srcPath || phase.kind !== "ready"}
            className={frameAction === "save" ? "btn-armed" : ""}
            title={
              frameAction === "save"
                ? "Frame copied to clipboard — click again to save as PNG"
                : `Copy current frame to clipboard (${formatKeybind(keybinds.saveFrame)}). Click again to save as PNG.`
            }
          >
            {frameAction === "save" ? "Save as PNG…" : "Copy frame"}
          </button>
          <span className="dim mono draft-readout">
            draft: {draftIn != null ? fmtTime(draftIn) : "--"} → {draftOut != null ? fmtTime(draftOut) : "--"}
          </span>
        </div>
      </section>

      {info && info.audio_tracks.length >= 1 && (
        <TrackMixer
          tracks={info.audio_tracks}
          mix={effectiveMix}
          onChange={setEffectiveMix}
          trackColors={trackColors}
          onTrackColorsChange={setTrackColors}
          trackNames={trackNames}
          onTrackNamesChange={setTrackNames}
          contextLabel={
            playheadRegion ? `Region ${playheadRegionIndex + 1}` : "Default"
          }
          contextColor={
            playheadRegion
              ? resolveRegionColor(playheadRegion, playheadRegionIndex)
              : null
          }
        />
      )}

      {/* Reserved chips slot — empty until the first region is created so
          the timeline below doesn't shift when chips appear. */}
      <div className="shell-slot-chips">
      {regions.length > 0 && (
        <section className="region-list">
          {regions.map((r, i) => {
            const colorSlot = r.colorIndex ?? (i % REGION_COLORS.length);
            const color = resolveRegionColor(r, i);
            return (
            <span
              key={r.id}
              className="region-chip"
              onClick={() => seek(r.inSecs)}
              title="Click to jump playhead to this region's in-point"
              style={{ ["--region-color" as never]: color }}
            >
              <ColorPicker
                className="region-chip-dot"
                colors={REGION_COLORS}
                selectedSlot={colorSlot}
                title="Change this region's color"
                onChange={(newSlot) => {
                  setRegions((rs) =>
                    rs.map((x) => (x.id === r.id ? { ...x, colorIndex: newSlot } : x))
                  );
                }}
              />
              <span className="region-chip-num">{i + 1}</span>
              <span className="region-chip-times mono">
                {fmtTime(r.inSecs)} → {fmtTime(r.outSecs)}
              </span>
              <span className="region-chip-len mono dim">
                {fmtTime(r.outSecs - r.inSecs)}
              </span>
              {r.crop && (
                <span className="region-chip-crop mono" title={`Crop ${r.crop.w}×${r.crop.h} at ${r.crop.x},${r.crop.y}`}>
                  ✂ {r.crop.w}×{r.crop.h}
                </span>
              )}
              {r.speed != null && r.speed !== 1 && (
                <span className="region-chip-speed-badge mono" title={`Playback speed: ${r.speed}×`}>
                  {r.speed}×
                </span>
              )}
              <span className="region-chip-divider" aria-hidden />
              <span className="region-chip-actions">
                <button
                  className={`region-chip-action${r.crop ? " active" : ""}`}
                  onClick={(e) => { e.stopPropagation(); startCropEdit(r.id); }}
                  title={r.crop ? "Edit crop" : "Add a crop to this region"}
                >
                  ✂
                </button>
                <span onClick={(e) => e.stopPropagation()}>
                  <SpeedPicker
                    value={r.speed}
                    onChange={(v) =>
                      setRegions((rs) => rs.map((x) => x.id === r.id ? { ...x, speed: v } : x))
                    }
                  />
                </span>
                <button
                  className={`region-chip-action${loopingRegionId === r.id ? " active-loop" : ""}`}
                  onClick={(e) => { e.stopPropagation(); toggleLoopRegion(r.id); }}
                  title={loopingRegionId === r.id ? "Stop looping" : "Loop this region"}
                >
                  ↻
                </button>
                <button
                  className="region-chip-action region-chip-delete"
                  onClick={(e) => { e.stopPropagation(); deleteRegion(r.id); }}
                  title="Delete region"
                >
                  ×
                </button>
              </span>
            </span>
            );
          })}
        </section>
      )}
      </div>

      <section
        className="timeline"
        ref={timelineRef}
        onPointerDown={onTimelinePointerDown}
        onPointerMove={onTimelinePointerMove}
        onPointerUp={onTimelinePointerUp}
        onPointerCancel={onTimelinePointerUp}
      >
        <canvas ref={keyframeCanvasRef} className="keyframes" />
        <canvas ref={waveCanvasRef} className="waveform" />
        {/* committed regions (one band per region; edges are draggable) */}
        {regions.map((r, i) => {
          if (duration <= 0) return null;
          const left = (r.inSecs / duration) * 100;
          const width = Math.max(0, ((r.outSecs - r.inSecs) / duration) * 100);
          const isResizingThis = resizingEdge?.regionId === r.id;
          const color = resolveRegionColor(r, i);
          return (
            <div
              key={r.id}
              className="region-band"
              style={{ left: `${left}%`, width: `${width}%`, ["--region-color" as never]: color }}
              title={`Region ${i + 1}: ${fmtTime(r.inSecs)} → ${fmtTime(r.outSecs)} — drag edges to adjust`}
            >
              <div
                className={`region-edge-handle left${isResizingThis && resizingEdge?.edge === "in" ? " active" : ""}`}
                onPointerDown={(e) => startEdgeResize(e, r.id, "in")}
                onPointerMove={onEdgeResizeMove}
                onPointerUp={onEdgeResizeUp}
                onPointerCancel={onEdgeResizeUp}
              />
              <span className="region-band-num">{i + 1}</span>
              <span className="region-band-dur mono" aria-hidden>
                {fmtTime(r.outSecs - r.inSecs)}
              </span>
              <div
                className={`region-edge-handle right${isResizingThis && resizingEdge?.edge === "out" ? " active" : ""}`}
                onPointerDown={(e) => startEdgeResize(e, r.id, "out")}
                onPointerMove={onEdgeResizeMove}
                onPointerUp={onEdgeResizeUp}
                onPointerCancel={onEdgeResizeUp}
              />
            </div>
          );
        })}
        {/* draft band (only when both endpoints set) */}
        {draftInPct != null && draftOutPct != null && draftOutPct > draftInPct && (
          <div
            className="region-band draft"
            style={{ left: `${draftInPct}%`, width: `${draftOutPct - draftInPct}%` }}
            title="Draft (will commit when in < out)"
          />
        )}
        {/* draft markers */}
        {draftInPct != null && <div className="marker mark-in" style={{ left: `${draftInPct}%` }} />}
        {draftOutPct != null && <div className="marker mark-out" style={{ left: `${draftOutPct}%` }} />}
        <div className="playhead" style={{ left: `${playheadPct}%` }} />
      </section>

      <footer className="hints" title="Click any shortcut to edit">
        <HintKbd onClick={() => setKeybindsOpen(true)} bind={keybinds.playPause} label="play/pause" />
        <HintKbd onClick={() => setKeybindsOpen(true)} bind={keybinds.frameBack} secondaryBind={keybinds.frameForward} label="frame" />
        <HintKbd onClick={() => setKeybindsOpen(true)} bind={keybinds.jumpStart} secondaryBind={keybinds.jumpEnd} label="jump" />
        <HintKbd onClick={() => setKeybindsOpen(true)} bind={keybinds.setIn} secondaryBind={keybinds.setOut} label="set in/out" />
        <HintKbd onClick={() => setKeybindsOpen(true)} bind={keybinds.export} label="export" />
        {regions.length > 0 && (
          <HintKbd
            onClick={() => setKeybindsOpen(true)}
            bind={keybinds.jumpRegion1}
            secondaryBind={keybinds[`jumpRegion${Math.min(regions.length, 9)}` as ActionId]}
            label={regions.length === 1 ? "jump to region" : "jump to region"}
          />
        )}
        <span className="hints-spacer" />
        <button className="hints-edit" onClick={() => setKeybindsOpen(true)} title="Edit shortcuts">
          edit shortcuts…
        </button>
      </footer>
      </section>{/* /.shell-bottom */}

      {exportOpen && (
        <ExportModal
          clips={collectClips()}
          mode={exportMode}
          setMode={setExportMode}
          size={exportSize}
          setSize={setExportSize}
          format={exportFormat}
          setFormat={setExportFormat}
          normalize={exportNormalize}
          setNormalize={setExportNormalize}
          gifResolution={exportGifResolution}
          setGifResolution={setExportGifResolution}
          sourceWidth={info?.width ?? 0}
          sourceHeight={info?.height ?? 0}
          sourceBitrateBps={info?.bit_rate_bps ?? null}
          onCancel={() => setExportOpen(false)}
          onConfirm={runExport}
        />
      )}

      {isDraggingFile && (
        <div className="drop-overlay">
          <div className="drop-message">
            <div className="drop-icon">⬇</div>
            <div>Drop to open</div>
          </div>
        </div>
      )}

      {editingRegion && info && (
        <CropOverlay
          videoElement={videoRef.current}
          sourceWidth={info.width}
          sourceHeight={info.height}
          initialCrop={editingRegion.crop}
          totalRegions={regions.length}
          onDone={finishCropEdit}
          onApplyToAll={applyCropToAllRegions}
          onCancel={cancelCropEdit}
        />
      )}

      {/* Read-only crop indicator: shows when playhead is inside a cropped region
          and the overlay isn't open (the overlay already shows the same bounds). */}
      {!editingRegion && activeCroppedRegion?.crop && info && (
        <CropIndicator
          videoElement={videoRef.current}
          sourceWidth={info.width}
          sourceHeight={info.height}
          crop={activeCroppedRegion.crop}
        />
      )}

      {tipsOpen && <TipsModal onClose={() => setTipsOpen(false)} />}

      {keybindsOpen && (
        <div className="modal-overlay" onMouseDown={(e) => { if (e.target === e.currentTarget) setKeybindsOpen(false); }}>
          <div className="modal">
            <header className="modal-header">
              <h2>Keyboard shortcuts</h2>
              <button className="modal-close" onClick={() => setKeybindsOpen(false)}>×</button>
            </header>
            <div className="kb-list">
              {(Object.keys(ACTION_LABELS) as ActionId[]).map((action) => {
                const isListening = listeningAction === action;
                const conflicts = (Object.keys(keybinds) as ActionId[])
                  .filter(
                    (other) =>
                      other !== action &&
                      formatKeybind(keybinds[other]) === formatKeybind(keybinds[action])
                  );
                return (
                  <div key={action} className={`kb-row${conflicts.length ? " has-conflict" : ""}`}>
                    <span className="kb-label">{ACTION_LABELS[action]}</span>
                    <button
                      className={`kb-binding${isListening ? " listening" : ""}`}
                      onClick={() => setListeningAction(action)}
                    >
                      {isListening ? "Press a key…  (Esc to cancel)" : formatKeybind(keybinds[action])}
                    </button>
                    {conflicts.length > 0 && (
                      <span className="kb-conflict" title={`Conflicts with: ${conflicts.map((c) => ACTION_LABELS[c]).join(", ")}`}>
                        conflict
                      </span>
                    )}
                  </div>
                );
              })}
            </div>
            <CacheControls />
            <DiagnosticsButton />
            <footer className="modal-footer">
              <button onClick={() => setKeybinds(DEFAULT_KEYBINDS)}>Reset to defaults</button>
              <button className="primary" onClick={() => setKeybindsOpen(false)}>Done</button>
            </footer>
          </div>
        </div>
      )}
    </div>
  );
}

function HintKbd(props: {
  bind: Keybind;
  secondaryBind?: Keybind;
  label: string;
  onClick: () => void;
}) {
  return (
    <span className="hint-item" onClick={props.onClick}>
      <kbd>{formatKeybind(props.bind)}</kbd>
      {props.secondaryBind && (
        <>
          /<kbd>{formatKeybind(props.secondaryBind)}</kbd>
        </>
      )}{" "}
      {props.label}
    </span>
  );
}

/** Copies the in-memory diagnostic log to the clipboard. The log records
 *  every significant backend operation (probe, proxy strategy, ffmpeg args,
 *  export path decisions) so you can paste it when reporting a bug. Nothing
 *  is written to disk or sent anywhere automatically. */
function DiagnosticsButton() {
  const [state, setState] = useState<"idle" | "copied" | "error">("idle");
  const copy = async () => {
    try {
      const text = await invoke<string>("get_diagnostics");
      await navigator.clipboard.writeText(text);
      setState("copied");
      setTimeout(() => setState("idle"), 2500);
    } catch {
      setState("error");
      setTimeout(() => setState("idle"), 2500);
    }
  };
  return (
    <div className="cache-row">
      <span className="cache-label">Diagnostics</span>
      <span className="cache-size mono" style={{ fontSize: "var(--type-xs)", opacity: 0.6 }}>
        {state === "copied" ? "Copied to clipboard" : state === "error" ? "Copy failed" : "operation log"}
      </span>
      <button className="cache-clear" onClick={copy} title="Copy the diagnostic log to clipboard — paste it when reporting a bug">
        Copy log
      </button>
    </div>
  );
}

/** Tiny cache-cleanup row inside the keybinds modal. Auto-prune for files
 *  >30 days untouched runs at app startup; this exposes the manual button
 *  plus the current size readout. */
function CacheControls() {
  const [size, setSize] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const refresh = useCallback(() => {
    invoke<number>("cache_size")
      .then((s) => setSize(s))
      .catch(() => setSize(null));
  }, []);
  useEffect(() => { refresh(); }, [refresh]);
  const fmt = (b: number) =>
    b >= 1_073_741_824
      ? `${(b / 1_073_741_824).toFixed(2)} GB`
      : b >= 1_048_576
      ? `${(b / 1_048_576).toFixed(1)} MB`
      : b >= 1024
      ? `${(b / 1024).toFixed(0)} KB`
      : `${b} B`;
  return (
    <div className="cache-row">
      <span className="cache-label">Cache</span>
      <span className="cache-size mono">{size == null ? "—" : fmt(size)}</span>
      <button
        className="cache-clear"
        disabled={busy || size === 0}
        onClick={async () => {
          setBusy(true);
          try {
            await invoke("clear_cache");
          } catch {}
          setBusy(false);
          refresh();
        }}
        title="Delete all cached proxies, waveforms, and project files. They'll be regenerated on next open."
      >
        {busy ? "Clearing…" : "Clear cache"}
      </button>
    </div>
  );
}

