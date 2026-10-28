//! Workflow state, review preparation, and read-only adapters.

pub mod agent;
pub mod archive;
pub mod checkout;
pub mod config;
pub mod github;
mod model;
pub mod openshell;
pub mod preparation;
pub mod review;
pub mod round;
pub mod sandbox_layout;
mod state;

pub use model::{
    Authorization, AuthorizationScope, CommentFingerprint, CommentHash, CommentSource, HeadSha,
    InputRevision, ObservedDiscussion, PrKey, StateError,
};
pub use state::{
    ModelValidation, ModelValidationError, RecordOutcome, RefreshOutcome, ReviewRequest, State,
};
