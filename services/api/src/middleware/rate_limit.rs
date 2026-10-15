use crate::state::SharedState;

/// Fixed-window Redis rate limit via atomic Lua INCR+EXPIRE.
/// Fail-closed: if Redis is unavailable, callers must return 503 (never silently disable limits).
pub async fn check(
    s: &SharedState,
    key: &str,
    limit: u64,
    window_secs: u64,
) -> anyhow::Result<bool> {
    let mut conn = s.redis.get_multiplexed_async_connection().await?;
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
