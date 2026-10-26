//! Workflow state for the local operator prototype.

mod model;
mod state;

pub use model::{Authorization, PrKey, StateError};
pub use state::{RecordOutcome, ReviewRequest, State};
