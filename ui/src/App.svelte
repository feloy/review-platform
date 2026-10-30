<script>
  import { onMount } from 'svelte';

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

  const connectionLabels = {
    connecting: 'Connecting to local API',
    connected: 'Live local updates',
    reconnecting: 'Reconnecting to local API',
  };

  let requests = $state([]);
  let hasSnapshot = $state(false);
  let connection = $state('connecting');
  let error = $state('');
  let lastReceivedAt = $state(null);

  onMount(() => {
    const events = new EventSource('/api/requests/events');

    events.onopen = () => {
      connection = 'connected';
    };
    events.onerror = () => {
      connection = 'reconnecting';
    };
    events.addEventListener('requests', (event) => {
      try {
        const snapshot = JSON.parse(event.data);
        if (!Array.isArray(snapshot.requests)) throw new Error('Invalid request list');
        requests = snapshot.requests;
        hasSnapshot = true;
        lastReceivedAt = new Date();
        error = '';
      } catch {
        error = 'Could not read the latest request update.';
      }
    });
    events.addEventListener('state_error', () => {
      error = 'Could not read local review state. The API will retry automatically.';
    });

    return () => events.close();
  });
</script>

<main class="page">
  <header class="page-header">
    <div>
      <p class="eyebrow">Review Platform</p>
      <h1>Review requests</h1>
      <p class="intro">Locally recorded state updates automatically after CLI changes. This page does not fetch live GitHub data.</p>
    </div>
    <p class="connection" data-state={connection} aria-live="polite">
      <span class="connection-dot" aria-hidden="true"></span>
      {connectionLabels[connection]}
    </p>
  </header>

  {#if connection === 'reconnecting'}
    <p class="error-banner" role="alert">
      Connection lost. Reconnecting automatically.
      {#if hasSnapshot} Showing the last received list.{/if}
    </p>
  {:else if error}
    <p class="error-banner" role="alert">
      {error}
      {#if hasSnapshot} Showing the last received list.{/if}
    </p>
  {/if}

  <section class="queue" aria-labelledby="queue-heading">
    <div class="queue-heading">
      <h2 id="queue-heading">Recorded PRs</h2>
      {#if hasSnapshot}
        <p class="queue-meta" aria-live="polite">
          {requests.length} {requests.length === 1 ? 'request' : 'requests'} · Updated {lastReceivedAt.toLocaleTimeString()}
        </p>
      {/if}
    </div>

    {#if !hasSnapshot}
      <div class="state-message" role="status">
        <md-linear-progress indeterminate aria-label="Loading local requests"></md-linear-progress>
        <p>Waiting for the local request list…</p>
      </div>
    {:else if requests.length === 0}
      <p class="state-message">No review requests recorded yet. Requests added through the CLI will appear here.</p>
    {:else}
      <ul class="request-list">
        {#each requests as request (request.repository + '#' + request.number)}
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
                  {#if request.latest_round.cleanup_pending}
                    <p class="warning">Cleanup pending</p>
                  {/if}
                {:else}
                  <p class="secondary">No rounds yet</p>
                {/if}
              </div>
            </div>
            <md-divider></md-divider>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</main>
