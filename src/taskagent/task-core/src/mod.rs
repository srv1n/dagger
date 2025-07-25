pub mod model;
pub mod storage;

pub use model::{Task, TaskOutput, TaskStatus, TaskType};
pub use storage::{Storage, SledStorage, JobMetadata, JobStatus};