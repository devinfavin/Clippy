import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

/**
 * Click the colored dot → portaled popover with the palette swatches.
 * Same fixed-positioning trick as SpeedPicker so it never gets clipped by
 * an ancestor's stacking context (region chips, mixer rows, etc).
 *
 * The trigger is the dot itself (rendered here so click handling is local).
 * Caller controls only the palette + selection.
 */
export function ColorPicker(props: {
  colors: string[];        // palette to choose from (4 swatches)
  selectedSlot: number;    // currently-resolved palette slot 0..N-1
  onChange: (slot: number) => void;
  /** Optional title attribute for the dot button. */
  title?: string;
  /** Class for the dot button — caller controls dot styling. */
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const dotRef = useRef<HTMLButtonElement>(null);
  const popRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  // Position the popover from the dot's bounding rect (viewport coords).
  // Centered horizontally on the dot; below by default, flips above if no room.
  useLayoutEffect(() => {
    if (!open || !dotRef.current) return;
    const dotRect = dotRef.current.getBoundingClientRect();
    const popW = popRef.current?.offsetWidth ?? 160;
    const popH = popRef.current?.offsetHeight ?? 44;
    const margin = 6;
    let left = dotRect.left + dotRect.width / 2 - popW / 2;
    left = Math.max(margin, Math.min(window.innerWidth - popW - margin, left));
    let top = dotRect.bottom + 8;
    if (top + popH > window.innerHeight - margin) {
      top = dotRect.top - popH - 8;
    }
    setPos({ left, top });
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      const t = e.target as Node;
      if (dotRef.current?.contains(t) || popRef.current?.contains(t)) return;
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

  const currentColor = props.colors[props.selectedSlot] ?? props.colors[0];

  return (
    <>
      <button
        ref={dotRef}
        type="button"
        className={props.className}
        style={{ background: currentColor }}
        title={props.title ?? "Change color"}
        aria-label={props.title ?? "Change color"}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((o) => !o);
        }}
      />
      {open &&
        pos &&
        createPortal(
          <div
            ref={popRef}
            className="color-popover"
            style={{ position: "fixed", left: pos.left, top: pos.top }}
            onClick={(e) => e.stopPropagation()}
            onMouseDown={(e) => e.stopPropagation()}
          >
            {props.colors.map((c, i) => (
              <button
                key={i}
                type="button"
                className={`color-swatch${i === props.selectedSlot ? " active" : ""}`}
                style={{ background: c }}
                title={`Color ${i + 1}`}
                onClick={() => {
                  props.onChange(i);
                  setOpen(false);
                }}
              />
            ))}
          </div>,
          document.body
        )}
    </>
  );
}
