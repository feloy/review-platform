// Human-facing summaries of observed workflow state; server checks still authorize actions.
export function preparationOutdated(request, preparation, invalid = false) {
  const validation = request.validated_model;
  return invalid || preparation.head_sha !== request.head_sha || preparation.revision !== request.revision ||
    preparation.authorization !== request.authorization || Boolean(preparation.validated_model && validation &&
      (preparation.validated_model !== validation.model ||
        preparation.models.find((choice) => choice.model === preparation.validated_model)?.agent !== validation.agent));
}

export function reviewBlocker(request) {
  const round = request.latest_round;
  if (request.publication?.status === 'active' || round?.status === 'publishing') return 'Wait for publication to finish before starting another review.';
  if (round?.status === 'publication_unknown') return 'Check GitHub and recover the uncertain publication before starting another review.';
  if (['starting', 'running'].includes(request.run?.status) || ['starting', 'running'].includes(round?.status)) return 'A review is already active. Wait for it to finish or recover it if it stopped.';
  if (round?.status === 'awaiting_decision') return 'Publish or reject the previous draft before starting another review.';
  if (round?.cleanup_pending) return round.cleanup_status === 'needs_attention'
    ? 'Recover the review environment before starting another review.' : 'Wait for review environment cleanup to finish before starting another review.';
  return '';
}

export function reviewSituation(request, preparation, invalid = false, selectedModel = preparation?.resolved_model) {
  const round = request.latest_round;
  const result = (title, message, action, label, tone = 'neutral') => ({ title, message, action, label, tone });
  if (request.publication?.status === 'active') return result('Publishing review', 'You can close this tab. Return here to see whether GitHub received the review.');
  if (round?.status === 'publication_unknown' || round?.status === 'publishing') return result('Confirm whether GitHub received the review', 'Check GitHub before deciding again. Recovery instructions are below.', null, null, 'warning');
  if (round?.cleanup_status === 'needs_attention') return result('Review environment needs recovery', round.status === 'awaiting_decision' ? 'Your draft is saved and can be read. Recover the environment before publishing or rejecting it.' : 'Recovery instructions are below. Finish recovery before continuing.', round.status === 'awaiting_decision' ? 'draft' : null, 'Read private draft', 'warning');
  if (round?.status === 'awaiting_decision') return result('Draft ready for your decision', round.cleanup_pending ? 'Read the draft now. Publishing and rejection become available when cleanup finishes.' : round.freshness === 'stale' ? 'The PR changed or closed. Read the outdated draft; you can reject it and prepare another review.' : 'Read the private draft, then publish it unchanged or reject it. This draft stays private until you publish it.', 'draft', 'Read private draft', round.freshness === 'stale' ? 'warning' : 'attention');
  if (['starting', 'running'].includes(request.run?.status) || ['starting', 'running'].includes(round?.status)) return result('Review in progress', 'The review runs on the server. You can close this tab and return later.');
  if (request.context?.state === 'closed') return result('Pull request closed', 'New reviews cannot start for a closed pull request.', 'github', 'Open pull request');
  if (round?.status === 'published' && round.head_sha === request.head_sha && round.revision === request.revision) return result('Review published', 'The review is on GitHub. Refresh from GitHub to look for later code or discussion changes.', 'receipt', 'Read published review', 'success');
  if (round?.cleanup_pending) return result('Finishing review cleanup', 'Wait for the review environment to be removed before continuing.');
  if (!request.discussion) return result('Check the pull request first', 'Fetch the current code and author discussion before giving review permission.', 'refresh', 'Fetch pull request details', 'attention');
  if (request.authorization === 'awaiting_lead') return result('Waiting for your permission', 'Inspect the pull request, then allow one review or keep permission for later reviews.', 'authorize', 'Review permissions', 'attention');
  if (preparation) {
    if (preparationOutdated(request, preparation, invalid)) return result('Review details changed', 'Check readiness again before choosing a model or starting the review.', 'prepare', 'Check review readiness', 'attention');
    const checks = {
      checks_pending: ['Waiting for GitHub checks', 'Checks have not finished. Check readiness again after they complete.'],
      checks_failing: ['GitHub checks need attention', 'Resolve the failing checks for this commit, then check readiness again.'],
      checks_unknown: ['Could not determine check results', 'Inspect the checks on GitHub, then check readiness again.'],
      no_checks: ['No checks reported', 'A review requires passing checks for this commit. Inspect CI on GitHub, then check readiness again.'],
    };
    if (checks[preparation.readiness]) return result(...checks[preparation.readiness], 'checks', 'View checks on GitHub', 'warning');
    if (preparation.readiness === 'pull_closed') return result('Pull request closed', 'New reviews cannot start for a closed pull request.', 'github', 'Open pull request');
    if (preparation.readiness === 'awaiting_authorization') return result('Waiting for your permission', 'The preparation refreshed GitHub inputs. Inspect them and allow the review again.', 'authorize', 'Review permissions', 'attention');
    if (preparation.readiness === 'discussion_not_observed') return result('Author discussion needs checking', 'Refresh from GitHub before continuing.', 'refresh', 'Fetch pull request details');
    if (preparation.readiness === 'ready' && !request.validated_model) return result('Waiting for the saved model choice', 'Local updates will confirm the saved choice. Check readiness again if the choice was cleared.', 'prepare', 'Check review readiness');
    if (preparation.readiness === 'ready' && preparation.validated_model === selectedModel && request.validated_model?.model === preparation.validated_model && request.validated_model?.agent === preparation.models.find((choice) => choice.model === selectedModel)?.agent) return result('Ready to start your review', 'The model is confirmed. Starting will recheck GitHub, permission, and the review environment.', 'run', 'Start this review', 'success');
    return result('Choose your review model', 'Choose a model in the preparation panel and confirm it before starting.', 'model', 'Choose review model', 'attention');
  }
  const retry = ['failed', 'interrupted'].includes(round?.status) || request.run?.status === 'failed';
  return result(retry ? 'Review needs attention' : round?.status === 'rejected' ? 'Draft rejected; nothing posted' : 'Permission granted', retry ? 'Read the failure details below, resolve the cause, then check readiness again.' : 'Check GitHub checks and choose a review model before starting.', 'prepare', 'Check review readiness', retry ? 'warning' : 'neutral');
}
