use std::{env, error::Error};
use serde::{Deserialize, Serialize};
use serde_json::json;
use schemars::JsonSchema;
use schemars::schema_for;
use anyhow::{Context, Result};
use serde_json::Value;
use chrono::{Local, Datelike, Timelike};
use regex::Regex;

use crate::utils::llm::{
    get_structured_output_with_template,
    get_structured_output_typed_with_template,
    get_structured_output_from_template
};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct TaskResult {
    task_id: String,
    status: String,
    result: String,
}


#[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct Stories {
        stories: Vec<Story>,
    }
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Story {
    id: String,
    description: String,
    acceptance_criteria: String,
    assigned_manager: String,
    dependencies: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Tasks {
    story_completed: Option<bool>,
    tasks: Vec<Task>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Task {
    task_id: String,
    task_description: String,
    assigned_worker: String,
    input: Value,
    dependencies: Vec<String>,
}

/// Example of using templates with LLM calls
pub async fn run_template_example() -> Result<()> {
    // Get current date, time, and day information
    let now = Local::now();
    let current_date = now.format("%Y-%m-%d").to_string();
    let current_time = now.format("%H:%M:%S").to_string();
    let current_day = match now.weekday() {
        chrono::Weekday::Mon => "Monday",
        chrono::Weekday::Tue => "Tuesday",
        chrono::Weekday::Wed => "Wednesday",
        chrono::Weekday::Thu => "Thursday",
        chrono::Weekday::Fri => "Friday",
        chrono::Weekday::Sat => "Saturday",
        chrono::Weekday::Sun => "Sunday",
    };
    
    // Get approximate location based on timezone
    let timezone = format!("{:?}", Local::now().timezone());
    let approximate_location = get_location_from_timezone(&timezone);

    println!("Date: {}, Time: {}, Day: {}, Location: {}", current_date, current_time, current_day, approximate_location);

    let objective_description = "Find the enrollment count of the clinical trial on H. pylori in acne vulgaris patients from Jan-May 2018 as listed on the NIH website.";
    
    // Example 1: Using a template with raw JSON output
    let context = json!({
        "current_date": current_date,
        "current_time": current_time,
        "current_day": current_day,
        "user_location": approximate_location,
        "worker_tools_descriptions": "Search & Retrieve(Google Search, Wikipedia, PubMed, Arxiv, Can browse pages and extract as Markdown content), Python Interpreter(can execute python code and return the result, used for data analysis and calculation. Even simple things like addition, subtraction, multiplication, division, where LLMs may hallucinate)",
        "objective_description": objective_description
    });

    let schema = serde_json::to_value(schema_for!(Stories))
        .context("Failed to convert Stories schema to JSON value")?;
    // println!("schema: {:#?}", schema);
    
    println!("Supervisor Planning for Objective: {}", objective_description);
    let model = env::var("OPEN_AI_MODEL")
        .unwrap_or_else(|_| "qwen-qwq-32b".to_string());
    
    let result = get_structured_output_with_template(
        "supervisor_planning", 
        &context, 
        schema, 
        &model, 
        5024
    ).await
    .map_err(|e| anyhow::anyhow!("Failed to get structured output with template: {}", e))?;

    // println!("Result: {:#?}", result);
    
    let stories: Stories = serde_json::from_value(result.clone())
        .map_err(|e| anyhow::anyhow!("Failed to deserialize stories: {}", e))?;
    
    // println!("Result: {:#?}", stories);
    
    let stories: Vec<Story> = result.get("stories")
        .ok_or_else(|| anyhow::anyhow!("Response missing 'stories' field"))?
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("'stories' field is not an array"))?
        .iter()
        .map(|v| serde_json::from_value::<Story>(v.clone())
            .map_err(|e| anyhow::anyhow!("Failed to deserialize story: {}", e)))
        .collect::<Result<Vec<_>, _>>()?;
    
    println!("Result: {:#?}", stories);
    
    // Example 2: Using a template with typed output
    // #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    // struct Story {
    //     id: String,
    //     description: String,
    //     acceptance_criteria: String,
    //     assigned_manager: String,
    //     dependencies: Vec<String>,
    // }
    
    

    let story_context = json!({
            "current_date": current_date,
            "user_location": approximate_location,
            "worker_tools_descriptions": "Search & Retrieve(Google Search, Wikipedia, PubMed, Arxiv, Can browse pages and extract as Markdown content), Python Interpreter(can execute python code and return the result, used for data analysis and calculation. Even simple things like addition, subtraction, multiplication, division, where LLMs may hallucinate)",
         "objective_description": objective_description,
        "story_id": stories[0].id,
        "story_description": stories[0].description,
        "acceptance_criteria": stories[0].acceptance_criteria,
        "story_dependencies": stories[0].dependencies
    });
    
    let task_schema = serde_json::to_value(schema_for!(Tasks))
        .context("Failed to convert Task schema to JSON value")?;
    println!("\nManager Planning for Story: {}", stories[0].description);
    let result = get_structured_output_with_template(
        "manager_planning", 
        &story_context, 
        task_schema, 
        &env::var("OPEN_AI_MODEL").unwrap_or_else(|_| "qwen-qwq-32b".to_string()), 
        5024
    ).await
    .map_err(|e| anyhow::anyhow!("Failed to get structured output from template: {}", e))?;
    println!("Result: {:#?}", result);

    let tasks: Tasks = serde_json::from_value(result.clone())
        .map_err(|e| anyhow::anyhow!("Failed to deserialize tasks: {}", e))?;
    
    println!("Result: {:#?}", tasks);
    
    Ok(())
}

/// Get an approximate location based on timezone
fn get_location_from_timezone(timezone: &str) -> String {
    println!("Timezone: {}", timezone);
    // Extract location information from timezone string using regex
    // Most timezone strings follow format like "America/New_York" or "Europe/London"
    if let Ok(re) = Regex::new(r"([A-Za-z]+)/([A-Za-z_]+)") {
        if let Some(captures) = re.captures(timezone) {
            if captures.len() >= 3 {
                let continent = &captures[1];
                let location = captures[2].replace("_", " ");
                return format!("{}, {}", location, continent);
            }
        }
    }
    
    // Fallback for when regex fails or timezone format is unexpected
    format!("Unknown location (Timezone: {})", timezone)
} 