import pkg from "../../package.json";

/** Single source of truth for the user-facing version string. The About tab
 *  and the settings sidebar footer both read this; previously the About tab
 *  hard-coded a stale "0.3.2" that didn't track package.json bumps. */
export const APP_VERSION: string = pkg.version;

export type SettingsTabId = "replay" | "keyboard" | "storage" | "about";

/**
 * Settings tab metadata. `iconColor` is the per-tab tint used by the
 * sidebar icon block — each tab gets its own hue so the rail reads as a
 * directory rather than four undifferentiated rows. Keep these in the
 * neutral-ish range; the rail sits next to a tinted active background and
 * we don't want clashes.
 */
export type SettingsTabMeta = {
  id: SettingsTabId;
  label: string;
  iconKey: "replay" | "keyboard" | "storage" | "about";
  iconColor: string;
};

export const SETTINGS_TABS: readonly SettingsTabMeta[] = [
  { id: "replay",   label: "Replay buffer", iconKey: "replay",   iconColor: "#f87171" },
  { id: "keyboard", label: "Keyboard",      iconKey: "keyboard", iconColor: "#c8b6ff" },
  { id: "storage",  label: "Storage",       iconKey: "storage",  iconColor: "#4ade80" },
  { id: "about",    label: "About",         iconKey: "about",    iconColor: "#94a3b8" },
] as const;

export { KeyboardSettingsTab } from "./tabs/keyboard";
export { StorageSettingsTab } from "./tabs/storage";
export { AboutTab } from "./tabs/about";
