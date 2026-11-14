<script>
  import { onMount } from 'svelte';
  import ButtonHelp from './ButtonHelp.svelte';
  import RequestItem from './RequestItem.svelte';

  const connectionLabels = {
    connecting: 'Connecting to local updates',
    connected: 'Local updates connected',
    reconnecting: 'Reconnecting to local updates',
  };

  let requests = $state([]);
  let hasSnapshot = $state(false);
  let connection = $state('connecting');
  let error = $state('');
  let sessionToken = $state('');
  let checkingSession = $state(true);
  let signInCode = $state('');
  let signingIn = $state(false);
  let signInError = $state('');
  let requestActions = $state({});
  let bulkRefresh = $state({ pending: false, message: '', error: false });
  let refreshQueue = $state([]);
  const refreshableRequests = $derived(requests.filter((request) => request.context?.state !== 'closed'));
  const requestActionPending = $derived(Object.values(requestActions).some((action) => action.pending));
  const refreshDisabled = $derived(!sessionToken || !hasSnapshot || bulkRefresh.pending || requestActionPending || !refreshableRequests.length);
  const refreshHelp = $derived(checkingSession ? 'Checking your sign-in session.'
    : !sessionToken ? 'Sign in to refresh PRs.'
    : !hasSnapshot ? 'Waiting for the local PR list.'
    : bulkRefresh.pending ? 'A GitHub refresh is already in progress.'
    : requestActionPending ? 'Wait for the current PR action to finish.'
    : !refreshableRequests.length ? 'No open or unchecked PRs to refresh.'
    : 'Check open and unchecked PRs for changes on GitHub.');

  const sessionKey = 'luthiai-session';

  function clearCompletedRefresh() {
    if (!bulkRefresh.pending) bulkRefresh = { pending: false, message: '', error: false };
  }

  function storeSession(token) {
    sessionToken = token;
    try {
      if (token) sessionStorage.setItem(sessionKey, token);
      else sessionStorage.removeItem(sessionKey);
    } catch {
      // The session still works until this page is closed or reloaded.
    }
  }

  async function validateStoredSession() {
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
  }

  async function checkSession() {
    await validateStoredSession();
    checkingSession = false;
  }

  async function exchangeCode(code) {
    if (signingIn) return;
    clearCompletedRefresh();
    signingIn = true;
    signInError = '';
    try {
      const response = await fetch('/api/session', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ code }),
      });
      if (!response.ok) {
        signInError = response.status === 401
          ? 'The code is invalid or already used. Restart the GUI for a new sign-in link.'
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
      checkingSession = false;
    }
  }

  async function signIn(event) {
    event.preventDefault();
    return exchangeCode(signInCode.trim());
  }

  async function performRequestAction(requestId, operation, body, successMessage, failureMessage) {
    return performAction(requestId, operation, `/api/requests/${operation}`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
    }, successMessage, failureMessage);
  }

  async function performAction(requestId, operation, url, options, successMessage, failureMessage) {
    if (requestActions[requestId]?.pending) return false;
    clearCompletedRefresh();
    requestActions[requestId] = { pending: true, operation, message: '', error: false };
    try {
      const response = await fetch(url, {
        ...options,
        headers: { ...options.headers, Authorization: `Bearer ${sessionToken}` },
      });
      if (response.status === 401) {
        storeSession('');
        signInError = 'Your session ended. If the code was already used, restart the GUI for a new sign-in link.';
        requestActions[requestId] = { pending: false, message: '', error: true };
        return false;
      }
      if (!response.ok) {
        const result = await response.json().catch(() => ({}));
        requestActions[requestId] = { pending: false, message: result.error ?? failureMessage, error: true };
        return false;
      }
      const result = await response.json();
      requestActions[requestId] = { pending: false, message: successMessage(result), error: false };
      return result;
    } catch {
      requestActions[requestId] = { pending: false, message: 'Could not connect to the local API. Try again.', error: true };
      return false;
    }
  }

  function refreshFromGithub(request, requestId) {
    return performRequestAction(requestId, 'refresh',
      { repository: request.repository, number: request.number },
      (result) => {
        const change = result.head_changed && result.discussion_changed ? 'The PR commit and author discussion changed.'
          : result.head_changed ? 'The PR has a different commit.'
          : result.discussion_changed ? 'The author discussion changed.'
          : result.changed ? 'GitHub details updated.' : 'No code or author discussion changes found.';
        return change + (result.authorization_cleared ? ' Inspect the changes and allow the review again; permission for one review was cleared.' : result.changed ? ' Check readiness again before starting a review.' : '');
      },
      'Could not refresh from GitHub.');
  }

  async function refreshAllFromGithub() {
    if (refreshDisabled) return;
    const queued = refreshableRequests.map(({ repository, number }) => ({ repository, number }));
    let refreshed = 0;
    let failed = 0;
    let skipped = 0;
    bulkRefresh = { pending: true, message: `Refreshing 0 of ${queued.length} PRs…`, error: false };
    refreshQueue = queued.map((key) => key.repository + '#' + key.number);
    for (const requestId of refreshQueue) delete requestActions[requestId];
    try {
      for (const key of queued) {
        if (!sessionToken) break;
        const requestId = key.repository + '#' + key.number;
        refreshQueue = refreshQueue.filter((id) => id !== requestId);
        const current = requests.find((request) => request.repository === key.repository && request.number === key.number);
        if (!current || current.context?.state === 'closed' || requestActions[requestId]?.pending) skipped += 1;
        else if (await refreshFromGithub(current, requestId)) refreshed += 1;
        else failed += 1;
        bulkRefresh.message = `Refreshing ${refreshed + failed + skipped} of ${queued.length} PRs…`;
      }
      const count = queued.length === 1 ? 'PR' : 'PRs';
      bulkRefresh.message = `Refreshed ${refreshed} of ${queued.length} ${count}.`
        + (failed ? ` ${failed} could not be refreshed; see the PR rows for details.` : '')
        + (skipped ? ` Skipped ${skipped} busy, removed, or closed ${skipped === 1 ? 'PR' : 'PRs'}.` : '')
        + (!sessionToken ? ' Refresh stopped because your session ended. Sign in to continue.' : '');
      bulkRefresh.error = failed > 0 || !sessionToken;
    } finally {
      refreshQueue = [];
      bulkRefresh.pending = false;
    }
  }

  function authorize(request, requestId, scope) {
    return performRequestAction(requestId, 'authorize', {
      key: { repository: request.repository, number: request.number },
      head_sha: request.head_sha,
      revision: request.revision,
      scope,
    }, () => scope === 'one_round' ? 'Permission granted for one review.' : 'Permission granted for all reviews of this PR.',
    'Could not grant review permission.');
  }

  function revoke(request, requestId) {
    return performRequestAction(requestId, 'revoke',
      { repository: request.repository, number: request.number },
      () => 'Permission revoked.', 'Could not revoke permission.');
  }

  function unmanage(request, requestId) {
    return performRequestAction(requestId, 'unmanage',
      { repository: request.repository, number: request.number },
      () => 'Management stopped. Local drafts, logs, and review history were deleted.',
      'Could not stop managing this PR.');
  }

  function prepareReview(request, requestId) {
    return performRequestAction(requestId, 'prepare',
      { repository: request.repository, number: request.number },
      (result) => result.context?.head_changed || result.context?.discussion_changed ? 'GitHub changed. Inspect the current commit and discussion before continuing.' : 'GitHub checks and review preparation updated.', 'Could not prepare the review.');
  }

  function validateModel(request, requestId, preparation, model) {
    return performRequestAction(requestId, 'validate-model', {
      key: { repository: request.repository, number: request.number },
      model,
      head_sha: preparation.head_sha,
      revision: preparation.revision,
      rules: {
        authorization: preparation.authorization,
        configuration_sha256: preparation.configuration_sha256,
      },
    }, (result) => result.readiness === 'ready' && result.validated_model === model
      ? 'Review model confirmed.' : 'Readiness changed. Check the review requirements before confirming the model.',
    'Could not confirm the review model.');
  }

  function runReview(request, requestId, preparation, model, reviewMode) {
    return performRequestAction(requestId, 'run', {
      key: { repository: request.repository, number: request.number },
      model,
      review_mode: reviewMode,
      head_sha: preparation.head_sha,
      revision: preparation.revision,
      rules: {
        authorization: preparation.authorization,
        configuration_sha256: preparation.configuration_sha256,
      },
    }, () => 'Review accepted. Starting checks on the server.', 'Could not start the review.');
  }

  function viewDraft(request, requestId, roundId) {
    const query = new URLSearchParams({ repository: request.repository, number: String(request.number) });
    return performAction(requestId, 'view-draft', `/api/drafts/${encodeURIComponent(roundId)}?${query}`, {
      method: 'GET', cache: 'no-store',
    }, () => '', 'Could not inspect the private draft.');
  }

  function rejectDraft(requestId, inspected) {
    return performAction(requestId, 'reject-draft', `/api/drafts/${encodeURIComponent(inspected.round_id)}/reject`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ key: inspected.key, body_hash: inspected.body_hash }),
    }, (result) => result.changed
      ? 'Draft rejected locally. The private draft and current authorization are preserved.'
      : 'This draft was already rejected. Current authorization is unchanged.', 'Could not reject the private draft.');
  }

  function publishDraft(requestId, inspected) {
    return performAction(requestId, 'publish-draft', `/api/drafts/${encodeURIComponent(inspected.round_id)}/publish`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ key: inspected.key, body_hash: inspected.body_hash, publisher_login: inspected.publisher_login }),
    }, () => 'Publication accepted. Follow its status below; closing this tab does not interrupt it.', 'Could not start draft publication.');
  }

  async function initializeSession(bootstrapCode) {
    await validateStoredSession();
    if (!sessionToken) await exchangeCode(bootstrapCode);
    else checkingSession = false;
  }

  onMount(() => {
    const url = new URL(window.location.href);
    const bootstrapCode = url.searchParams.get('code')?.trim() ?? '';
    if (bootstrapCode) {
      url.searchParams.delete('code');
      window.history.replaceState(window.history.state, '', url);
      initializeSession(bootstrapCode);
    } else {
      checkSession();
    }
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
    <h1>LuthiAI</h1>
    <div class="header-actions">
      <ButtonHelp text={refreshHelp} label="Refresh open PRs" disabled={refreshDisabled}>
        {#snippet children(descriptionId)}
          <!-- Material Web handles keyboard activation inside its button shadow DOM. -->
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <md-filled-button type="button" disabled={refreshDisabled} aria-describedby={descriptionId}
            onclick={refreshAllFromGithub}>
            {bulkRefresh.pending ? 'Refreshing PRs…' : 'Refresh open PRs'}
          </md-filled-button>
        {/snippet}
      </ButtonHelp>
      <ButtonHelp text={`${connectionLabels[connection]}. Refresh open PRs to check GitHub for changes.`} label="Local updates">
        {#snippet children(descriptionId)}
          <!-- Keyboard focus exposes the connection explanation. -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <span class="connection" data-state={connection} role="status" tabindex="0" aria-describedby={descriptionId}>
            <span class="connection-dot" aria-hidden="true"></span>
            <span class="visually-hidden">{connectionLabels[connection]}</span>
          </span>
        {/snippet}
      </ButtonHelp>
    </div>
  </header>

  {#if bulkRefresh.message}
    <p class="bulk-refresh-status" class:action-error={bulkRefresh.error} role={bulkRefresh.error ? 'alert' : 'status'}>{bulkRefresh.message}</p>
  {/if}

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
      <p>Open the sign-in link shown in the terminal, or paste the value of its <code>code</code> parameter here.</p>
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
      <h2 id="queue-heading" aria-live="polite">
        Pull requests
        {#if hasSnapshot}<md-gb-badge class="pr-count">{requests.length}</md-gb-badge>{/if}
      </h2>
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
          <RequestItem {request} {checkingSession} signedIn={Boolean(sessionToken)} action={requestActions[requestId]}
            refreshQueued={refreshQueue.includes(requestId) && request.context?.state !== 'closed'}
            onRefresh={() => refreshFromGithub(request, requestId)}
            onAuthorize={(inspected, scope) => authorize(inspected, requestId, scope)}
            onRevoke={() => revoke(request, requestId)}
            onUnmanage={() => unmanage(request, requestId)}
            onPrepare={() => prepareReview(request, requestId)}
            onValidate={(preparation, model) => validateModel(request, requestId, preparation, model)}
            onRun={(preparation, model, reviewMode) => runReview(request, requestId, preparation, model, reviewMode)}
            onViewDraft={(roundId) => viewDraft(request, requestId, roundId)}
            onRejectDraft={(inspected) => rejectDraft(requestId, inspected)}
            onPublishDraft={(inspected) => publishDraft(requestId, inspected)} />
        {/each}
      </ul>
    {/if}
  </section>
</main>
