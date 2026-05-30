import { RAIL_CROP_PRESETS, regionDisplayName, type AspectLock, type Crop, type Region } from "../types";
import { fittedSourceCropFor } from "../cropGeom";

type Props = {
  /** The region currently focused in the rail. Null when no region is
   *  selected — the panel falls back to an explainer + a launcher CTA so the
   *  user still has somewhere to land on the Crop tab. */
  activeRegion: Region | null;
  activeRegionIndex: number | null;
  sourceWidth: number;
  sourceHeight: number;
  hasSource: boolean;
  /** Total number of regions in the project. Used to decide whether to
   *  surface the "Apply to all regions" footer button (only meaningful at
   *  2+). */
  regionCount: number;
  /** Open the full crop editor (CropOverlay modal) for this region. The
   *  aspect chips here are quick presets; precise dragging happens there. */
  onLaunchEditor: (regionId: string) => void;
  /** Apply a preset aspect to the active region without opening the editor.
   *  `undefined` clears the crop. */
  onSetPresetForActive: (crop: Crop | undefined) => void;
  /** Apply the active region's current crop to every region. The export
   *  modal tells users to do this when a stitched export's regions have
   *  different crops; surfacing it here avoids the editor-modal detour. */
  onApplyToAll: (crop: Crop) => void;
};

/**
 * Crop tab in the editor rail. Two-purpose surface:
 *   1. Quick aspect chips — one click sets a centered preset crop on the
 *      active region (no modal). Most clips don't need precision; this
 *      shortcut is the 80% case.
 *   2. Launcher for the full CropOverlay editor for the active region, where
 *      pixel-precise drag/resize lives. Free-form crop is intentionally only
 *      reachable through the editor, not surfaced here — preset-snap is the
 *      rail's model.
 */
export function CropPanel(props: Props) {
  const { activeRegion, activeRegionIndex, sourceWidth, sourceHeight, hasSource,
          regionCount, onLaunchEditor, onSetPresetForActive, onApplyToAll } = props;

  const canEdit = hasSource && activeRegion != null;
  const currentCrop = activeRegion?.crop;
  const canApplyToAll = canEdit && currentCrop != null && regionCount > 1;

  return (
    <div className="crop-panel">
      <div className="crop-panel-header">
        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor"
             strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
          <path d="M6 2v16h16M2 6h16v16" />
        </svg>
        <span>Aspect</span>
        <span className="crop-panel-region">
          {activeRegion
            ? regionDisplayName(activeRegion, activeRegionIndex ?? 0)
            : "Pick a region first"}
        </span>
      </div>

      <div className="crop-panel-presets">
        {RAIL_CROP_PRESETS.map((preset) => (
          <CropAspectButton
            key={presetKey(preset)}
            preset={preset}
            current={currentCrop}
            disabled={!canEdit}
            sourceWidth={sourceWidth}
            sourceHeight={sourceHeight}
            onPick={(crop) => onSetPresetForActive(crop)}
          />
        ))}
        {/* Clear button removed — "Source" preset already clears the crop
            (see CropAspectButton.handle: source → onPick(undefined)). After
            dropping the Free entry, Clear was just a duplicate row. */}
      </div>

      <div className="crop-panel-output">
        <div className="crop-panel-output-label">Output</div>
        <div className="crop-panel-output-value mono">
          {currentCrop
            ? `${currentCrop.w} × ${currentCrop.h}`
            : `${sourceWidth || "—"} × ${sourceHeight || "—"} (source)`}
        </div>
        <div className="crop-panel-output-hint">
          {activeRegion
            ? "Stored on this region. Other regions keep their own crop."
            : "Click a region in the timeline or Regions tab to edit its crop."}
        </div>
      </div>

      <div className="crop-panel-footer">
        <button
          type="button"
          className="btn ghost"
          disabled={!canEdit}
          onClick={() => activeRegion && onLaunchEditor(activeRegion.id)}
          title="Open the precise crop editor for this region"
        >
          Open crop editor…
        </button>
        {/* Apply-to-all — only meaningful with 2+ regions AND an existing
            crop on the active region. Stitched export blocks on crop
            mismatch and explicitly tells the user to "Apply to all"; this
            avoids the editor-modal round-trip the error message implies. */}
        <button
          type="button"
          className="btn ghost"
          disabled={!canApplyToAll}
          onClick={() => currentCrop && onApplyToAll(currentCrop)}
          title={
            canApplyToAll
              ? "Copy this region's crop to every region — required for stitched export"
              : regionCount <= 1
                ? "Only one region — nothing else to apply this crop to"
                : "Set a crop on this region first"
          }
        >
          Apply to all
        </button>
      </div>
    </div>
  );
}

function presetKey(p: AspectLock): string {
  if (p.kind === "free") return "free";
  if (p.kind === "source") return "source";
  return `ratio-${p.w}-${p.h}`;
}

function presetLabel(p: AspectLock): { label: string; sub: string } {
  if (p.kind === "free") return { label: "Free", sub: "draw any" };
  if (p.kind === "source") return { label: "Source", sub: "no crop" };
  if (p.w === 16 && p.h === 9) return { label: p.label, sub: "Landscape" };
  if (p.w === 9 && p.h === 16) return { label: p.label, sub: "Vertical" };
  if (p.w === 1 && p.h === 1) return { label: p.label, sub: "Square" };
  if (p.w === 4 && p.h === 3) return { label: p.label, sub: "Standard" };
  return { label: p.label, sub: "" };
}

function CropAspectButton(props: {
  preset: AspectLock;
  current: Crop | undefined;
  disabled: boolean;
  sourceWidth: number;
  sourceHeight: number;
  onPick: (crop: Crop | undefined) => void;
}) {
  const { preset, current, disabled, sourceWidth, sourceHeight, onPick } = props;
  const { label, sub } = presetLabel(preset);
  // Compare against current crop loosely — "active" means the current crop
  // dimensions match this preset's aspect within ±1px. Source preset = no
  // crop. Free is never marked active (it requires user drawing).
  const active = isPresetActive(preset, current, sourceWidth, sourceHeight);

  const handle = () => {
    if (disabled) return;
    if (preset.kind === "source") return onPick(undefined);
    if (preset.kind === "free") return onPick(current);  // no-op; editor handles
    const ratio = preset.w / preset.h;
    const crop = fittedSourceCropFor(ratio, sourceWidth, sourceHeight);
    onPick(crop);
  };

  // Thumbnail rectangle drawn at the preset's aspect, contained in 26px box
  let thumbW = 26;
  let thumbH = 26;
  if (preset.kind === "ratio") {
    const r = preset.w / preset.h;
    if (r >= 1) { thumbW = 26; thumbH = 26 / r; }
    else { thumbH = 26; thumbW = 26 * r; }
  } else if (preset.kind === "source" && sourceWidth > 0 && sourceHeight > 0) {
    const r = sourceWidth / sourceHeight;
    if (r >= 1) { thumbW = 26; thumbH = 26 / r; }
    else { thumbH = 26; thumbW = 26 * r; }
  }

  return (
    <button
      type="button"
      className={`crop-preset-btn${active ? " active" : ""}`}
      disabled={disabled}
      onClick={handle}
    >
      <span className="crop-preset-thumb"
            style={{ width: `${thumbW}px`, height: `${thumbH}px` }}
            aria-hidden />
      <span className="crop-preset-text">
        <span className="crop-preset-label">{label}</span>
        <span className="crop-preset-sub mono">{sub}</span>
      </span>
    </button>
  );
}

function isPresetActive(
  preset: AspectLock,
  current: Crop | undefined,
  sourceWidth: number,
  sourceHeight: number,
): boolean {
  if (preset.kind === "source") return !current;
  if (preset.kind === "free") return false;
  if (!current) return false;
  if (sourceWidth <= 0 || sourceHeight <= 0) return false;
  const target = preset.w / preset.h;
  const actual = current.w / Math.max(1, current.h);
  return Math.abs(target - actual) < 0.01;
}
