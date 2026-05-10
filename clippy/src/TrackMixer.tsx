import type { AudioTrack, TrackMix } from "./types";
import { audioTrackLabel, trackColor } from "./types";

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
export function TrackMixer(props: {
  tracks: AudioTrack[];
  mix: TrackMix;
  onChange: (next: TrackMix) => void;
  /** "Default" if editing the source-wide mix, the region's 1-based number
   *  if editing a region's override. */
  contextLabel: string;
  /** Color used to tint the context badge so the user can see at a glance
   *  which region's mix is in scope. Null → neutral (default mix). */
  contextColor: string | null;
}) {
  if (props.tracks.length < 2) return null;

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
          const color = trackColor(t.index);
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
                <span className="track-dot" aria-hidden />
                {audioTrackLabel(t)}
              </span>
              <input
                type="range"
                className="track-slider"
                min={0}
                max={200}
                step={1}
                value={pct}
                onChange={(e) => update(t.index, { volume: parseInt(e.target.value, 10) / 100 })}
                disabled={muted}
                aria-label={`${audioTrackLabel(t)} volume`}
                style={
                  {
                    ["--fill-pct" as never]: `${Math.min(pct, 100)}%`,
                    ["--boost-pct" as never]: `${Math.max(0, pct - 100) / 1}%`,
                  } as React.CSSProperties
                }
              />
              <span className={`track-pct mono${vol > 1 ? " boost" : ""}`}>
                {muted ? "muted" : `${pct}%`}
              </span>
            </div>
          );
        })}
      </div>
    </section>
  );
}
