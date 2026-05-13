/** Lightweight controlled accordion for the settings sub-sections. */
export function Accordion(props: {
  open: boolean;
  onToggle: () => void;
  title: string;
  summary: string;
  children: React.ReactNode;
}) {
  return (
    <div className={`settings-accordion${props.open ? " is-open" : ""}`}>
      <button
        className="settings-accordion-head"
        onClick={props.onToggle}
        aria-expanded={props.open}
      >
        <span className="settings-accordion-arrow" aria-hidden>
          {props.open ? "▾" : "▸"}
        </span>
        <span className="settings-accordion-title">{props.title}</span>
        <span className="settings-accordion-summary">{props.summary}</span>
      </button>
      {props.open && <div className="settings-accordion-body">{props.children}</div>}
    </div>
  );
}
