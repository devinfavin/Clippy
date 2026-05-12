import { memo } from "react";
import { useReplayStatus } from "./useReplayState";

/**
 * Compact pill for the topbar that surfaces replay-buffer state at a glance.
 *
 *   [ ● Valorant ]            — buffer active, capturing this game
 *   [ ● watching for games ]  — buffer running, no game window focused
 *   [ ● saving… ]             — flush in progress
 *   (nothing)                 — buffer is off
 *
 * The pill used to show buffered seconds (e.g. `[● 3:42 · Valorant]`), but
 * for typical multi-hour gaming sessions the buffer is full ~99% of the time
 * — the timer just looked frozen, leading users to wonder if the buffer was
 * stuck. The pulsing dot signals liveness instead. The expanded
 * "saving…/saved" mid-save state lands with the v0.2.4 multi-worker chip
 * redesign.
 *
 * Clicking the pill fires `onClick`, which the parent wires to the settings
 * modal so the user can configure the buffer without hunting for the gear.
 */
export const ReplayStatusPill = memo(ReplayStatusPillImpl);

function ReplayStatusPillImpl(props: { onClick?: () => void }) {
  const status = useReplayStatus(500);

  if (status.state === "Idle") return null;

  if (status.state === "Saving") {
    return (
      <button
        className="replay-pill replay-pill-saving"
        onClick={props.onClick}
        title="Replay buffer — saving"
      >
        <span className="replay-pill-dot" />
        saving…
      </button>
    );
  }

  if (status.state === "Watching") {
    return (
      <button
        className="replay-pill replay-pill-watching"
        onClick={props.onClick}
        title="Replay buffer is on — focus a game in the allowlist to start capturing"
      >
        <span className="replay-pill-dot" />
        <span className="replay-pill-where">watching for games</span>
      </button>
    );
  }

  // Active
  const title = status.window_title ?? "";
  const where = title.trim().length > 0 ? truncate(title, 32) : "no game focused";

  return (
    <button
      className="replay-pill replay-pill-active"
      onClick={props.onClick}
      title={`Replay buffer recording · ${title || "no game focused"}\nClick to configure`}
    >
      <span className="replay-pill-dot" />
      <span className="replay-pill-where">{where}</span>
    </button>
  );
}

function truncate(s: string, max: number): string {
  if (s.length <= max) return s;
  return s.slice(0, max - 1) + "…";
}
