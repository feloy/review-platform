<script>
  import PreparationPanel from './PreparationPanel.svelte';
  import ReviewProgress from './ReviewProgress.svelte';
  import DraftPanel from './DraftPanel.svelte';
  import TimeAgo from './TimeAgo.svelte';
  import { reviewSituation } from './request-status.js';
  import { tick } from 'svelte';

  let { request, signedIn, checkingSession, action, onRefresh, onAuthorize, onRevoke, onUnmanage, onPrepare, onValidate, onRun, onViewDraft, onRejectDraft, onPublishDraft } = $props();
  let expanded = $state(false);
  let menuButton = $state();
  let actionsMenu = $state();
  let menuOpen = $state(false);
  let itemElement;
  let selectedModel = $state('');
  let reviewMode = $state('auto');
  let inspected = $state(null);
  let preparation = $state(null);
  let preparationInvalid = $state(false);
  const situation = $derived(reviewSituation(request, preparation, preparationInvalid, selectedModel));
  const pullUrl = $derived(`https://github.com/${request.repository}/pull/${request.number}`);

  function toggleMenu() {
    if (actionsMenu.open) actionsMenu.close();
    else {
      actionsMenu.defaultFocus = 'first-item';
      actionsMenu.show();
    }
  }

  function menuKeydown(event) {
    if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
    event.preventDefault();
    actionsMenu.defaultFocus = event.key === 'ArrowUp' ? 'last-item' : 'first-item';
    actionsMenu.show();
  }

  function selectMenuAction(event, operation) {
    // Material reports mouse and keyboard selection through close-menu.
    // Escape only dismisses; disabled custom elements must not run an action.
    if (event.detail.reason.key !== 'Escape' && !event.currentTarget.disabled) operation();
  }

  async function nextAction() {
    if (!signedIn || action?.pending) return;
    switch (situation.action) {
      case 'refresh': await onRefresh(); break;
      case 'authorize': inspected = { ...request }; break;
      case 'prepare': await prepareReview(); break;
      case 'draft': await viewDraft(); break;
      case 'run': await runReview(selectedModel); break;
      case 'model':
        await tick();
        itemElement?.querySelector('.preparation-panel')?.scrollIntoView?.({ block: 'nearest', behavior: 'smooth' });
        itemElement?.querySelector('.model-choice select')?.focus();
        break;
    }
  }
  let draft = $state(null);
  let confirmingRemoval = $state(false);
  const hasDraft = $derived(['awaiting_decision', 'publishing', 'publication_unknown', 'published', 'rejected']
    .includes(request.latest_round?.status));
  const managementBlocked = $derived(
    ['starting', 'running'].includes(request.run?.status) || request.publication?.status === 'active' ||
    request.latest_round?.cleanup_pending ||
    ['starting', 'running', 'publishing', 'publication_unknown'].includes(request.latest_round?.status));
  const compactClosed = $derived(
    request.context?.state === 'closed' &&
    request.latest_round?.status !== 'awaiting_decision' &&
    !managementBlocked);
  $effect(() => { if (!signedIn) { draft = null; confirmingRemoval = false; } });
  const inputsChanged = $derived(inspected &&
    (inspected.head_sha !== request.head_sha || inspected.revision !== request.revision));

  async function authorize(scope) {
    if (!inspected || inputsChanged || action?.pending) return;
    if (await onAuthorize(inspected, scope)) inspected = null;
  }

  async function removeRequest() {
    if (action?.pending || managementBlocked) return;
    if (await onUnmanage()) confirmingRemoval = false;
  }

  async function prepareReview() {
    if (action?.pending) return;
    const result = await onPrepare();
    if (result) {
      preparation = result;
      preparationInvalid = false;
    } else if (preparation) preparationInvalid = true;
  }

  async function validateModel(model) {
    if (!preparation || preparationInvalid || action?.pending) return;
    const result = await onValidate(preparation, model);
    if (result) preparation = result;
    else preparationInvalid = true;
  }

  async function runReview(model) {
    if (!preparation || preparationInvalid || action?.pending) return;
    const result = await onRun(preparation, model, reviewMode);
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

<li class="request-item" class:compact-closed={compactClosed} bind:this={itemElement}>
  <header class="request-heading">
    <div class="request-identity">
      <p class="field-label">Pull request</p>
      {#if request.context?.title}<p class="pr-title"><a href={pullUrl} target="_blank" rel="noopener noreferrer">{request.context.title}</a></p>{/if}
      <p class="request-name"><a href={pullUrl} target="_blank" rel="noopener noreferrer">{request.repository} <span>#{request.number}</span></a></p>
      {#if request.context}<p class="secondary">by @{request.context.author} · {request.context.state === 'closed' ? 'Closed' : request.context.draft ? 'Draft PR' : 'Open'}</p>{/if}
    </div>
    {#if !compactClosed || signedIn}
      <div class="request-actions">
      <!-- Material Web supplies keyboard activation inside its shadow DOM. -->
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-icon-button bind:this={menuButton}
        aria-label={`More actions for ${request.repository} #${request.number}`}
        aria-haspopup="menu" aria-expanded={menuOpen}
        title="More actions" onclick={toggleMenu} onkeydown={menuKeydown}>
        <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
          <path d="M12 8a2 2 0 1 0 0-4 2 2 0 0 0 0 4Zm0 2a2 2 0 1 0 0 4 2 2 0 0 0 0-4Zm0 6a2 2 0 1 0 0 4 2 2 0 0 0 0-4Z" />
        </svg>
      </md-icon-button>
      <md-menu bind:this={actionsMenu} anchorElement={menuButton}
        positioning="popover" anchor-corner="end-end" menu-corner="start-end"
        aria-label={`Actions for ${request.repository} #${request.number}`}
        onopening={() => menuOpen = true} onclosing={() => menuOpen = false}>
        {#if !compactClosed}
          <md-menu-item type="button" aria-expanded={expanded}
            aria-label={`${expanded ? 'Hide' : 'Show'} details for ${request.repository} #${request.number}`}
            onclose-menu={(event) => selectMenuAction(event, () => expanded = !expanded)}>
            <div slot="headline">{expanded ? 'Hide details' : 'Show details'}</div>
          </md-menu-item>
        {/if}
        {#if signedIn}
          {#if !compactClosed && hasDraft}
            <md-menu-item type="button" disabled={action?.pending}
              aria-label={`Read private draft for ${request.repository} #${request.number}`}
              onclose-menu={(event) => selectMenuAction(event, viewDraft)}>
              <div slot="headline">{action?.pending && action.operation === 'view-draft' ? 'Loading draft…' : 'Read private draft'}</div>
            </md-menu-item>
          {/if}
          <md-menu-item type="button" disabled={action?.pending}
            aria-label={`Refresh ${request.repository} #${request.number} from GitHub`}
            onclose-menu={(event) => selectMenuAction(event, onRefresh)}>
            <div slot="headline">{action?.pending && action.operation === 'refresh' ? 'Refreshing…' : 'Refresh from GitHub'}</div>
          </md-menu-item>
          {#if !compactClosed}
            <md-menu-item type="button" disabled={action?.pending || !request.discussion}
              aria-expanded={Boolean(inspected)}
              aria-label={`Manage authorization for ${request.repository} #${request.number}`}
              onclose-menu={(event) => selectMenuAction(event, () => inspected = { ...request })}>
              <div slot="headline">Manage authorization</div>
            </md-menu-item>
            <md-menu-item type="button" disabled={action?.pending}
              aria-label={`Check review readiness for ${request.repository} #${request.number}`}
              onclose-menu={(event) => selectMenuAction(event, prepareReview)}>
              <div slot="headline">{action?.pending && action.operation === 'prepare' ? 'Preparing…' : 'Check review readiness'}</div>
            </md-menu-item>
            {#if request.authorization !== 'awaiting_lead' || request.validated_model}
              <md-menu-item type="button" disabled={action?.pending}
                aria-label={`Revoke authorization for ${request.repository} #${request.number}`}
                onclose-menu={(event) => selectMenuAction(event, onRevoke)}>
                <div slot="headline">{action?.pending && action.operation === 'revoke' ? 'Revoking…' : 'Revoke authorization'}</div>
              </md-menu-item>
            {/if}
          {/if}
          {#if !compactClosed}<md-divider></md-divider>{/if}
          <md-menu-item type="button" disabled={action?.pending || managementBlocked}
            aria-label={`Stop managing ${request.repository} #${request.number}`}
            title={managementBlocked
              ? 'Finish or recover the active review, cleanup, or publication before stopping management.'
              : 'Permanently delete this PR and its local drafts, logs, and review history.'}
            onclose-menu={(event) => selectMenuAction(event, () => confirmingRemoval = true)}>
            <div slot="headline">{action?.pending && action.operation === 'unmanage' ? 'Stopping management…' : 'Stop managing PR'}</div>
          </md-menu-item>
        {/if}
      </md-menu>
      </div>
    {/if}
  </header>
  {#if signedIn && confirmingRemoval}
    <section class="authorization-panel" aria-label={`Stop managing ${request.repository} #${request.number}`}>
      <h3>Stop managing PR</h3>
      <p>Permanently remove {request.repository} #{request.number} and all its local drafts, logs, and review history.</p>
      <p>This cannot be undone. Published reviews remain on GitHub.</p>
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-filled-button disabled={action?.pending || managementBlocked} onclick={removeRequest}>
        Confirm removal
      </md-filled-button>
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-text-button disabled={action?.pending} onclick={() => confirmingRemoval = false}>Cancel removal</md-text-button>
    </section>
  {/if}
  {#if !compactClosed}
    <section class="next-step" data-tone={situation.tone} aria-label={`Next step for ${request.repository} #${request.number}`}>
    <div>
      <h3>{situation.title}</h3>
      <p>{situation.message}</p>
      <p class="github-observation">
        {#if request.context}
          GitHub last checked <TimeAgo value={request.context.checked_at} />
          · <a href={`https://github.com/${request.repository}/commit/${request.context.head_sha}`} target="_blank" rel="noopener noreferrer">commit {request.context.head_sha.slice(0, 7)}</a>
          {#if request.context.checks} · Checks: {{ green: 'passed', pending: 'pending', failing: 'failing', unknown: 'unknown', no_results: 'none reported' }[request.context.checks]}{/if}
        {:else}
          No GitHub check details are available for these inputs in this server session.
        {/if}
      </p>
      {#if request.context?.head_changed || request.context?.discussion_changed}
        <p class="observed-change">Last GitHub check: {request.context.head_changed && request.context.discussion_changed ? 'the PR commit and author discussion changed' : request.context.head_changed ? 'the PR commit changed' : 'the author discussion changed'}.</p>
      {/if}
    </div>
    {#if situation.action === 'github' || situation.action === 'checks' || situation.action === 'receipt'}
      <a class="primary-link" href={situation.action === 'checks' ? `https://github.com/${request.repository}/commit/${preparation?.head_sha ?? request.head_sha}/checks` : situation.action === 'receipt' ? request.latest_round?.receipt?.url ?? pullUrl : pullUrl} target="_blank" rel="noopener noreferrer">{situation.label}</a>
    {:else if situation.action && signedIn}
      <!-- Material Web supplies keyboard activation inside its shadow DOM. -->
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-filled-button disabled={action?.pending} onclick={nextAction}>{situation.label}</md-filled-button>
    {:else if situation.action && !checkingSession}
      <a href="#sign-in-heading">Sign in to continue</a>
    {/if}
    </section>
    <div class="request-row">
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
        {#if request.latest_round.task}
          <p class="secondary">{request.latest_round.task.scope.mode === 'full'
            ? 'Full PR review' : `Incremental since ${request.latest_round.task.scope.since.slice(0, 7)}`}</p>
        {/if}
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
        bind:reviewMode onRefresh={prepareReview} onSelect={(model) => selectedModel = model} onValidate={validateModel} onRun={runReview} onClose={() => preparation = null} />
    {/if}
    {#if draft}
      <DraftPanel {request} {draft} {action} onReject={onRejectDraft} onPublish={onPublishDraft} onClose={() => draft = null} />
    {/if}
    {/if}
  {/if}
  {#if action?.message}
    <p class:action-error={action.error} class="action-note" role={action.error ? 'alert' : 'status'}>
      {action.message}
    </p>
  {/if}
  {#if !compactClosed}
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
  {/if}
  <md-divider></md-divider>
</li>
