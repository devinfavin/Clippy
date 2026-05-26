import type { CSSProperties, ReactNode } from "react";

export type RailTabOption<V extends string> = {
  value: V;
  label: string;
  icon?: ReactNode;
  badge?: number;
};

/**
 * Segmented tab strip with a sliding indicator. Used by the editor rail to
 * switch between Audio / Regions / Crop. The indicator's left/width are
 * derived from the active option's position so the motion stays continuous
 * across re-renders.
 *
 * Generic over the value type so each consumer keeps its own union (e.g.
 * `"audio" | "regions" | "crop"`).
 */
export function RailTabs<V extends string>(props: {
  value: V;
  options: RailTabOption<V>[];
  onChange: (v: V) => void;
}) {
  const { value, options, onChange } = props;
  const activeIdx = Math.max(0, options.findIndex((o) => o.value === value));
  const indicatorStyle: CSSProperties = {
    left: `calc(3px + ${activeIdx} * ((100% - 6px) / ${options.length}))`,
    width: `calc((100% - 6px) / ${options.length})`,
  };
  return (
    <div className="rail-tabs" role="tablist">
      <div className="rail-tab-indicator" style={indicatorStyle} aria-hidden />
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="tab"
          aria-selected={o.value === value}
          className={`rail-tab${o.value === value ? " active" : ""}`}
          onClick={() => onChange(o.value)}
        >
          {o.icon && <span className="rail-tab-icon" aria-hidden>{o.icon}</span>}
          <span>{o.label}</span>
          {o.badge != null && o.badge > 0 && (
            <span className="rail-tab-badge" aria-hidden>{o.badge}</span>
          )}
        </button>
      ))}
    </div>
  );
}
