# REST API

[← README](../README.md)

## REST API

All endpoints live under `/api/v1/`. Authenticated routes expect an
`Authorization: Bearer <token>` header, where the token is a JWT (from login)
or a Personal Access Token. These examples continue the
[quick start](install.md#create-a-repository-and-clone-it); `PASSWORD` is the
value generated there. Save that password securely before
closing its shell.

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
