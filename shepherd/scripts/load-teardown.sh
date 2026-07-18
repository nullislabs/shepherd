#!/usr/bin/env bash
# scripts/load-teardown.sh - tear down the processes scripts/load-bootstrap.sh started.
# Idempotent.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/load-bootstrap.sh"
load_teardown
