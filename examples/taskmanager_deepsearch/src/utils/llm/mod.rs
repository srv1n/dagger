pub mod client;
pub mod chat_stream;
pub mod structured_outputs;
pub mod tool_call;

// Re-export commonly used items for convenience
pub use client::*;
pub use chat_stream::{stream_chat_completion, stream_chat_prompt};
pub use structured_outputs::{
    get_structured_output, 
    get_structured_output_typed,
    get_structured_output_with_template,
    get_structured_output_typed_with_template,
    get_structured_output_from_template
};
pub use tool_call::{Tool, ToolFunction, execute_with_tools};
