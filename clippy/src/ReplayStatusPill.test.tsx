import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { ReplayStatusPill } from "./ReplayStatusPill";
import type { ReplayStatus } from "./useReplayState";

// The pill polls useReplayStatus internally. Replace the hook so each
// test can pin a specific status without timing-dependent invoke mocks.
vi.mock("./useReplayState", async () => {
  const actual = await vi.importActual<typeof import("./useReplayState")>(
    "./useReplayState"
  );
  return {
    ...actual,
    useReplayStatus: vi.fn<() => ReplayStatus>(() => ({ state: "Idle" })),
  };
});

import { useReplayStatus } from "./useReplayState";

function withStatus(s: ReplayStatus) {
  (useReplayStatus as unknown as ReturnType<typeof vi.fn>).mockReturnValue(s);
}

describe("ReplayStatusPill", () => {
  it("renders nothing when Idle", () => {
    withStatus({ state: "Idle" });
    const { container } = render(<ReplayStatusPill />);
    expect(container.firstChild).toBeNull();
  });

  it("shows watching-state label when buffer is on but no game is focused", () => {
    withStatus({ state: "Watching" });
    render(<ReplayStatusPill />);
    const btn = screen.getByRole("button");
    expect(btn).toHaveClass("replay-pill-watching");
    expect(btn).toHaveTextContent(/watching for games/i);
  });

  it("shows buffered time + game title when Active", () => {
    withStatus({
      state: "Active",
      window_title: "Valorant",
      buffered_secs: 222, // 3:42
      vram_mb: 0,
    });
    render(<ReplayStatusPill />);
    const btn = screen.getByRole("button");
    expect(btn).toHaveClass("replay-pill-active");
    expect(btn).toHaveTextContent("3:42");
    expect(btn).toHaveTextContent("Valorant");
  });

  it("formats buffered_secs as m:ss with zero-padded seconds", () => {
    withStatus({
      state: "Active",
      window_title: "game",
      buffered_secs: 65, // 1:05
      vram_mb: 0,
    });
    render(<ReplayStatusPill />);
    expect(screen.getByRole("button")).toHaveTextContent("1:05");
  });

  it("truncates long window titles", () => {
    const long = "ABCDEFGHIJKLMNOPQRSTUVWXYZ-0123456789-extra-trailing";
    withStatus({
      state: "Active",
      window_title: long,
      buffered_secs: 0,
      vram_mb: 0,
    });
    render(<ReplayStatusPill />);
    const btn = screen.getByRole("button");
    // 28-char limit with an ellipsis sentinel — don't pin exact text since
    // the truncate helper is private, but the original title must NOT be
    // rendered verbatim.
    expect(btn).not.toHaveTextContent(long);
    expect(btn.textContent ?? "").toContain("…");
  });

  it("falls back to 'no game focused' when title is empty", () => {
    withStatus({
      state: "Active",
      window_title: "",
      buffered_secs: 5,
      vram_mb: 0,
    });
    render(<ReplayStatusPill />);
    expect(screen.getByRole("button")).toHaveTextContent(/no game focused/i);
  });

  it("shows 'saving…' label during Saving state", () => {
    withStatus({ state: "Saving" });
    render(<ReplayStatusPill />);
    const btn = screen.getByRole("button");
    expect(btn).toHaveClass("replay-pill-saving");
    expect(btn).toHaveTextContent(/saving/i);
  });
});
