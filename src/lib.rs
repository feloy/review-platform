//! Workflow state and reusable read-only GitHub access for Review Platform.

pub mod github;
mod model;
mod state;

pub use model::{Authorization, AuthorizationScope, HeadSha, PrKey, StateError};
pub use state::{RecordOutcome, ReviewRequest, State};
