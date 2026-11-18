import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { applyGeometry } from "./lib/geometry";
import { isTauri } from "./lib/tauri";
import { NotchWindow } from "./screens/notch/NotchWindow";
import { OnboardingWindow } from "./screens/onboarding/OnboardingWindow";
import { SettingsWindow } from "./screens/settings/SettingsWindow";
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/signals.css";

const ROUTES = {
  notch: NotchWindow,
  settings: SettingsWindow,
  onboarding: OnboardingWindow,
} as const;

type Route = keyof typeof ROUTES;

function routeFromHash(): Route | null {
  const name = window.location.hash.replace(/^#\//, "").split("?")[0] ?? "";
  return name in ROUTES ? (name as Route) : null;
}

function App() {
  const [route, setRoute] = useState<Route | null>(routeFromHash);
  useEffect(() => {
    const onHash = () => setRoute(routeFromHash());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  if (route) {
    const Screen = ROUTES[route];
    return <Screen />;
  }
  return <Index />;
}

function Index() {
  return (
    <nav style={{ padding: 24, display: "grid", gap: 8 }}>
      {(Object.keys(ROUTES) as Route[]).map((r) => (
        <a key={r} href={`#/${r}`} style={{ color: "var(--text)" }}>
          {r}
        </a>
      ))}
    </nav>
  );
}

document.documentElement.dataset.host = isTauri() ? "tauri" : "browser";
if (!isTauri()) {
  // Browser-only preview of a hardware notch, e.g. #/notch?notch=200
  const width = new URLSearchParams(window.location.hash.split("?")[1] ?? "").get("notch");
  if (width) {
    applyGeometry({ notch_width: Number(width), notch_height: 38 });
  }
}
const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
