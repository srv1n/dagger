use anyhow::anyhow;
use anyhow::{Context, Error, Result};

use dagger::register_action;
use dagger::DagExecutor;
use dagger::{Cache, Convertible, Node, NodeAction, Value};

use petgraph::graph::{DiGraph, NodeIndex};

use std::collections::HashMap;

use std::sync::Arc;

// Example NodeAction Implementation
//

async fn function_to_call1(node: &Node, inputs: &Cache) -> Result<Cache, Error> {
    let mut outputs = Cache::new();
    // Example operation: sum two inputs

    let num1: f64 = Convertible::from_value(inputs.get(&node.inputs[0].name).unwrap()).unwrap();
    let num2: f64 = Convertible::from_value(inputs.get(&node.inputs[1].name).unwrap()).unwrap();

    let new = num1 + num2;

    outputs.insert(node.outputs[0].clone().name, Value::Float(new));
    if let (Some(Value::Float(num1)), Some(Value::Float(num2))) =
        (inputs.get("num1"), inputs.get("num2"))
    {
        outputs.insert("sum".to_string(), Value::Float(num1 + num2));
    }
    Ok(outputs)
}

async fn function_to_call2(node: &Node, inputs: &Cache) -> Result<Cache, Error> {
    let mut outputs = Cache::new();
    // Example operation: square the input

    if let Some(Value::Float(num)) = inputs.get(node.inputs[0].reference.as_ref().unwrap()) {
        outputs.insert("squared_result".to_string(), Value::Float(num * num));
    }
    Ok(outputs)
}

async fn function_to_call3(node: &Node, inputs: &Cache) -> Result<Cache, Error> {
    let mut outputs = Cache::new();
    // Example operation: double the input

    if let Some(input_key) = node.inputs.get(0).and_then(|input| input.reference.clone()) {
        if let Some(result) = inputs.get(&input_key) {
            outputs.insert("doubled_result".to_string(), Value::Integer(2));
        }
    }
    Ok(outputs)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut executor = DagExecutor::new();

    register_action!(executor, "function_to_call1", function_to_call1);
    register_action!(executor, "function_to_call2", function_to_call2);
    register_action!(executor, "function_to_call3", function_to_call3);
    let graph = executor.load_yaml_file("pipeline.yaml")?;
    let (dag, node_indices) = executor.build_dag()?;

    let mut inputs = Cache::new();
    inputs.insert("num1".to_string(), Value::Float(10.0));
    inputs.insert("num2".to_string(), Value::Float(20.0));

    let result = run_dag(executor, dag, node_indices, inputs).await?;

    println!("{:?}", result);

    Ok(())
}

async fn run_dag(
    executor: DagExecutor,
    dag: DiGraph<Node, ()>,
    node_indices: HashMap<String, NodeIndex>,
    inputs: Cache,
) -> Result<Cache, Error> {
    let updated_inputs = executor.execute_dag(&dag, &node_indices, &inputs).await?;
    Ok(updated_inputs)
}
