import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ACTION_DESCRIPTIONS,
  ACTION_GROUP_LABELS,
  ACTION_GROUPS,
  ACTION_LABELS,
  formatKeybind,
  GLOBAL_ACTIONS,
  type ActionGroup,
  type ActionId,
  type Keybinds,
} from "./keybinds";
import type { UpdateState } from "./useUpdater";

const APP_VERSION = "0.1.0";

// ---------- Tab definitions ----------

export type SettingsTabId = "replay" | "keyboard" | "storage" | "about";

export const SETTINGS_TABS: readonly { id: SettingsTabId; label: string }[] = [
  { id: "replay", label: "Replay buffer" },
  { id: "keyboard", label: "Keyboard" },
  { id: "storage", label: "Storage" },
  { id: "about", label: "About" },
] as const;

// ---------- Keyboard tab ----------

/** Keyboard-shortcuts tab — two-column grid (action + description on the
 *  left, key combo button on the right), grouped by category. Each binding
 *  is still individually rebindable by clicking its combo; the Region 1-9
 *  jumps are collapsed into a single inline row of 9 mini-bindings under
 *  the "Regions" group to avoid 9 visually identical rows. */
export function KeyboardSettingsTab(props: {
  keybinds: Keybinds;
  listeningAction: ActionId | null;
  setListeningAction: (a: ActionId | null) => void;
}) {
  const { keybinds, listeningAction, setListeningAction } = props;
  const jumpRegionActions: ActionId[] = [
    "jumpRegion1", "jumpRegion2", "jumpRegion3",
    "jumpRegion4", "jumpRegion5", "jumpRegion6",
    "jumpRegion7", "jumpRegion8", "jumpRegion9",
  ];
  const isJumpRegion = (a: ActionId) =>
    (jumpRegionActions as string[]).includes(a as string);

  const groupOrder: ActionGroup[] = ["playback", "selection", "regions", "capture", "exports"];
  const grouped: Record<ActionGroup, ActionId[]> = {
    playback: [], selection: [], regions: [], capture: [], exports: [],
  };
  for (const action of Object.keys(ACTION_LABELS) as ActionId[]) {
    // Region 1-9 collapse into a single row appended at the end of "regions"
    if (isJumpRegion(action)) continue;
    grouped[ACTION_GROUPS[action]].push(action);
  }

  const renderRow = (action: ActionId) => {
    const isListening = listeningAction === action;
    const isGlobal = GLOBAL_ACTIONS.has(action);
    const conflicts = isGlobal
      ? []
      : (Object.keys(keybinds) as ActionId[]).filter(
          (other) =>
            other !== action &&
            !GLOBAL_ACTIONS.has(other) &&
            formatKeybind(keybinds[other]) === formatKeybind(keybinds[action])
        );
    return (
      <div key={action} className={`kb-row${conflicts.length ? " has-conflict" : ""}`}>
        <span className="kb-row-action">
          <span className="kb-row-action-name">
            {ACTION_LABELS[action]}
            {isGlobal && (
              <span className="kb-tag-global" title="Fires globally — works while Clippy is not focused">
                Global
              </span>
            )}
            {conflicts.length > 0 && (
              <span className="kb-conflict" title={`Conflicts with: ${conflicts.map((c) => ACTION_LABELS[c]).join(", ")}`}>
                conflict
              </span>
            )}
          </span>
          <span className="kb-row-action-desc">{ACTION_DESCRIPTIONS[action]}</span>
        </span>
        <button
          className={`kb-binding${isListening ? " listening" : ""}`}
          onClick={() => setListeningAction(action)}
        >
          {isListening ? "Press a key…  (Esc to cancel)" : formatKeybind(keybinds[action])}
        </button>
      </div>
    );
  };

  return (
    <section className="settings-section">
      <header className="settings-tab-header">
        <h3 className="settings-tab-title">Keyboard shortcuts</h3>
        <p className="settings-tab-blurb">
          Click any binding to record a new key combo. Globals (tagged below) fire
          even when Clippy is unfocused — they don't conflict with in-app bindings.
        </p>
      </header>
      <div className="kb-grid">
        {groupOrder.map((group) => {
          const rows = grouped[group];
          if (rows.length === 0 && group !== "regions") return null;
          return (
            <div key={group} className="kb-group">
              <p className="kb-group-label">{ACTION_GROUP_LABELS[group]}</p>
              {rows.map(renderRow)}
              {group === "regions" && (
                <div className="kb-row">
                  <span className="kb-row-action">
                    <span className="kb-row-action-name">Jump to region 1–9</span>
                    <span className="kb-row-action-desc">
                      Jump the playhead to a specific region by its index. Click any digit to rebind that one.
                    </span>
                  </span>
                  <span className="kb-jump-region-row">
                    {jumpRegionActions.map((action) => {
                      const isListening = listeningAction === action;
                      return (
                        <button
                          key={action}
                          className={`kb-binding kb-jump-region-btn${isListening ? " listening" : ""}`}
                          onClick={() => setListeningAction(action)}
                          title={`${ACTION_LABELS[action]} — click to rebind`}
                        >
                          {isListening ? "…" : formatKeybind(keybinds[action])}
                        </button>
                      );
                    })}
                  </span>
                </div>
              )}
            </div>
          );
        })}
      </div>
    </section>
  );
}

// ---------- Storage tab ----------

type StorageSummary = {
  app_data_dir: string;
  app_data_total_bytes: number;
  proxies_dir: string;
  proxies_bytes: number;
  diagnostics_log_path: string;
  diagnostics_log_bytes: number;
  other_bytes: number;
};

function fmtBytes(b: number): string {
  if (b >= 1_073_741_824) return `${(b / 1_073_741_824).toFixed(2)} GB`;
  if (b >= 1_048_576) return `${(b / 1_048_576).toFixed(1)} MB`;
  if (b >= 1024) return `${(b / 1024).toFixed(0)} KB`;
  if (b === 0) return "—";
  return `${b} B`;
}

/** Storage tab — per-component breakdown + open-data-folder + clear actions. */
export function StorageSettingsTab() {
  const [summary, setSummary] = useState<StorageSummary | null>(null);
  const [busy, setBusy] = useState<"cache" | "log" | null>(null);

  const refresh = useCallback(() => {
    invoke<StorageSummary>("storage_summary")
      .then(setSummary)
      .catch(() => setSummary(null));
  }, []);
  useEffect(() => { refresh(); }, [refresh]);

  const clearCache = async () => {
    setBusy("cache");
    try {
      await invoke("clear_cache");
    } catch {}
    setBusy(null);
    refresh();
  };
  const clearLog = async () => {
    setBusy("log");
    try {
      await invoke("clear_diagnostics_log");
    } catch {}
    setBusy(null);
    refresh();
  };
  const openDataDir = async () => {
    if (!summary) return;
    try {
      await invoke("reveal_in_folder", { path: summary.app_data_dir });
    } catch {}
  };

  return (
    <section className="settings-section">
      <header className="settings-tab-header">
        <h3 className="settings-tab-title">Storage</h3>
        <p className="settings-tab-blurb">
          Clippy keeps everything local under{" "}
          <code className="mono">%APPDATA%\Clippy\</code>. Files in the
          proxy cache untouched for 30 days are auto-pruned; clear manually below
          if you need the space back sooner.
        </p>
      </header>

      <div className="settings-storage-grid">
        <div className="settings-storage-row">
          <div className="settings-storage-info">
            <div className="settings-storage-label">Proxy cache</div>
            <div className="settings-storage-sub">Decoded MP4 proxies + waveforms used for fast scrubbing</div>
          </div>
          <div className="settings-storage-value mono">
            {summary ? fmtBytes(summary.proxies_bytes) : "—"}
          </div>
          <button
            className="settings-secondary-btn"
            onClick={clearCache}
            disabled={busy != null || !summary || summary.proxies_bytes === 0}
          >
            {busy === "cache" ? "Clearing…" : "Clear"}
          </button>
        </div>

        <div className="settings-storage-row">
          <div className="settings-storage-info">
            <div className="settings-storage-label">Diagnostics log</div>
            <div className="settings-storage-sub">
              <span className="mono">diagnostics.log</span> — appended on every graceful exit
            </div>
          </div>
          <div className="settings-storage-value mono">
            {summary ? fmtBytes(summary.diagnostics_log_bytes) : "—"}
          </div>
          <button
            className="settings-secondary-btn"
            onClick={clearLog}
            disabled={busy != null || !summary || summary.diagnostics_log_bytes === 0}
          >
            {busy === "log" ? "Clearing…" : "Clear"}
          </button>
        </div>

        <div className="settings-storage-row">
          <div className="settings-storage-info">
            <div className="settings-storage-label">Other</div>
            <div className="settings-storage-sub">Project state JSON, allowlist, save-folder pref</div>
          </div>
          <div className="settings-storage-value mono">
            {summary ? fmtBytes(summary.other_bytes) : "—"}
          </div>
          <span />
        </div>

        <div className="settings-storage-row settings-storage-row-total">
          <div className="settings-storage-info">
            <div className="settings-storage-label">Total app data</div>
            <div className="settings-storage-sub mono">
              {summary?.app_data_dir ?? ""}
            </div>
          </div>
          <div className="settings-storage-value mono">
            {summary ? fmtBytes(summary.app_data_total_bytes) : "—"}
          </div>
          <button
            className="settings-secondary-btn"
            onClick={openDataDir}
            disabled={!summary}
            title="Open this folder in File Explorer"
          >
            Open
          </button>
        </div>
      </div>
    </section>
  );
}

// ---------- About tab ----------

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
