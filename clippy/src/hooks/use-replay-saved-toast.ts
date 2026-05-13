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
    listen<string>("replay://saved", (event) => {
      const path = event.payload;
      if (!path || typeof path !== "string") return;
      if (getSaveBehavior() === "auto-open") {
        loadFile(path).catch((e) =>
          console.error("[clippy] replay auto-open failed:", e)
        );
      } else {
        setReplaySavedToast(path);
      }
    }).then((u) => (unlistenSaved = u));
    listen<string>("replay://save-error", (event) => {
      console.error("[clippy] replay save error:", event.payload);
    }).then((u) => (unlistenError = u));
    return () => {
      unlistenSaved?.();
      unlistenError?.();
    };
  }, [loadFile, setReplaySavedToast]);
}
