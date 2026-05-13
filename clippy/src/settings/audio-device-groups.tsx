import { useMemo, useState } from "react";
import type { AudioDevice } from "./replay-types";

type DeviceGroup = "physical" | "monitor" | "virtual";

/**
 * Classify a WASAPI render device by name. Heuristic — substring matching on
 * known patterns. Anything that doesn't look virtual or monitor falls into
 * "physical" so the user always sees their device somewhere.
 *
 * - Virtual: SteelSeries Sonar, Voicemeeter, VB-Audio, OBS Virtual, any
 *   "Virtual Audio Device" suffix Windows attaches to vDevs.
 * - Monitor: leading "N - " prefix Windows assigns to multi-sink GPU drivers,
 *   or explicit HDMI / DisplayPort audio labels.
 * - Physical: everything else (Speakers, Microphone, Realtek, Focusrite, …).
 */
function classifyDevice(name: string): DeviceGroup {
  const lower = name.toLowerCase();
  if (
    lower.includes("sonar") ||
    lower.includes("voicemeeter") ||
    lower.includes("vb-audio") ||
    lower.includes("vb cable") ||
    lower.includes("virtual audio") ||
    lower.includes("obs virtual")
  ) {
    return "virtual";
  }
  // "1 - ASUS VG32VQ1B" style — leading digit + dash is how Windows
  // distinguishes multi-monitor audio outputs from one GPU driver.
  if (/^\d+\s*-\s*/.test(name) || lower.includes("hdmi audio") || lower.includes("displayport")) {
    return "monitor";
  }
  return "physical";
}

const GROUP_META: Record<DeviceGroup, { label: string; hint: string }> = {
  physical: { label: "Physical devices", hint: "Speakers, headphones, microphones, USB audio interfaces." },
  monitor:  { label: "Monitor audio outputs", hint: "HDMI / DisplayPort audio attached to displays." },
  virtual:  { label: "Virtual devices", hint: "Sonar, Voicemeeter, OBS Virtual Audio, and similar routers." },
};

export function AudioDeviceGroups(props: {
  devices: AudioDevice[];
  selectedIds: Set<string>;
  audioNames: Record<string, string>;
  isRunning: boolean;
  onToggle: (id: string) => void;
  onRename: (id: string, name: string) => void;
}) {
  // Bucket by classification, preserving the original device order within
  // each group (Windows surfaces them in default-first order).
  const grouped = useMemo(() => {
    const buckets: Record<DeviceGroup, AudioDevice[]> = {
      physical: [],
      monitor: [],
      virtual: [],
    };
    for (const d of props.devices) {
      buckets[classifyDevice(d.name)].push(d);
    }
    return buckets;
  }, [props.devices]);

  // Default-expand rule: any group containing a currently-selected device
  // opens; first-run (nothing selected) keeps all groups collapsed so the
  // user isn't faced with a wall of 12 devices.
  const initialOpen = useMemo(() => {
    const open: Record<DeviceGroup, boolean> = {
      physical: false,
      monitor: false,
      virtual: false,
    };
    for (const d of props.devices) {
      if (props.selectedIds.has(d.id)) {
        open[classifyDevice(d.name)] = true;
      }
    }
    return open;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []); // computed once on mount; user toggles take over after that
  const [openGroups, setOpenGroups] = useState(initialOpen);

  const groupOrder: DeviceGroup[] = ["physical", "monitor", "virtual"];

  return (
    <div className="settings-audio-groups">
      {groupOrder.map((key) => {
        const items = grouped[key];
        if (items.length === 0) return null;
        const selectedCount = items.filter((d) => props.selectedIds.has(d.id)).length;
        const isOpen = openGroups[key];
        return (
          <div key={key} className={`settings-audio-group${isOpen ? " is-open" : ""}`}>
            <button
              className="settings-audio-group-head"
              onClick={() => setOpenGroups((g) => ({ ...g, [key]: !g[key] }))}
              aria-expanded={isOpen}
              type="button"
            >
              <span className="settings-audio-group-arrow" aria-hidden>
                {isOpen ? "▾" : "▸"}
              </span>
              <span className="settings-audio-group-label">{GROUP_META[key].label}</span>
              <span className="settings-audio-group-meta">
                {selectedCount > 0 ? (
                  <span className="settings-audio-group-count is-selected">
                    {selectedCount} selected
                  </span>
                ) : (
                  <span className="settings-audio-group-count">
                    {items.length} {items.length === 1 ? "device" : "devices"}
                  </span>
                )}
              </span>
            </button>
            {isOpen && (
              <div className="settings-audio-group-body">
                <p className="settings-audio-group-hint">{GROUP_META[key].hint}</p>
                {items.map((d) => {
                  const checked = props.selectedIds.has(d.id);
                  return (
                    <div key={d.id} className="settings-audio-row">
                      <label className="settings-checkbox">
                        <input
                          type="checkbox"
                          checked={checked}
                          onChange={() => props.onToggle(d.id)}
                          disabled={props.isRunning}
                        />
                        <span className="settings-audio-system" title={d.name}>{d.name}</span>
                      </label>
                      {checked && (
                        <span className="settings-audio-rename">
                          <span className="settings-audio-rename-prefix">Called</span>
                          <input
                            type="text"
                            className="settings-audio-name"
                            placeholder="e.g. Game"
                            value={props.audioNames[d.id] ?? ""}
                            onChange={(e) => props.onRename(d.id, e.target.value)}
                            disabled={props.isRunning}
                            aria-label={`Track name for ${d.name}`}
                          />
                        </span>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
