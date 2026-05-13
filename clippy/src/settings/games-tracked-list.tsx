import { useMemo, useState } from "react";

/** Returns true when `path` looks like a Steam install. Steam games live
 *  under `…\steamapps\common\<game>\…` regardless of which library drive. */
function isSteamPath(path: string): boolean {
  return /[\\/]steamapps[\\/]common[\\/]/i.test(path);
}

function baseExe(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

export function GamesTrackedList(props: {
  games: string[];
  recentGames: string[];
  search: string;
  onRemove: (path: string) => void;
}) {
  const q = props.search.toLowerCase().trim();
  const matches = (p: string) => !q || p.toLowerCase().includes(q);

  // Recently added: render the recents in order (most-recent-first), but
  // filter by the live games list so a deleted entry doesn't dangle.
  const present = useMemo(() => new Set(props.games.map((g) => g.toLowerCase())), [props.games]);
  const recentVisible = props.recentGames.filter(
    (p) => present.has(p.toLowerCase()) && matches(p)
  );

  const steam: string[] = [];
  const manual: string[] = [];
  for (const g of props.games) {
    if (!matches(g)) continue;
    if (isSteamPath(g)) steam.push(g);
    else manual.push(g);
  }

  // Default-expand behavior: while searching, every non-empty group opens
  // so matches are visible. Outside search, groups stay collapsed until
  // user clicks.
  const isSearching = q.length > 0;
  const [steamOpen, setSteamOpen] = useState(false);
  const [manualOpen, setManualOpen] = useState(false);
  const steamEffectiveOpen = isSearching ? steam.length > 0 : steamOpen;
  const manualEffectiveOpen = isSearching ? manual.length > 0 : manualOpen;

  const totalShown = recentVisible.length + steam.length + manual.length;
  if (totalShown === 0) {
    return (
      <p className="settings-section-blurb">
        {props.games.length === 0
          ? "No games detected. Click Rescan or add one manually."
          : "No matches."}
      </p>
    );
  }

  return (
    <div className="settings-games-groups">
      {/* Recently added — always visible at the top, no collapsing. */}
      {recentVisible.length > 0 && (
        <div className="settings-games-group">
          <div className="settings-games-group-head settings-games-group-head-static">
            <span className="settings-games-group-label">Recently added</span>
            <span className="settings-audio-group-count">
              {recentVisible.length} {recentVisible.length === 1 ? "game" : "games"}
            </span>
          </div>
          <div className="settings-games-group-body">
            <ul className="settings-game-list">
              {recentVisible.map((path) => (
                <GameRow key={`recent-${path}`} path={path} onRemove={props.onRemove} />
              ))}
            </ul>
          </div>
        </div>
      )}

      {/* Steam */}
      {steam.length > 0 && (
        <div className={`settings-games-group${steamEffectiveOpen ? " is-open" : ""}`}>
          <button
            className="settings-games-group-head"
            onClick={() => setSteamOpen((v) => !v)}
            aria-expanded={steamEffectiveOpen}
            type="button"
          >
            <span className="settings-audio-group-arrow" aria-hidden>
              {steamEffectiveOpen ? "▾" : "▸"}
            </span>
            <span className="settings-games-group-label">Steam</span>
            <span className="settings-audio-group-count">{steam.length}</span>
          </button>
          {steamEffectiveOpen && (
            <div className="settings-games-group-body">
              <ul className="settings-game-list">
                {steam.map((path) => (
                  <GameRow key={path} path={path} onRemove={props.onRemove} />
                ))}
              </ul>
            </div>
          )}
        </div>
      )}

      {/* Manual */}
      {manual.length > 0 && (
        <div className={`settings-games-group${manualEffectiveOpen ? " is-open" : ""}`}>
          <button
            className="settings-games-group-head"
            onClick={() => setManualOpen((v) => !v)}
            aria-expanded={manualEffectiveOpen}
            type="button"
          >
            <span className="settings-audio-group-arrow" aria-hidden>
              {manualEffectiveOpen ? "▾" : "▸"}
            </span>
            <span className="settings-games-group-label">Manual</span>
            <span className="settings-audio-group-count">{manual.length}</span>
          </button>
          {manualEffectiveOpen && (
            <div className="settings-games-group-body">
              <ul className="settings-game-list">
                {manual.map((path) => (
                  <GameRow key={path} path={path} onRemove={props.onRemove} />
                ))}
              </ul>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function GameRow(props: { path: string; onRemove: (p: string) => void }) {
  return (
    <li className="settings-game-row" title={props.path}>
      <span className="settings-game-name mono">{baseExe(props.path)}</span>
      <span className="settings-game-path">{props.path}</span>
      <button
        className="settings-row-remove"
        onClick={() => props.onRemove(props.path)}
        title="Remove from allowlist"
        aria-label="Remove"
      >
        ×
      </button>
    </li>
  );
}
