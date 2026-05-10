import { memo, useEffect, useRef, useState } from "react";
import type {
  AudioTrack,
  TrackColorOverrides,
  TrackMix,
  TrackNameOverrides,
} from "./types";
import { resolveTrackColor, resolveTrackName, TRACK_COLORS } from "./types";
import { ColorPicker } from "./ColorPicker";

/**
 * Per-track mute + volume slider for multi-audio sources (typical of
 * SteelSeries Sonar / OBS recordings with separate game / mic / Discord).
 *
 * Each row carries the track's palette color in three places — a left-edge
 * stripe, the name dot, and the slider thumb — so the same track is the
 * same color in the timeline waveform overlay above. Settings stored
 * sparsely; an absent entry means "default 1.0, unmuted".
 *
 * The header tells the user *which* mix they're editing — the source default
 * or a specific region — because per-region overrides auto-follow the
 * playhead. The same component edits both; the parent decides which slice
 * of state to write back to.
 */
// Wrapped in memo because it sits inside the App tree that re-renders on
// every scrub frame. Its props are reference-stable while the playhead is
// inside a single region (or outside any) — so for typical scrubbing this
// component skips the render entirely.
function TrackMixerImpl(props: {
  tracks: AudioTrack[];
  mix: TrackMix;
  onChange: (next: TrackMix) => void;
  /** Color overrides per stream-index. Click the dot on a track row to
   *  change its color via the ColorPicker. */
  trackColors: TrackColorOverrides;
  onTrackColorsChange: (next: TrackColorOverrides) => void;
  /** User-renamed labels per stream-index. Click the track name to edit. */
  trackNames: TrackNameOverrides;
  onTrackNamesChange: (next: TrackNameOverrides) => void;
  /** "Default" if editing the source-wide mix, the region's 1-based number
   *  if editing a region's override. */
  contextLabel: string;
  /** Color used to tint the context badge so the user can see at a glance
   *  which region's mix is in scope. Null → neutral (default mix). */
  contextColor: string | null;
}) {
  if (props.tracks.length < 1) return null;

  const update = (idx: number, patch: Partial<{ volume: number; muted: boolean }>) => {
    const cur = props.mix[idx] ?? { volume: 1, muted: false };
    const next: TrackMix = { ...props.mix, [idx]: { ...cur, ...patch } };
    props.onChange(next);
  };
  const reset = () => props.onChange({});

  const anyChange = props.tracks.some((t) => {
    const e = props.mix[t.index];
    return e && (e.muted || e.volume !== 1);
  });

  return (
    <section className="track-mixer" aria-label="Audio tracks">
      <header className="track-mixer-header">
        <span className="track-mixer-title">Audio mix</span>
        <span
          className="track-mixer-context"
          style={
            props.contextColor
              ? ({ ["--ctx-color" as never]: props.contextColor } as React.CSSProperties)
              : undefined
          }
          title={
            props.contextColor
              ? "Editing this region's mix override (auto-follows playhead). Move the playhead outside any region to edit the source default."
              : "Editing the source default mix. Move the playhead inside a region to edit that region's mix."
          }
        >
          {props.contextLabel}
        </span>
        <span className="track-mixer-count">
          {props.tracks.length} tracks
        </span>
        {anyChange && (
          <button
            className="track-mixer-reset"
            onClick={reset}
            title={`Reset ${props.contextLabel.toLowerCase()} to 100%`}
          >
            Reset
          </button>
        )}
      </header>
      <div className="track-mixer-rows">
        {props.tracks.map((t) => {
          const entry = props.mix[t.index] ?? { volume: 1, muted: false };
          const muted = entry.muted;
          const vol = entry.volume;
          const pct = Math.round(vol * 100);
          const color = resolveTrackColor(t.index, props.trackColors);
          const slot = props.trackColors[t.index] ?? (t.index % TRACK_COLORS.length);
          return (
            <div
              className={`track-row${muted ? " muted" : ""}`}
              key={t.index}
              style={
                {
                  ["--track-color" as never]: color,
                } as React.CSSProperties
              }
            >
              <span className="track-stripe" aria-hidden />
              <button
                className={`track-mute${muted ? " active" : ""}`}
                onClick={() => update(t.index, { muted: !muted })}
                title={muted ? "Unmute this track" : "Mute this track"}
                aria-pressed={muted}
              >
                <svg
                  width="16"
                  height="16"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  aria-hidden
                >
                  {muted ? (
                    <>
                      <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
                      <line x1="22" y1="9" x2="16" y2="15" />
                      <line x1="16" y1="9" x2="22" y2="15" />
                    </>
                  ) : (
                    <>
                      <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
                      <path d="M19.07 4.93a10 10 0 0 1 0 14.14" />
                      <path d="M15.54 8.46a5 5 0 0 1 0 7.07" />
                    </>
                  )}
                </svg>
              </button>
              <span className="track-name" title={t.codec + (t.layout ? ` · ${t.layout}` : "")}>
                <ColorPicker
                  className="track-dot"
                  colors={TRACK_COLORS}
                  selectedSlot={slot}
                  title="Change this track's color"
                  onChange={(newSlot) => {
                    const next = { ...props.trackColors, [t.index]: newSlot };
                    props.onTrackColorsChange(next);
                  }}
                />
                <EditableTrackName
                  track={t}
                  resolved={resolveTrackName(t, props.trackNames)}
                  onCommit={(value) => {
                    const next = { ...props.trackNames };
                    if (value.trim().length === 0) {
                      delete next[t.index];     // empty → remove override, fall back to default
                    } else {
                      next[t.index] = value.trim();
                    }
                    props.onTrackNamesChange(next);
                  }}
                />
              </span>
              <span className="track-slider-wrap">
                <span className="track-slider-tick" aria-hidden />
                <input
                  type="range"
                  className="track-slider"
                  min={0}
                  max={200}
                  step={1}
                  value={pct}
                  onChange={(e) => update(t.index, { volume: parseInt(e.target.value, 10) / 100 })}
                  disabled={muted}
                  aria-label={`${resolveTrackName(t, props.trackNames)} volume`}
                  style={
                    {
                      // Center-detent gradient stops: empty at unity (50%), fill
                      // grows outward toward the thumb in either direction.
                      ["--fill-start" as never]: `${Math.min(pct / 2, 50)}%`,
                      ["--fill-end" as never]: `${Math.max(pct / 2, 50)}%`,
                    } as React.CSSProperties
                  }
                />
              </span>
              <span className={`track-pct mono${vol > 1 ? " boost" : ""}`}>
                {muted ? "muted" : pct === 100 ? "default" : `${pct}%`}
              </span>
            </div>
          );
        })}
      </div>
    </section>
  );
}

export const TrackMixer = memo(TrackMixerImpl);

/**
 * Inline-editable track name. Renders the resolved name as plain text by
 * default; clicking switches to an `<input>` pre-filled with the same value.
 * Enter or blur commits, Esc cancels (no commit). Empty commit clears the
 * override so the name falls back to whatever the metadata title was.
 */
function EditableTrackName(props: {
  track: AudioTrack;
  resolved: string;
  onCommit: (value: string) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(props.resolved);
  const inputRef = useRef<HTMLInputElement>(null);

  // When opening edit mode, seed the draft with the current resolved name
  // and select all so the user can type-to-replace.
  useEffect(() => {
    if (editing) {
      setDraft(props.resolved);
      // Focus + select happens on next tick so the input has mounted.
      requestAnimationFrame(() => {
        inputRef.current?.focus();
        inputRef.current?.select();
      });
    }
  }, [editing, props.resolved]);

  if (!editing) {
    return (
      <span
        className="track-name-text"
        onClick={(e) => {
          e.stopPropagation();
          setEditing(true);
        }}
        title="Click to rename"
      >
        {props.resolved}
      </span>
    );
  }

  const commit = () => {
    setEditing(false);
    if (draft !== props.resolved) {
      props.onCommit(draft);
    }
  };
  const cancel = () => {
    setEditing(false);
    setDraft(props.resolved);
  };

  return (
    <input
      ref={inputRef}
      className="track-name-input"
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          commit();
        } else if (e.key === "Escape") {
          e.preventDefault();
          cancel();
        }
      }}
      // Don't let clicks/key events bubble to the chip-row (which would
      // toggle mute / fire other shortcuts).
      onClick={(e) => e.stopPropagation()}
      onMouseDown={(e) => e.stopPropagation()}
      maxLength={40}
      aria-label={`Rename ${props.resolved}`}
    />
  );
}
