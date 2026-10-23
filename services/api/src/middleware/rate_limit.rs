use crate::state::SharedState;

/// Fixed-window Redis rate limit via atomic Lua INCR+EXPIRE.
///
/// Fail-closed while Redis is configured: if Redis errors, callers must return
/// 503 (never silently disable limits). If REDIS_URL is unset, limits are
/// disabled entirely (fail-open) — the service logs a warning at startup.
/// Do not run production traffic that way; set REDIS_URL.
pub async fn check(
    s: &SharedState,
    key: &str,
    limit: u64,
    window_secs: u64,
) -> anyhow::Result<bool> {
    let Some(client) = &s.redis else {
        return Ok(true);
    };
    let mut conn = client.get_multiplexed_async_connection().await?;
    let script = redis::Script::new(
        r"local c = redis.call('INCR', KEYS[1]); if c == 1 then redis.call('EXPIRE', KEYS[1], ARGV[1]); end; return c;",
    );
    let count: u64 = script
        .key(key)
        .arg(window_secs)
        .invoke_async(&mut conn)
        .await?;
    Ok(count <= limit)
}
