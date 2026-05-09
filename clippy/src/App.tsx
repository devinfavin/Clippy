import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  newRegionId,
  SIZE_PRESETS,
  type ExportMode,
  type ExportProgress,
  type Phase,
  type ProxyProgress,
  type ProxyResult,
  type Region,
  type SizeLimit,
  type VideoInfo,
} from "./types";
import { fmtTime, fmtEta } from "./formatters";
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
import { ExportToast } from "./ExportToast";
import "./App.css";

// Per-region hue offsets so 3+ regions are visually distinguishable on the
// timeline. All hues live in the green/teal/lime range so the bands still read
// as "selection / keep" rather than being a rainbow.
const REGION_HUES = [142, 132, 152, 122, 162, 138, 148, 128, 158];

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

  // Export modal state
  const [exportOpen, setExportOpen] = useState(false);
  const [exportSize, setExportSize] = useState<SizeLimit>(SIZE_PRESETS[0]);
  const [exportMode, setExportMode] = useState<ExportMode>("separate");
  // Post-export toast: list of files just produced.
  const [lastExport, setLastExport] = useState<{ paths: string[] } | null>(null);

  // Waveform: bins of peak amplitude per timeline slice (0..1).
  const [waveform, setWaveform] = useState<Float32Array | null>(null);
  const waveformIdRef = useRef(0);
  const waveCanvasRef = useRef<HTMLCanvasElement | null>(null);

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
      setWaveform(null);

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
      setPhase({ kind: "probing" });

      const probed = await invoke<VideoInfo>("probe_video", { path: selected });
      setInfo(probed);

      setPhase({ kind: "proxying", progress: 0, eta: null });
      const result = await invoke<ProxyResult>("generate_proxy", {
        path: selected,
        durationSecs: probed.duration_secs,
      });
      setProxyPath(result.play_path);
      setProxyEncoder(result.strategy);
      setPhase({ kind: "ready" });

      // Kick off waveform extraction in the background (don't block ready state).
      const waveId = ++waveformIdRef.current;
      invoke<number[]>("extract_waveform", { path: selected })
        .then((bins) => {
          if (waveId !== waveformIdRef.current) return;
          setWaveform(new Float32Array(bins));
        })
        .catch((err) => {
          if (waveId !== waveformIdRef.current) return;
          console.error("[clippy] waveform extract failed:", err);
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

  const seek = useCallback(
    (t: number) => {
      const v = videoRef.current;
      if (!v) return;
      const clamped = Math.max(0, Math.min(duration, t));
      v.currentTime = clamped;
      pendingSeekRef.current = null;
      setCurrentTime(clamped);
    },
    [duration]
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
    },
    [duration]
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

  // Set draft in. If draft out is already set later than t, auto-commit a region.
  const setIn = useCallback(() => {
    const t = videoRef.current?.currentTime ?? 0;
    if (draftOut != null && t < draftOut) {
      setRegions((r) => [...r, { id: newRegionId(), inSecs: t, outSecs: draftOut }]
        .sort((a, b) => a.inSecs - b.inSecs));
      setDraftIn(null);
      setDraftOut(null);
    } else {
      setDraftIn(t);
      // If the existing draft out is now <= the new in, drop it.
      if (draftOut != null && t >= draftOut) setDraftOut(null);
    }
  }, [draftOut]);

  // Set draft out. If draft in is already set earlier than t, auto-commit a region.
  const setOut = useCallback(() => {
    const t = videoRef.current?.currentTime ?? 0;
    if (draftIn != null && t > draftIn) {
      setRegions((r) => [...r, { id: newRegionId(), inSecs: draftIn, outSecs: t }]
        .sort((a, b) => a.inSecs - b.inSecs));
      setDraftIn(null);
      setDraftOut(null);
    } else {
      setDraftOut(t);
      if (draftIn != null && t <= draftIn) setDraftIn(null);
    }
  }, [draftIn]);

  const deleteRegion = useCallback((id: string) => {
    setRegions((r) => r.filter((x) => x.id !== id));
  }, []);

  const clearAllRegions = useCallback(() => {
    setRegions([]);
    setDraftIn(null);
    setDraftOut(null);
  }, []);

  // Region edge dragging: track which region+edge is currently being resized.
  // Using state for the cursor styling, ref for the move handler closure.
  const [resizingEdge, setResizingEdge] = useState<{ regionId: string; edge: "in" | "out" } | null>(null);
  const resizingEdgeRef = useRef<{ regionId: string; edge: "in" | "out" } | null>(null);

  // Loop playback: while non-null, the timeupdate handler wraps the playhead
  // back to that region's in-point when it reaches the out-point.
  const [loopingRegionId, setLoopingRegionId] = useState<string | null>(null);
  const loopingRegionIdRef = useRef<string | null>(null);
  const regionsRef = useRef<Region[]>([]);
  useEffect(() => { regionsRef.current = regions; }, [regions]);

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
  // Build the list of [in, out] pairs that should be exported. Uses committed
  // regions when present, otherwise falls back to the draft pair.
  const collectClips = useCallback((): Array<{ inSecs: number; outSecs: number }> => {
    if (regions.length > 0) {
      return regions.map((r) => ({ inSecs: r.inSecs, outSecs: r.outSecs }));
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
    const sized = exportSize.kind === "mb";

    if (isStitched) {
      const suggested = sized
        ? `${baseName}_stitched_${exportSize.mb}mb.mp4`
        : `${baseName}_stitched.mp4`;
      const dest = await saveDialog({
        defaultPath: suggested,
        filters: [{ name: "MP4", extensions: ["mp4"] }],
      });
      if (!dest) return;

      setPhase({ kind: "exporting", progress: 0, current: 1, total: 1 });
      try {
        if (sized) {
          await invoke("export_concat_sized", {
            srcPath,
            regions: clips.map((c) => [c.inSecs, c.outSecs]),
            outputPath: dest,
            targetSizeMb: exportSize.mb,
          });
        } else {
          await invoke("export_concat", {
            srcPath,
            regions: clips.map((c) => [c.inSecs, c.outSecs]),
            outputPath: dest,
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
    const suggested =
      clips.length === 1
        ? `${baseName}_clip_${Math.round(clips[0].inSecs)}-${Math.round(clips[0].outSecs)}${sized ? `_${exportSize.mb}mb` : ""}.mp4`
        : `${baseName}_clip_1${sized ? `_${exportSize.mb}mb` : ""}.mp4`;
    const firstDest = await saveDialog({
      defaultPath: suggested,
      filters: [{ name: "MP4", extensions: ["mp4"] }],
    });
    if (!firstDest) return;

    let outputPaths: string[];
    if (clips.length === 1) {
      outputPaths = [firstDest];
    } else {
      const m = firstDest.match(/^(.*?)(?:_\d+)?\.mp4$/i);
      const base = m ? m[1] : firstDest.replace(/\.mp4$/i, "");
      outputPaths = clips.map((_, idx) => `${base}_${idx + 1}.mp4`);
    }

    try {
      for (let i = 0; i < clips.length; i++) {
        setPhase({ kind: "exporting", progress: 0, current: i + 1, total: clips.length });
        if (sized) {
          await invoke("export_clip_sized", {
            srcPath,
            inSecs: clips[i].inSecs,
            outSecs: clips[i].outSecs,
            outputPath: outputPaths[i],
            targetSizeMb: exportSize.mb,
          });
        } else {
          await invoke("export_clip", {
            srcPath,
            inSecs: clips[i].inSecs,
            outSecs: clips[i].outSecs,
            outputPath: outputPaths[i],
          });
        }
      }
      setPhase({ kind: "ready" });
      setLastExport({ paths: outputPaths });
    } catch (e) {
      setPhase({ kind: "error", message: String(e) });
    }
  }, [srcPath, collectClips, exportMode, exportSize]);

  // Alias for the keybind dispatcher.
  const handleExport = openExport;

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
  }, [keybinds, listeningAction, phase, handleOpen, playPause, stepFrames, seek, duration, setIn, setOut, handleExport, toggleLoopRegion, regions]);

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
    const t = timeFromPointer(e.clientX);
    scrubTo(t);
  };
  const onTimelinePointerUp = (e: React.PointerEvent) => {
    if (!isScrubbing) return;
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    setIsScrubbing(false);
    const v = videoRef.current;
    if (v) v.muted = wasMutedRef.current;
    // Apply final position, then restore play state if it was playing before scrub.
    const t = timeFromPointer(e.clientX);
    seek(t);
    if (v && wasPlayingRef.current) {
      v.play().catch(() => {});
    }
  };

  // Draw waveform onto the canvas (and redraw on container resize).
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
      if (!waveform || waveform.length === 0) return;

      const mid = cssH / 2;
      const N = waveform.length;
      // Background fill (subtle); use accent color at low alpha
      ctx.fillStyle = "rgba(79, 157, 255, 0.42)";

      // 1px-per-x bars; for each x take the max bin amplitude in its slice
      for (let x = 0; x < cssW; x++) {
        const binStart = Math.floor((x / cssW) * N);
        const binEnd = Math.max(binStart + 1, Math.floor(((x + 1) / cssW) * N));
        let max = 0;
        for (let i = binStart; i < binEnd && i < N; i++) {
          const v = waveform[i];
          if (v > max) max = v;
        }
        const h = max * (cssH * 0.85);
        if (h < 1) continue;
        ctx.fillRect(x, mid - h / 2, 1, h);
      }
    };

    draw();
    const ro = new ResizeObserver(draw);
    ro.observe(container);
    return () => ro.disconnect();
  }, [waveform]);

  const playheadPct = duration > 0 ? (currentTime / duration) * 100 : 0;
  const draftInPct = draftIn != null && duration > 0 ? (draftIn / duration) * 100 : null;
  const draftOutPct = draftOut != null && duration > 0 ? (draftOut / duration) * 100 : null;

  const exportableCount =
    regions.length > 0
      ? regions.length
      : draftIn != null && draftOut != null && draftIn < draftOut
      ? 1
      : 0;

  const phaseBanner = (() => {
    switch (phase.kind) {
      case "idle":
        return null;
      case "probing":
        return <Banner label="Reading file metadata…" />;
      case "proxying":
        return (
          <Banner
            label={`Preparing source… ${(phase.progress * 100).toFixed(0)}%  ·  ETA ${fmtEta(phase.eta)}`}
            progress={phase.progress}
          />
        );
      case "exporting":
        return (
          <Banner
            label={
              phase.total > 1
                ? `Exporting clip ${phase.current}/${phase.total}… ${(phase.progress * 100).toFixed(0)}%`
                : `Exporting clip (stream copy)… ${(phase.progress * 100).toFixed(0)}%`
            }
            progress={phase.progress}
          />
        );
      case "error":
        return <Banner label={`Error: ${phase.message}`} error />;
      case "ready":
        return null;
    }
  })();

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand" title="Clippy">
          <div className="brand-mark" aria-hidden />
          <span className="brand-name">Clippy</span>
        </div>
        <button className="btn" onClick={handleOpen} title={formatKeybind(keybinds.openFile)}>
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
          className="btn primary"
          onClick={openExport}
          disabled={phase.kind !== "ready" || !srcPath || exportableCount === 0}
          title={formatKeybind(keybinds.export)}
        >
          {exportableCount > 1 ? `Export ${exportableCount} regions…` : "Export…"}
        </button>
      </header>

      {phaseBanner}

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
          <span className="dim mono draft-readout">
            draft: {draftIn != null ? fmtTime(draftIn) : "--"} → {draftOut != null ? fmtTime(draftOut) : "--"}
          </span>
        </div>
      </section>

      {regions.length > 0 && (
        <section className="region-list">
          {regions.map((r, i) => (
            <span
              key={r.id}
              className={`region-chip${loopingRegionId === r.id ? " looping" : ""}`}
              onClick={() => seek(r.inSecs)}
              title="Click to jump playhead to this region's in-point"
              style={{ ["--region-hue" as any]: REGION_HUES[i % REGION_HUES.length] }}
            >
              <span className="region-chip-num">{i + 1}</span>
              <span className="region-chip-times mono">
                {fmtTime(r.inSecs)} → {fmtTime(r.outSecs)}
              </span>
              <span className="region-chip-len mono dim">
                ({fmtTime(r.outSecs - r.inSecs)})
              </span>
              <button
                className={`region-chip-loop-btn${loopingRegionId === r.id ? " active" : ""}`}
                onClick={(e) => { e.stopPropagation(); toggleLoopRegion(r.id); }}
                title={loopingRegionId === r.id ? "Stop looping" : "Loop this region"}
              >
                ↻
              </button>
              <button
                className="region-chip-x"
                onClick={(e) => { e.stopPropagation(); deleteRegion(r.id); }}
                title="Delete region"
              >
                ×
              </button>
            </span>
          ))}
        </section>
      )}

      <section
        className="timeline"
        ref={timelineRef}
        onPointerDown={onTimelinePointerDown}
        onPointerMove={onTimelinePointerMove}
        onPointerUp={onTimelinePointerUp}
        onPointerCancel={onTimelinePointerUp}
      >
        <canvas ref={waveCanvasRef} className="waveform" />
        {/* committed regions (one band per region; edges are draggable) */}
        {regions.map((r, i) => {
          if (duration <= 0) return null;
          const left = (r.inSecs / duration) * 100;
          const width = Math.max(0, ((r.outSecs - r.inSecs) / duration) * 100);
          const isResizingThis = resizingEdge?.regionId === r.id;
          const hue = REGION_HUES[i % REGION_HUES.length];
          return (
            <div
              key={r.id}
              className="region-band"
              style={{ left: `${left}%`, width: `${width}%`, ["--region-hue" as any]: hue }}
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

      {exportOpen && (
        <ExportModal
          clips={collectClips()}
          mode={exportMode}
          setMode={setExportMode}
          size={exportSize}
          setSize={setExportSize}
          sourceBitrateBps={info?.bit_rate_bps ?? null}
          onCancel={() => setExportOpen(false)}
          onConfirm={runExport}
        />
      )}

      {lastExport && (
        <ExportToast paths={lastExport.paths} onClose={() => setLastExport(null)} />
      )}

      {isDraggingFile && (
        <div className="drop-overlay">
          <div className="drop-message">
            <div className="drop-icon">⬇</div>
            <div>Drop to open</div>
          </div>
        </div>
      )}

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

function Banner(props: { label: string; progress?: number; error?: boolean }) {
  return (
    <div className={`banner${props.error ? " error" : ""}`}>
      <div className="banner-label">{props.label}</div>
      {typeof props.progress === "number" && (
        <div className="banner-bar">
          <div
            className="banner-fill"
            style={{ width: `${(props.progress * 100).toFixed(1)}%` }}
          />
        </div>
      )}
    </div>
  );
}
