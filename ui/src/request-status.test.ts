import { describe, expect, it } from 'vitest';
import { reviewBlocker, reviewFailure, reviewSituation } from './request-status.js';

const request = {
  head_sha: 'a'.repeat(40), revision: 2, authorization: 'one_round', discussion: {},
  validated_model: { model: 'alpha', agent: 'runner' },
};
const preparation = {
  head_sha: request.head_sha, revision: 2, authorization: 'one_round', readiness: 'ready',
  validated_model: 'alpha', resolved_model: 'alpha', models: [{ model: 'alpha', agent: 'runner' }],
};

describe('review next steps', () => {
  it('keeps the running review primary after one-round permission is consumed', () => {
    const situation = reviewSituation({ ...request, authorization: 'awaiting_lead', latest_round: { status: 'running' } });
    expect(situation.title).toBe('Reviewing…');
    expect(situation.action).toBe('progress');
  });

  it('allows reading a saved draft while its runner finishes cleanup', () => {
    const situation = reviewSituation({ ...request, authorization: 'awaiting_lead',
      run: { status: 'running' }, latest_round: { status: 'awaiting_decision', cleanup_pending: true } });
    expect(situation.action).toBe('draft');
    expect(situation.message).toContain('when cleanup finishes');
  });

  it.each(['publishing', 'publication_unknown'])('requires recovery for a saved %s outcome without an active publisher', (status) => {
    const situation = reviewSituation({ ...request, latest_round: { status } });
    expect(situation.message).toContain('Check GitHub');
    expect(situation.action).toBe('progress');
  });

  it.each([
    { publication: { round_id: 'r-current', status: 'failed', error: 'Safe publication failure.' }, title: 'Publication failed' },
    { run: { round_id: 'r-current', status: 'failed', failure: 'execution' }, title: 'Draft saved with warning' },
  ])('puts $title ahead of a saved draft and cached preparation', ({ title, ...failure }) => {
    const failed = { ...request, ...failure, latest_round: { id: 'r-current', status: 'awaiting_decision' } };
    for (const cached of [undefined, preparation]) {
      const situation = reviewSituation(failed, cached);
      expect(situation.title).toBe(title);
      expect(situation.action).toBe('progress');
      expect(situation.label).toBe('View issue');
      expect(situation.tone).toBe('warning');
    }
  });

  it('keeps review failures visible until a fresh preparation replaces the failed attempt', () => {
    const failed = { ...request, run: { status: 'failed', failure: 'stale_preparation' } };
    expect(reviewSituation(failed, preparation, true).title).toBe('Review failed');
    expect(reviewSituation(failed, preparation).title).toBe('Ready');
  });

  it.each(['failed', 'interrupted'])('exposes a persisted %s round without a GUI job', (status) => {
    const failed = { ...request, latest_round: { id: 'r-current', status, failure: 'timeout' } };
    expect(reviewSituation(failed).title).toBe('Review failed');
    expect(reviewSituation(failed).label).toBe('View issue');
    expect(reviewFailure(failed).messages).toEqual([expect.stringContaining('Agent timed out')]);
  });

  it.each(['awaiting_decision', 'running', 'published', 'rejected'])('ignores failures from an older round while the current round is %s', (status) => {
    const current = { ...request, latest_round: { id: 'r-current', status },
      publication: { round_id: 'r-old', status: 'failed', error: 'Old publication error' },
      run: { round_id: 'r-old', status: 'failed', failure: 'execution' } };
    expect(reviewFailure(current)).toBeNull();
    expect(reviewSituation(current).label).not.toBe('View issue');
  });

  it.each(['published', 'rejected'])('clears warnings when a failed draft operation is resolved as %s', (status) => {
    expect(reviewFailure({ ...request, latest_round: { id: 'r-current', status },
      publication: { round_id: 'r-current', status: 'failed', error: 'Old publication error' },
      run: { round_id: 'r-current', status: 'failed', failure: 'execution' } })).toBeNull();
  });

  it('still exposes a later startup failure after an older draft was rejected', () => {
    const failed = { ...request, latest_round: { id: 'r-old', status: 'rejected' },
      run: { status: 'failed', failure: 'stale_preparation' } };
    expect(reviewSituation(failed).title).toBe('Review failed');
    expect(reviewFailure(failed).messages).toEqual([expect.stringContaining('Prepare and inspect the review again')]);
  });

  it('does not propose starting until fresh preparation and the selected model match saved validation', () => {
    expect(reviewSituation(request).action).toBe('prepare');
    expect(reviewSituation(request, preparation).action).toBe('run');
    expect(reviewSituation(request, preparation, false, 'beta').action).toBe('model');
    expect(reviewSituation({ ...request, revision: 3 }, preparation).action).toBe('prepare');
    expect(reviewSituation(request, preparation, true).action).toBe('prepare');
    expect(reviewSituation({ ...request, validated_model: null }, preparation).action).toBe('prepare');
    expect(reviewSituation({ ...request, validated_model: { model: 'alpha', agent: 'other' } }, preparation).action).toBe('prepare');
    expect(reviewSituation({ ...request, validated_model: { model: 'beta', agent: 'runner' } }, preparation).action).toBe('prepare');
  });

  it.each(['checks_pending', 'checks_failing', 'checks_unknown', 'no_checks'])('offers rechecking or inspection instead of starting for %s', (readiness) => {
    const situation = reviewSituation(request, { ...preparation, readiness });
    expect(situation.action).toBe(readiness === 'checks_pending' ? 'prepare' : 'checks');
    expect(situation.secondaryAction).toBe(readiness === 'checks_pending' ? 'checks' : null);
  });

  it('does not mistake an earlier publication for a review of newer discussion', () => {
    const latest_round = { status: 'published', head_sha: request.head_sha, revision: 1 };
    expect(reviewSituation({ ...request, latest_round }).action).toBe('prepare');
    expect(reviewSituation({ ...request, latest_round: { ...latest_round, revision: 2 } }).action).toBe('receipt');
  });

  it('names the actual blocker for a draft decision or unfinished cleanup', () => {
    expect(reviewBlocker({ ...request, latest_round: { status: 'awaiting_decision' } })).toContain('Publish or reject');
    expect(reviewBlocker({ ...request, latest_round: { status: 'failed', cleanup_pending: true, cleanup_status: 'needs_attention' } })).toContain('Recover');
    expect(reviewBlocker(request)).toBe('');
  });
});
