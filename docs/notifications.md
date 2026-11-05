# Personal notification delivery

Accounts can choose email and Telegram independently for each activity or security event. Existing email choices and in-app notifications are preserved. Email delivery requires instance SMTP configuration. Password-reset and address-confirmation messages remain separate from these preferences.

Telegram uses a personal BotFather bot token, numeric chat ID (negative group IDs are supported), and an optional positive topic ID. The saved token is encrypted and never returned by the settings API. Keeping, replacing and removing the credential are explicit operations. A successful test or delivery confirms exactly the saved destination generation; a late result for an older generation cannot confirm a new destination.

Every Telegram credential/recipient change and every reduction in delivery requires the existing fresh-proof policy. For an account with MFA this means its current second factor or a verified user-verifying passkey; a password-only sudo session is insufficient. Accounts without MFA retain the existing password/passkey/SSO policy. The ten-minute proof window is not extended by refresh or navigation. A cancelled or failed challenge leaves the settings unchanged.

Disabling or redirecting delivery queues a warning to the **previously confirmed destinations**. New preferences do not suppress that warning. The old Telegram destination receives only the time, changed field/category names and a settings link. Its old encrypted token and recipient snapshot are retained only while delivery is pending, for at most six attempts and 24 hours, and erased at terminal completion or expiry. Logout-all does not cancel this warning. Account retirement/deletion destroys it. Delivery can fail if the old token has been revoked or the service is unavailable; it is not guaranteed. The existing previous-email-address warning on email changes remains a separate recovery path and is not duplicated by this outbox.

Save the page to store the current interface language (`en`, `ru`, `zh-CN`). Ordinary pending and test notifications use that saved language. The mandatory warning to a previous destination uses the language saved before the change. User-provided names are not translated. Testing uses the saved configuration and a clearly marked synthetic event: it never issues a token, changes MFA or accepts an arbitrary recipient/body. Each channel reports its own result. Pending tests are deduplicated, with one test per event per minute and at most six events per account per minute.

## New-device policy and rollout

Only a completely successful login can create a new-device alert. A signed, random browser witness cookie identifies a browser separately for each account; IP addresses and user-agent strings are descriptive context, not device identity. The cookie is HttpOnly, SameSite=Lax, path `/`, Secure on HTTPS, and has an absolute 180-day lifetime. Refresh does not extend it. The signature uses a domain-separated key derived from the JWT signing secret.

After upgrade, the first completed login of **each browser for each account** generates an alert. No known browsers are seeded from existing sessions, login logs, IP addresses or user agents. Cookie deletion, private browsing, expiry or rotation of the JWT signing key also makes a browser new.

Every successful password login without a valid witness produces an alert; there is no daily suppression that could hide another login. CLI scripts should use a suitably scoped PAT, or preserve and present the witness cookie between password logins. PAT, Git and ordinary API requests are not login completions and do not generate new-device alerts. No alert is emitted at the password-only stage of an unfinished MFA ceremony, on failed/cancelled authentication, or on session refresh.

## Operators

Migration `m20261105_000001_notification_channels` adds `notification_profiles`, `notification_deliveries` and `known_browsers`. Existing six-column email settings stay authoritative for their legacy categories. New security email choices default on; Telegram choices default off. The migration creates no known-browser evidence.

The common encrypted-column registry covers `notification_profiles.telegram_token_encrypted`, `notification_deliveries.previous_destination_encrypted` and `notification_deliveries.previous_token_encrypted`. Normal key-check/rekey procedures cover all three; the copied old-token ciphertext is a direct column, not nested ciphertext inside another encrypted JSON document.

Telegram egress is fixed to HTTPS `api.telegram.org`, with no redirects, configurable base URL or environment proxy. **Direct outbound HTTPS access is required.** Requests use plain text without a parse mode, disable link previews, and have bounded connect/total timeouts and response size. A Telegram chat migration is a terminal error; the recipient must be changed through the protected settings flow. Recheck connection-logging behavior when upgrading pinned reqwest: the scoped tracing regression does not cover every global log-to-tracing bridge.

The dispatcher runs without SMTP as well. It rechecks the current account, preferences, destination generation and repository permission before each ordinary send. Stale private payloads are cancelled, not redirected to a new address. Transient failures use bounded backoff and Telegram `retry_after`, with six attempts and a fixed 24-hour lifetime. Terminal rows retain only outcome metadata for 30 days. Cleanup after restart erases expired snapshots before dispatch. A stopped process cannot erase bytes until it restarts. New activity email uses the same per-event durable queue as Telegram. Pending versions of a folded thread are coalesced; deleting the source cancels its pending jobs. Historical `email_pending` rows from before this migration have no captured recipient/generation: they are retired with `legacy_destination_unknown`, preserving the in-app notification and preferences. They are not sent to a potentially changed address. The former per-person email digest is replaced by bounded per-event delivery.

A crash after remote acceptance but before local acknowledgement can duplicate a message; SMTP and Telegram do not provide end-to-end exactly-once delivery.

## API contract

All personal settings and test routes require a login session; PATs cannot read the personal Telegram destination or change/test delivery. The old read/write endpoint retains its six-key `email` shape; legacy writes now share the same fresh-proof guard as the new fields. PAT clients that previously used the old email settings endpoint must use a browser login session.

- `GET /api/v1/users/me/notification-settings`: `email`, all `events: {key: {email, telegram}}`, saved `locale`, `revision`, `telegram: {token_configured, chat_id, message_thread_id, confirmed}`, and `capabilities: {email, email_reason}`. No credential or ciphertext.
- `PUT` the same endpoint: optional `email`, `events`, `locale`, `telegram`. Omitted fields are preserved. `telegram.action` is `keep`, `set` or `remove`. `set` requires numeric `chat_id`; omitted `token` keeps the saved secret, explicit `token` replaces it even if identical; omitted/null `message_thread_id` clears the topic. `remove` clears configuration and all Telegram choices. Unknown fields/events/locales reject. Mixed protected and unprotected changes reject atomically without fresh proof (`403 sudo_required`); a revoked session remains `401 session_revoked`.
- `POST /api/v1/users/me/notification-settings/test`: `{event: key}`. Returns `{event_id, channels: {email: {status, reason}, telegram: {status, reason}}}`. Status is `queued`, `inflight`, `sent`, `failed`, `unavailable` or `cancelled`; reasons are safe finite codes.
- `GET /api/v1/users/me/notification-settings/test?event_id=...`: poll the caller's test only. Other accounts' IDs and non-test events return 404. Cooldown/pending limits return 429.

The authorization, old/new comparison, configuration write and mandatory warning insert share one retryable transaction on the authoritative write pool. Authorization is re-evaluated from the locked current account on every retry. Network delivery happens after commit.
