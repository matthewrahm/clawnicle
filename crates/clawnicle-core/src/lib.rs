mod error;
mod event;

pub use error::{Error, Result};
pub use event::{Event, EventPayload};

pub type WorkflowId = String;
