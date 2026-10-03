/*
 * //  Copyright 2026 The Tari Project
 * //  SPDX-License-Identifier: BSD-3-Clause
 */

/** The indexer answered a request with a non-2xx status. */
export class HttpError extends Error {
  readonly status: number;

  constructor(status: number, statusText: string, body: string) {
    super(`HTTP ${status}: ${statusText}${body ? ` - ${body}` : ""}`);
    this.name = "HttpError";
    this.status = status;
  }
}
