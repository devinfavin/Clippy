import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/** Mirror of Rust's ReplayStatus serde-tagged enum. */
export type ReplayStatus =
  | { state: "Idle" }
  | { state: "Watching" }
  | {
      state: "Active";
      window_title: string;
      buffered_secs: number;
      vram_mb: number;
    }
  | { state: "Saving" };

/** True iff the replay buffer is actually running (not Idle). */
export function isReplayRunning(s: ReplayStatus): boolean {
  return s.state !== "Idle";
}

/**
 * Poll the backend for replay buffer state. Cheap (a single mutex read +
 * clone on the Rust side) and only runs while mounted.
 */
export function useReplayStatus(intervalMs: number = 500): ReplayStatus {
  const [status, setStatus] = useState<ReplayStatus>({ state: "Idle" });

  useEffect(() => {
    let alive = true;
    let timer: number | null = null;

    const tick = async () => {
      try {
        const next = await invoke<ReplayStatus>("get_replay_status");
        if (alive) setStatus(next);
      } catch {
        // Backend not ready / command not found — keep last known state.
      }
      if (alive) timer = window.setTimeout(tick, intervalMs);
    };
    tick();

    return () => {
      alive = false;
      if (timer != null) window.clearTimeout(timer);
    };
  }, [intervalMs]);

  return status;
}
