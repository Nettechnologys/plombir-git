// The test double for `$app/state`'s `page`. In the app it is a rune-backed
// object, so a `$derived(page.params.repo)` re-runs when the route changes, and
// the A -> B -> A route-reuse tests depend on exactly that: `setTestPage` has to
// wake every derived value and effect that read the page, the way a client-side
// navigation does. A plain object would hand each component its first route
// forever and turn those tests green for the wrong reason.
export type TestPage = {
	data: Record<string, unknown>;
	params: Record<string, string>;
	route: { id: string | null };
	state: Record<string, unknown>;
	status: number;
	url: URL;
};

export const pageState: TestPage = $state({
	data: {},
	params: {},
	route: { id: null },
	state: {},
	status: 200,
	url: new URL('https://plombir-git.test/'),
});
