# LuthiAI configuration

Read this reference when asked to set up or change repositories, models, project prompts,
VM image references, local file locations, or GitHub access for the installed application.

- [Setup workflow](#setup-workflow)
- [Local files](#local-files)
- [OpenShell prerequisites](#openshell-prerequisites)
- [Minimal review configuration](#minimal-review-configuration)
- [Additional models and reviewers](#additional-models-and-reviewers)
- [Paths and model selection](#paths-and-model-selection)
- [Project review prompts](#project-review-prompts)
- [VM images and policies](#vm-images-and-policies)
- [GitHub credentials and HTTPS](#github-credentials-and-https)
- [Verify setup](#verify-setup)

## Setup workflow

Locate the effective files with `luthiai config-path` and `luthiai state-path`.
Inspect existing configuration before editing it and preserve unrelated settings.
Keep the GUI and CLI on the same files; do not edit saved review state as a setup shortcut.

Use the operator's trusted repository list, model IDs, provider names, publishing account,
VM archives, policy files, and expected checksums.
If required values are missing, identify them for the operator rather than inventing working values.
The example below contains placeholders, not usable provider names or verified image checksums.

Write the private configuration and project guidance within the requested setup scope.
Keep credentials in the operator's environment or OpenShell providers, never in configuration,
project prompts, repository files, or diagnostic output.
Setup does not authorize a review, validate a model choice, or approve publication.

## Local files

The global `--state-file PATH` option overrides the default local JSON state file.
The global `--config-file PATH` option overrides the review configuration file.
Relative override paths are resolved from the current directory.
Without overrides, the CLI uses the per-user locations for each platform:

| Platform | Default state file | Default config file |
| --- | --- | --- |
| Linux | `$XDG_STATE_HOME/luthiai/state.json` | `$XDG_CONFIG_HOME/luthiai/config.json` |
| macOS | `~/Library/Application Support/luthiai/state.json` | `~/Library/Application Support/luthiai/config.json` |
| Windows | `%LOCALAPPDATA%\luthiai\data\state.json` | `%APPDATA%\luthiai\config\config.json` |

On Linux, an unset or relative `XDG_STATE_HOME` uses `~/.local/state` instead.
An unset or relative `XDG_CONFIG_HOME` uses `~/.config` instead.
The CLI creates the application directory when it first saves state.

The state file and configuration file have separate schema versions.
The current state schema is version 8.
Versions 5 through 7 are upgraded in memory and saved as version 8 on the next mutation;
older and future versions are rejected.
Existing private draft artifacts are left unchanged.
A global `request_revision_floor` prevents reuse of discarded input revisions
without retaining removed PR identities or review history.

For project-local files, pass both `--state-file .luthiai/state.json`
and `--config-file .luthiai/config.json`.
Git ignores the `.luthiai/` directory.
Keep the state file private to the lead operator.

## OpenShell prerequisites

Install the OpenShell 0.1.7 CLI and configure its named gateway and workspace
before running `gateway status` or `run-review`.
The gateway operator starts the gateway with VM driver support,
enables the trusted VM mount configuration,
and configures a model provider compatible with the validated model
and a GitHub provider restricted to read-only REST access.
LuthiAI checks authenticated access and named provider availability,
and rejects a write-capable GitHub policy when the gateway exposes one.
The operator remains responsible for provider policy and credential rotation.

The VM gateway must run on the host that holds the staged archive and mount paths.
The current runner is intended for macOS and Linux VM setups;
remote gateway deployment is not implemented.

## Minimal review configuration

Create a private JSON configuration before using `prepare-review`,
`gateway status`, or `run-review`.
The configuration file has schema version 7;
the state file uses schema version 8.
Earlier configuration schemas are rejected during development;
update the private file before running review commands.

`models` defines selectable models and their co-author signatures.
`agents` lists the reviewers LuthiAI can run inside OpenShell, with their capabilities and runner settings.
The assistant edits this configuration on the lead's behalf.

Start with one repository, one model, and one Claude reviewer.
The example relies on the documented resource and turn-limit defaults;
replace the model, gateway, file paths, publishing account, and checksums with trusted values:

```json
{
  "schema_version": 7,
  "publisher_login": "expected-publisher-account",
  "execution": {
    "openshell": "/absolute/path/to/openshell",
    "gateway": "operator-gateway",
    "workspace": "default",
    "driver": "vm",
    "github_provider": "operator-github-readonly"
  },
  "models": [
    {
      "id": "model-alpha",
      "coauthors": [
        {
          "name": "Model Alpha",
          "email": "model@example.com"
        }
      ]
    }
  ],
  "agents": [
    {
      "id": "claude",
      "models": [
        "model-alpha"
      ],
      "runner": {
        "kind": "claude",
        "model_provider": "operator-model-provider",
        "policy": "policies/claude/review-policy.yaml",
        "policy_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "hold_command": [
          "/bin/sleep",
          "3600"
        ]
      }
    }
  ],
  "repositories": [
    {
      "name": "owner/repo",
      "prompt_file": "prompts/owner/repo.md",
      "review_models": [
        "model-alpha"
      ],
      "runners": [
        {
          "agent": "claude",
          "vm_archive": {
            "path": "images/claude/osi-claude-1.tar",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "config_sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
          }
        }
      ]
    }
  ]
}
```

## Additional models and reviewers

A single reviewer and model are sufficient to run a review.
To offer more model choices, add entries to `models`, add their IDs to the compatible reviewer's `agents[].models`,
and list them in the repository's `review_models` in proposal order.
LuthiAI can propose a model different from a co-author hint only when another compatible model is configured.

To use Codex instead of Claude, replace the example's `agents` entry with:

```json
{
  "id": "codex",
  "models": [
    "model-beta"
  ],
  "runner": {
    "kind": "codex",
    "model_provider": "operator-openai-responses-provider",
    "environment": {
      "OPENAI_BASE_URL": "https://model-gateway.example/v1"
    },
    "policy": "policies/codex/review-policy.yaml",
    "policy_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "hold_command": [
      "/bin/sleep",
      "3600"
    ],
    "max_turns": 100
  }
}
```

Add the corresponding `models` entry for `model-beta`, select it in the repository's `review_models`,
and replace that repository's runner with:

```json
{
  "agent": "codex",
  "vm_archive": {
    "path": "images/codex/osi-codex-1.tar",
    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "config_sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
  }
}
```

For both reviewers, keep both `agents` entries and repository runners,
with compatible configured models for each.
Use an operator-managed OpenAI Responses-compatible provider for Codex;
its explicit HTTPS endpoint, image executable, archive metadata, and policy requirements are described below.

## Paths and model selection

Replace the example model IDs, co-author signatures, prompt file, archive path, and checksums
with the trusted values for your setup.
Model IDs should identify the exact models that the reviewer can run.
Their spelling and case are preserved to match the provider's model IDs.
The `agents[].id` values are lowercase ASCII identifiers.

Host file paths in `prompt_file`, `vm_archive.path`, `runner.policy`, and `execution.openshell`
accept absolute paths or paths relative to the directory containing `config.json`.
Resolution is independent of the working directory when the application runs.
Archive, policy, and OpenShell executable paths must be nonempty and cannot contain `..` traversal.
The loaded configuration stores absolute host paths for adapters.
Its fingerprint includes the configuration directory;
moving a configuration requires fresh preparation and model validation.
Directory fingerprinting supports non-UTF-8 names.
Resolved archive, policy, and OpenShell paths still require UTF-8 names;
use absolute paths to those resources outside that directory when needed.
Paths to executables inside the VM, including `hold_command[0]`, remain absolute VM paths.

The CLI matches both the name and email of trailing `Co-authored-by` commit trailers,
case insensitively, so models sharing a co-author email can be distinguished.
It never treats the commit subject as a trailer.
It proposes the first repository-listed model absent from those trailers.
If every configured model matches a trailer, it proposes the first and explains the match.

For that model, it chooses the first repository-listed runner whose reviewer declares support.
Configuration is rejected if any listed review model has no compatible runner.
Commit trailers are author-controlled hints; only the lead's explicit model choice is trusted.

## Project review prompts

The lead maintains one project review prompt per repository, shared by all its reviewers and models.
It defines project conventions, design constraints, review priorities, and expectations for findings and wording.
Help the lead inspect and refine this guidance before reviews.

### Locate and edit the project prompt

1. Run `luthiai config-path` with the same global overrides as the lead's GUI to locate the effective configuration.
2. Find the repository in `repositories` and read its `prompt_file` value.
3. Resolve that path against the configuration file's directory unless it is already absolute.
   When the lead asks to inspect the prompt, provide its resolved path and current text.
4. The lead can edit the file in a text editor or ask the assistant to apply changes.
   Preserve guidance outside the requested change.

The `prompt_file` names a local UTF-8 text or Markdown file.
Relative paths resolve from the configuration file's directory, independently of the working directory;
absolute paths are also accepted.
Store project prompts as `prompts/<owner>/<repo>.md`, using the repository's organization or owner as the directory.
For the example above, create `prompts/owner/repo.md` under the directory containing `config.json`.
This is a setup convention; use the configured path when editing an existing prompt.
For example, project guidance could contain:

```text
Prioritize authorization bugs, data loss, and incorrect state transitions.
Check changes against the documented public behavior.
Explain each finding's impact and the condition that triggers it.
Keep optional style suggestions separate from blocking findings.
```

The file must be a regular file with nonblank contents of at most 64 KiB.
Its exact contents, including line breaks, are loaded with the configuration.
Prompt edits change the configuration fingerprint and invalidate a matching model validation;
prepare and inspect the review again and obtain the lead's model validation for the updated instructions.
Prompt contents are not printed by `prepare-review`.

### What the binary adds

| Part | Source and responsibility |
| --- | --- |
| **Project guidance** | The lead's editable `prompt_file`: project-specific review criteria, conventions, priorities, and presentation preferences. |
| **Reviewer guidance** | Embedded Claude or Codex instructions: sandbox paths, available tool usage, and reporting unavailable reads. |
| **System rules and output contract** | Embedded rules: PR content is untrusted; no repository code execution, dependency installation, or tests; a JSON response with `status`, public `comments`, and private `lead_advice`. The runner captures the review log separately. |
| **Round task** | Embedded full or incremental template filled with the PR, exact head and base commits, checkout path, and previous confirmed publication and reviewed commit when relevant. |

LuthiAI composes project guidance, the selected reviewer's guidance, and system rules in that order,
then supplies the generated round task separately.
System rules take priority over reviewer and project guidance.
The task determines the round's review scope and baseline;
sandbox mounts, tool configuration, and provider policies enforce access permissions.
Keep project prompts focused on the lead's review expectations rather than restating these built-in rules.
PR files, comments, and instruction files never become trusted prompt layers.
The composed prompt, task templates, and output schema participate in freshness fingerprints.
Updating them requires fresh preparation and model validation, even for an unchanged model choice.

### Inspect a round's instructions

For a round that reached instruction staging, the private files beside the state file retain the exact inputs.
Use `luthiai state-path` with the same global overrides to locate the state directory,
and `status` to find the round ID.
Under `rounds/ROUND_ID/instructions/` in that directory:

- `project-prompt.txt` is the project guidance used for that round.
- `system-prompt.txt` contains the composed project guidance, reviewer guidance, and system rules.
- `task-prompt.txt` contains the generated full or incremental task with its exact PR and commit context.
- `review-output.schema.json` is the required output schema.

These are private historical snapshots; edit the configured project prompt to change future reviews.
Read prompt snapshots only when requested, and keep their contents out of routine diagnostics.

### Update older configuration

To update schema 6 configuration, move `prompt_file` from each `agents` entry to its repository and set `schema_version` to 7.
Consolidate repository conventions into the project file;
reviewer tool instructions and full or incremental scope selection now belong to the application.

## VM images and policies

Archive paths must end in `.tar` and name regular files rather than symlinks.
Each archive needs a regular, separate OCI metadata file at `<archive>.config.json`.
Both SHA-256 checksums must match their files.
Calculate them with `shasum -a 256 FILE` on macOS or `sha256sum FILE` on Linux,
once for the tar and once for its `.config.json` file.

The configuration contains no credentials and should be kept private with the state file.
Replace the example gateway, provider, policy, and archive values
with the operator-managed values for the same local VM gateway.
The example checksums are placeholders and will fail verification.
The policy checksum is SHA-256 of the policy file;
the two VM image checksums cover the rootfs tar and its OCI metadata separately.

LuthiAI extracts `config.Env` from the verified metadata
and declares those non-secret values with OpenShell `--env` arguments.
The combined sandbox environment is limited to 64 KiB.
The Claude image must define an absolute `CLAUDE_CODE_EXECUTABLE` path there.
The Codex image must define `CODEX_PATH` with an absolute executable path instead.
Executable selection and mandatory environment values belong to the selected adapter;
`runner.environment` cannot override either reviewer's executable, Claude's gateway discovery flag, or `CODEX_HOME`.
Operator-specific non-secret values such as `ANTHROPIC_BASE_URL`
may be set in that reviewer's `runner.environment`.
Duplicate environment names are rejected.

The `hold_command` keeps a newly created sandbox alive without starting a reviewer;
the runner then rechecks every gate before executing the selected reviewer separately.
The sandbox mounts only the checkout and trusted instructions read-only,
plus the round's output directory writable.
Their fixed sandbox paths are `/sandbox/src`, `/sandbox/instructions`,
and `/sandbox/output`, respectively.
The runner always disables TTY and restarts and prevents automatic provider creation.

The shared `timeout_seconds`, `cpu`, `memory`, and `log_limit_bytes`
default to 1800 seconds, two CPUs, 4 GiB, and 8 MiB.
Each reviewer's `max_turns` defaults to 100.
For Claude it counts distinct main-assistant message IDs.
For Codex it counts completed reviewer messages and tool actions, deduplicated by item ID;
reasoning items do not consume a separate step.

The runner rejects `podman` as unsupported and unknown driver names as invalid
before creating a sandbox or consuming authorization.
Runner kinds `claude` and `codex` have execution adapters;
the `agents[].id` value is a separate configured identifier.
Do not put provider credentials or publishing credentials in this configuration.

Codex requires an OpenAI Responses-compatible model provider and an explicit HTTPS `OPENAI_BASE_URL`
in verified image metadata or `runner.environment`.
The endpoint must have no embedded credentials, query, or fragment.
Provider credentials stay in the operator-managed provider; do not put them in the environment configuration.

## GitHub credentials and HTTPS

`github-pr`, `refresh`, and `prepare-review` read public GitHub pull requests without a token.
For private repositories or a higher GitHub API rate limit,
set `LUTHIAI_GITHUB_TOKEN_RO` to a token with pull request, contents,
checks, and commit statuses read access.
The CLI uses this token only for GitHub reads.

To publish, set `LUTHIAI_GITHUB_TOKEN_RW` to a separate token
with pull-request write permission and set `publisher_login` in the private configuration
to the GitHub account expected to own that token.
The CLI verifies the account through `GET /user` before publication.
Write credentials remain in the operator process and never enter review sandboxes,
prompts, state files, or routine logs.
Rejection needs neither token nor GitHub access.

HTTPS uses the platform's trusted certificates, including organization roots installed for a TLS-intercepting proxy.
On Linux, `SSL_CERT_FILE` or `SSL_CERT_DIR` can select a PEM certificate bundle or directory instead of the system CA bundle.

## Verify setup

Check JSON syntax and verify the referenced prompt, policy, archive, and metadata files.
Compute file hashes and compare them with the operator's trusted expected values;
do not replace a mismatching expected checksum merely to make verification pass.

Inspect the configured gateway without creating a review sandbox:

```sh
luthiai gateway status
```

With custom files, keep the same global overrides used by the GUI:

```sh
luthiai --state-file /absolute/path/state.json --config-file /absolute/path/config.json gateway status
```

This loads configuration and checks gateway access, providers, and policy checksums.
It does not start a review or replace archive verification during review preparation and startup.
Once setup is complete, use the [CLI workflow](cli.md) for a PR the lead wants reviewed.
Configuration, referenced prompt, and path changes can invalidate model validation;
prepare the PR again and obtain the lead's model validation for the new configuration.

Set GitHub credentials before launching the GUI; it inherits credentials at startup.
Restarting the GUI preserves saved work, but wait for active reviews and publications to finish first.
