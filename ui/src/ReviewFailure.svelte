<script>
  import { reviewFailure } from './request-status.js';
  let { request } = $props();
  const failure = $derived(reviewFailure(request));
</script>

{#if failure}
  <div role="alert">
    {#each failure.messages as message}
      <p class="action-note action-error">{message}</p>
    {/each}
  </div>
{/if}
