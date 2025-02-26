# PubSub Agent Detailed Documentation

This document provides an in-depth review of the design and implementation of the PubSub agent within the Dagger library. It explains the evolution from an earlier, simpler design to the current hybrid model with task tracking, schema validation, and extensive macro automation. In addition, it details the two main procedural macros—`#[action]` and `#[pubsub_agent]`—that drive agent creation and simplify boilerplate code.

---

## Table of Contents

1. [Overview](#overview)
2. [Design Evolution](#design-evolution)
3. [Core Components and Architecture](#core-components-and-architecture)
    - [PubSubAgent Trait](#pubsubagent-trait)
    - [PubSubExecutor](#pubsubexecutor)
    - [Task Management](#task-management)
4. [Macros for Agent and Action Definition](#macros-for-agent-and-action-definition)
    - [Action Macro (#[action])](#action-macro-action)
    - [PubSub Agent Macro (#[pubsub_agent])](#pubsub-agent-macro-pubsub_agent)
5. [Usage Example](#usage-example)
6. [Validation and Schema Generation](#validation-and-schema-generation)
7. [Additional Notes and Best Practices](#additional-notes-and-best-practices)
8. [Summary](#summary)

---

## Overview

The Dagger library is designed to build multi-agent systems using a publish/subscribe (pub/sub) paradigm. Agents subscribe to a set of channels and publish messages to one or more channels. The system is built as a hybrid model with optional task tracking. In this model, each message can reference a **task_id**, and tasks are updated automatically based on whether an agent successfully processed the message.

The key improvements from the original design include:
- **Dynamic Agent Behavior:** Agents are not locked to fixed task types and can publish on multiple channels.
- **Automated Task Tracking:** The PubSubExecutor and TaskManager work together to handle task status updates (e.g., Pending, InProgress, Completed, Failed) automatically.
- **Schema Validation:** Both input and output schemas are validated to ensure message integrity.
- **Macro Automation:** The use of procedural macros (#[pubsub_agent] and #[action]) significantly reduces boilerplate code and enforces consistency across agents.

---

## Design Evolution

### Original Design Spec
In the original design, agents were part of a pure pub/sub system where:
- Agents subscribed to channels and published messages.
- There was no built-in mechanism to track task progress or enforce schema validation.
- The pub/sub model was flexible but lacked structure for monitoring execution.

### Current Working Model
The updated design introduces:
- **Hybrid Model:** Task IDs can optionally be provided to track message processing as tasks.
- **Automatic Schema Validation:** Using JSON schemas, both incoming and outgoing messages are validated.
- **Macro-Driven Agent Creation:** The `#[pubsub_agent]` macro transforms a simple asynchronous function into a fully compliant PubSub agent that implements the `PubSubAgent` trait.
- **Execution Tracing:** Each agent’s processing is recorded via a structured execution tree, aiding debugging and workflow visualization.

---

## Core Components and Architecture

### PubSubAgent Trait

Every agent in the system must implement the `PubSubAgent` trait. This trait defines the following methods:

- **name(&self) -> String:**  
  Returns the agent’s unique name.
  
- **subscriptions(&self) -> Vec<String>:**  
  Lists the channels to which the agent subscribes.

- **publications(&self) -> Vec<String>:**  
  Lists the channels to which the agent publishes.
  
- **input_schema(&self) -> Value and output_schema(&self) -> Value:**  
  Provide the JSON schema definitions for validating messages.
  
- **process_message(...):**  
  This is the core asynchronous method that processes an incoming message. It handles:
  - Recording the agent’s node in the execution tree.
  - Validating the message payload against the input schema.
  - Handling task status updates if a task_id is present.
  - Publishing messages or performing logic as defined in the user’s function.

### PubSubExecutor

The `PubSubExecutor` orchestrates the entire workflow:

- **Channel Management:**  
  Dynamically creates, manages, and cleans up channels. It also updates subscriber counts and persistent metadata in a sled database.
  
- **TaskManager Integration:**  
  Uses the TaskManager for creating, claiming, and completing tasks associating them with processes.
  
- **Message Publishing:**  
  Incorporates schema validation and automatic task creation when publishing messages.

### Task Management

Tasks allow for structured execution tracking and include states like _Pending_, _InProgress_, _Completed_, and _Failed_. Tasks are automatically updated during:
- Message publication (a task is created or updated).
- When an agent claims a task.
- When processing is complete, the task’s status is set based on the success of the operation.

---

## Macros for Agent and Action Definition

Macros greatly simplify the creation of agents and actions by automating routine tasks.

### Action Macro (#[action])

- **Purpose:**  
  Transform a user-defined asynchronous function into an action that can be executed within a DAG-like workflow.
  
- **Key Features:**
  - Extracts and validates function parameters.
  - Generates a JSON schema for both the parameters and the return type.
  - Wraps the function into a structure that implements the `NodeAction` trait.
  - Example usage:
  
  ```rust
  #[action(description = "Processes input and returns a computed result")]
  async fn compute(
      _executor: &mut DagExecutor,
      node: &Node,
      cache: &Cache,
      input: String
  ) -> Result<String> {
      Ok(format!("Computed: {}", input))
  }
  ```

### PubSub Agent Macro (#[pubsub_agent])

- **Purpose:**  
  Converts a regular asynchronous function into a complete PubSub agent that implements the `PubSubAgent` trait.
  
- **Attributes:**
  - **name:** Unique identifier for the agent (required).
  - **description:** A short description of the agent’s functionality.
  - **subscribe:** Comma-separated channels where the agent listens.
  - **publish:** Comma-separated channels where the agent can publish messages.
  - **input_schema:** A JSON string to validate incoming messages.
  - **output_schema:** A JSON string to validate outgoing messages.

- **Macro Internals:**
  - **Parameter Verification:**  
    Ensures the function includes the required parameters: `node_id: &str`, `channel: &str`, `message: Message`, `executor: &mut PubSubExecutor`, and `cache: &Cache`.
  
  - **Schema Generation:**  
    It automatically maps Rust function parameter types and return types to JSON schema types. Optional types (e.g., `Option<T>`) are handled correctly.
  
  - **Execution Wrapping:**  
    The macro generates a new struct for the agent (e.g., `__TaskProcessorAgent`) that implements the `PubSubAgent` trait with:
    - A `name` method that returns the agent’s name.
    - A `process_message` method that invokes the original function and integrates error handling, task updates, and execution tracing.
  
  - **Example Usage:**
  
    ```rust
    #[pubsub_agent(
        name = "TaskProcessor",
        description = "Processes tasks and publishes results",
        subscribe = "tasks",
        publish = "results",
        input_schema = r#"{"type": "object", "properties": {"task": {"type": "string"}}}"#,
        output_schema = r#"{"type": "object", "properties": {"result": {"type": "string"}}}"#
    )]
    async fn task_processor(
        node_id: &str,
        channel: &str,
        message: Message,
        executor: &mut PubSubExecutor,
        cache: &Cache
    ) -> Result<()> {
        let task = message.payload["task"].as_str().ok_or(anyhow!("Missing task"))?;
        let result_msg = Message::new(node_id.to_string(), json!({
            "result": format!("Processed: {}", task)
        }));
        executor.publish("results", result_msg, cache, None).await?;
        Ok(())
    }
    ```

  In this example, the macro:
    - Validates the function signature for required parameters.
    - Generates a JSON schema using the provided input and output schema strings.
    - Wraps the function so that when a message arrives on the "tasks" channel, it validates the payload, processes the task, and then may publish results to the "results" channel.
  
---

## Usage Example

By leveraging the `#[pubsub_agent]` macro, developers merely need to focus on the core logic. For instance, consider the following agent:

```rust
#[pubsub_agent(
    name = "ReviewAgent",
    description = "Examines messages for incomplete answers and triggers gap questions",
    subscribe = "intermediate_answers",
    publish = "gap_questions, search_queries",
    input_schema = r#"{"type": "object", "properties": {"intermediate_answer": {"type": "string"}}}"#,
    output_schema = r#"{"type": "null"}"#
)]
async fn review_agent(
    node_id: &str,
    channel: &str,
    message: Message,
    executor: &mut PubSubExecutor,
    cache: &Cache
) -> Result<()> {
    let answer = message.payload["intermediate_answer"].as_str().ok_or(anyhow!("Missing answer"))?;
    if answer.contains("incomplete") {
        executor.publish_message(
            node_id,
            "gap_questions",
            None,
            Some("plan"),
            json!({"query": "fill gaps"}),
            executor,
            cache,
        ).await?;
        executor.publish_message(
            node_id,
            "search_queries",
            None,
            Some("search"),
            json!({"query": "more data"}),
            executor,
            cache,
        ).await?;
    }
    Ok(())
}
```

Here, the agent:
- Subscribes to the `"intermediate_answers"` channel.
- Validates the incoming payload against a schema.
- Publishes to `"gap_questions"` and `"search_queries"` channels when the condition is met.
- Automatically updates task status and logs execution details.

---

## Validation and Schema Generation

The macros use Rust’s type system to construct JSON schemas dynamically:

- **Type Mapping:**  
  Types such as `String`, integers, floats, booleans, arrays (`Vec<T>`), and even optional types (`Option<T>`) are mapped to corresponding JSON schema definitions.
  
- **Input and Output Validation:**  
  Before executing the core logic of an agent, the `process_message` method calls `validate_input` (and later `validate_output` if necessary) ensuring that the message payload conforms to the declared schema. If validation fails, an error is returned immediately.

- **Compile-Time Checks:**  
  The macros validate that provided schema strings are valid JSON at compile time, reducing runtime errors and ensuring consistency throughout the project.

---

## Additional Notes and Best Practices

- **Parameter Naming:**  
  The agent function must include standard parameters (`node_id`, `channel`, `message`, `executor`, `cache`) exactly, to guarantee that the macro can generate the necessary call structure.
  
- **Error Handling:**  
  Make sure that detailed error messages are produced by propagating errors during schema validation or task updates.
  
- **Task Updating:**  
  When processing messages, always update task status. The macro-generated code wraps functions so that any changes to task state (from Pending to InProgress, and ultimately Completed or Failed) are handled automatically.
  
- **Execution Tree:**  
  Using an execution tree for tracing enhances debugging. Each agent invocation is logged with timestamps, node id, and outcome details.

---

## Summary

The current implementation of PubSub agents in the Dagger library demonstrates a robust hybrid design:
- **Flexibility:** Through dynamic pub/sub channels and non-restrictive task typology.
- **Reliability:** With built-in schema validation and execution tracing.
- **Ease of Development:** Procedural macros (`#[pubsub_agent]` and `#[action]`) significantly reduce boilerplate and enforce consistency across agents.

This document should serve as a comprehensive reference for developers working on or extending the PubSub agent functionality within Dagger.

------------------------------------------------------------

Feel free to ask further questions or request refinements if needed!
