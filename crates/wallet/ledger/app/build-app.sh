#!/usr/bin/env bash

#
# //  Copyright 2026 The Tari Project
# //  SPDX-License-Identifier: BSD-3-Clause
#

set -e

# 🎯 Ledger App Builder
# Builds the Ledger app using Docker for a given target device.

SUPPORTED_TARGETS=("nanosplus" "nanox" "stax" "flex")

usage() {
  echo ""
  echo "🦀 Ledger App Builder 🦀"
  echo ""
  echo "Usage: $0 <target...|all>"
  echo ""
  echo "📦 Supported targets:"
  for t in "${SUPPORTED_TARGETS[@]}"; do
    echo "   • $t"
  done
  echo ""
  echo "📖 Examples:"
  echo "   $0 nanosplus"
  echo "   $0 nanox flex"
  echo "   $0 all"
  echo ""
  exit 1
}

if [ -z "$1" ]; then
  echo "❌ Error: No target specified."
  usage
fi

# `all` expands to every supported target.
if [ "$1" = "all" ]; then
  set -- "${SUPPORTED_TARGETS[@]}"
fi

for TARGET in "$@"; do
  VALID=false
  for t in "${SUPPORTED_TARGETS[@]}"; do
    if [ "$TARGET" = "$t" ]; then
      VALID=true
      break
    fi
  done

  if [ "$VALID" = false ]; then
    echo "❌ Error: Unknown target '$TARGET'."
    usage
  fi
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# The Rust toolchain and every device SDK live in this image; the app cannot be built with the
# host toolchain. Overridable so a caller can pin a digest.
IMAGE="${LEDGER_APP_BUILDER_IMAGE:-ghcr.io/ledgerhq/ledger-app-builder/ledger-app-builder}"

# A TTY is only available when a human runs this; CI has none and `docker run -t` would fail.
DOCKER_TTY=()
if [ -t 0 ] && [ -t 1 ]; then
  DOCKER_TTY=(-it)
fi

for TARGET in "$@"; do
  echo ""
  echo "🚀 Building Ledger app for target: $TARGET"
  echo "📁 Using source directory: $SCRIPT_DIR"
  echo ""

  # NBGL targets (Stax/Flex) need the `nbgl` cargo feature, which enables the SDK's
  # `io_new` + `nano_nbgl`. BAGL targets (Nano S+/X) build with default features.
  EXTRA=""
  case "$TARGET" in
    stax | flex) EXTRA="-- --features nbgl" ;;
  esac

  # Mount the ledger workspace root (parent of this app crate) so the `../common` path
  # dependency resolves inside the container; build from the app crate directory.
  docker run --rm "${DOCKER_TTY[@]}" \
    -v "$SCRIPT_DIR/..:/app" \
    -w /app/app \
    "$IMAGE" \
    bash -lc "cargo ledger build $TARGET $EXTRA"

  echo ""
  echo "✅ Build complete for target: $TARGET"
  echo ""
done

