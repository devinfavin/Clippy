import { memo, useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { enable as autostartEnable, disable as autostartDisable, isEnabled as autostartIsEnabled } from "@tauri-apps/plugin-autostart";
import { useReplayStatus, isReplayRunning } from "./useReplayState";
import { Accordion } from "./settings/accordion";
import { AudioDeviceGroups } from "./settings/audio-device-groups";
import { GamesTrackedList } from "./settings/games-tracked-list";
import { ResourceImpact } from "./settings/resource-impact";
import { resolvedAutoEncoder } from "./settings/encoder-name";
import {
  BITRATE_PRESETS,
  LS,
  OVERLAY_DEFAULTS,
  getAutoStart,
  getReplayStartArgs,
  getSaveBehavior,
  lsArr,
  lsBool,
  lsMap,
  lsNum,
  type OverlayPosition,
} from "./settings/replay-ls";
import type {
  AudioDevice,
  CaptureMode,
  EncoderPref,
  MonitorInfo,
  ResolutionKind,
  SaveBehavior,
  SystemInfo,
} from "./settings/replay-types";

// Re-export the public API so callers keep importing from "./ReplaySettings".
export { getAutoStart, getReplayStartArgs, getSaveBehavior } from "./settings/replay-ls";
export { capabilityHint } from "./settings/resource-impact";
export type { EncoderPref, SaveBehavior, SystemInfo } from "./settings/replay-types";

export const ReplaySettings = memo(ReplaySettingsImpl);

const POSITION_LABELS: Record<OverlayPosition, string> = {
  topLeft: "top-left",
  topRight: "top-right",
  bottomLeft: "bottom-left",
  bottomRight: "bottom-right",
};

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
  const [notifyOpen, setNotifyOpen] = useState<boolean>(() => lsBool(LS.notifyOpen, false));

  // ----- in-game save-progress overlay settings -----
  const [overlayEnabled, setOverlayEnabled] = useState<boolean>(
    () => lsBool(LS.overlayEnabled, OVERLAY_DEFAULTS.enabled),
  );
  const [overlayPosition, setOverlayPosition] = useState<OverlayPosition>(
    () => (localStorage.getItem(LS.overlayPosition) as OverlayPosition | null) ?? OVERLAY_DEFAULTS.position,
  );
  const [overlayHideMs, setOverlayHideMs] = useState<number>(
    () => lsNum(LS.overlayHideMs, OVERLAY_DEFAULTS.hideMs),
  );
  const [overlaySuccessSound, setOverlaySuccessSound] = useState<boolean>(
    () => lsBool(LS.overlaySuccessSound, OVERLAY_DEFAULTS.successSound),
  );
  const [overlayFailureSound, setOverlayFailureSound] = useState<boolean>(
    () => lsBool(LS.overlayFailureSound, OVERLAY_DEFAULTS.failureSound),
  );
  const [overlayVolume, setOverlayVolume] = useState<number>(
    () => lsNum(LS.overlayVolume, OVERLAY_DEFAULTS.volume),
  );

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
  useEffect(() => { localStorage.setItem(LS.notifyOpen, String(notifyOpen)); }, [notifyOpen]);
  useEffect(() => { localStorage.setItem(LS.overlayEnabled, String(overlayEnabled)); }, [overlayEnabled]);
  useEffect(() => { localStorage.setItem(LS.overlayPosition, overlayPosition); }, [overlayPosition]);
  useEffect(() => { localStorage.setItem(LS.overlayHideMs, String(overlayHideMs)); }, [overlayHideMs]);
  useEffect(() => { localStorage.setItem(LS.overlaySuccessSound, String(overlaySuccessSound)); }, [overlaySuccessSound]);
  useEffect(() => { localStorage.setItem(LS.overlayFailureSound, String(overlayFailureSound)); }, [overlayFailureSound]);
  useEffect(() => { localStorage.setItem(LS.overlayVolume, String(overlayVolume)); }, [overlayVolume]);
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

      {/* ---------------- In-game save overlay ---------------- */}
      <Accordion
        open={notifyOpen}
        onToggle={() => setNotifyOpen((v) => !v)}
        title="In-game save overlay"
        summary={
          overlayEnabled
            ? `Shown at ${POSITION_LABELS[overlayPosition]}`
            : "Off — saves happen silently"
        }
      >
        <p className="settings-section-blurb">
          When you press the save hotkey during a game, a small overlay surfaces
          in the corner of your screen so you can see the save's progress without
          alt-tabbing. Errors persist by default so a failed save can't be
          missed.
        </p>

        <label className="settings-checkbox">
          <input
            type="checkbox"
            checked={overlayEnabled}
            onChange={(e) => setOverlayEnabled(e.target.checked)}
          />
          <span>Show overlay on save</span>
        </label>

        <div
          className="settings-row"
          style={{ opacity: overlayEnabled ? 1 : 0.5 }}
        >
          <label className="settings-label">Position</label>
          <select
            value={overlayPosition}
            onChange={(e) => setOverlayPosition(e.target.value as OverlayPosition)}
            disabled={!overlayEnabled}
          >
            <option value="topLeft">Top-left</option>
            <option value="topRight">Top-right</option>
            <option value="bottomLeft">Bottom-left</option>
            <option value="bottomRight">Bottom-right</option>
          </select>
        </div>

        <p className="settings-section-blurb settings-help">
          Hovering the overlay pauses its auto-hide timer; moving the cursor
          away restarts a fresh countdown.
        </p>

        <div
          className="settings-row"
          style={{ opacity: overlayEnabled ? 1 : 0.5 }}
        >
          <label className="settings-label">Auto-hide after</label>
          <select
            value={String(overlayHideMs)}
            onChange={(e) => setOverlayHideMs(parseInt(e.target.value, 10))}
            disabled={!overlayEnabled}
          >
            <option value="3000">3 seconds</option>
            <option value="5000">5 seconds</option>
            <option value="8000">8 seconds</option>
            <option value="15000">15 seconds</option>
            <option value="30000">30 seconds</option>
          </select>
        </div>

        <label className="settings-checkbox" style={{ opacity: overlayEnabled ? 1 : 0.5 }}>
          <input
            type="checkbox"
            checked={overlaySuccessSound}
            onChange={(e) => setOverlaySuccessSound(e.target.checked)}
            disabled={!overlayEnabled}
          />
          <span>Play a chime when a save completes</span>
        </label>

        <label className="settings-checkbox" style={{ opacity: overlayEnabled ? 1 : 0.5 }}>
          <input
            type="checkbox"
            checked={overlayFailureSound}
            onChange={(e) => setOverlayFailureSound(e.target.checked)}
            disabled={!overlayEnabled}
          />
          <span>Play a tone when a save fails</span>
        </label>

        <div
          className="settings-row"
          style={{ opacity: overlayEnabled && (overlaySuccessSound || overlayFailureSound) ? 1 : 0.5 }}
        >
          <label className="settings-label">Overlay sound volume</label>
          <input
            type="range"
            min={0}
            max={100}
            step={5}
            value={overlayVolume}
            onChange={(e) => setOverlayVolume(parseInt(e.target.value, 10))}
            disabled={!overlayEnabled || (!overlaySuccessSound && !overlayFailureSound)}
          />
          <span className="settings-value mono">{overlayVolume}%</span>
        </div>
      </Accordion>

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
