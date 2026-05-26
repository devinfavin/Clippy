import {
  ACTION_DESCRIPTIONS,
  ACTION_GROUP_LABELS,
  ACTION_GROUPS,
  ACTION_LABELS,
  formatKeybind,
  GLOBAL_ACTIONS,
  type ActionGroup,
  type ActionId,
  type Keybinds,
} from "../../keybinds";
import { SettingsGroup, SettingsLabel, SettingsRow } from "../primitives";

/** Keyboard-shortcuts tab — grouped by category, each row uses the shared
 *  settings primitives. Click a binding to record a new key combo. Region
 *  1-9 jumps stay read-only (the digit keys are the natural choice). */
export function KeyboardSettingsTab(props: {
  keybinds: Keybinds;
  listeningAction: ActionId | null;
  setListeningAction: (a: ActionId | null) => void;
}) {
  const { keybinds, listeningAction, setListeningAction } = props;
  const isJumpRegion = (a: ActionId) => a.startsWith("jumpRegion");

  const groupOrder: ActionGroup[] = ["playback", "selection", "regions", "capture", "exports"];
  const grouped: Record<ActionGroup, ActionId[]> = {
    playback: [], selection: [], regions: [], capture: [], exports: [],
  };
  for (const action of Object.keys(ACTION_LABELS) as ActionId[]) {
    if (isJumpRegion(action)) continue;
    grouped[ACTION_GROUPS[action]].push(action);
  }

  const renderRow = (action: ActionId) => {
    const isListening = listeningAction === action;
    const isGlobal = GLOBAL_ACTIONS.has(action);
    const conflicts = isGlobal
      ? []
      : (Object.keys(keybinds) as ActionId[]).filter(
          (other) =>
            other !== action &&
            !GLOBAL_ACTIONS.has(other) &&
            formatKeybind(keybinds[other]) === formatKeybind(keybinds[action])
        );
    return (
      <SettingsRow
        key={action}
        title={
          <span className="kb-title-row">
            <span>{ACTION_LABELS[action]}</span>
            {isGlobal && (
              <span className="kb-tag-global" title="Fires globally — works while Clippy is not focused">
                Global
              </span>
            )}
            {conflicts.length > 0 && (
              <span className="kb-conflict" title={`Conflicts with: ${conflicts.map((c) => ACTION_LABELS[c]).join(", ")}`}>
                conflict
              </span>
            )}
          </span>
        }
        subtitle={ACTION_DESCRIPTIONS[action]}
      >
        <button
          className={`kb-binding${isListening ? " listening" : ""}`}
          onClick={() => setListeningAction(action)}
        >
          {isListening ? "Press a key…  (Esc to cancel)" : formatKeybind(keybinds[action])}
        </button>
      </SettingsRow>
    );
  };

  return (
    <section className="settings-tab-pane">
      <header>
        <h3 className="settings-tab-pane-title">Keyboard shortcuts</h3>
        <p className="settings-tab-pane-blurb">
          Click any binding to record a new key combo. Globals (tagged below) fire
          even when Clippy is unfocused — they don't conflict with in-app bindings.
        </p>
      </header>

      {groupOrder.map((group) => {
        const rows = grouped[group];
        if (rows.length === 0 && group !== "regions") return null;
        return (
          <div key={group}>
            <SettingsLabel>{ACTION_GROUP_LABELS[group]}</SettingsLabel>
            <SettingsGroup>
              {rows.map(renderRow)}
              {group === "regions" && (
                <SettingsRow
                  title="Jump to region 1–9"
                  subtitle="Jump the playhead to a specific region by its index."
                >
                  {/* Region 1-9 jumps are permanently bound to the digit keys —
                      the 9-button grid was visually awkward AND nobody actually
                      remaps them. Read-only display by design. */}
                  <span className="kb-jump-region-display" aria-label="Region jump key bindings">
                    1&nbsp;2&nbsp;3&nbsp;4&nbsp;5&nbsp;6&nbsp;7&nbsp;8&nbsp;9
                  </span>
                </SettingsRow>
              )}
            </SettingsGroup>
          </div>
        );
      })}
    </section>
  );
}
