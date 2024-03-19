# Dagger - A library for executing directed acyclic graphs (DAGs) with custom actions

Dagger is a Rust library that provides a way to define and execute directed acyclic graphs (DAGs) with custom actions. It supports loading graph definitions from YAML files, validating the graph structure, and executing custom actions associated with each node in the graph.

## Features

* Define and execute DAGs with custom actions
* Load graph definitions from YAML files
* Validate the graph structure
* Execute custom actions associated with each node in the graph

## Usage

To use Dagger, add the following to your `Cargo.toml`:
```
[dependencies]
dagger = { git = "https://github.com/srv1n/dagger" }
```
Then, import the library in your Rust code:
```
use dagger::DagExecutor;
```
To register an action with the `DagExecutor`, use the `register_action!` macro:
```
#[macro_use]
extern crate dagger;

use dagger::register_action;
use dagger::DagExecutor;
use dagger::NodeAction;

struct MyAction;

#[async_trait::async_trait]
impl NodeAction for MyAction {
    fn name(&self) -> String {
        "my_action".to_string()
    }

    async fn execute(&self, node: &Node, inputs: &Cache) -> Result<Cache> {
        // Implementation of the action
    }
}

let executor = DagExecutor::new();
register_action!(executor, "My Action", MyAction);
```
For more examples and usage, see the `examples/src/main.rs` file.
