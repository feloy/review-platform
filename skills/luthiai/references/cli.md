# LuthiAI CLI reference

## Environment and inputs

Use the installed `luthiai` binary matching the skill's version.
`luthiai --help` lists commands; `luthiai help COMMAND` describes their flags.
Output is text; there is no JSON output flag.

For missing configuration, new repositories, or prompt and model setup,
read the [configuration guide](configuration.md).

`state-path` and `config-path` report the effective local files without creating them.
Use the same files as the lead's GUI.
Global `--state-file PATH` and `--config-file PATH` overrides go before the subcommand:

```sh
luthiai --state-file /absolute/path/state.json --config-file /absolute/path/config.json status owner/repo 42
```

Relative overrides resolve from the current directory.
Paths referenced inside the configuration resolve from that configuration file's directory.
Do not edit state JSON to change authorization, model validation, or round decisions.

| Input | Source |
| --- | --- |
| `owner/repo`, PR number | The requested GitHub PR; the number must be positive. |
| `HEAD_SHA` | The inspected head from `github-pr`, `status`, or the latest preparation; a full 40- or 64-character hexadecimal commit ID. |
| `REVISION` | The positive local input revision reported by `status` or preparation. |
| `MODEL_ID` | A configured model reported by preparation and selected by the lead; preserve its case. |
| `ROUND_ID` | The saved round reported by `run-review` or `status`. |
| `BODY_SHA256` | The exact publication body hash from `show-draft`. |

The operator supplies trusted configuration and an existing OpenShell gateway with VM support,
compatible model providers, and a read-only GitHub provider.
`gateway status` checks that setup without changing it.
Use the setup guide when configuration is missing; the operator manages gateway providers.
GitHub reads use `LUTHIAI_GITHUB_TOKEN_RO` when supplied;
publication uses `LUTHIAI_GITHUB_TOKEN_RW` and the configured `publisher_login`.
Keep credential values out of command output, prompts, and repository files.

## Register and prepare a PR

For a PR the user has asked LuthiAI to manage:

```sh
luthiai github-pr owner/repo 42 --diff --checks
luthiai request owner/repo 42 HEAD_SHA
luthiai refresh owner/repo 42
luthiai status owner/repo 42
luthiai prepare-review owner/repo 42
```

Replace placeholders with inspected output values; do not execute them literally.
`request` records a new request or updates its head, but does not authorize a review.
`refresh` observes an existing request's head and author discussion.
Its first observation establishes the discussion baseline and advances the revision.
Changed inputs can clear one-round authorization and model validation.
Preparation can save refreshed inputs even if a later check fails; inspect the resulting state before continuing.

## Lead decisions and review execution

When the lead has inspected the current head and revision and chosen one-round authorization:

```sh
luthiai authorize owner/repo 42 --scope one-round --head HEAD_SHA --revision REVISION
```

Use `--scope pr-cycle` only when the lead chooses persistent authorization for later rounds.
Revocation uses `luthiai revoke owner/repo 42` and needs no matching head or revision.
Authorization compares saved local inputs; refresh before presenting those inputs to the lead.

After authorization, prepare again and obtain the lead's model choice for the current inputs:

```sh
luthiai prepare-review owner/repo 42
luthiai prepare-review owner/repo 42 --model MODEL_ID --head HEAD_SHA --revision REVISION
```

Validation requires an open PR, active authorization, green exact-head checks, and a verified VM archive.
If preparation changes the inputs or clears authorization, return to the lead with the new values.
Neither preparation nor validation starts a review or consumes one-round authorization.
The reviewer is resolved from configured support for the selected model; it is not a separate CLI choice.

With those decisions recorded, start the requested review and inspect its saved draft:

```sh
luthiai gateway status
luthiai run-review owner/repo 42 --head HEAD_SHA --revision REVISION
luthiai show-draft owner/repo 42 --round ROUND_ID
```

`run-review` runs in the foreground and rechecks live inputs, CI, authorization, and configuration before dispatch.
One-round authorization is consumed when dispatch is recorded.
Missing checks, pending checks, and failed checks are not green.

Scope is incremental after the latest locally confirmed publication, using its reviewed commit after an ancestry check.
No receipt, divergent history, or missing previous history falls back to full review.
PR text and copied review markers cannot select a baseline.
Use `--full-review` when the lead requests a full review regardless of previous publications.

## Inspect and decide on drafts

`show-draft` displays the exact publication body and hash, reviewed inputs, decision, private lead advice, and freshness.
It seals the saved body for a later decision and does not post to GitHub.
`--log` additionally displays the sensitive review dialogue.
An unavailable freshness check reports unknown freshness while still allowing inspection.

For the lead's explicit publication of the inspected draft:

```sh
luthiai publish-draft owner/repo 42 --round ROUND_ID --body-hash BODY_SHA256
```

Publication verifies the saved body, publisher identity, completed sandbox cleanup, and live freshness.
It posts one `COMMENT` review on the reviewed commit.
Stale or unknown freshness, a closed PR, or a body exceeding GitHub's limit blocks publication.
A confirmed publication returns its saved receipt if the same command is repeated.

For the lead's explicit rejection instead:

```sh
luthiai reject-draft owner/repo 42 --round ROUND_ID --body-hash BODY_SHA256
```

Rejection requires completed cleanup but accepts stale or unknown freshness.
It posts nothing, preserves the private draft and current authorization,
and clears model validation only when it still matches the rejected round.

## Later rounds

A saved undecided draft must first receive the lead's publication or rejection decision.
For code changes since a previous round, inspect `github-pr owner/repo 42 --since PREVIOUS_HEAD_SHA --checks`.
After a force push that breaks ancestry, use `--diff` instead.
Then refresh and prepare the PR again.

One-round authorization needs renewal after dispatch and is cleared by input changes before dispatch.
PR-cycle authorization persists across commits, author comments, and draft rejection.
Each round still needs a current validated model and an explicit start.

## Recovery and failure handling

| Situation | Operation |
| --- | --- |
| Interrupted review or pending sandbox cleanup | `luthiai recover-review owner/repo 42` stops and deletes the recorded sandbox without rerunning the reviewer. It refuses an active runner. A completed draft survives cleanup. |
| Uncertain publication | `luthiai recover-publication owner/repo 42 --round ROUND_ID` checks for a matching posted review without posting again. A unique match saves its receipt; no match leaves the outcome unknown. |
| Multiple matching reviews | After inspection, the lead can select a confirmed match with `recover-publication ... --review-id REVIEW_ID`. |
| Lead has inspected GitHub and confirmed nothing was posted | `recover-publication ... --confirm-not-posted ATTEMPT_ID` permits another explicit draft decision. A delayed earlier request can still create a duplicate; do not infer this confirmation from an empty lookup. |
| Startup gate, configuration, or network failure | Report the cause and current state. Resolve the requirement before another explicit attempt; do not bypass checks or restore consumed authorization. |

Unresolved publication blocks refreshing the PR and starting another round.
Review recovery never restores consumed one-round authorization.

## Command index

| Command | Purpose |
| --- | --- |
| `github-pr REPOSITORY PR [--checks] [--diff \| --since HEAD_SHA]` | Inspect live GitHub inputs without reading or changing local state. |
| `request REPOSITORY PR HEAD_SHA` | Record a request at the inspected head. |
| `refresh REPOSITORY PR` | Save current head and author discussion fingerprints. |
| `status REPOSITORY PR` / `list` | Inspect saved request and latest-round state, or list requests. |
| `prepare-review REPOSITORY PR` | Refresh inputs, report readiness, and propose a model. |
| `prepare-review REPOSITORY PR --model MODEL_ID --head HEAD_SHA --revision REVISION` | Record the lead's inspected model choice. |
| `authorize REPOSITORY PR --scope one-round\|pr-cycle --head HEAD_SHA --revision REVISION` | Record the lead's authorization scope. |
| `revoke REPOSITORY PR` | Withdraw authorization at the lead's request. |
| `gateway status` | Inspect the configured gateway and providers. |
| `run-review REPOSITORY PR --head HEAD_SHA --revision REVISION [--full-review]` | Execute one validated review. |
| `show-draft REPOSITORY PR --round ROUND_ID [--log]` | Inspect a saved private draft. |
| `publish-draft REPOSITORY PR --round ROUND_ID --body-hash SHA256` | Publish the exact draft chosen by the lead. |
| `reject-draft REPOSITORY PR --round ROUND_ID --body-hash SHA256` | Record rejection without posting. |
| `recover-review REPOSITORY PR` | Clean up an interrupted sandbox. |
| `recover-publication REPOSITORY PR --round ROUND_ID` | Reconcile an uncertain post with GitHub. |
| `unmanage REPOSITORY PR` | Permanently delete the request, history, private drafts, and logs when the lead requests removal. Active work, pending cleanup, and uncertain publication block it; GitHub publications remain. |
| `state-path` / `config-path` | Report effective local file paths. |
| `gui` | Start the human interface in an interactive terminal. |
| `help [COMMAND]` / `--version` | Inspect CLI usage or version. |
