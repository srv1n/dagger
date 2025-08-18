//! Integration test runner for Dagger project
//!
//! This runner executes all test suites and provides a summary of results.
//! Use this to verify nothing is broken after making changes.

use colored::*;
use std::process::Command;
use std::time::Instant;

#[derive(Default)]
struct TestResults {
    passed: usize,
    failed: usize,
    total_time: std::time::Duration,
}

fn main() {
    println!("{}", "\n🚀 Dagger Integration Test Runner".bold().blue());
    println!("{}", "=".repeat(40).dimmed());

    let start = Instant::now();
    let mut results = TestResults::default();

    // Test suites to run
    let test_suites = vec![
        ("DAG Flow Tests", "test_dag_flow_simple"),
        ("Task Core Tests", "test_task_core_simple"),
    ];

    // Run each test suite
    for (name, test_module) in test_suites {
        println!("\n{} {}", "▶".green(), name.bold());

        let output = Command::new("cargo")
            .args(&["test", "--test", test_module, "--", "--nocapture"])
            .output()
            .expect("Failed to execute tests");

        if output.status.success() {
            results.passed += 1;
            println!("{} {} {}", "✓".green().bold(), name, "PASSED".green());

            // Extract test count from output
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(line) = stdout.lines().find(|l| l.contains("test result:")) {
                println!("  {}", line.dimmed());
            }
        } else {
            results.failed += 1;
            println!("{} {} {}", "✗".red().bold(), name, "FAILED".red());

            // Show failure details
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);

            println!("\n{}", "  Error Output:".red());
            for line in stderr
                .lines()
                .chain(stdout.lines())
                .filter(|l| l.contains("FAILED") || l.contains("error"))
                .take(10)
            {
                println!("  {}", line);
            }
        }
    }

    // Run example tests
    println!("\n{} {}", "▶".green(), "Example Compilation Tests".bold());

    let examples = vec!["simple_task", "dag_flow", "agent_simple"];

    for example in examples {
        print!("  Testing example '{}' ... ", example);

        let output = Command::new("cargo")
            .args(&["check", "--example", example])
            .current_dir("examples/".to_owned() + example)
            .output();

        match output {
            Ok(output) if output.status.success() => {
                println!("{}", "OK".green());
                results.passed += 1;
            }
            _ => {
                println!("{}", "FAILED".red());
                results.failed += 1;
            }
        }
    }

    results.total_time = start.elapsed();

    // Summary
    println!("\n{}", "=".repeat(40).dimmed());
    println!("{}", "Test Summary".bold());
    println!(
        "  {} {} passed, {} {} failed",
        results.passed.to_string().green().bold(),
        if results.passed == 1 { "test" } else { "tests" },
        results.failed.to_string().red().bold(),
        if results.failed == 1 { "test" } else { "tests" }
    );
    println!("  Total time: {:.2}s", results.total_time.as_secs_f64());

    // Exit code
    if results.failed > 0 {
        println!("\n{}", "❌ Some tests failed!".red().bold());
        std::process::exit(1);
    } else {
        println!("\n{}", "✅ All tests passed!".green().bold());
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_runner_compiles() {
        // This test just ensures the runner itself compiles
        assert!(true);
    }
}
