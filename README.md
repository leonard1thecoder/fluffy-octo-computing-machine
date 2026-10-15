# Nhlanganiso Catering & Baking — Backend

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
