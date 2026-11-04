<script lang="ts">
  import CopyableValue from '$lib/components/CopyableValue.svelte';
  let value = $state('fixture-first');
  let submissions = $state(0);
</script>

<form onsubmit={(event) => { event.preventDefault(); submissions += 1; }}>
  <CopyableValue {value} label="Token value" copyLabel="Copy token" />
  <button type="button" onclick={() => value = 'fixture-second'}>Replace value</button>
</form>
<output>{submissions}</output>
