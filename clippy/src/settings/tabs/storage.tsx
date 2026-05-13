import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

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
