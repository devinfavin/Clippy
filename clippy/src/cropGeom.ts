/**
 * The <video> uses object-fit: contain, so it letterbox/pillarboxes when
 * source aspect != element aspect. This computes the actual rendered video
 * area inside the element so callers can map mouse coords ↔ source pixels.
 */
export function fittedVideoRect(
  elemW: number,
  elemH: number,
  srcW: number,
  srcH: number
): { x: number; y: number; w: number; h: number } {
  if (srcW <= 0 || srcH <= 0 || elemW <= 0 || elemH <= 0) {
    return { x: 0, y: 0, w: elemW, h: elemH };
  }
  const srcAspect = srcW / srcH;
  const elemAspect = elemW / elemH;
  if (srcAspect > elemAspect) {
    // letterboxed (bars top/bottom)
    const h = elemW / srcAspect;
    return { x: 0, y: (elemH - h) / 2, w: elemW, h };
  }
  // pillarboxed (bars left/right)
  const w = elemH * srcAspect;
  return { x: (elemW - w) / 2, y: 0, w, h: elemH };
}

/**
 * Build a centered crop rectangle (in source-pixel coordinates) that matches
 * the given target w/h ratio while fitting entirely inside the source. Used
 * by the Crop tab's quick-aspect chips so the user can one-click a preset
 * without opening the editor.
 *
 * Returns dimensions rounded to even pixels because most video encoders
 * (libx264 included) reject odd output dimensions.
 */
export function fittedSourceCropFor(
  ratio: number,
  srcW: number,
  srcH: number,
): { x: number; y: number; w: number; h: number } | undefined {
  if (ratio <= 0 || srcW <= 0 || srcH <= 0) return undefined;
  let w: number;
  let h: number;
  // Match the longer axis to the source, then derive the other dimension.
  const sourceRatio = srcW / srcH;
  if (ratio >= sourceRatio) {
    // crop is wider than source — clamp to full width
    w = srcW;
    h = Math.round(w / ratio);
  } else {
    // crop is taller than source — clamp to full height
    h = srcH;
    w = Math.round(h * ratio);
  }
  // Even dimensions for encoder compatibility.
  if (w % 2) w -= 1;
  if (h % 2) h -= 1;
  const x = Math.max(0, Math.round((srcW - w) / 2));
  const y = Math.max(0, Math.round((srcH - h) / 2));
  return { x, y, w, h };
}
