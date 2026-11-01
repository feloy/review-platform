<script>
  let { request, draft, onClose } = $props();
  const changed = $derived(draft.head_sha !== request.head_sha || draft.revision !== request.revision ||
    draft.round_id !== request.latest_round?.id || draft.decision !== request.latest_round?.status);
  const freshnessLabels = { fresh: 'Fresh at inspection', stale: 'Stale', unknown: 'Unknown' };
  const verdictLabels = { ACCEPT: 'Accept', ACCEPT_WITH_NITS: 'Accept with nits', REQUEST_CHANGES: 'Request changes' };
  const decisionLabels = {
    awaiting_decision: 'Awaiting lead decision', publishing: 'Publishing', publication_unknown: 'Publication unknown',
    published: 'Published', rejected: 'Rejected',
  };
</script>

<section class="draft-panel" aria-label={`Private draft for ${request.repository} #${request.number}`}>
  <div class="draft-heading">
    <h3>Private draft</h3>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-text-button onclick={onClose}>Close draft</md-text-button>
  </div>
  <dl class="detail-list">
    <div><dt>Round</dt><dd><code>{draft.round_id}</code></dd></div>
    <div><dt>Reviewed head</dt><dd><code>{draft.head_sha}</code></dd></div>
    <div><dt>Input revision</dt><dd>{draft.revision}</dd></div>
    <div><dt>Review model</dt><dd>{draft.model}</dd></div>
    <div><dt>Agent</dt><dd>{draft.agent}</dd></div>
    <div><dt>Agent verdict</dt><dd>{verdictLabels[draft.verdict] ?? 'Unknown'}</dd></div>
    <div><dt>Decision at inspection</dt><dd>{decisionLabels[draft.decision] ?? 'Unknown'}</dd></div>
    <div><dt>Freshness</dt><dd>{freshnessLabels[draft.freshness] ?? 'Unknown'}</dd></div>
    <div><dt>Publication body SHA-256</dt><dd><code>{draft.publication_body_sha256}</code></dd></div>
  </dl>
  <p>Freshness is checked against GitHub when this draft opens. Reopen it to check again.</p>
  {#if draft.freshness === 'stale'}
    <p class="action-error" role="alert">The PR inputs changed or the PR closed. This draft is stale.</p>
  {:else if draft.freshness !== 'fresh'}
    <p class="warning" role="status">Current PR freshness could not be verified. Recheck GitHub before deciding on this draft.</p>
  {/if}
  {#if changed}
    <p class="warning" role="status">Local inputs or round status changed. This view keeps the draft you opened. Reopen it to inspect current state.</p>
  {/if}
  <h4>Review comments</h4>
  <p>This is the exact GitHub publication body, including its round footer.</p>
  <!-- Keyboard focus lets the lead scroll long draft text without a pointer. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="draft-text" role="region" aria-label="Review comments" tabindex="0"><pre>{draft.publication_body}</pre></div>
  <h4>Private lead advice</h4>
  <p>This advice stays private when the review is published.</p>
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="draft-text" role="region" aria-label="Private lead advice" tabindex="0"><pre>{draft.lead_advice || 'No private lead advice.'}</pre></div>
  <p>Use the CLI to publish or reject this draft.</p>
</section>
