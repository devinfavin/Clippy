import { invoke } from "@tauri-apps/api/core";

/**
 * Bridge a frontend error / warning into the backend diag log so it shows up
 * alongside replay/save/export entries when the user copies their diagnostics.
 * Without this, `console.error` calls only land in DevTools (which the user
 * can't open in production), making renderer-side IPC failures invisible.
 *
 * Best-effort: if the bridge IPC itself fails (e.g. backend not ready yet
 * during very early boot), swallow the error rather than letting log
 * plumbing crash the UI.
 */
export function logErr(scope: string, err: unknown): void {
  const msg = err instanceof Error ? err.message : String(err);
  // Keep the console.error too so dev builds still see structured stack
  // traces in DevTools — `frontend_diag` only gets the message string.
  // eslint-disable-next-line no-console
  console.error(`[clippy] ${scope}:`, err);
  invoke("frontend_diag", { msg: `[${scope}] ${msg}` }).catch(() => {
    // Bridge failed — nothing to do; we already logged to console above.
  });
}
