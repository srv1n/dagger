
use dagger::{get_global_input, insert_global_value, append_global_value, Cache, Message};
use dagger_macros::pubsub_agent;
use serde_json::json;
use anyhow::Result;
use dagger::PubSubExecutor;
use tracing::info;
use anyhow::anyhow;

use crate::utils::llm::llm_generate;

#[pubsub_agent(
    name = "PlanningAgent",
    subscribe = "initial_query, gap_questions, evaluation_results",
    publish = "search_queries, gap_questions",
    input_schema = r#"{"type": "object", "properties": {"query": {"type": "string"}, "gap_questions": {"type": "array"}}}"#,
    output_schema = r#"{"type": "object", "properties": {"search_queries": {"type": "array"}, "gap_questions": {"type": "array"}}}"#
)]
pub async fn planning_agent(
    node_id: &str,
    channel: &str,
    message: Message,
    executor: &mut PubSubExecutor,
    cache: &Cache,
) -> Result<()> {
    match channel {
        "initial_query" | "gap_questions" => {
            let query = message.payload["query"].as_str().ok_or(anyhow!("Missing query"))?;
            let rewriter_prompt = format!("Rewrite this query for better search results:\n\n{}", query);
            let rewriter_response = llm_generate(&rewriter_prompt, "query_rewriter").await?;
            let expanded_queries: Vec<String> = rewriter_response["queries"]
                .as_array()
                .ok_or(anyhow!("Invalid query rewriter response"))?
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();

            for expanded_query in &expanded_queries {
                append_global_value(
                    cache,
                    "global",
                    "task_queue",
                    json!({"type": "search", "query": expanded_query, "source": "initial", "attempt_count": 0}),
                )?;
            }
            // Update planned tasks count
            let current_planned: usize = get_global_input(cache, "global", "planned_tasks")?;
            insert_global_value(cache, "global", "planned_tasks", current_planned + expanded_queries.len())?;

            executor
                .publish(
                    "search_queries",
                    Message::new(node_id.to_string(), json!({"search_queries": expanded_queries})),
                    cache,
                )
                .await?;
        }
        "evaluation_results" => {
            let evaluation_results = message.payload["evaluation_results"]
                .as_array()
                .ok_or(anyhow!("Missing evaluation_results"))?;
            for result in evaluation_results {
                if let (Some(false), Some(reason)) = (result["pass"].as_bool(), result["reason"].as_str()) {
                    if reason.contains("definitive") {
                        let original_question = message.payload["question"].as_str().unwrap_or_default();
                        let rewriter_prompt = format!(
                            "Rephrase this query for better search results, given it failed definitiveness:\n\n{}",
                            original_question
                        );
                        let rewriter_response = llm_generate(&rewriter_prompt, "query_rewriter").await?;
                        let new_queries: Vec<String> = rewriter_response["queries"]
                            .as_array()
                            .ok_or(anyhow!("Invalid query rewriter response"))?
                            .iter()
                            .map(|v| v.as_str().unwrap().to_string())
                            .collect();

                        for new_query in &new_queries {
                            append_global_value(
                                cache,
                                "global",
                                "task_queue",
                                json!({"type": "search", "query": new_query, "source": "rephrased", "attempt_count": 0}),
                            )?;
                        }
                        let current_planned: usize = get_global_input(cache, "global", "planned_tasks")?;
                        insert_global_value(cache, "global", "planned_tasks", current_planned + new_queries.len())?;

                        executor
                            .publish(
                                "search_queries",
                                Message::new(node_id.to_string(), json!({"search_queries": new_queries})),
                                cache,
                            )
                            .await?;
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}