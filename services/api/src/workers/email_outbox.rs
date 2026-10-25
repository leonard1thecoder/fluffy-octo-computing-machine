use crate::state::SharedState;

/// Durable email outbox: rows inserted in the same DB transaction as the
/// business event; this worker sends via Gmail with bounded retries,
/// exponential backoff + jitter. A process crash never loses queued mail.
pub async fn run(state: SharedState) {
    tracing::info!("email outbox worker started");
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
    let mut empty_polls = 0_u8;
    loop {
        interval.tick().await;
        match process_batch(&state).await {
            Ok(0) => {
                empty_polls += 1;
                if empty_polls >= 6 {
                    tracing::info!("email outbox worker is running; no eligible pending emails");
                    empty_polls = 0;
                }
            }
            Ok(batch_size) => {
                empty_polls = 0;
                tracing::info!(batch_size, "email outbox batch loaded");
            }
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "outbox batch failed");
            }
        }
    }
}

async fn process_batch(state: &SharedState) -> anyhow::Result<usize> {
    let rows: Vec<(uuid::Uuid, String, String, String, i32)> = sqlx::query_as(
        "SELECT id, recipient, subject, body_html, attempts FROM email_outbox WHERE status='pending' AND next_attempt_at <= now() ORDER BY CASE WHEN subject='Reset your Nhlanganiso password' THEN 0 ELSE 1 END, CASE WHEN subject='Reset your Nhlanganiso password' THEN created_at END DESC, created_at ASC LIMIT 1",
    )
    .fetch_all(&state.db)
    .await?;
    let batch_size = rows.len();
    for (id, to, subject, html, attempts) in rows {
        tracing::info!(
            email_id = %id,
            attempt = attempts + 1,
            "outbound email delivery started"
        );
        let res = state.gmail.send(&to, &subject, &html).await;
        match res {
            Ok(provider_id) => {
                sqlx::query("UPDATE email_outbox SET status='sent', provider_message_id=$2, sent_at=now() WHERE id=$1")
                    .bind(id).bind(provider_id).execute(&state.db).await?;
                tracing::info!(email_id = %id, "outbound email delivery succeeded");
            }
            Err(e) => {
                let next_secs: i64 = 60 * 2_i64.pow(attempts.clamp(0, 6) as u32);
                let jitter = (rand::random::<i64>() % 30).abs();
                let retry_scheduled = attempts + 1 < 8;
                sqlx::query(
                    "UPDATE email_outbox SET attempts=attempts+1, next_attempt_at=now()+($2::bigint * interval '1 second'), last_error=$3, status=CASE WHEN attempts+1>=8 THEN 'failed'::email_status ELSE 'pending'::email_status END WHERE id=$1",
                )
                .bind(id).bind(next_secs + jitter).bind(format!("{e:.200}"))
                .execute(&state.db).await?;
                tracing::warn!(
                    email_id = %id,
                    attempt = attempts + 1,
                    retry_scheduled,
                    error = %format!("{e:#}"),
                    "outbound email delivery failed"
                );
            }
        }
    }
    Ok(batch_size)
}

/// Queue an email in the caller's transaction. Caller must commit.
pub async fn enqueue(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    recipient: &str,
    subject: &str,
    body_html: &str,
) -> anyhow::Result<uuid::Uuid> {
    if subject == "Reset your Nhlanganiso password" {
        let superseded = sqlx::query(
            "UPDATE email_outbox SET status='failed', last_error='Superseded by a newer password reset request' WHERE status='pending' AND subject=$1 AND recipient=$2",
        )
        .bind(subject)
        .bind(recipient)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        if superseded > 0 {
            tracing::info!(
                count = superseded,
                "older pending password reset emails superseded"
            );
        }
    }
    let id = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO email_outbox (recipient, subject, body_html) VALUES ($1,$2,$3) RETURNING id",
    )
    .bind(recipient)
    .bind(subject)
    .bind(body_html)
    .fetch_one(&mut **tx)
    .await?;
    Ok(id)
}

pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn branded_template(title: &str, inner: &str) -> String {
    // Nhlanganiso palette: cream #FAF6EE, brown #4A2E1B, burnt-orange #C2571B, gold #C9A227
    format!(
        "<html><body style=\"background:#FAF6EE;font-family:Arial,sans-serif;color:#2b2118\">\
        <div style=\"max-width:600px;margin:auto;background:#fff;border-top:6px solid #C2571B;padding:24px\">\
        <h1 style=\"color:#4A2E1B\">{title}</h1><div>{inner}</div>\
        <p style=\"color:#8a7a63;font-size:12px\">Nhlanganiso Catering &amp; Baking · <span style=\"color:#C9A227\">●</span></p>\
        </div></body></html>"
    )
}
