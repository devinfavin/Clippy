import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { keybindToShortcutString, type Keybind } from "../keybinds";

/// Push the save-replay binding to the OS whenever it changes. Backend
/// re-registers the global shortcut so it works while a game is focused.
export function useReplayHotkeyPush(saveReplayBinding: Keybind): void {
  useEffect(() => {
    const s = keybindToShortcutString(saveReplayBinding);
    if (!s) return;
    invoke("replay_set_save_hotkey", { shortcut: s }).catch((e) =>
      console.error("[clippy] replay_set_save_hotkey failed:", e)
    );
  }, [saveReplayBinding]);
}
