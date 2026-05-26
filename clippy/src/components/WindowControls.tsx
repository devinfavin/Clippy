import { useEffect, useState } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

/**
 * Min / Max-Restore / Close controls for the custom title bar.
 *
 * Tauri 2 needs `core:window:allow-minimize`, `allow-maximize`,
 * `allow-unmaximize`, `allow-close` in capabilities — `core:default` does
 * not grant these. The maximize toggle reads window state on mount and on
 * every Tauri resize event so the glyph stays in sync if the user
 * snap-resizes or restores via the OS shortcut.
 */
export function WindowControls() {
  const [isMaximized, setIsMaximized] = useState(false);

  useEffect(() => {
    const win = getCurrentWebviewWindow();
    let unlisten: (() => void) | undefined;
    void (async () => {
      try {
        setIsMaximized(await win.isMaximized());
        unlisten = await win.onResized(async () => {
          try { setIsMaximized(await win.isMaximized()); } catch {}
        });
      } catch {}
    })();
    return () => { try { unlisten?.(); } catch {} };
  }, []);

  const win = getCurrentWebviewWindow();
  return (
    <div className="window-controls" aria-label="Window controls">
      <button
        type="button"
        className="window-control"
        title="Minimize"
        aria-label="Minimize"
        onClick={() => { void win.minimize(); }}
      >
        <svg width="11" height="11" viewBox="0 0 12 12" aria-hidden>
          <line x1="2" y1="6" x2="10" y2="6" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round" />
        </svg>
      </button>
      <button
        type="button"
        className="window-control"
        title={isMaximized ? "Restore" : "Maximize"}
        aria-label={isMaximized ? "Restore" : "Maximize"}
        onClick={() => { void win.toggleMaximize(); }}
      >
        {isMaximized ? (
          <svg width="11" height="11" viewBox="0 0 12 12" aria-hidden>
            <rect x="2.5" y="3.5" width="6" height="6" fill="none" stroke="currentColor" strokeWidth="1" />
            <path d="M4.5 3.5 V2.5 H9.5 V7.5 H8.5" fill="none" stroke="currentColor" strokeWidth="1" />
          </svg>
        ) : (
          <svg width="11" height="11" viewBox="0 0 12 12" aria-hidden>
            <rect x="2.5" y="2.5" width="7" height="7" fill="none" stroke="currentColor" strokeWidth="1" />
          </svg>
        )}
      </button>
      <button
        type="button"
        className="window-control window-control-close"
        title="Close"
        aria-label="Close"
        onClick={() => { void win.close(); }}
      >
        <svg width="11" height="11" viewBox="0 0 12 12" aria-hidden>
          <line x1="3" y1="3" x2="9" y2="9" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round" />
          <line x1="9" y1="3" x2="3" y2="9" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round" />
        </svg>
      </button>
    </div>
  );
}
