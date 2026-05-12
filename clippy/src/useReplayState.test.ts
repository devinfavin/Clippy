import { describe, expect, it, vi } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import { isReplayRunning, useReplayStatus, type ReplayStatus } from "./useReplayState";
import { setTauriInvokeHandler } from "./test/setup";

// useReplayStatus polls the backend every `intervalMs`. Each poll calls
// invoke("get_replay_status") and stores whatever comes back. These tests
// pin every status variant + the polling-cadence contract so a refactor of
// the hook can't silently regress the pill behavior.
//
// We use real timers + a tight interval (25ms) instead of fake timers because
// the hook chains async invoke → setState → setTimeout, and vitest's fake-
// timer ↔ microtask interplay is fragile. With waitFor's default 1000ms
// timeout, the whole file runs in well under a second.

describe("isReplayRunning", () => {
  it("returns false only for the Idle state", () => {
    expect(isReplayRunning({ state: "Idle" })).toBe(false);
    expect(isReplayRunning({ state: "Watching" })).toBe(true);
    expect(isReplayRunning({ state: "Saving" })).toBe(true);
    expect(
      isReplayRunning({
        state: "Active",
        window_title: "Game",
        buffered_secs: 0,
        vram_mb: 0,
      })
    ).toBe(true);
  });
});

describe("useReplayStatus", () => {
  it("starts in Idle before the first poll resolves", () => {
    setTauriInvokeHandler(() => ({ state: "Watching" } as ReplayStatus));
    const { result } = renderHook(() => useReplayStatus(25));
    // Synchronously the hook returns its initial Idle state — the first
    // invoke is pending and won't resolve until microtasks flush.
    expect(result.current).toEqual({ state: "Idle" });
  });

  it("picks up the result of the first poll", async () => {
    setTauriInvokeHandler(() => ({ state: "Watching" } as ReplayStatus));
    const { result } = renderHook(() => useReplayStatus(25));
    await waitFor(() => expect(result.current.state).toBe("Watching"));
  });

  it("re-polls on the interval and reflects status transitions", async () => {
    // Step the hook through Watching → Active → Saving by mutating what the
    // invoke handler returns between polls. waitFor handles the polling
    // cadence + microtask flush for us.
    let next: ReplayStatus = { state: "Watching" };
    setTauriInvokeHandler(() => next);

    const { result } = renderHook(() => useReplayStatus(25));
    await waitFor(() => expect(result.current.state).toBe("Watching"));

    next = {
      state: "Active",
      window_title: "Valorant",
      buffered_secs: 42,
      vram_mb: 0,
    };
    await waitFor(() => {
      expect(result.current).toEqual({
        state: "Active",
        window_title: "Valorant",
        buffered_secs: 42,
        vram_mb: 0,
      });
    });

    next = { state: "Saving" };
    await waitFor(() => expect(result.current.state).toBe("Saving"));
  });

  it("keeps the last-known state when the backend errors out", async () => {
    // First poll succeeds → state is Active. Subsequent polls throw → state
    // must remain Active (don't snap back to Idle when the channel hiccups).
    let mode: "ok" | "throw" = "ok";
    setTauriInvokeHandler(() => {
      if (mode === "throw") throw new Error("simulated backend error");
      return {
        state: "Active",
        window_title: "X",
        buffered_secs: 1,
        vram_mb: 0,
      } as ReplayStatus;
    });

    const { result } = renderHook(() => useReplayStatus(25));
    await waitFor(() => expect(result.current.state).toBe("Active"));

    mode = "throw";
    // Let a few intervals' worth of failed polls go by, then verify state
    // didn't degrade.
    await new Promise((resolve) => setTimeout(resolve, 120));
    expect(result.current.state).toBe("Active");
  });

  it("stops polling after unmount", async () => {
    let calls = 0;
    setTauriInvokeHandler(() => {
      calls += 1;
      return { state: "Watching" } as ReplayStatus;
    });

    const { unmount, result } = renderHook(() => useReplayStatus(25));
    await waitFor(() => expect(result.current.state).toBe("Watching"));
    const callsBeforeUnmount = calls;

    unmount();
    // Several intervals' worth of time passes — no new calls should fire.
    await new Promise((resolve) => setTimeout(resolve, 150));
    // Allow at most 1 additional call (the one in flight when unmount fired).
    expect(calls - callsBeforeUnmount).toBeLessThanOrEqual(1);
  });
});

// Silence the "unused vi" warning when fake timers are disabled.
void vi;
