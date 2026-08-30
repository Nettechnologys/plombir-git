export type RepositoryRequestClaim = Readonly<{
	generation: number;
	owner: string;
	repo: string;
}>;

export type RequestClaim<RequestIdentity extends string | number> = Readonly<{
	generation: number;
	identity: RequestIdentity;
}>;

export type RepositoryResourceRequestClaim<ResourceIdentity extends string | number> = Readonly<{
	generation: number;
	owner: string;
	repo: string;
	resource: ResourceIdentity;
}>;

/**
 * Assigns one state slot to the newest request for an explicit identity.
 *
 * Component-local fences normally use a stable literal for one collection.
 * Paginated or filtered collections include those inputs in the identity so a
 * response cannot publish under a page or filter it did not request.
 */
export class LatestRequestFence<RequestIdentity extends string | number> {
	#generation = 0;

	begin(identity: RequestIdentity): RequestClaim<RequestIdentity> {
		return { generation: ++this.#generation, identity };
	}

	owns(claim: RequestClaim<RequestIdentity>, identity: RequestIdentity): boolean {
		return claim.generation === this.#generation && claim.identity === identity;
	}
}

/**
 * Assigns one repository-scoped state slot to the newest request that can
 * publish into it. The route identity is part of the claim: a response from a
 * repository that was navigated away from can never become current again.
 */
export class LatestRepositoryRequestFence {
	#generation = 0;

	begin(owner: string, repo: string): RepositoryRequestClaim {
		return { generation: ++this.#generation, owner, repo };
	}

	owns(claim: RepositoryRequestClaim, owner: string, repo: string): boolean {
		return (
			claim.generation === this.#generation && claim.owner === owner && claim.repo === repo
		);
	}
}

/**
 * Assigns one resource-detail state slot to the newest repository/resource
 * selection. Generation alone orders overlapping requests; the explicit route
 * and resource identity make accidental publication under a different current
 * selection fail closed as well.
 */
export class LatestRepositoryResourceRequestFence<ResourceIdentity extends string | number> {
	#generation = 0;

	begin(
		owner: string,
		repo: string,
		resource: ResourceIdentity,
	): RepositoryResourceRequestClaim<ResourceIdentity> {
		return { generation: ++this.#generation, owner, repo, resource };
	}

	owns(
		claim: RepositoryResourceRequestClaim<ResourceIdentity>,
		owner: string,
		repo: string,
		resource: ResourceIdentity,
	): boolean {
		return (
			claim.generation === this.#generation &&
			claim.owner === owner &&
			claim.repo === repo &&
			claim.resource === resource
		);
	}
}
