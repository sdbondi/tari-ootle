//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

/**
 * Serializes a request body to JSON, encoding every `bigint` as a string.
 *
 * The encoding is independent of the value — a field is a string whether it holds `1n` or
 * `2n ** 64n - 1n` — so a field never changes its JSON type between requests. A string is also the
 * only lossless encoding available: JavaScript cannot hold an integer above
 * `Number.MAX_SAFE_INTEGER` in a `number`.
 *
 * Every 64-bit field these APIs expose accepts a string as well as a number (`ootle_serde`'s
 * `str_number` adapter on the Rust side).
 */
export function stringifyRequestBody(body: unknown): string {
  return JSON.stringify(body, bigintReplacer);
}

function bigintReplacer(_key: string, value: unknown): unknown {
  return typeof value === "bigint" ? value.toString() : value;
}
