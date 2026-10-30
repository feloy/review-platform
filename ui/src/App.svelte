<script>
  import { onMount } from 'svelte';

  let message = $state('Waiting for the API…');
  let error = $state('');
  let loading = $state(false);

  async function callApi() {
    loading = true;
    error = '';
    try {
      const response = await fetch('/api/hello', { cache: 'no-store' });
      if (!response.ok) throw new Error(`API returned ${response.status}`);
      const body = await response.json();
      message = body.message;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : 'The API request failed.';
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void callApi();
  });
</script>

<main class="page">
  <section class="welcome" aria-labelledby="page-title">
    <p class="eyebrow">Review Platform</p>
    <h1 id="page-title">Hello</h1>
    <p class="intro">The local graphical interface is connected to the Review Platform command.</p>

    <div class="api-result" aria-live="polite">
      <p class="label">API response</p>
      {#if error}
        <p class="error">{error}</p>
      {:else}
        <p class="message">{message}</p>
      {/if}
    </div>

    <!-- Material Web handles keyboard activation inside its button shadow DOM. -->
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-filled-button onclick={callApi} disabled={loading}>
      {loading ? 'Calling API…' : 'Call API again'}
    </md-filled-button>
  </section>
</main>
