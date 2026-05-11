import { describe, expect, it } from "vitest";
import {
  capabilityHint,
  getReplayStartArgs,
  getSaveBehavior,
  getAutoStart,
  type SystemInfo,
} from "./ReplaySettings";

// `localStorage` is reset before every test by the global setup file,
// so each assertion below starts from a clean slate.

describe("getReplayStartArgs — defaults when localStorage is empty", () => {
  it("uses the per-window capture mode by default", () => {
    const args = getReplayStartArgs() as Record<string, unknown>;
    expect(args.captureMode).toEqual({ kind: "perWindow" });
  });

  it("hands back default bitrate=50_000, encoder=auto, 2s keyframe, 3 workers", () => {
    const args = getReplayStartArgs() as Record<string, unknown>;
    expect(args.bitrateKbps).toBe(50_000);
    expect(args.encoderPreference).toBe("auto");
    expect(args.keyframeIntervalSecs).toBe(2);
    expect(args.maxConcurrentWorkers).toBe(3);
  });

  it("does not ship fps — backend defaults to 60", () => {
    // fps was retired from the frontend; the worker always paces at 60 now.
    const args = getReplayStartArgs() as Record<string, unknown>;
    expect("fps" in args).toBe(false);
  });

  it("defaults resolutionMode to source", () => {
    const args = getReplayStartArgs() as Record<string, unknown>;
    expect(args.resolutionMode).toEqual({ kind: "source" });
  });

  it("defaults useProcessLoopback to true and audio arrays to empty", () => {
    const args = getReplayStartArgs() as Record<string, unknown>;
    expect(args.useProcessLoopback).toBe(true);
    expect(args.audioDeviceIds).toEqual([]);
    expect(args.audioDeviceNames).toEqual([]);
  });
});

describe("getReplayStartArgs — round-trip from localStorage", () => {
  it("ships values matching what the settings UI persists", () => {
    localStorage.setItem("clippy-replay-duration-secs", "240");
    localStorage.setItem("clippy-replay-bitrate-kbps", "100000");
    localStorage.setItem("clippy-replay-encoder-pref", "nvenc");
    localStorage.setItem("clippy-replay-keyframe-secs", "1");
    localStorage.setItem("clippy-replay-max-workers", "5");
    localStorage.setItem("clippy-replay-process-loopback", "false");

    const args = getReplayStartArgs() as Record<string, unknown>;
    expect(args.durationSecs).toBe(240);
    expect(args.bitrateKbps).toBe(100_000);
    expect(args.encoderPreference).toBe("nvenc");
    expect(args.keyframeIntervalSecs).toBe(1);
    expect(args.maxConcurrentWorkers).toBe(5);
    expect(args.useProcessLoopback).toBe(false);
  });

  it("emits resolutionMode={half} when half is persisted", () => {
    localStorage.setItem("clippy-replay-resolution-kind", "half");
    const args = getReplayStartArgs() as Record<string, unknown>;
    expect(args.resolutionMode).toEqual({ kind: "half" });
  });

  it("emits resolutionMode={custom, w, h} when custom is persisted", () => {
    localStorage.setItem("clippy-replay-resolution-kind", "custom");
    localStorage.setItem("clippy-replay-resolution-w", "2560");
    localStorage.setItem("clippy-replay-resolution-h", "1440");
    const args = getReplayStartArgs() as Record<string, unknown>;
    expect(args.resolutionMode).toEqual({
      kind: "custom",
      width: 2560,
      height: 1440,
    });
  });

  it("flips to monitor capture mode when both monitor key + hwnd are set", () => {
    localStorage.setItem("clippy-replay-capture-mode", "monitor");
    localStorage.setItem("clippy-replay-monitor-hwnd", "12345");
    const args = getReplayStartArgs() as Record<string, unknown>;
    expect(args.captureMode).toEqual({ kind: "monitor", hmonitor: "12345" });
  });

  it("falls back to perWindow even with mode=monitor if no hwnd was saved", () => {
    localStorage.setItem("clippy-replay-capture-mode", "monitor");
    const args = getReplayStartArgs() as Record<string, unknown>;
    expect(args.captureMode).toEqual({ kind: "perWindow" });
  });

  it("sends keyframeIntervalSecs as null when user picked Auto (value 0)", () => {
    localStorage.setItem("clippy-replay-keyframe-secs", "0");
    const args = getReplayStartArgs() as Record<string, unknown>;
    expect(args.keyframeIntervalSecs).toBeNull();
  });

  it("zips audio device names alongside ids in matching order", () => {
    localStorage.setItem(
      "clippy-replay-audio-device-ids",
      JSON.stringify(["dev-a", "dev-b", "dev-c"])
    );
    localStorage.setItem(
      "clippy-replay-audio-device-names",
      JSON.stringify({ "dev-a": "Game", "dev-c": "Chat" })
    );
    const args = getReplayStartArgs() as Record<string, unknown>;
    expect(args.audioDeviceIds).toEqual(["dev-a", "dev-b", "dev-c"]);
    // dev-b has no custom name → empty string keeps the parallel-array contract.
    expect(args.audioDeviceNames).toEqual(["Game", "", "Chat"]);
  });
});

describe("getSaveBehavior + getAutoStart helpers", () => {
  it("getSaveBehavior defaults to auto-open", () => {
    expect(getSaveBehavior()).toBe("auto-open");
  });

  it("getSaveBehavior reads notify when persisted", () => {
    localStorage.setItem("clippy-replay-save-behavior", "notify");
    expect(getSaveBehavior()).toBe("notify");
  });

  it("getAutoStart defaults to false", () => {
    expect(getAutoStart()).toBe(false);
  });

  it("getAutoStart returns true only for the literal 'true' string", () => {
    localStorage.setItem("clippy-replay-auto-start", "true");
    expect(getAutoStart()).toBe(true);
  });
});

// ---------- capability hint (resource calculator) ----------

const RTX: SystemInfo = {
  gpu_name: "NVIDIA GeForce RTX 4090",
  gpu_vram_mb: 24_000,
  ram_total_mb: 64_000,
  hw_encoders: ["NVIDIA H.264 Encoder MFT"],
};

const RADEON: SystemInfo = {
  gpu_name: "AMD Radeon RX 7900 XTX",
  gpu_vram_mb: 24_000,
  ram_total_mb: 32_000,
  hw_encoders: ["AMDh264Encoder"],
};

describe("capabilityHint", () => {
  it("warns when the chosen vendor encoder isn't present on the system", () => {
    const h = capabilityHint({
      encW: 1920,
      encH: 1080,
      fps: 60,
      encoderPref: "nvenc",
      sys: RADEON, // NVENC missing
    });
    expect(h?.tone).toBe("warn");
    expect(h?.headline).toMatch(/NVENC not detected/i);
  });

  it("does NOT warn when the vendor IS detected", () => {
    const h = capabilityHint({
      encW: 1920,
      encH: 1080,
      fps: 60,
      encoderPref: "nvenc",
      sys: RTX,
    });
    expect(h).toBeNull();
  });

  it("never warns about a missing vendor when sysinfo isn't loaded yet", () => {
    // Initial mount: the probe hasn't returned. Don't flash a scary message.
    const h = capabilityHint({
      encW: 1920,
      encH: 1080,
      fps: 60,
      encoderPref: "nvenc",
      sys: null,
    });
    expect(h).toBeNull();
  });

  it("flags software encoder past 1080p60 as a frame-drop risk", () => {
    const h = capabilityHint({
      encW: 1920,
      encH: 1080,
      fps: 120,
      encoderPref: "software",
      sys: RTX,
    });
    expect(h?.tone).toBe("warn");
    expect(h?.headline).toMatch(/software encoder/i);
  });

  it("flags software encoder at/below 1080p60 as info (not warn)", () => {
    const h = capabilityHint({
      encW: 1920,
      encH: 1080,
      fps: 60,
      encoderPref: "software",
      sys: RTX,
    });
    expect(h?.tone).toBe("info");
  });

  it("warns when pixel rate exceeds 1.5× 4K60", () => {
    // 4K240 ≈ 3840*2160*240 = 1.99B pix/s, comfortably above 1.5× threshold.
    const h = capabilityHint({
      encW: 3840,
      encH: 2160,
      fps: 240,
      encoderPref: "auto",
      sys: RTX,
    });
    expect(h?.tone).toBe("warn");
    expect(h?.headline).toMatch(/very high pixel rate/i);
  });

  it("returns null for the common 1080p60 / 1440p60 case", () => {
    expect(
      capabilityHint({ encW: 1920, encH: 1080, fps: 60, encoderPref: "auto", sys: RTX })
    ).toBeNull();
    expect(
      capabilityHint({ encW: 2560, encH: 1440, fps: 60, encoderPref: "auto", sys: RTX })
    ).toBeNull();
  });

  it("nudges (info) when bitrate ≥ 75 Mbps", () => {
    const h = capabilityHint({
      encW: 2560,
      encH: 1440,
      fps: 60,
      encoderPref: "auto",
      bitrateKbps: 100_000,
      sys: RTX,
    });
    expect(h?.tone).toBe("info");
    expect(h?.headline).toMatch(/100 Mbps/);
  });

  it("does NOT nudge for bitrates below 75 Mbps", () => {
    const h = capabilityHint({
      encW: 2560,
      encH: 1440,
      fps: 60,
      encoderPref: "auto",
      bitrateKbps: 50_000,
      sys: RTX,
    });
    expect(h).toBeNull();
  });
});
