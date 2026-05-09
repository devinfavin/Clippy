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
  | "jumpRegion1"
  | "jumpRegion2"
  | "jumpRegion3"
  | "jumpRegion4"
  | "jumpRegion5"
  | "jumpRegion6"
  | "jumpRegion7"
  | "jumpRegion8"
  | "jumpRegion9";

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
