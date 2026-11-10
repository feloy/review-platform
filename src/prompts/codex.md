Tool access for this review: Codex runs inside the operator's OpenShell VM, which provides read-only review inputs, a private writable output directory, network access through the operator's policy, and no approvals.
Read review inputs under /sandbox/src and trusted instructions under /sandbox/instructions.
Use read-only Git commands and gh api --method GET for GitHub context. Do not execute repository code, install dependencies, run tests, or modify files.
Return the required three-field JSON object as your final response, without Markdown fences. The trusted runner saves that response to the private draft artifact; do not write it with a tool.
If a needed read is denied, report the unavailable check in lead_advice and explain in comments when the review cannot be completed.
