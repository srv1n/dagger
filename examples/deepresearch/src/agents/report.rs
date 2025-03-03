// src/agents/report.rs (Corrected)
use anyhow::Result;
use dagger::{get_global_input, Cache, Message, PubSubExecutor, TaskStatus};
use dagger_macros::pubsub_agent;
use serde_json::json;
use tracing::info;

use crate::utils::llm::llm_generate;

#[pubsub_agent(
    name = "ReportAgent",
    subscribe = "report_request",
    publish = "final_answer",
    input_schema = r#"{"type": "object", "properties": {"report_request": {"type": "boolean"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"final_report": {"type": "string"}}}"#
)]
pub async fn report_agent(
    node_id: &str,
    channel: &str,  //Use the channel
    message: &Message, // Corrected: &Message
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    let knowledge_base: Vec<serde_json::Value> =
        get_global_input(&cache, "global", "knowledge_base")?;
    let report_prompt = format!(
        "Generate a final report based on the accumulated knowledge: {:?}",
        knowledge_base
    );
    let report_response = llm_generate(&report_prompt, "report_agent").await?;
    let final_report = report_response["final_report"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    executor
        .publish(
            "final_answer", // use correct channel
            Message {
                timestamp: chrono::Local::now().naive_local(),
                source: node_id.to_string(),
                channel: Some("final_answer".to_string()),
                task_id: None,
                message_id: format!("message_{}", chrono::Utc::now().timestamp_nanos()),
                payload: json!({ "final_answer": final_report }),
            },
        )
        .await?;

    Ok(())
}