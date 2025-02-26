pub mod supervisor;
pub mod planning;
pub mod search;
pub mod reader;
pub mod reasoning;
pub mod evaluation;
pub mod report;
use crate::agents::supervisor::__supervisor_agentAgent;
use crate::agents::planning::__planning_agentAgent;
use crate::agents::search::__search_agentAgent;
use crate::agents::reader::__reader_agentAgent;
use crate::agents::reasoning::__reasoning_agentAgent;
use crate::agents::evaluation::__evaluation_agentAgent;
use crate::agents::report::__report_agentAgent;
use dagger::PubSubAgent;
use std::sync::Arc;

pub fn get_all_agents() -> Vec<Arc<dyn PubSubAgent>> {
    vec![
        Arc::new(__supervisor_agentAgent::new()),
        Arc::new(__planning_agentAgent::new()),
        Arc::new(__search_agentAgent::new()),
        Arc::new(__reader_agentAgent::new()),
        Arc::new(__reasoning_agentAgent::new()),
        Arc::new(__evaluation_agentAgent::new()),
        Arc::new(__report_agentAgent::new()),
    ]
}