import { useEffect } from "react";

/// Close a modal when the user presses Esc. `enabled` lets callers gate
/// this on "modal is open AND not in a sub-mode that should swallow Esc".
export function useModalEscClose(
  enabled: boolean,
  onClose: () => void,
): void {
  useEffect(() => {
    if (!enabled) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [enabled, onClose]);
}
