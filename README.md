# LuthiAI

LuthiAI (pronounced “Luthier”) coordinates AI reviews of pull requests in GitHub repositories.

A lead developer uses its local GUI for one or more repositories,
with configuration and review state stored locally.

An AI assistant handles setup and PR registration through the CLI.

Before starting a review, the lead can choose the review model.

The lead can also refine the project's [review prompt](#refine-review-prompts),
for example to emphasize project conventions or specific areas of concern.

Each review is started explicitly through the CLI or GUI.

A separate reviewer runs in a secure sandbox managed by OpenShell,
with read-only access to the PR code and GitHub.

Each generated review stays private until the lead publishes it unchanged;
the lead can also reject it.

## Install and run LuthiAI

Place the compiled `luthiai` binary (`luthiai.exe` on Windows)
in a directory on your `PATH`.
Check its version:

```sh
luthiai --version
```

### Install the agent skill

Obtain the complete [`skills/luthiai/` folder](skills/luthiai/SKILL.md)
from the repository revision used to build your binary.
It includes the setup guide, CLI reference, and agent metadata;
use the skill version matching `luthiai --version`.
No LuthiAI source build is needed to use the skill.

For Codex, open the directory containing `skills/luthiai/` and install it with:

```sh
luthiai_skill_dir="${CODEX_HOME:-$HOME/.codex}/skills/luthiai"
mkdir -p "$luthiai_skill_dir"
cp -R skills/luthiai/. "$luthiai_skill_dir/"
```

The skill is available on your next Codex turn; ask Codex to use `$luthiai`.

Other agents can install the same folder in their supported skills directory.

Standalone skill archives use this same `skills/luthiai/` layout;
release publishing is currently manual.

### Setup

Ask your assistant to set up or change:

- **Repositories**: which GitHub repositories LuthiAI can review.
- **Review guidance**: each repository's conventions, review priorities, and areas of concern.
- **Models**: the review models available per repository, their proposal order,
  and the co-author signatures used to suggest a different model from one credited in the PR's commits.
- **Reviewers**: which programs run those models inside OpenShell, currently Claude or Codex.
- **GitHub access**: access to private repositories and the account used to publish reviews.
- **Sandbox setup and limits**: the OpenShell gateway, providers, VM images,
  CPU, memory, review duration, reviewer turns, and log size limits.
- **Local storage**: where configuration and review state are saved;
  drafts and logs live alongside the state file.

For example: “Add my repository, emphasize security in its reviews, and limit each review to 5 minutes.”
Your assistant uses the [LuthiAI configuration guide](skills/luthiai/references/configuration.md) to apply these choices.

The gateway operator supplies an already-running OpenShell gateway and compatible VM images;
this may be the lead developer or another administrator.
Configuration stays in private local files shared by the GUI and CLI.

### Refine review prompts

Each repository has a project review prompt intended for the lead to refine.
Ask your assistant: “Show me the review prompt for owner/repo and its file location.”
You can edit that text file yourself or ask your assistant to apply changes.

Find the configuration with `luthiai config-path`;
each repository's `prompt_file` points to its prompt, commonly `prompts/<owner>/<repo>.md` beside the configuration.
Relative paths start at the configuration file's directory.
The same project prompt is used by every reviewer and model for that repository.

LuthiAI combines your guidance with instructions supplied by the binary:

| Prompt part | What it covers |
| --- | --- |
| **Your project prompt** | Project conventions, design constraints, review priorities, and expectations for findings and wording. |
| **Built-in system rules** | Treating PR content as untrusted, reviewing without executing code or tests, the required output format, and separating public findings from private lead advice. |
| **Built-in reviewer guidance** | How Claude or Codex should use the available files and tools inside OpenShell. |
| **Generated round task** | The PR and exact commits to inspect, full or incremental scope, and the previous published review when relevant. |

System rules take priority over project guidance; LuthiAI controls the round's scope and sandbox permissions.
After editing a prompt, prepare the review again and validate the model for the updated instructions.
The [prompt guide](skills/luthiai/references/configuration.md#project-review-prompts)
also explains how to inspect the instructions saved for a past round.

### Run the graphical shell

Run the GUI from an interactive terminal:

```sh
luthiai gui
```

Open the one-time sign-in link printed in the terminal and keep it private.
The GUI runs at `http://localhost:8080` and guides you through reviews and draft decisions.
Ask your assistant to register the PRs you want to review.

Use **Refresh open PRs** in the header to check open and unchecked PRs for GitHub changes.
The connection indicator reports local live updates;
hover over it or focus it with the keyboard for details.

Each PR row shows its title, one review status, and the next action.
The footer shows GitHub freshness and, when needed, the CI result.
When CI is pending, use **Recheck CI** to fetch current results or **View checks on GitHub** to inspect them there.
Use **Details** for recorded inputs, permission, model, discussion counts, and round metadata.
**Show progress** and **View issue** open a separate panel for review activity and recovery instructions.
Past activity remains available through **Review activity** in Details.
The **⋮** menu contains refresh, review permissions, and stop managing.
Only one decision panel opens at a time; it keeps the inputs you inspected fixed across live updates.
Use the **Close …** button at the top right of each panel to dismiss it.
Closed PRs without outstanding work use a compact identity row.

The GUI has no persistent state of its own:
requests, review history, and drafts are stored in local files.
Restarting `luthiai gui` preserves saved work and provides a new sign-in link.
The process also runs reviews and publishes drafts, so wait for that work to finish before stopping it.
You can close or reload the browser tab at any time without interrupting reviews or publication.

### Current limitations

PRs must be registered explicitly; LuthiAI does not discover requests or schedule reviews automatically.
The review runner currently supports local OpenShell VM gateways on macOS and Linux;
remote gateway deployment is not implemented.

## How reviews work

1. Your assistant registers the PR through the CLI.
2. You inspect the PR, choose the model and project guidance, and start a review once CI is green.
3. The reviewer reads the PR code and discussion in OpenShell and writes a private draft.
4. You inspect the draft and either publish it unchanged or reject it.

A reviewer's recommendation does not grant formal GitHub approval;
a human maintainer decides whether to approve and merge the PR.

## Contributor documentation

See [ARCHITECTURE.md](ARCHITECTURE.md) for system design, security boundaries, and development guidance.
The [local API reference](docs/api.md) documents endpoints, payloads, and responses.
