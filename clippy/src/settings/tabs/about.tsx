import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { UpdateState } from "../../useUpdater";

const APP_VERSION = "0.3.2";

type AboutSystemInfo = {
  gpu_name: string;
  gpu_vram_mb: number;
  ram_total_mb: number;
  hw_encoders: string[];
};

/** About tab — version, system snapshot, diagnostics, report-a-bug. */
export function AboutTab(props: {
  updater: UpdateState;
  onCheckUpdates: () => void;
  onInstallUpdate: () => void;
}) {
  const [sys, setSys] = useState<AboutSystemInfo | null>(null);
  useEffect(() => {
    invoke<AboutSystemInfo>("replay_get_system_info")
      .then(setSys)
      .catch(() => {});
  }, []);

  const openIssues = async () => {
    try {
      // Tauri's opener plugin — opens the URL in the user's default browser
      // rather than inside the WebView.
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      await openUrl("https://github.com/devinfavin/Clippy/issues/new");
    } catch (e) {
      console.warn("[clippy] failed to open issues URL:", e);
    }
  };

  return (
    <section className="settings-section">
      <header className="settings-tab-header">
        <h3 className="settings-tab-title">About Clippy</h3>
        <p className="settings-tab-blurb">
          Local-only video clip editor for Windows. No telemetry, no cloud.
        </p>
      </header>

      <UpdaterPanel
        state={props.updater}
        onCheck={props.onCheckUpdates}
        onInstall={props.onInstallUpdate}
      />

      <div className="settings-about-grid">
        <div className="settings-about-row">
          <span className="settings-about-key">Version</span>
          <span className="settings-about-value mono">{APP_VERSION}</span>
        </div>
        <div className="settings-about-row">
          <span className="settings-about-key">Build</span>
          <span className="settings-about-value mono">
            {import.meta.env.DEV ? "dev" : "release"}
          </span>
        </div>
        <div className="settings-about-row">
          <span className="settings-about-key">GPU</span>
          <span className="settings-about-value mono" title={sys?.gpu_name}>
            {sys
              ? sys.gpu_vram_mb > 0
                ? `${sys.gpu_name} · ${(sys.gpu_vram_mb / 1024).toFixed(1)} GB`
                : sys.gpu_name || "—"
              : "probing…"}
          </span>
        </div>
        <div className="settings-about-row">
          <span className="settings-about-key">System RAM</span>
          <span className="settings-about-value mono">
            {sys && sys.ram_total_mb > 0
              ? `${(sys.ram_total_mb / 1024).toFixed(0)} GB`
              : "—"}
          </span>
        </div>
        <div className="settings-about-row">
          <span className="settings-about-key">HW encoders</span>
          <span className="settings-about-value mono">
            {sys
              ? sys.hw_encoders.length > 0
                ? sys.hw_encoders.map(shortenEncoderAbout).join(", ")
                : "none detected"
              : "probing…"}
          </span>
        </div>
      </div>

      <div className="settings-about-actions">
        <DiagnosticsButton />
        <button
          className="settings-secondary-btn"
          onClick={openIssues}
          title="Open the GitHub issues page in your browser"
        >
          Report an issue…
        </button>
      </div>

      <p className="settings-tab-help">
        Diagnostics include detected GPU, hardware encoders, audio devices, monitor
        list, and the most recent event log. Non-game window titles are redacted
        unless you turn on Verbose diagnostics under <strong>Replay buffer</strong>.
      </p>
    </section>
  );
}

function DiagnosticsButton() {
  const [state, setState] = useState<"idle" | "copied" | "error">("idle");
  const copy = async () => {
    try {
      const text = await invoke<string>("get_diagnostics");
      await navigator.clipboard.writeText(text);
      setState("copied");
      setTimeout(() => setState("idle"), 2500);
    } catch {
      setState("error");
      setTimeout(() => setState("idle"), 2500);
    }
  };
  return (
    <div className="cache-row">
      <span className="cache-label">Diagnostics</span>
      <span className="cache-size mono" style={{ fontSize: "var(--type-xs)", opacity: 0.6 }}>
        {state === "copied" ? "Copied to clipboard" : state === "error" ? "Copy failed" : "operation log"}
      </span>
      <button className="cache-clear" onClick={copy} title="Copy the diagnostic log to clipboard — paste it when reporting a bug">
        Copy log
      </button>
    </div>
  );
}

/** Auto-update panel. Mounted at the top of About, above the version grid.
 *  Renders one of four visual states: idle/uptodate (passive), available
 *  (call-to-action card), downloading/installing (progress), error
 *  (inline warning). The "Check for updates" button is always present so
 *  the user can manually re-check after fixing a network issue. */
function UpdaterPanel(props: {
  state: UpdateState;
  onCheck: () => void;
  onInstall: () => void;
}) {
  const { state } = props;

  // Available/downloading/installing get a full card; the rest sit as a
  // single inline row so they don't draw attention when nothing's new.
  if (state.kind === "available") {
    return (
      <div className="updater-card is-available">
        <div className="updater-card-head">
          <span className="updater-card-title">Update available</span>
          <span className="strategy-pill is-good">v{state.version}</span>
        </div>
        {state.notes && (
          <p className="updater-card-notes">{state.notes}</p>
        )}
        <div className="updater-card-actions">
          <button className="btn primary" onClick={props.onInstall}>
            Install and restart
          </button>
          <button className="btn ghost" onClick={props.onCheck}>
            Re-check
          </button>
        </div>
      </div>
    );
  }

  if (state.kind === "downloading") {
    const pct = state.total && state.total > 0
      ? Math.min(100, Math.round((state.downloaded / state.total) * 100))
      : null;
    return (
      <div className="updater-card is-busy">
        <div className="updater-card-head">
          <span className="updater-card-title">Downloading v{state.version}…</span>
          {pct != null && <span className="strategy-pill is-good">{pct}%</span>}
        </div>
        <div className="updater-progress">
          <div
            className="updater-progress-fill"
            style={{ width: pct != null ? `${pct}%` : "30%" }}
          />
        </div>
      </div>
    );
  }

  if (state.kind === "installing") {
    return (
      <div className="updater-card is-busy">
        <div className="updater-card-head">
          <span className="updater-card-title">Installing v{state.version}…</span>
        </div>
        <p className="updater-card-notes">Clippy will restart in a moment.</p>
      </div>
    );
  }

  // Idle / checking / uptodate / error all collapse to one row.
  let status: React.ReactNode;
  if (state.kind === "checking") {
    status = <span className="updater-row-status">Checking…</span>;
  } else if (state.kind === "uptodate") {
    status = <span className="updater-row-status">You're on the latest version.</span>;
  } else if (state.kind === "error") {
    status = (
      <span className="updater-row-status is-error" title={state.message}>
        Couldn't check ({state.message.slice(0, 60)}{state.message.length > 60 ? "…" : ""})
      </span>
    );
  } else {
    status = <span className="updater-row-status">Check for the latest version of Clippy.</span>;
  }

  return (
    <div className="updater-row">
      {status}
      <button
        className="settings-secondary-btn"
        onClick={props.onCheck}
        disabled={state.kind === "checking"}
      >
        {state.kind === "checking" ? "Checking…" : "Check for updates"}
      </button>
    </div>
  );
}

/** Same classification as ReplaySettings's shortenEncoder. Duplicated locally
 *  to keep About a self-contained tab without crossing module boundaries. */
function shortenEncoderAbout(name: string): string {
  const lower = name.toLowerCase();
  if (lower.includes("nvidia")) return "NVENC";
  if (lower.includes("amd") || lower.includes("amf")) return "AMF";
  if (lower.includes("intel") || lower.includes("quick sync")) return "QSV";
  return name.length > 32 ? name.slice(0, 29) + "…" : name;
}
