<script>
  let { request, signedIn, action, onRefresh } = $props();
  let expanded = $state(false);

  const authorizationLabels = {
    awaiting_lead: 'Awaiting lead',
    one_round: 'One round',
    pr_cycle: 'PR cycle',
  };

  const roundLabels = {
    starting: 'Starting',
    running: 'Running',
    awaiting_decision: 'Awaiting decision',
    publishing: 'Publishing',
    publication_unknown: 'Publication unknown',
    published: 'Published',
    rejected: 'Rejected',
    failed: 'Failed',
    interrupted: 'Interrupted',
  };
</script>

<li class="request-item">
  <div class="request-row">
    <div class="field">
      <p class="field-label">Pull request</p>
      <p class="request-name">{request.repository} <span>#{request.number}</span></p>
    </div>
    <div class="field">
      <p class="field-label">Recorded inputs</p>
      <code class="head-sha">{request.head_sha}</code>
      <p class="secondary">Revision {request.revision}</p>
    </div>
    <div class="field">
      <p class="field-label">Authorization</p>
      <span class="badge" data-state={request.authorization}>
        {authorizationLabels[request.authorization] ?? 'Unknown'}
      </span>
    </div>
    <div class="field">
      <p class="field-label">Latest round</p>
      {#if request.latest_round}
        <p class="round-status">{roundLabels[request.latest_round.status] ?? 'Unknown'}</p>
        <p class="secondary">{request.latest_round.id}</p>
        {#if request.latest_round.cleanup_status === 'needs_attention'}
          <p class="warning">
            {request.latest_round.status === 'starting' || request.latest_round.status === 'running'
              ? 'Review stopped; cleanup may be required'
              : 'Cleanup pending'}
          </p>
        {:else if request.latest_round.cleanup_status === 'unknown'}
          <p class="secondary">Cleanup status unavailable</p>
        {:else if request.latest_round.cleanup_status === 'in_progress' &&
          request.latest_round.status !== 'starting' && request.latest_round.status !== 'running'}
          <p class="secondary">Finishing cleanup…</p>
        {/if}
      {:else}
        <p class="secondary">No rounds yet</p>
      {/if}
    </div>
  </div>
  <div class="details-toggle">
    <!-- Material Web handles keyboard activation inside its button shadow DOM. -->
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-text-button
      aria-expanded={expanded}
      aria-label={`${expanded ? 'Hide' : 'Show'} details for ${request.repository} #${request.number}`}
      onclick={() => expanded = !expanded}
    >
      {expanded ? 'Hide details' : 'Show details'}
    </md-text-button>
    {#if signedIn}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-text-button
        disabled={action?.pending}
        aria-label={`Refresh ${request.repository} #${request.number} from GitHub`}
        onclick={() => onRefresh()}
      >{action?.pending ? 'Refreshing…' : 'Refresh from GitHub'}</md-text-button>
    {/if}
  </div>
  {#if signedIn}
    <p class="action-note">Changed inputs can clear one-round authorization and model validation.</p>
  {/if}
  {#if action?.message}
    <p class:action-error={action.error} class="action-note" role={action.error ? 'alert' : 'status'}>
      {action.message}
    </p>
  {/if}
  <div class="request-details" hidden={!expanded}>
    <div class="detail-group">
      <h3>Author discussion</h3>
      {#if request.discussion}
        <dl class="detail-list">
          <div><dt>Issue comments</dt><dd>{request.discussion.issue_comments}</dd></div>
          <div><dt>Review comments</dt><dd>{request.discussion.review_comments}</dd></div>
          <div><dt>Review bodies</dt><dd>{request.discussion.review_bodies}</dd></div>
        </dl>
      {:else}
        <p class="detail-empty">Not yet observed</p>
      {/if}
    </div>
    <div class="detail-group">
      <h3>Current model validation</h3>
      {#if request.validated_model}
        <dl class="detail-list">
          <div><dt>Model</dt><dd>{request.validated_model.model}</dd></div>
          <div><dt>Agent</dt><dd>{request.validated_model.agent}</dd></div>
        </dl>
      {:else}
        <p class="detail-empty">No validated model for current inputs</p>
      {/if}
    </div>
    <div class="detail-group">
      <h3>Latest round</h3>
      {#if request.latest_round}
        <dl class="detail-list">
          <div><dt>ID</dt><dd><code>{request.latest_round.id}</code></dd></div>
          <div><dt>Status</dt><dd>{roundLabels[request.latest_round.status] ?? 'Unknown'}</dd></div>
          <div><dt>Round head</dt><dd><code>{request.latest_round.head_sha}</code></dd></div>
          <div><dt>Input revision</dt><dd>{request.latest_round.revision}</dd></div>
          <div><dt>Model</dt><dd>{request.latest_round.model}</dd></div>
          <div><dt>Agent</dt><dd>{request.latest_round.agent}</dd></div>
        </dl>
      {:else}
        <p class="detail-empty">No rounds yet</p>
      {/if}
    </div>
  </div>
  <md-divider></md-divider>
</li>
