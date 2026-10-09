<script lang="ts">
	import Modal from '../components/Modal.svelte';

	interface Props {
		onclose?: () => void;
		autofocusCancel?: boolean;
		empty?: boolean;
	}

	let { onclose, autofocusCancel = false, empty = false }: Props = $props();

	let open = $state(false);
	let name = $state('');

	function close() {
		onclose?.();
		open = false;
	}
</script>

<button type="button" id="opener" onclick={() => (open = true)}>Open</button>

{#if open}
	{#if empty}
		<Modal onclose={close} label="Empty dialog">
			<p>Nothing focusable here.</p>
		</Modal>
	{:else}
		<Modal onclose={close} labelledby="harness-title">
			<h2 id="harness-title">Harness</h2>
			<input id="harness-name" bind:value={name} />
			<button type="button" id="harness-disabled" disabled>Disabled</button>
			<button type="button" id="harness-save">Save</button>
			<button type="button" id="harness-cancel" data-autofocus={autofocusCancel ? '' : undefined}>Cancel</button>
		</Modal>
	{/if}
{/if}
