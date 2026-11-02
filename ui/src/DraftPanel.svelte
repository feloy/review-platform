<script>
  import TimeAgo from './TimeAgo.svelte';
  import ButtonHelp from './ButtonHelp.svelte';
  let { request, draft, action, onReject, onPublish, onClose } = $props();
  let confirmation = $state(null);
  let confirmationInvalid = $state(false);
  let decisionSubmitted = $state(false);
  const awaitingDecision = $derived(draft.decision === 'awaiting_decision' &&
    request.latest_round?.id === draft.round_id && request.latest_round?.status === 'awaiting_decision');
  const cleanupComplete = $derived(request.latest_round?.cleanup_pending === false &&
    request.latest_round?.cleanup_status === 'complete');
  const publishing = $derived(request.publication?.status === 'active');
  const changed = $derived(draft.head_sha !== request.head_sha || draft.revision !== request.revision ||
    draft.round_id !== request.latest_round?.id || draft.decision !== request.latest_round?.status);
  const fresh = $derived(draft.freshness === 'fresh' && !changed);
  const decisionReason = $derived(!cleanupComplete ? 'Sandbox cleanup must finish before deciding on this draft.'
    : confirmationInvalid ? 'Reopen the draft before making another decision.'
    : publishing ? 'Wait for the current publication to finish before making another decision.' : '');
  const publishReason = $derived(decisionReason || (!fresh
    ? draft.freshness === 'stale' || changed
      ? 'Publication is blocked because the PR changed or closed. You can reject this draft and prepare a new review.'
      : 'Publication is blocked until GitHub freshness is verified. Reopen the draft to check again.'
    : !draft.publisher_login ? 'Configure the publishing account and reopen the draft before publishing.' : ''));
  const publishDisabled = $derived(action?.pending || !cleanupComplete || confirmationInvalid || publishing || !fresh || !draft.publisher_login);
  const rejectDisabled = $derived(action?.pending || !cleanupComplete || confirmationInvalid || publishing);
  $effect(() => {
    if (confirmation && (!awaitingDecision || !cleanupComplete || publishing ||
      (confirmation.kind === 'publish' && (!fresh || !draft.publisher_login)))) confirmationInvalid = true;
  });

  function confirmDecision(kind) {
    if (!awaitingDecision || !cleanupComplete || publishing || action?.pending || decisionSubmitted || confirmationInvalid ||
      (kind === 'publish' && (!fresh || !draft.publisher_login))) return;
    confirmation = { kind, key: { ...draft.key }, round_id: draft.round_id, body_hash: draft.publication_body_sha256, publisher_login: draft.publisher_login };
  }

  async function decide() {
    if (!confirmation || confirmationInvalid || !awaitingDecision || !cleanupComplete || publishing || action?.pending || decisionSubmitted ||
      (confirmation.kind === 'publish' && (!fresh || !draft.publisher_login))) return;
    const result = await (confirmation.kind === 'publish' ? onPublish(confirmation) : onReject(confirmation));
    if (result) {
      decisionSubmitted = true;
      confirmation = null;
      confirmationInvalid = false;
    }
  }
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
    <md-text-button disabled={action?.pending} onclick={onClose}>Close draft</md-text-button>
  </div>
  <div class="publication-destination">
    <h4>Publication destination</h4>
    <p>Review destination: <a href={`https://github.com/${draft.key.repository}/pull/${draft.key.number}`} target="_blank" rel="noopener noreferrer">{draft.key.repository} #{draft.key.number}</a>
      on <a href={`https://github.com/${draft.key.repository}/commit/${draft.head_sha}`} target="_blank" rel="noopener noreferrer">commit {draft.head_sha.slice(0, 7)}</a>.
      {#if draft.receipt}Published by <strong>@{draft.receipt.publisher_login}</strong>.
      {:else if draft.publisher_login}Publishing account: <strong>@{draft.publisher_login}</strong>. The server verifies this account before posting.
      {:else}The publishing account is unavailable. Configure publisher_login and reopen this draft before publishing.{/if}</p>
    <p>Publication posts a comment review. The agent’s recommendation does not formally approve or merge the PR.</p>
    {#if draft.inspected_at}<p>Draft inspected <TimeAgo value={draft.inspected_at} />. {draft.freshness === 'fresh' ? 'No PR changes were found at that check.' : draft.freshness === 'stale' ? 'The PR has changed or closed.' : 'GitHub freshness could not be verified.'}</p>{/if}
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
    <p class="warning" role="status">Current PR freshness could not be verified. Recheck GitHub before publishing. You can still reject this draft.</p>
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
  {#if awaitingDecision && !decisionSubmitted && !confirmation}
    <div class="authorization-buttons">
      <ButtonHelp text={publishReason} label="Publish draft" disabled={publishDisabled}>
      {#snippet children(descriptionId)}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-filled-button disabled={publishDisabled} aria-describedby={descriptionId}
        onclick={() => confirmDecision('publish')}>Publish draft</md-filled-button>
      {/snippet}
      </ButtonHelp>
      <ButtonHelp text={decisionReason} label="Reject draft" disabled={rejectDisabled}>
      {#snippet children(descriptionId)}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <md-text-button disabled={rejectDisabled} aria-describedby={descriptionId}
        onclick={() => confirmDecision('reject')}>Reject draft</md-text-button>
      {/snippet}
      </ButtonHelp>
    </div>
  {/if}
  {#if confirmationInvalid && !action?.pending}
    <p class="action-error" role="alert">The pending decision is no longer current. Reopen the draft before deciding.</p>
  {/if}
  {#if confirmation}
    {@const publish = confirmation.kind === 'publish'}
    <section class="authorization-panel" aria-label={publish ? 'Confirm draft publication' : 'Confirm draft rejection'}>
      <h4>{publish ? 'Publish this draft?' : 'Reject this draft?'}</h4>
      {#if publish}
        <p>Post to {draft.key.repository} #{draft.key.number} as <strong>@{confirmation.publisher_login}</strong>.</p>
        <p>The exact review comments shown above, including the round footer, will be posted to GitHub as a comment review on the reviewed commit.
          Private lead advice stays private. The server rechecks PR freshness and publisher identity before posting.</p>
      {:else}
        <p>Nothing will be posted. The private draft and current authorization are preserved.
          Model validation matching this round is cleared; another round needs a valid model choice.</p>
      {/if}
      <dl class="detail-list">
        <div><dt>Round to {publish ? 'publish' : 'reject'}</dt><dd><code>{confirmation.round_id}</code></dd></div>
        <div><dt>Body SHA-256 to {publish ? 'publish' : 'reject'}</dt><dd><code>{confirmation.body_hash}</code></dd></div>
      </dl>
      <div class="authorization-buttons">
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <md-filled-button disabled={action?.pending || confirmationInvalid || !awaitingDecision || !cleanupComplete || publishing || (publish && (!fresh || !draft.publisher_login))}
          onclick={decide}>{action?.pending ? (publish ? 'Starting publication…' : 'Rejecting…') : (publish ? 'Confirm publication' : 'Confirm rejection')}</md-filled-button>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <md-text-button disabled={action?.pending} onclick={() => confirmation = null}>{publish ? 'Cancel publication' : 'Cancel rejection'}</md-text-button>
      </div>
    </section>
  {/if}
  {#if draft.receipt}
    <p>Published as <a href={draft.receipt.url} target="_blank" rel="noopener noreferrer">GitHub review {draft.receipt.review_id}</a>
      by {draft.receipt.publisher_login}.</p>
  {/if}
</section>
