import { useEffect, useRef, useState } from "react";
import { ASPECT_PRESETS, type AspectLock, type Crop } from "./types";
import { fittedVideoRect } from "./cropGeom";

/** Returns the numeric w/h ratio of an AspectLock, or null if free. */
function aspectRatio(lock: AspectLock, srcW: number, srcH: number): number | null {
  if (lock.kind === "free") return null;
  if (lock.kind === "source") return srcW > 0 && srcH > 0 ? srcW / srcH : null;
  return lock.w / lock.h;
}

/** Tighten {x,y,w,h} to enforce a target aspect ratio while staying inside the
 *  source. We resize toward the smallest of (current_w, current_h * ratio)
 *  so the box always shrinks rather than growing past edges, then re-anchor. */
function snapToAspect(
  c: Crop,
  ratio: number,
  srcW: number,
  srcH: number,
  anchor: "tl" | "br" | "center" = "center"
): Crop {
  if (ratio <= 0) return c;
  // Try matching the current width; derive height; if that overflows, swap.
  let w = c.w;
  let h = Math.round(w / ratio);
  if (h > srcH || h < 32) {
    h = c.h;
    w = Math.round(h * ratio);
  }
  if (w > srcW) {
    w = srcW;
    h = Math.round(w / ratio);
  }
  if (h > srcH) {
    h = srcH;
    w = Math.round(h * ratio);
  }
  let x = c.x;
  let y = c.y;
  if (anchor === "center") {
    x = Math.round(c.x + (c.w - w) / 2);
    y = Math.round(c.y + (c.h - h) / 2);
  } else if (anchor === "br") {
    x = c.x + c.w - w;
    y = c.y + c.h - h;
  }
  x = Math.max(0, Math.min(srcW - w, x));
  y = Math.max(0, Math.min(srcH - h, y));
  return { x, y, w, h };
}

type DragMode =
  | { kind: "none" }
  | { kind: "move"; startMouse: { x: number; y: number }; startCrop: Crop }
  | { kind: "resize"; edge: ResizeEdge; startMouse: { x: number; y: number }; startCrop: Crop };

type ResizeEdge = "n" | "s" | "e" | "w" | "ne" | "nw" | "se" | "sw";

const HANDLES: ResizeEdge[] = ["nw", "n", "ne", "e", "se", "s", "sw", "w"];

/**
 * Floating overlay over the player that lets the user draw a crop rectangle
 * in source-pixel coordinates. The video element behind keeps showing the full
 * frame; the overlay just darkens the area being CUT and shows a clear KEEP
 * rectangle with corner/edge handles.
 *
 * Coords flow: mouse drags happen in display space; we always normalize to
 * source pixels before storing, and translate source pixels back to display
 * for rendering. That way a rendered window resize (or a different video
 * source) doesn't invalidate the stored crop.
 */
export function CropOverlay(props: {
  videoElement: HTMLVideoElement | null;
  sourceWidth: number;
  sourceHeight: number;
  initialCrop: Crop | undefined;
  // Total region count, used to decide whether to show "Apply to all".
  // 1 region (or undefined) → button hidden; 2+ → shown.
  totalRegions?: number;
  onDone: (crop: Crop | undefined) => void;
  // Apply this crop to every region in one click. Only meaningful with 2+ regions;
  // exists to make stitched exports trivially possible without per-region matching.
  onApplyToAll?: (crop: Crop) => void;
  onCancel: () => void;
}) {
  const { sourceWidth: srcW, sourceHeight: srcH } = props;

  // Track the player's current bounding rect so we can render at the right
  // position even if the user resizes the window mid-edit.
  const [bounds, setBounds] = useState<DOMRect | null>(null);
  useEffect(() => {
    const v = props.videoElement;
    if (!v) return;
    const update = () => setBounds(v.getBoundingClientRect());
    update();
    const ro = new ResizeObserver(update);
    ro.observe(v);
    window.addEventListener("scroll", update, true);
    window.addEventListener("resize", update);
    return () => {
      ro.disconnect();
      window.removeEventListener("scroll", update, true);
      window.removeEventListener("resize", update);
    };
  }, [props.videoElement]);

  // Crop in source-pixel space. Init to existing or full frame.
  const [crop, setCrop] = useState<Crop>(
    props.initialCrop ?? { x: 0, y: 0, w: srcW, h: srcH }
  );
  const cropRef = useRef(crop);
  useEffect(() => { cropRef.current = crop; }, [crop]);

  // Aspect ratio lock. "free" by default; switching to a preset re-snaps the
  // current crop to that ratio (centered) and constrains all subsequent drags.
  const [aspect, setAspect] = useState<AspectLock>({ kind: "free" });
  const aspectRef = useRef(aspect);
  useEffect(() => { aspectRef.current = aspect; }, [aspect]);
  const applyAspect = (lock: AspectLock) => {
    setAspect(lock);
    const r = aspectRatio(lock, srcW, srcH);
    if (r != null) {
      setCrop((c) => snapToAspect(c, r, srcW, srcH, "center"));
    }
  };

  const dragRef = useRef<DragMode>({ kind: "none" });

  // Rendered video area inside the player element.
  const fitted =
    bounds && srcW > 0 && srcH > 0
      ? fittedVideoRect(bounds.width, bounds.height, srcW, srcH)
      : null;

  // source px → element px
  const srcToElem = (sx: number, sy: number) => {
    if (!fitted) return { x: 0, y: 0 };
    return {
      x: fitted.x + (sx / srcW) * fitted.w,
      y: fitted.y + (sy / srcH) * fitted.h,
    };
  };
  const startMove = (e: React.PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dragRef.current = {
      kind: "move",
      startMouse: { x: e.clientX, y: e.clientY },
      startCrop: { ...cropRef.current },
    };
  };

  const startResize = (edge: ResizeEdge) => (e: React.PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dragRef.current = {
      kind: "resize",
      edge,
      startMouse: { x: e.clientX, y: e.clientY },
      startCrop: { ...cropRef.current },
    };
  };

  const onMove = (e: React.PointerEvent) => {
    const drag = dragRef.current;
    if (drag.kind === "none") return;
    e.preventDefault();
    if (!fitted) return;
    // Mouse delta in source pixels.
    const dxSrc = ((e.clientX - drag.startMouse.x) / fitted.w) * srcW;
    const dySrc = ((e.clientY - drag.startMouse.y) / fitted.h) * srcH;
    if (drag.kind === "move") {
      const c = drag.startCrop;
      const nx = Math.max(0, Math.min(srcW - c.w, Math.round(c.x + dxSrc)));
      const ny = Math.max(0, Math.min(srcH - c.h, Math.round(c.y + dySrc)));
      setCrop({ x: nx, y: ny, w: c.w, h: c.h });
    } else {
      const c = drag.startCrop;
      const MIN = 32; // never let a side collapse below 32 px
      let { x, y, w, h } = c;
      if (drag.edge.includes("n")) {
        const ny = Math.max(0, Math.min(c.y + c.h - MIN, Math.round(c.y + dySrc)));
        h = c.h + (c.y - ny);
        y = ny;
      }
      if (drag.edge.includes("s")) {
        const nh = Math.max(MIN, Math.min(srcH - c.y, Math.round(c.h + dySrc)));
        h = nh;
      }
      if (drag.edge.includes("w")) {
        const nx = Math.max(0, Math.min(c.x + c.w - MIN, Math.round(c.x + dxSrc)));
        w = c.w + (c.x - nx);
        x = nx;
      }
      if (drag.edge.includes("e")) {
        const nw = Math.max(MIN, Math.min(srcW - c.x, Math.round(c.w + dxSrc)));
        w = nw;
      }
      // Aspect lock: anchor the side opposite the drag edge so the user feels
      // they're pulling toward themselves; snap dimensions to the locked ratio.
      const r = aspectRatio(aspectRef.current, srcW, srcH);
      if (r != null) {
        const anchor: "tl" | "br" | "center" =
          drag.edge.includes("n") || drag.edge.includes("w") ? "br" : "tl";
        const snapped = snapToAspect({ x, y, w, h }, r, srcW, srcH, anchor);
        x = snapped.x; y = snapped.y; w = snapped.w; h = snapped.h;
      }
      setCrop({ x, y, w, h });
    }
  };

  const onUp = (e: React.PointerEvent) => {
    if (dragRef.current.kind === "none") return;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {}
    dragRef.current = { kind: "none" };
  };

  // Esc cancels, Enter applies. Enter routes to Apply-to-all when there are
  // multiple regions — matches the visually-primary button.
  useEffect(() => {
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        ev.preventDefault();
        ev.stopPropagation();
        props.onCancel();
      } else if (ev.key === "Enter" && !ev.ctrlKey && !ev.shiftKey && !ev.altKey) {
        ev.preventDefault();
        const c = cropRef.current;
        const isFull = c.x === 0 && c.y === 0 && c.w === srcW && c.h === srcH;
        if ((props.totalRegions ?? 1) > 1 && props.onApplyToAll && !isFull) {
          props.onApplyToAll(c);
        } else {
          props.onDone(isFull ? undefined : c);
        }
      }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () =>
      window.removeEventListener("keydown", onKey, { capture: true } as AddEventListenerOptions);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  if (!bounds || !fitted) return null;

  // Crop rectangle in element-pixel space.
  const tl = srcToElem(crop.x, crop.y);
  const br = srcToElem(crop.x + crop.w, crop.y + crop.h);
  const rectStyle: React.CSSProperties = {
    position: "absolute",
    left: tl.x,
    top: tl.y,
    width: br.x - tl.x,
    height: br.y - tl.y,
  };

  // Position toolbar above the rectangle if there's room, else below.
  const toolbarTop = tl.y > 56 ? tl.y - 48 : br.y + 12;

  const isFullFrame =
    crop.x === 0 && crop.y === 0 && crop.w === srcW && crop.h === srcH;

  return (
    <div
      className="crop-overlay"
      style={{
        position: "fixed",
        left: bounds.left,
        top: bounds.top,
        width: bounds.width,
        height: bounds.height,
        pointerEvents: "auto",
      }}
      onPointerMove={onMove}
      onPointerUp={onUp}
      onPointerCancel={onUp}
    >
      {/* Darken the whole player area; the keep-rect punches a clear hole. */}
      <div
        className="crop-shade"
        style={{
          position: "absolute",
          inset: 0,
          background: "rgba(0,0,0,0.55)",
          clipPath: `polygon(
            0 0, 100% 0, 100% 100%, 0 100%, 0 0,
            ${tl.x}px ${tl.y}px,
            ${tl.x}px ${br.y}px,
            ${br.x}px ${br.y}px,
            ${br.x}px ${tl.y}px,
            ${tl.x}px ${tl.y}px
          )`,
        }}
      />
      {/* Keep rectangle with movable inner area + resize handles on edges. */}
      <div className="crop-rect" style={rectStyle}>
        <div className="crop-rect-inner" onPointerDown={startMove} />
        {HANDLES.map((edge) => (
          <div
            key={edge}
            className={`crop-handle crop-handle-${edge}`}
            onPointerDown={startResize(edge)}
          />
        ))}
        {/* Rule-of-thirds grid */}
        <div className="crop-grid">
          <div className="crop-grid-line v" style={{ left: "33.33%" }} />
          <div className="crop-grid-line v" style={{ left: "66.66%" }} />
          <div className="crop-grid-line h" style={{ top: "33.33%" }} />
          <div className="crop-grid-line h" style={{ top: "66.66%" }} />
        </div>
        {/* Dim badge */}
        <div className="crop-dim-badge mono">
          {crop.w}×{crop.h}
        </div>
      </div>
      {/* Toolbar */}
      <div
        className="crop-toolbar"
        style={{ position: "absolute", left: tl.x, top: toolbarTop }}
        onPointerDown={(e) => e.stopPropagation()}
      >
        <span className="crop-toolbar-label mono">
          {isFullFrame ? "no crop" : `crop ${crop.w}×${crop.h} @ ${crop.x},${crop.y}`}
        </span>
        <span className="crop-aspect-row" title="Lock aspect ratio">
          {ASPECT_PRESETS.map((preset, i) => {
            const active =
              preset.kind === aspect.kind &&
              (preset.kind !== "ratio" ||
                (aspect.kind === "ratio" && preset.w === aspect.w && preset.h === aspect.h));
            const label =
              preset.kind === "free"
                ? "Free"
                : preset.kind === "source"
                ? "Source"
                : preset.label;
            return (
              <button
                key={i}
                className={`crop-aspect-btn${active ? " active" : ""}`}
                onClick={() => applyAspect(preset)}
              >
                {label}
              </button>
            );
          })}
        </span>
        <button
          className="crop-toolbar-btn"
          onClick={() => props.onDone(undefined)}
          title="Remove crop from this region"
        >
          Remove crop
        </button>
        <button className="crop-toolbar-btn" onClick={props.onCancel} title="Esc">
          Cancel
        </button>
        {(props.totalRegions ?? 1) > 1 && props.onApplyToAll && !isFullFrame ? (
          // 2+ regions: surface both scopes side-by-side; apply-to-all is primary
          // because the common case is "I want this same crop everywhere".
          <>
            <button
              className="crop-toolbar-btn"
              onClick={() => props.onDone(cropRef.current)}
              title="Apply this crop only to this region"
            >
              Apply to this region
            </button>
            <button
              className="crop-toolbar-btn primary"
              onClick={() => props.onApplyToAll!(cropRef.current)}
              title="Set this exact crop on every region (Enter)"
            >
              Apply to all
            </button>
          </>
        ) : (
          <button
            className="crop-toolbar-btn primary"
            onClick={() => props.onDone(isFullFrame ? undefined : cropRef.current)}
            title="Enter"
          >
            Done
          </button>
        )}
      </div>
    </div>
  );
}
