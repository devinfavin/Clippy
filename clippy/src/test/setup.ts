// Vitest setup — runs once before every test file. Two responsibilities:
//   1. wire up @testing-library/jest-dom matchers
//   2. install a Tauri `invoke` mock that returns programmable responses
//      so component tests don't have to hand-mock every command per-file
//
// Per-test customisation: `setTauriInvokeHandler((cmd, args) => …)` from
// any test file overrides the default (no-op resolving with `undefined`).

import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, vi } from "vitest";
import { cleanup } from "@testing-library/react";

/** Programmable per-test invoke handler. */
type InvokeHandler = (cmd: string, args?: unknown) => unknown;

let currentHandler: InvokeHandler = () => undefined;

export function setTauriInvokeHandler(h: InvokeHandler) {
  currentHandler = h;
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: unknown) => {
    return currentHandler(cmd, args);
  }),
}));

// Real plugin entry points the components import on mount — replace with
// no-op shims so test renders don't error trying to call into a non-existent
// Tauri runtime.
vi.mock("@tauri-apps/plugin-autostart", () => ({
  enable: vi.fn(async () => {}),
  disable: vi.fn(async () => {}),
  isEnabled: vi.fn(async () => false),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => null),
}));

beforeEach(() => {
  // Reset state every test so localStorage + invoke handler don't leak.
  localStorage.clear();
  currentHandler = () => undefined;
});

afterEach(() => {
  cleanup();
});
