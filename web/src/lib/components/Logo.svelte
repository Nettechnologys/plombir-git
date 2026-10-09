<!--
  The Plombir Git mark: a scoop of plombir on a wafer cone.

  One component so every page draws the same mark. The pages used to inline
  GitHub's Octocat, which is GitHub's trademark and not this product's
  (card_e34af7255aa6). `static/favicon.svg` draws the same paths;
  `logoMark.test.ts` holds the two together.
-->
<script lang="ts">
  interface Props {
    size?: number;
    /** Accessible name; omitted, the mark is decorative and hidden from AT. */
    title?: string;
  }

  let { size = 28, title }: Props = $props();
</script>

<svg
  viewBox="0 0 16 16"
  width={size}
  height={size}
  fill="currentColor"
  role={title ? 'img' : undefined}
  aria-hidden={title ? undefined : 'true'}
  aria-label={title}
  class="logo-mark"
>
  <path d="M2.4 7.4a5.6 5.6 0 0 1 11.2 0q-.7 1.9-1.4 0q-.7 1.9-1.4 0q-.7 1.9-1.4 0q-.7 1.9-1.4 0q-.7 1.9-1.4 0q-.7 1.9-1.4 0q-.7 1.9-1.4 0q-.7 1.9-1.4 0z" />
  <path fill-opacity=".72" d="M3.9 9.6h8.2L8 15.7z" />
</svg>
