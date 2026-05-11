// Design Pass 2 — Showcase. Visit `?showcase=1` (dev builds only) to see
// every token, button, input, chip, status-strip variant in isolation.
// Tokens are scoped to `.showcase-page` in Showcase.css so the rest of
// the app's current token system stays intact until the mechanical-apply
// step lands.

import { useState } from "react";
import "./Showcase.css";

export function Showcase() {
  return (
    <div className="showcase-page">
      <Header />
      <ColorsSection />
      <TypographySection />
      <RadiiSection />
      <ButtonsSection />
      <InputsSection />
      <StatusStripSection />
      <LoadingPatternsSection />
      <ChipsSection />
      <AccordionCardSection />
      <TopbarPreview />
      <EmptyEditorPreview />
      <TabbedModalPreview />
    </div>
  );
}

// ---------------------------------------------------------- //

function Header() {
  return (
    <header className="showcase-header">
      <h1 className="showcase-title">Clippy — Design Pass 2 showcase</h1>
      <p className="showcase-blurb">
        Token + primitive validation surface. Every component variant in every
        state lives here so we can pin the design system before mechanically
        applying it across the editor / settings / modals. Dev-only — gated
        behind <code>?showcase=1</code> and tree-shaken from release builds.
      </p>
    </header>
  );
}

// ---------- Colors ---------- //

const COLORS = {
  background: [
    { name: "--bg-base", value: "#1a1a1a" },
    { name: "--bg-surface", value: "#232323" },
    { name: "--bg-elevated", value: "#2b2b2b" },
    { name: "--bg-input", value: "#1e1e1e" },
  ],
  border: [
    { name: "--border-subtle", value: "#2e2e2e" },
    { name: "--border-strong", value: "#3a3a3a" },
  ],
  accent: [
    { name: "--accent", value: "#c8b6ff" },
    { name: "--accent-hover", value: "#d4c5ff" },
    { name: "--accent-active", value: "#b8a3ff" },
    { name: "--accent-muted", value: "#8a7ab8" },
  ],
  text: [
    { name: "--text-primary",   value: "rgba(255,255,255,0.92)" },
    { name: "--text-secondary", value: "rgba(255,255,255,0.65)" },
    { name: "--text-tertiary",  value: "rgba(255,255,255,0.42)" },
  ],
  status: [
    { name: "--status-good", value: "#4ade80" },
    { name: "--status-warn", value: "#fbbf24" },
    { name: "--status-bad",  value: "#f87171" },
  ],
  tracks: [
    { name: "--track-1", value: "#f5c799" }, // warm peach
    { name: "--track-2", value: "#f3b5d0" }, // soft rose
    { name: "--track-3", value: "#f0d68c" }, // muted gold
    { name: "--track-4", value: "#f5b3a3" }, // dusty coral
  ],
  regions: [
    { name: "--region-1", value: "#a8c8f0" }, // soft sky
    { name: "--region-2", value: "#a8e8d4" }, // misty mint
    { name: "--region-3", value: "#c0c5d0" }, // slate gray
    { name: "--region-4", value: "#99d6db" }, // pale teal
    { name: "--region-5", value: "#d8b8d0" }, // dusty plum
  ],
} as const;

function SwatchGrid(props: { items: readonly { name: string; value: string }[] }) {
  return (
    <div className="swatch-grid">
      {props.items.map((c) => (
        <div className="swatch" key={c.name}>
          <span className="swatch-chip" style={{ background: c.value }} />
          <div className="swatch-body">
            <div className="swatch-name">{c.name}</div>
            <div className="swatch-value">{c.value}</div>
          </div>
        </div>
      ))}
    </div>
  );
}

function ColorsSection() {
  return (
    <section className="showcase-section">
      <h2 className="showcase-section-title">Colors</h2>
      <p className="showcase-section-note">
        Validate side-by-side: any region or track color reading as "almost
        the accent" should be swapped. Status colors must never appear in the
        track or region grids.
      </p>

      <div className="showcase-subsection">
        <h3 className="showcase-subsection-title">Backgrounds</h3>
        <SwatchGrid items={COLORS.background} />
      </div>
      <div className="showcase-subsection">
        <h3 className="showcase-subsection-title">Borders</h3>
        <SwatchGrid items={COLORS.border} />
      </div>
      <div className="showcase-subsection">
        <h3 className="showcase-subsection-title">Accent (lilac — primary actions, focus, brand)</h3>
        <SwatchGrid items={COLORS.accent} />
      </div>
      <div className="showcase-subsection">
        <h3 className="showcase-subsection-title">Text (opacity-on-white, adapts to surface)</h3>
        <SwatchGrid items={COLORS.text} />
      </div>
      <div className="showcase-subsection">
        <h3 className="showcase-subsection-title">Status (semantic only)</h3>
        <SwatchGrid items={COLORS.status} />
      </div>
      <div className="showcase-subsection">
        <h3 className="showcase-subsection-title">Track palette (audio rows only)</h3>
        <SwatchGrid items={COLORS.tracks} />
      </div>
      <div className="showcase-subsection">
        <h3 className="showcase-subsection-title">Region palette (timeline regions only)</h3>
        <SwatchGrid items={COLORS.regions} />
      </div>
    </section>
  );
}

// ---------- Typography ---------- //

function TypographySection() {
  return (
    <section className="showcase-section">
      <h2 className="showcase-section-title">Typography</h2>
      <p className="showcase-section-note">
        Sample text rendered at each token's size and weight. UI scale setting
        deferred to Pass 3.
      </p>
      <div>
        <TypeRow token="--fs-display" meta="22 / 600">
          <span className="type-sample-display">Settings — Replay buffer</span>
        </TypeRow>
        <TypeRow token="--fs-heading" meta="17 / 600">
          <span className="type-sample-heading">Escape-from-Tarkov_2026-05-10.mp4</span>
        </TypeRow>
        <TypeRow token="--fs-body" meta="14 / 400">
          <span className="type-sample-body">
            Continuously captures the last few minutes of focused gameplay so
            you can save a clip after the fact.
          </span>
        </TypeRow>
        <TypeRow token="--fs-small" meta="13 / 400">
          <span className="type-sample-small">
            Files untouched for 30 days are auto-pruned.
          </span>
        </TypeRow>
        <TypeRow token="--fs-micro" meta="12 / 500">
          <span className="type-sample-micro">CALLED</span>
        </TypeRow>
        <TypeRow token="--fs-mono" meta="13 / 400 mono">
          <span className="type-sample-mono">C:\Users\favin\Videos\Clippy Replays\</span>
        </TypeRow>
      </div>
    </section>
  );
}

function TypeRow(props: { token: string; meta: string; children: React.ReactNode }) {
  return (
    <div className="type-row">
      <span className="type-token">{props.token}</span>
      <span className="type-meta">{props.meta}</span>
      <span>{props.children}</span>
    </div>
  );
}

// ---------- Radii ---------- //

function RadiiSection() {
  return (
    <section className="showcase-section">
      <h2 className="showcase-section-title">Radii</h2>
      <p className="showcase-section-note">
        Three sizes only — each applied to the surface it's intended for so
        the difference reads at the scale it'll actually be used.
      </p>
      <div className="radii-grid">
        <div className="radii-cell">
          <div className="radii-cell-meta">
            <span className="radii-cell-name">radius-sm</span>
            <span className="radii-cell-value">4px · buttons, inputs, chips</span>
          </div>
          <div className="radii-cell-demo">
            <button className="radii-demo-button">Save replay</button>
          </div>
        </div>
        <div className="radii-cell">
          <div className="radii-cell-meta">
            <span className="radii-cell-name">radius-md</span>
            <span className="radii-cell-value">6px · cards, panels, accordions</span>
          </div>
          <div className="radii-cell-demo">
            <div className="radii-demo-card">
              Card surface inside a dialog body.
            </div>
          </div>
        </div>
        <div className="radii-cell">
          <div className="radii-cell-meta">
            <span className="radii-cell-name">radius-lg</span>
            <span className="radii-cell-value">8px · top-level dialog frames</span>
          </div>
          <div className="radii-cell-demo">
            <div className="radii-demo-dialog">
              <div className="radii-demo-dialog-header">Settings</div>
              <div className="radii-demo-dialog-body">
                Dialog body content.
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}

// ---------- Buttons ---------- //

const BUTTON_VARIANTS: Array<{ id: string; label: string }> = [
  { id: "primary",     label: "primary" },
  { id: "secondary",   label: "secondary" },
  { id: "ghost",       label: "ghost" },
  { id: "destructive", label: "destructive" },
];

const BUTTON_STATES = [
  { id: "default",  label: "default" },
  { id: "hover",    label: "hover" },
  { id: "active",   label: "active" },
  { id: "focus",    label: "focus" },
  { id: "disabled", label: "disabled" },
  { id: "loading",  label: "loading" },
] as const;

function ButtonsSection() {
  return (
    <section className="showcase-section">
      <h2 className="showcase-section-title">Buttons</h2>
      <p className="showcase-section-note">
        Same dimensions across every variant (32px height, 12px h-padding, 4px
        radius). One primary per surface, max. Disabled buttons drop to 40%
        opacity and suppress hover; loading replaces the label with a spinner
        while preserving the button's width.
      </p>
      <div className="btn-matrix">
        <div className="btn-matrix-header label-col">variant</div>
        {BUTTON_STATES.map((s) => (
          <div key={s.id} className="btn-matrix-header">{s.label}</div>
        ))}
        {BUTTON_VARIANTS.map((v) => (
          <ButtonRow key={v.id} variant={v.id} label={v.label} />
        ))}
      </div>
    </section>
  );
}

function ButtonRow(props: { variant: string; label: string }) {
  const cls = `sc-btn sc-btn-${props.variant}`;
  return (
    <>
      <div className="btn-matrix-label">{props.label}</div>
      {BUTTON_STATES.map((s) => {
        if (s.id === "loading") {
          return (
            <span key={s.id}>
              <button className={cls} disabled aria-busy>
                <span className="sc-btn-spinner" aria-hidden />
              </button>
            </span>
          );
        }
        if (s.id === "disabled") {
          return (
            <span key={s.id}>
              <button className={`${cls} is-disabled`} disabled>
                {labelFor(props.variant)}
              </button>
            </span>
          );
        }
        return (
          <span key={s.id}>
            <button className={`${cls} is-${s.id}`}>
              {labelFor(props.variant)}
            </button>
          </span>
        );
      })}
    </>
  );
}
function labelFor(variant: string): string {
  switch (variant) {
    case "primary":     return "Save replay";
    case "secondary":   return "Browse…";
    case "ghost":       return "Cancel";
    case "destructive": return "Clear cache";
    default: return "Action";
  }
}

// ---------- Inputs ---------- //

function InputsSection() {
  return (
    <section className="showcase-section">
      <h2 className="showcase-section-title">Inputs</h2>
      <p className="showcase-section-note">
        Text fields, dropdowns, number inputs all share the same surface.
        Error state colors the border and surfaces a message below at the
        --fs-small / --status-bad tier.
      </p>
      <div>
        <InputRow label="default">
          <input className="sc-input" defaultValue="O:\Clips" />
        </InputRow>
        <InputRow label="hover">
          <input className="sc-input is-hover" defaultValue="O:\Clips" />
        </InputRow>
        <InputRow label="focused">
          <input className="sc-input is-focus" defaultValue="O:\Clips" />
        </InputRow>
        <InputRow label="disabled">
          <input className="sc-input is-disabled" disabled defaultValue="O:\Clips" />
        </InputRow>
        <InputRow label="error">
          <input className="sc-input is-error" defaultValue="ZZZZ" />
          <span className="sc-input-error-msg">
            Folder does not exist. Pick another or click Reset.
          </span>
        </InputRow>
      </div>
    </section>
  );
}

function InputRow(props: { label: string; children: React.ReactNode }) {
  return (
    <div className="input-row">
      <span className="input-label-cell">{props.label}</span>
      <span>{props.children}</span>
    </div>
  );
}

// ---------- Status strip ---------- //

function StatusStripSection() {
  return (
    <section className="showcase-section">
      <h2 className="showcase-section-title">Status strip / toast</h2>
      <p className="showcase-section-note">
        Single horizontal strip above the transport. Idle reserves the slot
        but renders nothing visible. Success / warning with embedded actions
        persist; without actions, auto-dismiss after 4s. Errors persist until
        dismissed.
      </p>

      <div className="sc-stack">
        <ShowcaseStatus tone="inprogress" icon="●">
          <span>Encoding clip — region 1 of 2</span>
          <span className="sc-status-progress" aria-hidden>
            <span className="sc-status-progress-fill" />
          </span>
        </ShowcaseStatus>

        <ShowcaseStatus tone="success" icon="✓">
          <span>Exported clip_44-49.mp3 · 113 KB</span>
          <span className="sc-status-actions">
            <button className="sc-link-button">Open folder</button>
            <button className="sc-link-button">Copy path</button>
          </span>
        </ShowcaseStatus>

        <ShowcaseStatus tone="warning" icon="⚠">
          <span>Stitched export needs every region to share the same crop.</span>
        </ShowcaseStatus>

        <ShowcaseStatus tone="error" icon="!">
          <span>FFmpeg exited with code -22.</span>
          <span className="sc-status-actions">
            <button className="sc-link-button">Details</button>
          </span>
        </ShowcaseStatus>

        <div className="sc-status-idle-note">
          idle state — slot reserves 56px but renders nothing
        </div>
      </div>
    </section>
  );
}

function ShowcaseStatus(props: {
  tone: "inprogress" | "success" | "warning" | "error";
  icon: string;
  children: React.ReactNode;
}) {
  return (
    <div className={`sc-status sc-status-${props.tone}`}>
      <span className="sc-status-icon" aria-hidden>{props.icon}</span>
      <span className="sc-status-text">{props.children}</span>
    </div>
  );
}

// ---------- Loading patterns ---------- //

function LoadingPatternsSection() {
  return (
    <section className="showcase-section">
      <h2 className="showcase-section-title">Loading patterns</h2>
      <p className="showcase-section-note">
        Determinate for known-step operations. Indeterminate (pulse) for
        operations with no progress signal. Skeleton for components still
        mounting. UI is never blocked — only the relevant control disables.
      </p>
      <div className="loading-grid">
        <div className="loading-card">
          <p className="loading-card-title">Determinate · progress bar</p>
          <div className="loading-bar">
            <span className="loading-bar-fill" />
          </div>
        </div>
        <div className="loading-card">
          <p className="loading-card-title">Indeterminate · pulse</p>
          <span className="loading-pulse" />
          <span style={{ marginLeft: 10, fontSize: "var(--fs-small)", color: "var(--text-secondary)" }}>
            Probing hardware…
          </span>
        </div>
        <div className="loading-card">
          <p className="loading-card-title">Skeleton · shimmer placeholders</p>
          <div className="loading-skeleton">
            <span className="loading-skeleton-line w-60" />
            <span className="loading-skeleton-line w-90" />
            <span className="loading-skeleton-line w-40" />
          </div>
        </div>
      </div>
    </section>
  );
}

// ---------- Chips / pills / kbd / tags ---------- //

function ChipsSection() {
  return (
    <section className="showcase-section">
      <h2 className="showcase-section-title">Chips · Pills · Kbd · Tags</h2>
      <p className="showcase-section-note">
        Glanceable status / metadata. Pills are uppercase + tracked. Kbd
        renders a keycap. Tags are a compact rounded label for metadata.
      </p>

      <div className="showcase-subsection">
        <h3 className="showcase-subsection-title">Chips (with optional leading dot)</h3>
        <div className="chip-grid">
          <span className="sc-chip"><span className="sc-chip-dot" style={{ background: "var(--region-1)" }} /> Region 1</span>
          <span className="sc-chip"><span className="sc-chip-dot" style={{ background: "var(--region-2)" }} /> Region 2</span>
          <span className="sc-chip"><span className="sc-chip-dot" style={{ background: "var(--region-3)" }} /> Region 3</span>
          <span className="sc-chip sc-chip-accent">Game allowlist · 117</span>
        </div>
      </div>

      <div className="showcase-subsection">
        <h3 className="showcase-subsection-title">Pills (uppercase)</h3>
        <div className="chip-grid">
          <span className="sc-pill sc-pill-good">direct</span>
          <span className="sc-pill sc-pill-warn">re-encode</span>
          <span className="sc-pill">draft</span>
          <span className="sc-pill">global</span>
        </div>
      </div>

      <div className="showcase-subsection">
        <h3 className="showcase-subsection-title">Kbd / shortcuts</h3>
        <div className="chip-grid">
          <span className="sc-kbd">Space</span>
          <span className="sc-kbd">Ctrl+O</span>
          <span className="sc-kbd">F</span>
          <span className="sc-kbd">Shift+F</span>
          <span className="sc-kbd">Alt+F10</span>
        </div>
      </div>

      <div className="showcase-subsection">
        <h3 className="showcase-subsection-title">Tags (mono)</h3>
        <div className="chip-grid">
          <span className="sc-tag">2560×1440</span>
          <span className="sc-tag">60.00fps</span>
          <span className="sc-tag">h264/aac</span>
          <span className="sc-tag">1:58.65</span>
        </div>
      </div>
    </section>
  );
}

// ---------- Accordion + Card ---------- //

function AccordionCardSection() {
  const [open, setOpen] = useState<string | null>("audio");
  return (
    <section className="showcase-section">
      <h2 className="showcase-section-title">Accordion · Card</h2>
      <p className="showcase-section-note">
        Accordion header collapses into a card on hover-fill. The summary on
        the right reflects current selection (count of items, last value,
        etc.). Cards are the canonical content surface inside dialogs.
      </p>

      <div className="showcase-subsection">
        <h3 className="showcase-subsection-title">Accordion (collapsed + open)</h3>
        <SCAccordion
          id="audio"
          open={open === "audio"}
          onToggle={() => setOpen(open === "audio" ? null : "audio")}
          title="Audio sources"
          summary="4 devices selected"
        >
          <p style={{ margin: 0 }}>
            Pick output devices to capture as separate audio tracks. Inline
            rename gives each track a meaningful name in the saved MP4.
          </p>
        </SCAccordion>
        <SCAccordion
          id="games"
          open={open === "games"}
          onToggle={() => setOpen(open === "games" ? null : "games")}
          title="Games tracked"
          summary="117 entries · 2 added recently"
        >
          <div className="sc-search" style={{ marginBottom: 14 }}>
            <span className="sc-search-icon" aria-hidden>⌕</span>
            <input
              className="sc-search-input"
              type="text"
              placeholder="Search games…"
              defaultValue=""
            />
          </div>

          <div className="sc-games-section-label">
            Recently added <span className="sc-games-section-count">2</span>
          </div>
          <div className="sc-game-row">
            <span>escapefromtarkov.exe</span>
            <span className="sc-game-row-path">C:\Battlestate Games\Escape From Tarkov</span>
          </div>
          <div className="sc-game-row">
            <span>helldivers2.exe</span>
            <span className="sc-game-row-path">C:\Program Files (x86)\Steam\steamapps\common\Helldivers 2</span>
          </div>

          <div className="sc-games-section-label">
            Steam <span className="sc-games-section-count">115</span>
          </div>
          <div className="sc-game-row">
            <span>aimlab_tb.exe</span>
            <span className="sc-game-row-path">…\Steam\steamapps\common\Aim Lab</span>
          </div>
          <div className="sc-game-row">
            <span>cs2.exe</span>
            <span className="sc-game-row-path">…\Steam\steamapps\common\Counter-Strike 2</span>
          </div>
        </SCAccordion>
      </div>

      <div className="showcase-subsection">
        <h3 className="showcase-subsection-title">Card</h3>
        <div className="sc-card" style={{ maxWidth: 480 }}>
          <p className="sc-card-title">Replay buffer is watching</p>
          <p className="sc-card-blurb">
            Focus a game in the allowlist and your save hotkey will flush the
            last 5 minutes to MP4.
          </p>
          <div className="sc-flex-row">
            <button className="sc-btn sc-btn-primary">Save replay</button>
            <button className="sc-btn sc-btn-ghost">Stop buffer</button>
          </div>
        </div>
      </div>
    </section>
  );
}

function KbRow(props: { name: string; desc: string; combo: string; global?: boolean }) {
  return (
    <div className="sc-kb-row">
      <span className="sc-kb-row-action">
        <span className="sc-kb-row-action-name">
          {props.name}
          {props.global && (
            <span className="sc-pill" style={{ marginLeft: 8, height: 18, fontSize: 10 }}>
              global
            </span>
          )}
        </span>
        <span className="sc-kb-row-action-desc">{props.desc}</span>
      </span>
      <span className="sc-kbd">{props.combo}</span>
    </div>
  );
}

function SCAccordion(props: {
  id: string;
  open: boolean;
  onToggle: () => void;
  title: string;
  summary: string;
  children: React.ReactNode;
}) {
  return (
    <div className="sc-accordion">
      <button className="sc-accordion-head" onClick={props.onToggle} aria-expanded={props.open}>
        <span className="sc-accordion-arrow">{props.open ? "▾" : "▸"}</span>
        <span className="sc-accordion-title">{props.title}</span>
        <span className="sc-accordion-summary">{props.summary}</span>
      </button>
      {props.open && <div className="sc-accordion-body">{props.children}</div>}
    </div>
  );
}

// ---------- Topbar preview ---------- //

function TopbarPreview() {
  const [helpOpen, setHelpOpen] = useState(true);
  return (
    <section className="showcase-section">
      <h2 className="showcase-section-title">Topbar preview</h2>
      <p className="showcase-section-note">
        Filename uses --fs-heading and dominates the bar. Metadata renders
        small + tertiary so it reads as the spec line under the name.
        Strategy is a pill: green for stream-copy, amber for re-encode. The
        "?" menu (shown open in the first example) replaces the old Tips
        dialog with category access.
      </p>

      <div className="sc-topbar" style={{ position: "relative" }}>
        <span className="sc-brand">
          <span className="sc-brand-mark" aria-hidden />
          Clippy
        </span>
        <span style={{ width: 1, height: 22, background: "var(--border-subtle)" }} />
        <span className="sc-topbar-file">
          <span className="sc-topbar-filename">
            Escape-from-Tarkov_2026-05-10_22-28-32.mp4
          </span>
          <span className="sc-topbar-meta">
            <span>2560×1440</span><span>·</span>
            <span>60.00fps</span><span>·</span>
            <span>h264/aac</span><span>·</span>
            <span>1:58.65</span>
            <span className="sc-pill sc-pill-good" style={{ marginLeft: 4 }}>direct</span>
          </span>
        </span>
        <span className="sc-help-wrap">
          <button
            className="sc-btn sc-btn-ghost"
            aria-expanded={helpOpen}
            onClick={() => setHelpOpen((v) => !v)}
          >
            ?
          </button>
          {helpOpen && (
            <div className="sc-help-menu" role="menu" aria-label="Help">
              <div className="sc-help-menu-section">Playback</div>
              <button className="sc-help-menu-item">
                <span>Play / Pause</span>
                <span className="sc-help-menu-item-shortcut">Space</span>
              </button>
              <button className="sc-help-menu-item">
                <span>Frame back / forward</span>
                <span className="sc-help-menu-item-shortcut">Q / E</span>
              </button>
              <div className="sc-help-menu-section">Regions</div>
              <button className="sc-help-menu-item">
                <span>Set in / out point</span>
                <span className="sc-help-menu-item-shortcut">F / Shift+F</span>
              </button>
              <button className="sc-help-menu-item">
                <span>Crop current region</span>
                <span className="sc-help-menu-item-shortcut">Shift+C</span>
              </button>
              <div className="sc-help-menu-divider" />
              <button className="sc-help-menu-item">
                <span>Open full keybind editor…</span>
              </button>
            </div>
          )}
        </span>
        <button className="sc-btn sc-btn-primary">Export</button>
      </div>

      <div className="sc-topbar" style={{ marginTop: 12 }}>
        <span className="sc-brand">
          <span className="sc-brand-mark" aria-hidden />
          Clippy
        </span>
        <span style={{ width: 1, height: 22, background: "var(--border-subtle)" }} />
        <span className="sc-topbar-file">
          <span className="sc-topbar-filename">2024-04-12 dashcam.mkv</span>
          <span className="sc-topbar-meta">
            <span>1920×1080</span><span>·</span>
            <span>30.00fps</span><span>·</span>
            <span>hevc/ac3</span><span>·</span>
            <span>12:04.10</span>
            <span className="sc-pill sc-pill-warn" style={{ marginLeft: 4 }}>re-encode</span>
          </span>
        </span>
        <button className="sc-btn sc-btn-ghost">?</button>
        <button className="sc-btn sc-btn-primary">Export</button>
      </div>
    </section>
  );
}

// ---------- Empty editor preview ---------- //

function EmptyEditorPreview() {
  return (
    <section className="showcase-section">
      <h2 className="showcase-section-title">Empty editor state</h2>
      <p className="showcase-section-note">
        Centered primary as the main affordance. No permanent dashed border —
        the active-drag overlay handles the drop-target visual when the user
        is actually dragging.
      </p>
      <div className="sc-empty-editor">
        <button className="sc-btn sc-btn-primary">Open video…</button>
        <span className="sc-empty-hint">or drag a video here</span>
      </div>
    </section>
  );
}

// ---------- Tabbed-modal preview ---------- //

function TabbedModalPreview() {
  const [active, setActive] = useState<"replay" | "keyboard" | "storage" | "about">("replay");
  const tabs: Array<{ id: typeof active; label: string }> = [
    { id: "replay",   label: "Replay buffer" },
    { id: "keyboard", label: "Keyboard" },
    { id: "storage",  label: "Storage" },
    { id: "about",    label: "About" },
  ];
  return (
    <section className="showcase-section">
      <h2 className="showcase-section-title">Settings dialog · tabbed-modal preview</h2>
      <p className="showcase-section-note">
        Left-rail navigation + content area + footer. Left-rail structure is
        locked. The header below uses --fs-display weight 600 — the top-level
        section title.
      </p>
      <div className="sc-modal">
        <header className="sc-modal-header">
          <span className="sc-modal-title">Settings</span>
          <button className="sc-btn sc-btn-ghost" aria-label="Close">×</button>
        </header>
        <div className="sc-modal-body">
          <nav className="sc-modal-nav" aria-label="Settings sections">
            {tabs.map((t) => (
              <button
                key={t.id}
                className={`sc-modal-nav-btn${active === t.id ? " is-active" : ""}`}
                onClick={() => setActive(t.id)}
              >
                {t.label}
              </button>
            ))}
          </nav>
          <div className="sc-modal-content">
            <h4>{tabs.find((t) => t.id === active)?.label}</h4>
            {active === "replay" && (
              <>
                <p>
                  Continuously captures the last few minutes of focused
                  gameplay. Press your save hotkey to flush the buffer to an
                  MP4.
                </p>
                <div className="sc-flex-row">
                  <button className="sc-btn sc-btn-secondary">Browse…</button>
                  <button className="sc-btn sc-btn-secondary">Reset</button>
                </div>
              </>
            )}
            {active === "keyboard" && (
              <>
                <p>
                  Click any binding to record a new key combo. Globals (tagged
                  GLOBAL) fire even when Clippy is unfocused.
                </p>
                <div className="sc-kb-grid">
                  <div>
                    <p className="sc-kb-group-label">Playback</p>
                    <KbRow name="Play / Pause" desc="Toggles playback at the current position." combo="Space" />
                    <KbRow name="Frame back / forward" desc="Step a single frame in either direction." combo="Q / E" />
                    <KbRow name="Jump to start / end" desc="Move the playhead to the file boundaries." combo="Home / End" />
                  </div>
                  <div>
                    <p className="sc-kb-group-label">Regions</p>
                    <KbRow name="Set in point" desc="Mark the start of a new region at the playhead." combo="F" />
                    <KbRow name="Set out point" desc="Mark the end of the in-progress region." combo="Shift+F" />
                    <KbRow name="Crop current region" desc="Opens the crop overlay for the region under the playhead." combo="Shift+C" />
                  </div>
                  <div>
                    <p className="sc-kb-group-label">Saves & Exports</p>
                    <KbRow name="Save replay" desc="Flushes the buffer to MP4 in your save folder." combo="Alt+F10" global />
                    <KbRow name="Export selection" desc="Opens the export dialog with the current regions." combo="Ctrl+E" />
                  </div>
                </div>
              </>
            )}
            {active === "storage" && (
              <p>
                Clippy keeps everything local. Proxy cache, diagnostics log,
                and project state live under <code>%APPDATA%\Clippy\</code>.
              </p>
            )}
            {active === "about" && (
              <p>
                Local-only video clip editor for Windows. No telemetry, no
                cloud.
              </p>
            )}
          </div>
        </div>
        <footer className="sc-modal-footer">
          <button className="sc-btn sc-btn-ghost">Cancel</button>
          <button className="sc-btn sc-btn-primary">Done</button>
        </footer>
      </div>
    </section>
  );
}
