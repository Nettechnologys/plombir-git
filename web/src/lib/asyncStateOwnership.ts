export type RepositoryRequestClaim = Readonly<{
	generation: number;
	owner: string;
	repo: string;
}>;

export type RepositoryResourceRequestClaim<ResourceIdentity extends string | number> = Readonly<{
	generation: number;
	owner: string;
	repo: string;
	resource: ResourceIdentity;
}>;

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
