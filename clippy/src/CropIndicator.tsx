import { useEffect, useState } from "react";
import type { Crop } from "./types";
import { fittedVideoRect } from "./cropGeom";

/**
 * Read-only overlay that marks the crop bounds on the player with corner
 * brackets. Subtle by design — it tells you "here's what will survive
 * export" without painting over the frame. Renders only when the playhead
 * is inside a region that has a crop.
 */
export function CropIndicator(props: {
  videoElement: HTMLVideoElement | null;
  sourceWidth: number;
  sourceHeight: number;
  crop: Crop;
}) {
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

  if (!bounds || props.sourceWidth <= 0 || props.sourceHeight <= 0) return null;

  const fitted = fittedVideoRect(
    bounds.width,
    bounds.height,
    props.sourceWidth,
    props.sourceHeight
  );
  const tlX = fitted.x + (props.crop.x / props.sourceWidth) * fitted.w;
  const tlY = fitted.y + (props.crop.y / props.sourceHeight) * fitted.h;
  const w = (props.crop.w / props.sourceWidth) * fitted.w;
  const h = (props.crop.h / props.sourceHeight) * fitted.h;

  // Hide when the crop is effectively the full frame (no useful indicator).
  if (
    props.crop.x === 0 &&
    props.crop.y === 0 &&
    props.crop.w >= props.sourceWidth &&
    props.crop.h >= props.sourceHeight
  ) {
    return null;
  }

  return (
    <div
      className="crop-indicator"
      style={{
        position: "fixed",
        left: bounds.left + tlX,
        top: bounds.top + tlY,
        width: w,
        height: h,
        pointerEvents: "none",
      }}
      aria-hidden
    >
      <div className="ci-outline" />
      <div className="ci-corner ci-tl" />
      <div className="ci-corner ci-tr" />
      <div className="ci-corner ci-bl" />
      <div className="ci-corner ci-br" />
      <div className="ci-badge mono">{props.crop.w}×{props.crop.h}</div>
    </div>
  );
}
