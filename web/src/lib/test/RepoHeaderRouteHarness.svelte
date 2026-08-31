<script lang="ts">
	import RepoHeader from '../components/RepoHeader.svelte';

	interface Props {
		owner: string;
		repo: string;
		defaultBranch?: string;
	}

	let { owner, repo, defaultBranch }: Props = $props();

	export function visit(nextOwner: string, nextRepo: string, nextDefaultBranch?: string): void {
		owner = nextOwner;
		repo = nextRepo;
		defaultBranch = nextDefaultBranch;
	}
</script>

<RepoHeader {owner} {repo} {defaultBranch} />
