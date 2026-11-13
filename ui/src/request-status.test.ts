import { describe, expect, it } from 'vitest';
import { reviewBlocker, reviewSituation } from './request-status.js';

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
