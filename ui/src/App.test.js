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

async function showSnapshot(source, requests) {
  await act(() => source.snapshot(requests));
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
