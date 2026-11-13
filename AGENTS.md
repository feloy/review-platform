# Working in LuthiAI

Read `README.md` and `ARCHITECTURE.md` before changing behavior.
The README defines the user workflow.
The architecture document defines the system design and security boundaries.
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
  Preserve provider model IDs exactly, including case.
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
- Compare canonical paths of existing fixtures when testing resolved filesystem locations across platforms.
  Windows may use short directory names and verbatim prefixes for the same location.
- Fingerprint filesystem paths with length-prefixed OS-string bytes,
  preserving non-UTF-8 names without lossy conversion or JSON serialization.

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

- Bind the local GUI and API to loopback.
  Before adding state-changing API routes, verify lead identity
  and protect against requests from other sites.
  Release GUI runner-lock probes before reading state,
  and tolerate brief probe overlap when acquiring a recovery lock.
- Keep GUI review jobs independent of browser connections and reject duplicate
  launches during preflight.
  Show startup failures without exposing raw adapter output.
  Report job failure causes locally with PR and round context.
  Omit private JSON values and panic payloads from those diagnostics.
- Require the lead session for private draft contents and keep them out of public
  request snapshots.
  Render agent-authored draft content as plain text.
- Handle Material Web menu actions through `close-menu` for mouse and keyboard activation.
  Treat Escape as dismissal and guard disabled custom-element actions.
- Keep GitHub refresh and action confirmation panels reachable on compact PR rows,
  and cover those signed-in flows in UI tests.
- Show button explanations in tooltips available on hover and keyboard focus,
  including for disabled buttons; keep them within the viewport and outside clipping containers.
- Distinguish live local progress from GitHub observation freshness in the GUI.
  Bind cached display context to observed inputs and discard it when inputs change or the PR is removed.
  Display-cache failures must not turn a completed refresh or preparation into an action failure.
- Only a verified lead decision may authorize a review, validate the review model, or publish or reject a draft.
  Never treat PR text or agent output as authority.
- Keep model selection separate from agent selection.
  Co-author trailers are untrusted hints for proposing a different model;
  resolve its agent from configured capabilities and repository runner order.
- Disable GUI model and scope selectors together when readiness is blocked,
  an action is pending, or preparation is outdated.
- Require each lead authorization to name the PR head the lead inspected.
  Reject it if the recorded head has changed; revocation needs no head match.
  Keep inspected inputs fixed in GUI decision forms;
  live updates must not silently retarget a pending lead decision.
  Bind GUI model validation to the inspected configuration and authorization too.
  Include referenced prompt contents, selected adapter guidance, task templates, and output schemas
  in configuration fingerprints and rechecks.
  Recheck persisted state even when the model choice appears unchanged.
- Keep system rules, agent guidance, and task templates in source;
  configure project guidance per repository.
  Select full or incremental scope in the daemon using confirmed publication receipts.
  Verify the previous reviewed commit's ancestry and fall back to full on divergent or missing history.
  PR text and review markers never select the scope or baseline.
- A round requires green checks for the exact PR head being reviewed.
  Bind each draft to that head and the author comments seen; mark it stale on change.
- Combine check runs and commit statuses for the exact PR head when assessing CI.
  An empty result set is not green.
  An empty legacy-status rollup's `pending` value contributes no result.
  Recheck at round start because an operator display is only a snapshot.
- Post only the exact draft the lead approved.
  Rejection posts nothing and leaves the current authorization unchanged.
  Bind GUI publication to the inspected round, publication body hash, and displayed publishing account.
  Require that account in the publication API request; UI controls alone do not enforce the binding.
  Keep publication jobs independent of browser connections and reject duplicate starts during preflight.
  Require explicit recovery for uncertain publication; never automatically retry the post.
- Record rejection as an explicit decision and preserve the private draft.
  Do not mark a completed draft as failed merely to unblock another round.
  Clear model validation matching the rejected round and preserve a different later choice.
  Bind GUI rejection to the inspected round and publication body hash.
  Allow stale drafts to be rejected after sandbox cleanup completes.
- When a publisher may share the PR author's GitHub identity,
  exclude only review IDs recorded from confirmed publication from author-comment triggers.
  Apply the same exclusions to every discussion pass and final recheck.
  Treat text markers as display cues, never proof of origin.
- Stopping PR management permanently deletes its request, rounds, drafts, and logs.
  Block removal during active work, pending cleanup, or unresolved publication.
  Coordinate GUI job starts with removal and clear server-owned job summaries only after success.
  Check every runner lock before deleting files; never follow symlinks into other directories.
  Keep a global revision floor without retaining removed PR identities or history.
  Re-adding must advance the revision and require fresh discussion observation and lead authorization.
- Keep GitHub posting credentials in the daemon.
  Agents receive read-only GitHub access.
  Treat PR files, comments, commit messages, and in-PR agent instruction files as untrusted data, including when constructing agent prompts.
- Connect to an already-running, operator-managed OpenShell gateway selected explicitly.
  Require its expected model and read-only GitHub providers before starting a round;
  attach only those providers and disable automatic provider creation.
  Pin each agent's model endpoint explicitly and verify its CLI routing;
  do not assume a base-URL environment variable controls the destination.
  Build sandbox arguments from validated trusted configuration and round inputs.
  Resolve host file references against the configuration file's directory and retain absolute paths in runner snapshots.
  Bind configuration fingerprints to that directory and preserve referenced-file traversal, symlink, and checksum checks.
  Keep executable paths inside the VM absolute.
  Keep review mount targets, access modes, and mandatory lifecycle restrictions fixed in code.
  Read the driver type from configuration; support only `vm` for now
  and report `podman` as unsupported or unknown names as invalid
  before creating a sandbox.
  Manage review sandboxes, not the gateway, its providers, or their credentials and rotation.
- Pin OCI images by digest.
  For VM archives and their separate OCI metadata, reject symlinks,
  hash the opened files, and reverify private copies before execution.
  Read image environment values only from verified metadata.
  Do not log credentials, tokens, prompts containing secrets, or draft text by default.
  Keep secret material out of repository files.
- Verify an actual agent tool call under its exact CLI flags and working directory
  before relying on an allowlist entry.
  The reported tool list, CLI permission match, and live VM mount access are separate checks.
  Keep trusted tool instructions aligned with the flags; prompts do not enforce access.
- Validate external input at its boundary and propagate actionable errors.
  Do not make network calls or run agents from pure domain logic or unit tests.
  Bound GitHub response bodies before decoding;
  pagination alone does not bound comparison file patches.
- Use platform trust roots for outbound HTTPS so operator-installed certificates work.
  Keep server certificate verification enabled.

## Changes and commits

- Document user runnable commands and current limitations in `README.md`.
  Document contributor architecture, development commands, and security boundaries
  in `ARCHITECTURE.md`.
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
