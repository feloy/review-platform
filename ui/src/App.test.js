import '@material/web/button/text-button.js';
import '@material/web/button/filled-button.js';
import '@material/web/divider/divider.js';
import '@material/web/progress/linear-progress.js';
import { act, render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import App from './App.svelte';

class FakeEventSource extends EventTarget {
  static instances = [];

  constructor(url) {
    super();
    this.url = url;
    this.onopen = null;
    this.onerror = null;
    this.close = vi.fn();
    FakeEventSource.instances.push(this);
  }

  open() {
    this.onopen?.(new Event('open'));
  }

  disconnect() {
    this.onerror?.(new Event('error'));
  }

  send(type, data) {
    this.dispatchEvent(new MessageEvent(type, { data }));
  }

  snapshot(requests) {
    this.send('requests', JSON.stringify({ requests }));
  }
}

function request(overrides = {}) {
  return {
    repository: 'owner/repo',
    number: 15,
    head_sha: 'a'.repeat(40),
    revision: 1,
    authorization: 'awaiting_lead',
    discussion: null,
    validated_model: null,
    latest_round: null,
    ...overrides,
  };
}

function round(overrides = {}) {
  return {
    id: 'r-example',
    status: 'running',
    cleanup_pending: true,
    cleanup_status: 'in_progress',
    head_sha: 'a'.repeat(40),
    revision: 1,
    model: 'model-alpha',
    agent: 'claude',
    ...overrides,
  };
}

function preparation(overrides = {}) {
  return {
    head_sha: 'a'.repeat(40), revision: 2, authorization: 'one_round',
    configuration_sha256: 'c'.repeat(64), checks: 'green',
    readiness: 'model_validation_required', readiness_message: 'lead model validation is required',
    proposed_model: 'model-alpha', proposal_matches_coauthor: false, validated_model: null,
    resolved_model: 'model-alpha', models: [
      { model: 'model-alpha', agent: 'claude' }, { model: 'model-beta', agent: 'claude-alt' },
    ], ...overrides,
  };
}

function preparedRequest(overrides = {}) {
  return request({ revision: 2, authorization: 'one_round',
    discussion: { issue_comments: 0, review_comments: 0, review_bodies: 0 }, ...overrides });
}

function draft(overrides = {}) {
  return {
    key: { repository: 'owner/repo', number: 15 }, round_id: 'r-example', head_sha: 'a'.repeat(40), revision: 1,
    decision: 'awaiting_decision', model: 'model-alpha', agent: 'claude', verdict: 'ACCEPT_WITH_NITS', freshness: 'fresh',
    publication_body: "# Finding\n<script>alert('draft')</script>\n\n_Agent review via Review Platform (round r-example)._",
    publication_body_sha256: 'd'.repeat(64), lead_advice: '<img src=x onerror=alert(1)> Private advice', ...overrides,
  };
}

async function showSnapshot(source, requests) {
  await act(() => source.snapshot(requests));
}

async function clickMaterialButton(user, container, label) {
  const control = [...container.querySelectorAll('md-text-button, md-filled-button')]
    .find((button) => button.textContent.trim() === label);
  expect(control).toBeTruthy();
  await waitFor(() => expect(control.shadowRoot.querySelector('button')).toBeTruthy());
  await user.click(control.shadowRoot.querySelector('button'));
  return control;
}

beforeEach(() => {
  FakeEventSource.instances = [];
  sessionStorage.clear();
  vi.stubGlobal('EventSource', FakeEventSource);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('review requests page', () => {
  it('connects to the request stream, shows an empty list, and closes it on unmount', async () => {
    const { unmount } = render(App);
    const source = FakeEventSource.instances[0];

    expect(source.url).toBe('/api/requests/events');
    expect(screen.getByText('Connecting to local API')).toBeTruthy();
    expect(screen.getByText('Waiting for the local request list…')).toBeTruthy();

    await act(() => source.open());
    await showSnapshot(source, []);
    expect(screen.getByText('Live local updates')).toBeTruthy();
    expect(screen.getByText(/No review requests recorded yet/)).toBeTruthy();
    expect(screen.getByText(/0 requests/)).toBeTruthy();
    expect(screen.queryByText('Refresh from GitHub')).toBeNull();
    expect(screen.queryByText('Manage authorization')).toBeNull();
    expect(screen.queryByText('Revoke authorization')).toBeNull();

    unmount();
    expect(source.close).toHaveBeenCalledOnce();
  });

  it('signs in and refreshes a request while keeping the live snapshot authoritative', async () => {
    const user = userEvent.setup();
    let finishRefresh;
    const fetchMock = vi.fn()
      .mockResolvedValueOnce({ ok: true, json: async () => ({ token: 'session-token' }) })
      .mockImplementationOnce(() => new Promise((resolve) => { finishRefresh = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request()]);

    expect(screen.queryByText('Refresh from GitHub')).toBeNull();
    await user.type(await screen.findByLabelText('One-time code'), 'test-code');
    const signIn = container.querySelector('md-filled-button');
    await waitFor(() => expect(signIn.shadowRoot.querySelector('button')).toBeTruthy());
    await user.click(signIn.shadowRoot.querySelector('button'));
    await waitFor(() => expect(screen.getByText('Refresh from GitHub')).toBeTruthy());
    expect(fetchMock.mock.calls[0][0]).toBe('/api/session');
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({ code: 'test-code' });
    expect(sessionStorage.getItem('review-platform-session')).toBe('session-token');
    expect(screen.getByText(/Changed inputs can clear one-round authorization/)).toBeTruthy();

    const refresh = [...container.querySelectorAll('md-text-button')].find((button) => button.textContent.includes('Refresh from GitHub'));
    await user.click(refresh.shadowRoot.querySelector('button'));
    expect(screen.getByText('Refreshing…')).toBeTruthy();
    expect(refresh.disabled).toBe(true);
    expect(fetchMock.mock.calls[1][0]).toBe('/api/requests/refresh');
    expect(fetchMock.mock.calls[1][1].headers.Authorization).toBe('Bearer session-token');
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({ repository: 'owner/repo', number: 15 });
    await act(() => finishRefresh({ ok: true, json: async () => ({ changed: true, authorization_cleared: true }) }));
    expect(await screen.findByText('Inputs changed. One-round authorization was cleared.')).toBeTruthy();
    expect(screen.getByText('Revision 1')).toBeTruthy();
    await showSnapshot(FakeEventSource.instances[0], [request({ revision: 2 })]);
    expect(screen.getByText('Revision 2')).toBeTruthy();
  });

  it('reports sign-in and refresh failures and clears expired sessions', async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn()
      .mockResolvedValueOnce({ ok: false, status: 401 })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ token: 'session-token' }) })
      .mockResolvedValueOnce({ ok: false, status: 502, json: async () => ({ error: 'GitHub rate limit reached. Try again later.' }) })
      .mockResolvedValueOnce({ ok: false, status: 401 });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request()]);
    await user.type(await screen.findByLabelText('One-time code'), 'test-code');
    const signIn = container.querySelector('md-filled-button');
    await user.click(signIn.shadowRoot.querySelector('button'));
    expect(await screen.findByText(/code is invalid or already used/)).toBeTruthy();
    await user.click(signIn.shadowRoot.querySelector('button'));
    await screen.findByText('Refresh from GitHub');
    const refresh = [...container.querySelectorAll('md-text-button')].find((button) => button.textContent.includes('Refresh from GitHub'));
    await user.click(refresh.shadowRoot.querySelector('button'));
    expect(await screen.findByText('GitHub rate limit reached. Try again later.')).toBeTruthy();
    await user.click(refresh.shadowRoot.querySelector('button'));
    expect(await screen.findByText(/Your session ended/)).toBeTruthy();
    expect(sessionStorage.getItem('review-platform-session')).toBeNull();
  });

  it('restores a valid tab session and discards one from a previous server launch', async () => {
    sessionStorage.setItem('review-platform-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true });
    vi.stubGlobal('fetch', fetchMock);
    const { unmount } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request()]);
    expect(await screen.findByText('Refresh from GitHub')).toBeTruthy();
    expect(fetchMock.mock.calls[0][1].headers.Authorization).toBe('Bearer saved-token');
    unmount();
    fetchMock.mockResolvedValueOnce({ ok: false, status: 401 });
    render(App);
    expect(await screen.findByLabelText('One-time code')).toBeTruthy();
    expect(sessionStorage.getItem('review-platform-session')).toBeNull();
  });

  it.each([
    ['one_round', 'Authorize one round', 'Authorized for one round.', 'One round'],
    ['pr_cycle', 'Authorize PR cycle', 'Authorized for the PR cycle.', 'PR cycle'],
  ])('authorizes %s for the displayed inputs and waits for the saved snapshot', async (scope, label, message, badge) => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    let finishAuthorization;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockImplementationOnce(() => new Promise((resolve) => { finishAuthorization = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    const observed = request({ revision: 2, discussion: { issue_comments: 0, review_comments: 0, review_bodies: 0 } });
    await showSnapshot(source, [observed]);
    await screen.findByText('Manage authorization');
    await clickMaterialButton(user, container, 'Manage authorization');
    const panel = screen.getByRole('region', { name: 'Authorize owner/repo #15' });
    expect(within(panel).getByText('a'.repeat(40))).toBeTruthy();
    expect(within(panel).getByText('Input revision').nextElementSibling.textContent).toBe('2');
    expect(within(panel).getByRole('link', { name: 'PR on GitHub' }).href).toBe('https://github.com/owner/repo/pull/15');

    const button = await clickMaterialButton(user, container, label);
    expect(button.disabled).toBe(true);
    expect(screen.getByText('Saving authorization…')).toBeTruthy();
    expect(fetchMock.mock.calls[1][0]).toBe('/api/requests/authorize');
    expect(fetchMock.mock.calls[1][1].headers.Authorization).toBe('Bearer saved-token');
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({
      key: { repository: 'owner/repo', number: 15 }, head_sha: 'a'.repeat(40), revision: 2, scope,
    });
    await clickMaterialButton(user, container, 'Refresh from GitHub');
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await act(() => finishAuthorization({ ok: true, json: async () => ({ changed: true }) }));
    expect(await screen.findByText(message)).toBeTruthy();
    expect(screen.queryByRole('region', { name: 'Authorize owner/repo #15' })).toBeNull();
    expect(screen.getByText('Awaiting lead')).toBeTruthy();
    await showSnapshot(source, [{ ...observed, authorization: scope }]);
    expect(screen.getByText(badge)).toBeTruthy();
  });

  it.each([
    { head_sha: 'b'.repeat(40), revision: 3 },
    { revision: 3 },
  ])('keeps the inspected inputs fixed when a snapshot changes to %o', async (change) => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ changed: true }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    const observed = request({ revision: 2, discussion: { issue_comments: 0, review_comments: 0, review_bodies: 0 } });
    await showSnapshot(source, [observed]);
    await screen.findByText('Manage authorization');
    await clickMaterialButton(user, container, 'Manage authorization');
    const updated = { ...observed, ...change };
    await showSnapshot(source, [updated]);
    const panel = screen.getByRole('region', { name: 'Authorize owner/repo #15' });
    expect(within(panel).getByText('a'.repeat(40))).toBeTruthy();
    expect(within(panel).getByText('Input revision').nextElementSibling.textContent).toBe('2');
    expect(screen.getByRole('alert').textContent).toContain('Inputs changed');
    const disabled = await clickMaterialButton(user, container, 'Authorize PR cycle');
    expect(disabled.disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledOnce();
    await clickMaterialButton(user, container, 'Close');
    await clickMaterialButton(user, container, 'Manage authorization');
    await clickMaterialButton(user, container, 'Authorize PR cycle');
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toMatchObject({
      head_sha: updated.head_sha, revision: updated.revision,
    });
  });

  it('requires a discussion baseline for authorization but permits revocation without it', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ changed: true }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ authorization: 'pr_cycle' })]);
    await screen.findByText('Manage authorization');
    const manage = await clickMaterialButton(user, container, 'Manage authorization');
    expect(manage.disabled).toBe(true);
    expect(screen.getByText('Refresh from GitHub before authorizing this request.')).toBeTruthy();
    expect(screen.queryByRole('region', { name: 'Authorize owner/repo #15' })).toBeNull();
    await clickMaterialButton(user, container, 'Revoke authorization');
    expect(fetchMock.mock.calls[1][0]).toBe('/api/requests/revoke');
    expect(fetchMock.mock.calls[1][1].headers.Authorization).toBe('Bearer saved-token');
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({ repository: 'owner/repo', number: 15 });
    expect(await screen.findByText('Authorization revoked.')).toBeTruthy();
    expect(screen.getByText('PR cycle')).toBeTruthy();
  });

  it('reports authorization conflicts and connection errors and clears an expired session', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: false, status: 409, json: async () => ({ error: 'Review inputs changed. Inspect the current head and revision before authorizing again.' }) })
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce({ ok: false, status: 401 });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ discussion: { issue_comments: 1, review_comments: 0, review_bodies: 0 } })]);
    await screen.findByText('Manage authorization');
    await clickMaterialButton(user, container, 'Manage authorization');
    await clickMaterialButton(user, container, 'Authorize one round');
    expect(await screen.findByText(/Review inputs changed/)).toBeTruthy();
    expect(screen.getByText('Awaiting lead')).toBeTruthy();
    await clickMaterialButton(user, container, 'Authorize one round');
    expect(await screen.findByText('Could not connect to the local API. Try again.')).toBeTruthy();
    await clickMaterialButton(user, container, 'Authorize one round');
    expect(await screen.findByText(/Your session ended/)).toBeTruthy();
    expect(sessionStorage.getItem('review-platform-session')).toBeNull();
    expect(screen.queryByText('Manage authorization')).toBeNull();
    expect(screen.queryByRole('region', { name: 'Authorize owner/repo #15' })).toBeNull();
  });

  it('prepares a review and validates a different model for the inspected inputs and rules', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    let finishPreparation, finishValidation;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockImplementationOnce(() => new Promise((resolve) => { finishPreparation = resolve; }))
      .mockImplementationOnce(() => new Promise((resolve) => { finishValidation = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    await showSnapshot(source, [preparedRequest()]);
    await screen.findByText('Prepare review');
    const prepare = await clickMaterialButton(user, container, 'Prepare review');
    expect(prepare.disabled).toBe(true);
    expect(screen.getByText('Preparing…')).toBeTruthy();
    expect(fetchMock.mock.calls[1][0]).toBe('/api/requests/prepare');
    expect(fetchMock.mock.calls[1][1].headers.Authorization).toBe('Bearer saved-token');
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({ repository: 'owner/repo', number: 15 });
    await act(() => finishPreparation({ ok: true, json: async () => preparation() }));
    const panel = await screen.findByRole('region', { name: 'Review preparation for owner/repo #15' });
    expect(within(panel).getByText('a'.repeat(40))).toBeTruthy();
    expect(within(panel).getByText('Green')).toBeTruthy();
    expect(within(panel).getByText('lead model validation is required')).toBeTruthy();
    const model = within(panel).getByLabelText('Review model for owner/repo #15');
    expect(model.value).toBe('model-alpha');
    await user.selectOptions(model, 'model-beta');
    expect(within(panel).getByText('claude-alt')).toBeTruthy();
    const validate = await clickMaterialButton(user, container, 'Validate model');
    expect(validate.disabled).toBe(true);
    expect(model.disabled).toBe(true);
    expect(fetchMock.mock.calls[2][0]).toBe('/api/requests/validate-model');
    expect(JSON.parse(fetchMock.mock.calls[2][1].body)).toEqual({
      key: { repository: 'owner/repo', number: 15 }, model: 'model-beta',
      head_sha: 'a'.repeat(40), revision: 2,
      rules: { authorization: 'one_round', configuration_sha256: 'c'.repeat(64) },
    });
    await act(() => finishValidation({ ok: true, json: async () => preparation({
      readiness: 'ready', readiness_message: 'ready to run', validated_model: 'model-beta', resolved_model: 'model-beta',
    }) }));
    expect(await screen.findByText('Review model validated.')).toBeTruthy();
    expect(within(panel).getByText('ready to run')).toBeTruthy();
    expect(within(panel).getByText(/This model is validated for the prepared head and revision/)).toBeTruthy();
    const validated = await clickMaterialButton(user, container, 'Model validated');
    expect(validated.disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(screen.getByText('No validated model for current inputs')).toBeTruthy();
    await showSnapshot(source, [preparedRequest({ validated_model: { model: 'model-beta', agent: 'claude-alt' } })]);
    expect(screen.queryByText('No validated model for current inputs')).toBeNull();
  });

  it.each([
    ['checks_pending', 'pending', 'checks for the current head are pending'],
    ['checks_failing', 'failing', 'checks for the current head are failing'],
    ['checks_unknown', 'unknown', 'checks for the current head have an unknown result'],
    ['no_checks', 'no_results', 'no checks were reported for the current head'],
    ['awaiting_authorization', 'green', 'lead authorization is required'],
    ['pull_closed', 'green', 'the PR is closed'],
  ])('shows %s as a blocker and disables model validation', async (readiness, checks, readiness_message) => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation({ readiness, checks, readiness_message }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest()]);
    await screen.findByText('Prepare review');
    await clickMaterialButton(user, container, 'Prepare review');
    expect(await screen.findByText(readiness_message)).toBeTruthy();
    expect(screen.getByLabelText('Review model for owner/repo #15').disabled).toBe(true);
    const button = await clickMaterialButton(user, container, 'Validate model');
    expect(button.disabled).toBe(true);
    expect((await clickMaterialButton(user, container, 'Run review')).disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it.each([
    { head_sha: 'b'.repeat(40), revision: 3 }, { revision: 3 }, { authorization: 'pr_cycle' },
  ])('keeps preparation fixed and prevents validation after %o', async (change) => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation() })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation(change) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    await showSnapshot(source, [preparedRequest()]);
    await screen.findByText('Prepare review');
    await clickMaterialButton(user, container, 'Prepare review');
    await screen.findByText('Review preparation updated.');
    await showSnapshot(source, [preparedRequest(change)]);
    const panel = screen.getByRole('region', { name: 'Review preparation for owner/repo #15' });
    expect(within(panel).getByText('a'.repeat(40))).toBeTruthy();
    expect(screen.getByRole('alert').textContent).toContain('This preparation is outdated');
    const validate = await clickMaterialButton(user, container, 'Validate model');
    expect(validate.disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await clickMaterialButton(user, container, 'Prepare review');
    await screen.findByText('Review preparation updated.');
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByLabelText('Review model for owner/repo #15').disabled).toBe(false);
  });

  it('requires new preparation after a model validation conflict', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation() })
      .mockResolvedValueOnce({ ok: false, status: 409, json: async () => ({ error: 'Configuration changed. Prepare again.' }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest()]);
    await screen.findByText('Prepare review');
    await clickMaterialButton(user, container, 'Prepare review');
    await screen.findByText('Review preparation updated.');
    await clickMaterialButton(user, container, 'Validate model');
    expect(await screen.findByText('Configuration changed. Prepare again.')).toBeTruthy();
    expect(screen.getByText(/This preparation is outdated/)).toBeTruthy();
    const validate = await clickMaterialButton(user, container, 'Validate model');
    expect(validate.disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it('uses the validated model as the default and explains a co-author hint match', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation({
        readiness: 'ready', readiness_message: 'ready to run', resolved_model: 'model-beta',
        validated_model: 'model-beta', proposal_matches_coauthor: true,
      }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest()]);
    await screen.findByText('Prepare review');
    await clickMaterialButton(user, container, 'Prepare review');
    expect(await screen.findByText(/The proposed model matches a PR co-author hint/)).toBeTruthy();
    const model = screen.getByLabelText('Review model for owner/repo #15');
    expect(model.value).toBe('model-beta');
    const validated = await clickMaterialButton(user, container, 'Model validated');
    expect(validated.disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await user.selectOptions(model, 'model-alpha');
    expect(screen.getByText('Validate model').disabled).toBe(false);
    expect(screen.queryByText(/This model is validated for the prepared head and revision/)).toBeNull();
    await user.selectOptions(model, 'model-beta');
    expect(screen.getByText('Model validated').disabled).toBe(true);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest({ revision: 3 })]);
    expect(screen.getByText('Validate model').disabled).toBe(true);
    expect(screen.getByRole('alert').textContent).toContain('This preparation is outdated');
  });

  it('reports preparation errors and clears an expired session', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: false, status: 422, json: async () => ({ error: 'Check review configuration.' }) })
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce({ ok: false, status: 401 }));
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest()]);
    await screen.findByText('Prepare review');
    await clickMaterialButton(user, container, 'Prepare review');
    expect(await screen.findByText('Check review configuration.')).toBeTruthy();
    expect(screen.queryByRole('region', { name: 'Review preparation for owner/repo #15' })).toBeNull();
    await clickMaterialButton(user, container, 'Prepare review');
    expect(await screen.findByText('Could not connect to the local API. Try again.')).toBeTruthy();
    await clickMaterialButton(user, container, 'Prepare review');
    expect(await screen.findByText(/Your session ended/)).toBeTruthy();
    expect(screen.queryByText('Prepare review')).toBeNull();
    expect(sessionStorage.getItem('review-platform-session')).toBeNull();
  });

  it('runs the validated selection for frozen inputs and prevents duplicate clicks', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    let acceptRun;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation({
        readiness: 'ready', readiness_message: 'ready to run', validated_model: 'model-beta', resolved_model: 'model-beta',
      }) })
      .mockImplementationOnce(() => new Promise((resolve) => { acceptRun = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest({ validated_model: { model: 'model-beta', agent: 'claude-alt' } })]);
    await screen.findByText('Prepare review');
    await clickMaterialButton(user, container, 'Prepare review');
    await screen.findByText('ready to run');
    const model = screen.getByLabelText('Review model for owner/repo #15');
    await user.selectOptions(model, 'model-alpha');
    expect((await clickMaterialButton(user, container, 'Run review')).disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await user.selectOptions(model, 'model-beta');
    const run = await clickMaterialButton(user, container, 'Run review');
    expect(run.disabled).toBe(true);
    expect(model.disabled).toBe(true);
    expect(fetchMock.mock.calls[2][0]).toBe('/api/requests/run');
    expect(fetchMock.mock.calls[2][1].headers.Authorization).toBe('Bearer saved-token');
    expect(JSON.parse(fetchMock.mock.calls[2][1].body)).toEqual({
      key: { repository: 'owner/repo', number: 15 }, model: 'model-beta', head_sha: 'a'.repeat(40), revision: 2,
      rules: { authorization: 'one_round', configuration_sha256: 'c'.repeat(64) },
    });
    await clickMaterialButton(user, container, 'Starting review…');
    expect(fetchMock).toHaveBeenCalledTimes(3);
    await act(() => acceptRun({ ok: true, status: 202, json: async () => ({ accepted: true }) }));
    expect(await screen.findByText('Review accepted. Starting checks on the server.')).toBeTruthy();
    expect(screen.queryByRole('region', { name: 'Review preparation for owner/repo #15' })).toBeNull();
    expect(screen.getByText('No rounds yet', { selector: '.secondary' })).toBeTruthy();
  });

  it.each([
    { head_sha: 'b'.repeat(40), revision: 3 }, { revision: 3 }, { authorization: 'pr_cycle' },
    { validated_model: { model: 'model-beta', agent: 'claude-alt' } },
    { validated_model: null },
    { run: { status: 'starting' } }, { run: { status: 'running', round_id: 'r-example' } },
    { latest_round: round() },
    { latest_round: round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' }) },
    { latest_round: round({ status: 'failed', cleanup_status: 'needs_attention' }) },
  ])('disables running after live state changes to %o', async (change) => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation({
        readiness: 'ready', readiness_message: 'ready to run', validated_model: 'model-alpha',
      }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    const original = preparedRequest({ validated_model: { model: 'model-alpha', agent: 'claude' } });
    await showSnapshot(source, [original]);
    await screen.findByText('Prepare review');
    await clickMaterialButton(user, container, 'Prepare review');
    await screen.findByText('ready to run');
    expect(screen.getByText('Run review').disabled).toBe(false);
    await showSnapshot(source, [{ ...original, ...change }]);
    expect((await clickMaterialButton(user, container, 'Run review')).disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('requires a new preparation after a rejected run request', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation({
        readiness: 'ready', readiness_message: 'ready to run', validated_model: 'model-alpha',
      }) })
      .mockResolvedValueOnce({ ok: false, status: 409, json: async () => ({ error: 'A review is already starting or running for this PR.' }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest({ validated_model: { model: 'model-alpha', agent: 'claude' } })]);
    await screen.findByText('Prepare review');
    await clickMaterialButton(user, container, 'Prepare review');
    await screen.findByText('ready to run');
    await clickMaterialButton(user, container, 'Run review');
    expect(await screen.findByText('A review is already starting or running for this PR.')).toBeTruthy();
    expect(screen.getByText(/This preparation is outdated/)).toBeTruthy();
    expect((await clickMaterialButton(user, container, 'Run review')).disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it('shows server progress, private draft readiness, failures, and cleanup from live updates', async () => {
    const { unmount } = render(App);
    const source = FakeEventSource.instances[0];
    await showSnapshot(source, [request({ run: { status: 'starting' } })]);
    expect(screen.getByText(/Starting review…/)).toBeTruthy();
    expect(screen.getByText(/You can close this tab and return to see progress/)).toBeTruthy();
    await showSnapshot(source, [request({ latest_round: round(), run: { status: 'running', round_id: 'r-example' } })]);
    expect(screen.getAllByText('Running').length).toBeGreaterThan(0);
    expect(screen.queryByText(/Starting review…/)).toBeNull();
    await showSnapshot(source, [request({ latest_round: round({ status: 'awaiting_decision', freshness: 'stale' }),
      run: { status: 'running', round_id: 'r-example' } })]);
    expect(screen.getAllByText('Draft ready').length).toBeGreaterThan(0);
    expect(screen.getByText('Finishing cleanup…')).toBeTruthy();
    expect(screen.getByText(/review-platform show-draft owner\/repo 15 --round r-example/)).toBeTruthy();
    expect(screen.getByText(/Draft freshness is stale/)).toBeTruthy();
    await showSnapshot(source, [request({ latest_round: round({ status: 'awaiting_decision', cleanup_status: 'needs_attention' }),
      run: { status: 'failed', round_id: 'r-example', failure: 'execution' } })]);
    expect(screen.getByRole('alert').textContent).toContain('The private draft was saved');
    expect(screen.getByText(/review-platform recover-review owner\/repo 15/)).toBeTruthy();
    await showSnapshot(source, [request({ run: { status: 'failed', round_id: null, failure: 'stale_preparation' } })]);
    expect(screen.getByRole('alert').textContent).toContain('Prepare and inspect the review again');
    await showSnapshot(source, [request({ latest_round: round({ status: 'failed', failure: 'timeout', cleanup_pending: false, cleanup_status: 'complete' }),
      run: { status: 'failed', round_id: 'r-example', failure: 'execution' } })]);
    expect(screen.getByRole('alert').textContent).toContain('The GUI review job failed');
    expect(screen.getByText(/Agent timed out/)).toBeTruthy();
    unmount();
    expect(source.close).toHaveBeenCalledOnce();
  });

  it('loads a private draft only on request and renders its exact contents as text', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    let finishDraft;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockImplementationOnce(() => new Promise((resolve) => { finishDraft = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status: 'awaiting_decision' }) })]);
    await screen.findByText('View draft');
    expect(screen.queryByText(draft().lead_advice)).toBeNull();
    const view = await clickMaterialButton(user, container, 'View draft');
    expect(view.disabled).toBe(true);
    expect(fetchMock.mock.calls[1][0]).toBe('/api/drafts/r-example?repository=owner%2Frepo&number=15');
    expect(fetchMock.mock.calls[1][1]).toEqual({ method: 'GET', cache: 'no-store', headers: { Authorization: 'Bearer saved-token' } });
    await clickMaterialButton(user, container, 'Loading draft…');
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await act(() => finishDraft({ ok: true, json: async () => draft() }));
    const panel = await screen.findByRole('region', { name: 'Private draft for owner/repo #15' });
    expect(within(panel).getByRole('region', { name: 'Review comments' }).textContent).toBe(draft().publication_body);
    expect(within(panel).getByRole('region', { name: 'Private lead advice' }).textContent).toBe(draft().lead_advice);
    expect(within(panel).getByText('Accept with nits')).toBeTruthy();
    expect(within(panel).getByText('Fresh at inspection')).toBeTruthy();
    expect(within(panel).getByText('a'.repeat(40))).toBeTruthy();
    expect(within(panel).getByText('d'.repeat(64))).toBeTruthy();
    expect(panel.querySelector('script, img, a')).toBeNull();
    expect(within(panel).getByText(/This advice stays private/)).toBeTruthy();
    await clickMaterialButton(user, container, 'Close draft');
    expect(screen.queryByRole('region', { name: 'Private draft for owner/repo #15' })).toBeNull();
    expect(screen.queryByText(draft().lead_advice)).toBeNull();
  });

  it.each([
    ['stale', 'The PR inputs changed or the PR closed. This draft is stale.'],
    ['unknown', 'Current PR freshness could not be verified. Recheck GitHub before publishing. You can still reject this draft.'],
  ])('allows inspecting a draft with %s freshness and explains its limits', async (freshness, message) => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft({ freshness, lead_advice: '' }) }));
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status: 'awaiting_decision' }) })]);
    await screen.findByText('View draft');
    await clickMaterialButton(user, container, 'View draft');
    const panel = await screen.findByRole('region', { name: 'Private draft for owner/repo #15' });
    expect(within(panel).getByText(message)).toBeTruthy();
    expect(within(panel).getByText('No private lead advice.')).toBeTruthy();
    expect(within(panel).getByText(/Reopen it to check again/)).toBeTruthy();
  });

  it('keeps the requested round fixed during loading and live updates until explicitly reopened', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    let finishDraft;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockImplementationOnce(() => new Promise((resolve) => { finishDraft = resolve; }))
      .mockResolvedValueOnce({ ok: true, json: async () => draft({ round_id: 'r-new', head_sha: 'b'.repeat(40), revision: 2, publication_body: 'New draft' }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    await showSnapshot(source, [request({ latest_round: round({ status: 'awaiting_decision' }) })]);
    await screen.findByText('View draft');
    await clickMaterialButton(user, container, 'View draft');
    const updated = request({ head_sha: 'b'.repeat(40), revision: 2,
      latest_round: round({ id: 'r-new', head_sha: 'b'.repeat(40), revision: 2, status: 'awaiting_decision' }) });
    await showSnapshot(source, [updated]);
    await act(() => finishDraft({ ok: true, json: async () => draft() }));
    const panel = await screen.findByRole('region', { name: 'Private draft for owner/repo #15' });
    expect(within(panel).getByText('r-example')).toBeTruthy();
    expect(within(panel).getByText('a'.repeat(40))).toBeTruthy();
    expect(within(panel).getByRole('region', { name: 'Review comments' }).textContent).toBe(draft().publication_body);
    expect(within(panel).getByText(/This view keeps the draft you opened/)).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await clickMaterialButton(user, container, 'View draft');
    await waitFor(() => expect(screen.getByRole('region', { name: 'Review comments' }).textContent).toBe('New draft'));
    expect(fetchMock.mock.calls[2][0]).toContain('/api/drafts/r-new?');
    expect(screen.queryByText(/This view keeps the draft you opened/)).toBeNull();
  });

  it.each(['stale', 'unknown'])('confirms the displayed draft with %s freshness and waits for SSE after rejection', async (freshness) => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    let finishReject;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft({ freshness }) })
      .mockImplementationOnce(() => new Promise((resolve) => { finishReject = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    const readyRound = round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' });
    await showSnapshot(source, [request({ authorization: 'pr_cycle', latest_round: readyRound })]);
    await screen.findByText('View draft');
    await clickMaterialButton(user, container, 'View draft');
    await screen.findByText(draft().lead_advice);
    await clickMaterialButton(user, container, 'Reject draft');
    const confirmation = screen.getByRole('region', { name: 'Confirm draft rejection' });
    expect(within(confirmation).getByText('r-example')).toBeTruthy();
    expect(within(confirmation).getByText('d'.repeat(64))).toBeTruthy();
    expect(within(confirmation).getByText(/private draft and current authorization are preserved/)).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await clickMaterialButton(user, container, 'Cancel rejection');
    expect(screen.queryByRole('region', { name: 'Confirm draft rejection' })).toBeNull();
    await clickMaterialButton(user, container, 'Reject draft');
    // New PR inputs do not prevent rejecting the old draft or alter the inspected decision.
    await showSnapshot(source, [request({ authorization: 'pr_cycle', head_sha: 'b'.repeat(40), revision: 2, latest_round: readyRound })]);
    const confirm = await clickMaterialButton(user, container, 'Confirm rejection');
    expect(confirm.disabled).toBe(true);
    expect(fetchMock.mock.calls[2][0]).toBe('/api/drafts/r-example/reject');
    expect(fetchMock.mock.calls[2][1].headers.Authorization).toBe('Bearer saved-token');
    expect(JSON.parse(fetchMock.mock.calls[2][1].body)).toEqual({ key: draft().key, body_hash: draft().publication_body_sha256 });
    await clickMaterialButton(user, container, 'Rejecting…');
    expect(fetchMock).toHaveBeenCalledTimes(3);
    if (freshness === 'unknown') {
      // SSE can report the recorded decision before its POST response arrives.
      await showSnapshot(source, [request({ authorization: 'pr_cycle', head_sha: 'b'.repeat(40), revision: 2,
        latest_round: { ...readyRound, status: 'rejected' } })]);
    }
    await act(() => finishReject({ ok: true, json: async () => ({ changed: true }) }));
    expect(await screen.findByText(/Draft rejected locally/)).toBeTruthy();
    if (freshness === 'stale') expect(screen.getAllByText('Draft ready').length).toBeGreaterThan(0);
    expect(screen.queryByText(/Round status or cleanup changed/)).toBeNull();
    expect(screen.queryByText('Reject draft')).toBeNull();
    expect(screen.getByText(draft().lead_advice)).toBeTruthy();
    await showSnapshot(source, [request({ authorization: 'pr_cycle', head_sha: 'b'.repeat(40), revision: 2,
      latest_round: { ...readyRound, status: 'rejected' } })]);
    expect(screen.getAllByText('Rejected').length).toBeGreaterThan(0);
    expect(screen.getByText('PR cycle')).toBeTruthy();
    expect(screen.queryByText('Draft ready')).toBeNull();
  });

  it.each([
    [true, 'in_progress'], [true, 'needs_attention'], [true, 'unknown'], [false, 'unknown'],
  ])('gates rejection until cleanup is complete (%s, %s)', async (cleanup_pending, cleanup_status) => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft() });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    await showSnapshot(source, [request({ latest_round: round({ status: 'awaiting_decision', cleanup_pending, cleanup_status }) })]);
    await screen.findByText('View draft');
    await clickMaterialButton(user, container, 'View draft');
    await screen.findByText(draft().lead_advice);
    expect((await clickMaterialButton(user, container, 'Reject draft')).disabled).toBe(true);
    expect(screen.queryByRole('region', { name: 'Confirm draft rejection' })).toBeNull();
    await showSnapshot(source, [request({ latest_round: round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' }) })]);
    expect((await clickMaterialButton(user, container, 'Reject draft')).disabled).toBe(false);
    expect(screen.getByRole('region', { name: 'Confirm draft rejection' })).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it.each([
    { id: 'r-new' }, { status: 'publishing' }, { cleanup_pending: true, cleanup_status: 'in_progress' },
  ])('invalidates an open rejection confirmation when the round changes: %j', async (change) => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft() });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    const readyRound = round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' });
    await showSnapshot(source, [request({ latest_round: readyRound })]);
    await screen.findByText('View draft');
    await clickMaterialButton(user, container, 'View draft');
    await screen.findByText(draft().lead_advice);
    await clickMaterialButton(user, container, 'Reject draft');
    await showSnapshot(source, [request({ latest_round: { ...readyRound, ...change } })]);
    expect(screen.getByText(/Round status or cleanup changed/)).toBeTruthy();
    expect((await clickMaterialButton(user, container, 'Confirm rejection')).disabled).toBe(true);
    // Returning to the prior state must not silently restore the old confirmation.
    await showSnapshot(source, [request({ latest_round: readyRound })]);
    expect((await clickMaterialButton(user, container, 'Confirm rejection')).disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(screen.getByRole('region', { name: 'Confirm draft rejection' }).textContent).toContain('r-example');
    await clickMaterialButton(user, container, 'Cancel rejection');
    expect((await clickMaterialButton(user, container, 'Reject draft')).disabled).toBe(true);
  });

  it.each([409, 401, 'offline', 'already_rejected'])('handles rejection result %s without losing private contents unless the session ends', async (result) => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft() });
    if (result === 'offline') fetchMock.mockRejectedValueOnce(new Error('offline'));
    else if (result === 'already_rejected') fetchMock.mockResolvedValueOnce({ ok: true, json: async () => ({ changed: false }) });
    else fetchMock.mockResolvedValueOnce({ ok: false, status: result, json: async () => ({ error: 'Draft decision changed. Reopen it.' }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' }) })]);
    await screen.findByText('View draft');
    await clickMaterialButton(user, container, 'View draft');
    await screen.findByText(draft().lead_advice);
    await clickMaterialButton(user, container, 'Reject draft');
    await clickMaterialButton(user, container, 'Confirm rejection');
    const message = result === 401 ? /Your session ended/ : result === 'offline' ? /Could not connect to the local API/
      : result === 'already_rejected' ? /This draft was already rejected/ : /Draft decision changed/;
    await screen.findByText(message);
    if (result === 401) {
      expect(screen.queryByText(draft().lead_advice)).toBeNull();
      expect(screen.queryByRole('region', { name: 'Confirm draft rejection' })).toBeNull();
      expect(sessionStorage.getItem('review-platform-session')).toBeNull();
    } else {
      expect(screen.getByText(draft().lead_advice)).toBeTruthy();
      if (result !== 'already_rejected') expect(screen.getByText('Confirm rejection')).toBeTruthy();
    }
  });

  it('removes old draft contents before reporting integrity or connection failures', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft() })
      .mockResolvedValueOnce({ ok: false, status: 422, json: async () => ({ error: 'Could not verify the private draft.' }) })
      .mockRejectedValueOnce(new Error('offline')));
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status: 'awaiting_decision' }) })]);
    await screen.findByText('View draft');
    await clickMaterialButton(user, container, 'View draft');
    await screen.findByText(draft().lead_advice);
    await clickMaterialButton(user, container, 'View draft');
    expect(await screen.findByText('Could not verify the private draft.')).toBeTruthy();
    expect(screen.queryByText(draft().lead_advice)).toBeNull();
    expect(screen.queryByRole('region', { name: 'Private draft for owner/repo #15' })).toBeNull();
    await clickMaterialButton(user, container, 'View draft');
    expect(await screen.findByText('Could not connect to the local API. Try again.')).toBeTruthy();
  });

  it('clears private contents and draft controls when the lead session expires', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft() })
      .mockResolvedValueOnce({ ok: false, status: 401 }));
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status: 'awaiting_decision' }) })]);
    await screen.findByText('View draft');
    await clickMaterialButton(user, container, 'View draft');
    await screen.findByText(draft().lead_advice);
    await clickMaterialButton(user, container, 'Refresh from GitHub');
    await screen.findByText(/Your session ended/);
    expect(screen.queryByRole('region', { name: 'Private draft for owner/repo #15' })).toBeNull();
    expect(screen.queryByText(draft().lead_advice)).toBeNull();
    expect(screen.queryByText('View draft')).toBeNull();
    expect(sessionStorage.getItem('review-platform-session')).toBeNull();
  });

  it('offers draft inspection only to signed-in leads and only for completed drafts', async () => {
    const { unmount } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status: 'awaiting_decision' }) })]);
    expect(screen.queryByText('View draft')).toBeNull();
    unmount();
    sessionStorage.setItem('review-platform-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal('fetch', fetchMock);
    render(App);
    const source = FakeEventSource.instances[1];
    for (const status of ['awaiting_decision', 'publishing', 'publication_unknown', 'published', 'rejected']) {
      await showSnapshot(source, [request({ latest_round: round({ status }) })]);
      expect(await screen.findByText('View draft')).toBeTruthy();
    }
    for (const status of ['starting', 'running', 'failed', 'interrupted']) {
      await showSnapshot(source, [request({ latest_round: round({ status }) })]);
      expect(screen.queryByText('View draft')).toBeNull();
    }
    expect(fetchMock).toHaveBeenCalledOnce();
  });

  it('keeps request details open and updates them when a new snapshot arrives', async () => {
    const user = userEvent.setup();
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    await act(() => source.open());
    await showSnapshot(source, [request()]);

    expect(screen.getByText('owner/repo')).toBeTruthy();
    expect(screen.getByText('#15')).toBeTruthy();
    expect(screen.getByText('Awaiting lead')).toBeTruthy();
    expect(screen.getByText('Revision 1')).toBeTruthy();
    expect(screen.getAllByText('No rounds yet').some((element) => !element.closest('[hidden]'))).toBe(true);
    expect(screen.queryByRole('heading', { name: 'Author discussion' })).toBeNull();

    const detailsControl = container.querySelector('md-text-button');
    await waitFor(() => expect(detailsControl.shadowRoot.querySelector('button')).toBeTruthy());
    const button = detailsControl.shadowRoot.querySelector('button');
    expect(button.getAttribute('aria-expanded')).toBe('false');
    await user.click(button);
    await waitFor(() => expect(button.getAttribute('aria-expanded')).toBe('true'));
    expect(screen.getByRole('heading', { name: 'Author discussion' })).toBeTruthy();
    expect(screen.getByText('Not yet observed')).toBeTruthy();

    await showSnapshot(source, [request({
      revision: 2,
      authorization: 'one_round',
      discussion: { issue_comments: 2, review_comments: 1, review_bodies: 3 },
      validated_model: { model: 'model-alpha', agent: 'claude' },
      latest_round: round({ revision: 2 }),
    })]);

    expect(screen.getByText('Revision 2')).toBeTruthy();
    expect(screen.getByText('One round')).toBeTruthy();
    expect(screen.getAllByText('Running').length).toBeGreaterThan(0);
    expect(screen.getByRole('heading', { name: 'Author discussion' })).toBeTruthy();
    const discussion = screen.getByRole('heading', { name: 'Author discussion' }).parentElement;
    expect(within(discussion).getByText('Issue comments').nextElementSibling.textContent).toBe('2');
    expect(within(discussion).getByText('Review comments').nextElementSibling.textContent).toBe('1');
    expect(within(discussion).getByText('Review bodies').nextElementSibling.textContent).toBe('3');
    const validation = screen.getByRole('heading', { name: 'Current model validation' }).parentElement;
    expect(within(validation).getByText('model-alpha')).toBeTruthy();
    expect(within(validation).getByText('claude')).toBeTruthy();
    const latestRound = screen.getByRole('heading', { name: 'Latest round', level: 3 }).parentElement;
    expect(within(latestRound).getByText('r-example')).toBeTruthy();
    expect(within(latestRound).getByText('Running')).toBeTruthy();
    expect(within(latestRound).getByText('model-alpha')).toBeTruthy();
    expect(screen.queryByText('Cleanup pending')).toBeNull();
    expect(screen.queryByText('Review stopped; cleanup may be required')).toBeNull();

    await user.click(button);
    await waitFor(() => expect(button.getAttribute('aria-expanded')).toBe('false'));
    expect(screen.queryByRole('heading', { name: 'Author discussion' })).toBeNull();
  });

  it('keeps the last list during connection and state errors and recovers on a new snapshot', async () => {
    render(App);
    const source = FakeEventSource.instances[0];
    await act(() => source.open());
    await showSnapshot(source, [request()]);

    await act(() => source.disconnect());
    expect(screen.getByRole('alert').textContent).toContain('Connection lost');
    expect(screen.getByRole('alert').textContent).toContain('Showing the last received list');
    expect(screen.getByText('owner/repo')).toBeTruthy();

    await act(() => source.open());
    await act(() => source.send('state_error', 'Could not read local review state.'));
    expect(screen.getByRole('alert').textContent).toContain('Could not read local review state');
    expect(screen.getByText('owner/repo')).toBeTruthy();

    await showSnapshot(source, [request({ revision: 2 })]);
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByText('Revision 2')).toBeTruthy();
  });

  it('reports malformed updates without discarding the last successful snapshot', async () => {
    render(App);
    const source = FakeEventSource.instances[0];
    await showSnapshot(source, [request()]);

    await act(() => source.send('requests', '{invalid'));
    expect(screen.getByRole('alert').textContent).toContain('Could not read the latest request update');
    expect(screen.getByText('owner/repo')).toBeTruthy();

    await act(() => source.send('requests', JSON.stringify({ requests: null })));
    expect(screen.getByRole('alert').textContent).toContain('Could not read the latest request update');
    await showSnapshot(source, []);
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByText(/No review requests recorded yet/)).toBeTruthy();
  });

  it('shows cleanup messages according to the runner state', async () => {
    render(App);
    const source = FakeEventSource.instances[0];

    await showSnapshot(source, [request({ latest_round: round() })]);
    expect(screen.getAllByText('Running').length).toBeGreaterThan(0);
    expect(screen.queryByText(/cleanup/i)).toBeNull();

    await showSnapshot(source, [request({ latest_round: round({ cleanup_status: 'needs_attention' }) })]);
    expect(screen.getByText('Review stopped; cleanup may be required')).toBeTruthy();

    await showSnapshot(source, [request({
      latest_round: round({ status: 'awaiting_decision', cleanup_status: 'in_progress' }),
    })]);
    expect(screen.getByText('Finishing cleanup…')).toBeTruthy();
    expect(screen.queryByText('Cleanup pending')).toBeNull();

    await showSnapshot(source, [request({
      latest_round: round({ status: 'awaiting_decision', cleanup_status: 'needs_attention' }),
    })]);
    expect(screen.getByText('Cleanup pending')).toBeTruthy();

    await showSnapshot(source, [request({ latest_round: round({ cleanup_status: 'unknown' }) })]);
    expect(screen.getByText('Cleanup status unavailable')).toBeTruthy();

    await showSnapshot(source, [request({
      latest_round: round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' }),
    })]);
    expect(screen.queryByText(/cleanup/i)).toBeNull();
  });
});
