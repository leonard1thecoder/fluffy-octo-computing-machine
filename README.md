# Nhlanganiso Catering & Baking — Backend

[![Backend CI](https://github.com/leonard1thecoder/fluffy-octo-computing-machine/actions/workflows/ci.yml/badge.svg)](https://github.com/leonard1thecoder/fluffy-octo-computing-machine/actions/workflows/ci.yml)

Production-oriented Rust (Axum + Tokio + PostgreSQL via SQLx) modular monolith.
Frontend (Next.js) communicates via versioned REST API under `/api/v1` (see `contracts/openapi.yaml`).
Currency: ZAR (integer cents internally, `%.2f` only at PayFast boundary).

## Layout

```
nhlanganiso/
  Dockerfile           # container image (build context: repo root)
  render.yaml          # Render Blueprint (API + Postgres; Redis optional)
  contracts/openapi.yaml
  services/api/        # Rust service
```

## Quick start

```bash
cp services/api/.env.example services/api/.env
# edit secrets: JWT_SECRET (openssl rand -base64 48), DATABASE_URL (Supabase
# session pooler), REDIS_URL (optional: Upstash or local, enables rate limiting),
# PUBLIC_*, CORS_ALLOWED_ORIGINS
# local Redis if needed: docker run -d -p 6379:6379 redis:7-alpine
cargo run --manifest-path services/api/Cargo.toml
# migrations run automatically on boot (sqlx::migrate!)
curl http://localhost:8080/api/v1/health/ready
```

## Where this deploys

Two Vercel projects, one repo each:

- **This repo = the Rust API.** It is built by Vercel (Rust preset, auto-detected
  from `services/api/Cargo.toml`) and served from
  `https://fluffy-octo-computing-machine-byij.vercel.app`.
  `./Dockerfile` (build context: repo root) and `render.yaml` remain available if
  you prefer a long-running container host (Render/Railway/Fly) — a container is
  the better fit for sustained traffic, since a Vercel function is invoked per
  request and the email-outbox worker only runs while the instance is awake.
- **The storefront = the Next.js frontend** (separate repo, `nhlanganiso-app`).
  It talks to the API through a same-origin rewrite, so the browser never makes a
  cross-origin request and CORS can never block it:
  `next.config.mjs` maps `/api/v1/:path*` -> `${API_UPSTREAM}/api/v1/:path*`.
  Set `API_UPSTREAM=https://fluffy-octo-computing-machine-byij.vercel.app` in the
  frontend's Vercel environment variables.

### API environment variables (Vercel: Project > Settings > Environment Variables)

| Variable | Value |
|---|---|
| `APP_ENV` | `production` |
| `DATABASE_URL` | Supabase **session pooler** (`:5432`, `?sslmode=require`) |
| `DATABASE_MAX_CONNECTIONS` | `5` |
| `DATABASE_MIN_CONNECTIONS` | `0` |
| `JWT_SECRET` | 32+ random bytes (`openssl rand -base64 48`) |
| `JWT_ISSUER` | `nhlanganiso-api` |
| `JWT_AUDIENCE` | `nhlanganiso-web` |
| `JWT_ACCESS_TTL_SECONDS` | `900` |
| `JWT_REFRESH_TTL_SECONDS` | `2592000` |
| `PUBLIC_WEB_URL` | `https://nhlanganiso-app.vercel.app` (where the API redirects back to) |
| `PUBLIC_API_URL` | `https://fluffy-octo-computing-machine-byij.vercel.app` (PayFast `notify_url`/`return_url` base) |
| `CORS_ALLOWED_ORIGINS` | `https://nhlanganiso-app.vercel.app` |
| `GOOGLE_CLIENT_ID` | Google Cloud Console > OAuth 2.0 Client ID |
| `GOOGLE_CLIENT_SECRET` | Google Cloud Console > client secret |
| `GOOGLE_REDIRECT_URI` | `https://fluffy-octo-computing-machine-byij.vercel.app/api/v1/auth/google/callback` |
| `GOOGLE_ISSUER` | `https://accounts.google.com` (default) |
| `GMAIL_CLIENT_ID` / `GMAIL_CLIENT_SECRET` / `GMAIL_REFRESH_TOKEN` | Google Cloud OAuth client and consent grant with `gmail.send` scope |
| `GMAIL_SENDER_EMAIL` / `GMAIL_SENDER_NAME` | Gmail mailbox authorized by the refresh token |
| `PAYFAST_MERCHANT_ID` / `PAYFAST_MERCHANT_KEY` / `PAYFAST_PASSPHRASE` | PayFast dashboard |
| `PAYFAST_MODE` | `sandbox` until you go live |
| `REDIS_URL` | optional - unset means rate limiting is disabled |
| `LOG_LEVEL` | `info` |

**The Google redirect URI points at the API host.** Google redirects the browser
straight to `fluffy-octo-computing-machine-byij.vercel.app/api/v1/auth/google/callback`
- a top-level browser navigation, so CORS never applies and the storefront proxy
is not involved. The API then bounces the browser to `PUBLIC_WEB_URL` with a
single-use code, which the storefront exchanges for tokens. The URI must match
the one registered in Google Cloud Console **exactly**, or Google's token
endpoint rejects the exchange with `redirect_uri_mismatch`.

## Supabase (Postgres)

Supabase **is** Postgres, so SQLx connects with a standard connection string.
The backend uses the owner/pooler DB role (bypasses RLS); the Supabase
`anon` key must never be able to read app tables (see `005_supabase_rls.sql`).

1. Create a Supabase project; note the project REF and DB password.
2. Dashboard > Database > Extensions: enable `pgcrypto` (needed for
   `gen_random_uuid()`; otherwise `001_identity.sql` fails).
3. Dashboard > Project Settings > Database: copy the connection strings.
   - API runtime (recommended): **session-mode pooler**, port `5432`
     (`...pooler.supabase.com:5432/postgres?sslmode=require`). Supports prepared
     statements, `FOR UPDATE`, transactions — what this backend uses.
   - Migrations / `pg_dump`: **direct** connection
     (`db.[REF].supabase.co:5432/postgres?sslmode=require`). Never migrate over
     the `:6543` transaction pooler.
   - Avoid the `:6543` transaction pooler for this long-lived service. If you
     must use it, set `DATABASE_STATEMENT_CACHE_SIZE=0` (auto-applied when the
     URL contains `:6543`; Supavisor Tx mode rejects prepared statements).
4. Set `DATABASE_URL` to the session-pooler URL (`?sslmode=require`).
   Keep the pool small against the shared pooler: `DATABASE_MAX_CONNECTIONS=5`,
   `DATABASE_MIN_CONNECTIONS=0`.
5. Apply migrations with the direct URL:
   ```bash
   cargo install sqlx-cli --no-default-features --features postgres-rustls
   DATABASE_URL="<direct-url>" sqlx migrate run --source services/api/migrations
   ```
   (The API also runs `sqlx::migrate!` on boot; use the direct/session URL there,
   not `:6543`.)
6. Redis is optional (Supabase has no Redis): without `REDIS_URL` the API boots
   with rate limiting disabled. To enforce limits, use Upstash or
   a local `docker run -d -p 6379:6379 redis:7-alpine` and set `REDIS_URL`.
7. Security: the Rust backend authenticates with its own JWT + owner DB role.
   Do not expose `service_role` keys to the browser; frontend calls the Rust API,
   not PostgREST, so no permissive RLS policies are provided.

## Checks (require Rust toolchain + Postgres + Redis)

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

> NOTE: scaffold was authored without `cargo`/Docker available in this environment,
> so `cargo test` / image build have NOT been run here. Run the commands above
> on a machine with Rust 1.82+, Postgres 16 and Redis 7 before deploying.

## PayFast sandbox → production

1. Merchant dashboard → Settings → set Salt Passphrase (same value as `PAYFAST_PASSPHRASE`).
2. Set `notify_url` to `https://<api>/api/v1/payments/payfast/itn` (must be public HTTPS).
3. Keep `PAYFAST_MODE=sandbox` (`sandbox.payfast.co.za`) for testing,
   `production` (`www.payfast.co.za`) only for live. Never mix.
4. Server validation posts back to `/eng/query/validate` on the same host and expects `VALID`.
5. Source check: ITN must come from `*.payfast.co.za`; keep `PAYFAST_VALIDATE_IP=true`.

Browser return URLs are informational only — only a fully validated ITN marks an order paid.

## Gmail API email delivery

1. Enable the Gmail API in the Google Cloud project.
2. Authorize a mailbox with the `https://www.googleapis.com/auth/gmail.send` scope and obtain an OAuth refresh token. The normal Google sign-in grant does not include this mail permission.
3. Set `GMAIL_CLIENT_ID`, `GMAIL_CLIENT_SECRET`, `GMAIL_REFRESH_TOKEN`, and `GMAIL_SENDER_EMAIL` in the API environment. The sender address must be the authorized Gmail mailbox or an allowed send-as alias.
4. Emails are queued in `email_outbox` in the same DB transaction as the business event,
   then sent by the `email-outbox` worker with exponential backoff + jitter.

## Manual merchant steps that cannot be automated

- PayFast live activation, passphrase, notify/return/cancel URLs, IP whitelist confirmation.
- Gmail API OAuth grant and sender mailbox configuration.
- DNS/TLS (HTTPS), CORS origins, trusted-proxy config, DB backups, secret rotation.
