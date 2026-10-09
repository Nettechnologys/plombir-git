<script lang="ts">
  import { goto } from '$app/navigation';
  import { auth, type Me } from '$lib/api/client.svelte';
  import {
    fetchUser,
    forgetDeletedAccount,
    isAuthReady,
    isLoggedIn,
  } from '$lib/stores/auth.svelte';
  import { createT } from '$lib/i18n';
  import { PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH } from '$lib/passwordPolicy';

  const t = createT();

  let me = $state<Me | null>(null);
  let loadError = $state('');

  let displayName = $state('');
  let bio = $state('');
  let profileBusy = $state(false);
  let profileMessage = $state('');
  let profileError = $state('');

  let avatarBusy = $state(false);
  let avatarError = $state('');

  let currentPassword = $state('');
  let newPassword = $state('');
  let confirmPassword = $state('');
  let passwordBusy = $state(false);
  let passwordMessage = $state('');
  let passwordError = $state('');

  let newEmail = $state('');
  let emailPassword = $state('');
  let emailBusy = $state(false);
  let emailMessage = $state('');
  let emailError = $state('');

  let deleteConfirmation = $state('');
  let deleteBusy = $state(false);
  let deleteError = $state('');

  // A password and an address of its own exist only on a local account; one
  // that signs in through a directory or a provider has both kept there.
  const local = $derived(me?.auth_provider === 'local');

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) {
      goto('/login');
      return;
    }
    load();
  });

  async function load() {
    loadError = '';
    try {
      const profile = await auth.me();
      me = profile;
      displayName = profile.display_name ?? '';
      bio = profile.bio ?? '';
    } catch (cause: unknown) {
      loadError = message(cause);
    }
  }

  function message(cause: unknown) {
    return cause instanceof Error ? cause.message : String(cause);
  }

  async function saveProfile(e: Event) {
    e.preventDefault();
    profileBusy = true;
    profileMessage = '';
    profileError = '';
    try {
      me = await auth.updateProfile({
        display_name: displayName.trim() || null,
        bio: bio.trim() || null,
      });
      profileMessage = t('settings.profile.saved', 'Profile saved.');
      await fetchUser();
    } catch (cause: unknown) {
      profileError = message(cause);
    } finally {
      profileBusy = false;
    }
  }

  async function uploadAvatar(e: Event) {
    const file = (e.currentTarget as HTMLInputElement).files?.[0];
    if (!file || !me) return;
    avatarBusy = true;
    avatarError = '';
    try {
      const { avatar_url } = await auth.uploadAvatar(file);
      me = { ...me, avatar_url };
    } catch (cause: unknown) {
      avatarError = message(cause);
    } finally {
      avatarBusy = false;
    }
  }

  async function removeAvatar() {
    if (!me) return;
    avatarBusy = true;
    avatarError = '';
    try {
      await auth.deleteAvatar();
      me = { ...me, avatar_url: null };
    } catch (cause: unknown) {
      avatarError = message(cause);
    } finally {
      avatarBusy = false;
    }
  }

  async function changePassword(e: Event) {
    e.preventDefault();
    passwordMessage = '';
    passwordError = '';
    if (newPassword !== confirmPassword) {
      passwordError = t('settings.profile.passwords_differ', 'The two new passwords differ.');
      return;
    }
    passwordBusy = true;
    try {
      await auth.changePassword(currentPassword, newPassword);
      currentPassword = '';
      newPassword = '';
      confirmPassword = '';
      passwordMessage = t(
        'settings.profile.password_changed',
        'Password changed. Every other session was signed out; access tokens and SSH keys still work.',
      );
    } catch (cause: unknown) {
      passwordError = message(cause);
    } finally {
      passwordBusy = false;
    }
  }

  async function changeEmail(e: Event) {
    e.preventDefault();
    emailBusy = true;
    emailMessage = '';
    emailError = '';
    try {
      const res = await auth.requestEmailChange(newEmail.trim(), emailPassword);
      emailMessage = res.message;
      emailPassword = '';
    } catch (cause: unknown) {
      emailError = message(cause);
    } finally {
      emailBusy = false;
    }
  }

  async function deleteAccount(e: Event) {
    e.preventDefault();
    if (!me) return;
    if (!confirm(t('settings.profile.delete_confirm', 'Delete this account and every repository it owns? This cannot be undone.'))) {
      return;
    }
    deleteBusy = true;
    deleteError = '';
    try {
      await auth.deleteAccount(
        local ? { password: deleteConfirmation } : { confirm_username: deleteConfirmation },
      );
      forgetDeletedAccount();
      window.location.href = '/';
    } catch (cause: unknown) {
      deleteError = message(cause);
    } finally {
      deleteBusy = false;
    }
  }
</script>

<svelte:head>
  <title>{t('settings.profile.title', 'Profile and account')} · Plombir Git</title>
</svelte:head>

<div class="profile-page">
  <div class="page-header">
    <h1>{t('settings.profile.title', 'Profile and account')}</h1>
  </div>

  {#if loadError}
    <div class="error-banner" role="alert">
      {loadError}
      <button type="button" class="btn btn-sm btn-outline" onclick={load}>
        {t('settings.profile.retry', 'Retry')}
      </button>
    </div>
  {:else if me}
    <section class="section">
      <h2>{t('settings.profile.profile', 'Profile')}</h2>
      <form class="profile-form" onsubmit={saveProfile}>
        <label>
          {t('settings.profile.display_name', 'Display name')}
          <input type="text" bind:value={displayName} maxlength={255} />
        </label>
        <label>
          {t('settings.profile.bio', 'Bio')}
          <textarea bind:value={bio} maxlength={2000} rows="4"></textarea>
        </label>
        {#if profileError}<p class="error" role="alert">{profileError}</p>{/if}
        {#if profileMessage}<p class="success" role="status">{profileMessage}</p>{/if}
        <button type="submit" class="btn btn-primary" disabled={profileBusy}>
          {t('settings.profile.save', 'Save profile')}
        </button>
      </form>
    </section>

    <section class="section">
      <h2>{t('settings.profile.avatar', 'Avatar')}</h2>
      <div class="avatar-row">
        {#if me.avatar_url}
          <img class="avatar" src={me.avatar_url} alt="" width="64" height="64" />
        {/if}
        <label class="btn btn-outline">
          {t('settings.profile.upload_avatar', 'Upload a picture')}
          <input
            class="avatar-input"
            type="file"
            accept="image/png,image/jpeg,image/gif,image/webp"
            onchange={uploadAvatar}
            disabled={avatarBusy}
          />
        </label>
        {#if me.avatar_url}
          <button type="button" class="btn btn-outline remove-avatar" onclick={removeAvatar} disabled={avatarBusy}>
            {t('settings.profile.remove_avatar', 'Remove')}
          </button>
        {/if}
      </div>
      <p class="muted">{t('settings.profile.avatar_hint', 'PNG, JPEG, GIF or WebP, at most 512 KiB.')}</p>
      {#if avatarError}<p class="error" role="alert">{avatarError}</p>{/if}
    </section>

    {#if local}
      <section class="section">
        <h2>{t('settings.profile.password', 'Password')}</h2>
        <form class="password-form" onsubmit={changePassword}>
          <label>
            {t('settings.profile.current_password', 'Current password')}
            <input type="password" bind:value={currentPassword} required autocomplete="current-password" />
          </label>
          <label>
            {t('settings.profile.new_password', 'New password')}
            <input
              type="password"
              bind:value={newPassword}
              required
              autocomplete="new-password"
              minlength={PASSWORD_MIN_LENGTH}
              maxlength={PASSWORD_MAX_LENGTH}
              aria-describedby="password-policy"
            />
            <small id="password-policy" class="hint">{t('auth.password_policy', { min: PASSWORD_MIN_LENGTH })}</small>
          </label>
          <label>
            {t('settings.profile.confirm_password', 'Repeat the new password')}
            <input type="password" bind:value={confirmPassword} required autocomplete="new-password" />
          </label>
          {#if passwordError}<p class="error" role="alert">{passwordError}</p>{/if}
          {#if passwordMessage}<p class="success" role="status">{passwordMessage}</p>{/if}
          <button type="submit" class="btn btn-primary" disabled={passwordBusy}>
            {t('settings.profile.change_password', 'Change password')}
          </button>
        </form>
      </section>

      <section class="section">
        <h2>{t('settings.profile.email', 'Email address')}</h2>
        <p class="muted">{t('settings.profile.current_email', 'Current address:')} <strong>{me.email}</strong></p>
        <form class="email-form" onsubmit={changeEmail}>
          <label>
            {t('settings.profile.new_email', 'New address')}
            <input type="email" bind:value={newEmail} required autocomplete="email" />
          </label>
          <label>
            {t('settings.profile.password_to_confirm', 'Your password')}
            <input type="password" bind:value={emailPassword} required autocomplete="current-password" />
          </label>
          {#if emailError}<p class="error" role="alert">{emailError}</p>{/if}
          {#if emailMessage}<p class="success" role="status">{emailMessage}</p>{/if}
          <button type="submit" class="btn btn-primary" disabled={emailBusy}>
            {t('settings.profile.send_confirmation', 'Send confirmation link')}
          </button>
        </form>
      </section>
    {:else}
      <section class="section">
        <p class="muted">
          {t(
            'settings.profile.external_account',
            'This account signs in through an identity provider, which holds its password and email address.',
          )}
        </p>
      </section>
    {/if}

    <section class="section danger">
      <h2>{t('settings.profile.delete', 'Delete account')}</h2>
      <p class="muted">
        {t(
          'settings.profile.delete_hint',
          'Deletes the account and every repository it owns. Organizations it owns must be transferred or deleted first.',
        )}
      </p>
      <form class="delete-form" onsubmit={deleteAccount}>
        <label>
          {local
            ? t('settings.profile.delete_password', 'Your password')
            : t('settings.profile.delete_username', 'Type your username to confirm')}
          <input
            type={local ? 'password' : 'text'}
            bind:value={deleteConfirmation}
            required
            autocomplete={local ? 'current-password' : 'off'}
          />
        </label>
        {#if deleteError}<p class="error" role="alert">{deleteError}</p>{/if}
        <button type="submit" class="btn btn-danger" disabled={deleteBusy}>
          {t('settings.profile.delete_button', 'Delete my account')}
        </button>
      </form>
    </section>
  {/if}
</div>

<style>
  .hint {
    display: block;
    margin-top: 4px;
    font-size: 12px;
    color: var(--text-secondary);
  }

  .profile-page { max-width: 760px; }

  .page-header { margin-bottom: 24px; }

  h1 { margin: 0; font-size: 28px; }

  h2 { margin: 0 0 12px; font-size: 18px; }

  p { margin: 0; }

  .muted { color: var(--text-secondary); }

  .section {
    margin-bottom: 20px;
    padding: 20px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-secondary);
  }

  .danger { border-color: var(--red, #da3633); }

  form {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin-top: 12px;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 14px;
    font-weight: 600;
  }

  input,
  textarea { padding: 8px 12px; }

  form button { align-self: flex-start; }

  .avatar-row {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 8px;
  }

  .avatar { border-radius: 50%; object-fit: cover; }

  .avatar-input { display: none; }

  .error { color: var(--red, #da3633); }

  .success { color: var(--green, #3fb950); }
</style>
