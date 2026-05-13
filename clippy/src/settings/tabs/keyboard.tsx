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

/** Keyboard-shortcuts tab — two-column grid (action + description on the
 *  left, key combo button on the right), grouped by category. Region 1-9
 *  jumps are permanently bound to the digit keys and shown as a read-only
 *  display under the "Regions" group; everything else is rebindable by
 *  clicking its combo. */
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
    // Region 1-9 collapse into a single row appended at the end of "regions"
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
      <div key={action} className={`kb-row${conflicts.length ? " has-conflict" : ""}`}>
        <span className="kb-row-action">
          <span className="kb-row-action-name">
            {ACTION_LABELS[action]}
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
          <span className="kb-row-action-desc">{ACTION_DESCRIPTIONS[action]}</span>
        </span>
        <button
          className={`kb-binding${isListening ? " listening" : ""}`}
          onClick={() => setListeningAction(action)}
        >
          {isListening ? "Press a key…  (Esc to cancel)" : formatKeybind(keybinds[action])}
        </button>
      </div>
    );
  };

  return (
    <section className="settings-section">
      <header className="settings-tab-header">
        <h3 className="settings-tab-title">Keyboard shortcuts</h3>
        <p className="settings-tab-blurb">
          Click any binding to record a new key combo. Globals (tagged below) fire
          even when Clippy is unfocused — they don't conflict with in-app bindings.
        </p>
      </header>
      <div className="kb-grid">
        {groupOrder.map((group) => {
          const rows = grouped[group];
          if (rows.length === 0 && group !== "regions") return null;
          return (
            <div key={group} className="kb-group">
              <p className="kb-group-label">{ACTION_GROUP_LABELS[group]}</p>
              {rows.map(renderRow)}
              {group === "regions" && (
                <div className="kb-row">
                  <span className="kb-row-action">
                    <span className="kb-row-action-name">Jump to region 1–9</span>
                    <span className="kb-row-action-desc">
                      Jump the playhead to a specific region by its index.
                    </span>
                  </span>
                  {/* Region 1-9 jumps are permanently bound to the digit
                      keys. Used to be individually rebindable, but the 9-
                      button grid was visually awkward AND nobody actually
                      remaps them — the digits are the natural choice. The
                      digit-row display is read-only on purpose. */}
                  <span className="kb-jump-region-display" aria-label="Region jump key bindings">
                    1&nbsp;2&nbsp;3&nbsp;4&nbsp;5&nbsp;6&nbsp;7&nbsp;8&nbsp;9
                  </span>
                </div>
              )}
            </div>
          );
        })}
      </div>
    </section>
  );
}
