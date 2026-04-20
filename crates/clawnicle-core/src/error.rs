use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("journal error: {0}")]
    Journal(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("tool failed: {0}")]
    Tool(String),

    #[error("workflow not found: {0}")]
    WorkflowNotFound(String),
}

pub type Result<T> = std::result::Result<T, Error>;
