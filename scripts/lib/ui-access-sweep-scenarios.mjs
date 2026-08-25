// Scenario actions are deliberately browser-only and fixture-agnostic. The
// runtime supplies the two personas, the repository names and the CDP-backed
// primitives; this registry supplies the actual UI paths and controls. Keeping
// the registry import-safe lets the cheap contract checker prove that every
// manifest scenario still has an executable owner.

async function dashboardCreateRepository(context) {
  const repository = context.repositoryFor(context.persona);
  await context.navigate('/dashboard');
  await context.click('.dashboard-header button.btn-primary');
  await context.fill('.create-form input[type="text"][required]', repository);
  await context.setChecked('.create-form input[type="checkbox"]', true, 0);
  await context.setChecked('.create-form input[type="checkbox"]', true, 1);
  await context.click('.create-form form button[type="submit"]');
  await context.waitForPath(`/${context.username}/${repository}`);
}

async function privateRepositoryPage(context) {
  await context.navigate(`/${context.ownerUsername}/${context.ownerRepository}`);
}

async function privateRepositoryStar(context) {
  await context.navigate(`/${context.ownerUsername}/${context.ownerRepository}`);
  await context.click('.repo-actions button.action-btn', 0);
}

async function privateRepositoryBlob(context) {
  await context.navigate(`/${context.ownerUsername}/${context.ownerRepository}/blob/README.md`);
}

export const UI_ACCESS_SWEEP_SCENARIOS = new Map([
  ['dashboard-create-repository', dashboardCreateRepository],
  ['private-repository-page', privateRepositoryPage],
  ['private-repository-star', privateRepositoryStar],
  ['private-repository-blob', privateRepositoryBlob],
]);
