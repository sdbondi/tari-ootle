//  Copyright 2024 The Tari Project
//  SPDX-License-Identifier: BSD-3-Clause

import { NodeCard } from "../components/NodeCard";
import { useSwarm } from "../state/context";
import { Empty } from "../ui";

export default function Wallets() {
  const swarm = useSwarm();

  return (
    <div className="stack">
      <div className="page-head">
        <div>
          <h1>Wallets</h1>
          <p>Ootle wallet daemons. Each one serves its own web UI and JSON-RPC.</p>
        </div>
      </div>

      {swarm.wallets.length ? (
        <div className="grid">
          {swarm.wallets.map((wallet) => (
            <NodeCard
              key={wallet.instance_id}
              id={wallet.instance_id}
              name={wallet.name}
              isRunning={wallet.is_running}
              links={
                <>
                  <a href={wallet.web} target="_blank" rel="noreferrer">
                    Web UI
                  </a>
                  <a href={wallet.jrpc} target="_blank" rel="noreferrer">
                    JSON-RPC
                  </a>
                </>
              }
            />
          ))}
        </div>
      ) : (
        <Empty>No wallet daemons are configured.</Empty>
      )}
    </div>
  );
}
