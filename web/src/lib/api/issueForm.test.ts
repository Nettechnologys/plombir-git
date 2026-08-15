import { describe, expect, it } from 'vitest';

import issuePageSource from '../../routes/[owner]/[repo]/issues/[number]/+page.svelte?raw';
import { buildIssueLinksPayload } from './issueForm';

describe('buildIssueLinksPayload', () => {
  it('uses explicit nulls for both clear operations', () => {
    expect(buildIssueLinksPayload({ assigneeId: '', milestoneId: '' })).toEqual({
      assignee_id: null,
      milestone_id: null,
    });
  });

  it('keeps selected ids numeric on the wire', () => {
    expect(buildIssueLinksPayload({ assigneeId: ' 12 ', milestoneId: '34' })).toEqual({
      assignee_id: 12,
      milestone_id: 34,
    });
  });

  it('is the payload builder used by the issue page', () => {
    expect(issuePageSource).toContain('buildIssueLinksPayload');
    expect(issuePageSource).toContain('issues.update');
    expect(issuePageSource).not.toMatch(/assignee_id:\s*[^,]+\|\|\s*undefined/);
    expect(issuePageSource).not.toMatch(/milestone_id:\s*[^,]+\|\|\s*undefined/);
  });
});
