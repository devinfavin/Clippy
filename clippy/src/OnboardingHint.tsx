import { useEffect, useState } from "react";

const SEEN_KEY = "clippy.onboarded.v1";

/**
 * One-shot coachmark shown on first launch (per machine, via localStorage).
 * Points at the topbar's "?" button as the way to discover the deeper
 * features — colored dots → recolor, click track names → rename, etc.
 *
 * Dismissible by clicking "Got it" or anywhere outside the bubble. Once
 * dismissed it never shows again. Resetting requires deleting the
 * localStorage key (or clearing site data).
 */
export function OnboardingHint() {
  // Don't render at all if the user has seen it. Stays out of the tree.
  const [visible, setVisible] = useState(() => {
    try {
      return localStorage.getItem(SEEN_KEY) !== "1";
    } catch {
      return false;
    }
  });

  // Animate-in slightly delayed so the user sees the app load first, then
  // the coachmark appears — feels like an introduction rather than a popup
  // ambushing them.
  const [shown, setShown] = useState(false);
  useEffect(() => {
    if (!visible) return;
    const t = window.setTimeout(() => setShown(true), 600);
    return () => window.clearTimeout(t);
  }, [visible]);

  if (!visible) return null;

  const dismiss = () => {
    try {
      localStorage.setItem(SEEN_KEY, "1");
    } catch {}
    setVisible(false);
  };

  return (
    <div
      className={`onboarding-overlay${shown ? " shown" : ""}`}
      onClick={dismiss}
    >
      <div
        className="onboarding-bubble"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="onboarding-arrow" aria-hidden />
        <div className="onboarding-title">Welcome to Clippy</div>
        <div className="onboarding-body">
          A focused clip editing tool.
          A few things you'd probably miss otherwise:
          <ul>
            <li>
              <b>Drag a video</b> onto the window to open it — or use{" "}
              <kbd>Ctrl+O</kbd>.
            </li>
            <li>
              On region chips, the <b>colored dot is clickable</b> — pick
              from a cool palette.
            </li>
            <li>
              In the audio mixer, <b>click track names</b> to rename them
              and <b>click the colored dots</b> to recolor.
            </li>
            <li>
              Hit the <b>?</b> in the topbar any time for the full tips list.
            </li>
          </ul>
        </div>
        <div className="onboarding-footer">
          <button className="primary" onClick={dismiss}>
            Got it
          </button>
        </div>
      </div>
    </div>
  );
}
