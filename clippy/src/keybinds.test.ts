import { describe, expect, it } from "vitest";
import {
  captureKeybind,
  formatKeybind,
  keybindToShortcutString,
  matchesBinding,
  type Keybind,
} from "./keybinds";

/** Build a minimal KeyboardEvent stand-in. */
function ke(
  key: string,
  opts: { ctrl?: boolean; shift?: boolean; alt?: boolean; meta?: boolean } = {}
): KeyboardEvent {
  return new KeyboardEvent("keydown", {
    key,
    ctrlKey: !!opts.ctrl,
    shiftKey: !!opts.shift,
    altKey: !!opts.alt,
    metaKey: !!opts.meta,
  });
}

describe("keybindToShortcutString", () => {
  it("formats the default save-replay binding the way Tauri's Shortcut::from_str expects", () => {
    expect(keybindToShortcutString({ key: "F10", alt: true })).toBe("Alt+F10");
  });

  it("orders modifiers Ctrl, Shift, Alt", () => {
    expect(
      keybindToShortcutString({ key: "s", ctrl: true, shift: true, alt: true })
    ).toBe("Ctrl+Shift+Alt+S");
  });

  it("renames Space and arrow keys to Tauri's identifiers", () => {
    expect(keybindToShortcutString({ key: " ", ctrl: true })).toBe("Ctrl+Space");
    expect(keybindToShortcutString({ key: "ArrowLeft" })).toBe("Left");
    expect(keybindToShortcutString({ key: "ArrowRight", alt: true })).toBe(
      "Alt+Right"
    );
    expect(keybindToShortcutString({ key: "ArrowUp" })).toBe("Up");
    expect(keybindToShortcutString({ key: "ArrowDown" })).toBe("Down");
  });

  it("returns null for modifier-only bindings (Tauri rejects them)", () => {
    for (const k of ["Control", "Shift", "Alt", "Meta", ""]) {
      expect(keybindToShortcutString({ key: k })).toBeNull();
    }
  });

  it("upper-cases single-character keys", () => {
    expect(keybindToShortcutString({ key: "a", ctrl: true })).toBe("Ctrl+A");
  });
});

describe("matchesBinding", () => {
  it("matches a Ctrl-modified letter regardless of letter case", () => {
    const b: Keybind = { key: "o", ctrl: true };
    expect(matchesBinding(ke("o", { ctrl: true }), b)).toBe(true);
    expect(matchesBinding(ke("O", { ctrl: true }), b)).toBe(true);
  });

  it("treats Cmd (metaKey) as equivalent to Ctrl", () => {
    const b: Keybind = { key: "e", ctrl: true };
    expect(matchesBinding(ke("e", { meta: true }), b)).toBe(true);
  });

  it("rejects when a required modifier is missing", () => {
    const b: Keybind = { key: "s", shift: true };
    expect(matchesBinding(ke("S", { shift: true }), b)).toBe(true);
    expect(matchesBinding(ke("s", { shift: false }), b)).toBe(false);
  });

  it("rejects when an unexpected modifier is held", () => {
    const b: Keybind = { key: "l" };
    expect(matchesBinding(ke("l", { ctrl: true }), b)).toBe(false);
    expect(matchesBinding(ke("l", { alt: true }), b)).toBe(false);
  });

  it("matches multi-char keys by exact string", () => {
    const b: Keybind = { key: "Home" };
    expect(matchesBinding(ke("Home"), b)).toBe(true);
    expect(matchesBinding(ke("End"), b)).toBe(false);
  });
});

describe("formatKeybind", () => {
  it("renders arrow keys as glyphs (for hint text)", () => {
    expect(formatKeybind({ key: "ArrowLeft" })).toBe("←");
    expect(formatKeybind({ key: "ArrowUp", ctrl: true })).toBe("Ctrl+↑");
  });

  it("renders Space as the word 'Space'", () => {
    expect(formatKeybind({ key: " " })).toBe("Space");
  });

  it("upper-cases single-char keys", () => {
    expect(formatKeybind({ key: "i" })).toBe("I");
  });
});

describe("captureKeybind", () => {
  it("returns null for bare modifier keys", () => {
    expect(captureKeybind(ke("Control"))).toBeNull();
    expect(captureKeybind(ke("Shift"))).toBeNull();
    expect(captureKeybind(ke("Alt"))).toBeNull();
    expect(captureKeybind(ke("Meta"))).toBeNull();
  });

  it("returns null for unmodified Escape (used to cancel listening)", () => {
    expect(captureKeybind(ke("Escape"))).toBeNull();
  });

  it("captures Escape only when modified (otherwise reserved for cancel)", () => {
    const b = captureKeybind(ke("Escape", { ctrl: true }));
    expect(b).toEqual({ key: "Escape", ctrl: true });
  });

  it("captures a letter with modifiers, lowercasing single chars", () => {
    expect(captureKeybind(ke("O", { ctrl: true }))).toEqual({
      key: "o",
      ctrl: true,
    });
  });

  it("preserves multi-char keys verbatim", () => {
    expect(captureKeybind(ke("Home", { shift: true }))).toEqual({
      key: "Home",
      shift: true,
    });
  });
});

describe("round-trip keybindToShortcutString ↔ matchesBinding", () => {
  // The same Keybind object drives the OS-level shortcut and the in-app
  // dispatch. Both consumers must agree on what "the user pressed it" means.
  const cases: Array<[string, Keybind, KeyboardEvent]> = [
    ["Alt+F10", { key: "F10", alt: true }, ke("F10", { alt: true })],
    ["Ctrl+O", { key: "o", ctrl: true }, ke("O", { ctrl: true })],
    ["Ctrl+Shift+S", { key: "s", ctrl: true, shift: true }, ke("S", { ctrl: true, shift: true })],
    ["Home", { key: "Home" }, ke("Home")],
  ];
  for (const [label, bind, event] of cases) {
    it(label, () => {
      expect(keybindToShortcutString(bind)).toBe(label);
      expect(matchesBinding(event, bind)).toBe(true);
    });
  }
});
