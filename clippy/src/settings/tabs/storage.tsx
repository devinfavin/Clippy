import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  SelectField,
  SettingsGroup,
  SettingsLabel,
  SettingsRow,
  Stepper,
} from "../primitives";

// ---- backend payloads ----
type StorageUsage = {
  save_dir: string;
  saved_replays_bytes: number;
  saved_replays_count: number;
  cache_bytes: number;
  other_bytes: number;
  total_bytes: number;
};
type StoragePruneResult = {
  freed_bytes: number;
  removed_count: number;
  removed_paths: string[];
  dry_run: boolean;
};

// ---- persisted settings ----
const LS = {
  capGb: "clippy.storage.cap_gb",     // 0 = no cap
  unkeptDays: "clippy.storage.unkept_days", // 0 = never
} as const;
const UNKEPT_OPTIONS = [
  { value: 0,  label: "Never" },
  { value: 7,  label: "After 7 days" },
  { value: 30, label: "After 30 days" },
  { value: 90, label: "After 90 days" },
] as const;
const GB = 1_073_741_824;

function loadNum(key: string, dflt: number): number {
  const raw = localStorage.getItem(key);
  if (raw == null) return dflt;
  const n = Number(raw);
  return Number.isFinite(n) ? n : dflt;
}

function fmtBytes(b: number): string {
  if (b >= GB) return `${(b / GB).toFixed(b >= 10 * GB ? 1 : 2)} GB`;
  if (b >= 1_048_576) return `${(b / 1_048_576).toFixed(1)} MB`;
  if (b >= 1024) return `${(b / 1024).toFixed(0)} KB`;
  if (b === 0) return "—";
  return `${b} B`;
}

/** Storage tab — usage breakdown, cap policy, auto-cleanup. Storage cap +
 *  unkept-cleanup are user-tunable here; the backend prune runs on every
 *  cap or unkept-days change (and also at app startup, from App.tsx). */
export function StorageSettingsTab() {
  const [usage, setUsage] = useState<StorageUsage | null>(null);
  const [busy, setBusy] = useState<"prune" | "open" | null>(null);
  const [capGb, setCapGbState] = useState<number>(() => loadNum(LS.capGb, 0));
  const [unkeptDays, setUnkeptDaysState] = useState<number>(() => loadNum(LS.unkeptDays, 0));
  const [confirm, setConfirm] = useState<StoragePruneResult | null>(null);

  const refresh = useCallback(() => {
    invoke<StorageUsage>("storage_usage")
      .then(setUsage)
      .catch(() => setUsage(null));
  }, []);
  useEffect(() => { refresh(); }, [refresh]);

  // Persist settings locally so the App.tsx startup prune can read them.
  useEffect(() => { localStorage.setItem(LS.capGb, String(capGb)); }, [capGb]);
  useEffect(() => { localStorage.setItem(LS.unkeptDays, String(unkeptDays)); }, [unkeptDays]);

  // Run the backend prune with the current settings. If we'd delete files,
  // gate behind a confirmation modal — the user shouldn't lose clips from a
  // setting change with no explicit go-ahead.
  const applyPolicy = useCallback(async (nextCapGb: number, nextUnkept: number) => {
    const cap_bytes = nextCapGb > 0 ? Math.round(nextCapGb * GB) : null;
    const unkept_max_days = nextUnkept > 0 ? nextUnkept : null;
    if (cap_bytes == null && unkept_max_days == null) return;
    try {
      const dry = await invoke<StoragePruneResult>("storage_prune", {
        capBytes: cap_bytes, unkeptMaxDays: unkept_max_days, dryRun: true,
      });
      if (dry.removed_count > 0) {
        setConfirm(dry);
      }
    } catch {}
  }, []);

  const setCapGb = (v: number) => {
    setCapGbState(v);
    void applyPolicy(v, unkeptDays);
  };
  const setUnkeptDays = (v: number) => {
    setUnkeptDaysState(v);
    void applyPolicy(capGb, v);
  };

  const confirmPrune = async () => {
    if (!confirm) return;
    setBusy("prune");
    try {
      const cap_bytes = capGb > 0 ? Math.round(capGb * GB) : null;
      const unkept_max_days = unkeptDays > 0 ? unkeptDays : null;
      await invoke<StoragePruneResult>("storage_prune", {
        capBytes: cap_bytes, unkeptMaxDays: unkept_max_days, dryRun: false,
      });
    } catch {}
    setBusy(null);
    setConfirm(null);
    refresh();
  };
  const cancelPrune = () => setConfirm(null);

  const openSaveDir = async () => {
    if (!usage) return;
    setBusy("open");
    try { await invoke("reveal_in_folder", { path: usage.save_dir }); } catch {}
    setBusy(null);
  };

  // Stacked bar + legend ----------------------------------------------------
  const total = usage?.total_bytes ?? 0;
  const cap = capGb > 0 ? capGb * GB : 0;
  const max = cap > 0 ? Math.max(cap, total) : total;
  const pctSaved = max > 0 ? ((usage?.saved_replays_bytes ?? 0) / max) * 100 : 0;
  const pctCache = max > 0 ? ((usage?.cache_bytes ?? 0) / max) * 100 : 0;
  const pctOther = max > 0 ? ((usage?.other_bytes ?? 0) / max) * 100 : 0;

  return (
    <section className="settings-tab-pane">
      <header>
        <h3 className="settings-tab-pane-title">Storage</h3>
        <p className="settings-tab-pane-blurb">
          Where Clippy saves your replays, and how much disk it's using. Cap +
          auto-cleanup let you keep things tidy without thinking about it.
        </p>
      </header>

      {/* Usage card — total + stacked bar + legend. Mirrors the design's
          big GB readout above a colored bar. */}
      <div className="storage-usage-card">
        <div className="storage-usage-top">
          <div>
            <div className="storage-usage-total mono">{fmtBytes(total)}</div>
            <div className="storage-usage-sub">
              {cap > 0
                ? `of ${fmtBytes(cap)} cap · ${fmtBytes(Math.max(0, cap - total))} free`
                : "No cap set"}
            </div>
          </div>
          <button
            className="settings-secondary-btn"
            onClick={openSaveDir}
            disabled={!usage || busy === "open"}
            title="Open the save folder in File Explorer"
          >
            Open folder
          </button>
        </div>

        <div className="storage-usage-bar">
          <span className="storage-bar-seg storage-bar-saved"  style={{ width: `${pctSaved}%` }} />
          <span className="storage-bar-seg storage-bar-cache"  style={{ width: `${pctCache}%` }} />
          <span className="storage-bar-seg storage-bar-other"  style={{ width: `${pctOther}%` }} />
        </div>

        <div className="storage-usage-legend">
          <LegendDot tone="saved" />
          <span>Saved replays</span>
          <span className="mono storage-legend-val">
            {fmtBytes(usage?.saved_replays_bytes ?? 0)}
          </span>
          <span className="storage-legend-spacer" />
          <LegendDot tone="cache" />
          <span>Cache</span>
          <span className="mono storage-legend-val">
            {fmtBytes(usage?.cache_bytes ?? 0)}
          </span>
          <span className="storage-legend-spacer" />
          <LegendDot tone="other" />
          <span>Other</span>
          <span className="mono storage-legend-val">
            {fmtBytes(usage?.other_bytes ?? 0)}
          </span>
        </div>
      </div>

      <div>
        <SettingsLabel>Location</SettingsLabel>
        <SettingsGroup>
          <SettingsRow
            title="Save folder"
            subtitle={<span className="mono">{usage?.save_dir ?? "…"}</span>}
          >
            <span className="mono s-row-stat">
              {usage ? `${usage.saved_replays_count} clip${usage.saved_replays_count === 1 ? "" : "s"}` : "—"}
            </span>
          </SettingsRow>
        </SettingsGroup>
      </div>

      <div>
        <SettingsLabel>Auto-cleanup</SettingsLabel>
        <SettingsGroup>
          <SettingsRow
            title="Storage cap"
            subtitle={
              capGb === 0
                ? "Off — Clippy won't trim saved replays automatically."
                : "When usage hits the cap, oldest replays are pruned first."
            }
          >
            <Stepper
              value={capGb}
              onChange={setCapGb}
              min={0}
              max={2000}
              step={50}
              unit={capGb === 0 ? "" : " GB"}
            />
          </SettingsRow>
          <SettingsRow
            title="Delete unkept replays after"
            subtitle="Only applies to clips you never opened in the editor."
          >
            <SelectField
              value={unkeptDays}
              onChange={setUnkeptDays}
              options={UNKEPT_OPTIONS}
              width={140}
            />
          </SettingsRow>
        </SettingsGroup>
      </div>

      {/* Confirmation modal — appears whenever a settings change would
          delete files. Stays inside the settings dialog so the user doesn't
          lose context. */}
      {confirm && confirm.removed_count > 0 && (
        <div className="storage-prune-confirm">
          <div className="storage-prune-confirm-body">
            <div className="storage-prune-confirm-title">
              Delete {confirm.removed_count} replay{confirm.removed_count === 1 ? "" : "s"}?
            </div>
            <div className="storage-prune-confirm-sub">
              This will free <strong>{fmtBytes(confirm.freed_bytes)}</strong>.
              Oldest files go first; anything you've opened in the editor is kept.
            </div>
            <div className="storage-prune-confirm-actions">
              <button
                className="settings-secondary-btn"
                onClick={cancelPrune}
                disabled={busy === "prune"}
              >
                Cancel
              </button>
              <button
                className="storage-prune-confirm-go"
                onClick={confirmPrune}
                disabled={busy === "prune"}
              >
                {busy === "prune" ? "Deleting…" : "Delete"}
              </button>
            </div>
          </div>
        </div>
      )}
    </section>
  );
}

function LegendDot(props: { tone: "saved" | "cache" | "other" }) {
  return <span className={`storage-legend-dot tone-${props.tone}`} aria-hidden />;
}
