import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { RailTabs, type RailTabOption } from "./RailTabs";

export type RailTab = "audio" | "regions" | "crop";

type Props = {
  /** Externally controllable active tab so other surfaces (timeline-click,
   *  region-list-click) can switch the rail focus. Falls back to `regions`. */
  tab: RailTab;
  onTabChange: (t: RailTab) => void;
  /** Count of regions — surfaces as a badge on the Regions tab. */
  regionCount: number;
  /** Whether the source has any audio tracks. Hides Audio tab when zero. */
  hasAudio: boolean;
  /** Active region context shown in the rail header (color dot + name + #).
   *  Null when no region is active — the header collapses to just the
   *  collapse-chevron row. */
  activeRegion?: {
    name: string;
    color: string;
    /** 1-based index used for the `#N` mono badge. */
    number: number;
  } | null;
  /** Panels — exactly one rendered at a time, matched to `tab`. */
  audio: ReactNode;
  regions: ReactNode;
  crop: ReactNode;
  /** Persist rail collapsed-state across sessions. Sentinel key keeps the
   *  rail off the localStorage hot path used by replay-ls. */
  storageKey?: string;
};

const COLLAPSED_KEY_DEFAULT = "clippy.rail.collapsed";

/**
 * Right-side rail next to the video preview. Hosts the segmented tab
 * control, the active panel, and the collapse chevron. Collapsing the rail
 * frees horizontal pixels for the preview — handy on smaller laptops or
 * when the rail content isn't relevant to the current task.
 */
export function EditorRail(props: Props) {
  const { tab, onTabChange, regionCount, hasAudio, audio, regions, crop, activeRegion } = props;
  const storageKey = props.storageKey ?? COLLAPSED_KEY_DEFAULT;

  const [collapsed, setCollapsed] = useState<boolean>(() => {
    try { return localStorage.getItem(storageKey) === "1"; } catch { return false; }
  });

  useEffect(() => {
    try { localStorage.setItem(storageKey, collapsed ? "1" : "0"); } catch {}
  }, [collapsed, storageKey]);

  // If audio tracks vanish (source change), drop the audio tab.
  useEffect(() => {
    if (!hasAudio && tab === "audio") onTabChange("regions");
  }, [hasAudio, tab, onTabChange]);

  const options: RailTabOption<RailTab>[] = [];
  if (hasAudio) {
    options.push({
      value: "audio",
      label: "Audio",
      icon: (
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor"
             strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
          <path d="M11 5L6 9H3v6h3l5 4V5z" fill="currentColor" stroke="none" />
          <path d="M16 9a4 4 0 010 6M19 6a8 8 0 010 12" />
        </svg>
      ),
    });
  }
  options.push({
    value: "regions",
    label: "Regions",
    badge: regionCount,
    icon: (
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden>
        <rect x="3" y="9" width="4" height="6" rx="1" fill="currentColor" />
        <rect x="10" y="5" width="4" height="14" rx="1" fill="currentColor" />
        <rect x="17" y="10" width="4" height="4" rx="1" fill="currentColor" />
      </svg>
    ),
  });
  options.push({
    value: "crop",
    label: "Crop",
    icon: (
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor"
           strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
        <path d="M6 2v16h16M2 6h16v16" />
      </svg>
    ),
  });

  if (collapsed) {
    return (
      <aside className="editor-rail collapsed" aria-label="Editor tools (collapsed)">
        <button
          type="button"
          className="rail-collapse-btn"
          title="Expand panel"
          aria-label="Expand panel"
          onClick={() => setCollapsed(false)}
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
               strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
            <path d="M15 6l-6 6 6 6" />
          </svg>
        </button>
        <span className="rail-divider" aria-hidden />
        {options.map((o) => (
          <button
            key={o.value}
            type="button"
            className={`rail-icon-btn${o.value === tab ? " active" : ""}`}
            title={o.label}
            aria-label={o.label}
            onClick={() => { onTabChange(o.value); setCollapsed(false); }}
          >
            {o.icon}
            {o.badge != null && o.badge > 0 && (
              <span className="rail-icon-badge">{o.badge}</span>
            )}
          </button>
        ))}
      </aside>
    );
  }

  return (
    <aside className="editor-rail" aria-label="Editor tools">
      <div className="editor-rail-card">
        {/* Active-region header — colored dot + name + #N + collapse chevron.
            Always rendered so the collapse button stays in a predictable
            spot; falls back to a quieter row when no region is active yet. */}
        <div className={`rail-active-header${activeRegion ? "" : " is-empty"}`}>
          {activeRegion ? (
            <>
              <span
                className="rail-active-dot"
                style={{
                  background: activeRegion.color,
                  boxShadow: `0 0 0 3px ${activeRegion.color}22`,
                }}
                aria-hidden
              />
              <span className="rail-active-name">{activeRegion.name}</span>
              <span className="rail-active-id mono">#{activeRegion.number}</span>
            </>
          ) : (
            <span className="rail-active-empty">Editor tools</span>
          )}
          <span className="rail-active-spacer" />
          <button
            type="button"
            className="rail-collapse-btn"
            title="Collapse panel"
            aria-label="Collapse panel"
            onClick={() => setCollapsed(true)}
          >
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                 strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="M9 6l6 6-6 6" />
            </svg>
          </button>
        </div>
        <div className="rail-tabs-row">
          <RailTabs value={tab} options={options} onChange={onTabChange} />
        </div>
        <div className="rail-panel">
          {tab === "audio" && audio}
          {tab === "regions" && regions}
          {tab === "crop" && crop}
        </div>
      </div>
    </aside>
  );
}
