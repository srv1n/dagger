//! End-to-end DAG Flow example with a realistic pipeline

use dagger::{action, coord::ActionRegistry, Cache, DagExecutor};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Order {
    id: String,
    customer: String,
    total: f64,
    country: String,
    items: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OrdersInput {
    orders: Vec<Order>,
}

#[action(name = "seed_orders")]
async fn seed_orders(_input: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let orders = vec![
        Order {
            id: "ord_1001".to_string(),
            customer: "Acme Co".to_string(),
            total: 820.0,
            country: "US".to_string(),
            items: vec!["widgets".to_string(), "gaskets".to_string()],
        },
        Order {
            id: "ord_1002".to_string(),
            customer: "Skyline".to_string(),
            total: 145.5,
            country: "CA".to_string(),
            items: vec!["screws".to_string()],
        },
        Order {
            id: "ord_1003".to_string(),
            customer: "Delta".to_string(),
            total: -10.0,
            country: "US".to_string(),
            items: vec!["refund".to_string()],
        },
        Order {
            id: "ord_1004".to_string(),
            customer: "Futura".to_string(),
            total: 312.0,
            country: "GB".to_string(),
            items: vec!["bearings".to_string(), "seals".to_string()],
        },
        Order {
            id: "ord_1005".to_string(),
            customer: "Boreal".to_string(),
            total: 58.0,
            country: "US".to_string(),
            items: vec![],
        },
    ];

    Ok(json!({ "orders": orders }))
}

#[action(name = "validate_orders")]
async fn validate_orders(input: OrdersInput) -> anyhow::Result<serde_json::Value> {
    let mut valid = Vec::new();
    let mut invalid_count = 0u64;

    for order in input.orders {
        if order.total > 0.0 && !order.items.is_empty() {
            valid.push(order);
        } else {
            invalid_count += 1;
        }
    }

    Ok(json!({
        "valid_orders": valid,
        "invalid_count": invalid_count
    }))
}

#[action(name = "geo_breakdown")]
async fn geo_breakdown(input: OrdersInput) -> anyhow::Result<serde_json::Value> {
    let mut by_country: HashMap<String, u64> = HashMap::new();
    for order in input.orders {
        *by_country.entry(order.country).or_insert(0) += 1;
    }

    Ok(json!({ "by_country": by_country }))
}

#[action(name = "risk_score")]
async fn risk_score(input: OrdersInput) -> anyhow::Result<serde_json::Value> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut high_risk_count = 0u64;

    for order in input.orders {
        let mut score = if order.total > 500.0 {
            0.9
        } else if order.total > 200.0 {
            0.6
        } else {
            0.2
        };

        if order.country != "US" {
            score += 0.1;
        }

        if score >= 0.7 {
            high_risk_count += 1;
        }

        scores.insert(order.id, score);
    }

    Ok(json!({
        "scores": scores,
        "high_risk_count": high_risk_count
    }))
}

#[derive(Debug, Deserialize)]
struct SummaryInput {
    orders: Vec<Order>,
    invalid_count: u64,
    by_country: HashMap<String, u64>,
    high_risk_count: u64,
}

#[action(name = "summarize")]
async fn summarize(input: SummaryInput) -> anyhow::Result<serde_json::Value> {
    let total_revenue: f64 = input.orders.iter().map(|o| o.total).sum();

    let summary = json!({
        "total_orders": input.orders.len(),
        "invalid_orders": input.invalid_count,
        "high_risk_orders": input.high_risk_count,
        "total_revenue": total_revenue,
        "by_country": input.by_country,
    });

    Ok(json!({ "summary": summary }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let registry = ActionRegistry::new();
    let mut executor = DagExecutor::new(None, registry, "sqlite::memory:").await?;

    executor
        .load_yaml_file("examples/fixtures/order_pipeline.yaml")
        .await?;

    let cache = Cache::new();
    let (_tx, rx) = tokio::sync::oneshot::channel();
    let report = executor
        .execute_static_dag("order_pipeline", &cache, rx)
        .await?;

    println!("Pipeline success: {}", report.overall_success);

    let summary: serde_json::Value = dagger::get_input(&cache, "summarize", "summary")?;
    println!("{}", serde_json::to_string_pretty(&summary)?);

    Ok(())
}
