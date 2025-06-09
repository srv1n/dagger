pub mod taskagent;
// pub mod enhanced_taskagent;
pub mod registry;
pub mod errors;
pub mod taskagent_builder;

// Re-export all the key structs and functions
pub use taskagent::*;
pub use taskagent_builder::*;
// pub use enhanced_taskagent::{
//     EnhancedTaskAgent, EnhancedTaskManager, ExampleEnhancedAgent
// };