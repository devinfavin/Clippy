import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { SPEED_PRESETS, speedLabel } from "./types";

/**
 * Inline pill on the region chip showing the current speed; click to open a
 * tiny popover with preset choices.
 *
 * The popover renders into a `position: fixed` element portaled to <body> so
 * it can never be clipped by the chip row's stacking context (and never gets
 * a scrollbar trying to fit inside an ancestor with overflow constraints).
 */
export function SpeedPicker(props: {
  value: number | undefined;
  onChange: (v: number | undefined) => void;
}) {
  const v = props.value ?? 1;
  const isDefault = v === 1;
  const [open, setOpen] = useState(false);
  const pillRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  // Compute popover position from the pill's bounding rect, in viewport coords.
  // Centered horizontally; placed below the pill, flips above if no room.
  useLayoutEffect(() => {
    if (!open || !pillRef.current) return;
    const pillRect = pillRef.current.getBoundingClientRect();
    // Approximate; popoverRef may not be measured yet on first frame.
    const popoverWidth = popoverRef.current?.offsetWidth ?? 80;
    const popoverHeight = popoverRef.current?.offsetHeight ?? 180;
    const margin = 6;
    let left = pillRect.left + pillRect.width / 2 - popoverWidth / 2;
    left = Math.max(margin, Math.min(window.innerWidth - popoverWidth - margin, left));
    let top = pillRect.bottom + 6;
    if (top + popoverHeight > window.innerHeight - margin) {
      top = pillRect.top - popoverHeight - 6;
    }
    setPos({ left, top });
  }, [open, props.value]);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      const t = e.target as Node;
      if (
        pillRef.current?.contains(t) ||
        popoverRef.current?.contains(t)
      ) return;
      setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    const onScroll = () => setOpen(false);
    window.addEventListener("mousedown", onDoc);
    window.addEventListener("keydown", onKey);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onScroll);
    return () => {
      window.removeEventListener("mousedown", onDoc);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onScroll);
    };
  }, [open]);

  return (
    <>
      <button
        ref={pillRef}
        className={`speed-pill${!isDefault ? " active" : ""}`}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((o) => !o);
        }}
        title={isDefault ? "Set playback speed" : `Speed: ${speedLabel(v)}`}
      >
        {speedLabel(v)}
      </button>
      {open &&
        pos &&
        createPortal(
          <div
            ref={popoverRef}
            className="speed-popover"
            style={{ position: "fixed", left: pos.left, top: pos.top }}
            onClick={(e) => e.stopPropagation()}
            onMouseDown={(e) => e.stopPropagation()}
          >
            {SPEED_PRESETS.map((s) => (
              <button
                key={s}
                className={`speed-popover-item${s === v ? " active" : ""}`}
                onClick={() => {
                  props.onChange(s === 1 ? undefined : s);
                  setOpen(false);
                }}
              >
                {speedLabel(s)}
              </button>
            ))}
          </div>,
          document.body
        )}
    </>
  );
}
