<script>
  import PreparationPanel from './PreparationPanel.svelte';
  import ReviewProgress from './ReviewProgress.svelte';
  import DraftPanel from './DraftPanel.svelte';

  let { request, signedIn, action, onRefresh, onAuthorize, onRevoke, onPrepare, onValidate, onRun, onViewDraft, onRejectDraft, onPublishDraft } = $props();
  let expanded = $state(false);
  let inspected = $state(null);
  let preparation = $state(null);
  let preparationInvalid = $state(false);
  let draft = $state(null);
  const hasDraft = $derived(['awaiting_decision', 'publishing', 'publication_unknown', 'published', 'rejected']
    .includes(request.latest_round?.status));
  $effect(() => { if (!signedIn) draft = null; });
  const inputsChanged = $derived(inspected &&
    (inspected.head_sha !== request.head_sha || inspected.revision !== request.revision));

  async function authorize(scope) {
    if (!inspected || inputsChanged || action?.pending) return;
    if (await onAuthorize(inspected, scope)) inspected = null;
  }

  async function prepareReview() {
    if (action?.pending) return;
    preparation = null;
    preparationInvalid = false;
    const result = await onPrepare();
    if (result) preparation = result;
  }

  async function validateModel(model) {
    if (!preparation || preparationInvalid || action?.pending) return;
    const result = await onValidate(preparation, model);
    if (result) preparation = result;
    else preparationInvalid = true;
  }

  async function runReview(model) {
    if (!preparation || preparationInvalid || action?.pending) return;
    const result = await onRun(preparation, model);
    preparationInvalid = true;
    if (result) preparation = null;
  }

  async function viewDraft() {
    if (!signedIn || !hasDraft || action?.pending) return;
    const roundId = request.latest_round.id;
    draft = null;
    const result = await onViewDraft(roundId);
    if (result && signedIn) draft = result;
  }

  const authorizationLabels = {
    awaiting_lead: 'Awaiting lead',
    one_round: 'One round',
    pr_cycle: 'PR cycle',
  };

  const roundLabels = {
    starting: 'Starting',
    running: 'Running',
    awaiting_decision: 'Draft ready',
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
      {#if hasDraft}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <md-text-button disabled={action?.pending} onclick={viewDraft}
          aria-label={`View private draft for ${request.repository} #${request.number}`}>
          {action?.pending && action.operation === 'view-draft' ? 'Loading draft…' : 'View draft'}
        </md-text-button>
      {/if}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-text-button
        disabled={action?.pending}
        aria-label={`Refresh ${request.repository} #${request.number} from GitHub`}
        onclick={() => onRefresh()}
      >{action?.pending && action.operation === 'refresh' ? 'Refreshing…' : 'Refresh from GitHub'}</md-text-button>
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-text-button
        disabled={action?.pending || !request.discussion}
        aria-expanded={Boolean(inspected)}
        aria-label={`Manage authorization for ${request.repository} #${request.number}`}
        onclick={() => inspected = { ...request }}
      >Manage authorization</md-text-button>
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-text-button disabled={action?.pending}
        aria-label={`Prepare review for ${request.repository} #${request.number}`}
        onclick={prepareReview}>
        {action?.pending && action.operation === 'prepare' ? 'Preparing…' : 'Prepare review'}
      </md-text-button>
      {#if request.authorization !== 'awaiting_lead' || request.validated_model}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <md-text-button disabled={action?.pending}
          aria-label={`Revoke authorization for ${request.repository} #${request.number}`}
          onclick={() => onRevoke()}>
          {action?.pending && action.operation === 'revoke' ? 'Revoking…' : 'Revoke authorization'}
        </md-text-button>
      {/if}
    {/if}
  </div>
  {#if signedIn}
    <p class="action-note">Changed inputs can clear one-round authorization and model validation.</p>
    {#if !request.discussion}
      <p class="action-note">Refresh from GitHub before authorizing this request.</p>
    {/if}
    {#if inspected}
      <section class="authorization-panel" aria-label={`Authorize ${request.repository} #${request.number}`}>
        <h3>Authorize review</h3>
        <p>Inspect the <a href={`https://github.com/${request.repository}/pull/${request.number}`}
          target="_blank" rel="noopener noreferrer">PR on GitHub</a> before authorizing these inputs.</p>
        <dl class="detail-list">
          <div><dt>Head</dt><dd><code>{inspected.head_sha}</code></dd></div>
          <div><dt>Input revision</dt><dd>{inspected.revision}</dd></div>
        </dl>
        <p>One round permits a single review of these inputs. PR cycle keeps authorization active for later commits and author replies.</p>
        <p>A review also requires green checks and a validated model.</p>
        {#if inputsChanged}
          <p class="action-error" role="alert">Inputs changed. Close this panel and inspect the current head and revision before authorizing.</p>
        {/if}
        <div class="authorization-buttons">
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <md-filled-button disabled={action?.pending || inputsChanged} onclick={() => authorize('one_round')}>
            Authorize one round
          </md-filled-button>
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <md-text-button disabled={action?.pending || inputsChanged} onclick={() => authorize('pr_cycle')}>
            Authorize PR cycle
          </md-text-button>
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <md-text-button disabled={action?.pending} onclick={() => inspected = null}>Close</md-text-button>
        </div>
        {#if action?.pending && action.operation === 'authorize'}
          <p role="status">Saving authorization…</p>
        {/if}
      </section>
    {/if}
    {#if preparation}
      <PreparationPanel {request} {preparation} invalid={preparationInvalid} {action}
        onValidate={validateModel} onRun={runReview} onClose={() => preparation = null} />
    {/if}
    {#if draft}
      <DraftPanel {request} {draft} {action} onReject={onRejectDraft} onPublish={onPublishDraft} onClose={() => draft = null} />
    {/if}
  {/if}
  {#if action?.message}
    <p class:action-error={action.error} class="action-note" role={action.error ? 'alert' : 'status'}>
      {action.message}
    </p>
  {/if}
  <ReviewProgress {request} {signedIn} />
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
