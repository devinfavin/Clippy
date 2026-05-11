// Keyboard binding definitions, persistence, and matching helpers.

export type Keybind = {
  key: string;
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
};

export type ActionId =
  | "openFile"
  | "playPause"
  | "frameBack"
  | "frameForward"
  | "jumpStart"
  | "jumpEnd"
  | "setIn"
  | "setOut"
  | "export"
  | "loopRegion"
  | "cropRegion"
  | "saveFrame"
  | "saveReplay"
  | "jumpRegion1"
  | "jumpRegion2"
  | "jumpRegion3"
  | "jumpRegion4"
  | "jumpRegion5"
  | "jumpRegion6"
  | "jumpRegion7"
  | "jumpRegion8"
  | "jumpRegion9";

/** Actions whose hotkey fires at the OS level (works while Clippy isn't focused). */
export const GLOBAL_ACTIONS: ReadonlySet<ActionId> = new Set<ActionId>(["saveReplay"]);

export type Keybinds = Record<ActionId, Keybind>;

export const ACTION_LABELS: Record<ActionId, string> = {
  openFile: "Open file",
  playPause: "Play / Pause",
  frameBack: "Frame back",
  frameForward: "Frame forward",
  jumpStart: "Jump to start",
  jumpEnd: "Jump to end",
  setIn: "Set in point",
  setOut: "Set out point",
  export: "Export selection",
  loopRegion: "Loop current region",
  cropRegion: "Crop current region",
  saveFrame: "Save current frame as PNG",
  saveReplay: "Save replay buffer",
  jumpRegion1: "Jump to region 1",
  jumpRegion2: "Jump to region 2",
  jumpRegion3: "Jump to region 3",
  jumpRegion4: "Jump to region 4",
  jumpRegion5: "Jump to region 5",
  jumpRegion6: "Jump to region 6",
  jumpRegion7: "Jump to region 7",
  jumpRegion8: "Jump to region 8",
  jumpRegion9: "Jump to region 9",
};

export type ActionGroup = "playback" | "selection" | "regions" | "capture" | "exports";

export const ACTION_GROUP_LABELS: Record<ActionGroup, string> = {
  playback:  "Playback",
  selection: "Selection",
  regions:   "Regions",
  capture:   "Capture",
  exports:   "Saves & Exports",
};

export const ACTION_GROUPS: Record<ActionId, ActionGroup> = {
  playPause:    "playback",
  frameBack:    "playback",
  frameForward: "playback",
  jumpStart:    "playback",
  jumpEnd:      "playback",
  setIn:        "selection",
  setOut:       "selection",
  loopRegion:   "regions",
  cropRegion:   "regions",
  jumpRegion1:  "regions",
  jumpRegion2:  "regions",
  jumpRegion3:  "regions",
  jumpRegion4:  "regions",
  jumpRegion5:  "regions",
  jumpRegion6:  "regions",
  jumpRegion7:  "regions",
  jumpRegion8:  "regions",
  jumpRegion9:  "regions",
  saveReplay:   "capture",
  openFile:     "exports",
  export:       "exports",
  saveFrame:    "exports",
};

export const ACTION_DESCRIPTIONS: Record<ActionId, string> = {
  openFile:     "Pick a video file to load into the editor.",
  playPause:    "Toggles playback at the current playhead.",
  frameBack:    "Step a single frame backward.",
  frameForward: "Step a single frame forward.",
  jumpStart:    "Move the playhead to the start of the file.",
  jumpEnd:      "Move the playhead to the end of the file.",
  setIn:        "Mark the start of a new region at the playhead.",
  setOut:       "Mark the end of the in-progress region.",
  export:       "Open the export dialog with the current regions.",
  loopRegion:   "Toggle loop playback for the region under the playhead.",
  cropRegion:   "Open the crop overlay for the region under the playhead.",
  saveFrame:    "Save the current frame as a PNG to your save folder.",
  saveReplay:   "Flush the replay buffer to MP4. Fires while Clippy is unfocused.",
  jumpRegion1:  "Jump the playhead to region 1.",
  jumpRegion2:  "Jump the playhead to region 2.",
  jumpRegion3:  "Jump the playhead to region 3.",
  jumpRegion4:  "Jump the playhead to region 4.",
  jumpRegion5:  "Jump the playhead to region 5.",
  jumpRegion6:  "Jump the playhead to region 6.",
  jumpRegion7:  "Jump the playhead to region 7.",
  jumpRegion8:  "Jump the playhead to region 8.",
  jumpRegion9:  "Jump the playhead to region 9.",
};

export const DEFAULT_KEYBINDS: Keybinds = {
  openFile:     { key: "o", ctrl: true },
  playPause:    { key: " " },
  frameBack:    { key: "," },
  frameForward: { key: "." },
  jumpStart:    { key: "Home" },
  jumpEnd:      { key: "End" },
  setIn:        { key: "i" },
  setOut:       { key: "o" },
  export:       { key: "e", ctrl: true },
  loopRegion:   { key: "l" },
  cropRegion:   { key: "c", shift: true },
  saveFrame:    { key: "s", shift: true },
  saveReplay:   { key: "F10", alt: true },
  jumpRegion1:  { key: "1" },
  jumpRegion2:  { key: "2" },
  jumpRegion3:  { key: "3" },
  jumpRegion4:  { key: "4" },
  jumpRegion5:  { key: "5" },
  jumpRegion6:  { key: "6" },
  jumpRegion7:  { key: "7" },
  jumpRegion8:  { key: "8" },
  jumpRegion9:  { key: "9" },
};

export const KEYBINDS_STORAGE_KEY = "clippy.keybinds.v1";

/**
 * Convert a Keybind into the string format Tauri's `Shortcut::from_str` accepts
 * (e.g. "Alt+F10", "Ctrl+Shift+S"). Used for global hotkeys like the save-replay
 * binding, which must be re-registered with the OS when the user rebinds.
 */
export function keybindToShortcutString(b: Keybind): string | null {
  const parts: string[] = [];
  if (b.ctrl) parts.push("Ctrl");
  if (b.shift) parts.push("Shift");
  if (b.alt) parts.push("Alt");
  let k = b.key;
  if (k === " ") k = "Space";
  else if (k === "ArrowLeft") k = "Left";
  else if (k === "ArrowRight") k = "Right";
  else if (k === "ArrowUp") k = "Up";
  else if (k === "ArrowDown") k = "Down";
  else if (k.length === 1) k = k.toUpperCase();
  // Tauri rejects modifier-only or empty keys
  if (!k || k === "Control" || k === "Shift" || k === "Alt" || k === "Meta") return null;
  parts.push(k);
  return parts.join("+");
}

export function formatKeybind(b: Keybind): string {
  const parts: string[] = [];
  if (b.ctrl) parts.push("Ctrl");
  if (b.shift) parts.push("Shift");
  if (b.alt) parts.push("Alt");
  let k = b.key;
  if (k === " ") k = "Space";
  else if (k === "ArrowLeft") k = "←";
  else if (k === "ArrowRight") k = "→";
  else if (k === "ArrowUp") k = "↑";
  else if (k === "ArrowDown") k = "↓";
  else if (k.length === 1) k = k.toUpperCase();
  parts.push(k);
  return parts.join("+");
}

export function matchesBinding(e: KeyboardEvent, b: Keybind): boolean {
  const ek = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  const bk = b.key.length === 1 ? b.key.toLowerCase() : b.key;
  if (ek !== bk) return false;
  if (!!b.ctrl !== (e.ctrlKey || e.metaKey)) return false;
  if (!!b.shift !== e.shiftKey) return false;
  if (!!b.alt !== e.altKey) return false;
  return true;
}

export function captureKeybind(e: KeyboardEvent): Keybind | null {
  if (e.key === "Control" || e.key === "Shift" || e.key === "Alt" || e.key === "Meta") return null;
  if (e.key === "Escape" && !e.ctrlKey && !e.shiftKey && !e.altKey) return null;
  const k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  const out: Keybind = { key: k };
  if (e.ctrlKey || e.metaKey) out.ctrl = true;
  if (e.shiftKey) out.shift = true;
  if (e.altKey) out.alt = true;
  return out;
}

export function loadKeybinds(): Keybinds {
  try {
    const raw = localStorage.getItem(KEYBINDS_STORAGE_KEY);
    if (!raw) return DEFAULT_KEYBINDS;
    const parsed = JSON.parse(raw);
    return { ...DEFAULT_KEYBINDS, ...parsed };
  } catch {
    return DEFAULT_KEYBINDS;
  }
}

export function saveKeybinds(k: Keybinds) {
  try {
    localStorage.setItem(KEYBINDS_STORAGE_KEY, JSON.stringify(k));
  } catch {}
}
