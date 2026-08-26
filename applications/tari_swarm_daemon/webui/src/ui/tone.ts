//  Copyright 2024 The Tari Project
//  SPDX-License-Identifier: BSD-3-Clause

/** The status vocabulary the whole interface is coloured by. */
export type Tone = "up" | "lag" | "down" | "rail" | "off";

export function toneColour(tone: Tone): string {
  return `var(--${tone === "off" ? "off" : tone})`;
}
