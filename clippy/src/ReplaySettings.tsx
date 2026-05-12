import { memo, useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { enable as autostartEnable, disable as autostartDisable, isEnabled as autostartIsEnabled } from "@tauri-apps/plugin-autostart";
import { useReplayStatus, isReplayRunning } from "./useReplayState";

/** Mirror of Rust's capture::MonitorInfo. */
type MonitorInfo = {
  hmonitor: string;
  index: number;
  label: string;
  device: string;
  primary: boolean;
  width: number;
  height: number;
};

/** Mirror of Rust's audio::AudioDevice. */
type AudioDevice = {
  id: string;
  name: string;
  is_default: boolean;
};

/** Mirror of Rust's sysinfo::SystemInfo. */
export type SystemInfo = {
  gpu_name: string;
  gpu_vram_mb: number;
  ram_total_mb: number;
  hw_encoders: string[];
};

type CaptureMode = "perWindow" | "monitor";
export type SaveBehavior = "auto-open" | "notify";

/** Mirror of Rust's ReplaySettings.encoder_preference. */
export type EncoderPref = "auto" | "nvenc" | "amf" | "qsv" | "software";
/** Mirror of Rust's ReplaySettings.resolution_mode kind tag. */
type ResolutionKind = "source" | "half" | "custom";

const LS = {
  duration: "clippy-replay-duration-secs",
  mode: "clippy-replay-capture-mode",
  monitor: "clippy-replay-monitor-hwnd",
  saveBehavior: "clippy-replay-save-behavior",
  audioIds: "clippy-replay-audio-device-ids",
  audioNames: "clippy-replay-audio-device-names", // map: deviceId → custom name
  processLoopback: "clippy-replay-process-loopback",
  audioOpen: "clippy-replay-audio-section-open",
  gamesOpen: "clippy-replay-games-section-open",
  qualityOpen: "clippy-replay-quality-section-open",
  autoStart: "clippy-replay-auto-start",
  hideOnClose: "clippy-hide-on-close",
  diagVerbose: "clippy-diag-verbose",
  // Phase 7 quality controls. (`fps` retired — 60 is the only sane default
  // for replay buffers above 1080p; > 60 doesn't materially improve the
  // saved clip and complicates the encoder cadence model.)
  bitrate: "clippy-replay-bitrate-kbps",
  resolutionKind: "clippy-replay-resolution-kind",
  resolutionW: "clippy-replay-resolution-w",
  resolutionH: "clippy-replay-resolution-h",
  encoderPref: "clippy-replay-encoder-pref",
  keyframeSecs: "clippy-replay-keyframe-secs", // 0 = auto
  maxWorkers: "clippy-replay-max-workers",
} as const;

/** Bitrate presets (kbps). Custom = user-typed value. */
const BITRATE_PRESETS = [
  { label: "Low", kbps: 10_000 },
  { label: "Medium", kbps: 30_000 },
  { label: "High", kbps: 50_000 },
  { label: "Ultra", kbps: 100_000 },
] as const;

function lsNum(key: string, fallback: number): number {
  const v = localStorage.getItem(key);
  if (v == null) return fallback;
  const n = parseInt(v, 10);
  return isFinite(n) ? n : fallback;
}

function lsBool(key: string, fallback: boolean): boolean {
  const v = localStorage.getItem(key);
  if (v == null) return fallback;
  return v === "true";
}

function lsArr(key: string): string[] {
  try {
    const v = localStorage.getItem(key);
    if (!v) return [];
    const arr = JSON.parse(v);
    return Array.isArray(arr) ? arr.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function lsMap(key: string): Record<string, string> {
  try {
    const v = localStorage.getItem(key);
    if (!v) return {};
    const obj = JSON.parse(v);
    if (obj && typeof obj === "object" && !Array.isArray(obj)) {
      const out: Record<string, string> = {};
      for (const [k, val] of Object.entries(obj)) {
        if (typeof val === "string") out[k] = val;
      }
      return out;
    }
    return {};
  } catch {
    return {};
  }
}

export function getSaveBehavior(): SaveBehavior {
  return (localStorage.getItem(LS.saveBehavior) as SaveBehavior) ?? "auto-open";
}

/** Persisted "auto-start replay buffer when Clippy launches" flag. */
export function getAutoStart(): boolean {
  return localStorage.getItem(LS.autoStart) === "true";
}

/**
 * Build the argument object for `replay_start` from persisted settings so the
 * Settings UI's manual Start button and the App-mount auto-start path agree
 * on what to pass.
 */
export function getReplayStartArgs(): Record<string, unknown> {
  const durationSecs = lsNum(LS.duration, 180);
  const captureMode = (localStorage.getItem(LS.mode) as CaptureMode) ?? "perWindow";
  const monitorHwnd = localStorage.getItem(LS.monitor) ?? "";
  const audioDeviceIds = lsArr(LS.audioIds);
  const audioNamesMap = lsMap(LS.audioNames);
  // Parallel array matching audioDeviceIds order — empty string means
  // "no custom name" so the worker uses a sensible default.
  const audioDeviceNames = audioDeviceIds.map((id) => audioNamesMap[id] ?? "");
  const useProcessLoopback = lsBool(LS.processLoopback, true);
  const bitrateKbps = lsNum(LS.bitrate, 25_000);
  const encoderPreference = (localStorage.getItem(LS.encoderPref) as EncoderPref) ?? "auto";
  const keyframeSecs = lsNum(LS.keyframeSecs, 2);
  const maxConcurrentWorkers = lsNum(LS.maxWorkers, 3);

  const resKind = (localStorage.getItem(LS.resolutionKind) as ResolutionKind) ?? "source";
  let resolutionMode: unknown;
  if (resKind === "half") {
    resolutionMode = { kind: "half" };
  } else if (resKind === "custom") {
    const w = lsNum(LS.resolutionW, 1920);
    const h = lsNum(LS.resolutionH, 1080);
    resolutionMode = { kind: "custom", width: w, height: h };
  } else {
    resolutionMode = { kind: "source" };
  }

  const args: Record<string, unknown> = {
    durationSecs,
    audioDeviceIds,
    audioDeviceNames,
    useProcessLoopback,
    // fps retired from the frontend — backend defaults to 60.
    bitrateKbps,
    resolutionMode,
    encoderPreference,
    // Tauri's serde mapping for Option<Option<u32>>: pass null to disable
    // GOP override (encoder picks); a positive number sets seconds.
    keyframeIntervalSecs: keyframeSecs > 0 ? keyframeSecs : null,
    maxConcurrentWorkers,
  };
  if (captureMode === "monitor" && monitorHwnd) {
    args.captureMode = { kind: "monitor", hmonitor: monitorHwnd };
  } else {
    args.captureMode = { kind: "perWindow" };
  }
  return args;
}

export const ReplaySettings = memo(ReplaySettingsImpl);

function ReplaySettingsImpl() {
  const status = useReplayStatus(500);
  const isRunning = isReplayRunning(status);

  // ----- core settings -----
  const [durationSecs, setDurationSecs] = useState<number>(() => lsNum(LS.duration, 180));
  const [captureMode, setCaptureMode] = useState<CaptureMode>(
    () => (localStorage.getItem(LS.mode) as CaptureMode) ?? "perWindow"
  );
  const [monitorHwnd, setMonitorHwnd] = useState<string>(
    () => localStorage.getItem(LS.monitor) ?? ""
  );
  const [saveBehavior, setSaveBehavior] = useState<SaveBehavior>(getSaveBehavior);
  const [monitors, setMonitors] = useState<MonitorInfo[]>([]);

  // ----- audio settings -----
  const [audioDevices, setAudioDevices] = useState<AudioDevice[]>([]);
  const [selectedAudioIds, setSelectedAudioIds] = useState<Set<string>>(
    () => new Set(lsArr(LS.audioIds))
  );
  const [audioNames, setAudioNames] = useState<Record<string, string>>(
    () => lsMap(LS.audioNames)
  );
  const [useProcessLoopback, setUseProcessLoopback] = useState<boolean>(
    () => lsBool(LS.processLoopback, true)
  );

  // ----- games allowlist -----
  const [games, setGames] = useState<string[]>([]);
  const [recentGames, setRecentGames] = useState<string[]>([]);
  const [gameSearch, setGameSearch] = useState("");
  const [addCountdown, setAddCountdown] = useState<number | null>(null);

  // ----- accordion state -----
  const [audioOpen, setAudioOpen] = useState<boolean>(() => lsBool(LS.audioOpen, false));
  const [gamesOpen, setGamesOpen] = useState<boolean>(() => lsBool(LS.gamesOpen, false));
  const [qualityOpen, setQualityOpen] = useState<boolean>(() => lsBool(LS.qualityOpen, false));

  // ----- Phase 7 quality controls (fps removed — hardcoded 60 backend-side) -----
  const [bitrateKbps, setBitrateKbps] = useState<number>(() => lsNum(LS.bitrate, 25_000));
  const [resKind, setResKind] = useState<ResolutionKind>(
    () => (localStorage.getItem(LS.resolutionKind) as ResolutionKind) ?? "source"
  );
  const [customW, setCustomW] = useState<number>(() => lsNum(LS.resolutionW, 1920));
  const [customH, setCustomH] = useState<number>(() => lsNum(LS.resolutionH, 1080));
  const [encoderPref, setEncoderPref] = useState<EncoderPref>(
    () => (localStorage.getItem(LS.encoderPref) as EncoderPref) ?? "auto"
  );
  // 0 = "Auto" (let the encoder pick its default GOP).
  const [keyframeSecs, setKeyframeSecs] = useState<number>(() => lsNum(LS.keyframeSecs, 2));
  const [maxWorkers, setMaxWorkers] = useState<number>(() => lsNum(LS.maxWorkers, 3));

  // ----- launch / shutdown preferences -----
  const [autoStart, setAutoStart] = useState<boolean>(getAutoStart);
  const [hideOnClose, setHideOnCloseState] = useState<boolean>(() =>
    lsBool(LS.hideOnClose, false)
  );
  // Sync the runtime backend copy whenever the toggle changes (and on mount).
  useEffect(() => {
    invoke("set_hide_on_close", { enabled: hideOnClose }).catch(() => {});
    localStorage.setItem(LS.hideOnClose, String(hideOnClose));
  }, [hideOnClose]);

  // Verbose-diag opt-in. Default OFF so window titles of non-game apps
  // (browser tabs, document names) never end up in a copy-pasted diag report.
  const [diagVerbose, setDiagVerbose] = useState<boolean>(() =>
    lsBool(LS.diagVerbose, false)
  );
  useEffect(() => {
    invoke("set_diag_verbose", { enabled: diagVerbose }).catch(() => {});
    localStorage.setItem(LS.diagVerbose, String(diagVerbose));
  }, [diagVerbose]);
  // Whether Clippy itself launches on Windows boot. Backed by the OS's
  // login-items mechanism via the autostart plugin (not localStorage).
  const [autostartWithOs, setAutostartWithOs] = useState<boolean>(false);
  useEffect(() => {
    autostartIsEnabled()
      .then((v) => setAutostartWithOs(v))
      .catch(() => {});
  }, []);
  const toggleAutostartWithOs = useCallback(async (next: boolean) => {
    try {
      if (next) await autostartEnable();
      else await autostartDisable();
      setAutostartWithOs(next);
    } catch (e) {
      setError(`autostart: ${e}`);
    }
  }, []);

  // ----- replay save location -----
  // Read from backend (which holds the canonical Mutex<PathBuf> state) so
  // the displayed path always matches what `finish_save` will use.
  const [saveDir, setSaveDirState] = useState<string>("");
  const refreshSaveDir = useCallback(() => {
    invoke<string>("replay_get_save_dir")
      .then(setSaveDirState)
      .catch(() => {});
  }, []);
  useEffect(() => { refreshSaveDir(); }, [refreshSaveDir]);

  const browseSaveDir = useCallback(async () => {
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
        defaultPath: saveDir || undefined,
      });
      if (typeof picked === "string" && picked.length > 0) {
        await invoke("replay_set_save_dir", { path: picked });
        refreshSaveDir();
      }
    } catch (e) {
      setError(String(e));
    }
  }, [saveDir, refreshSaveDir]);

  const openSaveDir = useCallback(async () => {
    if (!saveDir) return;
    try {
      await invoke("reveal_in_folder", { path: saveDir });
    } catch (e) {
      setError(String(e));
    }
  }, [saveDir]);

  const resetSaveDir = useCallback(async () => {
    try {
      const def = await invoke<string>("replay_reset_save_dir");
      setSaveDirState(def);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // ----- system info (one-shot probe) -----
  const [sysInfo, setSysInfo] = useState<SystemInfo | null>(null);
  useEffect(() => {
    invoke<SystemInfo>("replay_get_system_info")
      .then(setSysInfo)
      .catch(() => {});
  }, []);

  // ----- transient -----
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Fetch monitors and audio devices once.
  useEffect(() => {
    let alive = true;
    invoke<MonitorInfo[]>("replay_list_monitors")
      .then((list) => {
        if (!alive) return;
        setMonitors(list);
        if (!monitorHwnd && list.length > 0) {
          const primary = list.find((m) => m.primary) ?? list[0];
          setMonitorHwnd(primary.hmonitor);
        }
      })
      .catch(() => {});
    invoke<AudioDevice[]>("replay_list_audio_devices")
      .then((d) => alive && setAudioDevices(d))
      .catch(() => {});
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const refreshGames = useCallback(async () => {
    try {
      const [list, recent] = await Promise.all([
        invoke<string[]>("replay_list_games"),
        invoke<string[]>("replay_recent_games"),
      ]);
      setGames(list);
      setRecentGames(recent);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // Fetch games when the section opens (avoid eager fetch if user never opens it).
  useEffect(() => {
    if (gamesOpen) refreshGames();
  }, [gamesOpen, refreshGames]);

  // Persistence.
  useEffect(() => { localStorage.setItem(LS.duration, String(durationSecs)); }, [durationSecs]);
  useEffect(() => { localStorage.setItem(LS.mode, captureMode); }, [captureMode]);
  useEffect(() => { if (monitorHwnd) localStorage.setItem(LS.monitor, monitorHwnd); }, [monitorHwnd]);
  useEffect(() => { localStorage.setItem(LS.saveBehavior, saveBehavior); }, [saveBehavior]);
  useEffect(() => {
    localStorage.setItem(LS.audioIds, JSON.stringify([...selectedAudioIds]));
  }, [selectedAudioIds]);
  useEffect(() => {
    localStorage.setItem(LS.audioNames, JSON.stringify(audioNames));
  }, [audioNames]);
  useEffect(() => {
    localStorage.setItem(LS.processLoopback, String(useProcessLoopback));
  }, [useProcessLoopback]);
  useEffect(() => { localStorage.setItem(LS.audioOpen, String(audioOpen)); }, [audioOpen]);
  useEffect(() => { localStorage.setItem(LS.gamesOpen, String(gamesOpen)); }, [gamesOpen]);
  useEffect(() => { localStorage.setItem(LS.qualityOpen, String(qualityOpen)); }, [qualityOpen]);
  useEffect(() => { localStorage.setItem(LS.autoStart, String(autoStart)); }, [autoStart]);
  useEffect(() => { localStorage.setItem(LS.bitrate, String(bitrateKbps)); }, [bitrateKbps]);
  useEffect(() => { localStorage.setItem(LS.resolutionKind, resKind); }, [resKind]);
  useEffect(() => { localStorage.setItem(LS.resolutionW, String(customW)); }, [customW]);
  useEffect(() => { localStorage.setItem(LS.resolutionH, String(customH)); }, [customH]);
  useEffect(() => { localStorage.setItem(LS.encoderPref, encoderPref); }, [encoderPref]);
  useEffect(() => { localStorage.setItem(LS.keyframeSecs, String(keyframeSecs)); }, [keyframeSecs]);
  useEffect(() => { localStorage.setItem(LS.maxWorkers, String(maxWorkers)); }, [maxWorkers]);

  const start = useCallback(async () => {
    setError(null);
    setBusy(true);
    try {
      if (captureMode === "monitor" && !monitorHwnd) {
        throw new Error("pick a display first");
      }
      // localStorage was already updated by the persistence effects above,
      // so the helper sees the latest user choices.
      await invoke("replay_start", getReplayStartArgs());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [captureMode, monitorHwnd]);

  const stop = useCallback(async () => {
    setError(null);
    setBusy(true);
    try {
      await invoke("replay_stop");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  // ----- games actions -----
  const rescanLaunchers = useCallback(async () => {
    setError(null);
    try {
      await invoke<number>("replay_rescan_games");
      await refreshGames();
    } catch (e) {
      setError(String(e));
    }
  }, [refreshGames]);

  const addCurrentForeground = useCallback(() => {
    if (addCountdown != null) return;
    let n = 3;
    setAddCountdown(n);
    const tick = () => {
      n -= 1;
      if (n > 0) {
        setAddCountdown(n);
        setTimeout(tick, 1000);
      } else {
        setAddCountdown(null);
        invoke<string>("replay_add_current_game")
          .then(() => refreshGames())
          .catch((e) => setError(String(e)));
      }
    };
    setTimeout(tick, 1000);
  }, [addCountdown, refreshGames]);

  const browseAndAdd = useCallback(async () => {
    try {
      const picked = await openDialog({
        multiple: false,
        filters: [{ name: "Game executable", extensions: ["exe"] }],
      });
      if (typeof picked === "string" && picked.length > 0) {
        await invoke("replay_add_game", { exePath: picked });
        await refreshGames();
      }
    } catch (e) {
      setError(String(e));
    }
  }, [refreshGames]);

  const removeGame = useCallback(
    async (path: string) => {
      try {
        await invoke<boolean>("replay_remove_game", { exePath: path });
        await refreshGames();
      } catch (e) {
        setError(String(e));
      }
    },
    [refreshGames]
  );

  // ----- audio helpers -----
  const toggleAudioDevice = useCallback((id: string) => {
    setSelectedAudioIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const setAudioName = useCallback((id: string, name: string) => {
    setAudioNames((prev) => {
      const next = { ...prev };
      const trimmed = name.trim();
      if (trimmed.length === 0) delete next[id];
      else next[id] = trimmed;
      return next;
    });
  }, []);

  // Push the current device-id → friendly-name map down to the backend so
  // renames mid-buffer-session reach the next save. Debounced 300 ms so
  // typing doesn't spam the invoke channel; fires once on mount with the
  // localStorage-loaded map so the worker also sees pre-existing names.
  useEffect(() => {
    const t = window.setTimeout(() => {
      invoke("replay_set_audio_names", { names: audioNames }).catch((e) =>
        console.warn("[clippy] replay_set_audio_names failed:", e)
      );
    }, 300);
    return () => window.clearTimeout(t);
  }, [audioNames]);

  // ----- derived -----
  const minutes = (durationSecs / 60).toFixed(1).replace(/\.0$/, "");
  // (filteredGames + Recently-added / Steam / Manual grouping moved into
  // GamesTrackedList — it owns its own filtering + auto-expand-on-search.)

  return (
    <section className="settings-section">
      <header className="settings-tab-header">
        <h3 className="settings-tab-title">Replay buffer</h3>
        <p className="settings-tab-blurb">
          Continuously captures the last few minutes of focused gameplay so you can save a clip after the fact. Press your save hotkey to flush the buffer to an MP4.
        </p>
      </header>

      <div className="settings-row">
        <button
          className={`settings-primary-btn${isRunning ? " is-on" : ""}`}
          onClick={isRunning ? stop : start}
          disabled={busy}
        >
          {busy ? "…" : isRunning ? "Stop replay buffer" : "Start replay buffer"}
        </button>
        <span className="settings-aux">
          {!isRunning
            ? "Off"
            : status.state === "Saving"
              ? "Saving…"
              : status.state === "Active"
                ? `Recording: ${status.window_title || "current target"}`
                : captureMode === "monitor"
                  ? "Watching the selected display"
                  : "Watching for games — focus a game in the allowlist to start capturing"}
        </span>
      </div>

      {error && <div className="settings-error">{error}</div>}

      <label className="settings-checkbox">
        <input
          type="checkbox"
          checked={autostartWithOs}
          onChange={(e) => toggleAutostartWithOs(e.target.checked)}
        />
        <span>Start Clippy when Windows starts</span>
      </label>

      <label className="settings-checkbox">
        <input
          type="checkbox"
          checked={autoStart}
          onChange={(e) => setAutoStart(e.target.checked)}
        />
        <span>Start the replay buffer when Clippy launches</span>
      </label>

      <label className="settings-checkbox">
        <input
          type="checkbox"
          checked={hideOnClose}
          onChange={(e) => setHideOnCloseState(e.target.checked)}
        />
        <span>
          Close to system tray (keep Clippy running in the background)
          <span className="settings-aux"> Right-click the tray icon to fully quit.</span>
        </span>
      </label>

      <label className="settings-checkbox">
        <input
          type="checkbox"
          checked={diagVerbose}
          onChange={(e) => setDiagVerbose(e.target.checked)}
        />
        <span>
          Verbose diagnostics (log non-game window titles)
          <span className="settings-aux">
            {" "}
            Off by default for privacy — non-game window titles (browser tabs,
            document names) appear as &lt;non-game window&gt; in the log unless
            this is on.
          </span>
        </span>
      </label>

      <div className="settings-row">
        <label className="settings-label">Buffer duration</label>
        <input
          type="range"
          min={60}
          max={600}
          step={30}
          value={durationSecs}
          onChange={(e) => setDurationSecs(parseInt(e.target.value, 10))}
          disabled={isRunning}
        />
        <span className="settings-value mono">{minutes} min</span>
      </div>

      <div className="settings-row">
        <label className="settings-label">Capture mode</label>
        <select
          value={captureMode}
          onChange={(e) => setCaptureMode(e.target.value as CaptureMode)}
          disabled={isRunning}
        >
          <option value="perWindow">Per-window (game allowlist)</option>
          <option value="monitor">Full screen (single display)</option>
        </select>
      </div>

      {captureMode === "monitor" && (
        <div className="settings-row">
          <label className="settings-label">Display</label>
          <select
            value={monitorHwnd}
            onChange={(e) => setMonitorHwnd(e.target.value)}
            disabled={isRunning}
          >
            {monitors.length === 0 && <option value="">(no displays detected)</option>}
            {monitors.map((m) => (
              <option key={m.hmonitor} value={m.hmonitor}>
                {m.label} — {m.width}×{m.height}
              </option>
            ))}
          </select>
        </div>
      )}

      <div className="settings-row">
        <label className="settings-label">When a replay is saved</label>
        <select
          value={saveBehavior}
          onChange={(e) => setSaveBehavior(e.target.value as SaveBehavior)}
        >
          <option value="auto-open">Open in editor immediately</option>
          <option value="notify">Show a notification (click to open)</option>
        </select>
      </div>

      {/* ---------------- Save location ---------------- */}
      <div className="settings-row settings-row-stack">
        <label className="settings-label">Save folder</label>
        <div className="settings-path-row">
          <span className="settings-path mono" title={saveDir}>
            {saveDir || "(loading…)"}
          </span>
          <button
            className="settings-secondary-btn"
            onClick={openSaveDir}
            disabled={!saveDir}
            title="Open this folder in File Explorer"
          >
            Open
          </button>
          <button
            className="settings-secondary-btn"
            onClick={browseSaveDir}
            title="Pick a different folder for saved replays"
          >
            Browse…
          </button>
          <button
            className="settings-secondary-btn"
            onClick={resetSaveDir}
            title="Restore the default (Videos\Clippy Replays)"
          >
            Reset
          </button>
        </div>
      </div>

      {/* ---------------- Audio sources ---------------- */}
      <Accordion
        open={audioOpen}
        onToggle={() => setAudioOpen((v) => !v)}
        title="Audio sources"
        summary={
          selectedAudioIds.size > 0
            ? `${selectedAudioIds.size} device${selectedAudioIds.size === 1 ? "" : "s"} selected`
            : "Default output (no devices selected)"
        }
      >
        <label className="settings-checkbox">
          <input
            type="checkbox"
            checked={useProcessLoopback}
            onChange={(e) => setUseProcessLoopback(e.target.checked)}
            disabled={isRunning}
          />
          <span>
            Try to capture only the focused game's audio
            <span className="settings-aux"> (Process Loopback, Win11 22H2+; falls back to system audio if unavailable)</span>
          </span>
        </label>

        <p className="settings-section-blurb" style={{ marginTop: "var(--s-2)" }}>
          Pick output devices to capture as separate audio tracks in the saved MP4. Useful if you have a virtual audio router (SteelSeries Sonar, Voicemeeter) splitting game / chat / music to different outputs.
        </p>

        {audioDevices.length === 0 ? (
          <p className="settings-section-blurb">No render devices detected.</p>
        ) : (
          <AudioDeviceGroups
            devices={audioDevices}
            selectedIds={selectedAudioIds}
            audioNames={audioNames}
            isRunning={isRunning}
            onToggle={toggleAudioDevice}
            onRename={setAudioName}
          />
        )}
        {selectedAudioIds.size === 0 && (
          <p className="settings-section-blurb settings-help">
            With no devices selected, the buffer falls back to capturing the default output.
          </p>
        )}
      </Accordion>

      {/* ---------------- Quality controls ---------------- */}
      <Accordion
        open={qualityOpen}
        onToggle={() => setQualityOpen((v) => !v)}
        title="Quality & performance"
        summary={`${(bitrateKbps / 1000).toFixed(0)} Mbps · ${
          resKind === "source" ? "source res" : resKind === "half" ? "½ res" : `${customW}×${customH}`
        }${encoderPref !== "auto" ? ` · ${encoderPref.toUpperCase()}` : ""}`}
      >
        <div className="settings-row">
          <label className="settings-label">Resolution</label>
          <select
            value={resKind}
            onChange={(e) => setResKind(e.target.value as ResolutionKind)}
            disabled={isRunning}
          >
            <option value="source">Source (match captured surface)</option>
            <option value="half">Half (¼ pixels — lighter on disk)</option>
            <option value="custom">Custom…</option>
          </select>
          {resKind === "custom" && (
            <span className="settings-row-inline">
              <input
                type="number"
                className="settings-text settings-num"
                min={64}
                max={7680}
                step={16}
                value={customW}
                onChange={(e) => setCustomW(parseInt(e.target.value, 10) || 0)}
                disabled={isRunning}
              />
              <span className="settings-aux">×</span>
              <input
                type="number"
                className="settings-text settings-num"
                min={64}
                max={4320}
                step={16}
                value={customH}
                onChange={(e) => setCustomH(parseInt(e.target.value, 10) || 0)}
                disabled={isRunning}
              />
            </span>
          )}
        </div>

        <div className="settings-row">
          <label className="settings-label">Bitrate</label>
          <select
            value={
              BITRATE_PRESETS.find((p) => p.kbps === bitrateKbps)?.kbps ?? -1
            }
            onChange={(e) => {
              const v = parseInt(e.target.value, 10);
              if (v > 0) setBitrateKbps(v);
            }}
            disabled={isRunning}
          >
            {BITRATE_PRESETS.map((p) => (
              <option key={p.label} value={p.kbps}>
                {p.label} — {p.kbps / 1000} Mbps
              </option>
            ))}
            <option value={-1}>Custom…</option>
          </select>
          {!BITRATE_PRESETS.some((p) => p.kbps === bitrateKbps) && (
            <span className="settings-row-inline">
              <input
                type="number"
                className="settings-text settings-num"
                min={1000}
                max={200000}
                step={1000}
                value={bitrateKbps}
                onChange={(e) =>
                  setBitrateKbps(parseInt(e.target.value, 10) || 0)
                }
                disabled={isRunning}
              />
              <span className="settings-aux">kbps</span>
            </span>
          )}
        </div>

        <div className="settings-row">
          <label className="settings-label">Encoder</label>
          <select
            value={encoderPref}
            onChange={(e) => setEncoderPref(e.target.value as EncoderPref)}
            disabled={isRunning}
          >
            {/* Auto label includes the resolved vendor so the user isn't
                left guessing what "Auto" will actually pick. */}
            <option value="auto">
              {sysInfo
                ? `Auto · ${resolvedAutoEncoder(sysInfo)}`
                : "Auto (let Windows pick)"}
            </option>
            <option value="nvenc">NVIDIA NVENC</option>
            <option value="amf">AMD AMF</option>
            <option value="qsv">Intel Quick Sync</option>
            <option value="software">Software (CPU fallback)</option>
          </select>
        </div>

        <div className="settings-row">
          <label className="settings-label">Keyframe interval</label>
          <select
            value={keyframeSecs}
            onChange={(e) => setKeyframeSecs(parseInt(e.target.value, 10))}
            disabled={isRunning}
          >
            <option value={0}>Auto (encoder default)</option>
            <option value={1}>1 second</option>
            <option value={2}>2 seconds</option>
            <option value={4}>4 seconds</option>
          </select>
          <span className="settings-aux">tighter trims at save</span>
        </div>

        <div className="settings-row">
          <label className="settings-label">Max games captured</label>
          <input
            type="number"
            className="settings-text settings-num"
            min={1}
            max={10}
            step={1}
            value={maxWorkers}
            onChange={(e) =>
              setMaxWorkers(Math.max(1, Math.min(10, parseInt(e.target.value, 10) || 1)))
            }
            disabled={isRunning}
          />
          <span className="settings-aux">
            oldest is evicted when you focus a new game past the cap
          </span>
        </div>
      </Accordion>

      {/* ---------------- Resource impact calculator ---------------- *
       *  Source-resolution policy:
       *    - monitor mode → the user's chosen display
       *    - per-window mode → primary monitor (best proxy for the game,
       *      whose window size we can't know until focus). Final fallback
       *      to 1920×1080 lives inside ResourceImpact for the no-monitor
       *      edge case (HMONITOR enumeration empty).                       */}
      <ResourceImpact
        durationSecs={durationSecs}
        bitrateKbps={bitrateKbps}
        fps={60}
        encoderPref={encoderPref}
        captureMode={captureMode}
        monitor={
          captureMode === "monitor"
            ? monitors.find((m) => m.hmonitor === monitorHwnd) ?? null
            : monitors.find((m) => m.primary) ?? monitors[0] ?? null
        }
        resKind={resKind}
        customW={customW}
        customH={customH}
        audioTrackCount={
          (selectedAudioIds.size > 0 ? selectedAudioIds.size : 1) +
          (captureMode === "perWindow" && useProcessLoopback ? 1 : 0)
        }
        sys={sysInfo}
      />

      {/* ---------------- Games allowlist ---------------- */}
      <Accordion
        open={gamesOpen}
        onToggle={() => setGamesOpen((v) => !v)}
        title="Games tracked"
        summary={`${games.length} entr${games.length === 1 ? "y" : "ies"}`}
      >
        <p className="settings-section-blurb" style={{ marginTop: 0 }}>
          The buffer only captures windows whose process is in this list. Steam games auto-detect; non-Steam games (Battle.net, Riot, Epic, itch.io builds) add them manually below.
        </p>

        <div className="settings-row" style={{ flexWrap: "wrap" }}>
          <button className="settings-secondary-btn" onClick={rescanLaunchers}>
            Rescan launchers
          </button>
          <button
            className="settings-secondary-btn"
            onClick={addCurrentForeground}
            disabled={addCountdown != null}
            title="Switch to your game window now; the foreground process will be added when the countdown ends."
          >
            {addCountdown != null
              ? `Switch to your game… ${addCountdown}`
              : "Add current foreground (3s delay)"}
          </button>
          <button className="settings-secondary-btn" onClick={browseAndAdd}>
            Add by .exe path…
          </button>
        </div>

        <div className="settings-row">
          <input
            className="settings-text"
            placeholder="Search games…"
            value={gameSearch}
            onChange={(e) => setGameSearch(e.target.value)}
          />
        </div>

        <GamesTrackedList
          games={games}
          recentGames={recentGames}
          search={gameSearch}
          onRemove={removeGame}
        />
      </Accordion>
    </section>
  );
}

// (baseExe + isSteamPath helpers live with the GamesTrackedList component below.)

// ---------- Resource impact panel ----------

function fmtMb(mb: number): string {
  if (mb >= 1024) return `${(mb / 1024).toFixed(2)} GB`;
  return `${Math.round(mb)} MB`;
}

function ResourceImpact(props: {
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

function shortenEncoder(name: string): string {
  // "NVIDIA H.264 Encoder MFT" → "NVENC"; "Intel® Quick Sync …" → "QSV"; etc.
  const lower = name.toLowerCase();
  if (lower.includes("nvidia")) return "NVENC";
  if (lower.includes("amd") || lower.includes("amf")) return "AMF";
  if (lower.includes("intel") || lower.includes("quick sync")) return "QSV";
  return name.length > 32 ? name.slice(0, 29) + "…" : name;
}

/**
 * Inspect the detected HW encoder list and return the vendor that Media
 * Foundation's SORTANDFILTER ordering will pick first under `Auto`. Matches
 * the priority NVENC → AMF → QSV used by `pick_encoder` in encoder.rs, so
 * the dropdown's subtitle is honest about what the worker will activate.
 */
function resolvedAutoEncoder(sys: SystemInfo): string {
  const has = (needle: string) =>
    sys.hw_encoders.some((n) => n.toLowerCase().includes(needle));
  if (has("nvidia")) return "NVENC";
  if (has("amd") || has("amf")) return "AMF";
  if (has("intel") || has("quick sync")) return "QSV";
  return "Software";
}

function Metric(props: {
  label: string;
  value: string;
  tone: "ok" | "info" | "warn";
  aux?: string;
}) {
  return (
    <div className={`settings-calc-metric tone-${props.tone}`}>
      <div className="settings-calc-label">{props.label}</div>
      <div className="settings-calc-value">{props.value}</div>
      {props.aux && <div className="settings-calc-aux">{props.aux}</div>}
    </div>
  );
}

// ---------- Games-tracked grouped list ----------

/** Returns true when `path` looks like a Steam install. Steam games live
 *  under `…\steamapps\common\<game>\…` regardless of which library drive. */
function isSteamPath(path: string): boolean {
  return /[\\/]steamapps[\\/]common[\\/]/i.test(path);
}

function baseExe(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

function GamesTrackedList(props: {
  games: string[];
  recentGames: string[];
  search: string;
  onRemove: (path: string) => void;
}) {
  const q = props.search.toLowerCase().trim();
  const matches = (p: string) => !q || p.toLowerCase().includes(q);

  // Recently added: render the recents in order (most-recent-first), but
  // filter by the live games list so a deleted entry doesn't dangle.
  const present = useMemo(() => new Set(props.games.map((g) => g.toLowerCase())), [props.games]);
  const recentVisible = props.recentGames.filter(
    (p) => present.has(p.toLowerCase()) && matches(p)
  );

  const steam: string[] = [];
  const manual: string[] = [];
  for (const g of props.games) {
    if (!matches(g)) continue;
    if (isSteamPath(g)) steam.push(g);
    else manual.push(g);
  }

  // Default-expand behavior: while searching, every non-empty group opens
  // so matches are visible. Outside search, groups stay collapsed until
  // user clicks.
  const isSearching = q.length > 0;
  const [steamOpen, setSteamOpen] = useState(false);
  const [manualOpen, setManualOpen] = useState(false);
  const steamEffectiveOpen = isSearching ? steam.length > 0 : steamOpen;
  const manualEffectiveOpen = isSearching ? manual.length > 0 : manualOpen;

  const totalShown = recentVisible.length + steam.length + manual.length;
  if (totalShown === 0) {
    return (
      <p className="settings-section-blurb">
        {props.games.length === 0
          ? "No games detected. Click Rescan or add one manually."
          : "No matches."}
      </p>
    );
  }

  return (
    <div className="settings-games-groups">
      {/* Recently added — always visible at the top, no collapsing. */}
      {recentVisible.length > 0 && (
        <div className="settings-games-group">
          <div className="settings-games-group-head settings-games-group-head-static">
            <span className="settings-games-group-label">Recently added</span>
            <span className="settings-audio-group-count">
              {recentVisible.length} {recentVisible.length === 1 ? "game" : "games"}
            </span>
          </div>
          <div className="settings-games-group-body">
            <ul className="settings-game-list">
              {recentVisible.map((path) => (
                <GameRow key={`recent-${path}`} path={path} onRemove={props.onRemove} />
              ))}
            </ul>
          </div>
        </div>
      )}

      {/* Steam */}
      {steam.length > 0 && (
        <div className={`settings-games-group${steamEffectiveOpen ? " is-open" : ""}`}>
          <button
            className="settings-games-group-head"
            onClick={() => setSteamOpen((v) => !v)}
            aria-expanded={steamEffectiveOpen}
            type="button"
          >
            <span className="settings-audio-group-arrow" aria-hidden>
              {steamEffectiveOpen ? "▾" : "▸"}
            </span>
            <span className="settings-games-group-label">Steam</span>
            <span className="settings-audio-group-count">{steam.length}</span>
          </button>
          {steamEffectiveOpen && (
            <div className="settings-games-group-body">
              <ul className="settings-game-list">
                {steam.map((path) => (
                  <GameRow key={path} path={path} onRemove={props.onRemove} />
                ))}
              </ul>
            </div>
          )}
        </div>
      )}

      {/* Manual */}
      {manual.length > 0 && (
        <div className={`settings-games-group${manualEffectiveOpen ? " is-open" : ""}`}>
          <button
            className="settings-games-group-head"
            onClick={() => setManualOpen((v) => !v)}
            aria-expanded={manualEffectiveOpen}
            type="button"
          >
            <span className="settings-audio-group-arrow" aria-hidden>
              {manualEffectiveOpen ? "▾" : "▸"}
            </span>
            <span className="settings-games-group-label">Manual</span>
            <span className="settings-audio-group-count">{manual.length}</span>
          </button>
          {manualEffectiveOpen && (
            <div className="settings-games-group-body">
              <ul className="settings-game-list">
                {manual.map((path) => (
                  <GameRow key={path} path={path} onRemove={props.onRemove} />
                ))}
              </ul>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function GameRow(props: { path: string; onRemove: (p: string) => void }) {
  return (
    <li className="settings-game-row" title={props.path}>
      <span className="settings-game-name mono">{baseExe(props.path)}</span>
      <span className="settings-game-path">{props.path}</span>
      <button
        className="settings-row-remove"
        onClick={() => props.onRemove(props.path)}
        title="Remove from allowlist"
        aria-label="Remove"
      >
        ×
      </button>
    </li>
  );
}

// ---------- Audio device grouping (Physical / Monitor / Virtual) ----------

type DeviceGroup = "physical" | "monitor" | "virtual";

/**
 * Classify a WASAPI render device by name. Heuristic — substring matching on
 * known patterns. Anything that doesn't look virtual or monitor falls into
 * "physical" so the user always sees their device somewhere.
 *
 * - Virtual: SteelSeries Sonar, Voicemeeter, VB-Audio, OBS Virtual, any
 *   "Virtual Audio Device" suffix Windows attaches to vDevs.
 * - Monitor: leading "N - " prefix Windows assigns to multi-sink GPU drivers,
 *   or explicit HDMI / DisplayPort audio labels.
 * - Physical: everything else (Speakers, Microphone, Realtek, Focusrite, …).
 */
function classifyDevice(name: string): DeviceGroup {
  const lower = name.toLowerCase();
  if (
    lower.includes("sonar") ||
    lower.includes("voicemeeter") ||
    lower.includes("vb-audio") ||
    lower.includes("vb cable") ||
    lower.includes("virtual audio") ||
    lower.includes("obs virtual")
  ) {
    return "virtual";
  }
  // "1 - ASUS VG32VQ1B" style — leading digit + dash is how Windows
  // distinguishes multi-monitor audio outputs from one GPU driver.
  if (/^\d+\s*-\s*/.test(name) || lower.includes("hdmi audio") || lower.includes("displayport")) {
    return "monitor";
  }
  return "physical";
}

const GROUP_META: Record<DeviceGroup, { label: string; hint: string }> = {
  physical: { label: "Physical devices", hint: "Speakers, headphones, microphones, USB audio interfaces." },
  monitor:  { label: "Monitor audio outputs", hint: "HDMI / DisplayPort audio attached to displays." },
  virtual:  { label: "Virtual devices", hint: "Sonar, Voicemeeter, OBS Virtual Audio, and similar routers." },
};

function AudioDeviceGroups(props: {
  devices: AudioDevice[];
  selectedIds: Set<string>;
  audioNames: Record<string, string>;
  isRunning: boolean;
  onToggle: (id: string) => void;
  onRename: (id: string, name: string) => void;
}) {
  // Bucket by classification, preserving the original device order within
  // each group (Windows surfaces them in default-first order).
  const grouped = useMemo(() => {
    const buckets: Record<DeviceGroup, AudioDevice[]> = {
      physical: [],
      monitor: [],
      virtual: [],
    };
    for (const d of props.devices) {
      buckets[classifyDevice(d.name)].push(d);
    }
    return buckets;
  }, [props.devices]);

  // Default-expand rule: any group containing a currently-selected device
  // opens; first-run (nothing selected) keeps all groups collapsed so the
  // user isn't faced with a wall of 12 devices.
  const initialOpen = useMemo(() => {
    const open: Record<DeviceGroup, boolean> = {
      physical: false,
      monitor: false,
      virtual: false,
    };
    for (const d of props.devices) {
      if (props.selectedIds.has(d.id)) {
        open[classifyDevice(d.name)] = true;
      }
    }
    return open;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []); // computed once on mount; user toggles take over after that
  const [openGroups, setOpenGroups] = useState(initialOpen);

  const groupOrder: DeviceGroup[] = ["physical", "monitor", "virtual"];

  return (
    <div className="settings-audio-groups">
      {groupOrder.map((key) => {
        const items = grouped[key];
        if (items.length === 0) return null;
        const selectedCount = items.filter((d) => props.selectedIds.has(d.id)).length;
        const isOpen = openGroups[key];
        return (
          <div key={key} className={`settings-audio-group${isOpen ? " is-open" : ""}`}>
            <button
              className="settings-audio-group-head"
              onClick={() => setOpenGroups((g) => ({ ...g, [key]: !g[key] }))}
              aria-expanded={isOpen}
              type="button"
            >
              <span className="settings-audio-group-arrow" aria-hidden>
                {isOpen ? "▾" : "▸"}
              </span>
              <span className="settings-audio-group-label">{GROUP_META[key].label}</span>
              <span className="settings-audio-group-meta">
                {selectedCount > 0 ? (
                  <span className="settings-audio-group-count is-selected">
                    {selectedCount} selected
                  </span>
                ) : (
                  <span className="settings-audio-group-count">
                    {items.length} {items.length === 1 ? "device" : "devices"}
                  </span>
                )}
              </span>
            </button>
            {isOpen && (
              <div className="settings-audio-group-body">
                <p className="settings-audio-group-hint">{GROUP_META[key].hint}</p>
                {items.map((d) => {
                  const checked = props.selectedIds.has(d.id);
                  return (
                    <div key={d.id} className="settings-audio-row">
                      <label className="settings-checkbox">
                        <input
                          type="checkbox"
                          checked={checked}
                          onChange={() => props.onToggle(d.id)}
                          disabled={props.isRunning}
                        />
                        <span className="settings-audio-system" title={d.name}>{d.name}</span>
                      </label>
                      {checked && (
                        <span className="settings-audio-rename">
                          <span className="settings-audio-rename-prefix">Called</span>
                          <input
                            type="text"
                            className="settings-audio-name"
                            placeholder="e.g. Game"
                            value={props.audioNames[d.id] ?? ""}
                            onChange={(e) => props.onRename(d.id, e.target.value)}
                            disabled={props.isRunning}
                            aria-label={`Track name for ${d.name}`}
                          />
                        </span>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

/** Lightweight controlled accordion for the settings sub-sections. */
function Accordion(props: {
  open: boolean;
  onToggle: () => void;
  title: string;
  summary: string;
  children: React.ReactNode;
}) {
  return (
    <div className={`settings-accordion${props.open ? " is-open" : ""}`}>
      <button
        className="settings-accordion-head"
        onClick={props.onToggle}
        aria-expanded={props.open}
      >
        <span className="settings-accordion-arrow" aria-hidden>
          {props.open ? "▾" : "▸"}
        </span>
        <span className="settings-accordion-title">{props.title}</span>
        <span className="settings-accordion-summary">{props.summary}</span>
      </button>
      {props.open && <div className="settings-accordion-body">{props.children}</div>}
    </div>
  );
}
