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

export function reviewFailure(request) {
  const round = request.latest_round;
  const publication = round && request.publication?.round_id === round.id ? request.publication : null;
  const failedRun = request.run?.status === 'failed' && (!request.run.round_id || request.run.round_id === round?.id);
  // A job without a round failed during startup, after any previous completed round.
  if (['published', 'rejected'].includes(round?.status) && !(failedRun && !request.run.round_id)) return null;
  if (publication && ['failed', 'unknown'].includes(publication.status) &&
    ['awaiting_decision', 'publishing', 'publication_unknown'].includes(round.status)) {
    return {
      title: publication.status === 'unknown' ? 'Publication uncertain' : 'Publication failed',
      messages: [publication.error || 'Publication did not complete. Inspect the round before deciding again.'],
    };
  }
  if (publication?.status === 'active' || ['publishing', 'publication_unknown'].includes(round?.status) ||
    ['starting', 'running'].includes(request.run?.status)) return null;
  if (!failedRun && !['failed', 'interrupted'].includes(round?.status)) return null;
  const savedDraft = round?.status === 'awaiting_decision';
  const messages = [];
  if (failedRun) messages.push(request.run.failure === 'stale_preparation'
    ? 'Review inputs, authorization, configuration, or model changed. Prepare and inspect the review again.'
    : savedDraft ? 'The private draft was saved, but the runner reported an error. Check sandbox cleanup before continuing.'
    : 'The GUI review job failed. Check the round status, GitHub access, gateway, and configuration, then prepare again.');
  if (round?.failure) {
    const labels = {
      preflight: 'Startup checks failed', checkout: 'Checkout failed', sandbox_create: 'Sandbox creation failed',
      dispatch: 'Agent dispatch failed', timeout: 'Agent timed out', agent: 'Agent failed',
      output: 'Draft validation failed', cleanup: 'Sandbox cleanup failed', recovery: 'Review interrupted',
    };
    messages.push(`${labels[round.failure] ?? 'Review failed'}. Prepare again after resolving the failure and any pending cleanup.`);
  }
  if (!messages.length) messages.push('Inspect the failure before preparing another review.');
  return { title: savedDraft ? 'Draft saved with warning' : 'Review failed', messages };
}

export function reviewSituation(request, preparation, invalid = false, selectedModel = preparation?.resolved_model) {
  const round = request.latest_round;
  const failure = reviewFailure(request);
  const result = (title, message, action, label, tone = 'neutral', secondaryAction = null) => ({ title, message, action, label, tone, secondaryAction });
  if (request.publication?.status === 'active') return result('Publishing…', 'Publication continues if you close this tab.', 'progress', 'Show progress');
  if (round?.status === 'publication_unknown' || round?.status === 'publishing') return result('Publication uncertain', 'Check GitHub before deciding again. Inspect recovery instructions before continuing.', 'progress', 'View issue', 'warning');
  if (round?.cleanup_status === 'needs_attention') return result('Cleanup required', round.status === 'awaiting_decision' ? 'Your draft is saved. Recover the environment before publishing or rejecting it.' : 'Inspect recovery instructions before continuing.', round.status === 'awaiting_decision' ? 'draft' : 'progress', round.status === 'awaiting_decision' ? 'Read draft' : 'View issue', 'warning');
  if (failure && round?.status === 'awaiting_decision') return result(failure.title, 'Inspect the failure and recovery instructions. Your private draft is saved.', 'progress', 'View issue', 'warning');
  if (round?.status === 'awaiting_decision') return result(round.freshness === 'stale' ? 'Draft outdated' : 'Draft ready', round.cleanup_pending ? 'Publishing and rejection become available when cleanup finishes.' : round.freshness === 'stale' ? 'The PR changed or closed. You can read and reject the saved draft.' : 'Inspect the private draft before publishing it unchanged or rejecting it.', 'draft', 'Read draft', round.freshness === 'stale' ? 'warning' : 'attention');
  if (['starting', 'running'].includes(request.run?.status) || ['starting', 'running'].includes(round?.status)) return result(request.run?.status === 'starting' || round?.status === 'starting' ? 'Starting…' : 'Reviewing…', 'The review continues if you close this tab.', 'progress', 'Show progress');
  if (request.context?.state === 'closed') return result('Pull request closed', 'New reviews cannot start for a closed pull request.', 'github', 'Open pull request');
  if (round?.status === 'published' && round.head_sha === request.head_sha && round.revision === request.revision) return result('Published', 'The review is on GitHub. Refresh to check for later changes.', 'receipt', 'View review', 'success');
  if (round?.cleanup_pending) return result('Cleaning up…', 'Wait for the review environment to be removed.', 'progress', 'Show progress');
  if (failure && (!preparation || preparationOutdated(request, preparation, invalid))) return result(failure.title, 'Inspect the failure before preparing another review.', 'progress', 'View issue', 'warning');
  if (!request.discussion) return result('Needs refresh', 'Fetch the current code and author discussion before allowing a review.', 'refresh', 'Refresh PR', 'attention');
  if (request.authorization === 'awaiting_lead') return result('Needs permission', 'Inspect the PR, then allow one review or keep permission for later reviews.', 'authorize', 'Allow review', 'attention');
  if (preparation) {
    if (preparationOutdated(request, preparation, invalid)) return result('Needs refresh', 'Check readiness again before choosing a model or starting the review.', 'prepare', 'Refresh readiness', 'attention');
    if (preparation.readiness === 'checks_pending') return result('CI pending', 'Fetch the latest CI results from GitHub.', 'prepare', 'Recheck CI', 'warning', 'checks');
    const checks = {
      checks_failing: ['CI failing', 'Resolve the failing checks for this commit, then check readiness again.'],
      checks_unknown: ['CI unknown', 'Inspect the checks on GitHub, then check readiness again.'],
      no_checks: ['No CI results', 'A review requires passing checks for this commit. Inspect CI on GitHub, then check readiness again.'],
    };
    if (checks[preparation.readiness]) return result(...checks[preparation.readiness], 'checks', 'View checks on GitHub', 'warning');
    if (preparation.readiness === 'pull_closed') return result('Pull request closed', 'New reviews cannot start for a closed pull request.', 'github', 'Open pull request');
    if (preparation.readiness === 'awaiting_authorization') return result('Needs permission', 'The preparation refreshed GitHub inputs. Inspect them and allow the review again.', 'authorize', 'Allow review', 'attention');
    if (preparation.readiness === 'discussion_not_observed') return result('Needs refresh', 'Refresh from GitHub before continuing.', 'refresh', 'Refresh PR');
    if (preparation.readiness === 'ready' && !request.validated_model) return result('Model unconfirmed', 'Local updates will confirm the saved choice. Check readiness again if the choice was cleared.', 'prepare', 'Check review readiness');
    if (preparation.readiness === 'ready' && preparation.validated_model === selectedModel && request.validated_model?.model === preparation.validated_model && request.validated_model?.agent === preparation.models.find((choice) => choice.model === selectedModel)?.agent) return result('Ready', 'Starting rechecks GitHub, permission, and the review environment.', 'run', 'Start review', 'success');
    return result('Needs model', 'Choose and confirm a model for the inspected inputs.', 'model', 'Choose model', 'attention');
  }
  return result('Needs model', 'Check readiness and choose a review model for the current inputs.', 'prepare', 'Choose model', 'attention');
}
