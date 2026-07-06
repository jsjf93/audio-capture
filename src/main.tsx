import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";

// Both Tauri windows load this same bundle; the window label decides what
// to render. Dynamic imports keep each window's CSS out of the other —
// that matters for the overlay, whose html/body must stay transparent and
// must not inherit the main window's opaque background.
const isOverlaySpike = getCurrentWindow().label === "overlay-spike";

async function mount() {
  const root = ReactDOM.createRoot(document.getElementById("root") as HTMLElement);
  if (isOverlaySpike) {
    const { OverlaySpike } = await import("./spike/OverlaySpike");
    root.render(
      <React.StrictMode>
        <OverlaySpike />
      </React.StrictMode>,
    );
  } else {
    const { default: App } = await import("./App");
    root.render(
      <React.StrictMode>
        <App />
      </React.StrictMode>,
    );
  }
}

mount();
