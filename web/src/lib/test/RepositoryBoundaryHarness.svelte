<script lang="ts">
  import Boundary from '../../routes/[owner]/[repo]/+layout.svelte';
</script>

<Boundary>
  <div data-repository-content>
    <h1>Private repository content</h1>
    <a href="/fixture/secret/network">Repository tabs</a>
    <button type="button">Clone repository</button>
  </div>
</Boundary>
