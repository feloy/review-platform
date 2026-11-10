<script>
  import { preparationOutdated, reviewBlocker } from './request-status.js';
  import TimeAgo from './TimeAgo.svelte';
  import ButtonHelp from './ButtonHelp.svelte';
  let { request, preparation, invalid, action, onRefresh, onValidate, onRun, onClose, onSelect = () => {}, reviewMode = $bindable('auto') } = $props();
  let selectedModel = $state('');
  $effect(() => { selectedModel = preparation.resolved_model; });
  $effect(() => { onSelect(selectedModel); });

  const stale = $derived(preparationOutdated(request, preparation, invalid));
  const workflowBlocker = $derived(reviewBlocker(request));
  const permitsValidation = $derived(preparation.readiness === 'ready' ||
    preparation.readiness === 'model_validation_required');
  const selected = $derived(preparation.models.find((choice) => choice.model === selectedModel));
  const modelValidated = $derived(!stale && preparation.readiness === 'ready' &&
    preparation.validated_model === selectedModel);
  const currentModelValidated = $derived(modelValidated && request.validated_model?.model === selectedModel &&
    request.validated_model?.agent === selected?.agent);
  const readinessReasons = {
    awaiting_authorization: 'Give permission for this review before confirming a model.',
    discussion_not_observed: 'Refresh the author discussion from GitHub before confirming a model.',
    pull_closed: 'A model cannot be confirmed for a closed pull request.',
    checks_pending: 'Wait for GitHub checks to finish, then use Refresh readiness.',
    checks_failing: 'Resolve the failing GitHub checks, then use Refresh readiness.',
    checks_unknown: 'Check the unknown CI results on GitHub, then use Refresh readiness.',
    no_checks: 'Wait for GitHub to report passing checks for this commit, then use Refresh readiness.',
  };
  const validationReason = $derived(stale ? 'The PR, permission, or model configuration changed. Refresh readiness before confirming.'
    : !permitsValidation ? readinessReasons[preparation.readiness] ?? 'Check review readiness before confirming a model.'
    : !selected ? 'Choose an available review model first.'
    : modelValidated ? 'This model is already confirmed for the inspected inputs.' : '');
  const runReason = $derived(stale ? 'Refresh readiness and inspect the current PR before starting.'
    : workflowBlocker || (!modelValidated ? 'Confirm the selected review model before starting.'
    : !currentModelValidated ? 'Waiting for the saved model choice. Refresh readiness if it was cleared.' : ''));
  const validationDisabled = $derived(action?.pending || stale || !permitsValidation || !selected || modelValidated);
  const runDisabled = $derived(action?.pending || !currentModelValidated || Boolean(workflowBlocker));
  const checkLabels = {
    green: 'Green', pending: 'Pending', failing: 'Failing', unknown: 'Unknown', no_results: 'No checks reported',
  };
</script>

<section class="preparation-panel" aria-label={`Review preparation for ${request.repository} #${request.number}`}>
  <h3>Review preparation</h3>
  <ul class="readiness-checklist" aria-label="Requirements for this review">
    <li>Permission: {preparation.authorization === 'awaiting_lead' ? 'required' : preparation.authorization === 'one_round' ? 'one review allowed' : 'kept for later reviews'}</li>
    <li>GitHub checks: {checkLabels[preparation.checks] ?? 'Unknown'} for commit {preparation.head_sha.slice(0, 7)}</li>
    <li>Review model: {modelValidated ? `${selectedModel} confirmed` : 'confirmation required'}</li>
  </ul>
  {#if preparation.context}<p>GitHub checked <TimeAgo value={preparation.context.checked_at} />. Starting a review checks it again.</p>{/if}
  <dl class="detail-list">
    <div><dt>Prepared head</dt><dd><code>{preparation.head_sha}</code></dd></div>
    <div><dt>Input revision</dt><dd>{preparation.revision}</dd></div>
    <div><dt>Checks for this head</dt><dd>{checkLabels[preparation.checks] ?? 'Unknown'}</dd></div>
    <div><dt>Readiness</dt><dd>{preparation.readiness_message}</dd></div>
    <div><dt>Proposed model</dt><dd>{preparation.proposed_model}</dd></div>
    {#if preparation.validated_model}
      <div><dt>Validated model</dt><dd>{preparation.validated_model}</dd></div>
    {/if}
  </dl>
  {#if preparation.proposal_matches_coauthor}
    <p>The proposed model matches a PR co-author hint because no different model is configured.</p>
  {/if}
  <label class="model-choice">
    Review model for {request.repository} #{request.number}
    <select bind:value={selectedModel} disabled={action?.pending || stale || !permitsValidation}>
      {#each preparation.models as choice (choice.model)}
        <option value={choice.model}>{choice.model}</option>
      {/each}
    </select>
  </label>
  <p>Resolved agent: <strong>{selected?.agent ?? 'Unavailable'}</strong></p>
  <label class="model-choice">
    Review scope for {request.repository} #{request.number}
    <select bind:value={reviewMode} disabled={action?.pending || stale}>
      <option value="auto">Incremental after a published review</option>
      <option value="full">Full PR review</option>
    </select>
  </label>
  <p>Incremental review starts from the latest confirmed published review.
    The first review, or a force push that breaks that history, gets a full review.</p>
  {#if modelValidated}
    <p>This model is validated for the prepared head and revision. Choose a different model to change it.</p>
  {:else}
    <p>Inspect this head and revision before confirming the model. Validation refreshes GitHub inputs and checks again.</p>
  {/if}
  {#if stale}
    <p class="action-error" role="alert">This preparation is outdated. Use Refresh readiness, then inspect the current inputs and model.</p>
  {/if}
  <div class="authorization-buttons">
    <ButtonHelp text={validationReason} label="Validate model" disabled={validationDisabled}>
    {#snippet children(descriptionId)}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-filled-button disabled={validationDisabled} aria-describedby={descriptionId}
      onclick={() => onValidate(selectedModel)}>
      {action?.pending && action.operation === 'validate-model' ? 'Validating model…' : modelValidated ? 'Model validated' : 'Validate model'}
    </md-filled-button>
    {/snippet}
    </ButtonHelp>
    <ButtonHelp text={runReason} label="Run review" disabled={runDisabled}>
    {#snippet children(descriptionId)}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-filled-button disabled={runDisabled} aria-describedby={descriptionId}
      onclick={() => onRun(selectedModel)}>
      {action?.pending && action.operation === 'run' ? 'Starting review…' : 'Run review'}
    </md-filled-button>
    {/snippet}
    </ButtonHelp>
    <!-- Material Web handles keyboard activation inside its button shadow DOM. -->
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-text-button disabled={action?.pending} onclick={onRefresh}>
      {action?.pending && action.operation === 'prepare' ? 'Refreshing readiness…' : 'Refresh readiness'}
    </md-text-button>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-text-button disabled={action?.pending} onclick={onClose}>Close preparation</md-text-button>
  </div>
  <p>Running rechecks GitHub inputs, checks, authorization, configuration, and gateway readiness.</p>
  {#if modelValidated && !currentModelValidated}
    <p>Waiting for the saved model validation. Refresh readiness if it was cleared.</p>
  {/if}
</section>
