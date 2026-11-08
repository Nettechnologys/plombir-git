# REST API

[← README](../README.md)

## REST API

All endpoints live under `/api/v1/`. Authenticated routes expect an
`Authorization: Bearer <token>` header, where the token is a JWT (from login)
or a Personal Access Token. These examples continue the
[quick start](install.md#create-a-repository-and-clone-it); `PASSWORD` is the
value generated there. Save that password securely before
closing its shell.

Personal notification settings and delivery tests require a login session;
PATs cannot access the saved Telegram destination. See the
[notification API and fresh-proof policy](notifications.md#api-contract).

### Repository visibility

A repository the caller cannot read and a repository that does not exist both
answer `404` with `NOT_FOUND` / `repository not found`. This applies to
anonymous requests, authenticated outsiders and repository-confined PATs.
The response never supplies another private name or clone URL. Permission to
read does not imply permission to write: a reader denied a write/admin action
still receives `403`. Invalid/revoked credentials can receive `401` before any
repository lookup; backend outages remain `5xx`.

The browser uses the same full-page 404 for hidden and missing repositories,
without mounting repository navigation or clone controls. The HTML document
also has status 404 and is sent with `Cache-Control: private, no-store`.

Git smart HTTP and Git LFS retain credential discovery: an anonymous request
gets the same `401` for a hidden or missing repository so the client can ask its
credential helper. An authenticated outsider gets `404`. SSH user/deploy keys
receive the same non-zero failure and `repository not found` message for a
hidden or missing target. These rules conceal existence through response
status/body; they do not promise indistinguishable network timing.

```bash
# Login → returns a JWT
TOKEN="$(curl -fsS -X POST "$BASE_URL/api/v1/users/login" \
  -H "Content-Type: application/json" \
  -d "$(jq -n --arg login testuser --arg password "$PASSWORD" \
    '{login:$login,password:$password}')" | jq -er '.token')"

# Create a repository
curl -X POST "$BASE_URL/api/v1/repos" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"myrepo","description":"test repo","auto_init":true}'

# Open an issue
curl -X POST "$BASE_URL/api/v1/repos/testuser/myrepo/issues" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"title":"Bug report","body":"Something is wrong","labels":[]}'

# Open a pull request after pushing a feature branch
curl -X POST "$BASE_URL/api/v1/repos/testuser/myrepo/pulls" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"title":"Add feature","body":"Description","head":"feature","base":"main"}'
```

The full API is documented via OpenAPI. `/api-docs/openapi.json` is protected
by default and requires a session, a JWT or a PAT; load it into Swagger UI,
Redoc or a client generator:

```bash
TOKEN="$(curl -fsS "$BASE_URL/api/v1/users/login" \
  -H 'Content-Type: application/json' \
  -d "$(jq -n --arg login testuser --arg password "$PASSWORD" \
    '{login:$login,password:$password}')" | jq -er '.token')"

curl -H "Authorization: Bearer ${TOKEN}" "$BASE_URL/api-docs/openapi.json"
```


### Fresh proof for access administration (0.4.1)

Repository collaborator, organization membership/team, webhook (repository,
organization and system), mirror, branch/tag protection, CI secret/environment
writes, repository transfer/deletion and organization deletion require a browser
login session with fresh sudo proof. A PATCH containing `is_private` (repository)
or `visibility` (organization) uses the same rule, including a mixed or no-op patch.
Admin unlock/account/org/provider/runner deletion and bot deletion are guarded too.
MFA-enabled accounts must have proved their current factor. Failure is `403` with
`error.reason = "sudo_required"`; a revoked session is `401 session_revoked`.
Resource authorization precedes this check, preserving the private/missing 404 envelope.
PATs are rejected on these writes regardless of scope; clients must not retry with a
PAT or treat the refusal as success. Ordinary operations, CI variables and emergency
credential revocation retain their existing role/scope contract.

Team membership creation now rejects a user who is not a current organization
member with `400`; removal from the organization deletes its team grants atomically.
