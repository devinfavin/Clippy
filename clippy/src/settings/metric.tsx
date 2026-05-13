export function Metric(props: {
  label: string;
  value: string;
  tone: "ok" | "info" | "warn";
  aux?: string;
}) {
  return (
    <div className={`settings-calc-metric tone-${props.tone}`}>
      <div className="settings-calc-label">{props.label}</div>
      <div className="settings-calc-value">{props.value}</div>
      {props.aux && <div className="settings-calc-aux">{props.aux}</div>}
    </div>
  );
}
