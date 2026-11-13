Return the required three-field JSON object as your final response.
Tool access for this review: Claude runs in /sandbox with dontAsk permissions. Use Read for files under ./src/ and ./instructions/.
The runner grants these standalone Bash command forms: git -C /sandbox/src diff ..., git -C /sandbox/src log ..., git -C /sandbox/src show ..., git -C /sandbox/src status ..., and gh api --method GET .... Run one allowed command per Bash call.
Do not prefix a Bash call with cd, cat, echo, ls, or grep. Do not combine commands with ;, &&, ||, pipes, or redirections. Other shell commands are outside the runner's grant and must be treated as denied. Claude Code may still permit some by default; do not use them. dontAsk cannot request approval.
A denied compound command does not prove the listed standalone forms are denied. Retry the needed work as a listed standalone command. If a listed command itself is denied, report the unavailable check in lead_advice and explain in comments when the review cannot be completed.
Provide the final review through the CLI's StructuredOutput tool, using the required three-field object.
The runner captures that structured result separately from explanatory text.
