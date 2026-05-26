import type { SettingsTabMeta } from "./index";

/**
 * Tiny icon set for the Settings sidebar. One inline SVG per tab — kept here
 * rather than in a generic Icon component because (a) it's only four glyphs,
 * (b) the colored block wrapper is settings-specific, and (c) keeps the
 * settings module self-contained.
 */
export function SettingsIcon(props: { meta: SettingsTabMeta }) {
  const { iconKey, iconColor } = props.meta;
  return (
    <span
      className="settings-tab-icon"
      style={{ background: iconColor }}
      aria-hidden
    >
      {renderGlyph(iconKey)}
    </span>
  );
}

function renderGlyph(k: SettingsTabMeta["iconKey"]) {
  const stroke = 1.8;
  const common = {
    width: 12,
    height: 12,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: stroke,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };
  switch (k) {
    case "replay":
      // Filled record dot with halo ring.
      return (
        <svg {...common}>
          <circle cx="12" cy="12" r="9" />
          <circle cx="12" cy="12" r="3.5" fill="currentColor" stroke="none" />
        </svg>
      );
    case "keyboard":
      // Four-pointed sparkle to match the design — keyboard shortcuts as the
      // "spark" of productivity. Filled so it reads from across the sidebar.
      return (
        <svg {...common}>
          <path d="M12 3l2 5 5 2-5 2-2 5-2-5-5-2 5-2 2-5z" fill="currentColor" />
        </svg>
      );
    case "storage":
      // Folder.
      return (
        <svg {...common}>
          <path d="M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v9a2 2 0 01-2 2H5a2 2 0 01-2-2V7z" />
        </svg>
      );
    case "about":
      // Question mark in a circle.
      return (
        <svg {...common}>
          <circle cx="12" cy="12" r="9" />
          <path d="M9.5 9a2.5 2.5 0 015 0c0 1.5-1.5 2-2.5 3v1M12 17.5h.01" />
        </svg>
      );
  }
}
