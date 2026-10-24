import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "./lib/i18n";
import App from "./App";
import "./index.css";
import { desktopPlatform } from "@/lib/desktop-window";
import { isAndroid } from "./lib/mobile";

const desktop = desktopPlatform();
if (desktop) document.documentElement.dataset.desktopPlatform = desktop;
if (isAndroid()) document.documentElement.dataset.mobilePlatform = "android";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
