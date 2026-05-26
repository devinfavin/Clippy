import type { CSSProperties, ReactNode } from "react";

/**
 * Settings primitives — the visual vocabulary the Claude Design exploration
 * settled on for the settings panels. Each tab composes these instead of
 * defining its own ad-hoc row/group/toggle styles, so the four panels read
 * as one design system.
 *
 *   SettingsLabel  → uppercase eyebrow label above a SettingsGroup
 *   SettingsGroup  → rounded card with hairline-separated rows
 *   SettingsRow    → title + subtitle on left, control slot on right
 *   Toggle         → pill switch, lilac when on
 *   Stepper        → number stepper with +/- buttons
 *   SelectField    → styled dropdown
 *   StatusCard     → tinted footer card ("Buffer running · …" etc)
 */

export function SettingsLabel(props: { children: ReactNode }) {
  return <div className="s-label">{props.children}</div>;
}

export function SettingsGroup(props: { children: ReactNode; className?: string }) {
  const cls = "s-group" + (props.className ? ` ${props.className}` : "");
  return <div className={cls}>{props.children}</div>;
}

export function SettingsRow(props: {
  title: ReactNode;
  subtitle?: ReactNode;
  children?: ReactNode;
  /** Render the row as a button-like accessory row (chevron on right). Used
   *  by About-style nav entries; emits onClick. */
  onClick?: () => void;
  accessory?: boolean;
}) {
  const inner = (
    <>
      <div className="s-row-text">
        <div className="s-row-title">{props.title}</div>
        {props.subtitle && <div className="s-row-subtitle">{props.subtitle}</div>}
      </div>
      {props.children && <div className="s-row-control">{props.children}</div>}
      {props.accessory && (
        <svg className="s-row-chevron" width="14" height="14" viewBox="0 0 24 24"
             fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round"
             strokeLinejoin="round" aria-hidden>
          <path d="M9 6l6 6-6 6" />
        </svg>
      )}
    </>
  );
  if (props.onClick) {
    return (
      <button type="button" className="s-row s-row-button" onClick={props.onClick}>
        {inner}
      </button>
    );
  }
  return <div className="s-row">{inner}</div>;
}

export function Toggle(props: {
  value: boolean;
  onChange: (v: boolean) => void;
  /** Override the active-color (e.g. green for "running" states). Defaults
   *  to the brand lilac. */
  color?: string;
  ariaLabel?: string;
}) {
  const style: CSSProperties | undefined = props.color
    ? ({ ["--toggle-on" as never]: props.color } as CSSProperties)
    : undefined;
  return (
    <button
      type="button"
      role="switch"
      aria-checked={props.value}
      aria-label={props.ariaLabel}
      className={`s-toggle${props.value ? " is-on" : ""}`}
      style={style}
      onClick={() => props.onChange(!props.value)}
    >
      <span className="s-toggle-knob" aria-hidden />
    </button>
  );
}

export function Stepper(props: {
  value: number;
  onChange: (v: number) => void;
  min: number;
  max: number;
  step?: number;
  unit?: string;
}) {
  const step = props.step ?? 1;
  const clamp = (n: number) => Math.max(props.min, Math.min(props.max, n));
  return (
    <div className="s-stepper">
      <button
        type="button"
        className="s-stepper-btn"
        onClick={() => props.onChange(clamp(props.value - step))}
        disabled={props.value <= props.min}
        aria-label="Decrease"
      >−</button>
      <span className="s-stepper-value mono">
        {props.value}
        {props.unit && <span className="s-stepper-unit">{props.unit}</span>}
      </span>
      <button
        type="button"
        className="s-stepper-btn"
        onClick={() => props.onChange(clamp(props.value + step))}
        disabled={props.value >= props.max}
        aria-label="Increase"
      >+</button>
    </div>
  );
}

export function SelectField<V extends string | number>(props: {
  value: V;
  onChange: (v: V) => void;
  options: ReadonlyArray<{ value: V; label: string } | V>;
  width?: number;
  ariaLabel?: string;
}) {
  const opts = props.options.map((o) =>
    typeof o === "object" && o !== null && "value" in o ? o : { value: o as V, label: String(o) }
  );
  return (
    <div className="s-select-wrap" style={props.width ? { width: props.width } : undefined}>
      <select
        className="s-select"
        value={String(props.value)}
        onChange={(e) => {
          // Best-effort restoration of the original type (string|number).
          const raw: string = e.target.value;
          const match = opts.find((o) => String(o.value) === raw);
          props.onChange(match ? match.value : (raw as V));
        }}
        aria-label={props.ariaLabel}
      >
        {opts.map((o) => (
          <option key={String(o.value)} value={String(o.value)}>{o.label}</option>
        ))}
      </select>
      <svg className="s-select-chevron" width="12" height="12" viewBox="0 0 24 24"
           fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round"
           strokeLinejoin="round" aria-hidden>
        <path d="M6 9l6 6 6-6" />
      </svg>
    </div>
  );
}

/** Tinted status card — used as a section footer when a panel has a
 *  "running"/"active" state to surface. `tone` controls the tint hue. */
export function StatusCard(props: {
  tone: "good" | "warn" | "bad" | "info";
  children: ReactNode;
}) {
  return <div className={`s-status-card tone-${props.tone}`}>{props.children}</div>;
}
