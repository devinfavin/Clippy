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

/// Draw waveforms onto a canvas, redrawing on container resize.
///
/// `mode: "tracks"` (default): per-track waveforms layered with additive
/// blend, each track in its palette color. Used by surfaces that benefit
/// from seeing individual tracks (e.g. the per-track Audio panel rows).
///
/// `mode: "mixed"`: one neutral-toned waveform that's the max-envelope of
/// all unmuted tracks. Used by the main timeline so the colored per-track
/// stripes don't fight the region bands / playhead for visual attention.
export function useWaveformDraw(args: {
  canvasRef: React.RefObject<HTMLCanvasElement | null>;
  containerRef: React.RefObject<HTMLElement | null>;
  waveforms: Map<number, Float32Array>;
  regions: Region[];
  trackMix: TrackMix;
  trackColors: TrackColorOverrides;
  duration: number;
  mode?: "tracks" | "mixed";
}): void {
  const { canvasRef, containerRef, waveforms, regions, trackMix, trackColors, duration } = args;
  const mode = args.mode ?? "tracks";
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

      // Mixed-mode: render a single neutral envelope = max over all unmuted
      // tracks at each x, weighted by their volume. Skipping the per-region
      // override scan keeps it cheap; the rail's Audio panel still shows the
      // per-track detail. Used by the main timeline.
      //
      // Sized up 2026-05-30: alpha 0.32 → 0.55 so the envelope reads through
      // the region-band tints; bar width 1 → 2 px (with a 1 px gap) so peaks
      // look like a waveform, not a noise field; unityBar 0.55 → 0.65 × H so
      // loud audio fills more of the timeline strip.
      if (mode === "mixed") {
        const maxBar = cssH * 0.92;
        const unityBar = cssH * 0.65;
        // Lift the longest track so envelope bin counts are consistent.
        let longest = 0;
        for (const bins of waveforms.values()) {
          if (bins.length > longest) longest = bins.length;
        }
        if (longest === 0) return;
        ctx.fillStyle = "rgba(190, 195, 208, 0.55)";
        const barW = 2;
        const stride = 3; // bar + 1 px gap
        for (let x = 0; x < cssW; x += stride) {
          let envelope = 0;
          for (const [idx, bins] of waveforms.entries()) {
            const m = trackMix[idx];
            if (m?.muted) continue;
            const vol = m?.volume ?? 1;
            if (vol <= 0) continue;
            const N = bins.length;
            if (N === 0) continue;
            const binStart = Math.floor((x / cssW) * N);
            const binEnd = Math.max(binStart + 1, Math.floor(((x + stride) / cssW) * N));
            let max = 0;
            for (let i = binStart; i < binEnd && i < N; i++) {
              const v = bins[i];
              if (v > max) max = v;
            }
            const scaled = max * vol;
            if (scaled > envelope) envelope = scaled;
          }
          const h = Math.min(envelope * unityBar, maxBar);
          if (h < 1) continue;
          ctx.fillRect(x, mid - h / 2, barW, h);
        }
        return;
      }
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
      //
      // 2026-05-30: bar width 1 → 2 px with a 1 px gap and unityBar 0.55 →
      // 0.65 × cssH (same sizing as the mixed mode below) so the colored
      // envelope reads as a proper waveform, not a noise field — and the
      // keyframe ticks underneath stay visible in the gaps between bars.
      const maxBar = cssH * 0.92; // hard cap (canvas-bound)
      const unityBar = cssH * 0.65; // bar height at volume == 1.0
      const barW = 2;
      const stride = 3; // bar + 1 px gap

      ctx.globalCompositeOperation = "source-over";
      ctx.fillStyle = "rgba(140, 144, 154, 0.22)";
      for (const [idx, bins] of entries) {
        const N = bins.length;
        if (N === 0) continue;
        for (let x = 0; x < cssW; x += stride) {
          if (!xMixes[x][idx]?.muted) continue;
          const binStart = Math.floor((x / cssW) * N);
          const binEnd = Math.max(binStart + 1, Math.floor(((x + stride) / cssW) * N));
          let max = 0;
          for (let i = binStart; i < binEnd && i < N; i++) {
            const v = bins[i];
            if (v > max) max = v;
          }
          const h = Math.min(max * unityBar, maxBar);
          if (h < 1) continue;
          ctx.fillRect(x, mid - h / 2, barW, h);
        }
      }

      // Average active-count for alpha — heuristic: count active in the
      // middle x to avoid scanning all xMixes again.
      const midMix = xMixes[Math.floor(cssW / 2)] ?? trackMix;
      const activeCount = entries.filter(([i]) => !midMix[i]?.muted).length;
      const baseAlpha = activeCount <= 1 ? 0.9 : 0.65;
      ctx.globalCompositeOperation = "lighter";
      for (const [idx, bins] of entries) {
        const N = bins.length;
        if (N === 0) continue;
        ctx.fillStyle = hexWithAlpha(resolveTrackColor(idx, trackColors), baseAlpha);
        for (let x = 0; x < cssW; x += stride) {
          const m = xMixes[x][idx];
          if (m?.muted) continue;
          const vol = m?.volume ?? 1;
          if (vol <= 0) continue;
          const binStart = Math.floor((x / cssW) * N);
          const binEnd = Math.max(binStart + 1, Math.floor(((x + stride) / cssW) * N));
          let max = 0;
          for (let i = binStart; i < binEnd && i < N; i++) {
            const v = bins[i];
            if (v > max) max = v;
          }
          const h = Math.min(max * vol * unityBar, maxBar);
          if (h < 1) continue;
          ctx.fillRect(x, mid - h / 2, barW, h);
        }
      }
      ctx.globalCompositeOperation = "source-over";
    };

    draw();
    const ro = new ResizeObserver(draw);
    ro.observe(container);
    return () => ro.disconnect();
  }, [waveforms, trackMix, regions, duration, trackColors, canvasRef, containerRef, mode]);
}
