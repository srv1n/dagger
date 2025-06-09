use anyhow::{anyhow, Result};
use dagger::pubsub::pubsub::{Message, PubSubAgent, PubSubConfig, PubSubExecutor, PubSubWorkflowSpec};
use dagger::dag_flow::Cache;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tracing::{info, warn};

/// A news aggregation supervisor that coordinates the workflow
struct NewsAggregatorSupervisor;

#[async_trait]
impl PubSubAgent for NewsAggregatorSupervisor {
    fn name(&self) -> String {
        "NewsAggregatorSupervisor".to_string()
    }

    fn description(&self) -> String {
        "Orchestrates the news aggregation process".to_string()
    }

    fn subscriptions(&self) -> Vec<String> {
        vec!["start_aggregation".to_string()]
    }

    fn publications(&self) -> Vec<String> {
        vec!["search_requests".to_string()]
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "topic": {"type": "string"},
                "num_sources": {"type": "integer"}
            },
            "required": ["topic"]
        })
    }

    fn output_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "search_query": {"type": "string"},
                "source": {"type": "string"},
                "request_id": {"type": "string"}
            },
            "required": ["search_query", "source", "request_id"]
        })
    }

    async fn process_message(
        &self,
        node_id: &str,
        _channel: &str,
        message: &Message,
        executor: &mut PubSubExecutor,
        cache: &Cache,
    ) -> Result<()> {
        let topic = message.payload["topic"].as_str().unwrap_or("technology");
        let num_sources = message.payload["num_sources"].as_i64().unwrap_or(3);

        info!("🎯 Supervisor: Starting news aggregation for topic '{}'", topic);

        // Create search requests for different news sources
        let sources = vec!["TechCrunch", "BBC", "Reuters", "CNN", "The Guardian"];
        
        for (i, source) in sources.iter().take(num_sources as usize).enumerate() {
            let search_payload = json!({
                "search_query": format!("{} latest news", topic),
                "source": source,
                "request_id": format!("req_{}_{}", topic, i)
            });

            let search_message = Message::new(node_id.to_string(), search_payload);
            executor.publish("search_requests", search_message, cache, None).await?;
            
            info!("📤 Supervisor: Sent search request to {}", source);
        }

        Ok(())
    }
}

/// A fictional news fetcher that simulates fetching news from different sources
struct NewsFetcher;

#[async_trait]
impl PubSubAgent for NewsFetcher {
    fn name(&self) -> String {
        "NewsFetcher".to_string()
    }

    fn description(&self) -> String {
        "Fetches news from various sources (fictional)".to_string()
    }

    fn subscriptions(&self) -> Vec<String> {
        vec!["search_requests".to_string()]
    }

    fn publications(&self) -> Vec<String> {
        vec!["raw_articles".to_string()]
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "search_query": {"type": "string"},
                "source": {"type": "string"},
                "request_id": {"type": "string"}
            },
            "required": ["search_query", "source", "request_id"]
        })
    }

    fn output_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "articles": {"type": "array"},
                "source": {"type": "string"},
                "total_found": {"type": "integer"}
            },
            "required": ["articles", "source", "total_found"]
        })
    }

    async fn process_message(
        &self,
        node_id: &str,
        _channel: &str,
        message: &Message,
        executor: &mut PubSubExecutor,
        cache: &Cache,
    ) -> Result<()> {
        let query = message.payload["search_query"].as_str().unwrap_or("");
        let source = message.payload["source"].as_str().unwrap_or("Unknown");
        let request_id = message.payload["request_id"].as_str().unwrap_or("");

        info!("📰 Fetcher: Searching '{}' on {}", query, source);

        // Simulate network delay
        tokio::time::sleep(Duration::from_millis(100 + fastrand::u64(100..500))).await;

        // Generate fictional articles based on the source
        let articles = match source {
            "TechCrunch" => vec![
                json!({
                    "title": "Revolutionary AI Breakthrough Changes Everything",
                    "summary": "Scientists develop new AI model that can predict market trends with 95% accuracy",
                    "url": "https://techcrunch.com/fake-article-1",
                    "published": "2024-01-15T10:30:00Z"
                }),
                json!({
                    "title": "Startup Raises $100M for Quantum Computing Platform",
                    "summary": "New quantum computing startup secures massive funding round",
                    "url": "https://techcrunch.com/fake-article-2",
                    "published": "2024-01-15T08:15:00Z"
                })
            ],
            "BBC" => vec![
                json!({
                    "title": "Global Technology Summit Announces New Standards",
                    "summary": "World leaders agree on new international technology standards",
                    "url": "https://bbc.com/fake-article-1",
                    "published": "2024-01-15T09:45:00Z"
                })
            ],
            "Reuters" => vec![
                json!({
                    "title": "Tech Stocks Surge Following Innovation Announcement",
                    "summary": "Major technology stocks see significant gains after breakthrough",
                    "url": "https://reuters.com/fake-article-1",
                    "published": "2024-01-15T11:20:00Z"
                })
            ],
            _ => vec![
                json!({
                    "title": format!("Breaking: {} News Update", source),
                    "summary": format!("Latest developments in {} from {}", query, source),
                    "url": format!("https://{}.com/fake-article", source.to_lowercase()),
                    "published": "2024-01-15T12:00:00Z"
                })
            ]
        };

        let response_payload = json!({
            "articles": articles,
            "source": source,
            "total_found": articles.len(),
            "request_id": request_id
        });

        let response = Message::new(node_id.to_string(), response_payload);
        executor.publish("raw_articles", response, cache, None).await?;

        info!("✅ Fetcher: Found {} articles from {}", articles.len(), source);
        Ok(())
    }
}

/// An article processor that analyzes and summarizes articles
struct ArticleProcessor;

#[async_trait]
impl PubSubAgent for ArticleProcessor {
    fn name(&self) -> String {
        "ArticleProcessor".to_string()
    }

    fn description(&self) -> String {
        "Processes and analyzes articles for relevance and sentiment".to_string()
    }

    fn subscriptions(&self) -> Vec<String> {
        vec!["raw_articles".to_string()]
    }

    fn publications(&self) -> Vec<String> {
        vec!["processed_articles".to_string()]
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "articles": {"type": "array"},
                "source": {"type": "string"},
                "total_found": {"type": "integer"}
            },
            "required": ["articles", "source"]
        })
    }

    fn output_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "processed_articles": {"type": "array"},
                "source": {"type": "string"},
                "sentiment_score": {"type": "number"},
                "relevance_score": {"type": "number"}
            },
            "required": ["processed_articles", "source"]
        })
    }

    async fn process_message(
        &self,
        node_id: &str,
        _channel: &str,
        message: &Message,
        executor: &mut PubSubExecutor,
        cache: &Cache,
    ) -> Result<()> {
        let empty_vec = vec![];
        let articles = message.payload["articles"].as_array().unwrap_or(&empty_vec);
        let source = message.payload["source"].as_str().unwrap_or("Unknown");

        info!("🔬 Processor: Analyzing {} articles from {}", articles.len(), source);

        // Simulate processing delay
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut processed_articles = Vec::new();
        let mut total_sentiment = 0.0;
        let mut total_relevance = 0.0;

        for article in articles {
            // Simulate sentiment analysis (fake scores)
            let sentiment = fastrand::f64() * 2.0 - 1.0; // Random between -1 and 1
            let relevance = fastrand::f64(); // Random between 0 and 1

            total_sentiment += sentiment;
            total_relevance += relevance;

            let processed = json!({
                "title": article["title"],
                "summary": article["summary"],
                "url": article["url"],
                "published": article["published"],
                "sentiment": sentiment,
                "relevance": relevance,
                "keywords": vec!["technology", "innovation", "news"],
                "category": if relevance > 0.7 { "high_priority" } else { "normal" }
            });

            processed_articles.push(processed);
        }

        let avg_sentiment = if articles.is_empty() { 0.0 } else { total_sentiment / articles.len() as f64 };
        let avg_relevance = if articles.is_empty() { 0.0 } else { total_relevance / articles.len() as f64 };

        let response_payload = json!({
            "processed_articles": processed_articles,
            "source": source,
            "sentiment_score": avg_sentiment,
            "relevance_score": avg_relevance
        });

        let response = Message::new(node_id.to_string(), response_payload);
        executor.publish("processed_articles", response, cache, None).await?;

        info!("🎯 Processor: Processed {} articles from {} (sentiment: {:.2}, relevance: {:.2})", 
            processed_articles.len(), source, avg_sentiment, avg_relevance);
        Ok(())
    }
}

/// A report generator that creates final summaries
struct ReportGenerator;

#[async_trait]
impl PubSubAgent for ReportGenerator {
    fn name(&self) -> String {
        "ReportGenerator".to_string()
    }

    fn description(&self) -> String {
        "Generates final reports from processed articles".to_string()
    }

    fn subscriptions(&self) -> Vec<String> {
        vec!["processed_articles".to_string()]
    }

    fn publications(&self) -> Vec<String> {
        vec!["final_reports".to_string()]
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "processed_articles": {"type": "array"},
                "source": {"type": "string"},
                "sentiment_score": {"type": "number"},
                "relevance_score": {"type": "number"}
            },
            "required": ["processed_articles", "source"]
        })
    }

    fn output_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "report": {"type": "object"},
                "summary": {"type": "string"}
            },
            "required": ["report", "summary"]
        })
    }

    async fn process_message(
        &self,
        node_id: &str,
        _channel: &str,
        message: &Message,
        executor: &mut PubSubExecutor,
        cache: &Cache,
    ) -> Result<()> {
        let empty_vec = vec![];
        let articles = message.payload["processed_articles"].as_array().unwrap_or(&empty_vec);
        let source = message.payload["source"].as_str().unwrap_or("Unknown");
        let sentiment = message.payload["sentiment_score"].as_f64().unwrap_or(0.0);
        let relevance = message.payload["relevance_score"].as_f64().unwrap_or(0.0);

        info!("📊 Reporter: Generating report for {} articles from {}", articles.len(), source);

        // Simulate report generation delay
        tokio::time::sleep(Duration::from_millis(150)).await;

        let high_priority_count = articles.iter()
            .filter(|article| article["category"].as_str() == Some("high_priority"))
            .count();

        let sentiment_label = if sentiment > 0.2 {
            "Positive"
        } else if sentiment < -0.2 {
            "Negative"
        } else {
            "Neutral"
        };

        let report = json!({
            "source": source,
            "total_articles": articles.len(),
            "high_priority_articles": high_priority_count,
            "average_sentiment": sentiment,
            "sentiment_label": sentiment_label,
            "average_relevance": relevance,
            "generated_at": chrono::Utc::now().to_rfc3339(),
            "top_articles": articles.iter().take(3).collect::<Vec<_>>()
        });

        let summary = format!(
            "📋 Report Summary for {}:\n\
            - Found {} articles ({} high priority)\n\
            - Overall sentiment: {} ({:.2})\n\
            - Average relevance: {:.2}\n\
            - Generated at: {}",
            source,
            articles.len(),
            high_priority_count,
            sentiment_label,
            sentiment,
            relevance,
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
        );

        let response_payload = json!({
            "report": report,
            "summary": summary
        });

        let response = Message::new(node_id.to_string(), response_payload);
        executor.publish("final_reports", response, cache, None).await?;

        info!("📄 Reporter: Generated report for {}", source);
        println!("\n{}\n", summary);
        Ok(())
    }
}

/// A final collector that gathers all reports
struct FinalCollector {
    collected_reports: Arc<tokio::sync::Mutex<Vec<Value>>>,
}

impl FinalCollector {
    fn new() -> Self {
        Self {
            collected_reports: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl PubSubAgent for FinalCollector {
    fn name(&self) -> String {
        "FinalCollector".to_string()
    }

    fn description(&self) -> String {
        "Collects all final reports and creates master summary".to_string()
    }

    fn subscriptions(&self) -> Vec<String> {
        vec!["final_reports".to_string()]
    }

    fn publications(&self) -> Vec<String> {
        vec![] // Final collector doesn't publish
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "report": {"type": "object"},
                "summary": {"type": "string"}
            },
            "required": ["report", "summary"]
        })
    }

    fn output_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "master_summary": {"type": "string"},
                "total_sources": {"type": "integer"}
            }
        })
    }

    async fn process_message(
        &self,
        _node_id: &str,
        _channel: &str,
        message: &Message,
        _executor: &mut PubSubExecutor,
        _cache: &Cache,
    ) -> Result<()> {
        let report = &message.payload["report"];
        let summary = message.payload["summary"].as_str().unwrap_or("");

        let mut reports = self.collected_reports.lock().await;
        reports.push(report.clone());

        info!("📥 Collector: Received report #{} from {}", 
            reports.len(), 
            report["source"].as_str().unwrap_or("Unknown"));

        // Print the individual summary
        println!("{}", summary);

        // If we've collected multiple reports, create a master summary
        if reports.len() >= 2 {
            let total_articles: i64 = reports.iter()
                .map(|r| r["total_articles"].as_i64().unwrap_or(0))
                .sum();

            let avg_sentiment: f64 = reports.iter()
                .map(|r| r["average_sentiment"].as_f64().unwrap_or(0.0))
                .sum::<f64>() / reports.len() as f64;

            let sources: Vec<String> = reports.iter()
                .map(|r| r["source"].as_str().unwrap_or("Unknown").to_string())
                .collect();

            let master_summary = format!(
                "\n🎉 === MASTER SUMMARY ===\n\
                📊 Aggregated {} reports from sources: {}\n\
                📰 Total articles processed: {}\n\
                😊 Overall sentiment: {:.2} ({})\n\
                ⏰ Completed at: {}\n\
                ========================\n",
                reports.len(),
                sources.join(", "),
                total_articles,
                avg_sentiment,
                if avg_sentiment > 0.2 { "Positive" } else if avg_sentiment < -0.2 { "Negative" } else { "Neutral" },
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
            );

            println!("{}", master_summary);
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing for better logging
    tracing_subscriber::fmt::init();

    println!("🚀 Starting Dagger Pub/Sub News Aggregation Demo");
    println!("=================================================");

    let mut executor = PubSubExecutor::new(Some(PubSubConfig::default()));
    let cache = Cache::new();

    // Register all agents
    executor.register_agent(Arc::new(NewsAggregatorSupervisor)).await?;
    executor.register_agent(Arc::new(NewsFetcher)).await?;
    executor.register_agent(Arc::new(ArticleProcessor)).await?;
    executor.register_agent(Arc::new(ReportGenerator)).await?;
    executor.register_agent(Arc::new(FinalCollector::new())).await?;

    // Show registered agents
    let agents = executor.list_agents().await;
    println!("\n📋 Registered {} agents:", agents.len());
    for agent in &agents {
        println!("  - {} (subscribes: {:?}, publishes: {:?})", 
            agent.name, agent.subscriptions, agent.publications);
    }

    let channels = executor.list_channels().await;
    println!("\n📡 Available channels: {:?}", channels);

    // Create initial message to start the workflow
    let initial_message = Message::new(
        "external_user".to_string(),
        json!({
            "topic": "artificial intelligence",
            "num_sources": 3
        })
    );

    let workflow = PubSubWorkflowSpec::StartWith {
        channel: "start_aggregation".to_string(),
        message: initial_message,
    };

    // Set up cancellation after a reasonable time
    let (cancel_tx, cancel_rx) = oneshot::channel();
    
    // Cancel after 5 seconds to allow the workflow to complete
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let _ = cancel_tx.send(());
    });

    println!("\n🎬 Starting workflow...\n");

    // Execute the workflow
    if let Err(e) = executor.execute(workflow, &cache, cancel_rx).await {
        eprintln!("❌ Execution failed: {}", e);
    } else {
        println!("✅ Workflow completed successfully!");
    }

    // Allow some time for final messages to be processed
    tokio::time::sleep(Duration::from_millis(500)).await;

    println!("\n🏁 Demo completed!");
    Ok(())
}