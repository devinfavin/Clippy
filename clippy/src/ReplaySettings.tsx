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
import {
  SelectField,
  SettingsGroup,
  SettingsLabel,
  SettingsRow,
  StatusCard,
  Stepper,
  Toggle,
} from "./settings/primitives";
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

  // ----- accordion state — only Audio sources + Games tracked keep their
  // accordion treatment after the Phase-7 retrofit. Quality and Overlay
  // sections moved to flat SettingsGroups.
  const [audioOpen, setAudioOpen] = useState<boolean>(() => lsBool(LS.audioOpen, false));
  const [gamesOpen, setGamesOpen] = useState<boolean>(() => lsBool(LS.gamesOpen, false));

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
    <section className="settings-tab-pane">
      <header>
        <h3 className="settings-tab-pane-title">Replay buffer</h3>
        <p className="settings-tab-pane-blurb">
          Continuously captures the last few minutes of focused gameplay so you can
          save a clip after the fact. Press your save hotkey to flush the buffer
          to an MP4.
        </p>
      </header>

      {/* Big status / start-stop card at the top — answers "is it running?"
          at a glance and gives the primary action. */}
      <StatusCard tone={isRunning ? "good" : "info"}>
        <span
          className="replay-buffer-status-dot"
          data-running={isRunning ? "1" : "0"}
          aria-hidden
        />
        <div className="replay-buffer-status-text">
          <div className="replay-buffer-status-title">
            {!isRunning
              ? "Replay buffer is off"
              : status.state === "Saving"
                ? "Saving last replay…"
                : status.state === "Active"
                  ? `Capturing · ${status.window_title || "focused game"}`
                  : captureMode === "monitor"
                    ? "Watching the selected display"
                    : "Watching for games"}
          </div>
          <div className="replay-buffer-status-sub mono">
            {isRunning
              ? `${minutes} min buffer · h264/aac · ${captureMode === "monitor" ? "monitor" : "per-window"}`
              : "Press Start to arm the buffer."}
          </div>
        </div>
        <button
          className={`btn primary${busy ? " is-busy" : ""}`}
          onClick={isRunning ? stop : start}
          disabled={busy}
        >
          {busy ? "…" : isRunning ? "Stop" : "Start"}
        </button>
      </StatusCard>

      {error && <div className="settings-error">{error}</div>}

      <div>
        <SettingsLabel>App behavior</SettingsLabel>
        <SettingsGroup>
          <SettingsRow
            title="Start Clippy when Windows starts"
            subtitle="Auto-launch on login so the buffer is armed before your games are."
          >
            <Toggle value={autostartWithOs} onChange={toggleAutostartWithOs} />
          </SettingsRow>
          <SettingsRow
            title="Start the replay buffer when Clippy launches"
            subtitle="Skip the manual Start click each session."
          >
            <Toggle value={autoStart} onChange={setAutoStart} />
          </SettingsRow>
          <SettingsRow
            title="Close to system tray"
            subtitle="Keep Clippy running in the background when you hit ×. Right-click the tray icon to fully quit."
          >
            <Toggle value={hideOnClose} onChange={setHideOnCloseState} />
          </SettingsRow>
          <SettingsRow
            title="Verbose diagnostics"
            subtitle={
              <>Log non-game window titles. Off by default for privacy — browser tabs and document names appear as <span className="mono">&lt;non-game window&gt;</span> in the log unless this is on.</>
            }
          >
            <Toggle value={diagVerbose} onChange={setDiagVerbose} />
          </SettingsRow>
        </SettingsGroup>
      </div>

      <div>
        <SettingsLabel>Buffer</SettingsLabel>
        <SettingsGroup>
          <SettingsRow
            title="Buffer duration"
            subtitle="How many seconds Clippy keeps in memory before the save hotkey trims a clip."
          >
            <Stepper
              value={durationSecs}
              onChange={setDurationSecs}
              min={60}
              max={600}
              step={30}
              unit="s"
            />
          </SettingsRow>
          <SettingsRow
            title="Capture mode"
            subtitle="Per-window keeps Clippy invisible until a tracked game has focus."
          >
            <SelectField
              value={captureMode}
              onChange={(v) => setCaptureMode(v as CaptureMode)}
              options={[
                { value: "perWindow", label: "Per-window (allowlist)" },
                { value: "monitor", label: "Full-screen monitor" },
              ]}
              width={200}
            />
          </SettingsRow>
          {captureMode === "monitor" && (
            <SettingsRow title="Display">
              <SelectField
                value={monitorHwnd}
                onChange={(v) => setMonitorHwnd(String(v))}
                options={
                  monitors.length === 0
                    ? [{ value: "", label: "(no displays detected)" }]
                    : monitors.map((m) => ({
                        value: m.hmonitor,
                        label: `${m.label} — ${m.width}×${m.height}`,
                      }))
                }
                width={240}
              />
            </SettingsRow>
          )}
          <SettingsRow
            title="When a replay is saved"
            subtitle="Auto-open lands you in the editor; Notify keeps the game in focus."
          >
            <SelectField
              value={saveBehavior}
              onChange={(v) => setSaveBehavior(v as SaveBehavior)}
              options={[
                { value: "auto-open", label: "Open in editor" },
                { value: "notify",    label: "Notification only" },
              ]}
              width={200}
            />
          </SettingsRow>
          <SettingsRow
            title="Save folder"
            subtitle={<span className="mono">{saveDir || "(loading…)"}</span>}
          >
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
              Change…
            </button>
            <button
              className="settings-secondary-btn"
              onClick={resetSaveDir}
              title="Restore the default (Videos\Clippy Replays)"
            >
              Reset
            </button>
          </SettingsRow>
        </SettingsGroup>
      </div>

      <div>
        <SettingsLabel>In-game save overlay</SettingsLabel>
        <SettingsGroup>
          <SettingsRow
            title="Show overlay on save"
            subtitle="Bottom-corner toast over the game while the save flushes."
          >
            <Toggle value={overlayEnabled} onChange={setOverlayEnabled} />
          </SettingsRow>
          <SettingsRow title="Position">
            <SelectField
              value={overlayPosition}
              onChange={(v) => setOverlayPosition(v as OverlayPosition)}
              options={[
                { value: "topLeft",     label: "Top-left" },
                { value: "topRight",    label: "Top-right" },
                { value: "bottomLeft",  label: "Bottom-left" },
                { value: "bottomRight", label: "Bottom-right" },
              ]}
              width={150}
            />
          </SettingsRow>
          <SettingsRow
            title="Auto-hide after"
            subtitle="Hovering the overlay pauses the timer; moving away restarts a fresh countdown."
          >
            <SelectField
              value={overlayHideMs}
              onChange={(v) => setOverlayHideMs(Number(v))}
              options={[
                { value: 3000,  label: "3 seconds" },
                { value: 5000,  label: "5 seconds" },
                { value: 8000,  label: "8 seconds" },
                { value: 15000, label: "15 seconds" },
                { value: 30000, label: "30 seconds" },
              ]}
              width={130}
            />
          </SettingsRow>
          <SettingsRow
            title="Chime on save"
            subtitle="Soft sound when a replay finishes saving."
          >
            <Toggle value={overlaySuccessSound} onChange={setOverlaySuccessSound} />
          </SettingsRow>
          <SettingsRow
            title="Tone on save failure"
            subtitle="Different sound when the save fails — so you notice while in-game."
          >
            <Toggle value={overlayFailureSound} onChange={setOverlayFailureSound} />
          </SettingsRow>
          <SettingsRow title="Overlay sound volume">
            <input
              type="range"
              min={0}
              max={100}
              step={5}
              value={overlayVolume}
              onChange={(e) => setOverlayVolume(parseInt(e.target.value, 10))}
              disabled={!overlayEnabled || (!overlaySuccessSound && !overlayFailureSound)}
              className="replay-volume-slider"
            />
            <span className="mono s-row-stat">{overlayVolume}%</span>
          </SettingsRow>
        </SettingsGroup>
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

      <div>
        <SettingsLabel>Encoding</SettingsLabel>
        <SettingsGroup>
          <SettingsRow
            title="Resolution"
            subtitle="Source matches the captured surface; Half saves disk space."
          >
            <SelectField
              value={resKind}
              onChange={(v) => setResKind(v as ResolutionKind)}
              options={[
                { value: "source", label: "Source" },
                { value: "half",   label: "Half (¼ pixels)" },
                { value: "custom", label: "Custom…" },
              ]}
              width={150}
            />
          </SettingsRow>
          {resKind === "custom" && (
            <SettingsRow title="Custom dimensions">
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
              <span className="s-row-stat">×</span>
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
            </SettingsRow>
          )}
          <SettingsRow title="Bitrate">
            <SelectField
              value={BITRATE_PRESETS.find((p) => p.kbps === bitrateKbps)?.kbps ?? -1}
              onChange={(v) => {
                const n = Number(v);
                if (n > 0) setBitrateKbps(n);
              }}
              options={[
                ...BITRATE_PRESETS.map((p) => ({
                  value: p.kbps,
                  label: `${p.label} — ${p.kbps / 1000} Mbps`,
                })),
                { value: -1, label: "Custom…" },
              ]}
              width={220}
            />
          </SettingsRow>
          {!BITRATE_PRESETS.some((p) => p.kbps === bitrateKbps) && (
            <SettingsRow title="Custom bitrate">
              <input
                type="number"
                className="settings-text settings-num"
                min={1000}
                max={200000}
                step={1000}
                value={bitrateKbps}
                onChange={(e) => setBitrateKbps(parseInt(e.target.value, 10) || 0)}
                disabled={isRunning}
              />
              <span className="s-row-stat">kbps</span>
            </SettingsRow>
          )}
          <SettingsRow
            title="Encoder"
            subtitle={sysInfo ? `Auto resolves to ${resolvedAutoEncoder(sysInfo)} on this system.` : "Auto picks the best hardware encoder available."}
          >
            <SelectField
              value={encoderPref}
              onChange={(v) => setEncoderPref(v as EncoderPref)}
              options={[
                { value: "auto",     label: sysInfo ? `Auto · ${resolvedAutoEncoder(sysInfo)}` : "Auto" },
                { value: "nvenc",    label: "NVIDIA NVENC" },
                { value: "amf",      label: "AMD AMF" },
                { value: "qsv",      label: "Intel Quick Sync" },
                { value: "software", label: "Software (CPU)" },
              ]}
              width={220}
            />
          </SettingsRow>
          <SettingsRow
            title="Keyframe interval"
            subtitle="Tighter intervals = tighter trims at save time."
          >
            <SelectField
              value={keyframeSecs}
              onChange={(v) => setKeyframeSecs(Number(v))}
              options={[
                { value: 0, label: "Auto" },
                { value: 1, label: "1 second" },
                { value: 2, label: "2 seconds" },
                { value: 4, label: "4 seconds" },
              ]}
              width={140}
            />
          </SettingsRow>
          <SettingsRow
            title="Max games captured"
            subtitle="When you focus a new game past the cap, the oldest worker is evicted."
          >
            <Stepper
              value={maxWorkers}
              onChange={setMaxWorkers}
              min={1}
              max={10}
              step={1}
            />
          </SettingsRow>
        </SettingsGroup>
      </div>

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
