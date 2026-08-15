import { describe, expect, it } from 'vitest';

import boardPageSource from '../../routes/[owner]/[repo]/boards/+page.svelte?raw';
import {
  buildBoardCardUpdatePayload,
  buildBoardUpdatePayload,
  buildColumnUpdatePayload,
} from './boardForm';

describe('board edit payloads', () => {
  it('keeps an existing issue link while editing the note', () => {
    expect(buildBoardCardUpdatePayload({ note: 'updated note', issueId: '42' })).toEqual({
      note: 'updated note',
      issue_id: 42,
    });
  });

  it('uses explicit null only when the operator selects no issue', () => {
    expect(buildBoardCardUpdatePayload({ note: 'standalone', issueId: '' })).toEqual({
      note: 'standalone',
      issue_id: null,
    });
  });

  it('normalizes board and column names', () => {
    expect(buildBoardUpdatePayload({ name: ' Roadmap ', description: ' Q4 ' })).toEqual({
      name: 'Roadmap',
      description: 'Q4',
    });
    expect(buildColumnUpdatePayload(' In review ')).toEqual({ name: 'In review' });
  });

  it('wires every formerly unused PATCH client method into the board page', () => {
    expect(boardPageSource).toContain('buildBoardUpdatePayload');
    expect(boardPageSource).toContain('buildColumnUpdatePayload');
    expect(boardPageSource).toContain('buildBoardCardUpdatePayload');
    expect(boardPageSource).toContain('boards.update(');
    expect(boardPageSource).toContain('boards.updateColumn(');
    expect(boardPageSource).toContain('boards.updateCard(');
  });
});
