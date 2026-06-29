import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  cropsEqual,
  GIF_DEFAULT_RESOLUTION,
  newRegionId,
  regionDisplayName,
  SIZE_PRESETS,
  resolveRegionColor,
  trackMixIsDefault,
  trackMixToBackend,
  type Crop,
  type ExportFormat,
  type ExportMode,
  type GifResolution,
  type Phase,
  type ProjectState,
  type ProxyResult,
  type Region,
  type SizeLimit,
  type TrackColorOverrides,
  type TrackMix,
  type TrackNameOverrides,
  type VideoInfo,
} from "./types";
import { fmtTime } from "./formatters";
import { logErr } from "./logErr";
import {
  DEFAULT_KEYBINDS,
  formatKeybind,
  keybindToShortcutString,
  loadKeybinds,
  saveKeybinds,
  type ActionId,
  type Keybinds,
} from "./keybinds";
import {
  AboutTab,
  APP_VERSION,
  KeyboardSettingsTab,
  SETTINGS_TABS,
  StorageSettingsTab,
  type SettingsTabId,
} from "./settings";
import { SettingsIcon } from "./settings/settings-icon";
import { ExportModal } from "./ExportModal";
import { CropOverlay } from "./CropOverlay";
import { CropIndicator } from "./CropIndicator";
import { AudioPanel } from "./components/AudioPanel";
import { useAudioTracks } from "./useAudioTracks";
import { StatusStrip, type StatusContent } from "./StatusStrip";
import { TipsModal } from "./TipsModal";
import { OnboardingHint } from "./OnboardingHint";
import { ReplayStatusPill } from "./ReplayStatusPill";
import { useUpdater } from "./useUpdater";
import { ReplaySettings } from "./ReplaySettings";
import { useDragDropFile } from "./hooks/use-drag-drop-file";
import { useExportProgress } from "./hooks/use-export-progress";
import { useGlobalKeybinds } from "./hooks/use-global-keybinds";
import { useKeybindCapture } from "./hooks/use-keybind-capture";
import { useKeyframeDraw } from "./hooks/use-keyframe-draw";
import { useModalEscClose } from "./hooks/use-modal-esc-close";
import { useProjectAutosave } from "./hooks/use-project-autosave";
import { useProxyProgress } from "./hooks/use-proxy-progress";
import { useReplayAutoStart } from "./hooks/use-replay-auto-start";
import { useReplayHotkeyPush } from "./hooks/use-replay-hotkey-push";
import { useReplaySavedToast } from "./hooks/use-replay-saved-toast";
import { useWaveformDraw } from "./hooks/use-waveform-draw";
import { HintKbd } from "./components/HintKbd";
import { WindowControls } from "./components/WindowControls";
import { EditorRail, type RailTab } from "./components/EditorRail";
import { RegionsPanel } from "./components/RegionsPanel";
import { CropPanel } from "./components/CropPanel";
import { EmptyState } from "./components/EmptyState";
import "./App.css";

import { Showcase } from "./Showcase";

/** Dev-only design-system showcase. Visit the app with `?showcase=1` to see
 *  every token, button, input, chip, status-strip variant in isolation —
 *  used for Design Pass 2 validation before tokens get mechanically applied
 *  across the existing UI. Gated to dev builds; tree-shaken in release. */
function isShowcaseRequested(): boolean {
  if (!import.meta.env.DEV) return false;
  try {
    return new URLSearchParams(window.location.search).has("showcase");
  } catch {
    return false;
  }
}

export default function App() {
  // Hard switch — if ?showcase=1 is in the URL (dev builds only), render the
  // showcase page instead of the editor. No side effects from main App
  // mount (no Tauri commands fire, no event listeners install).
  // The conditional resolves to `false` at build time in release because
  // import.meta.env.DEV is statically replaced, so Vite's tree-shaker
  // strips the Showcase module out of the release bundle entirely.
  if (isShowcaseRequested()) {
    return <Showcase />;
  }
  return <Editor />;
}

function Editor() {
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
  // Editor rail — which tab is focused and which region is the "active" one
  // (drives the Crop panel's edits and the Regions panel's expanded row).
  const [railTab, setRailTab] = useState<RailTab>("regions");
  const [activeRegionId, setActiveRegionId] = useState<string | null>(null);
  // Hover state for the timeline tooltip (seconds at the cursor's x). Cleared
  // on pointer-leave / while dragging.
  const [hoverTime, setHoverTime] = useState<number | null>(null);
  const [hoverPct, setHoverPct] = useState<number | null>(null);
  // Hidden mirror <video> used to render a thumbnail at the hovered timecode.
  // Mounted whenever we have a proxy src; pulls frames only on seek so it's
  // ~free when the user isn't moving the cursor. The tooltip's canvas reads
  // from this video via drawImage on the `seeked` event.
  const previewVideoRef = useRef<HTMLVideoElement | null>(null);
  const previewCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const previewSeekRafRef = useRef<number | null>(null);
  const [isScrubbing, setIsScrubbing] = useState(false);
  const [proxyEncoder, setProxyEncoder] = useState<string | null>(null);

  // Keybinds
  const [keybinds, setKeybinds] = useState<Keybinds>(() => loadKeybinds());
  const [keybindsOpen, setKeybindsOpen] = useState(false);
  const [settingsTab, setSettingsTab] = useState<SettingsTabId>("replay");
  const [listeningAction, setListeningAction] = useState<ActionId | null>(null);
  useEffect(() => { saveKeybinds(keybinds); }, [keybinds]);

  // Auto-updater. Silent check on launch; manual check + install from the
  // About tab. State is passed down so the tab can render the same banner
  // the user sees on the topbar.
  const updater = useUpdater();

  useReplayHotkeyPush(keybinds.saveReplay);

  // Tips modal — opened by the "?" button in the topbar.
  const [tipsOpen, setTipsOpen] = useState(false);

  // Export modal state
  const [exportOpen, setExportOpen] = useState(false);
  const [exportSize, setExportSize] = useState<SizeLimit>(SIZE_PRESETS[0]);
  const [exportMode, setExportMode] = useState<ExportMode>("separate");
  const [exportFormat, setExportFormat] = useState<ExportFormat>("mp4");
  const [exportPreserveTracks, setExportPreserveTracks] = useState(false);
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
  const [replaySavedToast, setReplaySavedToast] = useState<string | null>(null);
  // Surfaces replay-buffer save failures (which are otherwise console-only) so
  // an in-game save that fails reaches the user via StatusStrip.
  const [replayError, setReplayError] = useState<string | null>(null);

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
          logErr("register_file_url", err);
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
      // Mark this clip as "kept" so the storage auto-cleanup spares it from
      // the unkept-replays sweep. Fire-and-forget — the index write is
      // tolerant to failures and we don't want to block the load path.
      invoke("storage_mark_opened", { path: selected }).catch(() => {});
      setRegions([]);
      setDraftIn(null);
      setDraftOut(null);
      setCurrentTime(0);
      setTrackMix({});
      setTrackColors({});
      setTrackNames({});
      // Defensive: any selection / crop-edit / loop reference points at a
      // region id that's about to be cleared. Drop them so the rail header,
      // audio context, and crop overlay don't dangle on a stale id between
      // the regions wipe and the project-state restore below.
      setActiveRegionId(null);
      setCropEditingRegionId(null);
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
        logErr("load_project", err);
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
            logErr(`waveform extract track ${idx}`, err);
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
          logErr("probe_keyframes", err);
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
      .catch((err) => logErr("get_initial_path", err));
    // loadFile is stable (useCallback with empty deps), but we intentionally
    // run this only once at mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const [isDraggingFile, setIsDraggingFile] = useState(false);
  useDragDropFile(loadFile, setIsDraggingFile);
  useReplayAutoStart();
  useReplaySavedToast(loadFile, setReplaySavedToast, setReplayError);
  useProxyProgress(setPhase);
  useExportProgress(setPhase);

  // Apply the user's storage cap + unkept-cleanup policy at startup. Reads
  // the settings the Storage tab persisted to localStorage; fires once with
  // dry_run: false (no confirmation prompt) because cap changes are
  // confirmed at the point they're made — startup is just maintenance.
  useEffect(() => {
    const capGb = Number(localStorage.getItem("clippy.storage.cap_gb") || 0);
    const days = Number(localStorage.getItem("clippy.storage.unkept_days") || 0);
    if (capGb <= 0 && days <= 0) return;
    const capBytes = capGb > 0 ? Math.round(capGb * 1_073_741_824) : null;
    const unkeptMaxDays = days > 0 ? days : null;
    invoke("storage_prune", { capBytes, unkeptMaxDays, dryRun: false }).catch(() => {});
  }, []);

  // Forward-declared above the Esc gate so the effect's deps stay honest;
  // the actual cropEditingRegionId state hook lives further down. Kept here
  // as a closure read via ref instead of a dep to avoid re-binding the
  // listener every render — Esc only needs the current value at fire time.
  const cropOverlayOpenRef = useRef(false);

  // Esc clears the active-region selection so the user can reach a
  // no-region-selected state without finding the active row in the rail.
  // Gated: don't fire while a modal is open or a field has focus, both of
  // which use Esc for their own dismissal/cancel semantics.
  useEffect(() => {
    if (activeRegionId == null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (exportOpen || keybindsOpen || tipsOpen) return;
      if (cropOverlayOpenRef.current) return;
      const tag = (e.target as HTMLElement | null)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      setActiveRegionId(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [activeRegionId, exportOpen, keybindsOpen, tipsOpen]);

  // ---- transport controls ----
  const playPause = useCallback(() => {
    const v = videoRef.current;
    if (!v) return;
    if (v.paused) v.play();
    else v.pause();
  }, []);

  useProjectAutosave({ srcPath, regions, trackMix, trackColors, trackNames });

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
      const id = newRegionId();
      setRegions((r) =>
        [
          ...r,
          {
            id,
            inSecs: t,
            outSecs: draftOut,
            crop: inheritedCrop(r),
            mix: inheritedMix(r),
          },
        ].sort((a, b) => a.inSecs - b.inSecs)
      );
      setActiveRegionId(id);
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
      const id = newRegionId();
      setRegions((r) =>
        [
          ...r,
          {
            id,
            inSecs: draftIn,
            outSecs: t,
            crop: inheritedCrop(r),
            mix: inheritedMix(r),
          },
        ].sort((a, b) => a.inSecs - b.inSecs)
      );
      setActiveRegionId(id);
      setDraftIn(null);
      setDraftOut(null);
    } else {
      setDraftOut(t);
      if (draftIn != null && t <= draftIn) setDraftIn(null);
    }
  }, [draftIn, inheritedCrop, inheritedMix]);

  const deleteRegion = useCallback((id: string) => {
    setRegions((r) => r.filter((x) => x.id !== id));
    setActiveRegionId((cur) => (cur === id ? null : cur));
  }, []);

  // Create a new region anchored at the playhead. Default span is 5s, capped
  // at the source duration. Sets the new region as active so the rail's
  // expanded row and Crop panel immediately reflect the new context. Avoids
  // creating duplicate regions at the exact same in-point.
  const addRegionFromPlayhead = useCallback(() => {
    const v = videoRef.current;
    const total = info?.duration_secs ?? 0;
    if (!v || total <= 0) return;
    const t = v.currentTime;
    const span = 5;
    const inSecs = Math.max(0, Math.min(t, Math.max(0, total - 0.5)));
    const outSecs = Math.max(inSecs + 0.5, Math.min(total, inSecs + span));
    const newId = newRegionId();
    setRegions((r) => {
      // Skip when an existing region already starts within 0.05s of here —
      // double-tap protection against fast accidental clicks.
      if (r.some((x) => Math.abs(x.inSecs - inSecs) < 0.05)) return r;
      return [
        ...r,
        { id: newId, inSecs, outSecs },
      ].sort((a, b) => a.inSecs - b.inSecs);
    });
    setActiveRegionId(newId);
  }, [info]);

  const setRegionSpeed = useCallback((id: string, speed: number | undefined) => {
    setRegions((rs) => rs.map((r) => (r.id === id ? { ...r, speed } : r)));
  }, []);
  const setRegionColorIndex = useCallback((id: string, colorIndex: number) => {
    setRegions((rs) => rs.map((r) => (r.id === id ? { ...r, colorIndex } : r)));
  }, []);
  // User-set region label, persisted via useProjectAutosave. Empty input
  // clears the override (regionDisplayName falls back to "Region N").
  const setRegionName = useCallback((id: string, name: string) => {
    const trimmed = name.trim();
    setRegions((rs) =>
      rs.map((r) => {
        if (r.id !== id) return r;
        if (trimmed.length === 0) {
          const { name: _drop, ...rest } = r;
          return rest;
        }
        return { ...r, name: trimmed };
      })
    );
  }, []);
  /** Direct crop assignment used by the Crop panel's preset chips. Skips the
   *  full overlay editor — preset crops are deterministic so we just write
   *  the result. `undefined` clears the crop. */
  const setRegionCropDirect = useCallback((id: string, crop: Crop | undefined) => {
    setRegions((rs) => rs.map((r) => (r.id === id ? { ...r, crop } : r)));
  }, []);

  // Crop edit mode: which region's crop are we editing? null = not editing.
  const [cropEditingRegionId, setCropEditingRegionId] = useState<string | null>(null);
  const wasPlayingBeforeCropRef = useRef(false);
  // Mirror into the ref the Esc-deselect gate reads. Saves rebinding the
  // listener every time cropEditingRegionId flips and avoids putting the
  // state in the listener's deps (which would defeat the gate).
  useEffect(() => {
    cropOverlayOpenRef.current = cropEditingRegionId != null;
  }, [cropEditingRegionId]);
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
  const editingRegion = useMemo(
    () => (cropEditingRegionId ? regions.find((r) => r.id === cropEditingRegionId) ?? null : null),
    [regions, cropEditingRegionId],
  );

  // Active cropped region for the read-only indicator: first region whose crop
  // is set AND contains the playhead. Memoized on currentTime+regions so
  // downstream `memo()` consumers see a stable reference between time updates
  // that don't actually cross a region boundary.
  const activeCroppedRegion = useMemo(
    () =>
      regions.find(
        (r) => r.crop && currentTime >= r.inSecs && currentTime <= r.outSecs,
      ) ?? null,
    [regions, currentTime],
  );

  // Region containing the playhead (first match, sorted by inSecs already).
  // Drives the PLAYBACK mix only — as the playhead crosses a region boundary,
  // useAudioTracks switches gains to that region's mix in real time.
  const playheadRegion = useMemo(
    () =>
      regions.find(
        (r) => currentTime >= r.inSecs && currentTime <= r.outSecs,
      ) ?? null,
    [regions, currentTime],
  );
  // Region that the AUDIO MIXER edits. Anchored to the user's explicit
  // selection (clicking a region in the rail) rather than the playhead so
  // muting a track on the selected region doesn't depend on whether the
  // playhead happens to be inside the region at click time. Falls back to
  // the playhead-containing region (for the legacy "scrub through a region
  // and tweak its mix" workflow) and then to global if neither applies.
  const audioEditRegion = useMemo(
    () => regions.find((r) => r.id === activeRegionId) ?? playheadRegion ?? null,
    [regions, activeRegionId, playheadRegion],
  );
  const audioEditRegionIndex = useMemo(
    () => (audioEditRegion ? regions.indexOf(audioEditRegion) : -1),
    [regions, audioEditRegion],
  );

  // The rail's "active region" chip — stabilize the object identity so any
  // future React.memo on EditorRail isn't defeated by a fresh literal per render.
  const railActiveRegion = useMemo(() => {
    const r = regions.find((x) => x.id === activeRegionId) ?? playheadRegion ?? null;
    if (!r) return null;
    const idx = regions.indexOf(r);
    return {
      name: regionDisplayName(r, idx),
      color: resolveRegionColor(r, idx),
      number: idx + 1,
    };
  }, [regions, activeRegionId, playheadRegion]);
  // Two views on the mix:
  //   • editMix — what the AudioPanel reads/writes. Tied to the selected
  //     region so a mute lands on the right slice of state.
  //   • playbackMix — what useAudioTracks consumes. Tied to the playhead so
  //     audio at any given moment reflects the local context. These differ
  //     when the user is editing region N's mix while the playhead is
  //     outside it — that's expected; the user hears the change once they
  //     scrub into region N.
  const editMix: TrackMix = audioEditRegion?.mix ?? trackMix;
  const playbackMix: TrackMix = playheadRegion?.mix ?? trackMix;
  // Writes go to the audio-edit region's mix when one is selected, else
  // the global trackMix. Captured as a stable callback so AudioPanel
  // re-renders don't churn its onChange identity.
  const setEffectiveMix = useCallback(
    (next: TrackMix) => {
      if (audioEditRegion) {
        const id = audioEditRegion.id;
        setRegions((rs) => rs.map((r) => (r.id === id ? { ...r, mix: next } : r)));
      } else {
        setTrackMix(next);
      }
    },
    [audioEditRegion]
  );

  // Multi-track audio: extract each track on file load and feed them through
  // WebAudio so the mixer's sliders/mutes affect playback in real time. Hook
  // is a no-op for single-track sources. The mix here is the PLAYBACK mix —
  // changes flow in as the playhead crosses region boundaries.
  useAudioTracks({
    videoElement: videoRef.current,
    srcPath,
    tracks: info?.audio_tracks ?? [],
    mix: playbackMix,
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
            preserveMultiTrack: exportPreserveTracks,
            trackMix: defaultMixPayload,
            totalAudioTracks,
          });
        } else if (sized) {
          await invoke("export_concat_sized", {
            srcPath,
            regions: regionPayload,
            outputPath: dest,
            targetSizeMb: exportSize.mb,
            preserveMultiTrack: exportPreserveTracks,
            trackMix: defaultMixPayload,
            totalAudioTracks,
          });
        } else {
          await invoke("export_concat", {
            srcPath,
            regions: regionPayload,
            outputPath: dest,
            preserveMultiTrack: exportPreserveTracks,
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
            preserveMultiTrack: exportPreserveTracks,
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
            preserveMultiTrack: exportPreserveTracks,
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
            preserveMultiTrack: exportPreserveTracks,
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
  }, [srcPath, collectClips, exportMode, exportSize, exportFormat, exportPreserveTracks, exportGifResolution, info, trackMix]);

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
  const dispatchKeybind = useCallback((action: ActionId) => {
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
  }, [handleOpen, playPause, stepFrames, seek, duration, setIn, setOut, handleExport, toggleLoopRegion, regions, cropCurrentRegion, saveCurrentFrame]);
  useGlobalKeybinds({ keybinds, listeningAction, phase, dispatch: dispatchKeybind });
  useKeybindCapture({ listeningAction, setListeningAction, setKeybinds });
  useModalEscClose(keybindsOpen && !listeningAction, () => setKeybindsOpen(false));

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
    // Click landing outside any region band clears the active selection so
    // the user can reach a "no region selected" state without finding the
    // already-active row in the rail. Hit-test by checking whether any
    // .region-band ancestor exists between target and the timeline element.
    const target = e.target as HTMLElement | null;
    const inBand =
      target?.closest?.(".region-band") != null ||
      target?.classList?.contains("region-edge-handle") === true;
    if (!inBand && activeRegionId != null) {
      setActiveRegionId(null);
    }
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
    // Hover tooltip — only when not scrubbing or edge-resizing. Updates each
    // pointer tick, which is cheap (two number setters) and React batches.
    if (!(e.buttons & 1) && !resizingEdge && duration > 0) {
      const el = timelineRef.current;
      if (el) {
        const rect = el.getBoundingClientRect();
        const x = Math.max(0, Math.min(rect.width, e.clientX - rect.left));
        setHoverTime(timeFromPointer(e.clientX));
        setHoverPct((x / rect.width) * 100);
      }
    }
    if (!(e.buttons & 1)) return;
    if (!isScrubbing) return;
    scrubXRef.current = e.clientX;
    if (scrubRafRef.current == null) {
      scrubRafRef.current = requestAnimationFrame(flushScrub);
    }
  };
  const onTimelinePointerLeave = () => {
    setHoverTime(null);
    setHoverPct(null);
  };

  // Drive the hidden preview-video element from hover state. Coalesced via
  // rAF so a fast-moving cursor doesn't fire a seek per pointer tick — one
  // seek per frame is the natural ceiling on what the decoder can satisfy.
  useEffect(() => {
    if (hoverTime == null) return;
    const v = previewVideoRef.current;
    if (!v) return;
    if (previewSeekRafRef.current != null) cancelAnimationFrame(previewSeekRafRef.current);
    previewSeekRafRef.current = requestAnimationFrame(() => {
      previewSeekRafRef.current = null;
      try {
        // Clamp; some demuxers reject seeks past duration by a hair.
        const dur = v.duration;
        const t = Number.isFinite(dur) && dur > 0
          ? Math.min(hoverTime, dur - 0.05)
          : hoverTime;
        v.currentTime = Math.max(0, t);
      } catch {}
    });
    return () => {
      if (previewSeekRafRef.current != null) {
        cancelAnimationFrame(previewSeekRafRef.current);
        previewSeekRafRef.current = null;
      }
    };
  }, [hoverTime]);

  /** When the hidden video lands on the requested frame, copy it onto the
   *  tooltip canvas. Both refs may be null (tooltip hidden, video unmounted)
   *  so guard accordingly. */
  const onPreviewSeeked = useCallback(() => {
    const v = previewVideoRef.current;
    const c = previewCanvasRef.current;
    if (!v || !c) return;
    const ctx = c.getContext("2d");
    if (!ctx) return;
    try { ctx.drawImage(v, 0, 0, c.width, c.height); } catch {}
  }, []);
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

  useWaveformDraw({
    canvasRef: waveCanvasRef,
    containerRef: timelineRef,
    waveforms,
    regions,
    trackMix,
    trackColors,
    duration,
    // Per-track colored envelope on the timeline so the user can see at a
    // glance which track is doing what — and the gaps between the 2 px
    // bars let the keyframe ticks underneath stay legible. Mixed-neutral
    // mode is still available via the hook for surfaces that don't want
    // the per-track palette.
    mode: "tracks",
  });
  useKeyframeDraw({
    canvasRef: keyframeCanvasRef,
    containerRef: timelineRef,
    keyframes,
    duration,
  });

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
    if (replayError) {
      return {
        kind: "error",
        message: replayError,
        onDismiss: () => setReplayError(null),
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
    if (replaySavedToast) {
      return {
        kind: "replay-saved",
        path: replaySavedToast,
        onOpen: () => {
          const p = replaySavedToast;
          setReplaySavedToast(null);
          loadFile(p).catch((e) => logErr("replay clip open after save", e));
        },
        onDismiss: () => setReplaySavedToast(null),
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
    replaySavedToast,
    replayError,
    loadFile,
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
      <OnboardingHint keybinds={keybinds} />
      {/* Hidden mirror of the source for hover-frame thumbnails. Lives at the
          app root so it persists across timeline / tooltip remounts. preload
          is critical — without it Chrome won't keep enough frames around to
          serve fast scrub seeks. */}
      {proxySrc && (
        <video
          ref={previewVideoRef}
          src={proxySrc}
          className="hover-preview-video"
          muted
          preload="auto"
          onSeeked={onPreviewSeeked}
          aria-hidden
        />
      )}
      <header className="topbar" data-tauri-drag-region>
        <div className="brand" title="Clippy" data-tauri-drag-region>
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
                <span className="filemeta-line">
                  <span>{info.width}×{info.height}</span>
                  <span className="filemeta-sep">·</span>
                  <span>{info.fps.toFixed(2)}fps</span>
                  <span className="filemeta-sep">·</span>
                  <span>
                    {info.video_codec}
                    {info.audio_codec ? `/${info.audio_codec}` : ""}
                  </span>
                  <span className="filemeta-sep">·</span>
                  <span>{fmtTime(info.duration_secs)}</span>
                  {proxyEncoder && (
                    <span
                      className={`strategy-pill ${
                        proxyEncoder === "direct" || proxyEncoder === "remux"
                          ? "is-good"
                          : "is-warn"
                      }`}
                      title="Source preparation strategy"
                    >
                      {proxyEncoder}
                    </span>
                  )}
                </span>
              )}
            </>
          ) : (
            <span className="filemeta-empty">No file loaded</span>
          )}
        </div>
        {/* Replay buffer status sits in the topbar as the "app state" indicator
            (analogous to Discord's avatar/status cluster, Steam's online dot).
            Only renders when the buffer is running — hidden when Idle. */}
        <ReplayStatusPill onClick={() => { setSettingsTab("replay"); setKeybindsOpen(true); }} />
        {updater.state.kind === "available" && (
          <button
            type="button"
            className="update-pill"
            onClick={() => { setSettingsTab("about"); setKeybindsOpen(true); }}
            title={`Update to v${updater.state.version} available — click to install`}
          >
            <span className="update-pill-dot" aria-hidden />
            Update to v{updater.state.version}
          </button>
        )}
        <button
          className="help-button"
          onClick={() => setTipsOpen(true)}
          title="Tips & shortcuts"
          aria-label="Tips and shortcuts"
        >
          ?
        </button>
        {/* Settings entry point — previously reachable only via the replay
            pill (hidden when Idle), the bottom hint row, or the rare
            update-available pill. Gear lives next to the help button so
            both meta-actions cluster on the right edge before Export. */}
        <button
          className="topbar-settings-btn"
          onClick={() => setKeybindsOpen(true)}
          title="Settings"
          aria-label="Settings"
        >
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor"
               strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09a1.65 1.65 0 0 0 1.51-1 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>
        <button
          className="btn primary"
          onClick={openExport}
          disabled={phase.kind !== "ready" || !srcPath || exportableCount === 0}
          title={formatKeybind(keybinds.export)}
        >
          {exportableCount > 1 ? `Export ${exportableCount} regions…` : "Export…"}
        </button>
        <WindowControls />
      </header>

      {srcPath == null && phase.kind === "idle" ? (
        <EmptyState
          onOpenDialog={handleOpen}
          onLoadPath={(p) => { void loadFile(p).catch((e) => logErr("recent clip open", e)); }}
          onOpenSettings={() => setKeybindsOpen(true)}
        />
      ) : (
      <>
      <div className="stage-wrap">
      <main className="stage">
        <div className="video-card">
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
          <div className="placeholder">
            {phase.kind === "error" ? (
              <>
                <div className="placeholder-title">Couldn't load that file.</div>
                <button className="btn ghost placeholder-cta" onClick={handleOpen} type="button">
                  Open another…
                </button>
              </>
            ) : (
              <div className="placeholder-title">Loading…</div>
            )}
          </div>
        )}
        {/* On-preview overlays — 1-9 region key hints, speed badge for the
            active region. Crop indicator stays driven by playhead position
            via the separate CropIndicator element below the modal. */}
        {proxySrc && regions.length > 0 && (
          <div className="preview-keys" aria-hidden>
            {regions.slice(0, 9).map((r, i) => (
              <button
                key={r.id}
                type="button"
                className={`preview-key${activeRegionId === r.id ? " active" : ""}`}
                onClick={() => { setActiveRegionId(r.id); seek(r.inSecs); }}
                title={`Jump to region ${i + 1}`}
              >
                {i + 1}
              </button>
            ))}
          </div>
        )}
        {(() => {
          const activeR = regions.find((x) => x.id === activeRegionId) ?? null;
          const speed = activeR?.speed;
          if (!proxySrc || !activeR || !speed || speed === 1) return null;
          const color = resolveRegionColor(activeR, regions.indexOf(activeR));
          return (
            <div className="preview-speed-badge mono" style={{ ["--badge-color" as never]: color }}>
              <svg width="11" height="11" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
                <path d="M12 3l2 5 5 2-5 2-2 5-2-5-5-2 5-2 2-5z" />
              </svg>
              {speed < 1 ? "Slow-mo" : "Speed up"} · {speed}×
            </div>
          );
        })()}
        </div>
      </main>
      <EditorRail
        tab={railTab}
        onTabChange={setRailTab}
        regionCount={regions.length}
        hasAudio={!!info && info.audio_tracks.length >= 1}
        activeRegion={railActiveRegion}
        audio={
          info && info.audio_tracks.length >= 1 ? (
            <AudioPanel
              tracks={info.audio_tracks}
              mix={editMix}
              onChange={setEffectiveMix}
              trackColors={trackColors}
              onTrackColorsChange={setTrackColors}
              trackNames={trackNames}
              onTrackNamesChange={setTrackNames}
              contextLabel={
                audioEditRegion
                  ? regionDisplayName(audioEditRegion, audioEditRegionIndex)
                  : "Default"
              }
              contextColor={
                audioEditRegion
                  ? resolveRegionColor(audioEditRegion, audioEditRegionIndex)
                  : null
              }
              waveforms={waveforms}
            />
          ) : null
        }
        regions={
          <RegionsPanel
            regions={regions}
            activeId={activeRegionId}
            playheadSecs={currentTime}
            hasSource={!!srcPath && phase.kind === "ready"}
            loopingRegionId={loopingRegionId}
            onFocus={(id) => {
              // null = explicit deselect (re-click on the active row). Don't
              // seek in that case — the user is clearing focus, not asking
              // to jump back to a region they're already inside.
              setActiveRegionId(id);
              if (id == null) return;
              const r = regions.find((x) => x.id === id);
              if (r) seek(r.inSecs);
            }}
            onAddFromPlayhead={addRegionFromPlayhead}
            onRename={setRegionName}
            onDelete={deleteRegion}
            onSetSpeed={setRegionSpeed}
            onSetColor={setRegionColorIndex}
            onToggleLoop={toggleLoopRegion}
            onStartCropEdit={startCropEdit}
          />
        }
        crop={
          <CropPanel
            activeRegion={
              regions.find((r) => r.id === activeRegionId) ??
              (playheadRegion ?? null)
            }
            activeRegionIndex={(() => {
              const r = regions.find((x) => x.id === activeRegionId) ?? playheadRegion;
              return r ? regions.indexOf(r) : null;
            })()}
            sourceWidth={info?.width ?? 0}
            sourceHeight={info?.height ?? 0}
            hasSource={!!srcPath && phase.kind === "ready"}
            regionCount={regions.length}
            onLaunchEditor={(id) => startCropEdit(id)}
            onSetPresetForActive={(crop) => {
              const r = regions.find((x) => x.id === activeRegionId) ?? playheadRegion;
              if (r) setRegionCropDirect(r.id, crop);
            }}
            onApplyToAll={applyCropToAllRegions}
          />
        }
      />
      </div>

      {/* Bottom zone — status strip (consolidated) + transport (always) +
          tool slots (conditional, in fixed-height containers so adding the
          first region or loading a multi-track source doesn't shift earlier
          layout). */}
      <section className="shell-bottom">
      <StatusStrip content={statusContent} />
      <section className="transport-row">
        <div className="time-readout">
          <span className="mono">{fmtTime(currentTime)}</span>
          <span className="dim mono"> / {fmtTime(duration)}</span>
        </div>
        {/* Draft chip — always in the DOM with a reserved slot; toggled via
            visibility so adding/clearing a draft endpoint doesn't shift the
            transport row. Sits next to the time-readout because both belong
            to the "where am I" axis. Active = filled chip; otherwise the
            slot is invisible but holds its space. */}
        <div
          className="draft-chip-slot"
          aria-hidden={!(draftIn != null || draftOut != null)}
        >
          <span
            className={`draft-chip${(draftIn != null || draftOut != null) ? " is-active" : ""}`}
            title="In-progress region; commits when in < out"
            style={{
              visibility: (draftIn != null || draftOut != null) ? "visible" : "hidden",
            }}
          >
            <span className="draft-chip-label">draft</span>
            <span className="draft-chip-value mono">
              {draftIn != null ? fmtTime(draftIn) : "--"} → {draftOut != null ? fmtTime(draftOut) : "--"}
            </span>
          </span>
        </div>

        <div className="transport-buttons">
          <button
            onClick={() => seek(0)}
            title={formatKeybind(keybinds.jumpStart)}
            aria-label="Jump to start"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                 strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <polygon points="19 20 9 12 19 4" />
              <line x1="5" y1="19" x2="5" y2="5" />
            </svg>
          </button>
          <button
            onClick={() => stepFrames(-1)}
            title={formatKeybind(keybinds.frameBack)}
            aria-label="Step back one frame"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                 strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <polyline points="15 18 9 12 15 6" />
            </svg>
          </button>
          <button
            onClick={playPause}
            title={formatKeybind(keybinds.playPause)}
            className="play"
            aria-label={isPlaying ? "Pause" : "Play"}
          >
            {isPlaying ? (
              <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
                <rect x="6" y="4" width="4" height="16" rx="1" />
                <rect x="14" y="4" width="4" height="16" rx="1" />
              </svg>
            ) : (
              <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
                <polygon points="6 4 20 12 6 20 6 4" />
              </svg>
            )}
          </button>
          <button
            onClick={() => stepFrames(1)}
            title={formatKeybind(keybinds.frameForward)}
            aria-label="Step forward one frame"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                 strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <polyline points="9 18 15 12 9 6" />
            </svg>
          </button>
          <button
            onClick={() => seek(duration)}
            title={formatKeybind(keybinds.jumpEnd)}
            aria-label="Jump to end"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                 strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <polygon points="5 4 15 12 5 20 5 4" />
              <line x1="19" y1="5" x2="19" y2="19" />
            </svg>
          </button>
        </div>

      <section
        className="timeline"
        ref={timelineRef}
        onPointerDown={onTimelinePointerDown}
        onPointerMove={onTimelinePointerMove}
        onPointerUp={onTimelinePointerUp}
        onPointerCancel={onTimelinePointerUp}
        onPointerLeave={onTimelinePointerLeave}
      >
        <div className="timeline-inner">
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
              title={`${regionDisplayName(r, i)}: ${fmtTime(r.inSecs)} → ${fmtTime(r.outSecs)} — drag edges to adjust`}
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
        </div>
        {/* Hover tooltip — frame thumbnail + timecode at the cursor position.
            Lives OUTSIDE the .timeline-inner clip box so it can escape the
            rounded-corner mask and float above the row. */}
        {hoverTime != null && hoverPct != null && !isScrubbing && phase.kind === "ready" && (
          <div
            className="timeline-hover-frame"
            style={{ left: `${hoverPct}%` }}
            aria-hidden
          >
            <div className="timeline-hover-frame-thumb">
              <canvas ref={previewCanvasRef} width={160} height={90} />
            </div>
            <div className="timeline-hover-frame-time">{fmtTime(hoverTime)}</div>
          </div>
        )}
      </section>

        <div className="mark-buttons">
          {/* Primary marks — subtly accented so the eye lands here first.
              Set In/Out are by far the most-used buttons in this row. */}
          <button className="mark-primary" onClick={setIn} title={formatKeybind(keybinds.setIn)}>
            Set In
          </button>
          <button className="mark-primary" onClick={setOut} title={formatKeybind(keybinds.setOut)}>
            Set Out
          </button>

          {/* (Draft chip moved into .time-readout — was here, jumped the
              mark-buttons row on every in/out edit.) */}

          {/* Secondary marks — ghost, smaller hit-feel; utility actions. */}
          <button
            className="mark-secondary"
            onClick={() => {
              // Disabled-on-empty so a stray click on a fresh clip does
              // nothing. Confirm at 2+ regions so an accidental click
              // doesn't quietly nuke real work; a single region or a draft
              // alone is cheap enough to skip the prompt.
              if (regions.length >= 2) {
                const ok = window.confirm(
                  `Clear all ${regions.length} regions? This can't be undone (the project autosave will pick up the empty state).`
                );
                if (!ok) return;
              }
              clearAllRegions();
            }}
            disabled={
              regions.length === 0 && draftIn == null && draftOut == null
            }
            title={
              regions.length === 0 && draftIn == null && draftOut == null
                ? "Nothing to clear"
                : regions.length >= 2
                  ? `Remove all ${regions.length} regions and any draft (asks to confirm)`
                  : "Remove all regions and the current draft"
            }
          >
            Clear all
          </button>
          <button
            onClick={saveCurrentFrame}
            disabled={!srcPath || phase.kind !== "ready"}
            className={`mark-secondary${frameAction === "save" ? " btn-armed" : ""}`}
            title={
              frameAction === "save"
                ? "Frame copied to clipboard — click again to save as PNG"
                : `Copy current frame to clipboard (${formatKeybind(keybinds.saveFrame)}). Click again to save as PNG.`
            }
          >
            {frameAction === "save" ? "Save as PNG…" : "Copy frame"}
          </button>
        </div>
      </section>

      <footer className="hints" title="Click any shortcut to edit">
        {/* Each hint and the "edit shortcuts…" link should land on the
            Keyboard tab — the visible intent is "edit this binding,"
            not "open whichever settings tab was last viewed." */}
        {(() => {
          const openKeyboardSettings = () => {
            setSettingsTab("keyboard");
            setKeybindsOpen(true);
          };
          return (
            <>
              <HintKbd onClick={openKeyboardSettings} bind={keybinds.playPause} label="play/pause" />
              <HintKbd onClick={openKeyboardSettings} bind={keybinds.frameBack} secondaryBind={keybinds.frameForward} label="frame" />
              <HintKbd onClick={openKeyboardSettings} bind={keybinds.jumpStart} secondaryBind={keybinds.jumpEnd} label="jump" />
              <HintKbd onClick={openKeyboardSettings} bind={keybinds.setIn} secondaryBind={keybinds.setOut} label="set in/out" />
              <HintKbd onClick={openKeyboardSettings} bind={keybinds.export} label="export" />
              {regions.length > 0 && (
                <HintKbd
                  onClick={openKeyboardSettings}
                  bind={keybinds.jumpRegion1}
                  /* Only surface a secondary keybind when there's a
                     meaningful range; with 1 region "1 / 1" was confusing. */
                  secondaryBind={regions.length > 1
                    ? keybinds[`jumpRegion${Math.min(regions.length, 9)}` as ActionId]
                    : undefined}
                  label="jump to region"
                />
              )}
              <span className="hints-spacer" />
              <button className="hints-edit" onClick={openKeyboardSettings} title="Edit shortcuts">
                edit shortcuts…
              </button>
            </>
          );
        })()}
      </footer>
      </section>{/* /.shell-bottom */}
      </>
      )}

      {exportOpen && (
        <ExportModal
          clips={collectClips()}
          mode={exportMode}
          setMode={setExportMode}
          size={exportSize}
          setSize={setExportSize}
          format={exportFormat}
          setFormat={setExportFormat}
          preserveMultiTrack={exportPreserveTracks}
          setPreserveMultiTrack={setExportPreserveTracks}
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

      {tipsOpen && (
        <TipsModal
          keybinds={keybinds}
          onClose={() => setTipsOpen(false)}
          onOpenKeyboardSettings={() => {
            setSettingsTab("keyboard");
            setKeybindsOpen(true);
          }}
        />
      )}

      {keybindsOpen && (
        <div className="modal-overlay" onMouseDown={(e) => { if (e.target === e.currentTarget) setKeybindsOpen(false); }}>
          <div className="modal modal-tabbed">
            <header className="modal-header">
              <h2>Settings</h2>
              <button className="modal-close" onClick={() => setKeybindsOpen(false)} aria-label="Close">×</button>
            </header>

            <div className="modal-tab-shell">
              <nav className="modal-tab-rail" aria-label="Settings sections">
                {SETTINGS_TABS.map((t) => (
                  <button
                    key={t.id}
                    className={`modal-tab-btn${settingsTab === t.id ? " is-active" : ""}`}
                    onClick={() => setSettingsTab(t.id)}
                    aria-current={settingsTab === t.id ? "page" : undefined}
                  >
                    <SettingsIcon meta={t} />
                    <span>{t.label}</span>
                  </button>
                ))}
                <span className="modal-tab-rail-spacer" />
                <span className="modal-tab-rail-version mono" aria-hidden>
                  v{APP_VERSION}
                </span>
              </nav>

              <div className="modal-tab-content">
                {settingsTab === "replay" && <ReplaySettings />}
                {settingsTab === "keyboard" && (
                  <KeyboardSettingsTab
                    keybinds={keybinds}
                    listeningAction={listeningAction}
                    setListeningAction={setListeningAction}
                  />
                )}
                {settingsTab === "storage" && <StorageSettingsTab />}
                {settingsTab === "about" && (
                  <AboutTab
                    updater={updater.state}
                    onCheckUpdates={() => void updater.checkNow()}
                    onInstallUpdate={() => void updater.installNow()}
                  />
                )}
              </div>
            </div>

            <footer className="modal-footer">
              {settingsTab === "keyboard" ? (
                <button
                  onClick={() => {
                    setKeybinds(DEFAULT_KEYBINDS);
                    // The keybinds-driven useEffect re-registers the global
                    // hotkey on saveReplay reference change, but if the user
                    // never customized saveReplay the reference doesn't change
                    // and the OS would keep whatever it has. Push it explicitly
                    // so a Reset always restores the default Alt+F10 binding.
                    const s = keybindToShortcutString(DEFAULT_KEYBINDS.saveReplay);
                    if (s) {
                      invoke("replay_set_save_hotkey", { shortcut: s }).catch((e) =>
                        logErr("replay_set_save_hotkey", e)
                      );
                    }
                  }}
                >
                  Reset to defaults
                </button>
              ) : (
                <span /> /* spacer — keeps Done right-aligned */
              )}
              <button className="primary" onClick={() => setKeybindsOpen(false)}>Done</button>
            </footer>
          </div>
        </div>
      )}
    </div>
  );
}

