<script>
  import { preparationOutdated } from './request-status.js';
  import ButtonHelp from './ButtonHelp.svelte';
  import Panel from './Panel.svelte';
  let { request, preparation, invalid, action, onRefresh, onValidate, onClose, selectedModel = $bindable(''), reviewMode = $bindable('auto') } = $props();
  const scopeHelpId = $props.id();

  const stale = $derived(preparationOutdated(request, preparation, invalid));
  const permitsValidation = $derived(preparation.readiness === 'ready' ||
    preparation.readiness === 'model_validation_required');
  const selected = $derived(preparation.models.find((choice) => choice.model === selectedModel));
  const modelValidated = $derived(!stale && preparation.readiness === 'ready' &&
    preparation.validated_model === selectedModel);
  const readinessReasons = {
    awaiting_authorization: 'Give permission for this review before confirming a model.',
    discussion_not_observed: 'Refresh the author discussion from GitHub before confirming a model.',
    pull_closed: 'A model cannot be confirmed for a closed pull request.',
    checks_pending: 'Wait for GitHub checks to finish, then use Refresh readiness.',
    checks_failing: 'Resolve the failing GitHub checks, then use Refresh readiness.',
    checks_unknown: 'Check the unknown CI results on GitHub, then use Refresh readiness.',
    no_checks: 'Wait for GitHub to report passing checks for this commit, then use Refresh readiness.',
  };
  const readinessReason = $derived(readinessReasons[preparation.readiness] ?? 'Check review readiness before confirming a model.');
  const validationReason = $derived(action?.pending ? 'Wait for the current action to finish.'
    : stale ? 'Refresh readiness and inspect the current PR, permission, and model configuration before confirming.'
    : !permitsValidation ? readinessReason
    : !selected ? 'Choose an available review model first.'
    : modelValidated ? 'This model is already confirmed for this commit and author discussion.'
    : 'Save this model choice after rechecking the PR, permission, configuration, and GitHub checks.');
  const selectionDisabled = $derived(action?.pending || stale || !permitsValidation);
  const validationDisabled = $derived(selectionDisabled || !selected || modelValidated);
  const checkLabels = {
    green: 'Passing', pending: 'Pending', failing: 'Failing', unknown: 'Unknown', no_results: 'No checks reported',
  };
</script>

<Panel title="Choose model" label={`Choose model for ${request.repository} #${request.number}`}
  closeLabel="Close model choice" closeDisabled={action?.pending} {onClose}>
  <p>Inspect the <a href={`https://github.com/${request.repository}/pull/${request.number}`}
    target="_blank" rel="noopener noreferrer">PR on GitHub</a> at commit <a
    href={`https://github.com/${request.repository}/commit/${preparation.head_sha}`}
    title={preparation.head_sha} target="_blank" rel="noopener noreferrer"><code>{preparation.head_sha.slice(0, 7)}</code></a>
    before confirming the model.</p>
  <p>GitHub checks: <strong>{checkLabels[preparation.checks] ?? 'Unknown'}</strong>.</p>
  {#if stale}
    <p class="action-error" role="alert">Review readiness is outdated. Use Refresh readiness, then inspect the PR again.</p>
  {:else if !permitsValidation}
    <p class="warning">{readinessReason}</p>
  {/if}
  <label class="model-choice">
    Review model
    <select aria-label={`Review model for ${request.repository} #${request.number}`}
      bind:value={selectedModel} disabled={selectionDisabled}>
      {#each preparation.models as choice (choice.model)}
        <option value={choice.model}>{choice.model}{choice.model === preparation.proposed_model ? ' (suggested)' : ''}</option>
      {/each}
    </select>
  </label>
  <p class="secondary">Reviewer: <strong>{selected?.agent ?? 'Unavailable'}</strong></p>
  {#if preparation.proposal_matches_coauthor}
    <p class="warning">The suggested model matches a PR co-author hint because no different model is configured.</p>
  {/if}
  <label class="model-choice">
    Review scope
    <select aria-label={`Review scope for ${request.repository} #${request.number}`} aria-describedby={scopeHelpId}
      bind:value={reviewMode} disabled={selectionDisabled}>
      <option value="auto">Automatic</option>
      <option value="full">Full PR</option>
    </select>
  </label>
  <p class="secondary" id={scopeHelpId}>{reviewMode === 'full' ? 'Review all changes in this PR.'
    : 'Review changes since the last published review. Review the full PR when there is no usable earlier review.'}</p>
  {#if modelValidated}
    <p>This model is confirmed for this commit and author discussion.</p>
  {/if}
  <p class="note" role="note">Note: Confirming a model does not start a review.</p>
  {#snippet actions()}
    <ButtonHelp text="Fetch the current PR, permission, model configuration, and GitHub checks."
      label="Refresh readiness" disabled={action?.pending}>
    {#snippet children(descriptionId)}
    <!-- Material Web handles keyboard activation inside its button shadow DOM. -->
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-text-button disabled={action?.pending} aria-describedby={descriptionId} onclick={onRefresh}>
      {action?.pending && action.operation === 'prepare' ? 'Refreshing readiness…' : 'Refresh readiness'}
    </md-text-button>
    {/snippet}
    </ButtonHelp>
    <ButtonHelp text={validationReason} label="Confirm model" disabled={validationDisabled}>
    {#snippet children(descriptionId)}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-filled-button disabled={validationDisabled} aria-describedby={descriptionId}
      onclick={() => { if (!validationDisabled) onValidate(selectedModel); }}>
      {action?.pending && action.operation === 'validate-model' ? 'Confirming model…' : 'Confirm model'}
    </md-filled-button>
    {/snippet}
    </ButtonHelp>
  {/snippet}
</Panel>
