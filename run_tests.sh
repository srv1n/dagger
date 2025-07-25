#!/bin/bash

# Dagger Test Suite Runner
# This script runs all tests for the Dagger project

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${BLUE}🚀 Dagger Test Suite${NC}"
echo "======================================"

# Clean up old test databases
echo -e "\n${YELLOW}Cleaning up test databases...${NC}"
rm -rf test_*_db
rm -rf examples/*/target

# Run clippy first
echo -e "\n${YELLOW}Running clippy...${NC}"
if cargo clippy --all-targets --all-features -- -D warnings; then
    echo -e "${GREEN}✓ Clippy passed${NC}"
else
    echo -e "${RED}✗ Clippy failed${NC}"
    exit 1
fi

# Run format check
echo -e "\n${YELLOW}Checking formatting...${NC}"
if cargo fmt -- --check; then
    echo -e "${GREEN}✓ Format check passed${NC}"
else
    echo -e "${RED}✗ Format check failed${NC}"
    echo "Run 'cargo fmt' to fix formatting issues"
    exit 1
fi

# Build the project
echo -e "\n${YELLOW}Building project...${NC}"
if cargo build --all-features; then
    echo -e "${GREEN}✓ Build successful${NC}"
else
    echo -e "${RED}✗ Build failed${NC}"
    exit 1
fi

# Run unit tests
echo -e "\n${YELLOW}Running unit tests...${NC}"
if cargo test --lib --all-features; then
    echo -e "${GREEN}✓ Unit tests passed${NC}"
else
    echo -e "${RED}✗ Unit tests failed${NC}"
    exit 1
fi

# Run integration tests
echo -e "\n${YELLOW}Running integration tests...${NC}"

# DAG Flow tests
echo -e "\n${BLUE}DAG Flow Tests:${NC}"
if cargo test --test test_dag_flow -- --nocapture; then
    echo -e "${GREEN}✓ DAG Flow tests passed${NC}"
else
    echo -e "${RED}✗ DAG Flow tests failed${NC}"
    exit 1
fi

# Task Core tests
echo -e "\n${BLUE}Task Core Tests:${NC}"
if cargo test --test test_task_core -- --nocapture; then
    echo -e "${GREEN}✓ Task Core tests passed${NC}"
else
    echo -e "${RED}✗ Task Core tests failed${NC}"
    exit 1
fi

# Test examples compilation
echo -e "\n${YELLOW}Testing example compilation...${NC}"

examples=("simple_task" "dag_flow" "agent_simple")

for example in "${examples[@]}"; do
    echo -n "  Checking $example... "
    if (cd "examples/$example" && cargo check --quiet); then
        echo -e "${GREEN}OK${NC}"
    else
        echo -e "${RED}FAILED${NC}"
        exit 1
    fi
done

# Run documentation tests
echo -e "\n${YELLOW}Running doc tests...${NC}"
if cargo test --doc --all-features; then
    echo -e "${GREEN}✓ Doc tests passed${NC}"
else
    echo -e "${RED}✗ Doc tests failed${NC}"
    exit 1
fi

# Final cleanup
echo -e "\n${YELLOW}Cleaning up...${NC}"
rm -rf test_*_db

echo -e "\n======================================"
echo -e "${GREEN}✅ All tests passed successfully!${NC}"
echo -e "\nYou can run individual test suites with:"
echo "  cargo test --test test_dag_flow"
echo "  cargo test --test test_task_core"
echo -e "\nOr use the integration runner:"
echo "  cargo run --bin integration_runner"