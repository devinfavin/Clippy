import { useEffect } from "react";

/**
 * In-app discoverability modal — surfaces the non-obvious tricks the UI
 * doesn't otherwise advertise. Triggered by the `?` button in the topbar.
 *
 * Intentionally short. The full keybind list lives in the keybinds editor
 * (Ctrl+K via the footer "edit shortcuts…" link); this is a curated set of
 * "things you probably didn't know you could do".
 */
export function TipsModal(props: { onClose: () => void }) {
  // Esc closes.
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
          <Section title="Quickly">
            <Tip kbd="Space">Play / pause</Tip>
            <Tip kbd="F">Set in-point at the playhead</Tip>
            <Tip kbd="Shift+F">Set out-point at the playhead</Tip>
            <Tip kbd="Ctrl+E">Open the Export dialog</Tip>
          </Section>

          <Section title="Save a frame">
            <Tip kbd="Shift+S">
              First press copies the current frame to the clipboard. Press
              again within 5 s to save it as a PNG instead.
            </Tip>
          </Section>

          <Section title="Region tricks">
            <Tip>
              <b>Click the colored dot</b> on a region chip to recolor it.
              The change applies everywhere — chip, timeline band, looping
              indicator.
            </Tip>
            <Tip>
              <b>Hover a region chip</b> to reveal the action tray (crop
              edit, speed, loop, delete).
            </Tip>
            <Tip kbd="Shift+C">
              Open the crop overlay for the region currently under the playhead.
            </Tip>
            <Tip>
              Drag the <b>edges of a region band</b> on the timeline to
              fine-tune its in/out points after the fact.
            </Tip>
            <Tip>
              New regions <b>inherit the most recent crop, speed, and audio
              mix</b> — so once you've set one, the rest pick them up.
            </Tip>
          </Section>

          <Section title="Audio mix">
            <Tip>
              <b>Click a track's name</b> in the mixer to rename it. Useful
              when source metadata gives you "Track 2" instead of "Discord".
            </Tip>
            <Tip>
              <b>Click the colored dot</b> on a track row to recolor it.
              Same color appears on the row's stripe, the slider thumb, and
              the track's waveform layer above.
            </Tip>
            <Tip>
              The mixer auto-follows the playhead. Inside a region, you're
              editing <i>that region's</i> audio mix. Outside any region,
              you're editing the source default.
            </Tip>
          </Section>

          <Section title="Export">
            <Tip>
              MP4 / MP3 / GIF in the same dialog. GIF resolution preset goes
              up to source-size if you really need it.
            </Tip>
            <Tip>
              <b>Normalize loudness</b> boosts quiet game audio to a
              Discord-friendly level (~-16 LUFS) without you needing to
              touch the mix.
            </Tip>
            <Tip>
              Right after an export, the status strip shows{" "}
              <b>Open folder</b> + <b>Copy path</b>.
            </Tip>
          </Section>

          <Section title="Project state">
            <Tip>
              Regions, crops, speeds, audio mix, track colors, and renames
              all persist per source. Reopen the same file later and your
              work is back.
            </Tip>
            <Tip>
              Cache lives in <code className="mono">%APPDATA%\com.devin.clippy\proxies\</code>
              {" "}and auto-prunes after 30 days. Clear it manually from the
              keybind editor's footer if you want.
            </Tip>
          </Section>
        </div>
        <footer className="modal-footer tips-footer">
          <span className="dim">Want the full keybind list? Open the keybind editor.</span>
          <button className="primary" onClick={props.onClose}>
            Got it
          </button>
        </footer>
      </div>
    </div>
  );
}

function Section(props: { title: string; children: React.ReactNode }) {
  return (
    <section className="tips-section">
      <h3 className="section-title">{props.title}</h3>
      <div className="tips-list">{props.children}</div>
    </section>
  );
}

function Tip(props: { kbd?: string; children: React.ReactNode }) {
  return (
    <div className="tip-row">
      {props.kbd && <kbd className="tip-kbd">{props.kbd}</kbd>}
      <span className="tip-body">{props.children}</span>
    </div>
  );
}
