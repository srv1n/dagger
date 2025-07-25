# Dagger Test Suite

This directory contains comprehensive test suites for the Dagger project, covering both the DAG Flow system and the Task-Core agent system.

## Test Files

### `test_dag_flow.rs`
Tests for the DAG Flow execution engine:
- Simple DAG execution with dependencies
- Parallel node execution
- Failure handling and retry mechanisms
- YAML workflow loading
- Cache persistence
- Timeout handling

### `test_task_core.rs`
Tests for the Task-Core agent system:
- Agent registration and execution
- Task lifecycle management
- Dependency resolution
- Retry mechanisms (retryable vs non-retryable errors)
- Subtask spawning
- Concurrent execution
- System persistence and recovery
- Timeout handling

### `integration_runner.rs`
A comprehensive test runner that executes all test suites and provides a summary.

## Running Tests

### Quick Start
```bash
# Run all tests with the shell script
./run_tests.sh

# Or use the integration runner
cargo run --bin integration_runner
```

### Individual Test Suites
```bash
# Run DAG Flow tests only
cargo test --test test_dag_flow

# Run Task-Core tests only
cargo test --test test_task_core

# Run a specific test
cargo test --test test_dag_flow test_simple_dag_execution

# Run with output
cargo test --test test_dag_flow -- --nocapture
```

### Unit Tests
```bash
# Run all unit tests
cargo test --lib

# Run tests for a specific module
cargo test --lib dag_flow::
```

### Example Tests
```bash
# Check all examples compile
for example in simple_task dag_flow agent_simple; do
    (cd examples/$example && cargo check)
done
```

## Test Database Cleanup

The tests create temporary databases (e.g., `test_*_db/`). These are automatically cleaned up, but you can manually remove them:

```bash
rm -rf test_*_db/
```

## Writing New Tests

When adding new functionality:

1. **For DAG Flow features**: Add tests to `test_dag_flow.rs`
2. **For Task-Core features**: Add tests to `test_task_core.rs`
3. **Follow the pattern**:
   - Use descriptive test names
   - Clean up resources (databases)
   - Test both success and failure cases
   - Include timeout tests for async operations

## Test Coverage Areas

### DAG Flow
- ✅ Basic execution
- ✅ Parallel execution
- ✅ Dependency management
- ✅ Error handling
- ✅ Retry logic
- ✅ YAML loading
- ✅ Cache operations

### Task-Core
- ✅ Agent lifecycle
- ✅ Task submission
- ✅ Dependency blocking/unblocking
- ✅ Retry mechanisms
- ✅ Concurrent execution
- ✅ Persistence/recovery
- ✅ Error categorization

## Continuous Integration

For CI/CD pipelines, use:

```bash
# GitHub Actions example
- name: Run Tests
  run: ./run_tests.sh

# Or with cargo
- name: Run Tests
  run: |
    cargo clippy -- -D warnings
    cargo fmt -- --check
    cargo test --all-features
```

## Debugging Failed Tests

1. **Run with verbose output**:
   ```bash
   RUST_LOG=debug cargo test --test test_name -- --nocapture
   ```

2. **Run single test**:
   ```bash
   cargo test --test test_dag_flow test_simple_dag_execution -- --exact
   ```

3. **Check test databases**:
   ```bash
   ls -la test_*_db/
   ```

4. **Enable backtrace**:
   ```bash
   RUST_BACKTRACE=1 cargo test
   ```