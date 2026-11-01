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
