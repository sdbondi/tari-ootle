//  Copyright 2024 The Tari Project
//  SPDX-License-Identifier: BSD-3-Clause

import { Navigate, Route, Routes } from "react-router-dom";
import Shell from "./components/Shell";
import BaseLayer from "./routes/BaseLayer";
import Indexers from "./routes/Indexers";
import Instances from "./routes/Instances";
import LogView from "./routes/LogView";
import Overview from "./routes/Overview";
import Validators from "./routes/Validators";
import Wallets from "./routes/Wallets";
import { SwarmProvider } from "./state/SwarmProvider";

export default function App() {
  return (
    <SwarmProvider>
      <Routes>
        <Route path="/" element={<Shell />}>
          <Route index element={<Overview />} />
          <Route path="validators" element={<Validators />} />
          <Route path="validators/:id" element={<Validators />} />
          <Route path="wallets" element={<Wallets />} />
          <Route path="indexers" element={<Indexers />} />
          <Route path="base-layer" element={<BaseLayer />} />
          <Route path="instances" element={<Instances />} />
          <Route path="log/:name/:format" element={<LogView />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Route>
      </Routes>
    </SwarmProvider>
  );
}
