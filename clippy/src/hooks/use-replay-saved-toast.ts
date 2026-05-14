import { useEffect } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getSaveBehavior } from "../settings/replay-ls";

/// Replay-buffer saves: backend writes an MP4 and emits its path as a
/// string payload. The user's chosen save behavior (settings → Replay
/// buffer) decides whether to open it immediately or just toast a
/// "click to open" notice.
export function useReplaySavedToast(
  loadFile: (path: string) => Promise<void>,
  setReplaySavedToast: React.Dispatch<React.SetStateAction<string | null>>,
): void {
  useEffect(() => {
    let unlistenSaved: UnlistenFn | null = null;
    let unlistenError: UnlistenFn | null = null;
    let unlistenOverlayOpen: UnlistenFn | null = null;
    // The backend payload became structured in v0.3.5 (`{ id, path, sizeMb,
    // windowTitle }`). Accept both shapes so the hook keeps working if the
    // payload ever reverts to a bare path string.
    listen<{ path: string } | string>("replay://saved", (event) => {
      const payload = event.payload as { path?: string } | string;
      const path = typeof payload === "string" ? payload : payload?.path;
      if (!path || typeof path !== "string") return;
      if (getSaveBehavior() === "auto-open") {
        loadFile(path).catch((e) =>
          console.error("[clippy] replay auto-open failed:", e)
        );
      } else {
        setReplaySavedToast(path);
      }
    }).then((u) => (unlistenSaved = u));
    listen<{ msg?: string } | string>("replay://save-error", (event) => {
      console.error("[clippy] replay save error:", event.payload);
    }).then((u) => (unlistenError = u));
    // Triggered by the in-game overlay's "Open" button. Always loads the
    // clip, independent of the saveBehavior setting, because the user has
    // explicitly asked to open it. The overlay also brings the main window
    // forward before emitting, so loadFile runs in a visible window.
    listen<string>("clippy://open-saved", (event) => {
      const path = event.payload;
      if (!path || typeof path !== "string") return;
      loadFile(path).catch((e) =>
        console.error("[clippy] overlay open-in-editor failed:", e)
      );
    }).then((u) => (unlistenOverlayOpen = u));
    return () => {
      unlistenSaved?.();
      unlistenError?.();
      unlistenOverlayOpen?.();
    };
  }, [loadFile, setReplaySavedToast]);
}
