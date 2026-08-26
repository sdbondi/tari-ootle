//  Copyright 2024 The Tari Project
//  SPDX-License-Identifier: BSD-3-Clause

import React from "react";
import * as ReactDOM from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import App from "./App";
import "./theme/theme.css";

// Follows the system until the viewer picks a side; set before first paint to avoid a flash.
document.documentElement.dataset.theme =
  localStorage.getItem("swarm.theme") ??
  (window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <BrowserRouter>
      <App />
    </BrowserRouter>
  </React.StrictMode>,
);
