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
