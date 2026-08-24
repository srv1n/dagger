#!/bin/bash

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

run() {
    echo -e "\n${YELLOW}$*${NC}"
    "$@"
}

echo -e "${BLUE}Dagger Test Suite${NC}"
echo "======================================"

run cargo fmt --all -- --check
run cargo clippy --workspace --all-targets --all-features -- -D warnings
run cargo check --workspace --all-targets --all-features
run cargo test --workspace --all-features
run cargo test --doc --workspace --all-features
run cargo doc --workspace --all-features --no-deps

echo -e "\n======================================"
echo -e "${GREEN}All checks passed.${NC}"
