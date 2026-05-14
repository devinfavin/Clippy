import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getAllWebviewWindows, getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { primaryMonitor, availableMonitors, cursorPosition, type Monitor } from "@tauri-apps/api/window";
import { PhysicalPosition } from "@tauri-apps/api/dpi";
import { readOverlaySettings, type OverlayPosition } from "./settings/replay-ls";
import blipUrl from "./assets/sounds/blip.mp3";
import badBlipUrl from "./assets/sounds/badBlip.mp3";
import "./OverlayApp.css";

// ----- shared state shape, mirrors Rust payloads (camelCase via serde) -----

type SaveStarted = { id: number; windowTitle: string; durationSecs: number };
type SaveProgress = { id: number; stage: "writing" | "bsf" | "muxing" | "finalizing" | string };
type Saved = {
  id: number;
  path: string;
  sizeMb: number;
  windowTitle: string;
  tracksSaved: number;
  tracksTotal: number;
};
type SaveError = { id: number; kind: string; msg: string };
type SpawnFailed = { id: number; windowTitle: string; kind: string; msg: string };

type Phase =
  | { kind: "idle" }
  | { kind: "started"; id: number; title: string; duration: number }
  | { kind: "progress"; id: number; title: string; duration: number; stage: string }
  | { kind: "done"; id: number; title: string; path: string; sizeMb: number; tracksSaved: number; tracksTotal: number }
  | { kind: "error"; id: number; kind_: string; msg: string }
  | { kind: "spawn-error"; id: number; title: string; kind_: string; msg: string };

const STAGE_LABEL: Record<string, string> = {
  writing: "Writing to disk…",
  bsf: "Fixing stream timing…",
  muxing: "Muxing MP4…",
  finalizing: "Finalizing…",
};

const ERROR_FRIENDLY: Record<string, string> = {
  buffer_empty: "Replay buffer is empty — give it a few seconds of capture first.",
  not_running: "Replay buffer isn't running for the focused window.",
  io: "Couldn't write the save file (disk full, permission, or path missing).",
  ffmpeg: "FFmpeg couldn't mux the output. See diagnostics for details.",
  generic: "",
};

const SPAWN_FRIENDLY: Record<string, string> = {
  nvenc_ceiling:
    "Your GPU is at its concurrent encoder limit — close another captured game (or restart the buffer) to free a slot.",
  encoder_init:
    "The hardware encoder didn't initialise. The game won't be captured this session.",
  generic: "",
};

// ----- sound (bundled MP3 cues) -----
//
// Two short clips ship with the renderer bundle: `blip.mp3` on success,
// `badBlip.mp3` on failure. Vite handles asset URL rewriting at build time
// so these imports give us hashed asset paths that work in dev and prod.
// HTMLAudioElement is created fresh per play so overlapping saves (rare —
// hotkey mash before the prior toast hides) don't truncate each other.

function playSound(url: string, volume01: number) {
  try {
    const a = new Audio(url);
    a.volume = Math.min(1, Math.max(0, volume01));
    // Promise rejects on autoplay-blocked, which can happen if the renderer
    // has never received a user interaction. Swallow — the visual overlay
    // still appears, and the user can interact with the overlay to re-enable.
    a.play().catch(() => {});
  } catch {}
}

function playSuccessChime(volume01: number) {
  playSound(blipUrl, volume01);
}

function playFailureBuzz(volume01: number) {
  playSound(badBlipUrl, volume01);
}

// ----- window positioning -----

const MARGIN_LOGICAL = 24;

async function resolvePrimary(): Promise<Monitor | null> {
  // Tauri 2's primaryMonitor() can return null in some setups (RDP, headless
  // adapters, certain multi-monitor configs). Fall back to availableMonitors()
  // and pick the monitor at origin (Windows' primary convention).
  try {
    const p = await primaryMonitor();
    if (p) return p;
  } catch {}
  try {
    const all = await availableMonitors();
    if (all.length === 0) return null;
    const atOrigin = all.find((m) => m.position.x === 0 && m.position.y === 0);
    return atOrigin ?? all[0];
  } catch {
    return null;
  }
}

// Configured logical size of the overlay window — must match what
// WebviewWindowBuilder::inner_size sets in lib.rs. Used as the dimensions
// for corner-positioning math so we don't depend on `outerSize()`, which
// can hang on a freshly-shown borderless+transparent window in Tauri 2.
const OVERLAY_LOGICAL_W = 400;
const OVERLAY_LOGICAL_H = 150;

async function positionOverlay(position: OverlayPosition) {
  const win = getCurrentWebviewWindow();
  const mon = await resolvePrimary();
  if (!mon) return;

  // Stay in PHYSICAL pixels throughout. Monitor.size + Monitor.position from
  // Tauri 2 are physical-pixel screen-space, and PhysicalPosition for
  // setPosition is interpreted in that same coordinate space. Skipping
  // outerSize() — it can hang on a freshly-shown borderless+transparent
  // window in Tauri 2; the configured logical inner_size × scaleFactor gives
  // an accurate-enough outer-edge estimate for corner placement.
  const sf = mon.scaleFactor ?? 1;
  const marginPx = Math.round(MARGIN_LOGICAL * sf);
  const winW = Math.round(OVERLAY_LOGICAL_W * sf);
  const winH = Math.round(OVERLAY_LOGICAL_H * sf);
  const monLeft = mon.position.x;
  const monTop = mon.position.y;
  const monRight = monLeft + mon.size.width;
  const monBottom = monTop + mon.size.height;
  let x: number;
  let y: number;
  switch (position) {
    case "topLeft":
      x = monLeft + marginPx;
      y = monTop + marginPx;
      break;
    case "topRight":
      x = monRight - winW - marginPx;
      y = monTop + marginPx;
      break;
    case "bottomLeft":
      x = monLeft + marginPx;
      y = monBottom - winH - marginPx;
      break;
    case "bottomRight":
    default:
      x = monRight - winW - marginPx;
      y = monBottom - winH - marginPx;
      break;
  }
  await win.setPosition(new PhysicalPosition(Math.round(x), Math.round(y)));
}

async function showOverlay(position: OverlayPosition) {
  const win = getCurrentWebviewWindow();
  // Position FIRST (while still hidden) so the window appears in the right
  // corner immediately on show — no flash at the cascade-default position.
  // The required permissions (core:window:allow-show / allow-set-position /
  // allow-set-always-on-top) are declared in capabilities/default.json; if
  // they're missing, all three of these throw silently and the window stays
  // at whatever cascade position Windows picked. That was the original
  // v0.3.5 position bug.
  try {
    await positionOverlay(position);
  } catch (e) {
    console.warn("[overlay] positionOverlay failed:", e);
  }
  try {
    await win.show();
    await win.setAlwaysOnTop(true);
  } catch (e) {
    console.warn("[overlay] show/alwaysOnTop failed:", e);
  }
}

async function hideOverlay() {
  await getCurrentWebviewWindow().hide();
}

/// Bring the main editor window forward and load the saved clip. Used by the
/// success overlay's "Open" action so a user clicking it from in-game lands
/// directly in the editor with the clip ready, rather than just opening the
/// containing folder.
async function openInEditor(path: string) {
  // Surface the saved clip to the main window via the same event the auto-
  // open path uses. The main window's existing handler picks it up and runs
  // loadFile() in its own context.
  const all = await getAllWebviewWindows();
  const main = all.find((w) => w.label === "main");
  if (main) {
    try {
      await main.show();
      await main.unminimize();
      await main.setFocus();
      // Reuse the in-app saved-toast pathway: re-emit `replay://saved` here
      // is wrong (would re-trigger our own overlay). Instead use a dedicated
      // "load this file" event the main window listens for.
      await main.emit("clippy://open-saved", path);
    } catch (e) {
      console.warn("[overlay] openInEditor: main window action failed:", e);
    }
  }
}

// ----- component -----

export function OverlayApp() {
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  const hideTimerRef = useRef<number | null>(null);

  // Cancel any pending auto-hide whenever the phase changes (e.g. a second
  // save lands while the prior "done" toast is still fading).
  const clearTimer = () => {
    if (hideTimerRef.current != null) {
      clearTimeout(hideTimerRef.current);
      hideTimerRef.current = null;
    }
  };

  useEffect(() => {
    let unlisteners: UnlistenFn[] = [];
    let cancelled = false;

    (async () => {
      const settings = readOverlaySettings();
      if (!settings.enabled) return; // overlay disabled; never show.

      const onStarted = await listen<SaveStarted>("replay://save-started", (e) => {
        if (cancelled) return;
        clearTimer();
        setPhase({
          kind: "started",
          id: e.payload.id,
          title: e.payload.windowTitle,
          duration: e.payload.durationSecs,
        });
        // Re-read settings on every show so changes from the settings UI
        // apply on the next save without restarting the app.
        showOverlay(readOverlaySettings().position).catch(() => {});
      });
      unlisteners.push(onStarted);

      const onProgress = await listen<SaveProgress>("replay://save-progress", (e) => {
        if (cancelled) return;
        setPhase((prev) => {
          // Ignore progress for a save we didn't see start (window opened
          // mid-save — shouldn't normally happen but be defensive).
          if (
            prev.kind === "idle" ||
            prev.kind === "done" ||
            prev.kind === "error" ||
            prev.kind === "spawn-error"
          ) {
            return prev;
          }
          if (prev.id !== e.payload.id) return prev;
          return {
            kind: "progress",
            id: prev.id,
            title: prev.title,
            duration: prev.duration,
            stage: e.payload.stage,
          };
        });
      });
      unlisteners.push(onProgress);

      const onSaved = await listen<Saved>("replay://saved", (e) => {
        if (cancelled) return;
        setPhase({
          kind: "done",
          id: e.payload.id,
          title: e.payload.windowTitle,
          path: e.payload.path,
          sizeMb: e.payload.sizeMb,
          tracksSaved: e.payload.tracksSaved ?? 0,
          tracksTotal: e.payload.tracksTotal ?? 0,
        });
        clearTimer();
        const s = readOverlaySettings();
        if (s.successSound) playSuccessChime(s.volume / 100);
        // Always start a hide timer — hover detection in the click-through
        // effect cancels it if the cursor is over the overlay. We never let
        // the toast persist forever; the cursor-poll loop that backs
        // click-through would otherwise run indefinitely.
        hideTimerRef.current = window.setTimeout(() => {
          hideOverlay().catch(() => {});
          setPhase({ kind: "idle" });
        }, s.hideMs);
      });
      unlisteners.push(onSaved);

      const onError = await listen<SaveError>("replay://save-error", (e) => {
        if (cancelled) return;
        setPhase({
          kind: "error",
          id: e.payload.id,
          kind_: e.payload.kind,
          msg: e.payload.msg,
        });
        clearTimer();
        const s = readOverlaySettings();
        if (s.failureSound) playFailureBuzz(s.volume / 100);
        hideTimerRef.current = window.setTimeout(() => {
          hideOverlay().catch(() => {});
          setPhase({ kind: "idle" });
        }, s.hideMs);
      });
      unlisteners.push(onError);

      // Worker spawn failures (NVENC concurrent-encoder cap, MFT init refusal,
      // etc.) — surfaced via the same overlay UX so the user sees in-game
      // that the buffer didn't start, instead of finding out later when their
      // save hotkey does nothing.
      const onSpawnFailed = await listen<SpawnFailed>(
        "replay://spawn-failed",
        (e) => {
          if (cancelled) return;
          setPhase({
            kind: "spawn-error",
            id: e.payload.id,
            title: e.payload.windowTitle,
            kind_: e.payload.kind,
            msg: e.payload.msg,
          });
          clearTimer();
          const s = readOverlaySettings();
          if (s.failureSound) playFailureBuzz(s.volume / 100);
          hideTimerRef.current = window.setTimeout(() => {
            hideOverlay().catch(() => {});
            setPhase({ kind: "idle" });
          }, s.hideMs);
          showOverlay(s.position).catch(() => {});
        },
      );
      unlisteners.push(onSpawnFailed);
    })();

    return () => {
      cancelled = true;
      clearTimer();
      unlisteners.forEach((u) => u());
    };
  }, []);

  // Click-through + hover-pause: the overlay should pass clicks through to the
  // game/desktop EXCEPT when the cursor is over an action button. While the
  // cursor is inside the overlay rect at all, the auto-hide timer pauses so
  // the user can read the message; on un-hover, a fresh timer starts. The
  // toast NEVER persists indefinitely — that would leave the cursor-poll
  // loop running forever.
  const actionsRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const win = getCurrentWebviewWindow();
    let cancelled = false;
    let timerId: number | null = null;
    let monitorSf = 1;
    primaryMonitor().then((m) => { if (m) monitorSf = m.scaleFactor ?? 1; }).catch(() => {});

    // No-button states (started / progress) — always click-through, no
    // hover-pause needed (these phases don't have a hide timer yet).
    if (phase.kind !== "done" && phase.kind !== "error" && phase.kind !== "spawn-error") {
      win.setIgnoreCursorEvents(true).catch(() => {});
      return;
    }

    let currentlyIgnoring = true;
    let wasInsideWindow = false;
    win.setIgnoreCursorEvents(true).catch(() => {});

    const tick = async () => {
      if (cancelled) return;
      try {
        const cur = await cursorPosition();
        const winPos = await win.outerPosition();
        // Convert cursor screen-space physical px → window-local CSS px.
        const localX = (cur.x - winPos.x) / monitorSf;
        const localY = (cur.y - winPos.y) / monitorSf;

        // Inside the overlay's bounding rect? Use the configured logical
        // inner-size; the rect is always at (0,0)..(W,H) in window-local.
        const insideWindow =
          localX >= 0 && localX <= OVERLAY_LOGICAL_W &&
          localY >= 0 && localY <= OVERLAY_LOGICAL_H;

        // Hit-test buttons inside the actions row. Only relevant when the
        // cursor is also inside the window — saves a querySelectorAll on
        // every tick when the cursor is elsewhere.
        let overButton = false;
        if (insideWindow) {
          const buttons = actionsRef.current?.querySelectorAll("button");
          if (buttons) {
            for (const btn of Array.from(buttons)) {
              const r = (btn as HTMLElement).getBoundingClientRect();
              if (localX >= r.left && localX <= r.right &&
                  localY >= r.top && localY <= r.bottom) {
                overButton = true;
                break;
              }
            }
          }
        }

        // Toggle OS-level click-through.
        const shouldIgnore = !overButton;
        if (shouldIgnore !== currentlyIgnoring) {
          currentlyIgnoring = shouldIgnore;
          await win.setIgnoreCursorEvents(shouldIgnore);
        }

        // Hover-enter / hover-exit transitions drive the hide-timer state:
        // enter → cancel the running timer so the toast doesn't disappear
        // out from under a user who's actively reading it. Exit → start a
        // fresh full-duration timer (rather than resuming a paused one;
        // simpler and gives a "you have N seconds to act after looking
        // away" UX that matches OS toast notifications).
        if (insideWindow !== wasInsideWindow) {
          wasInsideWindow = insideWindow;
          if (insideWindow) {
            if (hideTimerRef.current != null) {
              clearTimeout(hideTimerRef.current);
              hideTimerRef.current = null;
            }
          } else {
            // Restart with a fresh full-duration timer. Read settings each
            // time so a slider change applies on the next un-hover without
            // needing another save.
            const ms = readOverlaySettings().hideMs;
            hideTimerRef.current = window.setTimeout(() => {
              hideOverlay().catch(() => {});
              setPhase({ kind: "idle" });
            }, ms);
          }
        }
      } catch {
        // Cursor / position API can transiently fail (window mid-move,
        // monitor reconfig); skip this tick and try again.
      }
      if (!cancelled) timerId = window.setTimeout(tick, 50);
    };
    tick();

    return () => {
      cancelled = true;
      if (timerId != null) clearTimeout(timerId);
      win.setIgnoreCursorEvents(true).catch(() => {});
    };
  }, [phase.kind]);

  const dismiss = () => {
    clearTimer();
    hideOverlay().catch(() => {});
    setPhase({ kind: "idle" });
  };

  const reveal = async (path: string) => {
    try {
      await invoke("reveal_in_folder", { path });
    } catch (e) {
      console.warn("[overlay] reveal failed:", e);
    }
  };

  // ----- render per phase -----

  if (phase.kind === "idle") {
    // Window is hidden in this state. Render nothing to avoid a flash if
    // setPhase('idle') beats the .hide() call.
    return null;
  }

  return (
    <div className={`overlay-card overlay-${phase.kind}`}>
      <div className="overlay-row">
        {(phase.kind === "started" || phase.kind === "progress") && (
          <div className="overlay-spinner" />
        )}
        {phase.kind === "done" && <div className="overlay-icon overlay-icon-good">✓</div>}
        {phase.kind === "error" && <div className="overlay-icon overlay-icon-bad">!</div>}
        {phase.kind === "spawn-error" && <div className="overlay-icon overlay-icon-bad">!</div>}

        <div className="overlay-text">
          {phase.kind === "started" && (
            <>
              <div className="overlay-title">
                Saving{phase.duration > 0 ? ` ${phase.duration}s` : ""}
                {phase.title ? ` of ${phase.title}` : "…"}
              </div>
              <div className="overlay-sub">Snapshotting buffer…</div>
            </>
          )}
          {phase.kind === "progress" && (
            <>
              <div className="overlay-title">
                Saving{phase.duration > 0 ? ` ${phase.duration}s` : ""}
                {phase.title ? ` of ${phase.title}` : "…"}
              </div>
              <div className="overlay-sub">{STAGE_LABEL[phase.stage] ?? phase.stage}</div>
            </>
          )}
          {phase.kind === "done" && (
            <>
              <div className="overlay-title">Saved{phase.title ? ` ${phase.title}` : ""}</div>
              <div className="overlay-sub">
                <span className="overlay-mono">{basename(phase.path)}</span>
                {phase.sizeMb > 0 && (
                  <span className="overlay-dim"> · {phase.sizeMb.toFixed(1)} MB</span>
                )}
                {phase.tracksTotal > 0 && phase.tracksSaved < phase.tracksTotal && (
                  <span className="overlay-warn">
                    {" "}· {phase.tracksSaved} of {phase.tracksTotal} audio tracks
                  </span>
                )}
              </div>
            </>
          )}
          {phase.kind === "error" && (
            <>
              <div className="overlay-title">Save failed</div>
              <div className="overlay-sub">{ERROR_FRIENDLY[phase.kind_] || phase.msg}</div>
            </>
          )}
          {phase.kind === "spawn-error" && (
            <>
              <div className="overlay-title">
                Buffer didn't start{phase.title ? ` for ${phase.title}` : ""}
              </div>
              <div className="overlay-sub">{SPAWN_FRIENDLY[phase.kind_] || phase.msg}</div>
            </>
          )}
        </div>
      </div>

      {phase.kind === "done" && (
        <div className="overlay-actions" ref={actionsRef}>
          <button
            className="overlay-btn-primary"
            onClick={() => {
              openInEditor(phase.path).catch(() => {});
              dismiss();
            }}
          >
            Open
          </button>
          <button className="overlay-btn" onClick={() => reveal(phase.path)}>
            Reveal
          </button>
          <button className="overlay-btn" onClick={dismiss}>
            Dismiss
          </button>
        </div>
      )}
      {phase.kind === "error" && (
        <div className="overlay-actions" ref={actionsRef}>
          <button className="overlay-btn" onClick={dismiss}>
            Dismiss
          </button>
        </div>
      )}
      {phase.kind === "spawn-error" && (
        <div className="overlay-actions" ref={actionsRef}>
          <button className="overlay-btn" onClick={dismiss}>
            Dismiss
          </button>
        </div>
      )}
    </div>
  );
}

function basename(p: string): string {
  // Cross-platform basename. Windows uses backslashes; Tauri returns native.
  const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
  return i >= 0 ? p.slice(i + 1) : p;
}
