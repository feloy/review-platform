import { act, render, screen } from '@testing-library/svelte';
import { afterEach, expect, it, vi } from 'vitest';
import TimeAgo from './TimeAgo.svelte';

afterEach(() => vi.useRealTimers());

it('updates elapsed time while retaining the exact observation timestamp', async () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date('2026-11-02T12:03:00Z'));
  render(TimeAgo, { value: Date.parse('2026-11-02T12:00:00Z') / 1000 });
  const time = screen.getByText('3 minutes ago');
  expect(time.getAttribute('datetime')).toBe('2026-11-02T12:00:00.000Z');
  expect(time.title).toBeTruthy();
  await act(() => vi.advanceTimersByTime(60_000));
  expect(screen.getByText('4 minutes ago')).toBeTruthy();
});

it('accepts GitHub publication timestamps', () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date('2026-11-02T12:00:00Z'));
  render(TimeAgo, { value: '2026-11-01T12:00:00Z' });
  expect(screen.getByText('1 day ago')).toBeTruthy();
});

it.each([undefined, null, 'invalid'])('does not invent a check time for %s', (value) => {
  const { container } = render(TimeAgo, { value });
  expect(screen.getByText('time unavailable')).toBeTruthy();
  expect(container.querySelector('time')).toBeNull();
});
