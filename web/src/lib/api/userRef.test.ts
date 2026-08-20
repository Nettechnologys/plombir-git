import { describe, expect, it } from 'vitest';

import { buildUserRef } from './userRef';

describe('buildUserRef', () => {
  it('reads a bare run of digits as the numeric id', () => {
    expect(buildUserRef(' 42 ')).toEqual({ user_id: 42 });
    expect(buildUserRef(42)).toEqual({ user_id: 42 });
  });

  it('rejects an id that is not a usable row key', () => {
    expect(buildUserRef('0')).toBeNull();
    expect(buildUserRef('99999999999999999999')).toBeNull();
  });

  it('reads anything with an @ as an e-mail', () => {
    expect(buildUserRef(' zubrenok@example.com ')).toEqual({ email: 'zubrenok@example.com' });
  });

  it('reads everything else as a username — including one that looks numeric-ish', () => {
    expect(buildUserRef('Zubrenok')).toEqual({ username: 'Zubrenok' });
    expect(buildUserRef('-1')).toEqual({ username: '-1' });
    expect(buildUserRef('1.5')).toEqual({ username: '1.5' });
  });

  it('refuses an empty field instead of sending a body that names nobody', () => {
    expect(buildUserRef('')).toBeNull();
    expect(buildUserRef('   ')).toBeNull();
  });
});
