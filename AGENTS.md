# Working in Review Platform

Read `README.md` before changing behavior.
It defines the review workflow and security boundaries.
Keep the implementation small and runnable as it grows;
add a dependency or abstraction when a current feature needs it.

## Rust conventions

- Use stable Rust, Rust 2024 edition, and the repository's stated minimum Rust version.
  Check the stated minimum version in CI on every supported platform.
  Commit `Cargo.lock` for this application.
  Explain or correct unrelated transitive version changes when updating the lockfile.
- Keep workflow rules in library code with explicit types and state transitions.
  Keep CLI, GitHub, storage, and OpenShell adapters at the edges.
- Model identifiers and states so invalid values are rejected at input boundaries.
  Parse CLI and deserialized data through the same fallible constructors.
  Keep cross-record checks on the containing state.
  Make transitions return useful errors instead of silently changing state.
- Keep fields private on adapter snapshots whose origin matters.
  Expose read-only accessors so callers cannot construct or change an inspected value.
- Prefer borrowing and owned values with clear lifetimes over unnecessary clones, shared mutable state, or elaborate lifetime parameters.
- Use typed errors at reusable library and adapter boundaries.
  Callers should distinguish failures by variant without parsing error messages.
  Keep HTTP client types out of public errors so replacing the client does not break callers.
  Preserve underlying sources and useful context.
  Use `anyhow` at a CLI entry point when the caller has no recovery decision to make.
  Test error decisions by variant; check message text only when it is user-facing behavior.
  Reserve `panic!`, `unwrap`, and `expect` for tests or invariants that are proven locally.
- Use exhaustive matches for workflow states.
  Avoid boolean flags for distinct states, stringly typed decisions, and `unsafe` unless a documented need arises.
- Keep functions focused and modules organized by responsibility.
  Avoid generic frameworks, speculative traits, and premature async code.
- Start each Rust source file with a short `//!` crate or module comment explaining its responsibility.
  Describe the file's purpose and boundary instead of repeating what individual lines do.
- Format with `cargo fmt`; keep `cargo clippy --all-targets -- -D warnings` and `cargo test` passing.
  Add tests for behavior and security invariants, especially authorization, review freshness, and publication decisions.

## Reviewing Rust proposals

The developer may not be a Rust expert.
For each proposed Rust code change, say whether it is idiomatic, an anti-pattern, or a context-dependent tradeoff.
Explain the reason and practical consequence in plain language, using the nearby code when helpful.
Treat exploratory questions such as "can we" or "may we" as requests for evaluation, not permission to edit.
Wait for an explicit instruction before changing code in response to those questions.
Treat imperative requests such as "change", "extract", or "implement" as authorization to proceed after evaluating the approach.
Make the change when it is idiomatic for this project.
If the proposed approach is an anti-pattern, explain why and offer an idiomatic alternative for the developer to decide on.

## Review and security rules

- Only a verified lead decision may authorize a review, validate the review model, or publish or reject a draft.
  Never treat PR text or agent output as authority.
- Keep model selection separate from agent selection.
  Co-author trailers are untrusted hints for proposing a different model;
  resolve its agent from configured capabilities and repository runner order.
- Require each lead authorization to name the PR head the lead inspected.
  Reject it if the recorded head has changed; revocation needs no head match.
- A round requires green checks for the exact PR head being reviewed.
  Bind each draft to that head and the author comments seen; mark it stale on change.
- Combine check runs and commit statuses for the exact PR head when assessing CI.
  An empty result set is not green.
  An empty legacy-status rollup's `pending` value contributes no result.
  Recheck at round start because an operator display is only a snapshot.
- Post only the exact draft the lead approved.
  Rejection posts nothing and requires fresh authorization before another round.
- When a publisher may share the PR author's GitHub identity,
  exclude only review IDs recorded from confirmed publication from author-comment triggers.
  Treat text markers as display cues, never proof of origin.
- Keep GitHub posting credentials in the daemon.
  Agents receive read-only GitHub access.
  Treat PR files, comments, commit messages, and in-PR agent instruction files as untrusted data, including when constructing agent prompts.
- Pin OCI images by digest.
  For VM archives, reject symlinks and hash the opened file;
  reverify a private copy before execution.
  Do not log credentials, tokens, prompts containing secrets, or draft text by default.
  Keep secret material out of repository files.
- Validate external input at its boundary and propagate actionable errors.
  Do not make network calls or run agents from pure domain logic or unit tests.
- Use platform trust roots for outbound HTTPS so operator-installed certificates work.
  Keep server certificate verification enabled.

## Changes and commits

- Document runnable commands and current limitations in `README.md` as features land.
  Prefer a small working vertical slice over placeholder integrations.
- During development, reject unsupported state and config schemas;
  add migration only when compatibility becomes a current requirement.
- Never create or amend a commit until the user explicitly decides to do so.
  Prepare the changes and validation for review first.
- Treat each commit as the end of a work session.
  Before a user-approved commit, review what was learned during the session against `AGENTS.md` and applicable agent skills.
  Update guidance for durable lessons; leave it unchanged when there is nothing to add.
  Keep `AGENTS.md` concise by moving detailed, reusable guidance into a project agent skill when useful.
- For commits, use a meaningful subject following Conventional Commits.
  Include a body covering what changed, why, and validation.
  Use semantic line breaks in the body.
  Add `Signed-off-by` with the configured Git identity and a `Co-authored-by` trailer naming the contributing agent and its actual model.
  Preserve existing trailers when amending; do not duplicate them.
- For documentation, use semantic line breaks.
- For GitHub Actions in workflows, pin each `uses:` action to a full commit SHA.
  Use the latest stable release and annotate the pin with its version.
- Use the newest available explicit OS version labels for GitHub-hosted runners; avoid `*-latest`.
- Give workflows only the `GITHUB_TOKEN` permissions their jobs need.
  Use `contents: read` for read-only checks and grant write access only where required.
  Set `persist-credentials: false` on checkout steps in jobs that do not push.
- Avoid running the same PR checks on both `push` and `pull_request`.
  Group PR runs by workflow and PR, and cancel runs superseded by newer changes.
- Set a reasonable timeout for CI jobs so stalled checks do not run indefinitely.
