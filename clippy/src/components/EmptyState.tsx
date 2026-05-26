import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useReplayStatus, isReplayRunning } from "../useReplayState";

type SavedReplay = {
  path: string;
  name: string;
  size_bytes: number;
  modified_secs: number;
};

type Props = {
  /** Called when the user clicks "Open video…" / "or drag here". Wires to
   *  the same dialog flow as the topbar button. */
  onOpenDialog: () => void;
  /** Called when the user picks a recent clip. Passes the absolute path so
   *  the parent can reuse loadFile. */
  onLoadPath: (path: string) => void;
};

/**
 * Full-window empty state shown when no file is loaded. Picks one of two
 * variants based on whether the save folder has any clips:
 *
 *   - Hero    — first launch / empty folder. Centered logo + headline +
 *               Open CTA + drag hint.
 *   - Recent  — folder has saved replays. Two-column layout: open CTA +
 *               buffer status card on the left, recent clip list on the
 *               right. Clicking an item loads it into the editor.
 */
export function EmptyState(props: Props) {
  const [recents, setRecents] = useState<SavedReplay[] | null>(null);

  useEffect(() => {
    let alive = true;
    invoke<SavedReplay[]>("storage_list_replays", { limit: 8 })
      .then((list) => { if (alive) setRecents(list); })
      .catch(() => { if (alive) setRecents([]); });
    return () => { alive = false; };
  }, []);

  // Until the list lands, show Hero — it works for both cases and avoids a
  // flash of layout while the IPC resolves. ~50ms in practice.
  if (recents == null || recents.length === 0) {
    return <Hero onOpenDialog={props.onOpenDialog} />;
  }
  return <Recent recents={recents} onOpenDialog={props.onOpenDialog} onLoadPath={props.onLoadPath} />;
}

function Hero(props: { onOpenDialog: () => void }) {
  return (
    <div className="empty-hero">
      <div className="empty-hero-inner">
        <div className="empty-hero-logo" aria-hidden>
          <svg width="48" height="48" viewBox="0 0 32 32">
            <path d="M11 10.5v11l9-5.5z" fill="currentColor" />
          </svg>
        </div>
        <div className="empty-hero-title">
          Capture the play.<br />
          <span className="empty-hero-title-dim">Edit it in seconds.</span>
        </div>
        <p className="empty-hero-blurb">
          Clippy keeps the last minute of focused gameplay in memory.
          Press your save hotkey to flush, then trim and ship.
        </p>
        <div className="empty-hero-actions">
          <button className="btn primary empty-hero-cta" onClick={props.onOpenDialog} type="button">
            Open video…
          </button>
          <span className="empty-hero-hint">or drag a video here</span>
        </div>
      </div>
    </div>
  );
}

function Recent(props: {
  recents: SavedReplay[];
  onOpenDialog: () => void;
  onLoadPath: (path: string) => void;
}) {
  return (
    <div className="empty-recent">
      <div className="empty-recent-left">
        <div>
          <div className="empty-recent-h1">Open a clip</div>
          <p className="empty-recent-blurb">
            Pick a file, drop it on the window, or wait for your replay buffer to flush.
          </p>
        </div>
        <div className="empty-recent-actions">
          <button className="btn primary empty-hero-cta" onClick={props.onOpenDialog} type="button">
            Open video…
          </button>
        </div>
        <BufferStatusCard />
      </div>
      <div className="empty-recent-right">
        <div className="empty-recent-list-header">
          <span className="empty-recent-list-title">Recent</span>
          <span className="empty-recent-list-count">
            {props.recents.length} clip{props.recents.length === 1 ? "" : "s"}
          </span>
        </div>
        <ul className="empty-recent-list" role="list">
          {props.recents.map((r) => (
            <li key={r.path}>
              <button
                type="button"
                className="empty-recent-row"
                onClick={() => props.onLoadPath(r.path)}
                title={r.path}
              >
                <div className="empty-recent-row-thumb" aria-hidden>
                  {/* Real frame thumbnails would require ffmpeg invocations
                      at boot — skipped here for load-time budget. The
                      neutral square keeps the row anchored. */}
                  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                       strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
                    <rect x="3" y="5" width="18" height="14" rx="2" />
                    <path d="M10 9l5 3-5 3z" fill="currentColor" stroke="none" />
                  </svg>
                </div>
                <div className="empty-recent-row-text">
                  <div className="empty-recent-row-name">{r.name}</div>
                  <div className="empty-recent-row-meta mono">
                    <span>{relativeTime(r.modified_secs)}</span>
                    <span className="dim">·</span>
                    <span>{fmtBytes(r.size_bytes)}</span>
                  </div>
                </div>
                <svg className="empty-recent-row-chevron" width="14" height="14" viewBox="0 0 24 24"
                     fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round"
                     strokeLinejoin="round" aria-hidden>
                  <path d="M9 6l6 6-6 6" />
                </svg>
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

/** "4 min ago" / "Yesterday" / "2 days ago" / ISO-ish for old entries. */
function relativeTime(epochSecs: number): string {
  if (!epochSecs) return "—";
  const now = Math.floor(Date.now() / 1000);
  const delta = Math.max(0, now - epochSecs);
  if (delta < 60) return "Just now";
  if (delta < 3600) {
    const m = Math.floor(delta / 60);
    return `${m} min ago`;
  }
  if (delta < 86_400) {
    const h = Math.floor(delta / 3600);
    return `${h} hour${h === 1 ? "" : "s"} ago`;
  }
  if (delta < 86_400 * 2) return "Yesterday";
  if (delta < 86_400 * 7) {
    const d = Math.floor(delta / 86_400);
    return `${d} days ago`;
  }
  const date = new Date(epochSecs * 1000);
  return date.toLocaleDateString();
}

function fmtBytes(b: number): string {
  if (b >= 1_073_741_824) return `${(b / 1_073_741_824).toFixed(b >= 10 * 1_073_741_824 ? 1 : 2)} GB`;
  if (b >= 1_048_576) return `${(b / 1_048_576).toFixed(0)} MB`;
  if (b >= 1024) return `${(b / 1024).toFixed(0)} KB`;
  return `${b} B`;
}

/** "Replay buffer is running · Watching for games" status card. Polls the
 *  same `get_replay_status` IPC the topbar pill uses. Only renders content
 *  when the buffer is actually armed — empty placeholder keeps the column
 *  layout stable while Idle. */
function BufferStatusCard() {
  const status = useReplayStatus(1000);
  if (!isReplayRunning(status)) {
    return (
      <div className="empty-recent-buffer is-idle">
        <span className="empty-recent-buffer-dot" aria-hidden />
        <div className="empty-recent-buffer-text">
          <div className="empty-recent-buffer-title">Replay buffer is off</div>
          <div className="empty-recent-buffer-sub">Enable in Settings · Replay buffer</div>
        </div>
      </div>
    );
  }
  const title =
    status.state === "Saving"  ? "Saving last replay…" :
    status.state === "Active"  ? `Capturing · ${status.window_title || "focused game"}` :
                                 "Watching for games";
  const sub =
    status.state === "Active"
      ? `${formatBufferedSecs(status.buffered_secs)} buffered · ${(status.vram_mb).toFixed(0)} MB VRAM`
      : "Press Alt+F10 in-game to save the last minute";
  return (
    <div className={`empty-recent-buffer is-${status.state.toLowerCase()}`}>
      <span className="empty-recent-buffer-dot" aria-hidden />
      <div className="empty-recent-buffer-text">
        <div className="empty-recent-buffer-title">{title}</div>
        <div className="empty-recent-buffer-sub">{sub}</div>
      </div>
    </div>
  );
}

function formatBufferedSecs(s: number): string {
  if (!Number.isFinite(s) || s <= 0) return "0s";
  if (s < 60) return `${Math.round(s)}s`;
  const m = Math.floor(s / 60);
  const r = Math.round(s - m * 60);
  return `${m}m ${r}s`;
}

