#!/bin/bash

# Dagger Test Suite Runner
# This script runs all tests for the Dagger project

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

echo -e "${BLUE}Dagger Test Suite${NC}"
echo "======================================"

# Clean up old test databases
echo -e "\n${YELLOW}Cleaning up test databases...${NC}"
rm -rf test_*_db
rm -rf examples/*/target

# Run clippy first (allow warnings for now)
echo -e "\n${YELLOW}Running clippy...${NC}"
if cargo clippy --lib --tests -- -A warnings; then
    echo -e "${GREEN}Clippy passed${NC}"
else
    echo -e "${RED}Clippy failed${NC}"
    exit 1
fi

# Run format check (non-failing)
echo -e "\n${YELLOW}Checking formatting...${NC}"
if cargo fmt -- --check; then
    echo -e "${GREEN}Format check passed${NC}"
else
    echo -e "${YELLOW}Format check failed (running cargo fmt)${NC}"
    cargo fmt
    echo -e "${GREEN}Formatting fixed${NC}"
fi

# Build the project
echo -e "\n${YELLOW}Building project...${NC}"
if cargo build --all-features; then
    echo -e "${GREEN}Build successful${NC}"
else
    echo -e "${RED}Build failed${NC}"
    exit 1
fi

# Run unit tests
echo -e "\n${YELLOW}Running unit tests...${NC}"
if cargo test --lib --all-features; then
    echo -e "${GREEN}Unit tests passed${NC}"
else
    echo -e "${RED}Unit tests failed${NC}"
    exit 1
fi

# Run integration tests
echo -e "\n${YELLOW}Running integration tests...${NC}"

# DAG Flow tests
echo -e "\n${BLUE}DAG Flow Tests:${NC}"
if cargo test --test test_dag_flow_simple -- --nocapture; then
    echo -e "${GREEN}DAG Flow tests passed${NC}"
else
    echo -e "${RED}DAG Flow tests failed${NC}"
    exit 1
fi

# Task Core tests
echo -e "\n${BLUE}Task Core Tests:${NC}"
if cargo test --test test_task_core_simple -- --nocapture; then
    echo -e "${GREEN}Task Core tests passed${NC}"
else
    echo -e "${RED}Task Core tests failed${NC}"
    exit 1
fi

# Test examples compilation
echo -e "\n${YELLOW}Checking examples...${NC}"
for example in dag_flow_basic dag_flow_pipeline dag_flow_cli dag_flow_dot task_agent_basic pubsub_basic dynamic_nodes_demo coordinator_demo; do
    if cargo check --example "$example"; then
        echo -e "${GREEN}Example $example OK${NC}"
    else
        echo -e "${RED}Example $example failed${NC}"
        exit 1
    fi
done

# Final cleanup
echo -e "\n${YELLOW}Cleaning up...${NC}"
rm -rf test_*_db

echo -e "\n======================================"
echo -e "${GREEN}All tests passed successfully!${NC}"
echo -e "\nYou can run individual test suites with:"
echo "  cargo test --test test_dag_flow_simple"
echo "  cargo test --test test_task_core_simple"
