<script lang="ts">
  import type { PasskeyDevice } from '$lib/api/passkeys';
  import { createT } from '$lib/i18n';

  let { value = $bindable<PasskeyDevice>('auto'), disabled = false } = $props<{
    value?: PasskeyDevice;
    disabled?: boolean;
  }>();
  const t = createT();
  const hintId = $props.id();
</script>

<div class="device-choice">
  <label>
    {t('account_security.passkeys.device')}
    <select bind:value {disabled} aria-describedby={value === 'phone' ? hintId : undefined}>
      <option value="auto">{t('account_security.passkeys.device_auto')}</option>
      <option value="phone">{t('account_security.passkeys.device_phone')}</option>
    </select>
  </label>
  {#if value === 'phone'}
    <p id={hintId}>{t('account_security.passkeys.phone_hint')}</p>
  {/if}
</div>

<style>
  .device-choice, label { display: grid; gap: 6px; }
  label { color: var(--text-primary); font-size: 14px; }
  select { width: 100%; min-width: 0; }
  p { margin: 0; color: var(--text-secondary); font-size: 12px; }
</style>
