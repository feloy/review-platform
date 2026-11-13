# Local API reference

The GUI server exposes this API at `http://localhost:8080` on IPv4 loopback.
Start it with `luthiai gui` in an interactive terminal.
For server lifecycle, trust boundaries, and job ownership, see
[the architecture guide](../ARCHITECTURE.md#local-gui-and-api).

## Request snapshots and events

```sh
curl http://localhost:8080/api/requests
curl -N http://localhost:8080/api/requests/events
```

`GET /api/requests` returns a JSON snapshot sorted by repository and PR number.
Each request includes optional discussion counts and model validation,
plus a summary of its latest round when one exists, including its failure kind and draft freshness.
The optional `context` contains persisted display-only GitHub metadata
bound to `head_sha` and `revision`,
with `checked_at`, optional exact-head `checks`, and code and discussion change flags.
Round summaries include `created_at`, optional `dispatched_at`, and optional `finished_at`.
New rounds also include `task` with `base_sha` and `scope`:
`mode: full`, or `mode: incremental` with the previously reviewed `since` commit and confirmed `review_id`.

The optional `run` reports the latest GUI job for this server launch:
`status` (`starting`, `running`, `finished`, or `failed`), `round_id`, and `failure`.
`running` means a round has been recorded; its persisted round status shows execution progress.
Job failure categories omit raw adapter errors, prompts, and provider details.

Review and publication job failures also write local diagnostics to the GUI server's terminal (stderr),
including the PR, round when known, and underlying error causes.
Inspect that terminal for configuration, credentials, network, or state persistence failures.
JSON diagnostics report the error category and location while omitting input values;
worker task failures report panic or cancellation without repeating panic payloads.

The optional `publication` reports the latest GUI publication job for this server launch:
`round_id`, `status` (`active`, `published`, `failed`, or `unknown`), safe `error`,
and optional post-publication `freshness`.
`active` includes preflight; it does not mean GitHub has received a post.

The latest round's optional `receipt` contains the confirmed review ID and URL,
publisher login and numeric ID, reviewed commit ID, and publication time.
Saved receipts and pending publication states remain visible after a server restart.
Persisted round and cleanup information remains available after restarting the GUI.

`GET /api/requests/events` is a server-sent event stream.
It sends a `requests` event with the same JSON snapshot on connection
and whenever displayed local state or runner activity changes,
within about one second of the change.
It sends `state_error` if the local state cannot be read,
then resumes snapshots when the read succeeds.
The browser reconnects automatically if the stream is interrupted;
reloading the page also starts a new connection.

## Sessions and actions

`/api` reports the API name and version, and `/api/hello` remains available.
Request snapshots and static assets do not require sign-in.
Private draft contents require the lead session token.

`POST /api/session` exchanges the one-time code for a launch-scoped session token.
`POST /api/requests/refresh`, `POST /api/requests/authorize`,
`POST /api/requests/revoke`, `POST /api/requests/unmanage`, `POST /api/requests/prepare`,
`POST /api/requests/validate-model`, `POST /api/requests/run`,
`POST /api/drafts/ROUND_ID/reject`, and `POST /api/drafts/ROUND_ID/publish`
require that token and a same-origin request;
the server rejects cross-origin and missing-origin POST requests.

### Authorization and revocation

Authorization accepts `key` with `repository` and `number`,
the inspected `head_sha` and `revision`, and `scope` (`one_round` or `pr_cycle`).
Revocation accepts only `repository` and `number`; it needs no inspected head or revision.
Both return `changed` to report whether local state changed.
Authorization conflicts return HTTP 409 and missing requests return HTTP 404.
These decisions compare local state, using the same rules and locks as the CLI.
Refresh from GitHub before inspecting the inputs for a decision.

### Stop managing a PR

Management removal accepts `repository` and `number` at `POST /api/requests/unmanage`.
It permanently deletes the request, its rounds, publication receipts, and saved draft and log files,
then removes the PR from snapshots and SSE updates.
Successful GUI removal also clears its in-memory review and publication job summaries.
GUI job starts for that PR are rejected while removal is in progress.
It returns `changed: true` on removal.
Missing requests, including ones already removed, return HTTP 404;
active GUI jobs, active rounds, pending cleanup, and unresolved publication return HTTP 409.
It requires no inspected head or revision and makes no GitHub calls.
File deletion precedes the state update so failures leave the request available for retry.
Some files may already be deleted after a failure; retry removal to finish.

### Preparation and model validation

Preparation accepts `repository` and `number` and uses the effective `--config-file`.
It returns the prepared `head_sha`, `revision`, `authorization`,
`configuration_sha256`, exact-head `checks`, `readiness` and `readiness_message`,
`proposed_model`, `proposal_matches_coauthor`, `validated_model`, `resolved_model`,
and `models` with each configured model and its resolved reviewer.

Validation accepts `key` with `repository` and `number`, the selected `model`,
the inspected `head_sha` and `revision`,
and `rules` with the prepared `authorization` and `configuration_sha256`.
It returns a new preparation snapshot after saving the model choice.

Blocked preparation returns a snapshot; blocked or stale validation returns HTTP 409.
Configuration and archive verification failures return HTTP 422.
GitHub failures can leave freshly observed inputs saved without a new model choice.
The snapshots omit prompts, provider details, and archive paths.

### Start a review

Run accepts the same inspected payload as validation, including the selected model and rules,
plus optional `review_mode`: `auto` (the default) or `full`.
Automatic scope uses only publication receipts recorded for this PR;
copied markers and reviews without a locally recorded publication receipt do not select an incremental baseline.
It rechecks the persisted model choice and inspected inputs, authorization, and configuration
before startup and again before dispatch.
It returns HTTP 202 with `accepted: true` when the server takes ownership of the job;
this acknowledges scheduling, not successful startup or reviewer dispatch.
An already-active GUI job returns HTTP 409.
Watch request snapshots or SSE for startup errors and round progress.
One-round authorization is consumed only when dispatch is recorded.

### Inspect a draft

`GET /api/drafts/ROUND_ID?repository=owner%2Frepo&number=42`
requires the session token in its `Authorization: Bearer TOKEN` header.
It accepts a validated round ID and PR key and returns only a completed draft belonging to that PR.
The response contains `key`, `round_id`, `head_sha`, `revision`, `decision`, `model`, `agent`,
`verdict`, `freshness`, `publication_body`, `publication_body_sha256`, `lead_advice`, and optional `receipt`.
It also reports the inspected `publisher_login` when configuration is available
and the `inspected_at` time, without preventing inspection when publication setup is unavailable.
The body includes the exact round footer that publication would post.

Freshness uses the CLI's live GitHub checks and confirmed-publication exclusions;
it is separate from the round's recorded freshness in public snapshots.
Inspection does not persist a missing publication seal.
Missing sessions return HTTP 401, missing rounds or a different PR return HTTP 404,
unfinished rounds return HTTP 409, and failed draft verification returns HTTP 422.
Responses disable caching and omit review logs, prompts, provider details, paths, and raw errors.

### Reject a draft

`POST /api/drafts/ROUND_ID/reject` accepts `key` with `repository` and `number`,
and `body_hash` with the inspected `publication_body_sha256`.
It uses the CLI's rejection operation under the same PR and state locks,
verifies the original artifact and publication seal, and requires completed cleanup.
It does not fetch GitHub inputs or require current head or revision values.

It returns `changed: true` for a new rejection and `changed: false` for a repeated matching decision.
A different hash, pending cleanup, or a conflicting decision returns HTTP 409;
failed draft verification returns HTTP 422.
Watch request snapshots or SSE for the recorded decision.

### Publish a draft

`POST /api/drafts/ROUND_ID/publish` accepts the same `key` and inspected `body_hash`.
It also requires the inspected `publisher_login` so startup can reject account changes.
Missing or null accounts return HTTP 422; account comparison ignores letter case.
It returns HTTP 202 with `accepted: true` when the server owns the publication job,
or HTTP 409 when a GUI publication for the PR is already active.
Acceptance acknowledges scheduling; watch request snapshots or SSE for the outcome.
Preflight failures do not post a review and appear as safe job errors.
Rejection is blocked while a GUI publication for that PR is active.

The shared CLI operation verifies artifacts, the body hash and GitHub size limit,
completed cleanup, publisher identity, and live freshness before recording dispatch.
A matching completed publication returns its saved receipt without another post.
Unknown outcomes and an uncompleted publication require CLI recovery before another decision.
