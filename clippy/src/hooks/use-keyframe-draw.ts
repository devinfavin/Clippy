import { useEffect } from "react";

/// Draw keyframe ticks. Faint vertical hairlines at every video keyframe so
/// the user can see where stream-copy cuts will snap. Sits below the wave
/// canvas in the stacking order — the waveform additively blends on top so
/// these ticks don't compete visually during playback.
export function useKeyframeDraw(args: {
  canvasRef: React.RefObject<HTMLCanvasElement | null>;
  containerRef: React.RefObject<HTMLElement | null>;
  keyframes: Float32Array | null;
  duration: number;
}): void {
  const { canvasRef, containerRef, keyframes, duration } = args;
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
  }, [keyframes, duration, canvasRef, containerRef]);
}
