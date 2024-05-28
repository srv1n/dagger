use anyhow::{anyhow, Error, Result};

use dagger::any::downcast;
use dagger::any::DynAny;
use dagger::get_input_values;
use dagger::get_value;
use dagger::insert_value;
use dagger::parse_input;
use dagger::register_action;
use dagger::Cache;
use dagger::DagExecutor;
use dagger::{Convertible, Node, NodeAction};
use std::collections::HashMap;
use tracing::Level;
use tracing::{debug, error, info, trace, warn};
use tracing_subscriber::fmt;
use tracing_subscriber::FmtSubscriber;

use std::sync::Arc;

// Example NodeAction Implementation
//
async fn function_to_call1(node: &Node, cache: &Cache) -> Result<()> {
    // let mut outputs = HashMap::new();
    // println!("Cache: {:#?}", cache);
    let num1 = get_value::<f64>(cache, "inputs", "num1")
        .ok_or_else(|| anyhow!("Input '{}' not found.", node.inputs[0].name))?;
    // .ok_or_else(|| anyhow!("Input '{}' not found.", node.inputs[0].name))?;
    let num2 = get_value::<f64>(cache, "inputs", &node.inputs[1].name)
        .ok_or_else(|| anyhow!("Input '{}' not found.", node.inputs[1].name))?;

    // Perform the operation with the retrieved and converted inputs
    let new = num1 + num2;

    insert_value(cache, node.id.clone(), node.outputs[0].clone().name, new);

    // Insert the result into the outputs
    // outputs.insert(node.outputs[0].clone().name, new);

    Ok(())
}

async fn function_to_call2(node: &Node, cache: &Cache) -> Result<()> {
    let result: f64 = parse_input(cache, node.inputs[0].clone())?;

    // Perform the operation
    insert_value(
        cache,
        node.id.clone(),
        node.outputs[0].clone().name,
        result * result,
    );
    // outputs.insert("squared_result".to_string(), num * num);
    Ok(())
}

async fn function_to_call3(node: &Node, cache: &Cache) -> Result<(), Error> {
    let num: f64 = parse_input(cache, node.inputs[0].clone())?;

    insert_value(
        cache,
        node.id.clone(),
        node.outputs[0].name.clone(),
        num * 3.0,
    );

    insert_value(
        cache,
        node.id.clone(),
        node.outputs[1].name.clone(),
        "Testo".to_string(),
    );

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        // all spans/events with a level higher than TRACE (e.g, debug, info, warn, etc.)
        // will be written to stdout.
        .with_max_level(Level::TRACE)
        // completes the builder.
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let mut executor = DagExecutor::new();

    register_action!(executor, "function_to_call1", function_to_call1);
    register_action!(executor, "function_to_call2", function_to_call2);
    register_action!(executor, "function_to_call3", function_to_call3);
    executor.load_yaml_file("pipeline.yaml")?;

    let names = executor.list_dags();
    println!("Loaded DAGs: {:#?}", names);
    let filter_tags = executor.list_dag_filtered_tag("maths");
    println!("Filtered DAGs: {:#?}", filter_tags);
    // let mut temp = HashMap::new();
    // temp.insert("num1".to_string(), DataValue::Float(10.0));
    // temp.insert("num2".to_string(), DataValue::Float(20.0));

    // let mut inputs = HashMap::new();
    // inputs.insert("inputs".to_string(), temp);
    let cache = Cache::new(HashMap::new());
    insert_value(&cache, "inputs".to_string(), "num1".to_string(), 10.0);

    insert_value(&cache, "inputs".to_string(), "num2".to_string(), 20.0);
    let _ = run_dag(executor, "example", &cache).await?;
    let result = cache.read().unwrap();
    println!("{:#?}", result);
    let test: f64 = get_value::<f64>(&cache, "node3", "doubled_result").unwrap();
    println!("{:#?}", test);

    Ok(())
}

async fn run_dag(executor: DagExecutor, name: &str, cache: &Cache) -> Result<(), Error> {
    let updated_inputs = executor.execute_dag(name, &cache).await?;
    Ok(())
}
