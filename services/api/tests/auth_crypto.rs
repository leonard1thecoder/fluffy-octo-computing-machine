use nhlanganiso_api::modules::auth::{jwt, password};

#[test]
fn argon2id_roundtrip() {
    let h = password::hash("correct-horse-123").unwrap();
    assert!(password::verify("correct-horse-123", &h));
    assert!(!password::verify("wrong", &h));
    assert!(h.starts_with("$argon2id$"));
}

#[test]
fn refresh_tokens_are_hashed_and_unique() {
    let (raw1, hash1) = jwt::new_refresh_token();
    let (raw2, hash2) = jwt::new_refresh_token();
    assert_ne!(raw1, raw2);
    assert_ne!(hash1, hash2);
    assert_eq!(hash1, jwt::hash_token(&raw1));
    // hash never contains the raw token
    assert!(!hash1.contains(&raw1));
}
