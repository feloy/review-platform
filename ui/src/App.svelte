<script>
  import { onMount } from 'svelte';
  import RequestItem from './RequestItem.svelte';

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
  let sessionToken = $state('');
  let checkingSession = $state(true);
  let signInCode = $state('');
  let signingIn = $state(false);
  let signInError = $state('');
  let refreshState = $state({});

  const sessionKey = 'review-platform-session';

  function storeSession(token) {
    sessionToken = token;
    try {
      if (token) sessionStorage.setItem(sessionKey, token);
      else sessionStorage.removeItem(sessionKey);
    } catch {
      // The session still works until this page is closed or reloaded.
    }
  }

  async function checkSession() {
    let token = '';
    try { token = sessionStorage.getItem(sessionKey) ?? ''; } catch { /* Storage may be disabled. */ }
    if (token) {
      try {
        const response = await fetch('/api/session', { headers: { Authorization: `Bearer ${token}` } });
        if (response.ok) storeSession(token);
        else storeSession('');
      } catch {
        // Keep the session available if the server is temporarily unreachable.
        storeSession(token);
      }
    }
    checkingSession = false;
  }

  async function signIn(event) {
    event.preventDefault();
    if (signingIn) return;
    signingIn = true;
    signInError = '';
    try {
      const response = await fetch('/api/session', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ code: signInCode.trim() }),
      });
      if (!response.ok) {
        signInError = response.status === 401
          ? 'The code is invalid or already used. Restart the GUI for a new code.'
          : 'Could not sign in. Check the local API and try again.';
        return;
      }
      const result = await response.json();
      storeSession(result.token);
      signInCode = '';
    } catch {
      signInError = 'Could not connect to the local API. Try again.';
    } finally {
      signingIn = false;
    }
  }

  async function refreshFromGithub(request, requestId) {
    refreshState[requestId] = { pending: true, message: '', error: false };
    try {
      const response = await fetch('/api/requests/refresh', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${sessionToken}` },
        body: JSON.stringify({ repository: request.repository, number: request.number }),
      });
      if (response.status === 401) {
        storeSession('');
        signInError = 'Your session ended. If the code was already used, restart the GUI for a new one.';
        refreshState[requestId] = { pending: false, message: '', error: true };
        return;
      }
      if (!response.ok) {
        const result = await response.json().catch(() => ({}));
        refreshState[requestId] = { pending: false, message: result.error ?? 'Could not refresh from GitHub.', error: true };
        return;
      }
      const result = await response.json();
      const message = result.authorization_cleared
        ? 'Inputs changed. One-round authorization was cleared.'
        : result.changed ? 'GitHub inputs updated.' : 'GitHub inputs unchanged.';
      refreshState[requestId] = { pending: false, message, error: false };
    } catch {
      refreshState[requestId] = { pending: false, message: 'Could not connect to the local API. Try again.', error: true };
    }
  }

  onMount(() => {
    checkSession();
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
      <p class="intro">Locally recorded state updates automatically. Refresh a request to fetch its current GitHub inputs.</p>
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

  {#if !checkingSession && !sessionToken}
    <section class="sign-in" aria-labelledby="sign-in-heading">
      <h2 id="sign-in-heading">Sign in for actions</h2>
      <p>Enter the one-time code shown in the terminal running Review Platform.</p>
      <form onsubmit={signIn}>
        <label for="sign-in-code">One-time code</label>
        <input id="sign-in-code" type="password" autocomplete="off" required bind:value={signInCode}
          onkeydown={(event) => { if (event.key === 'Enter' && signInCode.trim()) signIn(event); }} />
        <!-- Material Web handles keyboard activation inside its button shadow DOM. -->
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <md-filled-button type="button" disabled={signingIn || !signInCode.trim()} onclick={signIn}>
          {signingIn ? 'Signing in…' : 'Sign in'}
        </md-filled-button>
      </form>
      {#if signInError}<p class="action-error" role="alert">{signInError}</p>{/if}
    </section>
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
          {@const requestId = request.repository + '#' + request.number}
          <RequestItem {request} signedIn={Boolean(sessionToken)} action={refreshState[requestId]}
            onRefresh={() => refreshFromGithub(request, requestId)} />
        {/each}
      </ul>
    {/if}
  </section>
</main>
