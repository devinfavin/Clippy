import { useEffect } from "react";
import { captureKeybind, type ActionId, type Keybinds } from "../keybinds";

/// Capture next keypress when listening to bind a new shortcut. Esc cancels
/// without writing; any other capturable keystroke updates the bound action.
export function useKeybindCapture(args: {
  listeningAction: ActionId | null;
  setListeningAction: React.Dispatch<React.SetStateAction<ActionId | null>>;
  setKeybinds: React.Dispatch<React.SetStateAction<Keybinds>>;
}): void {
  const { listeningAction, setListeningAction, setKeybinds } = args;
  useEffect(() => {
    if (!listeningAction) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape" && !e.ctrlKey && !e.shiftKey && !e.altKey) {
        setListeningAction(null);
        return;
      }
      const captured = captureKeybind(e);
      if (!captured) return;
      setKeybinds((prev) => ({ ...prev, [listeningAction]: captured }));
      setListeningAction(null);
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true } as AddEventListenerOptions);
  }, [listeningAction, setListeningAction, setKeybinds]);
}
