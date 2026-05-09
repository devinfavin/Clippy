import { useEffect } from "react";
import { fmtTime, fmtMb, estimateBytesForClip } from "./formatters";
import { SIZE_PRESETS, type ExportMode, type SizeLimit } from "./types";

export function ExportModal(props: {
  clips: Array<{ inSecs: number; outSecs: number }>;
  mode: ExportMode;
  setMode: (m: ExportMode) => void;
  size: SizeLimit;
  setSize: (s: SizeLimit) => void;
  sourceBitrateBps: number | null;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  // Esc closes the modal; Enter confirms.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        props.onCancel();
      } else if (e.key === "Enter" && !e.ctrlKey && !e.shiftKey && !e.altKey) {
        const tag = (e.target as HTMLElement | null)?.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA") return;
        e.preventDefault();
        props.onConfirm();
      }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true } as AddEventListenerOptions);
  }, [props]);

  const totalDuration = props.clips.reduce((sum, c) => sum + (c.outSecs - c.inSecs), 0);
  const isStitch = props.mode === "stitched" && props.clips.length > 1;

  // Estimated total output bytes for the current mode + size selection.
  const totalEstimateBytes: number | null = (() => {
    if (props.size.kind === "mb") {
      const mb = props.size.mb;
      return (isStitch ? mb : mb * props.clips.length) * 1024 * 1024;
    }
    // No-limit estimate uses source bitrate × duration. Same in both modes
    // because stream copy preserves the source bitrate.
    if (props.sourceBitrateBps == null) return null;
    return Math.round(((props.sourceBitrateBps * totalDuration) / 8) * 1.02);
  })();

  // Per-clip estimate for the size-selected preset (used in the per-clip list).
  const perClipEstimate = (durationSecs: number) =>
    estimateBytesForClip(durationSecs, props.size, props.sourceBitrateBps);

  // Warn if a sized preset is going to push video bitrate too low to be watchable.
  const sizedWarning = (() => {
    if (props.size.kind !== "mb") return null;
    const perClipMb = props.size.mb;
    const firstClipDuration = props.clips.length > 0 ? props.clips[0].outSecs - props.clips[0].inSecs : 0;
    const perClipDuration = isStitch ? totalDuration : firstClipDuration;
    if (perClipDuration <= 0) return null;
    const audioBytes = (96000 / 8) * perClipDuration;
    const videoBytes = perClipMb * 1024 * 1024 * 0.95 - audioBytes;
    const videoKbps = (videoBytes * 8) / perClipDuration / 1000;
    if (videoKbps < 500) return `Very low bitrate (~${Math.max(0, Math.round(videoKbps))} kbps video). Picture quality will suffer.`;
    if (videoKbps < 1500) return `Low bitrate (~${Math.round(videoKbps)} kbps video).`;
    return null;
  })();

  return (
    <div className="modal-overlay" onMouseDown={(e) => { if (e.target === e.currentTarget) props.onCancel(); }}>
      <div className="modal export-modal">
        <header className="modal-header">
          <h2>Export</h2>
          <button className="modal-close" onClick={props.onCancel}>×</button>
        </header>

        <div className="export-body">
          {props.clips.length > 1 && (
            <fieldset className="export-section">
              <legend>Mode</legend>
              <label className="radio-row">
                <input
                  type="radio"
                  checked={props.mode === "separate"}
                  onChange={() => props.setMode("separate")}
                />
                <span>{props.clips.length} separate clips</span>
              </label>
              <label className="radio-row">
                <input
                  type="radio"
                  checked={props.mode === "stitched"}
                  onChange={() => props.setMode("stitched")}
                />
                <span>1 stitched clip ({fmtTime(totalDuration)})</span>
              </label>
            </fieldset>
          )}

          <fieldset className="export-section">
            <legend>Size limit</legend>
            {SIZE_PRESETS.map((preset, i) => {
              const active =
                preset.kind === props.size.kind &&
                (preset.kind === "none" || preset.mb === (props.size as { mb: number }).mb);
              const label = preset.kind === "none" ? "No limit (source quality)" : preset.label;
              return (
                <label className="radio-row" key={i}>
                  <input
                    type="radio"
                    checked={active}
                    onChange={() => props.setSize(preset)}
                  />
                  <span>{label}</span>
                </label>
              );
            })}
          </fieldset>

          <div className="export-estimate">
            <div className="estimate-row">
              <span className="estimate-label">Estimated output</span>
              <span className="estimate-value mono">
                {totalEstimateBytes != null
                  ? isStitch || props.clips.length === 1
                    ? `≈ ${fmtMb(totalEstimateBytes)}`
                    : `≈ ${fmtMb(totalEstimateBytes)} total (${props.clips.length} × ${fmtMb(totalEstimateBytes / props.clips.length)})`
                  : "—"}
              </span>
            </div>
            {sizedWarning && <div className="estimate-warning">⚠ {sizedWarning}</div>}
            {!isStitch && props.clips.length > 1 && (
              <ul className="estimate-per-clip">
                {props.clips.map((c, i) => {
                  const dur = c.outSecs - c.inSecs;
                  const est = perClipEstimate(dur);
                  return (
                    <li key={i}>
                      <span className="dim mono">#{i + 1}</span>{" "}
                      <span className="mono">{fmtTime(c.inSecs)} → {fmtTime(c.outSecs)}</span>{" "}
                      <span className="dim">({fmtTime(dur)})</span>
                      <span className="estimate-clip-size mono">
                        {est != null ? `≈ ${fmtMb(est)}` : "—"}
                      </span>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
        </div>

        <footer className="modal-footer">
          <button onClick={props.onCancel}>Cancel</button>
          <button className="primary" onClick={props.onConfirm}>
            {isStitch ? "Export stitched…" : props.clips.length > 1 ? `Export ${props.clips.length} clips…` : "Export…"}
          </button>
        </footer>
      </div>
    </div>
  );
}
