//! Workflow state and reusable read-only GitHub access for Review Platform.

pub mod github;
mod model;
mod state;

pub use model::{
    Authorization, AuthorizationScope, CommentFingerprint, CommentHash, CommentSource, HeadSha,
    InputRevision, ObservedDiscussion, PrKey, StateError,
};
pub use state::{RecordOutcome, RefreshOutcome, ReviewRequest, State};
