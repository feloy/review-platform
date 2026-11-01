<script>
  let { request, preparation, invalid, action, onValidate, onRun, onClose } = $props();
  let selectedModel = $state('');
  $effect(() => { selectedModel = preparation.resolved_model; });

  const stale = $derived(invalid || preparation.head_sha !== request.head_sha ||
    preparation.revision !== request.revision || preparation.authorization !== request.authorization ||
    (preparation.validated_model && request.validated_model &&
      preparation.validated_model !== request.validated_model.model));
  const runBlocked = $derived(['starting', 'running'].includes(request.run?.status) ||
    request.latest_round?.cleanup_pending ||
    ['starting', 'running', 'awaiting_decision', 'publishing', 'publication_unknown'].includes(request.latest_round?.status));
  const permitsValidation = $derived(preparation.readiness === 'ready' ||
    preparation.readiness === 'model_validation_required');
  const selected = $derived(preparation.models.find((choice) => choice.model === selectedModel));
  const modelValidated = $derived(!stale && preparation.readiness === 'ready' &&
    preparation.validated_model === selectedModel);
  const currentModelValidated = $derived(modelValidated && request.validated_model?.model === selectedModel &&
    request.validated_model?.agent === selected?.agent);
  const checkLabels = {
    green: 'Green', pending: 'Pending', failing: 'Failing', unknown: 'Unknown', no_results: 'No checks reported',
  };
</script>

<section class="preparation-panel" aria-label={`Review preparation for ${request.repository} #${request.number}`}>
  <h3>Review preparation</h3>
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
  {#if modelValidated}
    <p>This model is validated for the prepared head and revision. Choose a different model to change it.</p>
  {:else}
    <p>Inspect this head and revision before confirming the model. Validation refreshes GitHub inputs and checks again.</p>
  {/if}
  {#if stale}
    <p class="action-error" role="alert">This preparation is outdated. Prepare the review again and inspect the current inputs and model.</p>
  {/if}
  <div class="authorization-buttons">
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-filled-button disabled={action?.pending || stale || !permitsValidation || !selected || modelValidated}
      onclick={() => onValidate(selectedModel)}>
      {action?.pending && action.operation === 'validate-model' ? 'Validating model…' : modelValidated ? 'Model validated' : 'Validate model'}
    </md-filled-button>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-filled-button disabled={action?.pending || !currentModelValidated || runBlocked}
      onclick={() => onRun(selectedModel)}>
      {action?.pending && action.operation === 'run' ? 'Starting review…' : 'Run review'}
    </md-filled-button>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-text-button disabled={action?.pending} onclick={onClose}>Close preparation</md-text-button>
  </div>
  <p>Running rechecks GitHub inputs, checks, authorization, configuration, and gateway readiness.</p>
  {#if modelValidated && !currentModelValidated}
    <p>Waiting for the saved model validation. Prepare again if it was cleared.</p>
  {/if}
  {#if runBlocked}<p>A review is active, needs a lead decision, or requires sandbox cleanup before another round.</p>{/if}
</section>
