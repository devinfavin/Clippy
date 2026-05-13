export type SettingsTabId = "replay" | "keyboard" | "storage" | "about";

export const SETTINGS_TABS: readonly { id: SettingsTabId; label: string }[] = [
  { id: "replay", label: "Replay buffer" },
  { id: "keyboard", label: "Keyboard" },
  { id: "storage", label: "Storage" },
  { id: "about", label: "About" },
] as const;

export { KeyboardSettingsTab } from "./tabs/keyboard";
export { StorageSettingsTab } from "./tabs/storage";
export { AboutTab } from "./tabs/about";
