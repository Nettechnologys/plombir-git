<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import { LatestRequestFence } from '$lib/asyncStateOwnership';
  import {
    buildOrganizationUpdatePayload,
    buildUserRef,
    orgs,
    repos,
    type Organization,
    type OrganizationMember,
    type OrganizationMemberRole,
    type OrganizationTeam,
    type OrganizationVisibility,
    type TeamMember,
    type TeamMemberRole,
    type TeamPermission,
  } from '$lib/api/client.svelte';
  import { createT, formatDate, formatTranslationFallback } from '$lib/i18n';
  import { getUser } from '$lib/stores/auth.svelte';

  const t = createT();
  let name = $derived($page.params.name!);

  let org = $state<Organization | null>(null);
  let teams = $state<OrganizationTeam[]>([]);
  let members = $state<OrganizationMember[]>([]);
  let orgRepos = $state<any[]>([]);
  let loading = $state(true);
  let error = $state('');
  let busyAction = $state<string | null>(null);

  let editingOrg = $state(false);
  let editDisplayName = $state('');
  let editDescription = $state('');
  let editVisibility = $state<OrganizationVisibility>('public');

  let newMemberIdentifier = $state('');
  let newMemberRole = $state<OrganizationMemberRole>('member');

  let newTeamName = $state('');
  let newTeamPermission = $state<TeamPermission>('read');
  let expandedTeamId = $state<number | null>(null);
  let teamMembers = $state<Record<number, TeamMember[]>>({});
  let loadingTeamId = $state<number | null>(null);
  let newTeamMemberIdentifier = $state('');
  let newTeamMemberRole = $state<TeamMemberRole>('member');

  let newRepoName = $state('');
  let newRepoPrivate = $state(false);
  const organizationRequests = new LatestRequestFence<string>();
  const memberRequests = new LatestRequestFence<string>();
  const teamRequests = new LatestRequestFence<string>();
  const repositoryRequests = new LatestRequestFence<string>();
  const teamMemberRequests = new LatestRequestFence<string>();
  let routeGeneration = 0;

  const canManage = $derived(
    org !== null &&
      getUser() !== null &&
      (org.owner_id === getUser()?.id ||
        members.some(
          (member) =>
            member.user_id === getUser()?.id && (member.role === 'owner' || member.role === 'admin'),
        )),
  );

  // Creating a repository here is a *member's* right, not an admin's: the API
  // gate (`NamespaceCreate`) admits anyone on the membership roll. The form
  // used to be rendered for everybody, including a stranger reading a public
  // organization, whose only feedback was a 403 on submit.
  const canCreateRepo = $derived(
    org !== null &&
      getUser() !== null &&
      (org.owner_id === getUser()?.id ||
        members.some((member) => member.user_id === getUser()?.id)),
  );

  $effect(() => {
    const expectedName = name;
    routeGeneration += 1;
    resetRouteState();
    void load(expectedName, routeGeneration);
  });

  function resetRouteState(): void {
    org = null;
    teams = [];
    members = [];
    orgRepos = [];
    loading = true;
    error = '';
    busyAction = null;
    editingOrg = false;
    editDisplayName = '';
    editDescription = '';
    editVisibility = 'public';
    newMemberIdentifier = '';
    newMemberRole = 'member';
    newTeamName = '';
    newTeamPermission = 'read';
    expandedTeamId = null;
    teamMembers = {};
    loadingTeamId = null;
    newTeamMemberIdentifier = '';
    newTeamMemberRole = 'member';
    newRepoName = '';
    newRepoPrivate = false;
  }

  function isCurrentRoute(expectedName: string, expectedRoute: number): boolean {
    return routeGeneration === expectedRoute && name === expectedName;
  }

  // A membership row's account name, or the bare id when the row outlives the
  // account it points at — never a silently blank entry.
  function memberName(member: { username: string | null; user_id: number }): string {
    return member.username ?? t('orgs.user_id', { userId: member.user_id });
  }

  function actionError(
    cause: unknown,
    fallback: string,
    expectedName = name,
    expectedRoute = routeGeneration,
  ) {
    if (isCurrentRoute(expectedName, expectedRoute)) {
      error = cause instanceof Error && cause.message ? cause.message : fallback;
    }
  }

  async function refreshMembers(expectedName = name, expectedRoute = routeGeneration) {
    if (!isCurrentRoute(expectedName, expectedRoute)) return;
    const claim = memberRequests.begin(expectedName);
    try {
      const nextMembers = await orgs.listMembers(expectedName);
      if (memberRequests.owns(claim, name) && isCurrentRoute(expectedName, expectedRoute)) {
        members = nextMembers;
      }
    } catch (cause: unknown) {
      if (memberRequests.owns(claim, name) && isCurrentRoute(expectedName, expectedRoute)) {
        throw cause;
      }
    }
  }

  async function refreshTeams(expectedName = name, expectedRoute = routeGeneration) {
    if (!isCurrentRoute(expectedName, expectedRoute)) return;
    const claim = teamRequests.begin(expectedName);
    try {
      const nextTeams = await orgs.listTeams(expectedName);
      if (teamRequests.owns(claim, name) && isCurrentRoute(expectedName, expectedRoute)) {
        teams = nextTeams;
      }
    } catch (cause: unknown) {
      if (teamRequests.owns(claim, name) && isCurrentRoute(expectedName, expectedRoute)) {
        throw cause;
      }
    }
  }

  async function refreshTeamMembers(
    teamId: number,
    expectedName = name,
    expectedRoute = routeGeneration,
  ) {
    if (!isCurrentRoute(expectedName, expectedRoute)) return;
    const identity = `${expectedName}:${teamId}`;
    const claim = teamMemberRequests.begin(identity);
    loadingTeamId = teamId;
    try {
      const nextMembers = await orgs.listTeamMembers(expectedName, teamId);
      if (
        teamMemberRequests.owns(claim, identity) &&
        isCurrentRoute(expectedName, expectedRoute) &&
        expandedTeamId === teamId
      ) {
        teamMembers[teamId] = nextMembers;
      }
    } catch (cause: unknown) {
      if (
        teamMemberRequests.owns(claim, identity) &&
        isCurrentRoute(expectedName, expectedRoute) &&
        expandedTeamId === teamId
      ) {
        throw cause;
      }
    } finally {
      if (
        teamMemberRequests.owns(claim, identity) &&
        isCurrentRoute(expectedName, expectedRoute) &&
        expandedTeamId === teamId
      ) {
        loadingTeamId = null;
      }
    }
  }

  async function refreshRepositories(expectedName = name, expectedRoute = routeGeneration) {
    if (!isCurrentRoute(expectedName, expectedRoute)) return;
    const claim = repositoryRequests.begin(expectedName);
    try {
      const nextRepositories = (await repos.list(expectedName)).data;
      if (repositoryRequests.owns(claim, name) && isCurrentRoute(expectedName, expectedRoute)) {
        orgRepos = nextRepositories;
      }
    } catch (cause: unknown) {
      if (repositoryRequests.owns(claim, name) && isCurrentRoute(expectedName, expectedRoute)) {
        throw cause;
      }
    }
  }

  async function load(expectedName: string, expectedRoute: number) {
    if (!isCurrentRoute(expectedName, expectedRoute)) return;
    const organizationClaim = organizationRequests.begin(expectedName);
    const memberClaim = memberRequests.begin(expectedName);
    const teamClaim = teamRequests.begin(expectedName);
    const repositoryClaim = repositoryRequests.begin(expectedName);
    const ownsLoad = () =>
      isCurrentRoute(expectedName, expectedRoute) &&
      organizationRequests.owns(organizationClaim, name) &&
      memberRequests.owns(memberClaim, name) &&
      teamRequests.owns(teamClaim, name) &&
      repositoryRequests.owns(repositoryClaim, name);
    try {
      const [loadedOrg, loadedMembers, loadedTeams, loadedRepos] = await Promise.all([
        orgs.get(expectedName),
        orgs.listMembers(expectedName),
        orgs.listTeams(expectedName),
        repos.list(expectedName),
      ]);
      if (ownsLoad()) {
        org = loadedOrg;
        members = loadedMembers;
        teams = loadedTeams;
        orgRepos = loadedRepos.data;
      }
    } catch (cause: unknown) {
      if (ownsLoad()) actionError(cause, t('errors.load_failed'), expectedName, expectedRoute);
    } finally {
      if (isCurrentRoute(expectedName, expectedRoute)) loading = false;
    }
  }

  function startEditingOrganization() {
    if (!org) return;
    editDisplayName = org.display_name || '';
    editDescription = org.description || '';
    editVisibility = org.visibility;
    editingOrg = true;
    error = '';
  }

  async function saveOrganization(event: SubmitEvent) {
    event.preventDefault();
    if (busyAction !== null) return;
    const expectedName = name;
    const expectedRoute = routeGeneration;
    const claim = organizationRequests.begin(expectedName);
    busyAction = 'update-org';
    error = '';
    try {
      const nextOrganization = await orgs.update(
        expectedName,
        buildOrganizationUpdatePayload({
          displayName: editDisplayName,
          description: editDescription,
          visibility: editVisibility,
        }),
      );
      if (
        organizationRequests.owns(claim, name) &&
        isCurrentRoute(expectedName, expectedRoute)
      ) {
        org = nextOrganization;
        editingOrg = false;
      }
    } catch (cause: unknown) {
      if (organizationRequests.owns(claim, name)) {
        actionError(cause, t('orgs.update_failed'), expectedName, expectedRoute);
      }
    } finally {
      if (isCurrentRoute(expectedName, expectedRoute)) busyAction = null;
    }
  }

  async function deleteOrganization() {
    if (busyAction !== null || !org || !confirm(t('orgs.delete_confirm', { name: org.name }))) {
      return;
    }
    const expectedName = name;
    const expectedRoute = routeGeneration;
    const organizationName = org.name;
    busyAction = 'delete-org';
    error = '';
    try {
      await orgs.delete(organizationName);
      if (isCurrentRoute(expectedName, expectedRoute)) await goto('/orgs');
    } catch (cause: unknown) {
      actionError(cause, t('orgs.delete_failed'), expectedName, expectedRoute);
      if (isCurrentRoute(expectedName, expectedRoute)) busyAction = null;
    }
  }

  async function addOrganizationMember(event: SubmitEvent) {
    event.preventDefault();
    if (busyAction !== null) return;
    // The name is enough — the API resolves username / id itself; an unknown
    // one, or an e-mail (refused), comes back as a 400 saying what was wrong.
    if (buildUserRef(newMemberIdentifier) === null) {
      error = t('orgs.member_required');
      return;
    }

    const expectedName = name;
    const expectedRoute = routeGeneration;
    memberRequests.begin(expectedName);
    busyAction = 'add-org-member';
    error = '';
    try {
      await orgs.addMember(expectedName, newMemberIdentifier.trim(), newMemberRole);
      if (!isCurrentRoute(expectedName, expectedRoute)) return;
      newMemberIdentifier = '';
      newMemberRole = 'member';
      await refreshMembers(expectedName, expectedRoute);
    } catch (cause: unknown) {
      actionError(cause, t('orgs.add_member_failed'), expectedName, expectedRoute);
    } finally {
      if (isCurrentRoute(expectedName, expectedRoute)) busyAction = null;
    }
  }

  async function removeOrganizationMember(member: OrganizationMember) {
    if (
      busyAction !== null ||
      !confirm(t('orgs.remove_member_confirm', { user: memberName(member) }))
    ) {
      return;
    }
    const expectedName = name;
    const expectedRoute = routeGeneration;
    memberRequests.begin(expectedName);
    busyAction = `remove-org-member-${member.user_id}`;
    error = '';
    try {
      await orgs.removeMember(expectedName, member.user_id);
      if (!isCurrentRoute(expectedName, expectedRoute)) return;
      await refreshMembers(expectedName, expectedRoute);
    } catch (cause: unknown) {
      actionError(cause, t('orgs.remove_member_failed'), expectedName, expectedRoute);
    } finally {
      if (isCurrentRoute(expectedName, expectedRoute)) busyAction = null;
    }
  }

  async function createTeam(event: SubmitEvent) {
    event.preventDefault();
    if (busyAction !== null || !newTeamName.trim()) return;
    const expectedName = name;
    const expectedRoute = routeGeneration;
    teamRequests.begin(expectedName);
    busyAction = 'create-team';
    error = '';
    try {
      await orgs.createTeam(expectedName, newTeamName.trim(), undefined, newTeamPermission);
      if (!isCurrentRoute(expectedName, expectedRoute)) return;
      newTeamName = '';
      await refreshTeams(expectedName, expectedRoute);
    } catch (cause: unknown) {
      actionError(cause, t('orgs.create_team_failed'), expectedName, expectedRoute);
    } finally {
      if (isCurrentRoute(expectedName, expectedRoute)) busyAction = null;
    }
  }

  async function deleteTeam(team: OrganizationTeam) {
    if (busyAction !== null || !confirm(t('orgs.delete_team_confirm', { name: team.name }))) {
      return;
    }
    const expectedName = name;
    const expectedRoute = routeGeneration;
    teamRequests.begin(expectedName);
    busyAction = `delete-team-${team.id}`;
    error = '';
    try {
      await orgs.deleteTeam(expectedName, team.id);
      if (!isCurrentRoute(expectedName, expectedRoute)) return;
      if (expandedTeamId === team.id) {
        teamMemberRequests.begin(`${expectedName}:deleted:${team.id}`);
        expandedTeamId = null;
        loadingTeamId = null;
      }
      delete teamMembers[team.id];
      await refreshTeams(expectedName, expectedRoute);
    } catch (cause: unknown) {
      actionError(cause, t('orgs.delete_team_failed'), expectedName, expectedRoute);
    } finally {
      if (isCurrentRoute(expectedName, expectedRoute)) busyAction = null;
    }
  }

  async function toggleTeamMembers(teamId: number) {
    if (expandedTeamId === teamId) {
      teamMemberRequests.begin(`${name}:collapsed:${teamId}`);
      expandedTeamId = null;
      loadingTeamId = null;
      return;
    }

    const expectedName = name;
    const expectedRoute = routeGeneration;
    expandedTeamId = teamId;
    newTeamMemberIdentifier = '';
    newTeamMemberRole = 'member';
    error = '';
    try {
      await refreshTeamMembers(teamId, expectedName, expectedRoute);
    } catch (cause: unknown) {
      actionError(cause, t('orgs.load_team_members_failed'), expectedName, expectedRoute);
    }
  }

  async function addTeamMember(event: SubmitEvent, teamId: number) {
    event.preventDefault();
    if (busyAction !== null) return;
    if (buildUserRef(newTeamMemberIdentifier) === null) {
      error = t('orgs.member_required');
      return;
    }

    const expectedName = name;
    const expectedRoute = routeGeneration;
    teamMemberRequests.begin(`${expectedName}:${teamId}`);
    busyAction = `add-team-member-${teamId}`;
    error = '';
    try {
      await orgs.addTeamMember(
        expectedName,
        teamId,
        newTeamMemberIdentifier.trim(),
        newTeamMemberRole,
      );
      if (!isCurrentRoute(expectedName, expectedRoute)) return;
      newTeamMemberIdentifier = '';
      newTeamMemberRole = 'member';
      await refreshTeamMembers(teamId, expectedName, expectedRoute);
    } catch (cause: unknown) {
      actionError(cause, t('orgs.add_team_member_failed'), expectedName, expectedRoute);
    } finally {
      if (isCurrentRoute(expectedName, expectedRoute)) busyAction = null;
    }
  }

  async function removeTeamMember(teamId: number, member: TeamMember) {
    if (
      busyAction !== null ||
      !confirm(t('orgs.remove_team_member_confirm', { user: memberName(member) }))
    ) {
      return;
    }
    const expectedName = name;
    const expectedRoute = routeGeneration;
    teamMemberRequests.begin(`${expectedName}:${teamId}`);
    busyAction = `remove-team-member-${teamId}-${member.user_id}`;
    error = '';
    try {
      await orgs.removeTeamMember(expectedName, teamId, member.user_id);
      if (!isCurrentRoute(expectedName, expectedRoute)) return;
      await refreshTeamMembers(teamId, expectedName, expectedRoute);
    } catch (cause: unknown) {
      actionError(cause, t('orgs.remove_team_member_failed'), expectedName, expectedRoute);
    } finally {
      if (isCurrentRoute(expectedName, expectedRoute)) busyAction = null;
    }
  }

  async function createOrgRepo() {
    if (busyAction !== null || !newRepoName.trim()) return;
    const expectedName = name;
    const expectedRoute = routeGeneration;
    repositoryRequests.begin(expectedName);
    busyAction = 'create-repo';
    error = '';
    try {
      await repos.create({
        name: newRepoName,
        is_private: newRepoPrivate,
        org: expectedName,
      });
      if (!isCurrentRoute(expectedName, expectedRoute)) return;
      newRepoName = '';
      newRepoPrivate = false;
      await refreshRepositories(expectedName, expectedRoute);
    } catch (cause: unknown) {
      actionError(cause, t('errors.create_failed'), expectedName, expectedRoute);
    } finally {
      if (isCurrentRoute(expectedName, expectedRoute)) busyAction = null;
    }
  }
</script>

<div class="container">
  {#if loading}
    <p>{t('common.loading')}</p>
  {:else if error && !org}
    <div class="error">{error}</div>
  {:else if org}
    <div class="org-header">
      <div class="org-heading">
        <div class="org-avatar">{org.name[0]?.toUpperCase() || '?'}</div>
        <div>
          <h1>{org.display_name || org.name}</h1>
          <p class="org-meta">@{org.name} · {t(`orgs.visibility_${org.visibility}`, undefined, formatTranslationFallback(org.visibility))} · {t('common.created', { date: formatDate(org.created_at) })}</p>
          {#if org.description}<p class="org-desc">{org.description}</p>{/if}
        </div>
      </div>
      {#if canManage}
        <div class="header-actions">
          <button type="button" class="btn-secondary" onclick={startEditingOrganization} disabled={busyAction !== null}>
            {t('common.edit')}
          </button>
          <button type="button" class="btn-danger" onclick={deleteOrganization} disabled={busyAction !== null}>
            {busyAction === 'delete-org' ? t('common.loading') : t('orgs.delete_organization')}
          </button>
        </div>
      {/if}
    </div>

    {#if editingOrg}
      <form class="section edit-organization" onsubmit={saveOrganization}>
        <h2>{t('orgs.edit_organization')}</h2>
        <div class="form-grid">
          <label>
            <span>{t('orgs.display_name')}</span>
            <input type="text" bind:value={editDisplayName} disabled={busyAction !== null} />
          </label>
          <label>
            <span>{t('orgs.visibility')}</span>
            <select bind:value={editVisibility} disabled={busyAction !== null}>
              <option value="public">{t('orgs.visibility_public')}</option>
              <option value="private">{t('orgs.visibility_private')}</option>
            </select>
          </label>
          <label class="wide-field">
            <span>{t('orgs.description')}</span>
            <textarea bind:value={editDescription} rows="3" disabled={busyAction !== null}></textarea>
          </label>
        </div>
        <div class="form-actions">
          <button type="submit" class="btn-sm" disabled={busyAction !== null}>
            {busyAction === 'update-org' ? t('common.loading') : t('common.save')}
          </button>
          <button type="button" class="btn-secondary" onclick={() => editingOrg = false} disabled={busyAction !== null}>
            {t('common.cancel')}
          </button>
        </div>
      </form>
    {/if}

    {#if error}
      <div class="error page-error" role="alert">{error}</div>
    {/if}

    <!-- Organization Repositories -->
    <div class="section repositories-section">
      <h2>{t('orgs.repositories', { count: String(orgRepos.length) })}</h2>
      {#if canCreateRepo}
        <div class="create-form">
          <input
            type="text"
            bind:value={newRepoName}
            placeholder={t('orgs.new_repo')}
            disabled={busyAction !== null}
          />
          <label class="checkbox-label">
            <input type="checkbox" bind:checked={newRepoPrivate} disabled={busyAction !== null} />
            {t('orgs.private')}
          </label>
          <button
            type="button"
            class="btn-sm"
            onclick={createOrgRepo}
            disabled={busyAction !== null || !newRepoName.trim()}
          >
            {busyAction === 'create-repo' ? t('common.loading') : t('orgs.create_repo')}
          </button>
        </div>
        <!-- This form takes a name and nothing else. The dashboard one carries
             README / .gitignore / licence / label-set templates and knows how to
             create under an organization, so it is offered rather than copied. -->
        <p class="create-hint">
          <a href={`/dashboard?owner=${encodeURIComponent(org.name)}`}>{t('orgs.create_repo_advanced')}</a>
        </p>
      {/if}
      {#if orgRepos.length === 0}
        <p class="empty">{t('orgs.no_repos')}</p>
      {:else}
        <div class="repo-list">
          {#each orgRepos as repo}
            <a href={`/${org.name}/${repo.name}`} class="repo-item">
              <span class="repo-icon">{repo.is_private ? '🔒' : '📖'}</span>
              <span class="repo-name">{repo.name}</span>
              {#if repo.description}<span class="repo-desc">{repo.description}</span>{/if}
            </a>
          {/each}
        </div>
      {/if}
    </div>

    <div class="grid">
      <!-- Teams -->
      <div class="section">
        <h2>{t('orgs.teams', { count: String(teams.length) })}</h2>
        {#if canManage}
          <form class="create-form" onsubmit={createTeam}>
            <input type="text" bind:value={newTeamName} placeholder={t('orgs.new_team')} disabled={busyAction !== null} />
            <select bind:value={newTeamPermission} disabled={busyAction !== null}>
              <option value="read">{t('orgs.permission.read')}</option>
              <option value="write">{t('orgs.permission.write')}</option>
              <option value="admin">{t('orgs.permission.admin')}</option>
            </select>
            <button type="submit" class="btn-sm" disabled={busyAction !== null || !newTeamName.trim()}>
              {busyAction === 'create-team' ? t('common.loading') : t('orgs.create_team')}
            </button>
          </form>
        {/if}
        {#if teams.length === 0}
          <p class="empty">{t('orgs.no_teams')}</p>
        {:else}
          <div class="managed-list">
            {#each teams as team (team.id)}
              <div class="managed-item">
                <div class="item-row">
                  <div>
                    <span class="item-name">{team.name}</span>
                    {#if team.description}<p class="item-description">{team.description}</p>{/if}
                  </div>
                  <div class="item-actions">
                    <span class="badge">{t(`orgs.permission.${team.permission}`, undefined, formatTranslationFallback(team.permission))}</span>
                    <button type="button" class="btn-link" onclick={() => toggleTeamMembers(team.id)} disabled={loadingTeamId === team.id}>
                      {expandedTeamId === team.id ? t('orgs.hide_team_members') : t('orgs.view_team_members')}
                    </button>
                    {#if canManage}
                      <button type="button" class="btn-danger" onclick={() => deleteTeam(team)} disabled={busyAction !== null}>
                        {busyAction === `delete-team-${team.id}` ? t('common.loading') : t('common.delete')}
                      </button>
                    {/if}
                  </div>
                </div>

                {#if expandedTeamId === team.id}
                  <div class="team-members">
                    <h3>{t('orgs.team_members')}</h3>
                    {#if canManage}
                      <form class="member-form" onsubmit={(event) => addTeamMember(event, team.id)}>
                        <input
                          type="text"
                          bind:value={newTeamMemberIdentifier}
                          placeholder={t('orgs.member_placeholder')}
                          disabled={busyAction !== null}
                        />
                        <select bind:value={newTeamMemberRole} disabled={busyAction !== null}>
                          <option value="member">{t('orgs.role_member')}</option>
                          <option value="maintainer">{t('orgs.role_maintainer')}</option>
                        </select>
                        <button type="submit" class="btn-sm" disabled={busyAction !== null}>
                          {busyAction === `add-team-member-${team.id}` ? t('common.loading') : t('common.add')}
                        </button>
                      </form>
                    {/if}

                    {#if loadingTeamId === team.id}
                      <p class="empty">{t('common.loading')}</p>
                    {:else if (teamMembers[team.id] || []).length === 0}
                      <p class="empty">{t('orgs.no_team_members')}</p>
                    {:else}
                      {#each teamMembers[team.id] || [] as teamMember (teamMember.id)}
                        <div class="item compact-item">
                          <span class="item-name">{memberName(teamMember)}</span>
                          <div class="item-actions">
                            <span class="badge">{t(`orgs.role_${teamMember.role}`, undefined, formatTranslationFallback(teamMember.role))}</span>
                            {#if canManage}
                              <button
                                type="button"
                                class="btn-danger"
                                onclick={() => removeTeamMember(team.id, teamMember)}
                                disabled={busyAction !== null}
                              >
                                {busyAction === `remove-team-member-${team.id}-${teamMember.user_id}` ? t('common.loading') : t('common.delete')}
                              </button>
                            {/if}
                          </div>
                        </div>
                      {/each}
                    {/if}
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
      </div>

      <!-- Members -->
      <div class="section">
        <h2>{t('orgs.members', { count: String(members.length) })}</h2>
        {#if canManage}
          <form class="member-form" onsubmit={addOrganizationMember}>
            <input
              type="text"
              bind:value={newMemberIdentifier}
              placeholder={t('orgs.member_placeholder')}
              disabled={busyAction !== null}
            />
            <select bind:value={newMemberRole} disabled={busyAction !== null}>
              <option value="member">{t('orgs.role_member')}</option>
              <option value="admin">{t('orgs.role_admin')}</option>
              <option value="owner">{t('orgs.role_owner')}</option>
            </select>
            <button type="submit" class="btn-sm" disabled={busyAction !== null}>
              {busyAction === 'add-org-member' ? t('common.loading') : t('common.add')}
            </button>
          </form>
        {/if}
        {#if members.length === 0}
          <p class="empty">{t('orgs.no_members')}</p>
        {:else}
          {#each members as member (member.id)}
            <div class="item">
              <span class="item-name">{memberName(member)}</span>
              <div class="item-actions">
                <span class="badge">{t(`orgs.role_${member.role}`, undefined, formatTranslationFallback(member.role))}</span>
                {#if canManage}
                  <button
                    type="button"
                    class="btn-danger"
                    onclick={() => removeOrganizationMember(member)}
                    disabled={busyAction !== null}
                  >
                    {busyAction === `remove-org-member-${member.user_id}` ? t('common.loading') : t('common.delete')}
                  </button>
                {/if}
              </div>
            </div>
          {/each}
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .org-header,
  .org-heading,
  .header-actions,
  .item-row,
  .item-actions,
  .form-actions {
    display: flex;
    align-items: center;
  }

  .org-header {
    justify-content: space-between;
    gap: 1rem;
    margin-bottom: 1.5rem;
  }

  .org-heading,
  .header-actions,
  .item-actions,
  .form-actions {
    gap: 0.75rem;
  }

  .org-avatar {
    width: 64px;
    height: 64px;
    border-radius: 50%;
    background: var(--accent);
    color: white;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 1.5rem;
    font-weight: 700;
    flex: 0 0 auto;
  }

  h1 {
    color: var(--text-primary);
    margin: 0;
  }

  .org-meta {
    color: var(--text-secondary);
    font-size: 0.9rem;
    margin: 0.3rem 0 0;
  }

  .org-desc {
    color: var(--text-primary);
    margin: 0.5rem 0 0;
  }

  .grid {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 1.5rem;
  }

  .section {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 1.25rem;
  }

  .repositories-section,
  .edit-organization,
  .page-error {
    margin-bottom: 1.5rem;
  }

  h2,
  h3 {
    color: var(--text-primary);
    margin: 0 0 1rem;
  }

  h2 {
    font-size: 1.1rem;
  }

  h3 {
    font-size: 0.95rem;
  }

  .form-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1rem;
    margin-bottom: 1rem;
  }

  .form-grid label {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    color: var(--text-secondary);
    font-size: 0.85rem;
  }

  .wide-field {
    grid-column: 1 / -1;
  }

  .create-form,
  .member-form {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 1rem;
    align-items: center;
  }

  input,
  select,
  textarea {
    background: var(--bg-primary);
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.45rem 0.6rem;
    font: inherit;
  }

  .create-form input,
  .member-form input {
    min-width: 0;
    flex: 1;
  }

  textarea {
    resize: vertical;
  }

  .checkbox-label {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.85rem;
    color: var(--text-secondary);
    white-space: nowrap;
  }

  button {
    cursor: pointer;
  }

  button:disabled {
    cursor: not-allowed;
    opacity: 0.6;
  }

  .btn-sm,
  .btn-secondary,
  .btn-danger,
  .btn-link {
    border-radius: 4px;
    padding: 0.4rem 0.7rem;
    font-size: 0.82rem;
  }

  .btn-sm {
    background: var(--accent);
    color: white;
    border: 1px solid var(--accent);
  }

  .btn-secondary {
    background: var(--bg-primary);
    border: 1px solid var(--border);
    color: var(--text-primary);
  }

  .btn-danger {
    background: rgba(248, 81, 73, 0.12);
    border: 1px solid #f85149;
    color: #f85149;
  }

  .btn-link {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--accent);
  }

  .managed-list {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .managed-item {
    border-top: 1px solid var(--border);
    padding-top: 0.75rem;
  }

  .managed-item:first-child {
    border-top: none;
    padding-top: 0;
  }

  .item-row {
    justify-content: space-between;
    gap: 0.75rem;
  }

  .item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--border);
  }

  .item:last-child {
    border-bottom: none;
  }

  .compact-item {
    font-size: 0.88rem;
  }

  .item-name {
    color: var(--text-primary);
  }

  .item-description {
    color: var(--text-secondary);
    font-size: 0.82rem;
    margin: 0.25rem 0 0;
  }

  .team-members {
    background: var(--bg-primary);
    border-radius: 6px;
    margin-top: 0.75rem;
    padding: 0.75rem;
  }

  .badge {
    background: var(--bg-primary);
    color: var(--text-secondary);
    padding: 0.15rem 0.5rem;
    border-radius: 12px;
    font-size: 0.75rem;
    border: 1px solid var(--border);
    white-space: nowrap;
  }

  .empty {
    color: var(--text-secondary);
    font-style: italic;
  }

  .create-hint {
    margin: -0.25rem 0 0.75rem;
    font-size: 0.8rem;
    color: var(--text-secondary);
  }

  .error {
    color: #f85149;
    background: rgba(248, 81, 73, 0.1);
    padding: 0.5rem 0.75rem;
    border-radius: 6px;
  }

  .repo-list {
    display: flex;
    flex-direction: column;
  }

  .repo-item {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.5rem 0.75rem;
    color: var(--text-primary);
    text-decoration: none;
    border-bottom: 1px solid var(--border);
  }

  .repo-item:last-child {
    border-bottom: none;
  }

  .repo-item:hover {
    background: var(--bg-hover);
  }

  .repo-icon {
    font-size: 1rem;
  }

  .repo-name {
    font-weight: 500;
  }

  .repo-desc {
    color: var(--text-secondary);
    font-size: 0.85rem;
    margin-left: auto;
  }

  @media (max-width: 900px) {
    .grid,
    .form-grid {
      grid-template-columns: 1fr;
    }

    .wide-field {
      grid-column: auto;
    }

    .org-header,
    .item-row {
      align-items: flex-start;
      flex-direction: column;
    }

    .create-form,
    .member-form {
      align-items: stretch;
      flex-direction: column;
    }
  }
</style>
