<script>
  import TimeAgo from './TimeAgo.svelte';
  import ReviewFailure from './ReviewFailure.svelte';
  let { request } = $props();
  const round = $derived(request.latest_round);
  const run = $derived(request.run);
  const publication = $derived(request.publication?.round_id === round?.id ? request.publication : null);
  const active = $derived(run?.status === 'starting' || run?.status === 'running');
</script>

<ReviewFailure {request} />
{#if publication?.status === 'active'}
  <p class="action-note" role="status">Verifying publisher and PR inputs, then posting to GitHub…</p>
{/if}
{#if round?.status === 'publication_unknown' || (round?.status === 'publishing' && publication?.status !== 'active')}
  <p class="action-note warning" role="alert">Publication needs recovery. Check GitHub before deciding again; the GUI will not retry the post.</p>
  <p class="action-note">Recover publication through the CLI:
    <code>luthiai recover-publication {request.repository} {request.number} --round {round.id}</code>
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
    {run.status === 'starting' ? 'Checking inputs and preparing the review environment.' : 'Reviewer running in the sandbox.'}
  </p>
{/if}
{#if round?.status === 'awaiting_decision'}
  {#if round.freshness === 'stale' || round.freshness === 'unknown'}
    <p class="action-note warning">Draft freshness is {round.freshness}. Recheck PR inputs before publishing. The draft can still be rejected.</p>
  {/if}
{/if}
{#if round?.cleanup_status === 'needs_attention'}
  <p class="action-note">Recover the sandbox through the CLI:
    <code>luthiai recover-review {request.repository} {request.number}</code>
  </p>
{:else if round?.cleanup_status === 'unknown'}
  <p class="action-note">Cleanup status unavailable</p>
{:else if round?.cleanup_pending && !['starting', 'running'].includes(round.status)}
  <p class="action-note">Finishing cleanup…</p>
{/if}
