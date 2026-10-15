# Nhlanganiso Catering & Baking — Backend

[![Backend CI](https://github.com/leonard1thecoder/fluffy-octo-computing-machine/actions/workflows/ci.yml/badge.svg)](https://github.com/leonard1thecoder/fluffy-octo-computing-machine/actions/workflows/ci.yml)

Production-oriented Rust (Axum + Tokio + PostgreSQL via SQLx) modular monolith.
Frontend (Next.js) communicates via versioned REST API under `/api/v1` (see `contracts/openapi.yaml`).
Currency: ZAR (integer cents internally, `%.2f` only at PayFast boundary).

## Layout

```
nhlanganiso/
  docker-compose.yml
  contracts/openapi.yaml
  services/api/        # Rust service
```

## Quick start

```bash
cp services/api/.env.example services/api/.env
# edit secrets: JWT_SECRET (openssl rand -base64 48), DATABASE_URL, REDIS_URL,
# BREVO_API_KEY, PAYFAST_* , PUBLIC_* , CORS_ALLOWED_ORIGINS
docker compose up --build
# migrations run automatically on boot (sqlx::migrate!)
curl http://localhost:8080/api/v1/health/ready
```

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
6. Redis is still required separately (Supabase has no Redis): use Upstash or
   self-hosted Redis and set `REDIS_URL`. For compose, replace the `api`
   `DATABASE_URL`/`REDIS_URL` in `docker-compose.yml` or run the API outside
   compose with Supabase env.
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

## Brevo

1. Verify sender domain/address in Brevo dashboard.
2. Create API key (SMTP + transactional), set `BREVO_API_KEY` + sender envs.
3. Emails are queued in `email_outbox` in the same DB transaction as the business event,
   then sent by the `email-outbox` worker with exponential backoff + jitter.

## Manual merchant steps that cannot be automated

- PayFast live activation, passphrase, notify/return/cancel URLs, IP whitelist confirmation.
- Brevo sender verification + dedicated IP (if used).
- DNS/TLS (HTTPS), CORS origins, trusted-proxy config, DB backups, secret rotation.
