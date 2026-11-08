#!/usr/bin/env bash
# Golden screenshots (macOS, manual gate — pixels depend on host fonts/OS).
# Set PILCROW_UPDATE_GOLDENS=1 to rewrite the baselines after an intentional
# visual change, and review the PNG diff before committing it.
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo run -p pilcrow-app --features goldens --bin pilcrow-goldens --locked "$@"
