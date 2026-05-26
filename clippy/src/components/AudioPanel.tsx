import { useState } from "react";
import {
  defaultTrackColorIndex,
  resolveTrackColor,
  resolveTrackName,
  TRACK_COLORS,
  type AudioTrack,
  type TrackColorOverrides,
  type TrackMix,
  type TrackNameOverrides,
} from "../types";

type Props = {
  tracks: AudioTrack[];
  mix: TrackMix;
  onChange: (next: TrackMix) => void;
  trackColors: TrackColorOverrides;
  onTrackColorsChange: (next: TrackColorOverrides) => void;
  trackNames: TrackNameOverrides;
  onTrackNamesChange: (next: TrackNameOverrides) => void;
  /** "Default" or "Region N" — surfaced as a context badge so the user knows
   *  which mix they're editing. The mix auto-follows the playhead. */
  contextLabel: string;
  contextColor: string | null;
  /** Per-track peak amplitude arrays, keyed by stream index. Drives the
   *  inline waveform under each row's controls. */
  waveforms: Map<number, Float32Array>;
};

/**
 * Compact mixer for the editor rail's Audio tab. Replaces TrackMixer in the
 * rail context — TrackMixer was sized for the wide below-stage slot and its
 * horizontal layout broke at 320 px. This panel stacks vertically: header /
 * per-track row(s) / hint, with the slider + inline waveform inside each row.
 *
 * Data model unchanged: per-track TrackMix entries, parent owns mix vs region
 * write-back. Per-region mix override behaviour is preserved — the parent's
 * `onChange` writes to whichever slice (region.mix or sourceDefault) the
 * playhead resolves to.
 */
export function AudioPanel(props: Props) {
  const {
    tracks, mix, onChange,
    trackColors, onTrackColorsChange,
    trackNames, onTrackNamesChange,
    contextLabel, contextColor,
    waveforms,
  } = props;

  if (tracks.length < 1) return null;

  const anyChange = tracks.some((t) => {
    const e = mix[t.index];
    return e && (e.muted || e.volume !== 1);
  });
  const reset = () => onChange({});

  const update = (idx: number, patch: Partial<{ volume: number; muted: boolean }>) => {
    const cur = mix[idx] ?? { volume: 1, muted: false };
    onChange({ ...mix, [idx]: { ...cur, ...patch } });
  };

  return (
    <div className="audio-panel">
      <div className="audio-panel-header">
        <span className="audio-panel-title">Audio mix</span>
        <span
          className="audio-panel-ctx"
          style={
            contextColor
              ? ({ ["--ctx-color" as never]: contextColor } as React.CSSProperties)
              : undefined
          }
          title={
            contextColor
              ? "Editing this region's mix override — auto-follows the playhead."
              : "Editing the source default mix."
          }
        >
          {contextLabel}
        </span>
        <span className="audio-panel-spacer" />
        <button
          type="button"
          className="audio-panel-reset"
          onClick={reset}
          disabled={!anyChange}
          title="Reset all tracks to unity, unmuted"
        >
          Reset
        </button>
      </div>

      <div className="audio-panel-tracks">
        {tracks.map((track) => {
          const entry = mix[track.index] ?? { volume: 1, muted: false };
          const color = resolveTrackColor(track.index, trackColors);
          const colorSlot = trackColors[track.index] ?? defaultTrackColorIndex(track.index);
          const name = resolveTrackName(track, trackNames);
          const wave = waveforms.get(track.index);
          return (
            <TrackRow
              key={track.index}
              track={track}
              name={name}
              color={color}
              colorSlot={colorSlot}
              muted={entry.muted}
              volume={entry.volume}
              wave={wave}
              onToggleMute={() => update(track.index, { muted: !entry.muted })}
              onVolume={(v) => update(track.index, { volume: v })}
              onRename={(newName) => {
                const next: TrackNameOverrides = { ...trackNames };
                if (newName.trim().length === 0) delete next[track.index];
                else next[track.index] = newName.trim();
                onTrackNamesChange(next);
              }}
              onPickColor={(slot) => {
                const next: TrackColorOverrides = { ...trackColors };
                if (slot === defaultTrackColorIndex(track.index)) delete next[track.index];
                else next[track.index] = slot;
                onTrackColorsChange(next);
              }}
            />
          );
        })}
      </div>
    </div>
  );
}

function TrackRow(props: {
  track: AudioTrack;
  name: string;
  color: string;
  colorSlot: number;
  muted: boolean;
  volume: number;
  wave: Float32Array | undefined;
  onToggleMute: () => void;
  onVolume: (v: number) => void;
  onRename: (name: string) => void;
  onPickColor: (slot: number) => void;
}) {
  const [renaming, setRenaming] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  const pct = Math.round(props.volume * 100);
  return (
    <div className={`audio-track-row${props.muted ? " is-muted" : ""}`}
         style={{ ["--track-color" as never]: props.color }}>
      <div className="audio-track-head">
        <button
          type="button"
          className="audio-track-mute"
          onClick={props.onToggleMute}
          title={props.muted ? "Unmute" : "Mute"}
          aria-label={props.muted ? "Unmute" : "Mute"}
        >
          {props.muted ? (
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                 strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="M11 5L6 9H3v6h3l5 4V5z" fill="currentColor" stroke="none" />
              <path d="M22 9l-6 6M16 9l6 6" />
            </svg>
          ) : (
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                 strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="M11 5L6 9H3v6h3l5 4V5z" fill="currentColor" stroke="none" />
              <path d="M16 9a4 4 0 010 6M19 6a8 8 0 010 12" />
            </svg>
          )}
        </button>
        <span className="audio-track-dot-wrap">
          <button
            type="button"
            className="audio-track-dot"
            style={{ background: props.color }}
            onClick={() => setPickerOpen((v) => !v)}
            title="Change track color"
            aria-label="Change track color"
          />
          {pickerOpen && (
            <span className="audio-track-color-pop" onMouseLeave={() => setPickerOpen(false)}>
              {TRACK_COLORS.map((c, i) => (
                <button
                  key={i}
                  type="button"
                  className={`audio-track-color-swatch${i === props.colorSlot ? " selected" : ""}`}
                  style={{ background: c }}
                  onClick={() => { props.onPickColor(i); setPickerOpen(false); }}
                  aria-label={`Color ${i + 1}`}
                />
              ))}
            </span>
          )}
        </span>
        {renaming ? (
          <input
            autoFocus
            defaultValue={props.name}
            className="audio-track-name-input"
            onBlur={(e) => { props.onRename(e.currentTarget.value); setRenaming(false); }}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
              if (e.key === "Escape") setRenaming(false);
            }}
          />
        ) : (
          <span
            className="audio-track-name"
            onDoubleClick={() => setRenaming(true)}
            title="Double-click to rename"
          >
            {props.name}
          </span>
        )}
        <span className="audio-track-pct mono">{pct}%</span>
      </div>
      <MiniWaveform data={props.wave} color={props.color} />
      <input
        type="range"
        min={0}
        max={200}
        value={Math.round(props.volume * 100)}
        onChange={(e) => props.onVolume(Number(e.target.value) / 100)}
        className="audio-track-slider"
        style={{
          background: `linear-gradient(to right, ${props.color} ${pct / 2}%, var(--bg-elevated) ${pct / 2}%)`,
        }}
        aria-label={`${props.name} volume`}
      />
    </div>
  );
}

/** Inline mini-waveform under each track row. SVG with one rect per ~2px so
 *  it stays readable in the rail's narrow width (~ 290px usable). Falls back
 *  to a flat baseline when the track's waveform hasn't extracted yet. */
function MiniWaveform(props: { data: Float32Array | undefined; color: string }) {
  const w = 100;  // viewBox width; SVG scales to container
  const h = 18;
  const bins = 60;
  const barW = w / bins - 0.5;
  const data = props.data;
  return (
    <svg className="audio-track-wave" viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" aria-hidden>
      {Array.from({ length: bins }, (_, i) => {
        let v = 0;
        if (data && data.length > 0) {
          const start = Math.floor((i / bins) * data.length);
          const end = Math.max(start + 1, Math.floor(((i + 1) / bins) * data.length));
          let max = 0;
          for (let k = start; k < end && k < data.length; k++) {
            const a = data[k];
            if (a > max) max = a;
          }
          v = max;
        }
        const bh = Math.max(1, v * h * 0.9);
        const x = i * (w / bins);
        const y = (h - bh) / 2;
        return <rect key={i} x={x} y={y} width={barW} height={bh} rx={barW / 2} fill={props.color} opacity={0.6} />;
      })}
    </svg>
  );
}
