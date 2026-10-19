<script lang="ts">
  // An issue label as a chip in the label's own colour. Lists used to draw every
  // label in the same purple, so `bug` and `documentation` looked alike although
  // each carries a colour (card_c899d7177f65). A label whose colour is unknown —
  // the repository's label list did not load, or the label was renamed since —
  // keeps the neutral outline.
  import { readableTextOn, safeHexColor } from '$lib/utils/color';

  let { name, color = null }: { name: string; color?: string | null } = $props();
  let background = $derived(color ? safeHexColor(color, '') : '');
</script>

{#if background}
  <span class="label-chip" data-label={name} style={`background-color: ${background}; color: ${readableTextOn(background)}`}>{name}</span>
{:else}
  <span class="label-chip outline" data-label={name}>{name}</span>
{/if}

<style>
  .label-chip {
    display: inline-block;
    padding: 0 7px;
    border: 1px solid transparent;
    border-radius: 10px;
    font-size: 11px;
    font-weight: 600;
    line-height: 18px;
    margin-left: 4px;
  }

  .label-chip.outline {
    border-color: var(--purple);
    color: var(--purple);
    font-weight: 400;
  }
</style>
