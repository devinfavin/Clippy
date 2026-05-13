import { useEffect } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";

const VIDEO_EXTS = ["mkv", "mp4", "mov", "webm", "m4v", "avi"];
const isVideoPath = (p: string) =>
  VIDEO_EXTS.includes(p.split(".").pop()?.toLowerCase() ?? "");

/// Window-level drag-and-drop: drop a video file on the window to open it.
export function useDragDropFile(
  loadFile: (path: string) => Promise<void>,
  setIsDraggingFile: React.Dispatch<React.SetStateAction<boolean>>,
): void {
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    import("@tauri-apps/api/webview").then(({ getCurrentWebview }) => {
      getCurrentWebview()
        .onDragDropEvent((event) => {
          const p = event.payload as { type: string; paths?: string[] };
          if (p.type === "enter" || p.type === "over") {
            const hasVideo = (p.paths ?? []).some(isVideoPath);
            setIsDraggingFile(hasVideo);
          } else if (p.type === "drop") {
            setIsDraggingFile(false);
            const first = (p.paths ?? []).find(isVideoPath);
            if (first) loadFile(first);
          } else {
            setIsDraggingFile(false);
          }
        })
        .then((u) => (unlisten = u));
    });
    return () => {
      unlisten?.();
    };
  }, [loadFile, setIsDraggingFile]);
}
