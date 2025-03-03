1. Entry Point: src/main.rs
File: src/main.rs
Function: #[tokio::main] async fn main()
Role: Initializes the system, sets up the PubSubExecutor, registers agents, starts the workflow, and monitors outcomes.
Flow:
Creates a Cache (Arc<Cache>).
Initializes PubSubExecutor with no config, a sled DB path ("pubsub_db"), and the cache.
Registers all agents from get_all_agents() (defined in src/agents/mod.rs).
Executes the workflow with an initial "start" message using PubSubWorkflowSpec::EventDriven.
Monitors outcomes via outcome_rx with a 30-second timeout.
Stops the executor and prints the execution tree and cache.
2. TaskManager
File: src/lib.rs (via dagger/pubsubagent.rs)
Struct: TaskManager (inside PubSubExecutor)
Role: Manages a queue of tasks (Arc<RwLock<Vec<Task>>>) with states (Pending, InProgress, Completed, Failed).
Key Methods:
claim_task: Picks a pending task for an agent.
complete_task: Updates task status.
create_or_update_task: Adds or updates tasks (used by publish when a task is specified).
Usage in Your Code: Not directly leveraged by your current execute-based flow. It’s populated by agents (e.g., SupervisorAgent via publish with a task) but only processed by start(), which you’re not using.
3. Publish
File: src/lib.rs (via dagger/pubsubagent.rs)
Method: PubSubExecutor::publish
Signature: 
rust
pub async fn publish(&self, channel: &str, mut message: Message, cache: &Cache, task: Option<(String, Value)>) -> Result<String, PubSubError>
Role: Sends a Message to a channel, optionally creating/updating a task in TaskManager.
Flow:
Ensures the channel exists.
Updates message.channel and message.source.
If task is provided, creates/updates a task in TaskManager.
Broadcasts the message via async_broadcast::Sender.
Usage: Called by agents (e.g., SupervisorAgent publishes to "initial_query") and main (publishes to "start").
4. Execute
File: src/lib.rs (via dagger/pubsubagent.rs)
Method: PubSubExecutor::execute
Signature: 
rust
pub async fn execute(&mut self, spec: PubSubWorkflowSpec, cancel_rx: oneshot::Receiver<()>) -> Result<(PubSubExecutionReport, mpsc::Receiver<NodeExecutionOutcome>), PubSubError>
Role: Starts the pub/sub workflow by publishing an initial message and setting up channel listeners for all agents.
Flow:
Publishes the initial message (e.g., to "start").
Creates an mpsc::channel for outcomes (outcome_tx, outcome_rx).
Spawns tasks for each agent’s subscriptions, listening to channel receivers (async_broadcast::Receiver).
Processes messages, updates the execution tree, and sends outcomes to outcome_rx.
Usage: Called in main to kick off the workflow.
5. OutcomeRx
Type: mpsc::Receiver<NodeExecutionOutcome>
Role: Receives execution outcomes (success/failure, errors) from agents as they process messages.
Structure: NodeExecutionOutcome (from dagger):
rust
pub struct NodeExecutionOutcome {
    pub node_id: String,
    pub success: bool,
    pub retry_messages: Vec<String>,
    pub final_error: Option<String>,
}
Usage: Monitored in main to track workflow progress and detect completion or failure.
6. Agents
Files: src/agents/*.rs
Role: Implement PubSubAgent via #[pubsub_agent] macro, processing messages and publishing results.
Examples:
SupervisorAgent: Starts the workflow, monitors completion, publishes "initial_query" and "report_request".
PlanningAgent: Expands queries, publishes "search_queries".
EvaluationAgent: Evaluates answers, publishes "evaluation_results", "gap_questions", "task_completed".
7. Cache
Type: Arc<Cache> (where Cache is RwLock<HashMap<String, HashMap<String, Value>>>)
Role: Shared state for agents to store tasks (task_queue), knowledge (knowledge_base), and counters (planned_tasks, completed_tasks).
Usage: Accessed via helper functions (insert_global_value, get_global_input, append_global_value) from src/utils/memory.rs.


[main.rs]
   |
   +--> Creates Cache (Arc<Cache>)
   |
   +--> Initializes PubSubExecutor (sled_db: "pubsub_db", cache)
   |     |
   |     +--> TaskManager (Arc<RwLock<Vec<Task>>>)
   |     +--> Channels (Arc<RwLock<HashMap<String, (Sender, Receiver)>>)
   |     +--> Agent Registry (Arc<RwLock<HashMap<String, AgentEntry>>>)
   |
   +--> Registers Agents (get_all_agents())
   |     |
   |     +--> SupervisorAgent (sub: "start", pub: "initial_query")
   |     +--> PlanningAgent (sub: "initial_query", pub: "search_queries")
   |     +--> SearchAgent (sub: "search_queries", pub: "search_results")
   |     +--> ReaderAgent (sub: "search_results", pub: "page_content", "knowledge")
   |     +--> ReasoningAgent (sub: "knowledge", pub: "intermediate_answers")
   |     +--> EvaluationAgent (sub: "intermediate_answers", pub: "evaluation_results", "task_completed")
   |     +--> ReportAgent (sub: "report_request", pub: "final_answer")
   |
   +--> execute(PubSubWorkflowSpec::EventDriven { channel: "start", initial_message })
   |     |
   |     +--> publish("start", initial_message)  // Publishes to "start" channel
   |     |     |
   |     |     +--> SupervisorAgent listens
   |     |           |
   |     |           +--> Initializes cache (task_queue, knowledge_base, etc.)
   |     |           +--> publish("initial_query")
   |     |                  |
   |     |                  +--> PlanningAgent listens
   |     |                        |
   |     |                        +--> Expands queries, updates task_queue
   |     |                        +--> publish("search_queries")
   |     |                              |
   |     |                              +--> SearchAgent listens
   |     |                                    |
   |     |                                    +--> Performs search, updates task_queue
   |     |                                    +--> publish("search_results")
   |     |                                          |
   |     |                                          +--> ReaderAgent listens
   |     |                                                |
   |     |                                                +--> Reads content, updates knowledge_base
   |     |                                                +--> publish("page_content", "knowledge")
   |     |                                                      |
   |     |                                                      +--> ReasoningAgent listens ("knowledge")
   |     |                                                            |
   |     |                                                            +--> Generates answer, updates task_queue
   |     |                                                            +--> publish("intermediate_answers")
   |     |                                                                  |
   |     |                                                                  +--> EvaluationAgent listens
   |     |                                                                        |
   |     |                                                                        +--> Evaluates answer, updates task_queue
   |     |                                                                        +--> publish("evaluation_results", "task_completed")
   |     |                                                                              |
   |     |                                                                              +--> SupervisorAgent listens ("task_completed")
   |     |                                                                                    |
   |     |                                                                                    +--> Checks completion, publish("report_request")
   |     |                                                                                          |
   |     |                                                                                          +--> ReportAgent listens
   |     |                                                                                                |
   |     |                                                                                                +--> Generates report
   |     |                                                                                                +--> publish("final_answer")
   |     |                                                                                                      |
   |     |                                                                                                      +--> SupervisorAgent listens ("final_answer")
   |     |                                                                                                            |
   |     |                                                                                                            +--> Stops executor
   |     |
   |     +--> Spawns channel listeners for each agent
   |     |     |
   |     |     +--> Processes messages from channels (e.g., "start" -> SupervisorAgent)
   |     |     +--> Sends outcomes to outcome_rx
   |     |
   |     +--> Returns (PubSubExecutionReport, outcome_rx)
   |
   +--> Monitors outcome_rx
   |     |
   |     +--> Logs outcomes (e.g., node_id, success)
   |     +--> Breaks on SupervisorAgent failure or timeout
   |
   +--> Stops executor
   +--> Prints DOT graph and cache



   Flow Explanation
Entry Point (main):
Starts with tokio::main, initializing tracing and the cache.
Creates PubSubExecutor, which sets up TaskManager, channels, and the agent registry.
Agent Registration:
get_all_agents() returns a Vec<Arc<dyn PubSubAgent>>, registering each agent with its subscriptions (e.g., SupervisorAgent to "start").
Execute Workflow:
execute publishes the initial "start" message and spawns tasks for each agent to listen to their subscribed channels.
Each agent’s process_message is called when a message arrives on its channel, triggered by publish.
Message Flow:
SupervisorAgent starts the chain, publishing "initial_query".
Agents (PlanningAgent -> SearchAgent -> ReaderAgent -> ReasoningAgent -> EvaluationAgent) process and publish messages sequentially.
SupervisorAgent monitors "task_completed", eventually triggering ReportAgent via "report_request".
TaskManager:
Populated indirectly via publish with tasks (e.g., "task_queue" updates), but not directly used since you’re using execute, not start().
Publish:
Used by agents and main to send messages, driving the workflow forward.
OutcomeRx:
Receives NodeExecutionOutcome from execute’s channel listeners, allowing main to track agent success/failure.
Cache:
Shared state updated by agents (e.g., task_queue grows with pending tasks, knowledge_base with results).
