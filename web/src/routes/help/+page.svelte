<svelte:head>
  <title>Help - IronForge</title>
</svelte:head>

<div class="help-page">
  <header class="help-header">
    <h1>Help</h1>
    <p>Common commands and entry points for using this IronForge instance.</p>
  </header>

  <section class="help-section">
    <h2>Clone a repository</h2>
    <div class="command-list">
      <code>git clone http://localhost:8080/git/OWNER/REPO</code>
      <code>git clone ssh://git@localhost:2222/OWNER/REPO</code>
    </div>
  </section>

  <section class="help-section">
    <h2>Use access tokens</h2>
    <p>Create a personal access token from security settings, then use it as a Bearer token for API requests or as the password for HTTPS Git operations.</p>
    <a href="/settings/tokens" class="link-button">Manage access tokens</a>
  </section>

  <section class="help-section">
    <h2>Find work</h2>
    <div class="quick-links">
      <a href="/explore">Explore repositories</a>
      <a href="/search">Search code and issues</a>
      <a href="/imports">Import repositories</a>
    </div>
  </section>
</div>

<style>
  .help-page {
    max-width: 920px;
    margin: 0 auto;
    padding: 40px 24px 64px;
  }

  .help-header {
    border-bottom: 1px solid var(--border-color);
    padding-bottom: 20px;
    margin-bottom: 28px;
  }

  .help-header h1 {
    margin: 0 0 8px;
    font-size: 32px;
  }

  .help-header p,
  .help-section p {
    color: var(--text-secondary);
    line-height: 1.6;
  }

  .help-section {
    padding: 20px 0;
    border-bottom: 1px solid var(--border-color);
  }

  .help-section h2 {
    margin: 0 0 12px;
    font-size: 20px;
  }

  .command-list {
    display: grid;
    gap: 10px;
  }

  code {
    display: block;
    overflow-x: auto;
    padding: 12px 14px;
    border: 1px solid var(--border-color);
    border-radius: 6px;
    background: var(--bg-secondary);
    color: var(--text-primary);
  }

  .quick-links {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
  }

  .quick-links a,
  .link-button {
    color: var(--color-primary);
    font-weight: 600;
    text-decoration: none;
  }

  .quick-links a:hover,
  .link-button:hover {
    text-decoration: underline;
  }
</style>
