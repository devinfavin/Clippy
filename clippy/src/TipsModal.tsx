import { useEffect } from "react";
import { formatKeybind, type Keybinds } from "./keybinds";

/**
 * In-app discoverability modal — surfaces the non-obvious tricks the UI
 * doesn't otherwise advertise. Triggered by the `?` button in the topbar.
 *
 * One card per category. Body capped at 2-4 short lines each — if a tip
 * needs more, the medium is wrong (room for a future gif/video card).
 * The full keybind list lives in the Keyboard tab of Settings.
 *
 * Every <Kbd action="..."> below resolves to whatever the user currently
 * has bound to that action, so the modal never lies about a default the
 * user has rebound.
 */
export function TipsModal(props: {
  keybinds: Keybinds;
  onClose: () => void;
  /** Open Settings → Keyboard from the footer link. The link previously
   *  read as instructive text only; making it actionable closes the loop
   *  for users who want to rebind without hunting for the gear. */
  onOpenKeyboardSettings: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        props.onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [props]);

  const k = props.keybinds;
  const region1to9 = `${formatKeybind(k.jumpRegion1)}–${formatKeybind(k.jumpRegion9)}`;

  return (
    <div
      className="modal-overlay"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) props.onClose();
      }}
    >
      <div className="modal tips-modal">
        <header className="modal-header">
          <h2>Tips</h2>
          <button className="modal-close" onClick={props.onClose}>
            ×
          </button>
        </header>
        <div className="tips-body">
          <div className="tips-grid">
            <TipCard title="Playback">
              <kbd className="tip-kbd">{formatKeybind(k.playPause)}</kbd> plays / pauses;{" "}
              <kbd className="tip-kbd">{formatKeybind(k.frameBack)}</kbd>{" "}
              <kbd className="tip-kbd">{formatKeybind(k.frameForward)}</kbd>{" "}
              step one frame.{" "}
              <kbd className="tip-kbd">{region1to9}</kbd>{" "}
              jumps to that region.
            </TipCard>

            <TipCard title="Regions">
              <kbd className="tip-kbd">{formatKeybind(k.setIn)}</kbd> /{" "}
              <kbd className="tip-kbd">{formatKeybind(k.setOut)}</kbd>{" "}
              sets in / out. Drag a region's edges on the timeline to fine-tune;
              new regions inherit the last crop, speed, and audio mix.
            </TipCard>

            <TipCard title="Audio mix">
              Click a track's name to rename it; click the colored dot to recolor.
              Inside a region you're editing that region's mix — outside, the
              source default.
            </TipCard>

            <TipCard title="Export">
              <kbd className="tip-kbd">{formatKeybind(k.export)}</kbd> opens the export dialog (MP4 / MP3 / GIF).
              Toggle <b>Normalize loudness</b> to boost quiet game audio to a
              Discord-friendly level without touching the mix.
            </TipCard>

            <TipCard title="Save a frame">
              <kbd className="tip-kbd">{formatKeybind(k.saveFrame)}</kbd> copies the current frame to
              the clipboard. Press again within 5 s to also save it as a PNG.
            </TipCard>

            <TipCard title="Project state">
              Regions, crops, speeds, audio mix, and track renames all persist
              per source. Reopen the same file later and your work is back.
            </TipCard>
          </div>
        </div>
        <footer className="modal-footer tips-footer">
          <button
            type="button"
            className="tips-open-settings"
            onClick={() => { props.onOpenKeyboardSettings(); props.onClose(); }}
            title="Open Settings → Keyboard"
          >
            Open Settings → Keyboard for the full list and rebindings.
          </button>
          <button className="btn primary" onClick={props.onClose}>
            Got it
          </button>
        </footer>
      </div>
    </div>
  );
}

function TipCard(props: { title: string; children: React.ReactNode }) {
  return (
    <section className="tip-card">
      <h3 className="tip-card-title">{props.title}</h3>
      <p className="tip-card-body">{props.children}</p>
    </section>
  );
}
