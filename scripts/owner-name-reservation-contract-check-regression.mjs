import assert from 'node:assert/strict';
import path from 'node:path';
import test from 'node:test';
import { loadProductionRust } from './lib/rust-consumer-contract.mjs';
import { checkOwnerReservations } from './owner-name-reservation-contract-check.mjs';

const baseline = loadProductionRust(path.resolve('crates'));
test('current owner-name boundaries pass', () => {
  assert.deepEqual(checkOwnerReservations(baseline), []);
});
test('a new local-only namespace door is rejected', () => {
  const changed = new Map(baseline);
  changed.set(path.resolve('crates/rg-core/src/new_door.rs'), 'pub async fn create() { owner_name_is_taken(db, name).await?; }');
  assert.ok(checkOwnerReservations(changed).some((failure) => failure.includes('new_door.rs:create')));
});
test('removing the shared LDAP check is rejected', () => {
  const changed = new Map(baseline);
  const file = path.resolve('crates/rg-core/src/namespace.rs');
  const source = changed.get(file);
  assert.ok(source.includes('ldap_directory_holds'));
  changed.set(file, source.replaceAll('ldap_directory_holds', 'no_directory_check'));
  assert.ok(checkOwnerReservations(changed).some((failure) => failure.includes('missing ldap_directory_holds')));
});
for (const file of ['crates/rg-core/src/user/bots.rs', 'crates/rg-core/src/org/mod.rs']) {
  test(`${file}: removing the reservation call is rejected`, () => {
    const changed = new Map(baseline);
    const absolute = path.resolve(file);
    const source = changed.get(absolute);
    assert.ok(source.includes('owner_name_is_reserved'));
    changed.set(absolute, source.replaceAll('owner_name_is_reserved', 'owner_name_is_taken'));
    assert.ok(checkOwnerReservations(changed).some((failure) => failure.includes('missing owner_name_is_reserved')));
  });
}
