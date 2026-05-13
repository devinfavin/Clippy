import { Metric } from "./metric";
import { shortenEncoder } from "./encoder-name";
import type {
  CaptureMode,
  EncoderPref,
  MonitorInfo,
  ResolutionKind,
  SystemInfo,
} from "./replay-types";

function fmtMb(mb: number): string {
  if (mb >= 1024) return `${(mb / 1024).toFixed(2)} GB`;
  return `${Math.round(mb)} MB`;
}

export function ResourceImpact(props: {
  durationSecs: number;
  bitrateKbps: number;
  fps: number;
  encoderPref: EncoderPref;
  captureMode: CaptureMode;
  monitor: MonitorInfo | null;
  resKind: ResolutionKind;
  customW: number;
  customH: number;
  audioTrackCount: number;
  sys: SystemInfo | null;
}) {
  const {
    durationSecs,
    bitrateKbps,
    fps,
    encoderPref,
    captureMode,
    monitor,
    resKind,
    customW,
    customH,
    audioTrackCount,
    sys,
  } = props;

  // ----- per-save file size + buffer RAM -----
  const videoBytes = (bitrateKbps * 1000 * durationSecs) / 8;
  const audioBytesPerTrack = (192 * 1000 * durationSecs) / 8; // AAC 192 kbps
  const audioBytes = audioBytesPerTrack * Math.max(1, audioTrackCount);
  const totalBytes = (videoBytes + audioBytes) * 1.05; // ~5% MP4 container overhead
  const fileMb = totalBytes / (1024 * 1024);
  // The encoded packets sit in RAM until save, so buffer RAM tracks file size.
  const ramPerWorkerMb = fileMb;

  // ----- output resolution applied to VRAM math -----
  const srcW = monitor?.width ?? 1920;
  const srcH = monitor?.height ?? 1080;
  const [encW, encH] =
    resKind === "half"
      ? [Math.max(16, Math.floor(srcW / 2)), Math.max(16, Math.floor(srcH / 2))]
      : resKind === "custom"
        ? [Math.max(16, customW), Math.max(16, customH)]
        : [srcW, srcH];

  // ----- per-worker VRAM rough estimate -----
  const bgraBytes = srcW * srcH * 4 * 2; // WGC frame pool: 2 BGRA frames at source res
  const nv12Bytes = Math.ceil((encW * encH * 3) / 2) * 2; // VP output + encoder input at encode res
  const encoderWorkingMb = 180; // NVENC/AMF/QSV typical working set
  const vramPerWorkerMb = encoderWorkingMb + (bgraBytes + nv12Bytes) / (1024 * 1024);

  // ----- color tone vs system limits -----
  const ramTone =
    sys && sys.ram_total_mb > 0
      ? ramPerWorkerMb > sys.ram_total_mb * 0.25
        ? "warn"
        : ramPerWorkerMb > sys.ram_total_mb * 0.1
          ? "info"
          : "ok"
      : "info";
  const vramTone =
    sys && sys.gpu_vram_mb > 0
      ? vramPerWorkerMb > sys.gpu_vram_mb * 0.5
        ? "warn"
        : vramPerWorkerMb > sys.gpu_vram_mb * 0.25
          ? "info"
          : "ok"
      : "info";

  // ----- encoder capability hint (green/yellow/red) -----
  // Heuristic only — actual headroom depends on driver and encode preset.
  // Goal: catch obviously-impossible combinations before the user starts.
  const encoderHint = capabilityHint({ encW, encH, fps, encoderPref, bitrateKbps, sys });

  return (
    <div className="settings-calc">
      <div className="settings-calc-head">Estimated impact per saved clip</div>
      <div className="settings-calc-grid">
        <Metric label="File size" value={fmtMb(fileMb)} tone="ok" />
        <Metric
          label="Buffered RAM (per game)"
          value={fmtMb(ramPerWorkerMb)}
          tone={ramTone}
          aux={
            sys && sys.ram_total_mb > 0
              ? `of ${(sys.ram_total_mb / 1024).toFixed(0)} GB system RAM`
              : undefined
          }
        />
        <Metric
          label="GPU memory (per game)"
          value={fmtMb(vramPerWorkerMb)}
          tone={vramTone}
          aux={
            sys && sys.gpu_vram_mb > 0
              ? `of ${(sys.gpu_vram_mb / 1024).toFixed(1)} GB VRAM`
              : undefined
          }
        />
      </div>

      {encoderHint && (
        <div className={`settings-calc-hint tone-${encoderHint.tone}`}>
          <strong>{encoderHint.headline}</strong>
          {encoderHint.detail && <span> {encoderHint.detail}</span>}
        </div>
      )}

      <div className="settings-calc-system">
        {sys ? (
          <>
            <span title="Detected GPU adapter">
              <strong>GPU:</strong> {sys.gpu_name || "—"}
              {sys.gpu_vram_mb > 0 && ` (${(sys.gpu_vram_mb / 1024).toFixed(1)} GB)`}
            </span>
            <span title="Hardware H.264 encoders detected">
              <strong>HW encoders:</strong>{" "}
              {sys.hw_encoders.length > 0
                ? sys.hw_encoders.map(shortenEncoder).join(", ")
                : "none"}
            </span>
          </>
        ) : (
          <span>Probing hardware…</span>
        )}
      </div>

      {captureMode === "perWindow" && (
        <p className="settings-section-blurb settings-help">
          Per-window VRAM is per active game; if you tab between several games in
          one session each spawns its own buffer. The estimate uses your primary
          monitor's resolution as a proxy for the game window — actual size is
          unknown until a game is focused.
        </p>
      )}
    </div>
  );
}

/**
 * Rule-of-thumb capability check: flags impossible or unreliable combinations
 * (chosen encoder not detected, very high resolution × fps on integrated GPU,
 * software encoder past 1080p60) so the user gets an early warning instead of
 * a generic spawn failure once they hit Start.
 *
 * Exported for Tier 3 tests — keeps the math out of the component so we can
 * pin the rules in unit tests instead of grepping for branches via the DOM.
 */
export function capabilityHint(args: {
  encW: number;
  encH: number;
  fps: number;
  encoderPref: EncoderPref;
  bitrateKbps?: number;
  sys: SystemInfo | null;
}): { tone: "ok" | "info" | "warn"; headline: string; detail?: string } | null {
  const { encW, encH, fps, encoderPref, bitrateKbps, sys } = args;
  const pixels = encW * encH;
  const pixelRate = pixels * fps;

  // Vendor presence in detected encoders.
  const hasVendor = (needles: string[]) =>
    !!sys?.hw_encoders.some((n) =>
      needles.some((s) => n.toLowerCase().includes(s))
    );
  const present = {
    nvenc: hasVendor(["nvidia"]),
    amf: hasVendor(["amd", "amf"]),
    qsv: hasVendor(["intel", "quick sync"]),
  };

  if (encoderPref === "nvenc" && sys && !present.nvenc) {
    return {
      tone: "warn",
      headline: "NVENC not detected on this system.",
      detail: "Auto will fall back to whatever HW encoder Windows picks.",
    };
  }
  if (encoderPref === "amf" && sys && !present.amf) {
    return {
      tone: "warn",
      headline: "AMD AMF not detected.",
      detail: "Auto will fall back to whatever HW encoder Windows picks.",
    };
  }
  if (encoderPref === "qsv" && sys && !present.qsv) {
    return {
      tone: "warn",
      headline: "Intel Quick Sync not detected.",
      detail: "Auto will fall back to whatever HW encoder Windows picks.",
    };
  }
  if (encoderPref === "software") {
    if (pixelRate > 1920 * 1080 * 60) {
      return {
        tone: "warn",
        headline: "Software encoder above 1080p60 will likely drop frames.",
        detail: "Switch to a hardware encoder if available.",
      };
    }
    return {
      tone: "info",
      headline: "Software encoder uses CPU heavily — only for testing.",
    };
  }

  // High-bitrate guidance — at 75+ Mbps each 5-min buffer eats ~2.6 GB of
  // RAM and a saved clip is hundreds of MB. Most users don't need this for
  // anything they'd share; flag as info so it's a soft nudge, not a block.
  if (bitrateKbps != null && bitrateKbps >= 75_000) {
    return {
      tone: "info",
      headline: `${(bitrateKbps / 1000).toFixed(0)} Mbps is more than most clips need.`,
      detail:
        "High (50 Mbps) is visually indistinguishable for typical gameplay and uses half the RAM + disk. Keep Ultra only if you're archiving footage.",
    };
  }

  // Hardware ceiling heuristic: 1440p120 / 4K60 are usually fine on modern
  // discrete GPUs; 1440p240 / 4K120 push the encoder hard.
  const px4k60 = 3840 * 2160 * 60;
  if (pixelRate > px4k60 * 1.5) {
    return {
      tone: "warn",
      headline: "Very high pixel rate — only top-end GPUs sustain this without dropping.",
      detail: "Consider Half resolution, or step fps down to 60.",
    };
  }
  if (pixelRate > px4k60) {
    return {
      tone: "info",
      headline: "Heavy encode load. Watch the diag log for frame drops on first run.",
    };
  }
  return null;
}
