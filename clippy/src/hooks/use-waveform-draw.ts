import { useEffect } from "react";
import { resolveTrackColor } from "../types";
import type { Region, TrackColorOverrides, TrackMix } from "../types";

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

/// Draw all per-track waveforms layered onto the canvas (and redraw on
/// container resize). Each track uses its palette color; additive blend so
/// simultaneously-loud tracks brighten where they overlap. Drawing centered
/// on the mid-line — bar heights = max amplitude in that x's bin slice.
export function useWaveformDraw(args: {
  canvasRef: React.RefObject<HTMLCanvasElement | null>;
  containerRef: React.RefObject<HTMLElement | null>;
  waveforms: Map<number, Float32Array>;
  regions: Region[];
  trackMix: TrackMix;
  trackColors: TrackColorOverrides;
  duration: number;
}): void {
  const { canvasRef, containerRef, waveforms, regions, trackMix, trackColors, duration } = args;
  useEffect(() => {
    const canvas = canvasRef.current;
    const container = containerRef.current;
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
  }, [waveforms, trackMix, regions, duration, trackColors, canvasRef, containerRef]);
}
