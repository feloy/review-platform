//! Workflow state, model and agent configuration, and read-only adapters.

pub mod archive;
pub mod config;
pub mod github;
mod model;
pub mod preparation;
mod state;

pub use model::{
    Authorization, AuthorizationScope, CommentFingerprint, CommentHash, CommentSource, HeadSha,
    InputRevision, ObservedDiscussion, PrKey, StateError,
};
pub use state::{
    ModelValidation, ModelValidationError, RecordOutcome, RefreshOutcome, ReviewRequest, State,
};
