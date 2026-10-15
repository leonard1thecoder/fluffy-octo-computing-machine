use crate::state::SharedState;

/// Durable email outbox: rows inserted in the same DB transaction as the
/// business event; this worker sends via Brevo with bounded retries,
/// exponential backoff + jitter. A process crash never loses queued mail.
pub async fn run(state: SharedState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
    loop {
        interval.tick().await;
        let _ = process_batch(&state).await.map_err(|e| tracing::warn!(error=%e, "outbox batch failed"));
    }
}

async fn process_batch(state: &SharedState) -> anyhow::Result<()> {
    let rows: Vec<(uuid::Uuid, String, String, String, i32)> = sqlx::query_as(
        "SELECT id, recipient, subject, body_html, attempts FROM email_outbox WHERE status='pending' AND next_attempt_at <= now() ORDER BY created_at LIMIT 20",
    )
    .fetch_all(&state.db)
    .await?;
    for (id, to, subject, html, attempts) in rows {
        let res = state.brevo.send(&to, &subject, &html).await;
        match res {
            Ok(provider_id) => {
                sqlx::query("UPDATE email_outbox SET status='sent', provider_message_id=$2, sent_at=now() WHERE id=$1")
                    .bind(id).bind(provider_id).execute(&state.db).await?;
            }
            Err(e) => {
                let next_secs: i64 = 60 * 2_i64.pow(attempts.clamp(0, 6) as u32);
                let jitter = (rand::random::<i64>() % 30).abs();
                sqlx::query(
                    "UPDATE email_outbox SET attempts=attempts+1, next_attempt_at=now()+($2::bigint * interval '1 second'), last_error=$3, status=CASE WHEN attempts+1>=8 THEN 'failed' ELSE 'pending' END WHERE id=$1",
                )
                .bind(id).bind(next_secs + jitter).bind(format!("{e:.200}"))
                .execute(&state.db).await?;
            }
        }
    }
    Ok(())
}

/// Queue an email in the caller's transaction. Caller must commit.
pub async fn enqueue(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    recipient: &str,
    subject: &str,
    body_html: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO email_outbox (recipient, subject, body_html) VALUES ($1,$2,$3)",
    )
    .bind(recipient).bind(subject).bind(body_html)
    .execute(&mut **tx).await?;
    Ok(())
}

pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
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
