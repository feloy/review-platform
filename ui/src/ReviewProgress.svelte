<script>
  let { request } = $props();
  const round = $derived(request.latest_round);
  const run = $derived(request.run);
  const active = $derived(run?.status === 'starting' || run?.status === 'running');
  const failed = $derived(run?.status === 'failed' && (!run.round_id || run.round_id === round?.id));
  const failureLabels = {
    preflight: 'Startup checks failed', checkout: 'Checkout failed', sandbox_create: 'Sandbox creation failed',
    dispatch: 'Agent dispatch failed', timeout: 'Agent timed out', agent: 'Agent failed',
    output: 'Draft validation failed', cleanup: 'Sandbox cleanup failed', recovery: 'Review interrupted',
  };
</script>

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
  <p class="action-note">Private draft ready. Inspect it through the CLI:
    <code>review-platform show-draft {request.repository} {request.number} --round {round.id}</code>
  </p>
  {#if round.freshness === 'stale' || round.freshness === 'unknown'}
    <p class="action-note warning">Draft freshness is {round.freshness}. Recheck PR inputs before deciding on the draft.</p>
  {/if}
{/if}
{#if round?.cleanup_status === 'needs_attention'}
  <p class="action-note">Recover the sandbox through the CLI:
    <code>review-platform recover-review {request.repository} {request.number}</code>
  </p>
{/if}
