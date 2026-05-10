import { useEffect } from "react";
import { fmtTime, fmtMb, estimateBytesForClip } from "./formatters";
import {
  cropsEqual,
  GIF_RESOLUTION_PRESETS,
  gifBytesPerSec,
  MP3_BITRATE_BPS,
  SIZE_PRESETS,
  type Crop,
  type ExportFormat,
  type ExportMode,
  type GifResolution,
  type SizeLimit,
} from "./types";

/**
 * Segmented-button row used for Format / Mode / Size pickers. Replaces the
 * native radio fieldsets so the modal can present each picker as a clean
 * row of cards instead of a stacked list of radios.
 */
function Segmented<T>(props: {
  label: string;
  options: Array<{
    value: T;
    label: string;
    sub?: string;
    disabled?: boolean;
    title?: string;
  }>;
  value: T;
  isEqual?: (a: T, b: T) => boolean;
  onChange: (v: T) => void;
}) {
  const eq = props.isEqual ?? ((a: T, b: T) => a === b);
  return (
    <div className="export-section">
      <div className="export-section-label">{props.label}</div>
      <div className="seg" role="radiogroup" aria-label={props.label}>
        {props.options.map((opt, i) => {
          const active = eq(opt.value, props.value);
          return (
            <button
              key={i}
              type="button"
              role="radio"
              aria-checked={active}
              disabled={opt.disabled}
              title={opt.title}
              className={`seg-btn${active ? " active" : ""}${opt.disabled ? " disabled" : ""}`}
              onClick={() => !opt.disabled && props.onChange(opt.value)}
            >
              <span className="seg-btn-label">{opt.label}</span>
              {opt.sub && <span className="seg-btn-sub">{opt.sub}</span>}
            </button>
          );
        })}
      </div>
    </div>
  );
}

export function ExportModal(props: {
  clips: Array<{ inSecs: number; outSecs: number; crop?: Crop; speed?: number }>;
  mode: ExportMode;
  setMode: (m: ExportMode) => void;
  size: SizeLimit;
  setSize: (s: SizeLimit) => void;
  format: ExportFormat;
  setFormat: (f: ExportFormat) => void;
  normalize: boolean;
  setNormalize: (n: boolean) => void;
  gifResolution: GifResolution;
  setGifResolution: (g: GifResolution) => void;
  sourceWidth: number;
  sourceHeight: number;
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

  // Effective duration accounts for per-region speed (a 4× clip is 1/4 as long).
  const effDuration = (c: { inSecs: number; outSecs: number; speed?: number }) =>
    Math.max(0, c.outSecs - c.inSecs) / (c.speed ?? 1);
  const totalDuration = props.clips.reduce((sum, c) => sum + effDuration(c), 0);
  const isStitch = props.mode === "stitched" && props.clips.length > 1;
  const isAudio = props.format === "mp3";
  const isGif = props.format === "gif";
  const isVideo = props.format === "mp4";

  const cropMismatch =
    !isAudio &&
    props.clips.length > 1 &&
    props.clips.some((c) => !cropsEqual(c.crop, props.clips[0].crop));
  // Sized stitched + GIF stitched also require uniform speed (single-pass paths).
  const speedMismatch =
    props.clips.length > 1 &&
    props.clips.some((c) => (c.speed ?? 1) !== (props.clips[0].speed ?? 1));
  const sizedStitchBlocked = isStitch && props.size.kind === "mb" && (cropMismatch || speedMismatch);
  const cropStitchBlocked = isStitch && cropMismatch && !isAudio;
  const stitchBlocked = sizedStitchBlocked || cropStitchBlocked;
  const anyCropped = !isAudio && props.clips.some((c) => !!c.crop);
  const anySpeedAdjusted = props.clips.some((c) => (c.speed ?? 1) !== 1);

  // GIF byte estimate scales with chosen resolution and source aspect.
  const sourceAspect =
    props.sourceWidth > 0 && props.sourceHeight > 0
      ? props.sourceWidth / props.sourceHeight
      : 16 / 9;
  const gifEffectiveWidth =
    props.gifResolution.kind === "source"
      ? props.sourceWidth || 1280
      : Math.min(props.gifResolution.w, props.sourceWidth || props.gifResolution.w);
  const gifBps = gifBytesPerSec(gifEffectiveWidth, sourceAspect);

  const totalEstimateBytes: number | null = (() => {
    if (isAudio) return Math.round((MP3_BITRATE_BPS * totalDuration) / 8);
    if (isGif) return Math.round(gifBps * totalDuration);
    if (props.size.kind === "mb") {
      const mb = props.size.mb;
      return (isStitch ? mb : mb * props.clips.length) * 1024 * 1024;
    }
    if (props.sourceBitrateBps == null) return null;
    return Math.round(((props.sourceBitrateBps * totalDuration) / 8) * 1.02);
  })();

  const perClipEstimate = (clipDur: number) =>
    isAudio
      ? Math.round((MP3_BITRATE_BPS * clipDur) / 8)
      : isGif
      ? Math.round(gifBps * clipDur)
      : estimateBytesForClip(clipDur, props.size, props.sourceBitrateBps);

  // Warn if a sized preset is going to push video bitrate too low to be watchable.
  const sizedWarning = (() => {
    if (isAudio || props.size.kind !== "mb") return null;
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

  const confirmLabel = (() => {
    const ext = isAudio ? "MP3" : isGif ? "GIF" : "";
    if (isStitch) return ext ? `Export stitched ${ext}…` : "Export stitched…";
    if (props.clips.length > 1)
      return ext ? `Export ${props.clips.length} ${ext}s…` : `Export ${props.clips.length} clips…`;
    return ext ? `Export ${ext}…` : "Export…";
  })();

  return (
    <div className="modal-overlay" onMouseDown={(e) => { if (e.target === e.currentTarget) props.onCancel(); }}>
      <div className="modal export-modal">
        <header className="modal-header">
          <h2>Export</h2>
          <button className="modal-close" onClick={props.onCancel}>×</button>
        </header>

        <div className="export-body">
          <Segmented
            label="Format"
            value={props.format}
            onChange={props.setFormat}
            options={[
              { value: "mp4", label: "MP4", sub: "video" },
              { value: "mp3", label: "MP3", sub: "audio · 192k" },
              { value: "gif", label: "GIF", sub: "silent · 15fps · 480px" },
            ]}
          />

          {props.clips.length > 1 && (
            <>
              <Segmented
                label="Mode"
                value={props.mode}
                onChange={props.setMode}
                options={[
                  {
                    value: "separate",
                    label: `${props.clips.length} clips`,
                    sub: "one file each",
                  },
                  {
                    value: "stitched",
                    label: "Stitched",
                    sub: fmtTime(totalDuration),
                    disabled: cropMismatch,
                    title: cropMismatch
                      ? "Stitched needs every region to share the same crop"
                      : undefined,
                  },
                ]}
              />
              {cropMismatch && (
                <div className="estimate-warning">
                  ⚠ Stitched needs every region to share the same crop. Use the
                  ✂ button on a region and choose <em>Apply to all</em>.
                </div>
              )}
            </>
          )}

          {isVideo && (
            <Segmented
              label="Size"
              value={props.size}
              isEqual={(a, b) =>
                a.kind === b.kind &&
                (a.kind === "none" || (b.kind === "mb" && a.mb === b.mb))
              }
              onChange={props.setSize}
              options={SIZE_PRESETS.map((preset) => ({
                value: preset,
                label:
                  preset.kind === "none" ? "No limit" : preset.label,
                sub:
                  preset.kind === "none" ? "source quality" : "discord-friendly",
              }))}
            />
          )}

          {isGif && (
            <Segmented
              label="Resolution"
              value={props.gifResolution}
              isEqual={(a, b) =>
                a.kind === b.kind &&
                (a.kind !== "px" || (b.kind === "px" && a.w === b.w))
              }
              onChange={props.setGifResolution}
              options={GIF_RESOLUTION_PRESETS.map((preset) => ({
                value: preset,
                label:
                  preset.kind === "source"
                    ? "Source"
                    : preset.label,
                sub:
                  preset.kind === "source"
                    ? props.sourceWidth > 0
                      ? `${props.sourceWidth}px`
                      : "no scale"
                    : `${preset.w}px wide`,
              }))}
            />
          )}

          {!isGif && (
            <label className="export-toggle">
              <input
                type="checkbox"
                checked={props.normalize}
                onChange={(e) => props.setNormalize(e.target.checked)}
              />
              <span className="export-toggle-label">Normalize loudness</span>
              <span className="export-toggle-sub">
                Boosts quiet game audio to a Discord-friendly level (~-16 LUFS).
              </span>
            </label>
          )}

          <div className="export-estimate">
            <div className="estimate-row">
              <span className="estimate-label">Estimated output</span>
              <span className="estimate-value mono">
                {totalEstimateBytes != null
                  ? isStitch || props.clips.length === 1
                    ? `≈ ${fmtMb(totalEstimateBytes)}`
                    : `≈ ${fmtMb(totalEstimateBytes)} total · ${props.clips.length} × ${fmtMb(totalEstimateBytes / props.clips.length)}`
                  : "—"}
              </span>
            </div>
            {sizedWarning && <div className="estimate-warning">⚠ {sizedWarning}</div>}
            {(anyCropped || anySpeedAdjusted) && !isGif && !isAudio && (
              <div className="estimate-note">
                {anyCropped && anySpeedAdjusted
                  ? "Crop / speed adjustments re-encode the affected regions."
                  : anyCropped
                  ? "Cropped regions are re-encoded (no stream-copy)."
                  : "Sped-up / slowed-down regions are re-encoded."}
              </div>
            )}
            {isAudio && props.clips.some((c) => !!c.crop) && (
              <div className="estimate-note">
                Crops are ignored for audio-only export.
              </div>
            )}
            {isGif && totalDuration > 10 && (
              <div className="estimate-warning">
                ⚠ GIFs over ~10 s get huge fast. Consider trimming or splitting.
              </div>
            )}
            {sizedStitchBlocked && (
              <div className="estimate-warning">
                ⚠ Size-targeted stitch needs every region to share the same crop
                and speed.
              </div>
            )}
            {!isStitch && props.clips.length > 1 && (
              <ul className="estimate-per-clip">
                {props.clips.map((c, i) => {
                  const dur = effDuration(c);
                  const est = perClipEstimate(dur);
                  return (
                    <li key={i}>
                      <span className="dim mono">#{i + 1}</span>{" "}
                      <span className="mono">{fmtTime(c.inSecs)} → {fmtTime(c.outSecs)}</span>{" "}
                      <span className="dim">({fmtTime(dur)}{(c.speed ?? 1) !== 1 ? ` · ${c.speed}×` : ""})</span>
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
          <button className="primary" onClick={props.onConfirm} disabled={stitchBlocked}>
            {confirmLabel}
          </button>
        </footer>
      </div>
    </div>
  );
}
