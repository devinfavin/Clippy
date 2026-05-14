import { useCallback, useEffect, useRef, useState } from "react";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { logErr } from "./logErr";

/**
 * Auto-updater state machine. Drives the About-tab UI and the on-launch
 * silent check.
 *
 *   idle      — never checked this session
 *   checking  — RPC out to the manifest endpoint
 *   uptodate  — manifest fetched, version matches
 *   available — newer version found; user can review notes and install
 *   downloading — installer is streaming down
 *   installing  — installer is running (Windows MSI/NSIS handoff)
 *   error     — anything went wrong; `message` carries detail
 *
 * Network errors (offline, GitHub Release not yet published, DNS) all land
 * in `error` and are silent on the launch check — they only surface in the
 * About tab when the user explicitly clicks Check.
 */
export type UpdateState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "uptodate"; checkedAt: number }
  | { kind: "available"; version: string; notes: string | null; update: Update }
  | { kind: "downloading"; version: string; downloaded: number; total: number | null }
  | { kind: "installing"; version: string }
  | { kind: "error"; message: string };

export function useUpdater() {
  const [state, setState] = useState<UpdateState>({ kind: "idle" });
  // Keep the latest Update handle around so the install action can find it
  // even after we've transitioned state forward.
  const updateRef = useRef<Update | null>(null);

  const checkNow = useCallback(async (opts?: { silent?: boolean }) => {
    if (!opts?.silent) setState({ kind: "checking" });
    try {
      const update = await check();
      if (!update) {
        setState({ kind: "uptodate", checkedAt: Date.now() });
        return;
      }
      updateRef.current = update;
      setState({
        kind: "available",
        version: update.version,
        notes: update.body ?? null,
        update,
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      if (opts?.silent) {
        // Silent check (on launch) — log but don't surface a banner if the
        // manifest 404s or the network is unavailable; we don't want the
        // app yelling at the user on every launch when offline.
        logErr("updater silent check", msg);
        return;
      }
      setState({ kind: "error", message: msg });
    }
  }, []);

  const installNow = useCallback(async () => {
    const update = updateRef.current;
    if (!update) return;
    setState({
      kind: "downloading",
      version: update.version,
      downloaded: 0,
      total: null,
    });
    try {
      let downloaded = 0;
      let total: number | null = null;
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? null;
          setState({ kind: "downloading", version: update.version, downloaded: 0, total });
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          setState({ kind: "downloading", version: update.version, downloaded, total });
        } else if (event.event === "Finished") {
          setState({ kind: "installing", version: update.version });
        }
      });
      // On Windows the installer handoff exits this process; relaunch is
      // a no-op if it didn't. Wrapped in try/catch so a relaunch failure
      // doesn't crash the UI.
      try { await relaunch(); } catch {}
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setState({ kind: "error", message: msg });
    }
  }, []);

  // One silent check shortly after launch. Delayed so it doesn't compete
  // with the heavier startup work (probing, replay buffer init, etc.).
  useEffect(() => {
    const timer = window.setTimeout(() => {
      void checkNow({ silent: true });
    }, 4000);
    return () => window.clearTimeout(timer);
  }, [checkNow]);

  return { state, checkNow, installNow };
}
