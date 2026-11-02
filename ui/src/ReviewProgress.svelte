<script>
  import TimeAgo from './TimeAgo.svelte';
  let { request, signedIn } = $props();
  const round = $derived(request.latest_round);
  const run = $derived(request.run);
  const publication = $derived(request.publication?.round_id === round?.id ? request.publication : null);
  const active = $derived(run?.status === 'starting' || run?.status === 'running');
  const failed = $derived(run?.status === 'failed' && (!run.round_id || run.round_id === round?.id));
  const failureLabels = {
    preflight: 'Startup checks failed', checkout: 'Checkout failed', sandbox_create: 'Sandbox creation failed',
    dispatch: 'Agent dispatch failed', timeout: 'Agent timed out', agent: 'Agent failed',
    output: 'Draft validation failed', cleanup: 'Sandbox cleanup failed', recovery: 'Review interrupted',
  };
</script>

{#if publication?.status === 'active'}
  <p class="action-note" role="status">Publishing draft… The server verifies the publisher and PR inputs before posting. Closing this tab does not interrupt publication.</p>
{:else if publication?.error}
  <p class="action-note action-error" role="alert">{publication.error}</p>
{/if}
{#if round?.status === 'publication_unknown' || (round?.status === 'publishing' && publication?.status !== 'active')}
  <p class="action-note warning" role="alert">Publication needs recovery. Check GitHub before deciding again; the GUI will not retry the post.</p>
  <p class="action-note">Recover publication through the CLI:
    <code>review-platform recover-publication {request.repository} {request.number} --round {round.id}</code>
  </p>
{/if}
{#if round?.receipt}
  <p class="action-note">Published <TimeAgo value={round.receipt.published_at} /> as <a href={round.receipt.url} target="_blank" rel="noopener noreferrer">GitHub review {round.receipt.review_id}</a>
    by @{round.receipt.publisher_login}, tied to commit <code>{round.receipt.commit_id}</code>.</p>
{/if}
{#if publication?.status === 'published' && publication.freshness === 'stale'}
  <p class="action-note warning">PR inputs changed after publication. The review remains tied to its original commit.</p>
{:else if publication?.status === 'published' && publication.freshness === 'unknown'}
  <p class="action-note warning">PR freshness could not be rechecked after publication. The saved receipt confirms publication.</p>
{/if}

{#if round?.dispatched_at}<p class="action-note">Review started <TimeAgo value={round.dispatched_at} />{#if round.finished_at}; finished <TimeAgo value={round.finished_at} />{/if}.</p>
{:else if round?.created_at}<p class="action-note">Review setup began <TimeAgo value={round.created_at} />.</p>{/if}
{#if active}
  <p class="action-note" role="status">
    {#if run.status === 'starting' || run.round_id !== round?.id}Starting review… {/if}
    The review runs on the server. You can close this tab and return to see progress.
  </p>
{/if}
{#if failed}
  <p class="action-note action-error" role="alert">
    {#if run.failure === 'stale_preparation'}
      Review inputs, authorization, configuration, or model changed. Prepare and inspect the review again.
    {:else if round?.id === run.round_id && round?.status === 'awaiting_decision'}
      The private draft was saved, but the runner reported an error. Check sandbox cleanup before continuing.
    {:else}
      The GUI review job failed. Check the round status, GitHub access, gateway, and configuration, then prepare again.
    {/if}
  </p>
{/if}
{#if round?.failure}
  <p class="action-note action-error">{failureLabels[round.failure] ?? 'Review failed'}. Prepare again after resolving the failure and any pending cleanup.</p>
{/if}
{#if round?.status === 'awaiting_decision'}
  <p class="action-note">Private draft ready.
    {#if signedIn}Choose “Read private draft” to inspect it here, or use the CLI:
    {:else}Sign in to view it here, or inspect it through the CLI:{/if}
    <code>review-platform show-draft {request.repository} {request.number} --round {round.id}</code>
  </p>
  {#if round.freshness === 'stale' || round.freshness === 'unknown'}
    <p class="action-note warning">Draft freshness is {round.freshness}. Recheck PR inputs before publishing. The draft can still be rejected.</p>
  {/if}
{/if}
{#if round?.cleanup_status === 'needs_attention'}
  <p class="action-note">Recover the sandbox through the CLI:
    <code>review-platform recover-review {request.repository} {request.number}</code>
  </p>
{/if}
