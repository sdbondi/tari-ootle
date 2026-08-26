//  Copyright 2024 The Tari Project
//  SPDX-License-Identifier: BSD-3-Clause

import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The swarm daemon's default webserver bind address.
const DAEMON = process.env.SWARM_DAEMON_URL || "http://127.0.0.1:8080";

export default defineConfig({
  build: {
    // Keep .gitkeep
    emptyOutDir: false,
  },
  plugins: [react()],
  server: {
    // `npm run dev` talks to a running swarm daemon without any extra setup. Point it
    // somewhere else with SWARM_DAEMON_URL.
    proxy: {
      "/json_rpc": DAEMON,
      "/misc": DAEMON,
    },
  },
});
