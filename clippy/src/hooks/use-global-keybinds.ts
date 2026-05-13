import { useEffect } from "react";
import {
  GLOBAL_ACTIONS,
  matchesBinding,
  type ActionId,
  type Keybinds,
} from "../keybinds";
import type { Phase } from "../types";

/// Window-level keydown handler that dispatches in-app actions when the
/// user presses a bound shortcut. Suppressed while the keybind editor is
/// capturing, while typing into INPUT/TEXTAREA, and (for non-openFile
/// actions) when the editor isn't in a ready/exporting phase.
export function useGlobalKeybinds(args: {
  keybinds: Keybinds;
  listeningAction: ActionId | null;
  phase: Phase;
  dispatch: (action: ActionId) => void;
}): void {
  const { keybinds, listeningAction, phase, dispatch } = args;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Don't fire app shortcuts while the keybind editor is capturing.
      if (listeningAction != null) return;
      // openFile is allowed even before a video is loaded; everything else needs a ready/exporting phase.
      const tag = (e.target as HTMLElement | null)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;

      for (const action of Object.keys(keybinds) as ActionId[]) {
        // Global actions are routed by the OS hotkey, not the in-app dispatch.
        if (GLOBAL_ACTIONS.has(action)) continue;
        if (matchesBinding(e, keybinds[action])) {
          if (action !== "openFile" && phase.kind !== "ready" && phase.kind !== "exporting") return;
          e.preventDefault();
          dispatch(action);
          return;
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [keybinds, listeningAction, phase, dispatch]);
}
