import '@material/web/button/text-button.js';
import '@material/web/button/filled-button.js';
import '@material/web/divider/divider.js';
import '@material/web/iconbutton/icon-button.js';
import '@material/web/labs/gb/components/badge/md-gb-badge.js';
import '@material/web/menu/menu.js';
import '@material/web/menu/menu-item.js';
import '@material/web/progress/linear-progress.js';
import { act, render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import App from './App.svelte';

type MaterialControl = HTMLElement & { disabled?: boolean };
type UserEvent = ReturnType<typeof userEvent.setup>;
type FetchResponse = { ok: boolean; status?: number; json?: () => Promise<unknown> };
type FinishFetch = (response: FetchResponse) => void;
type ValidatedModel = ModelValidation | string;

interface Discussion {
  issue_comments: number;
  review_comments: number;
  review_bodies: number;
}

interface ModelValidation {
  model: string;
  agent: string;
}

interface RequestContext {
  title: string;
  author: string;
  state: string;
  draft: boolean;
  head_sha: string;
  revision: number;
  checked_at: number;
  checks: string | null;
  head_changed: boolean;
  discussion_changed: boolean;
}

interface ReviewRound {
  id: string;
  status: string;
  cleanup_pending: boolean;
  cleanup_status: string;
  head_sha: string;
  revision: number;
  model: string;
  agent: string;
  freshness?: string;
  failure?: string | null;
  receipt?: {
    review_id: number;
    url: string;
    publisher_login: string;
    commit_id: string;
  } | null;
}

interface Preparation {
  head_sha: string;
  revision: number;
  authorization: string;
  configuration_sha256: string;
  checks: string;
  readiness: string;
  readiness_message: string;
  proposed_model: string;
  proposal_matches_coauthor: boolean;
  validated_model: ValidatedModel | null;
  resolved_model: string;
  models: ModelValidation[];
}

interface RequestSnapshot {
  repository: string;
  number: number;
  head_sha: string;
  revision: number;
  authorization: string;
  discussion: Discussion | null;
  validated_model: ValidatedModel | null;
  latest_round: ReviewRound | null;
  context?: RequestContext;
  run?: { status: string; round_id?: string | null; failure?: string | null };
  publication?: {
    round_id?: string;
    status: string;
    error?: string | null;
    freshness?: string | null;
  };
}

interface DraftSnapshot {
  key: { repository: string; number: number };
  round_id: string;
  head_sha: string;
  revision: number;
  publisher_login: string;
  decision: string;
  model: string;
  agent: string;
  verdict: string;
  freshness: string;
  publication_body: string;
  publication_body_sha256: string;
  lead_advice: string;
}

class FakeEventSource extends EventTarget {
  static instances: FakeEventSource[] = [];
  readonly url: string;
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  close = vi.fn();

  constructor(url: string) {
    super();
    this.url = url;
    this.onopen = null;
    this.onerror = null;
    FakeEventSource.instances.push(this);
  }

  open() {
    this.onopen?.(new Event('open'));
  }

  disconnect() {
    this.onerror?.(new Event('error'));
  }

  send(type: string, data: string) {
    this.dispatchEvent(new MessageEvent(type, { data }));
  }

  snapshot(requests: RequestSnapshot[]) {
    this.send('requests', JSON.stringify({ requests }));
  }
}

function request(overrides: Partial<RequestSnapshot> = {}): RequestSnapshot {
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

function context(overrides: Partial<RequestContext> = {}): RequestContext {
  return {
    title: 'Fix login redirect', author: 'alice', state: 'open', draft: false,
    head_sha: 'a'.repeat(40), revision: 1, checked_at: Math.floor(Date.now() / 1000),
    checks: null, head_changed: false, discussion_changed: false, ...overrides,
  };
}

function round(overrides: Partial<ReviewRound> = {}): ReviewRound {
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

function preparation(overrides: Partial<Preparation> = {}): Preparation {
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

function preparedRequest(overrides: Partial<RequestSnapshot> = {}): RequestSnapshot {
  return request({ revision: 2, authorization: 'one_round',
    discussion: { issue_comments: 0, review_comments: 0, review_bodies: 0 }, ...overrides });
}

function draft(overrides: Partial<DraftSnapshot> = {}): DraftSnapshot {
  return {
    key: { repository: 'owner/repo', number: 15 }, round_id: 'r-example', head_sha: 'a'.repeat(40), revision: 1,
    publisher_login: 'review-bot',
    decision: 'awaiting_decision', model: 'model-alpha', agent: 'claude', verdict: 'ACCEPT_WITH_NITS', freshness: 'fresh',
    publication_body: "# Finding\n<script>alert('draft')</script>\n\n_Agent review via LuthiAI (round r-example)._",
    publication_body_sha256: 'd'.repeat(64), lead_advice: '<img src=x onerror=alert(1)> Private advice', ...overrides,
  };
}

async function showSnapshot(source: FakeEventSource, requests: RequestSnapshot[]) {
  await act(() => source.snapshot(requests));
}

async function clickMaterialButton(user: UserEvent, container: Element, label: string): Promise<MaterialControl> {
  const control = [...container.querySelectorAll('md-text-button, md-filled-button, md-menu-item')]
    .find((button) => button.textContent.trim() === label) as HTMLElement;
  expect(control).toBeTruthy();
  const menu = control.closest('md-menu');
  if (menu && !menu.open) await openActionsMenu(user, menu.closest('.request-item'));
  await waitFor(() => expect(control.shadowRoot.querySelector('button')).toBeTruthy());
  await user.click(control.shadowRoot.querySelector('button'));
  return control;
}

async function openActionsMenu(user: UserEvent, row: Element) {
  const trigger = row.querySelector('md-icon-button');
  await trigger.updateComplete;
  const menu = row.querySelector('md-menu');
  const opened = new Promise((resolve) => menu.addEventListener('opened', resolve, { once: true }));
  await user.click(trigger.shadowRoot.querySelector('button'));
  await opened;
  expect(menu.open).toBe(true);
  return menu;
}

beforeEach(() => {
  FakeEventSource.instances = [];
  sessionStorage.clear();
  window.history.replaceState(null, '', '/');
  vi.stubGlobal('EventSource', FakeEventSource);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('refresh open PRs', () => {
  it('places refresh in the header and explains the sign-in requirement on hover and focus', async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request()]);
    const button = await clickMaterialButton(user, container, 'Refresh open PRs');
    expect(button.disabled).toBe(true);
    const header = container.querySelector<HTMLElement>('.page-header');
    expect(header.contains(button)).toBe(true);
    expect(within(header).getByRole('heading', { name: 'LuthiAI', level: 1 })).toBeTruthy();
    expect(screen.getByRole('heading', { name: 'Pull requests 1', level: 2 })).toBeTruthy();
    expect(button.closest('.request-list')).toBeNull();
    const trigger = within(header).getByRole('group', { name: 'Refresh open PRs' });
    const tooltip = document.getElementById(button.getAttribute('aria-describedby'));
    expect(tooltip.textContent).toBe('Sign in to refresh PRs.');
    expect(trigger.tabIndex).toBe(0);
    await user.hover(trigger);
    expect(tooltip.hasAttribute('data-test-popover-open')).toBe(true);
    await user.unhover(trigger);
    expect(tooltip.hasAttribute('data-test-popover-open')).toBe(false);
    await act(() => trigger.focus());
    expect(tooltip.hasAttribute('data-test-popover-open')).toBe(true);
    await user.keyboard('{Escape}');
    expect(tooltip.hasAttribute('data-test-popover-open')).toBe(false);
    button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('waits for the list and disables refreshing when no open or unchecked PRs remain', async () => {
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    await screen.findByText('Refresh open PRs');
    const button = container.querySelector('.page-header md-filled-button') as MaterialControl;
    const tooltip = document.getElementById(button.getAttribute('aria-describedby'));
    await waitFor(() => expect(fetchMock).toHaveBeenCalledOnce());
    expect(button.disabled).toBe(true);
    expect(tooltip.textContent).toBe('Waiting for the local PR list.');
    for (const snapshot of [[], [request({ context: context({ state: 'closed' }) })]]) {
      await showSnapshot(source, snapshot);
      expect(button.disabled).toBe(true);
      expect(tooltip.textContent).toBe('No open or unchecked PRs to refresh.');
    }
    await showSnapshot(source, [request()]);
    expect(button.disabled).toBe(false);
    expect(tooltip.textContent).toBe('Check open and unchecked PRs for changes on GitHub.');
  });

  it('keeps batch progress current and clears the completed summary when the next PR action starts', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    let finishFirst: FinishFetch;
    let finishSecond: FinishFetch;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockImplementationOnce(() => new Promise((resolve) => { finishFirst = resolve; }))
      .mockImplementationOnce(() => new Promise((resolve) => { finishSecond = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    const initial = [request({ context: context() }), request({ number: 16 }), request({ number: 17, context: context({ state: 'closed' }) })];
    await showSnapshot(source, initial);
    await screen.findAllByText('Refresh from GitHub');
    const button = await clickMaterialButton(user, container, 'Refresh open PRs');
    expect(button.disabled).toBe(true);
    expect(screen.getByText('Refreshing 0 of 2 PRs…').getAttribute('role')).toBe('status');
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(fetchMock.mock.calls[1][0]).toBe('/api/requests/refresh');
    expect(fetchMock.mock.calls[1][1].headers.Authorization).toBe('Bearer saved-token');
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({ repository: 'owner/repo', number: 15 });
    button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await showSnapshot(source, [request({ revision: 2 }), ...initial.slice(1), request({ number: 18 })]);
    await act(() => finishFirst({ ok: true, json: async () => ({ changed: true, authorization_cleared: true }) }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(3));
    expect(screen.getByText('Refreshing 1 of 2 PRs…').getAttribute('role')).toBe('status');
    expect(JSON.parse(fetchMock.mock.calls[2][1].body)).toEqual({ repository: 'owner/repo', number: 16 });
    await act(() => finishSecond({ ok: true, json: async () => ({ changed: false }) }));
    expect(await screen.findByText('Refreshed 2 of 2 PRs.')).toBeTruthy();
    expect(screen.getByText('Revision 2')).toBeTruthy();
    expect(screen.getByText(/Inspect the changes and allow the review again/)).toBeTruthy();
    expect(button.disabled).toBe(false);
    expect(fetchMock).toHaveBeenCalledTimes(3);

    let finishPreparation: FinishFetch;
    fetchMock.mockImplementationOnce(() => new Promise((resolve) => { finishPreparation = resolve; }));
    await clickMaterialButton(user, container.querySelector('.request-item'), 'Check review readiness');
    expect(screen.queryByText('Refreshed 2 of 2 PRs.')).toBeNull();
    expect(screen.getByText('Preparing…')).toBeTruthy();
    await act(() => finishPreparation({ ok: false, status: 502, json: async () => ({ error: 'Could not prepare the review.' }) }));
    expect(await screen.findByText('Could not prepare the review.')).toBeTruthy();
    expect(container.querySelector('.bulk-refresh-status')).toBeNull();
  });

  it.each(['github', 'offline'])('continues after a %s failure and shows both the summary and individual error', async (failure) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true });
    if (failure === 'github') fetchMock.mockResolvedValueOnce({ ok: false, status: 502, json: async () => ({ error: 'GitHub is unavailable. Try again later.' }) });
    else fetchMock.mockRejectedValueOnce(new Error('offline'));
    fetchMock.mockResolvedValueOnce({ ok: true, json: async () => ({ changed: false }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request(), request({ number: 16 })]);
    await screen.findAllByText('Refresh from GitHub');
    await clickMaterialButton(user, container, 'Refresh open PRs');
    expect(await screen.findByText('Refreshed 1 of 2 PRs. 1 could not be refreshed; see the PR rows for details.')).toBeTruthy();
    const [first, second] = [...container.querySelectorAll('.request-item')] as HTMLElement[];
    expect(within(first).getByText(failure === 'github' ? 'GitHub is unavailable. Try again later.' : 'Could not connect to the local API. Try again.')).toBeTruthy();
    expect(within(second).getByText('No code or author discussion changes found.')).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it('stops when the session expires and clears the old refresh error when signing in again', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: false, status: 401 });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request(), request({ number: 16 })]);
    await screen.findAllByText('Refresh from GitHub');
    const button = await clickMaterialButton(user, container, 'Refresh open PRs');
    expect(await screen.findByText(/Refresh stopped because your session ended/)).toBeTruthy();
    expect(await screen.findByLabelText('One-time code')).toBeTruthy();
    expect(sessionStorage.getItem('luthiai-session')).toBeNull();
    expect(button.disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);

    let finishSignIn: FinishFetch;
    fetchMock.mockImplementationOnce(() => new Promise((resolve) => { finishSignIn = resolve; }));
    await user.type(screen.getByLabelText('One-time code'), 'new-code');
    await clickMaterialButton(user, container.querySelector('.sign-in'), 'Sign in');
    expect(screen.queryByText(/Refresh stopped because your session ended/)).toBeNull();
    expect(screen.getByText('Signing in…')).toBeTruthy();
    await act(() => finishSignIn({ ok: true, json: async () => ({ token: 'new-token' }) }));
    await waitFor(() => expect(screen.queryByLabelText('One-time code')).toBeNull());
    expect(container.querySelector('.bulk-refresh-status')).toBeNull();
  });

  it('waits for pending row actions and skips PRs removed or closed during the batch', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    let finishRowRefresh: FinishFetch;
    let finishBatchRefresh: FinishFetch;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockImplementationOnce(() => new Promise((resolve) => { finishRowRefresh = resolve; }))
      .mockImplementationOnce(() => new Promise((resolve) => { finishBatchRefresh = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    await showSnapshot(source, [request(), request({ number: 16 }), request({ number: 17 })]);
    await screen.findAllByText('Refresh from GitHub');
    await clickMaterialButton(user, container.querySelector('.request-item'), 'Refresh from GitHub');
    const button = await clickMaterialButton(user, container, 'Refresh open PRs');
    expect(button.disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await act(() => finishRowRefresh({ ok: true, json: async () => ({ changed: false }) }));
    await waitFor(() => expect(button.disabled).toBe(false));
    await clickMaterialButton(user, container, 'Refresh open PRs');
    await showSnapshot(source, [request(), request({ number: 17, context: context({ state: 'closed' }) })]);
    await act(() => finishBatchRefresh({ ok: true, json: async () => ({ changed: false }) }));
    expect(await screen.findByText('Refreshed 1 of 3 PRs. Skipped 2 busy, removed, or closed PRs.')).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });
});

describe('sign-in link', () => {
  it('exchanges the URL code and removes it from the address bar', async () => {
    window.history.pushState({}, '', '/?code=url-code');
    const fetchMock = vi.fn()
      .mockResolvedValueOnce({ ok: true, json: async () => ({ token: 'session-token' }) });
    vi.stubGlobal('fetch', fetchMock);
    render(App);

    await waitFor(() => expect(fetchMock).toHaveBeenCalledOnce());
    expect(fetchMock.mock.calls[0][0]).toBe('/api/session');
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({ code: 'url-code' });
    expect(window.location.search).toBe('');
    await waitFor(() => expect(sessionStorage.getItem('luthiai-session')).toBe('session-token'));
    await waitFor(() => expect(screen.queryByLabelText('One-time code')).toBeNull());
    await showSnapshot(FakeEventSource.instances[0], [request()]);
    expect(await screen.findByText('Refresh from GitHub')).toBeTruthy();
  });

  it('restores a valid session instead of exchanging an already used URL code', async () => {
    sessionStorage.setItem('luthiai-session', 'saved-token');
    window.history.pushState({}, '', '/?code=url-code');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);

    await waitFor(() => expect(fetchMock).toHaveBeenCalledOnce());
    expect(fetchMock.mock.calls[0][0]).toBe('/api/session');
    expect(fetchMock.mock.calls[0][1].headers.Authorization).toBe('Bearer saved-token');
    await showSnapshot(FakeEventSource.instances[0], [request()]);
    const button = [...container.querySelectorAll('.request-item md-menu-item')]
      .find((item) => item.textContent.trim() === 'Refresh from GitHub');
    expect(button).toBeTruthy();
    await waitFor(() => expect(screen.queryByLabelText('One-time code')).toBeNull());
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(sessionStorage.getItem('luthiai-session')).toBe('saved-token');
    expect(window.location.search).toBe('');
  });

  it('exchanges a URL code without showing sign-in after a stored session expires', async () => {
    sessionStorage.setItem('luthiai-session', 'expired-token');
    window.history.pushState({}, '', '/?code=url-code');
    let finishSessionCheck: FinishFetch;
    const fetchMock = vi.fn()
      .mockImplementationOnce(() => new Promise((resolve) => { finishSessionCheck = resolve; }))
      .mockResolvedValueOnce({ ok: true, json: async () => ({ token: 'session-token' }) });
    vi.stubGlobal('fetch', fetchMock);
    render(App);

    expect(screen.queryByLabelText('One-time code')).toBeNull();
    await act(() => finishSessionCheck({ ok: false, status: 401 }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    expect(screen.queryByLabelText('One-time code')).toBeNull();
    expect(fetchMock.mock.calls[0][0]).toBe('/api/session');
    expect(fetchMock.mock.calls[0][1].headers.Authorization).toBe('Bearer expired-token');
    expect(fetchMock.mock.calls[1][0]).toBe('/api/session');
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({ code: 'url-code' });
    await waitFor(() => expect(sessionStorage.getItem('luthiai-session')).toBe('session-token'));
    expect(window.location.search).toBe('');
  });
});

describe('review requests page', () => {
  it.each([
    [preparedRequest(), 'Check review readiness'],
    [request({ latest_round: round({ status: 'awaiting_decision' }) }), 'Read private draft'],
  ])('uses the same action label in the menu and next step for %o', async (snapshot, label) => {
    sessionStorage.setItem('luthiai-session', 'saved-token');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true }));
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [snapshot]);
    await screen.findAllByText(label);
    await waitFor(() => expect(container.querySelector('.next-step md-filled-button')?.textContent.trim()).toBe(label));
    const menu = container.querySelector('md-menu');
    const item = within(menu).getByText(label).closest('md-menu-item');
    expect(item.ariaLabel.startsWith(label)).toBe(true);
  });

  it('shows the sign-in link only when its target is available after session checking', async () => {
    sessionStorage.setItem('luthiai-session', 'expired-token');
    let finishSessionCheck: FinishFetch;
    vi.stubGlobal('fetch', vi.fn().mockImplementationOnce(() => new Promise((resolve) => { finishSessionCheck = resolve; })));
    render(App);
    await showSnapshot(FakeEventSource.instances[0], [request()]);
    expect(screen.queryByRole('link', { name: 'Sign in to continue' })).toBeNull();
    expect(document.getElementById('sign-in-heading')).toBeNull();
    await act(() => finishSessionCheck({ ok: false, status: 401 }));
    const link = await screen.findByRole('link', { name: 'Sign in to continue' });
    expect(document.querySelector(link.getAttribute('href'))).toBe(screen.getByRole('heading', { name: 'Sign in for actions' }));
  });

  it('offers public details in the row menu and closes it after selection or outside clicks', async () => {
    const user = userEvent.setup();
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request()]);
    const row = container.querySelector('.request-item');
    const trigger = row.querySelector('md-icon-button');
    const menu = row.querySelector('md-menu');
    expect(menu.open).toBe(false);
    expect(row.querySelector('.request-heading md-icon-button')).toBe(trigger);
    await openActionsMenu(user, row);
    expect(trigger.ariaLabel).toBe('More actions for owner/repo #15');
    expect(trigger.ariaHasPopup).toBe('menu');
    expect(trigger.ariaExpanded).toBe('true');
    expect([...menu.querySelectorAll('md-menu-item')].map((item) => item.textContent.trim()))
      .toEqual(['Show details']);
    await clickMaterialButton(user, row, 'Show details');
    await waitFor(() => expect(menu.open).toBe(false));
    expect(screen.getByRole('heading', { name: 'Author discussion' })).toBeTruthy();
    await openActionsMenu(user, row);
    expect(within(menu).getByText('Hide details')).toBeTruthy();
    await user.click(screen.getByRole('heading', { name: 'LuthiAI', level: 1 }));
    await waitFor(() => expect(menu.open).toBe(false));
    await waitFor(() => expect(trigger.ariaExpanded).toBe('false'));
  });

  it('keeps row menus independent and refreshes the PR whose menu was selected', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ changed: false }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request(), request({ number: 16 })]);
    await screen.findAllByText('Refresh from GitHub');
    const [first, second] = container.querySelectorAll('.request-item');
    expect(second.querySelector('.next-step md-filled-button').textContent.trim()).toBe('Fetch pull request details');
    await openActionsMenu(user, first);
    const secondMenu = await openActionsMenu(user, second);
    expect(first.querySelector('md-menu').open).toBe(false);
    expect(secondMenu.anchorElement).toBe(second.querySelector('md-icon-button'));
    await clickMaterialButton(user, second, 'Refresh from GitHub');
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({ repository: 'owner/repo', number: 16 });
    await waitFor(() => expect(secondMenu.open).toBe(false));
  });

  it('opens the menu with arrow keys, navigates enabled actions, and restores focus on Escape', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true }));
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request()]);
    await screen.findByText('Manage authorization');
    const row = container.querySelector('.request-item');
    const trigger = row.querySelector('md-icon-button');
    const menu = row.querySelector('md-menu');
    const items = [...menu.querySelectorAll('md-menu-item')];
    await trigger.updateComplete;
    trigger.shadowRoot.querySelector('button').focus();
    await user.keyboard('{ArrowDown}');
    await waitFor(() => expect(trigger.ariaExpanded).toBe('true'));
    await waitFor(() => expect(document.activeElement).toBe(items[0]));
    await user.keyboard('{ArrowDown}{ArrowDown}');
    expect(document.activeElement).toBe(items.find((item) => item.textContent.trim() === 'Check review readiness'));
    await user.keyboard('{Escape}');
    await waitFor(() => expect(menu.open).toBe(false));
    expect(document.activeElement).toBe(trigger);
    expect(trigger.shadowRoot.activeElement).toBe(trigger.shadowRoot.querySelector('button'));
    await user.keyboard('{ArrowUp}');
    await waitFor(() => expect(document.activeElement).toBe(items.at(-1)));
    await user.keyboard('{Escape}');
  });

  it.each(['{Enter}', ' '])('selects a menu action with the keyboard using %s', async (key) => {
    const user = userEvent.setup();
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request()]);
    const row = container.querySelector('.request-item');
    const menu = await openActionsMenu(user, row);
    await user.keyboard(key);
    await waitFor(() => expect(menu.open).toBe(false));
    expect(screen.getByRole('heading', { name: 'Author discussion' })).toBeTruthy();
  });

  it('shows PR context as text with a GitHub check time distinct from local updates', async () => {
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    await showSnapshot(source, [request()]);
    const identity = container.querySelector<HTMLElement>('.request-identity');
    const fallback = within(identity).getByRole('link', { name: 'owner/repo #15' }) as HTMLAnchorElement;
    expect(fallback.href).toBe('https://github.com/owner/repo/pull/15');
    expect(fallback.closest('.pr-title')).toBeTruthy();
    expect(identity.querySelector('.pr-meta')).toBeNull();
    const title = '<img src=x onerror=alert(1)> Fix login redirect';
    const context = { title, author: 'alice', state: 'open', draft: false,
      head_sha: 'a'.repeat(40), revision: 1, checked_at: Math.floor(Date.now() / 1000) - 120,
      checks: 'green', head_changed: false, discussion_changed: true };
    await showSnapshot(source, [request({ context })]);
    expect((screen.getByRole('link', { name: title }) as HTMLAnchorElement).href).toBe('https://github.com/owner/repo/pull/15');
    expect(within(identity).getAllByRole('link')).toHaveLength(1);
    expect(screen.getByText('owner/repo #15 · @alice · Open')).toBeTruthy();
    expect(identity.querySelector('.pr-meta a')).toBeNull();
    expect(screen.getByText('2 minutes ago')).toBeTruthy();
    expect(screen.getByRole('group', { name: 'Local updates' })).toBeTruthy();
    expect(screen.getByText(/the author discussion changed/)).toBeTruthy();
    expect(container.querySelector('img')).toBeNull();
    await showSnapshot(source, [request({ context, latest_round: round() })]);
    expect(screen.getByText('2 minutes ago')).toBeTruthy();
    expect(screen.getByText('Review in progress')).toBeTruthy();
  });

  it('shows an inactive closed PR as a compact summary row', async () => {
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0],
      [request({ context: context({ state: 'closed' }) })]);
    const row = container.querySelector('.request-item');
    expect(screen.getByRole('link', { name: 'Fix login redirect' })).toBeTruthy();
    expect(screen.getByText('owner/repo #15 · @alice · Closed')).toBeTruthy();
    expect(screen.queryByText('Pull request closed')).toBeNull();
    expect(row.querySelector('.request-row')).toBeNull();
    expect(row.querySelector('.request-details')).toBeNull();
    expect(row.querySelector('.next-step')).toBeNull();
    expect(row.querySelector('md-icon-button')).toBeNull();
    await showSnapshot(FakeEventSource.instances[0], [request({
      context: context({ state: 'closed' }),
      latest_round: round({ status: 'published', cleanup_pending: false, cleanup_status: 'complete' }),
    })]);
    expect(row.classList.contains('compact-closed')).toBe(true);
  });

  it('keeps closed-PR controls when a draft still needs a decision', async () => {
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({
      context: context({ state: 'closed' }),
      latest_round: round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' }),
    })]);
    const row = container.querySelector('.request-item');
    expect(screen.getByText('Draft ready for your decision')).toBeTruthy();
    expect(row.querySelector('.request-row')).toBeTruthy();
    expect(row.querySelector('.request-details')).toBeTruthy();
  });

  it('refreshes a compact closed PR and restores full controls when an open snapshot arrives', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    let finishRefresh: FinishFetch;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockImplementationOnce(() => new Promise((resolve) => { finishRefresh = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    const observed = request({
      context: context({ state: 'closed' }),
      discussion: { issue_comments: 0, review_comments: 0, review_bodies: 0 },
      latest_round: round({ status: 'published', cleanup_pending: false, cleanup_status: 'complete' }),
    });
    await showSnapshot(source, [observed]);
    await screen.findByText('Stop managing PR');
    const row = container.querySelector('.request-item');
    expect(row.classList.contains('compact-closed')).toBe(true);
    expect(row.querySelector('.request-row')).toBeNull();
    expect(row.querySelector('.next-step')).toBeNull();
    const menu = await openActionsMenu(user, row);
    expect([...menu.querySelectorAll('md-menu-item')].map((item) => item.textContent.trim()))
      .toEqual(['Refresh from GitHub', 'Stop managing PR']);

    const refresh = await clickMaterialButton(user, row, 'Refresh from GitHub');
    expect(refresh.disabled).toBe(true);
    expect(screen.getByText('Refreshing…')).toBeTruthy();
    expect(fetchMock.mock.calls[1][0]).toBe('/api/requests/refresh');
    expect(fetchMock.mock.calls[1][1].headers.Authorization).toBe('Bearer saved-token');
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({ repository: 'owner/repo', number: 15 });
    await act(() => finishRefresh({ ok: true, json: async () => ({ changed: false }) }));
    expect(await screen.findByText('No code or author discussion changes found.')).toBeTruthy();
    expect(row.classList.contains('compact-closed')).toBe(true);
    expect(screen.queryByText('Manage authorization')).toBeNull();

    await showSnapshot(source, [{ ...observed, context: context({ state: 'open' }) }]);
    expect(screen.getByText('owner/repo #15 · @alice · Open')).toBeTruthy();
    expect(row.classList.contains('compact-closed')).toBe(false);
    expect(row.querySelector('.request-row')).toBeTruthy();
    expect(row.querySelector('.request-details')).toBeTruthy();
    expect(row.querySelector('.next-step')).toBeTruthy();
    expect(within(menu).getByText('Read private draft')).toBeTruthy();
    expect(within(menu).getByText('Check review readiness')).toBeTruthy();
    await clickMaterialButton(user, row, 'Show details');
    expect(screen.getByRole('heading', { name: 'Author discussion' })).toBeTruthy();
    await clickMaterialButton(user, row, 'Manage authorization');
    const panel = screen.getByRole('region', { name: 'Authorize owner/repo #15' });
    expect(within(panel).getByText(observed.head_sha)).toBeTruthy();
    expect(within(panel).getByText('Authorize one round')).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('offers the next action and starts only the currently confirmed model', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation({ readiness: 'ready', validated_model: 'model-alpha' }) })
      .mockResolvedValueOnce({ ok: true, json: async () => ({ accepted: true }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest({ validated_model: { model: 'model-alpha', agent: 'claude' } })]);
    await screen.findAllByText('Check review readiness');
    await clickMaterialButton(user, container.querySelector('.next-step'), 'Check review readiness');
    await screen.findByText('Start this review');
    await user.selectOptions(screen.getByLabelText('Review model for owner/repo #15'), 'model-beta');
    expect(screen.queryByText('Start this review')).toBeNull();
    expect(screen.getByText('Confirm the selected review model before starting.')).toBeTruthy();
    await user.selectOptions(screen.getByLabelText('Review model for owner/repo #15'), 'model-alpha');
    await screen.findByText('Start this review');
    await clickMaterialButton(user, container, 'Start this review');
    expect(fetchMock.mock.calls[2][0]).toBe('/api/requests/run');
    expect(JSON.parse(fetchMock.mock.calls[2][1].body).model).toBe('model-alpha');
  });

  it('shows button help on hover and keyboard focus, including for disabled actions', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation() });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest()]);
    await screen.findAllByText('Check review readiness');
    await clickMaterialButton(user, container.querySelector('.next-step'), 'Check review readiness');
    const panel = await screen.findByRole('region', { name: 'Review preparation for owner/repo #15' });
    const button = within(panel).getByText('Run review') as MaterialControl;
    const trigger = within(panel).getByRole('group', { name: 'Run review' });
    const tooltip = document.getElementById(trigger.getAttribute('aria-describedby'));
    expect(tooltip.getAttribute('role')).toBe('tooltip');
    expect(tooltip.textContent).toBe('Confirm the selected review model before starting.');
    expect(panel.querySelector('.control-reason')).toBeNull();
    expect(trigger.tabIndex).toBe(0);
    expect(trigger.getAttribute('aria-disabled')).toBe('true');
    expect(tooltip.hasAttribute('data-test-popover-open')).toBe(false);
    await user.hover(button);
    expect(tooltip.hasAttribute('data-test-popover-open')).toBe(true);
    await act(() => window.dispatchEvent(new Event('scroll')));
    expect(tooltip.hasAttribute('data-test-popover-open')).toBe(true);
    await user.keyboard('{Escape}');
    expect(tooltip.hasAttribute('data-test-popover-open')).toBe(false);
    await user.unhover(button);
    await act(() => trigger.focus());
    expect(tooltip.hasAttribute('data-test-popover-open')).toBe(true);
    await user.keyboard('{Escape}');
    expect(tooltip.hasAttribute('data-test-popover-open')).toBe(false);
    expect(button.disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('explains that a missing publishing account blocks publication while keeping rejection available', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft({ publisher_login: null }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' }) })]);
    await screen.findAllByText('Read private draft');
    await clickMaterialButton(user, container, 'Read private draft');
    await screen.findByText(/The publishing account is unavailable/);
    expect((await clickMaterialButton(user, container, 'Publish draft')).disabled).toBe(true);
    expect((await clickMaterialButton(user, container, 'Reject draft')).disabled).toBe(false);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('connects to the request stream, shows an empty list, and closes it on unmount', async () => {
    const user = userEvent.setup();
    const { container, unmount } = render(App);
    const source = FakeEventSource.instances[0];
    const indicator = container.querySelector('.connection') as HTMLElement;
    const tooltip = document.getElementById(indicator.getAttribute('aria-describedby'));

    expect(source.url).toBe('/api/requests/events');
    expect(indicator.textContent.trim()).toBe('Connecting to local updates');
    expect(screen.getByText('Waiting for the local request list…')).toBeTruthy();
    expect(container.querySelector('md-gb-badge')).toBeNull();
    await user.hover(indicator);
    expect(tooltip.hasAttribute('data-test-popover-open')).toBe(true);
    await user.unhover(indicator);
    expect(tooltip.hasAttribute('data-test-popover-open')).toBe(false);
    await act(() => indicator.focus());
    expect(tooltip.hasAttribute('data-test-popover-open')).toBe(true);

    await act(() => source.open());
    await showSnapshot(source, []);
    expect(indicator.textContent.trim()).toBe('Local updates connected');
    expect(tooltip.textContent).toBe('Local updates connected. Refresh open PRs to check GitHub for changes.');
    await user.keyboard('{Escape}');
    expect(tooltip.hasAttribute('data-test-popover-open')).toBe(false);
    expect(screen.getByText(/No review requests recorded yet/)).toBeTruthy();
    const heading = screen.getByRole('heading', { name: 'Pull requests 0', level: 2 });
    const badge = heading.querySelector('md-gb-badge');
    await waitFor(() => expect(badge.shadowRoot.querySelector('.badge-large')).toBeTruthy());
    expect(badge.textContent).toBe('0');
    expect(screen.queryByText('Refresh from GitHub')).toBeNull();
    expect(screen.queryByText('Manage authorization')).toBeNull();
    expect(screen.queryByText('Revoke authorization')).toBeNull();
    expect(screen.queryByText('Stop managing PR')).toBeNull();

    unmount();
    expect(source.close).toHaveBeenCalledOnce();
  });

  it('signs in and refreshes a request while keeping the live snapshot authoritative', async () => {
    const user = userEvent.setup();
    let finishRefresh: FinishFetch;
    const fetchMock = vi.fn()
      .mockResolvedValueOnce({ ok: true, json: async () => ({ token: 'session-token' }) })
      .mockImplementationOnce(() => new Promise((resolve) => { finishRefresh = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request()]);

    expect(screen.queryByText('Refresh from GitHub')).toBeNull();
    await user.type(await screen.findByLabelText('One-time code'), 'test-code');
    const signIn = container.querySelector('.sign-in md-filled-button');
    await waitFor(() => expect(signIn.shadowRoot.querySelector('button')).toBeTruthy());
    await user.click(signIn.shadowRoot.querySelector('button'));
    await waitFor(() => expect(screen.getByText('Refresh from GitHub')).toBeTruthy());
    expect(fetchMock.mock.calls[0][0]).toBe('/api/session');
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({ code: 'test-code' });
    expect(sessionStorage.getItem('luthiai-session')).toBe('session-token');
    expect(screen.getByText(/Changed inputs can clear one-round authorization/)).toBeTruthy();

    const refresh = await clickMaterialButton(user, container, 'Refresh from GitHub');
    expect(screen.getByText('Refreshing…')).toBeTruthy();
    expect(refresh.disabled).toBe(true);
    expect(fetchMock.mock.calls[1][0]).toBe('/api/requests/refresh');
    expect(fetchMock.mock.calls[1][1].headers.Authorization).toBe('Bearer session-token');
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({ repository: 'owner/repo', number: 15 });
    await act(() => finishRefresh({ ok: true, json: async () => ({ changed: true, authorization_cleared: true }) }));
    expect(await screen.findByText('GitHub details updated. Inspect the changes and allow the review again; permission for one review was cleared.')).toBeTruthy();
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
    const signIn = container.querySelector('.sign-in md-filled-button');
    await user.click(signIn.shadowRoot.querySelector('button'));
    expect(await screen.findByText(/code is invalid or already used/)).toBeTruthy();
    await user.click(signIn.shadowRoot.querySelector('button'));
    await screen.findByText('Refresh from GitHub');
    await clickMaterialButton(user, container, 'Refresh from GitHub');
    expect(await screen.findByText('GitHub rate limit reached. Try again later.')).toBeTruthy();
    await clickMaterialButton(user, container, 'Refresh from GitHub');
    expect(await screen.findByText(/Your session ended/)).toBeTruthy();
    expect(sessionStorage.getItem('luthiai-session')).toBeNull();
  });

  it('restores a valid tab session and discards one from a previous server launch', async () => {
    sessionStorage.setItem('luthiai-session', 'saved-token');
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
    expect(sessionStorage.getItem('luthiai-session')).toBeNull();
  });

  it.each([
    ['one_round', 'Authorize one round', 'Authorized for one round.', 'One round'],
    ['pr_cycle', 'Authorize PR cycle', 'Authorized for the PR cycle.', 'PR cycle'],
  ])('authorizes %s for the displayed inputs and waits for the saved snapshot', async (scope, label, message, badge) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    let finishAuthorization: FinishFetch;
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
    expect((within(panel).getByRole('link', { name: 'PR on GitHub' }) as HTMLAnchorElement).href).toBe('https://github.com/owner/repo/pull/15');

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
    sessionStorage.setItem('luthiai-session', 'saved-token');
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
    sessionStorage.setItem('luthiai-session', 'saved-token');
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

  it.each(['open', 'closed'])('stops managing a %s PR and removes it when the live snapshot updates', async (state) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    let finishRemoval: FinishFetch;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockImplementationOnce(() => new Promise((resolve) => { finishRemoval = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    await showSnapshot(source, [request({ context: context({ state }) })]);
    expect(container.querySelector('.request-item').classList.contains('compact-closed')).toBe(state === 'closed');
    await screen.findByText('Stop managing PR');
    await clickMaterialButton(user, container, 'Stop managing PR');
    expect(fetchMock).toHaveBeenCalledOnce();
    expect(screen.getByText(/This cannot be undone/)).toBeTruthy();
    await clickMaterialButton(user, container, 'Confirm removal');
    expect(screen.getByText('Stopping management…')).toBeTruthy();
    expect(fetchMock.mock.calls[1][0]).toBe('/api/requests/unmanage');
    expect(fetchMock.mock.calls[1][1].headers.Authorization).toBe('Bearer saved-token');
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({ repository: 'owner/repo', number: 15 });
    await clickMaterialButton(user, container, 'Confirm removal');
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await act(() => finishRemoval({ ok: true, json: async () => ({ changed: true }) }));
    expect(await screen.findByText(/Management stopped/)).toBeTruthy();
    expect(screen.getByRole('link', { name: 'Fix login redirect' })).toBeTruthy();
    await showSnapshot(source, []);
    expect(screen.queryByRole('link', { name: 'Fix login redirect' })).toBeNull();
    expect(screen.getByText(/No review requests recorded yet/)).toBeTruthy();
  });

  it.each([
    { run: { status: 'starting' } },
    { run: { status: 'running' } },
    { publication: { status: 'active' } },
    { latest_round: round({ status: 'starting' }) },
    { latest_round: round({ status: 'running' }) },
    { latest_round: round({ status: 'awaiting_decision' }) },
    { latest_round: round({ status: 'publication_unknown', cleanup_pending: false }) },
    { latest_round: round({ status: 'publishing', cleanup_pending: false }) },
  ])('disables management removal while work needs attention: %o', async (overrides) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request(overrides)]);
    await screen.findByText('Stop managing PR');
    const button = await clickMaterialButton(user, container, 'Stop managing PR');
    expect(button.disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledOnce();
  });

  it.each(['open', 'closed'])('cancels removal of a %s PR without sending a deletion request', async (state) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ context: context({ state }) })]);
    await screen.findByText('Stop managing PR');
    await clickMaterialButton(user, container, 'Stop managing PR');
    expect(screen.getByText(/Permanently remove owner\/repo #15/)).toBeTruthy();
    await clickMaterialButton(user, container, 'Cancel removal');
    expect(screen.queryByText('Confirm removal')).toBeNull();
    expect(fetchMock).toHaveBeenCalledOnce();
    expect(screen.getByRole('link', { name: 'Fix login redirect' })).toBeTruthy();
  });

  it('keeps the PR visible and reports a management removal conflict', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: false, status: 409, json: async () => ({ error: 'Finish cleanup before stopping management.' }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request()]);
    await screen.findByText('Stop managing PR');
    await clickMaterialButton(user, container, 'Stop managing PR');
    await clickMaterialButton(user, container, 'Confirm removal');
    expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'Finish cleanup before stopping management.');
    expect(screen.getByText('owner/repo')).toBeTruthy();
  });

  it('reports authorization conflicts and connection errors and clears an expired session', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
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
    expect(sessionStorage.getItem('luthiai-session')).toBeNull();
    expect(screen.queryByText('Manage authorization')).toBeNull();
    expect(screen.queryByRole('region', { name: 'Authorize owner/repo #15' })).toBeNull();
  });

  it('prepares a review and validates a different model for the inspected inputs and rules', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    let finishPreparation: FinishFetch;
    let finishValidation: FinishFetch;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockImplementationOnce(() => new Promise((resolve) => { finishPreparation = resolve; }))
      .mockImplementationOnce(() => new Promise((resolve) => { finishValidation = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    await showSnapshot(source, [preparedRequest()]);
    await screen.findAllByText('Check review readiness');
    const prepare = await clickMaterialButton(user, container, 'Check review readiness');
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
    const model = within(panel).getByLabelText('Review model for owner/repo #15') as HTMLSelectElement;
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
    ['discussion_not_observed', 'green', 'author discussion has not been observed'],
    ['pull_closed', 'green', 'the PR is closed'],
  ])('shows %s as a blocker and disables model and scope selection', async (readiness, checks, readiness_message) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation({ readiness, checks, readiness_message }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest()]);
    await screen.findAllByText('Check review readiness');
    await clickMaterialButton(user, container, 'Check review readiness');
    expect(await screen.findByText(readiness_message)).toBeTruthy();
    expect((screen.getByLabelText('Review model for owner/repo #15') as HTMLSelectElement).disabled).toBe(true);
    const scope = screen.getByLabelText('Review scope for owner/repo #15') as HTMLSelectElement;
    expect(scope.disabled).toBe(true);
    await user.selectOptions(scope, 'full');
    expect(scope.value).toBe('auto');
    const button = await clickMaterialButton(user, container, 'Validate model');
    expect(button.disabled).toBe(true);
    expect((await clickMaterialButton(user, container, 'Run review')).disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it.each([
    ['checks_pending', 'pending'], ['checks_failing', 'failing'],
    ['checks_unknown', 'unknown'], ['no_checks', 'no_results'],
  ])('refreshes %s from the visible preparation panel without reopening the row menu', async (readiness, checks) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    let finishRefresh: FinishFetch;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation({ readiness, checks }) })
      .mockImplementationOnce(() => new Promise((resolve) => { finishRefresh = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest()]);
    await screen.findAllByText('Check review readiness');
    await clickMaterialButton(user, container.querySelector('.next-step'), 'Check review readiness');
    const panel = await screen.findByRole('region', { name: 'Review preparation for owner/repo #15' });
    const originalChecks = within(panel).getByText('Checks for this head').nextElementSibling.textContent;
    expect((screen.getByRole('link', { name: 'View checks on GitHub' }) as HTMLAnchorElement).href).toBe(`https://github.com/owner/repo/commit/${'a'.repeat(40)}/checks`);
    expect(container.querySelector('md-menu').open).toBe(false);
    const refresh = await clickMaterialButton(user, panel, 'Refresh readiness');
    expect(refresh.closest('.authorization-buttons')).toBeTruthy();
    expect(refresh.disabled).toBe(true);
    expect(within(panel).getByText('Refreshing readiness…')).toBeTruthy();
    expect(within(panel).getByText('Checks for this head').nextElementSibling.textContent).toBe(originalChecks);
    expect((within(panel).getByLabelText('Review model for owner/repo #15') as HTMLSelectElement).disabled).toBe(true);
    expect((within(panel).getByLabelText('Review scope for owner/repo #15') as HTMLSelectElement).disabled).toBe(true);
    refresh.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(fetchMock.mock.calls[2][0]).toBe('/api/requests/prepare');
    expect(fetchMock.mock.calls[2][1].headers.Authorization).toBe('Bearer saved-token');
    expect(JSON.parse(fetchMock.mock.calls[2][1].body)).toEqual({ repository: 'owner/repo', number: 15 });
    await act(() => finishRefresh({ ok: true, json: async () => preparation() }));
    await waitFor(() => expect(within(panel).getByText('Checks for this head').nextElementSibling.textContent).toBe('Green'));
    expect(screen.getByRole('region', { name: 'Review preparation for owner/repo #15' })).toBe(panel);
    expect((within(panel).getByLabelText('Review model for owner/repo #15') as HTMLSelectElement).disabled).toBe(false);
    expect((within(panel).getByLabelText('Review scope for owner/repo #15') as HTMLSelectElement).disabled).toBe(false);
    expect((within(panel).getByText('Validate model') as MaterialControl).disabled).toBe(false);
    expect((within(panel).getByText('Run review') as MaterialControl).disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it.each(['github', 'offline'])('keeps failed %s readiness refreshes visible and requires a successful retry before running', async (failure) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const ready = preparation({ readiness: 'ready', validated_model: 'model-alpha' });
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => ready });
    if (failure === 'github') fetchMock.mockResolvedValueOnce({ ok: false, status: 502, json: async () => ({ error: 'GitHub is unavailable. Try again later.' }) });
    else fetchMock.mockRejectedValueOnce(new Error('offline'));
    fetchMock.mockResolvedValueOnce({ ok: true, json: async () => ready });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest({ validated_model: { model: 'model-alpha', agent: 'claude' } })]);
    await screen.findAllByText('Check review readiness');
    await clickMaterialButton(user, container.querySelector('.next-step'), 'Check review readiness');
    const panel = await screen.findByRole('region', { name: 'Review preparation for owner/repo #15' });
    expect((within(panel).getByText('Run review') as MaterialControl).disabled).toBe(false);
    await clickMaterialButton(user, panel, 'Refresh readiness');
    await screen.findByText(failure === 'github' ? 'GitHub is unavailable. Try again later.' : 'Could not connect to the local API. Try again.');
    expect(within(panel).getByText('a'.repeat(40))).toBeTruthy();
    expect(within(panel).getByText(/This preparation is outdated/)).toBeTruthy();
    expect((await clickMaterialButton(user, panel, 'Run review')).disabled).toBe(true);
    expect((await clickMaterialButton(user, panel, 'Validate model')).disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(3);
    await clickMaterialButton(user, panel, 'Refresh readiness');
    await waitFor(() => expect(within(panel).queryByText(/This preparation is outdated/)).toBeNull());
    expect((within(panel).getByText('Run review') as MaterialControl).disabled).toBe(false);
    expect(fetchMock).toHaveBeenCalledTimes(4);
    expect(fetchMock.mock.calls.slice(1).every(([url]) => url === '/api/requests/prepare')).toBe(true);
  });

  it.each([
    { head_sha: 'b'.repeat(40), revision: 3 }, { revision: 3 }, { authorization: 'pr_cycle' },
  ])('keeps preparation fixed and prevents validation after %o', async (change) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation() })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation(change) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    await showSnapshot(source, [preparedRequest()]);
    await screen.findAllByText('Check review readiness');
    await clickMaterialButton(user, container, 'Check review readiness');
    await screen.findByText('GitHub checks and review preparation updated.');
    await showSnapshot(source, [preparedRequest(change)]);
    const panel = screen.getByRole('region', { name: 'Review preparation for owner/repo #15' });
    expect(within(panel).getByText('a'.repeat(40))).toBeTruthy();
    expect(screen.getByRole('alert').textContent).toContain('This preparation is outdated');
    const validate = await clickMaterialButton(user, container, 'Validate model');
    expect(validate.disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await clickMaterialButton(user, container, 'Check review readiness');
    await screen.findByText('GitHub checks and review preparation updated.');
    expect(screen.queryByRole('alert')).toBeNull();
    expect((screen.getByLabelText('Review model for owner/repo #15') as HTMLSelectElement).disabled).toBe(false);
  });

  it('requires new preparation after a model validation conflict', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation() })
      .mockResolvedValueOnce({ ok: false, status: 409, json: async () => ({ error: 'Configuration changed. Prepare again.' }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest()]);
    await screen.findAllByText('Check review readiness');
    await clickMaterialButton(user, container, 'Check review readiness');
    await screen.findByText('GitHub checks and review preparation updated.');
    await clickMaterialButton(user, container, 'Validate model');
    expect(await screen.findByText('Configuration changed. Prepare again.')).toBeTruthy();
    expect(screen.getByText(/This preparation is outdated/)).toBeTruthy();
    const validate = await clickMaterialButton(user, container, 'Validate model');
    expect(validate.disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it('uses the validated model as the default and explains a co-author hint match', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation({
        readiness: 'ready', readiness_message: 'ready to run', resolved_model: 'model-beta',
        validated_model: 'model-beta', proposal_matches_coauthor: true,
      }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest()]);
    await screen.findAllByText('Check review readiness');
    await clickMaterialButton(user, container, 'Check review readiness');
    expect(await screen.findByText(/The proposed model matches a PR co-author hint/)).toBeTruthy();
    const model = screen.getByLabelText('Review model for owner/repo #15') as HTMLSelectElement;
    expect(model.value).toBe('model-beta');
    const validated = await clickMaterialButton(user, container, 'Model validated');
    expect(validated.disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await user.selectOptions(model, 'model-alpha');
    expect((screen.getByText('Validate model') as MaterialControl).disabled).toBe(false);
    expect(screen.queryByText(/This model is validated for the prepared head and revision/)).toBeNull();
    await user.selectOptions(model, 'model-beta');
    expect((screen.getByText('Model validated') as MaterialControl).disabled).toBe(true);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest({ revision: 3 })]);
    expect((screen.getByText('Validate model') as MaterialControl).disabled).toBe(true);
    expect(screen.getByRole('alert').textContent).toContain('This preparation is outdated');
  });

  it('reports preparation errors and clears an expired session', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: false, status: 422, json: async () => ({ error: 'Check review configuration.' }) })
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce({ ok: false, status: 401 }));
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest()]);
    await screen.findAllByText('Check review readiness');
    await clickMaterialButton(user, container, 'Check review readiness');
    expect(await screen.findByText('Check review configuration.')).toBeTruthy();
    expect(screen.queryByRole('region', { name: 'Review preparation for owner/repo #15' })).toBeNull();
    await clickMaterialButton(user, container, 'Check review readiness');
    expect(await screen.findByText('Could not connect to the local API. Try again.')).toBeTruthy();
    await clickMaterialButton(user, container, 'Check review readiness');
    expect(await screen.findByText(/Your session ended/)).toBeTruthy();
    expect(screen.queryByText('Check review readiness')).toBeNull();
    expect(sessionStorage.getItem('luthiai-session')).toBeNull();
  });

  it.each(['auto', 'full'])('runs the validated selection with %s scope for frozen inputs and prevents duplicate clicks', async (reviewMode) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    let acceptRun: FinishFetch;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation({
        readiness: 'ready', readiness_message: 'ready to run', validated_model: 'model-beta', resolved_model: 'model-beta',
      }) })
      .mockImplementationOnce(() => new Promise((resolve) => { acceptRun = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest({ validated_model: { model: 'model-beta', agent: 'claude-alt' } })]);
    await screen.findAllByText('Check review readiness');
    await clickMaterialButton(user, container, 'Check review readiness');
    await screen.findByText('ready to run');
    const model = screen.getByLabelText('Review model for owner/repo #15') as HTMLSelectElement;
    await user.selectOptions(model, 'model-alpha');
    expect((await clickMaterialButton(user, container, 'Run review')).disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await user.selectOptions(model, 'model-beta');
    const scope = screen.getByLabelText('Review scope for owner/repo #15') as HTMLSelectElement;
    expect(scope.value).toBe('auto');
    await user.selectOptions(scope, reviewMode);
    const run = await clickMaterialButton(user, container, 'Run review');
    expect(run.disabled).toBe(true);
    expect(model.disabled).toBe(true);
    expect(scope.disabled).toBe(true);
    expect(fetchMock.mock.calls[2][0]).toBe('/api/requests/run');
    expect(fetchMock.mock.calls[2][1].headers.Authorization).toBe('Bearer saved-token');
    expect(JSON.parse(fetchMock.mock.calls[2][1].body)).toEqual({
      key: { repository: 'owner/repo', number: 15 }, model: 'model-beta', head_sha: 'a'.repeat(40), revision: 2,
      review_mode: reviewMode,
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
    { publication: { status: 'active' } },
    { latest_round: round() },
    { latest_round: round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' }) },
    { latest_round: round({ status: 'failed', cleanup_status: 'needs_attention' }) },
  ])('disables running after live state changes to %o', async (change) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation({
        readiness: 'ready', readiness_message: 'ready to run', validated_model: 'model-alpha',
      }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    const original = preparedRequest({ validated_model: { model: 'model-alpha', agent: 'claude' } });
    await showSnapshot(source, [original]);
    await screen.findAllByText('Check review readiness');
    await clickMaterialButton(user, container, 'Check review readiness');
    await screen.findByText('ready to run');
    expect((screen.getByText('Run review') as MaterialControl).disabled).toBe(false);
    await showSnapshot(source, [{ ...original, ...change }]);
    expect((await clickMaterialButton(user, container, 'Run review')).disabled).toBe(true);
    if (change.publication?.status === 'active') {
      expect(screen.getByText('Wait for publication to finish before starting another review.')).toBeTruthy();
    }
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('requires a new preparation after a rejected run request', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => preparation({
        readiness: 'ready', readiness_message: 'ready to run', validated_model: 'model-alpha',
      }) })
      .mockResolvedValueOnce({ ok: false, status: 409, json: async () => ({ error: 'A review is already starting or running for this PR.' }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [preparedRequest({ validated_model: { model: 'model-alpha', agent: 'claude' } })]);
    await screen.findAllByText('Check review readiness');
    await clickMaterialButton(user, container, 'Check review readiness');
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
    expect(screen.getByText(/luthiai show-draft owner\/repo 15 --round r-example/)).toBeTruthy();
    expect(screen.getByText(/Draft freshness is stale/)).toBeTruthy();
    await showSnapshot(source, [request({ latest_round: round({ status: 'awaiting_decision', cleanup_status: 'needs_attention' }),
      run: { status: 'failed', round_id: 'r-example', failure: 'execution' } })]);
    expect(screen.getByRole('alert').textContent).toContain('The private draft was saved');
    expect(screen.getByText(/luthiai recover-review owner\/repo 15/)).toBeTruthy();
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
    sessionStorage.setItem('luthiai-session', 'saved-token');
    let finishDraft: FinishFetch;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockImplementationOnce(() => new Promise((resolve) => { finishDraft = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status: 'awaiting_decision' }) })]);
    await screen.findAllByText('Read private draft');
    expect(screen.queryByText(draft().lead_advice)).toBeNull();
    const view = await clickMaterialButton(user, container, 'Read private draft');
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
    expect(within(panel).getByRole('region', { name: 'Review comments' }).querySelector('script, img, a')).toBeNull();
    expect(within(panel).getByRole('region', { name: 'Private lead advice' }).querySelector('script, img, a')).toBeNull();
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
    sessionStorage.setItem('luthiai-session', 'saved-token');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft({ freshness, lead_advice: '' }) }));
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status: 'awaiting_decision' }) })]);
    await screen.findAllByText('Read private draft');
    await clickMaterialButton(user, container, 'Read private draft');
    const panel = await screen.findByRole('region', { name: 'Private draft for owner/repo #15' });
    expect(within(panel).getByText(message)).toBeTruthy();
    expect(within(panel).getByText('No private lead advice.')).toBeTruthy();
    expect(within(panel).getByText(/Reopen it to check again/)).toBeTruthy();
  });

  it('keeps the requested round fixed during loading and live updates until explicitly reopened', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    let finishDraft: FinishFetch;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockImplementationOnce(() => new Promise((resolve) => { finishDraft = resolve; }))
      .mockResolvedValueOnce({ ok: true, json: async () => draft({ round_id: 'r-new', head_sha: 'b'.repeat(40), revision: 2, publication_body: 'New draft' }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    await showSnapshot(source, [request({ latest_round: round({ status: 'awaiting_decision' }) })]);
    await screen.findAllByText('Read private draft');
    await clickMaterialButton(user, container, 'Read private draft');
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
    await clickMaterialButton(user, container, 'Read private draft');
    await waitFor(() => expect(screen.getByRole('region', { name: 'Review comments' }).textContent).toBe('New draft'));
    expect(fetchMock.mock.calls[2][0]).toContain('/api/drafts/r-new?');
    expect(screen.queryByText(/This view keeps the draft you opened/)).toBeNull();
  });

  it.each(['stale', 'unknown'])('confirms the displayed draft with %s freshness and waits for SSE after rejection', async (freshness) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    let finishReject: FinishFetch;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft({ freshness }) })
      .mockImplementationOnce(() => new Promise((resolve) => { finishReject = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    const readyRound = round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' });
    await showSnapshot(source, [request({ authorization: 'pr_cycle', latest_round: readyRound })]);
    await screen.findAllByText('Read private draft');
    await clickMaterialButton(user, container, 'Read private draft');
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
    expect(screen.queryByText(/The pending decision is no longer current/)).toBeNull();
    expect(screen.queryByText('Reject draft')).toBeNull();
    expect(screen.getByText(draft().lead_advice)).toBeTruthy();
    await showSnapshot(source, [request({ authorization: 'pr_cycle', head_sha: 'b'.repeat(40), revision: 2,
      latest_round: { ...readyRound, status: 'rejected' } })]);
    expect(screen.getAllByText('Rejected').length).toBeGreaterThan(0);
    expect(screen.getByText('PR cycle')).toBeTruthy();
    expect(screen.queryByText('Draft ready')).toBeNull();
  });

  it('confirms the inspected publication hash, starts once, and shows the receipt from SSE', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    let finishPublication: FinishFetch;
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft() })
      .mockImplementationOnce(() => new Promise((resolve) => { finishPublication = resolve; }));
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    const readyRound = round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' });
    await showSnapshot(source, [request({ latest_round: readyRound })]);
    await screen.findAllByText('Read private draft');
    await clickMaterialButton(user, container, 'Read private draft');
    await screen.findByText(draft().lead_advice);
    await clickMaterialButton(user, container, 'Publish draft');
    const confirmation = screen.getByRole('region', { name: 'Confirm draft publication' });
    expect(within(confirmation).getByText('r-example')).toBeTruthy();
    expect(within(confirmation).getByText('d'.repeat(64))).toBeTruthy();
    expect(within(confirmation).getByText(/Private lead advice stays private/)).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledTimes(2);
    await clickMaterialButton(user, container, 'Cancel publication');
    expect(screen.queryByRole('region', { name: 'Confirm draft publication' })).toBeNull();
    await clickMaterialButton(user, container, 'Publish draft');
    const confirm = await clickMaterialButton(user, container, 'Confirm publication');
    expect(confirm.disabled).toBe(true);
    expect(fetchMock.mock.calls[2][0]).toBe('/api/drafts/r-example/publish');
    expect(fetchMock.mock.calls[2][1].headers.Authorization).toBe('Bearer saved-token');
    expect(JSON.parse(fetchMock.mock.calls[2][1].body)).toEqual({ key: draft().key, body_hash: draft().publication_body_sha256, publisher_login: draft().publisher_login });
    await clickMaterialButton(user, container, 'Starting publication…');
    expect(fetchMock).toHaveBeenCalledTimes(3);
    await act(() => finishPublication({ ok: true, status: 202, json: async () => ({ accepted: true }) }));
    await screen.findByText(/Publication accepted/);
    expect(screen.getAllByText('Draft ready').length).toBeGreaterThan(0);
    expect(screen.queryByText('Publish draft')).toBeNull();
    expect(screen.getByText(draft().lead_advice)).toBeTruthy();
    await showSnapshot(source, [request({ latest_round: { ...readyRound, status: 'publishing' },
      publication: { round_id: 'r-example', status: 'active' } })]);
    expect(screen.getByText(/Publishing draft…/)).toBeTruthy();
    expect(screen.queryByText(/Publication needs recovery/)).toBeNull();
    await showSnapshot(source, [request({ latest_round: { ...readyRound, status: 'published', receipt: {
      review_id: 71, url: 'https://github.com/owner/repo/pull/15#pullrequestreview-71', publisher_login: 'publisher', commit_id: 'a'.repeat(40),
    } }, publication: { round_id: 'r-example', status: 'published', freshness: 'stale' } })]);
    const link = screen.getByRole('link', { name: 'GitHub review 71' });
    expect(link.getAttribute('href')).toBe('https://github.com/owner/repo/pull/15#pullrequestreview-71');
    const receiptNotes = [...container.querySelectorAll('.action-note')].filter((note) => note.textContent.includes('@publisher'));
    expect(receiptNotes).toHaveLength(1);
    expect(receiptNotes[0].textContent).toContain('tied to commit');
    expect(screen.getByText(/PR inputs changed after publication/)).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it.each(['stale', 'unknown'])('blocks publication for %s freshness while leaving rejection available', async (freshness) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft({ freshness }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' }) })]);
    await screen.findAllByText('Read private draft');
    await clickMaterialButton(user, container, 'Read private draft');
    await screen.findByText(draft().lead_advice);
    expect((await clickMaterialButton(user, container, 'Publish draft')).disabled).toBe(true);
    expect((await clickMaterialButton(user, container, 'Reject draft')).disabled).toBe(false);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it.each([
    { head_sha: 'b'.repeat(40) }, { revision: 2 },
    { latest_round: round({ id: 'r-new', status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' }) },
  ])('invalidates publication confirmation across live input changes: %j', async (change) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft() });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    const initial = request({ latest_round: round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' }) });
    await showSnapshot(source, [initial]);
    await screen.findAllByText('Read private draft');
    await clickMaterialButton(user, container, 'Read private draft');
    await screen.findByText(draft().lead_advice);
    await clickMaterialButton(user, container, 'Publish draft');
    await showSnapshot(source, [{ ...initial, ...change }]);
    expect((await clickMaterialButton(user, container, 'Confirm publication')).disabled).toBe(true);
    expect(screen.getByRole('region', { name: 'Confirm draft publication' }).textContent).toContain('r-example');
    await showSnapshot(source, [initial]);
    expect((await clickMaterialButton(user, container, 'Confirm publication')).disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it.each(['publishing', 'publication_unknown'])('shows recovery guidance for persisted %s without posting or allowing another decision', async (status) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft({ decision: status }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status, cleanup_pending: false, cleanup_status: 'complete' }) })]);
    await screen.findAllByText('Read private draft');
    expect(screen.getByText(/Publication needs recovery/)).toBeTruthy();
    expect(screen.getByText(/luthiai recover-publication owner\/repo 15 --round r-example/)).toBeTruthy();
    await clickMaterialButton(user, container, 'Read private draft');
    await screen.findByText(draft().lead_advice);
    expect(screen.queryByText('Publish draft')).toBeNull();
    expect(screen.queryByText('Reject draft')).toBeNull();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it.each(['active', 'failed', 'unknown', 'published'])('displays publication job %s from live updates', async (status) => {
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    await showSnapshot(source, [request({ latest_round: round({ status: status === 'unknown' ? 'publication_unknown' : 'awaiting_decision' }),
      publication: { round_id: 'r-example', status, error: status === 'failed' || status === 'unknown' ? 'Safe publication failure.' : null,
        freshness: status === 'published' ? 'unknown' : null } })]);
    if (status === 'active') expect(screen.getByText(/Publishing draft…/)).toBeTruthy();
    else if (status === 'published') expect(screen.getByText(/saved receipt confirms publication/)).toBeTruthy();
    else expect(screen.getByText('Safe publication failure.')).toBeTruthy();
    expect(container.querySelector('script')).toBeNull();
    await showSnapshot(source, [request({ latest_round: round({ id: 'r-new' }),
      publication: { round_id: 'r-example', status, error: 'Safe publication failure.' } })]);
    expect(screen.queryByText('Safe publication failure.')).toBeNull();
  });

  it.each([
    [true, 'in_progress'], [true, 'needs_attention'], [true, 'unknown'], [false, 'unknown'],
  ])('gates rejection until cleanup is complete (%s, %s)', async (cleanup_pending, cleanup_status) => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft() });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    await showSnapshot(source, [request({ latest_round: round({ status: 'awaiting_decision', cleanup_pending, cleanup_status }) })]);
    await screen.findAllByText('Read private draft');
    await clickMaterialButton(user, container, 'Read private draft');
    await screen.findByText(draft().lead_advice);
    expect((await clickMaterialButton(user, container, 'Publish draft')).disabled).toBe(true);
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
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft() });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    const source = FakeEventSource.instances[0];
    const readyRound = round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' });
    await showSnapshot(source, [request({ latest_round: readyRound })]);
    await screen.findAllByText('Read private draft');
    await clickMaterialButton(user, container, 'Read private draft');
    await screen.findByText(draft().lead_advice);
    await clickMaterialButton(user, container, 'Reject draft');
    await showSnapshot(source, [request({ latest_round: { ...readyRound, ...change } })]);
    expect(screen.getByText(/The pending decision is no longer current/)).toBeTruthy();
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
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft() });
    if (result === 'offline') fetchMock.mockRejectedValueOnce(new Error('offline'));
    else if (result === 'already_rejected') fetchMock.mockResolvedValueOnce({ ok: true, json: async () => ({ changed: false }) });
    else fetchMock.mockResolvedValueOnce({ ok: false, status: result, json: async () => ({ error: 'Draft decision changed. Reopen it.' }) });
    vi.stubGlobal('fetch', fetchMock);
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status: 'awaiting_decision', cleanup_pending: false, cleanup_status: 'complete' }) })]);
    await screen.findAllByText('Read private draft');
    await clickMaterialButton(user, container, 'Read private draft');
    await screen.findByText(draft().lead_advice);
    await clickMaterialButton(user, container, 'Reject draft');
    await clickMaterialButton(user, container, 'Confirm rejection');
    const message = result === 401 ? /Your session ended/ : result === 'offline' ? /Could not connect to the local API/
      : result === 'already_rejected' ? /This draft was already rejected/ : /Draft decision changed/;
    await screen.findByText(message);
    if (result === 401) {
      expect(screen.queryByText(draft().lead_advice)).toBeNull();
      expect(screen.queryByRole('region', { name: 'Confirm draft rejection' })).toBeNull();
      expect(sessionStorage.getItem('luthiai-session')).toBeNull();
    } else {
      expect(screen.getByText(draft().lead_advice)).toBeTruthy();
      if (result !== 'already_rejected') expect(screen.getByText('Confirm rejection')).toBeTruthy();
    }
  });

  it('removes old draft contents before reporting integrity or connection failures', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft() })
      .mockResolvedValueOnce({ ok: false, status: 422, json: async () => ({ error: 'Could not verify the private draft.' }) })
      .mockRejectedValueOnce(new Error('offline')));
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status: 'awaiting_decision' }) })]);
    await screen.findAllByText('Read private draft');
    await clickMaterialButton(user, container, 'Read private draft');
    await screen.findByText(draft().lead_advice);
    await clickMaterialButton(user, container, 'Read private draft');
    expect(await screen.findByText('Could not verify the private draft.')).toBeTruthy();
    expect(screen.queryByText(draft().lead_advice)).toBeNull();
    expect(screen.queryByRole('region', { name: 'Private draft for owner/repo #15' })).toBeNull();
    await clickMaterialButton(user, container, 'Read private draft');
    expect(await screen.findByText('Could not connect to the local API. Try again.')).toBeTruthy();
  });

  it('clears private contents and draft controls when the lead session expires', async () => {
    const user = userEvent.setup();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce({ ok: true })
      .mockResolvedValueOnce({ ok: true, json: async () => draft() })
      .mockResolvedValueOnce({ ok: false, status: 401 }));
    const { container } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status: 'awaiting_decision' }) })]);
    await screen.findAllByText('Read private draft');
    await clickMaterialButton(user, container, 'Read private draft');
    await screen.findByText(draft().lead_advice);
    await clickMaterialButton(user, container, 'Refresh from GitHub');
    await screen.findByText(/Your session ended/);
    expect(screen.queryByRole('region', { name: 'Private draft for owner/repo #15' })).toBeNull();
    expect(screen.queryByText(draft().lead_advice)).toBeNull();
    expect(screen.queryByText('Read private draft')).toBeNull();
    expect(sessionStorage.getItem('luthiai-session')).toBeNull();
  });

  it('offers draft inspection only to signed-in leads and only for completed drafts', async () => {
    const { unmount } = render(App);
    await showSnapshot(FakeEventSource.instances[0], [request({ latest_round: round({ status: 'awaiting_decision' }) })]);
    expect(screen.queryByText('Read private draft')).toBeNull();
    unmount();
    sessionStorage.setItem('luthiai-session', 'saved-token');
    const fetchMock = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal('fetch', fetchMock);
    render(App);
    const source = FakeEventSource.instances[1];
    for (const status of ['awaiting_decision', 'publishing', 'publication_unknown', 'published', 'rejected']) {
      await showSnapshot(source, [request({ latest_round: round({ status }) })]);
      expect(await screen.findAllByText('Read private draft')).toBeTruthy();
    }
    for (const status of ['starting', 'running', 'failed', 'interrupted']) {
      await showSnapshot(source, [request({ latest_round: round({ status }) })]);
      expect(screen.queryByText('Read private draft')).toBeNull();
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

    const detailsControl = await clickMaterialButton(user, container, 'Show details');
    await waitFor(() => expect(detailsControl.ariaExpanded).toBe('true'));
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

    await clickMaterialButton(user, container, 'Hide details');
    await waitFor(() => expect(detailsControl.ariaExpanded).toBe('false'));
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
