import { useRef, useState } from "react";
import { fmtTime } from "../formatters";
import { useDismissPopover } from "../hooks/use-dismiss-popover";
import {
  REGION_COLORS,
  regionDisplayName,
  resolveRegionColor,
  SPEED_PRESETS,
  type Region,
} from "../types";

type Props = {
  regions: Region[];
  activeId: string | null;
  playheadSecs: number;
  hasSource: boolean;
  loopingRegionId: string | null;
  /** Notify parent the user clicked a region. Passing `null` deselects.
   *  Clicking the already-active row toggles selection off. Parent decides
   *  whether to seek. */
  onFocus: (id: string | null) => void;
  /** Add a new region from the playhead position. Parent owns the geometry/
   *  collision rules; this panel just fires the intent. */
  onAddFromPlayhead: () => void;
  onRename: (id: string, name: string) => void;
  onDelete: (id: string) => void;
  onSetSpeed: (id: string, speed: number | undefined) => void;
  onSetColor: (id: string, colorIndex: number) => void;
  onToggleLoop: (id: string) => void;
  onStartCropEdit: (id: string) => void;
};

/**
 * Replacement for the legacy chip strip — a vertical list where the active
 * region expands inline to expose speed pills, crop indicator, loop, and
 * delete. Hosted inside the editor rail's Regions tab.
 *
 * Rename writes through `onRename` to the parent, which patches `region.name`
 * and persists via `useProjectAutosave`. `regionDisplayName` falls back to
 * "Region N" by natural index when the user hasn't renamed.
 */
export function RegionsPanel(props: Props) {
  const {
    regions,
    activeId,
    playheadSecs,
    hasSource,
    loopingRegionId,
    onFocus,
    onAddFromPlayhead,
    onRename,
    onDelete,
    onSetSpeed,
    onSetColor,
    onToggleLoop,
    onStartCropEdit,
  } = props;

  const [renamingId, setRenamingId] = useState<string | null>(null);

  // Display palette without the 4× preset (scope: speed ceiling stays at 2×;
  // the legacy 4× entry stays valid in saved data but isn't exposed in new UI).
  const speedSteps = SPEED_PRESETS.filter((s) => s <= 2);

  return (
    <div className="regions-panel">
      <button
        type="button"
        className="regions-add-cta"
        onClick={onAddFromPlayhead}
        disabled={!hasSource}
        title="Add a region starting at the current playhead"
      >
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor"
             strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
          <path d="M12 5v14M5 12h14" />
        </svg>
        <span>New region from playhead</span>
        <span className="mono regions-add-cta-time">{fmtTime(playheadSecs)}</span>
      </button>

      {regions.length === 0 ? (
        <div className="regions-empty">
          <span>No regions yet.</span>
          <span className="dim">Set in/out on the timeline or use the button above.</span>
        </div>
      ) : (
        <ul className="regions-list" role="list">
          {regions.map((r, i) => {
            const active = r.id === activeId;
            const renaming = renamingId === r.id;
            const colorIndex = r.colorIndex ?? (i % REGION_COLORS.length);
            const color = resolveRegionColor(r, i);
            const dur = r.outSecs - r.inSecs;
            const speed = r.speed ?? 1;
            const looping = loopingRegionId === r.id;
            const displayName = regionDisplayName(r, i);
            return (
              <li
                key={r.id}
                className={`region-row${active ? " active" : ""}`}
                style={{ ["--tint-color" as never]: color }}
                onClick={() => onFocus(active ? null : r.id)}
              >
                <div className="region-row-head">
                  <span className="region-row-id" aria-hidden>{i + 1}</span>
                  {renaming ? (
                    <input
                      autoFocus
                      defaultValue={displayName}
                      onClick={(e) => e.stopPropagation()}
                      onBlur={(e) => {
                        // Empty string clears the override; parent helper
                        // strips the field. No local fallback needed —
                        // regionDisplayName handles "Region N".
                        onRename(r.id, e.currentTarget.value);
                        setRenamingId(null);
                      }}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") e.currentTarget.blur();
                        if (e.key === "Escape") setRenamingId(null);
                      }}
                      className="region-row-rename"
                    />
                  ) : (
                    <span
                      className="region-row-name"
                      onDoubleClick={(e) => { e.stopPropagation(); setRenamingId(r.id); }}
                      title="Double-click to rename"
                    >
                      {displayName}
                    </span>
                  )}
                  <span className="mono region-row-dur">{fmtTime(dur)}</span>
                </div>
                <div className="region-row-times mono">
                  <span>{fmtTime(r.inSecs)}</span>
                  <span className="region-row-arrow" aria-hidden>→</span>
                  <span>{fmtTime(r.outSecs)}</span>
                </div>

                {active && (
                  <div className="region-row-active-tools" onClick={(e) => e.stopPropagation()}>
                    <div className="region-speed-pills" role="group" aria-label="Playback speed">
                      {speedSteps.map((s) => {
                        const on = speed === s;
                        return (
                          <button
                            key={s}
                            type="button"
                            className={`region-speed-pill${on ? " active" : ""}`}
                            onClick={() => onSetSpeed(r.id, s === 1 ? undefined : s)}
                          >
                            {s}×
                          </button>
                        );
                      })}
                    </div>
                    <div className="region-row-bottom">
                      <button
                        type="button"
                        className={`region-row-crop-badge${r.crop ? " has-crop" : ""}`}
                        onClick={() => onStartCropEdit(r.id)}
                        title={r.crop ? "Edit crop" : "Add a crop to this region"}
                      >
                        <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                             strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                          <path d="M6 2v16h16M2 6h16v16" />
                        </svg>
                        <span className="mono">
                          {r.crop ? `${r.crop.w}×${r.crop.h}` : "source"}
                        </span>
                      </button>
                      <span className="region-row-spacer" />
                      <RegionColorButton
                        color={color}
                        colorIndex={colorIndex}
                        onPick={(idx) => onSetColor(r.id, idx)}
                      />
                      <button
                        type="button"
                        className={`region-row-icon-btn${looping ? " active" : ""}`}
                        onClick={() => onToggleLoop(r.id)}
                        title={looping ? "Stop looping" : "Loop this region"}
                        aria-label={looping ? "Stop looping" : "Loop this region"}
                      >
                        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                             strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                          <path d="M17 2l4 4-4 4M3 12V8a4 4 0 014-4h14M7 22l-4-4 4-4M21 12v4a4 4 0 01-4 4H3" />
                        </svg>
                      </button>
                      <button
                        type="button"
                        className="region-row-icon-btn region-row-delete"
                        onClick={() => onDelete(r.id)}
                        title="Delete region"
                        aria-label="Delete region"
                      >
                        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                             strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                          <path d="M3 6h18M8 6V4a2 2 0 012-2h4a2 2 0 012 2v2M19 6l-1 14a2 2 0 01-2 2H8a2 2 0 01-2-2L5 6M10 11v6M14 11v6" />
                        </svg>
                      </button>
                    </div>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

/** Inline swatch popover. Lives next to the loop/delete actions in the
 *  expanded row. Kept tiny — the full ColorPicker is overkill here.
 *
 *  Dismisses on Esc or outside-click (via useDismissPopover); the older
 *  onMouseLeave path stranded the popover when the user clicked but didn't
 *  sweep the cursor away. */
function RegionColorButton(props: {
  color: string;
  colorIndex: number;
  onPick: (idx: number) => void;
}) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLSpanElement | null>(null);
  useDismissPopover(open, wrapRef, () => setOpen(false));
  return (
    <span className="region-row-color-wrap" ref={wrapRef}>
      <button
        type="button"
        className="region-row-color-btn"
        style={{ background: props.color }}
        onClick={(e) => { e.stopPropagation(); setOpen((v) => !v); }}
        title="Change region color"
        aria-label="Change region color"
      />
      {open && (
        <span className="region-row-color-pop">
          {REGION_COLORS.map((c, i) => (
            <button
              key={i}
              type="button"
              className={`region-row-color-swatch${i === props.colorIndex ? " selected" : ""}`}
              style={{ background: c }}
              onClick={(e) => { e.stopPropagation(); props.onPick(i); setOpen(false); }}
              aria-label={`Color slot ${i + 1}`}
            />
          ))}
        </span>
      )}
    </span>
  );
}
