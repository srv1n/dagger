use anyhow::{anyhow, Error, Result};

use dagger::register_action;
use dagger::DagExecutor;
use dagger::{Cache, Convertible, Node, NodeAction, Value};
use std::sync::Arc;

// Example NodeAction Implementation
//
async fn function_to_call1(node: &Node, inputs: &Cache) -> Result<Cache, Error> {
    let mut outputs = Cache::new();
    let num1: f64 = Convertible::from_value(
        inputs
            .get(&node.inputs[0].name)
            .ok_or_else(|| anyhow!("Input '{}' not found.", node.inputs[0].name))?,
    )
    .ok_or_else(|| anyhow!("Failed to convert input '{}' to f64.", node.inputs[0].name))?;

    let num2: f64 = Convertible::from_value(
        inputs
            .get(&node.inputs[1].name)
            .ok_or_else(|| anyhow!("Input '{}' not found.", node.inputs[1].name))?,
    )
    .ok_or_else(|| anyhow!("Failed to convert input '{}' to f64.", node.inputs[1].name))?;

    // Perform the operation with the retrieved and converted inputs
    let new = num1 + num2;

    // Insert the result into the outputs
    // outputs.insert(node.outputs[0].clone().name, Value::Float(new));

    Ok(outputs)
}
use anyhow::Context;

async fn function_to_call2(node: &Node, inputs: &Cache) -> Result<Cache, anyhow::Error> {
    let mut outputs = Cache::new();
    // Example operation: square the input

    if let Some(Value::Float(num)) = inputs.get(node.inputs[0].reference.as_ref().unwrap()) {
        outputs.insert("squared_result2".to_string(), Value::Float(num * num));
        Ok(outputs)
    } else {
        Err(anyhow::anyhow!("Failed to get input for squaring"))
    }
}

async fn function_to_call3(node: &Node, inputs: &Cache) -> Result<Cache, anyhow::Error> {
    let mut outputs = Cache::new();
    // Example operation: double the input

    if let Some(input_key) = node.inputs.get(0).and_then(|input| input.reference.clone()) {
        if let Some(result) = inputs.get(&input_key) {
            outputs.insert(
                "doubled_result".to_string(),
                Value::Integer(Convertible::from_value(result).context(format!(
                    "Failed to convert input '{}' to f64.",
                    node.inputs[0].name
                ))?),
            );
            Ok(outputs)
        } else {
            Err(anyhow::anyhow!("Failed to get input for doubling"))
        }
    } else {
        Err(anyhow::anyhow!("Failed to get input reference"))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut executor = DagExecutor::new();

    register_action!(executor, "function_to_call1", function_to_call1);
    register_action!(executor, "function_to_call2", function_to_call2);
    register_action!(executor, "function_to_call3", function_to_call3);
    executor.load_yaml_file("pipeline.yaml")?;

    let mut inputs = Cache::new();
    inputs.insert("num1".to_string(), Value::Float(10.0));
    inputs.insert("num2".to_string(), Value::Float(20.0));

    let result = run_dag(executor, "example", inputs).await?;

    println!("{:?}", result);

    Ok(())
}

async fn run_dag(executor: DagExecutor, name: &str, inputs: Cache) -> Result<Cache, Error> {
    let updated_inputs = executor.execute_dag(name, &inputs).await?;
    Ok(updated_inputs)
}
