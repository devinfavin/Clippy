import { useEffect, type RefObject } from "react";

/**
 * Close a popover on Esc or a pointerdown outside its anchor element.
 *
 * Used by the per-row color pickers in the regions list and audio panel.
 * Both used to dismiss only on `onMouseLeave`, which left them stuck if
 * the user clicked the swatch but didn't sweep the cursor away — and
 * keyboard users had no dismissal at all.
 *
 * Capture-phase pointerdown so the outside-click runs before React's
 * synthetic-event delegation, which fires bubble-phase and would otherwise
 * miss clicks on elements that stop propagation (e.g. the row's onClick
 * → onFocus toggle in RegionsPanel).
 */
export function useDismissPopover(
  open: boolean,
  anchorRef: RefObject<HTMLElement | null>,
  onClose: () => void,
): void {
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    const onDown = (e: PointerEvent) => {
      const el = anchorRef.current;
      if (!el) return;
      if (e.target instanceof Node && el.contains(e.target)) return;
      onClose();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("pointerdown", onDown, true);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("pointerdown", onDown, true);
    };
  }, [open, anchorRef, onClose]);
}
