export type RepositoryRequestClaim = Readonly<{
	generation: number;
	owner: string;
	repo: string;
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
