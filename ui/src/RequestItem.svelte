<script>
  import PreparationPanel from './PreparationPanel.svelte';
  import ReviewProgress from './ReviewProgress.svelte';
  import DraftPanel from './DraftPanel.svelte';
  import TimeAgo from './TimeAgo.svelte';
  import ButtonHelp from './ButtonHelp.svelte';
  import Panel from './Panel.svelte';
  import { reviewSituation } from './request-status.js';
  import { tick } from 'svelte';

  let { request, signedIn, checkingSession, action, refreshQueued = false, onRefresh, onAuthorize, onRevoke, onUnmanage, onPrepare, onValidate, onRun, onViewDraft, onRejectDraft, onPublishDraft } = $props();
  let expanded = $state(false);
  let panel = $state(null);
  const detailsId = $props.id();
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
  const checksUrl = $derived(`https://github.com/${request.repository}/commit/${preparation?.head_sha ?? request.head_sha}/checks`);

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
    if (refreshLabel) return;
    if (situation.action === 'progress') {
      openPanel('progress');
      return;
    }
    if (!signedIn || action?.pending) return;
    switch (situation.action) {
      case 'refresh': await onRefresh(); break;
      case 'authorize': openPermissions(); break;
      case 'prepare': await prepareReview(); break;
      case 'draft': await viewDraft(); break;
      case 'run': await runReview(selectedModel); break;
      case 'model':
        openPanel('preparation');
        await tick();
        itemElement?.querySelector('.preparation-panel')?.scrollIntoView?.({ block: 'nearest', behavior: 'smooth' });
        itemElement?.querySelector('.model-choice select')?.focus();
        break;
    }
  }
  let draft = $state(null);
  function openPanel(next) {
    panel = next;
    if (next !== 'permissions') inspected = null;
    if (next !== 'draft') draft = null;
  }

  function openPermissions() {
    if (action?.pending || !signedIn || panel === 'permissions') return;
    openPanel('permissions');
    inspected = { ...request };
  }

  const refreshLabel = $derived(refreshQueued ? 'Queued for refresh'
    : action?.pending && action.operation === 'refresh' ? 'Refreshing…' : null);
  const primaryDisabled = $derived(Boolean(refreshLabel) || situation.action !== 'progress' && action?.pending);
  const primaryLabel = $derived(refreshLabel ?? (action?.pending && situation.action !== 'progress'
    ? ({ prepare: 'Checking…', 'view-draft': 'Loading draft…', run: 'Starting review…' }[action.operation] ?? situation.label)
    : situation.label));
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
  $effect(() => { if (!signedIn) { openPanel(null); preparation = null; } });
  const inputsChanged = $derived(inspected &&
    (inspected.head_sha !== request.head_sha || inspected.revision !== request.revision));
  const permissionDisabled = $derived(action?.pending || inputsChanged || !inspected?.discussion);
  const oneReviewDisabled = $derived(permissionDisabled || request.authorization === 'one_round');
  const allReviewsDisabled = $derived(permissionDisabled || request.authorization === 'pr_cycle');
  const permissionHelp = $derived(action?.pending ? 'Wait for the current action to finish.'
    : inputsChanged ? 'Close this panel and inspect the updated PR before allowing a review.'
    : !inspected?.discussion ? 'Refresh from GitHub first.' : null);

  async function authorize(scope) {
    if (!inspected?.discussion || inputsChanged || action?.pending || request.authorization === scope) return;
    if (await onAuthorize(inspected, scope)) openPanel(null);
  }

  async function revokePermission() {
    if (action?.pending) return;
    if (await onRevoke()) openPanel(null);
  }

  async function removeRequest() {
    if (action?.pending || managementBlocked) return;
    if (await onUnmanage()) openPanel(null);
  }

  async function prepareReview() {
    if (action?.pending) return;
    openPanel('preparation');
    const result = await onPrepare();
    if (result) {
      preparation = result;
      selectedModel = result.resolved_model;
      preparationInvalid = false;
    } else if (preparation) preparationInvalid = true;
  }

  async function validateModel(model) {
    if (!preparation || preparationInvalid || action?.pending) return;
    const result = await onValidate(preparation, model);
    if (result) {
      preparation = result;
      selectedModel = result.resolved_model;
    } else preparationInvalid = true;
  }

  async function runReview(model) {
    if (!preparation || preparationInvalid || action?.pending) return;
    const result = await onRun(preparation, model, reviewMode);
    preparationInvalid = true;
    if (result) {
      preparation = null;
      openPanel(null);
    }
  }

  async function viewDraft() {
    if (!signedIn || !hasDraft || action?.pending) return;
    const roundId = request.latest_round.id;
    openPanel('draft');
    draft = null;
    const result = await onViewDraft(roundId);
    if (result && signedIn && panel === 'draft') draft = result;
  }

  const authorizationLabels = {
    awaiting_lead: 'Awaiting lead',
    one_round: 'One round',
    pr_cycle: 'PR cycle',
  };

  const permissionLabels = {
    awaiting_lead: 'Not granted',
    one_round: 'One review',
    pr_cycle: 'All reviews for this PR',
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
      <p class="pr-title">
        <a href={pullUrl} target="_blank" rel="noopener noreferrer">
          {#if request.context?.title}{request.context.title}{:else}{request.repository} <span>#{request.number}</span>{/if}
        </a>
      </p>
      {#if request.context}
        <p class="secondary pr-meta">{request.repository} #{request.number} · @{request.context.author} · {request.context.state === 'closed' ? 'Closed' : request.context.draft ? 'Draft' : 'Open'}</p>
      {/if}
    </div>
    {#if signedIn}
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
          <md-menu-item type="button" disabled={action?.pending || refreshQueued}
            aria-label={`Refresh ${request.repository} #${request.number} from GitHub`}
            onclose-menu={(event) => selectMenuAction(event, onRefresh)}>
            <div slot="headline">{action?.pending && action.operation === 'refresh' ? 'Refreshing…' : 'Refresh from GitHub'}</div>
          </md-menu-item>
          {#if !compactClosed}
            <md-menu-item type="button" disabled={action?.pending}
              aria-expanded={panel === 'permissions'}
              aria-label={`Review permissions for ${request.repository} #${request.number}`}
              onclose-menu={(event) => selectMenuAction(event, openPermissions)}>
              <div slot="headline">Review permissions</div>
            </md-menu-item>
          {/if}
          {#if !compactClosed}<md-divider></md-divider>{/if}
          <md-menu-item type="button" disabled={action?.pending || managementBlocked}
            aria-label={`Stop managing ${request.repository} #${request.number}`}
            title={managementBlocked
              ? 'Finish or recover the active review, cleanup, or publication before stopping management.'
              : 'Permanently delete this PR and its local drafts, logs, and review history.'}
            onclose-menu={(event) => selectMenuAction(event, () => openPanel('remove'))}>
            <div slot="headline">{action?.pending && action.operation === 'unmanage' ? 'Stopping management…' : 'Stop managing PR'}</div>
          </md-menu-item>
      </md-menu>
      </div>
    {/if}
  </header>
  {#if signedIn && panel === 'remove'}
    <section class="authorization-panel" aria-label={`Stop managing ${request.repository} #${request.number}`}>
      <header class="panel-heading">
        <h3>Stop managing PR</h3>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <md-text-button disabled={action?.pending} onclick={() => openPanel(null)}>Close removal</md-text-button>
      </header>
      <p>Permanently remove {request.repository} #{request.number} and all its local drafts, logs, and review history.</p>
      <p>This cannot be undone. Published reviews remain on GitHub.</p>
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-filled-button disabled={action?.pending || managementBlocked} onclick={removeRequest}>
        Confirm removal
      </md-filled-button>
    </section>
  {/if}
  {#if !compactClosed}
    <section class="next-step" data-tone={situation.tone} aria-label={`Next step for ${request.repository} #${request.number}`}>
      <h3 class="workflow-status" aria-live="polite">{situation.title}</h3>
      <div class="next-step-actions">
      {#if situation.secondaryAction === 'checks'}
        <a class="secondary-link" href={checksUrl} target="_blank" rel="noopener noreferrer">View checks on GitHub</a>
      {/if}
      {#if !refreshLabel && (situation.action === 'github' || situation.action === 'checks' || situation.action === 'receipt')}
        <ButtonHelp text={situation.message} label={situation.label}>
          {#snippet children(descriptionId)}
            <a class="primary-link" aria-describedby={descriptionId} href={situation.action === 'checks' ? checksUrl : situation.action === 'receipt' ? request.latest_round?.receipt?.url ?? pullUrl : pullUrl} target="_blank" rel="noopener noreferrer">{situation.label}</a>
          {/snippet}
        </ButtonHelp>
      {:else if refreshLabel || situation.action === 'progress' || situation.action && signedIn}
        <ButtonHelp text={refreshQueued ? 'Waiting for earlier PRs to refresh.' : refreshLabel ? 'Fetching the latest PR inputs from GitHub.' : situation.message}
          label={refreshLabel ?? situation.label} disabled={primaryDisabled}>
          {#snippet children(descriptionId)}
            <!-- Material Web supplies keyboard activation inside its shadow DOM. -->
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <md-filled-button disabled={primaryDisabled} aria-describedby={descriptionId}
              aria-expanded={!refreshLabel && situation.action === 'progress' ? panel === 'progress' : undefined}
              aria-controls={!refreshLabel && situation.action === 'progress' ? `${detailsId}-progress` : undefined}
              onclick={nextAction}>{primaryLabel}</md-filled-button>
          {/snippet}
        </ButtonHelp>
      {:else if situation.action && !checkingSession}
        <a href="#sign-in-heading">Sign in to continue</a>
      {/if}
      </div>
    </section>
    <footer class="request-footer">
      <p class="github-observation">
        {#if request.context}
          GitHub checked <TimeAgo value={request.context.checked_at} />
        {:else}
          GitHub not checked
        {/if}
      </p>
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-text-button aria-label={`Details for ${request.repository} #${request.number}`}
        aria-expanded={expanded} aria-controls={detailsId} onclick={() => expanded = true}>Details</md-text-button>
    </footer>
    {#if panel === 'progress'}
      <section class="progress-panel" id={`${detailsId}-progress`} aria-label={`Review progress for ${request.repository} #${request.number}`}>
        <header class="panel-heading">
          <h3>Review progress</h3>
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <md-text-button onclick={() => openPanel(null)}>Close progress</md-text-button>
        </header>
        <ReviewProgress {request} />
        {#if signedIn && hasDraft}
          <ButtonHelp text="Inspect the saved private draft without changing its decision." label="Read draft" disabled={action?.pending}>
            {#snippet children(descriptionId)}
              <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
              <md-text-button disabled={action?.pending} aria-describedby={descriptionId} onclick={viewDraft}>Read draft</md-text-button>
            {/snippet}
          </ButtonHelp>
        {/if}
      </section>
    {/if}
    {#if signedIn}
    {#if panel === 'permissions' && inspected}
      <Panel title="Review permissions" label={`Authorize ${request.repository} #${request.number}`}
        closeLabel="Close permissions" closeDisabled={action?.pending} onClose={() => openPanel(null)}>
        <p>Inspect the <a href={`https://github.com/${request.repository}/pull/${request.number}`}
          target="_blank" rel="noopener noreferrer">PR on GitHub</a> at commit <a
          href={`https://github.com/${request.repository}/commit/${inspected.head_sha}`}
          title={inspected.head_sha} target="_blank" rel="noopener noreferrer"><code>{inspected.head_sha.slice(0, 7)}</code></a>
          before granting permission.</p>
        <p>Current permission: {permissionLabels[request.authorization] ?? 'Unknown'}.</p>
        <p class="note" role="note">Note: Granting permission does not start a review.</p>
        <p class="note" role="note">Note: To change or revoke permission later, open the three-dots menu and choose “Review permissions”.</p>
        {#if !inspected.discussion}<p class="warning">Refresh from GitHub before allowing a review.</p>{/if}
        {#if inputsChanged}
          <p class="action-error" role="alert">The PR changed. Close this panel and inspect it again before granting permission.</p>
        {/if}
        {#if action?.pending && action.operation === 'authorize'}
          <p role="status">Saving permission…</p>
        {/if}
        {#snippet actions()}
          {#if request.authorization !== 'awaiting_lead' || request.validated_model}
            <ButtonHelp text="Prevent new reviews and clear the confirmed model. Saved drafts are kept." label="Revoke permission" disabled={action?.pending}>
            {#snippet children(descriptionId)}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <md-text-button disabled={action?.pending} aria-describedby={descriptionId} onclick={revokePermission}>Revoke permission</md-text-button>
            {/snippet}
            </ButtonHelp>
          {/if}
          <ButtonHelp text={permissionHelp ?? (request.authorization === 'pr_cycle' ? 'This permission is already set.' : 'Allow this and later reviews of this PR, including after new commits or author replies, until you revoke permission.')} label="Allow all reviews for this PR" disabled={allReviewsDisabled}>
          {#snippet children(descriptionId)}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <md-text-button disabled={allReviewsDisabled} aria-describedby={descriptionId} onclick={() => authorize('pr_cycle')}>
            Allow all reviews for this PR
          </md-text-button>
          {/snippet}
          </ButtonHelp>
          <ButtonHelp text={permissionHelp ?? (request.authorization === 'one_round' ? 'This permission is already set.' : 'Allow one review of this commit and author discussion. New commits or author replies clear unused permission.')} label="Allow one review" disabled={oneReviewDisabled}>
          {#snippet children(descriptionId)}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <md-filled-button disabled={oneReviewDisabled} aria-describedby={descriptionId} onclick={() => authorize('one_round')}>
            Allow one review
          </md-filled-button>
          {/snippet}
          </ButtonHelp>
        {/snippet}
      </Panel>
    {/if}
    {#if panel === 'preparation' && preparation}
      <PreparationPanel {request} {preparation} invalid={preparationInvalid} {action}
        bind:reviewMode bind:selectedModel onRefresh={prepareReview} onValidate={validateModel} onRun={runReview} onClose={() => openPanel(null)} />
    {/if}
    {#if panel === 'draft' && draft}
      <DraftPanel {request} {draft} {action} onReject={onRejectDraft} onPublish={onPublishDraft} onClose={() => openPanel(null)} />
    {/if}
    {/if}
  {/if}
  {#if action?.message}
    <p class:action-error={action.error} class="action-note" role={action.error ? 'alert' : 'status'}>
      {action.message}
    </p>
  {/if}
  {#if !compactClosed}
    <section class="request-details" id={detailsId} hidden={!expanded} aria-label={`PR details for ${request.repository} #${request.number}`}>
    <header class="panel-heading">
      <h3>PR details</h3>
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-text-button onclick={() => expanded = false}>Close details</md-text-button>
    </header>
    <div class="detail-group detail-actions authorization-buttons">
      {#if request.latest_round || request.run || request.publication}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <md-text-button onclick={() => openPanel('progress')}>Review activity</md-text-button>
      {/if}
      {#if signedIn}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <md-text-button disabled={action?.pending || managementBlocked || request.latest_round?.status === 'awaiting_decision'} onclick={prepareReview}>Check review readiness</md-text-button>
        {#if hasDraft}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <md-text-button disabled={action?.pending} onclick={viewDraft}>Read saved draft</md-text-button>
        {/if}
      {/if}
    </div>
    <div class="detail-group">
      <h3>Recorded inputs</h3>
      <dl class="detail-list">
        <div><dt>Head</dt><dd><a href={`https://github.com/${request.repository}/commit/${request.head_sha}`} target="_blank" rel="noopener noreferrer"><code>{request.head_sha}</code></a></dd></div>
        <div><dt>Input revision</dt><dd>{request.revision}</dd></div>
        <div><dt>Permission</dt><dd>{authorizationLabels[request.authorization] ?? 'Unknown'}</dd></div>
      </dl>
      {#if request.context?.head_changed || request.context?.discussion_changed}
        <p class="observed-change">Last GitHub check: {request.context.head_changed && request.context.discussion_changed ? 'the PR commit and author discussion changed' : request.context.head_changed ? 'the PR commit changed' : 'the author discussion changed'}.</p>
      {/if}
    </div>
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
          {#if request.latest_round.task}
            <div><dt>Scope</dt><dd>{request.latest_round.task.scope.mode === 'full' ? 'Full PR review' : `Incremental since ${request.latest_round.task.scope.since.slice(0, 7)}`}</dd></div>
          {/if}
          <div><dt>Cleanup</dt><dd>{request.latest_round.cleanup_status === 'complete' ? 'Complete' : request.latest_round.cleanup_status === 'needs_attention' ? 'Cleanup pending' : request.latest_round.cleanup_status === 'in_progress' ? 'Finishing cleanup…' : 'Cleanup status unavailable'}</dd></div>
        </dl>
      {:else}
        <p class="detail-empty">No rounds yet</p>
      {/if}
    </div>
    </section>
  {/if}
  <md-divider></md-divider>
</li>
