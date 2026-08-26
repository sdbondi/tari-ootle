//  Copyright 2024 The Tari Project
//  SPDX-License-Identifier: BSD-3-Clause

import { createContext, useContext } from "react";
import { Indexer, Instance, LogFile, StdoutFile, ValidatorNode, VnDetail, WalletDaemon } from "../types";

export interface InstanceLogs {
  logs: LogFile[];
  stdout: StdoutFile[];
}

export interface Toast {
  id: number;
  message: string;
}

export interface Swarm {
  instances: Instance[];
  validators: ValidatorNode[];
  wallets: WalletDaemon[];
  indexers: Indexer[];
  /** Validator detail keyed by instance id. */
  details: Record<number, VnDetail>;
  /** Logs keyed by instance id. */
  logs: Record<number, InstanceLogs>;
  baseNodeHeight: number | null;
  isMining: boolean;
  loaded: boolean;
  intervalMs: number;
  setIntervalMs: (ms: number) => void;
  refresh: () => void;
  /** Runs an action, reports failures, then refreshes. */
  act: (label: string, run: () => Promise<unknown>) => Promise<void>;
  toasts: Toast[];
  dismiss: (id: number) => void;
  report: (message: string) => void;
}

export const SwarmContext = createContext<Swarm | null>(null);

export function useSwarm(): Swarm {
  const ctx = useContext(SwarmContext);
  if (!ctx) {
    throw new Error("useSwarm must be used inside SwarmProvider");
  }
  return ctx;
}
