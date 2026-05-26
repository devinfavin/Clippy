import React from "react";
import ReactDOM from "react-dom/client";
// Bundled fonts — never fetched at runtime. Latin only; Cyrillic subset
// stays out of the bundle since the UI is English-only today.
import "@fontsource/geist/latin-400.css";
import "@fontsource/geist/latin-500.css";
import "@fontsource/geist/latin-600.css";
import "@fontsource/geist/latin-700.css";
import "@fontsource/geist-mono/latin-400.css";
import "@fontsource/geist-mono/latin-500.css";
import "@fontsource/geist-mono/latin-600.css";
import App from "./App";
import { OverlayApp } from "./OverlayApp";

// Two windows share this bundle: the main editor (default URL) and the
// in-game save-progress overlay (?overlay=1). The overlay window is created
// hidden at startup by lib.rs::setup; this branch picks which React tree to
// render based on the URL it was opened with.
const isOverlay = new URLSearchParams(window.location.search).get("overlay") === "1";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {isOverlay ? <OverlayApp /> : <App />}
  </React.StrictMode>,
);
