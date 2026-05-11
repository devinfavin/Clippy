import { memo, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { fmtTime, fmtMb, fmtEta } from "./formatters";

/**
 * Discriminated union of every status the strip can surface. The component is
 * passed exactly one of these (or null = strip hidden). Priority is decided
 * by the caller — App.tsx picks the highest-importance state.
 */
export type StatusContent =
  | { kind: "error"; message: string; onDismiss?: () => void }
  | { kind: "phase"; label: string; progress?: number; etaSecs?: number | null }
  | { kind: "export-done"; paths: string[]; onDismiss: () => void }
  | { kind: "replay-saved"; path: string; onOpen: () => void; onDismiss: () => void }
  | { kind: "frame-copied"; onDismiss: () => void }
  | { kind: "looping"; regionIndex: number; regionColor: string; onStop: () => void }
  | { kind: "scrubbing"; time: number };

type Tone = "info" | "accent" | "muted" | "success" | "error";

const SVG = {
  // Lucide-style 16x16 icons, currentColor for tinting.
  spinner: (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
         strokeWidth="2.5" strokeLinecap="round" className="status-icon-spin">
      <path d="M21 12a9 9 0 1 1-6.219-8.56" />
    </svg>
  ),
  upload: (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
         strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
      <polyline points="17 8 12 3 7 8" />
      <line x1="12" y1="3" x2="12" y2="15" />
    </svg>
  ),
  loop: (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
         strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <polyline points="17 1 21 5 17 9" />
      <path d="M3 11V9a4 4 0 0 1 4-4h14" />
      <polyline points="7 23 3 19 7 15" />
      <path d="M21 13v2a4 4 0 0 1-4 4H3" />
    </svg>
  ),
  scrub: (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
         strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <line x1="5" y1="12" x2="19" y2="12" />
      <polyline points="12 5 5 12 12 19" />
      <polyline points="12 5 19 12 12 19" />
    </svg>
  ),
  check: (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
         strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
      <polyline points="20 6 9 17 4 12" />
    </svg>
  ),
  error: (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
         strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="12" cy="12" r="10" />
      <line x1="12" y1="8" x2="12" y2="12" />
      <line x1="12" y1="16" x2="12.01" y2="16" />
    </svg>
  ),
  close: (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor"
         strokeWidth="2" strokeLinecap="round">
      <line x1="18" y1="6" x2="6" y2="18" />
      <line x1="6" y1="6" x2="18" y2="18" />
    </svg>
  ),
};

// Memoized — App.tsx wraps `content` in useMemo so this skips re-renders on
// every timeupdate during normal playback when nothing in the strip changed.
export const StatusStrip = memo(StatusStripImpl);

function StatusStripImpl(props: { content: StatusContent | null }) {
  // Render an empty strip (collapsed) when there's no content. The CSS
  // animates max-height + opacity so showing/hiding feels intentional.
  const visible = props.content != null;
  return (
    <div className={`status-strip${visible ? " visible" : ""}`} aria-live="polite">
      <div className="status-strip-inner">
        {props.content && <StatusContentView content={props.content} />}
      </div>
    </div>
  );
}

function StatusContentView({ content }: { content: StatusContent }) {
  switch (content.kind) {
    case "error":
      return (
        <Row tone="error" icon={SVG.error} text={content.message}
             actions={content.onDismiss && [{ label: SVG.close, onClick: content.onDismiss, title: "Dismiss" }]} />
      );
    case "phase":
      return (
        <Row tone="info" icon={SVG.spinner} text={content.label}
             progress={content.progress}
             aux={content.etaSecs != null ? `ETA ${fmtEta(content.etaSecs)}` : undefined} />
      );
    case "looping":
      return (
        <Row tone="accent" icon={SVG.loop}
             text={
               <>
                 Looping{" "}
                 <span className="status-region-pill"
                       style={{ ["--region-color" as never]: content.regionColor } as React.CSSProperties}>
                   <span className="status-region-dot" /> Region {content.regionIndex + 1}
                 </span>
               </>
             }
             actions={[{ label: "Stop", onClick: content.onStop, title: "Stop looping" }]} />
      );
    case "scrubbing":
      return <Row tone="muted" icon={SVG.scrub} text={`Scrubbing · ${fmtTime(content.time)}`} />;
    case "frame-copied":
      return (
        <Row tone="success" icon={SVG.check}
             text="Frame copied to clipboard — click Save Frame again to write a PNG"
             actions={[{ label: SVG.close, onClick: content.onDismiss, title: "Dismiss" }]} />
      );
    case "export-done":
      return <ExportDoneRow paths={content.paths} onDismiss={content.onDismiss} />;
    case "replay-saved": {
      const fname = content.path.split(/[\\/]/).pop() ?? "replay";
      return (
        <Row
          tone="success"
          icon={SVG.upload}
          text={`Replay saved · ${fname}`}
          actions={[
            { label: "Open in editor", onClick: content.onOpen, title: "Load this clip into the editor" },
            { label: SVG.close, onClick: content.onDismiss, title: "Dismiss" },
          ]}
        />
      );
    }
  }
}

function ExportDoneRow(props: { paths: string[]; onDismiss: () => void }) {
  // File sizes are loaded async; show "…" until they arrive.
  const [sizes, setSizes] = useState<Record<string, number>>({});
  useEffect(() => {
    let alive = true;
    Promise.all(
      props.paths.map(async (p) => {
        try {
          const sz = await invoke<number>("file_size", { path: p });
          return [p, sz] as const;
        } catch {
          return [p, 0] as const;
        }
      })
    ).then((entries) => alive && setSizes(Object.fromEntries(entries)));
    return () => { alive = false; };
  }, [props.paths]);

  const total = Object.values(sizes).reduce((s, n) => s + n, 0);
  const allLoaded = Object.keys(sizes).length === props.paths.length;
  const firstName = props.paths[0]?.split(/[\\/]/).pop() ?? "clip";
  const text =
    props.paths.length === 1
      ? `Exported ${firstName}${allLoaded ? ` · ${fmtMb(total)}` : ""}`
      : `Exported ${props.paths.length} clips${allLoaded ? ` · ${fmtMb(total)} total` : ""}`;

  const reveal = () => invoke("reveal_in_folder", { path: props.paths[0] }).catch(() => {});
  const copyPaths = () => {
    navigator.clipboard.writeText(props.paths.join("\n")).catch(() => {});
  };

  return (
    <Row
      tone="success"
      icon={SVG.upload}
      text={text}
      actions={[
        { label: "Open folder", onClick: reveal, title: "Reveal first file in Explorer" },
        { label: props.paths.length > 1 ? "Copy paths" : "Copy path", onClick: copyPaths, title: "Copy file path(s) to clipboard" },
        { label: SVG.close, onClick: props.onDismiss, title: "Dismiss" },
      ]}
    />
  );
}

/**
 * Single visual language for every state: tint stripe + icon + text + optional
 * progress bar + optional inline action buttons. Tone drives the left-edge
 * stripe color (info/accent/muted/success/error) so glance-recognition is
 * consistent across kinds.
 */
function Row(props: {
  tone: Tone;
  icon: React.ReactNode;
  text: React.ReactNode;
  progress?: number;
  aux?: React.ReactNode;
  actions?: Array<{ label: React.ReactNode; onClick: () => void; title?: string }>;
}) {
  return (
    <div className={`status-row status-tone-${props.tone}`}>
      <span className="status-icon">{props.icon}</span>
      <span className="status-text">{props.text}</span>
      {props.aux && <span className="status-aux">{props.aux}</span>}
      {typeof props.progress === "number" && (
        <span className="status-progress">
          <span style={{ width: `${(props.progress * 100).toFixed(1)}%` }} />
        </span>
      )}
      {props.actions && props.actions.length > 0 && (
        <span className="status-actions">
          {props.actions.map((a, i) => (
            <button
              key={i}
              className="status-action"
              onClick={a.onClick}
              title={a.title}
              aria-label={a.title}
            >
              {a.label}
            </button>
          ))}
        </span>
      )}
    </div>
  );
}
