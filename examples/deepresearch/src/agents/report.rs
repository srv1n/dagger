
use dagger::{get_global_input, insert_global_value, append_global_value, Cache, Message};
use dagger_macros::pubsub_agent;
use serde_json::json;
use anyhow::Result;
use dagger::PubSubExecutor;
use tracing::info;
use anyhow::anyhow;
use tokio::time::{sleep, Duration};
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
    channel: &str,
    message: Message,
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    sleep(Duration::from_secs(2)).await;
    let knowledge_base: Vec<serde_json::Value> = get_global_input(cache, "global", "knowledge_base")?;
    let report_prompt = format!("Generate a final report based on the accumulated knowledge: {:?}", knowledge_base);
    let report_response = llm_generate(&report_prompt, "report_agent").await?;
    let final_report = report_response["final_report"].as_str().unwrap_or_default().to_string();

    executor
        .publish(
            "final_answer",
            Message::new(node_id.to_string(), json!({"final_answer": final_report})),
            cache,
            None
        )
        .await?;

    Ok(())
}