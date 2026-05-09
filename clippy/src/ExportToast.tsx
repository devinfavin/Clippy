import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { fmtMb } from "./formatters";

export function ExportToast(props: { paths: string[]; onClose: () => void }) {
  const [sizes, setSizes] = useState<Record<string, number>>({});
  const [copiedPath, setCopiedPath] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    Promise.all(
      props.paths.map(async (p) => {
        try {
          const sz = await invoke<number>("file_size", { path: p });
          return [p, sz] as const;
        } catch {
          return [p, 0] as const;
        }
      })
    ).then((entries) => {
      if (alive) setSizes(Object.fromEntries(entries));
    });
    return () => { alive = false; };
  }, [props.paths]);

  const reveal = async (path: string) => {
    try {
      await invoke("reveal_in_folder", { path });
    } catch (err) {
      console.error("[clippy] reveal failed:", err);
    }
  };

  const copyPath = async (path: string) => {
    try {
      await navigator.clipboard.writeText(path);
      setCopiedPath(path);
      setTimeout(() => setCopiedPath((p) => (p === path ? null : p)), 1200);
    } catch (err) {
      console.error("[clippy] copy failed:", err);
    }
  };

  return (
    <div className="export-toast">
      <header className="toast-header">
        <h3>Export complete · {props.paths.length} file{props.paths.length === 1 ? "" : "s"}</h3>
        <button className="modal-close" onClick={props.onClose}>×</button>
      </header>
      <div className="toast-files">
        {props.paths.map((p) => {
          const name = p.split(/[\\/]/).pop() ?? p;
          return (
            <div key={p} className="toast-file">
              <div className="toast-file-info">
                <div className="toast-file-name mono" title={p}>{name}</div>
                <div className="toast-file-meta dim mono">
                  {sizes[p] != null ? fmtMb(sizes[p]) : "…"}
                </div>
              </div>
              <div className="toast-actions">
                <button onClick={() => reveal(p)} title="Open the containing folder in Explorer">
                  Open folder
                </button>
                <button onClick={() => copyPath(p)} title="Copy the file path to clipboard">
                  {copiedPath === p ? "Copied!" : "Copy path"}
                </button>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
