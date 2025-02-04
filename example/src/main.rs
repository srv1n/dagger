use anyhow::{anyhow, Error, Result};

use dagger::any::downcast;
use dagger::any::DynAny;

use dagger::insert_value;

use dagger::parse_input_from_name;
use dagger::register_action;
use dagger::serialize_cache_to_json;
use dagger::serialize_cache_to_prettyjson;
use dagger::Cache;
use dagger::DagExecutionReport;
use dagger::DagExecutor;

use dagger::{Convertible, Node, NodeAction};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::oneshot;
use tracing::Level;
use tracing::{debug, error, info, trace, warn};
use tracing_subscriber::fmt;
use tracing_subscriber::FmtSubscriber;

// Example NodeAction Implementation
//
async fn function_to_call1(node: &Node, cache: &Cache) -> Result<()> {
    // let mut outputs = HashMap::new();
    // println!("Cache: {:#?}", cache);
    let num1: f64 = parse_input_from_name(cache, "num1".to_string(), &node.inputs)?;
    // .ok_or_else(|| anyhow!("Input '{}' not found.", node.inputs[0].name))?;
    let num2: f64 = parse_input_from_name(cache, "num2".to_string(), &node.inputs)?;

    // Perform the operation with the retrieved and converted inputs
    let new = num1 + num2;

    insert_value(cache, &node.id, &node.outputs[0].name, new);

    insert_value(cache, &node.id, "Gaandu", vec!["Resident", "Alien"]);

    insert_value(cache, &node.id, "bandu", vec![1, 2, 3, 4]);

    // Insert the result into the outputs
    // outputs.insert(node.outputs[0].clone().name, new);

    Ok(())
}

async fn function_to_call2(node: &Node, cache: &Cache) -> Result<()> {
    let result: f64 = parse_input_from_name(cache, "result".to_string(), &node.inputs)?;

    // Perform the operation
    insert_value(cache, &node.id, &node.outputs[0].name, result * result);
    // outputs.insert("squared_result".to_string(), num * num);
    Ok(())
}

async fn function_to_call3(node: &Node, cache: &Cache) -> Result<(), Error> {
    let num: f64 = parse_input_from_name(cache, "result".to_string(), &node.inputs)?;
    println!("num from parse_input: {:#?}", num);

    let numm: f64 = parse_input_from_name(cache, "result".to_string(), &node.inputs)?;
    println!("num from parse_input_from_name: {:#?}", numm);

    insert_value(cache, &node.id, &node.outputs[0].name, num * 3.0);

    insert_value(cache, &node.id, &node.outputs[1].name, "Testo".to_string());

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
    executor.load_yaml_file("pipeline.yaml");
    executor.load_yaml_dir("./dags");

    let names = executor.list_dags();
    println!("Loaded DAGs: {:#?}", names);
    let filter_tags = executor.list_dag_filtered_tag("math");
    println!("Filtered DAGs: {:#?}", filter_tags);
    let multiple_filter_tags = executor.list_dag_multiple_tags(vec!["math".to_string()]);
    println!("Multiple Filtered DAGs: {:#?}", multiple_filter_tags);
    // let mut temp = HashMap::new();
    // temp.insert("num1".to_string(), DataValue::Float(10.0));
    // temp.insert("num2".to_string(), DataValue::Float(20.0));

    // let mut inputs = HashMap::new();
    // inputs.insert("inputs".to_string(), temp);
    let cache = Cache::new(HashMap::new());
    insert_value(&cache, "inputs", "num1", 10.0);

    // insert_value(&cache, "inputs", "num2", 20.0);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let dag_report = run_dag(executor, "infolder", &cache, cancel_rx).await?;

    let result = cache.read().unwrap();
    // println!("{:#?}", result);

    let jsson = serialize_cache_to_prettyjson(&cache).map_err(|e| e.to_string())?;

    println!("{}", jsson);
    println!("DAG Report: {:#?}", dag_report);

    // can you pretty print the json above?

    // let pretty_json = serde_json::to_string_pretty(&jsson)?;

    // println!("{:#?}", pretty_json);
    // println!("{:#?}", jsson);
    // let test: f64 = get_value::<f64>(&cache, "node3", "doubled_result").unwrap();
    // println!("{:#?}", test);

    // let serializable_cache = SerializableCache(cache);
    // let shata = serializable_cache.to_json().map_err(|e| e.to_string());

    // println!("{:#?}", shata);

    Ok(())
}

async fn run_dag(
    executor: DagExecutor,
    name: &str,
    cache: &Cache,
    cancel_rx: oneshot::Receiver<()>,
) -> Result<DagExecutionReport, Error> {
    let updated_inputs = executor.execute_dag(name, &cache, cancel_rx).await?;
    Ok(updated_inputs)
}