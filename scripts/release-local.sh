#!/usr/bin/env bash
# release-local.sh — cut a full Dante-BabelBox release from this Mac.
#
# GitHub Actions minutes are exhausted, so releases are built here. The shared
# pipeline is scripts/release-rust.sh; this file only says what the project is.
#
#   scripts/release-local.sh                  build into dist-release/
#   scripts/release-local.sh --version 0.2.0  set an explicit version
#   scripts/release-local.sh --upload         tag and publish the GitHub release
set -euo pipefail

RR_NAME="Dante-BabelBox"
RR_SLUG="dante-babelbox"
RR_IDENT="com.stoatworks.dante-babelbox"
RR_EXTRA_FILES=("README.md" "LICENSE" "USAGE.md" "bridge.example.toml" "mics.example.toml")
RR_EXTRA_DIRS=("docs")

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release-rust.sh"
