use subtle::ConstantTimeEq;

/// PayFast signature (official docs, developers.payfast.co.za):
/// 1. Drop `signature`, drop empty values (after trim).
/// 2. For hosted checkout: iterate fields in insertion order (caller must pass
///    documented order: merchant_id, merchant_key, return_url, cancel_url,
///    notify_url, name_first, ..., amount, item_name, m_payment_id, ...).
///    For ITN verification: iterate in received POST order.
/// 3. `key=urlencode(value)` joined with `&`, spaces as `+`, uppercase %XX.
/// 4. Append `&passphrase=urlencode(passphrase)` if configured.
/// 5. Lowercase MD5 hex.
pub fn build_param_string(ordered: &[(String, String)], passphrase: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (k, v) in ordered {
        if k == "signature" {
            continue;
        }
        let t = v.trim();
        if t.is_empty() {
            continue;
        }
        parts.push(format!("{k}={}", payfast_encode(t)));
    }
    let mut s = parts.join("&");
    if let Some(p) = passphrase.map(str::trim).filter(|p| !p.is_empty()) {
        if !s.is_empty() {
            s.push('&');
        }
        s.push_str(&format!("passphrase={}", payfast_encode(p)));
    }
    s
}

pub fn generate_signature(ordered: &[(String, String)], passphrase: Option<&str>) -> String {
    let s = build_param_string(ordered, passphrase);
    format!("{:x}", md5::compute(s.as_bytes()))
}

pub fn signatures_match(expected: &str, received: &str) -> bool {
    expected.as_bytes().ct_eq(received.as_bytes()).into()
}

/// PHP-compatible urlencode: urlencoding::encode uses %20 for space and
/// lowercase hex; PayFast expects `+` for space and UPPERCASE hex.
fn payfast_encode(v: &str) -> String {
    let enc = urlencoding::encode(v.trim());
    let mut out = String::with_capacity(enc.len());
    // urlencoding already percent-encodes; fix spaces + uppercase hex
    let tmp = enc.replace("%20", "+");
    let bytes = tmp.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            out.push('%');
            out.push((bytes[i + 1] as char).to_ascii_uppercase());
            out.push((bytes[i + 2] as char).to_ascii_uppercase());
            i += 3;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Amounts: internal minor units (cents) -> PayFast `%.2f` string.
pub fn cents_to_payfast(cents: i64) -> String {
    format!("{:.2}", cents as f64 / 100.0)
}

/// Exact comparison: parse reported gross to cents, compare with expected.
pub fn amounts_match_cents(reported_gross: &str, expected_cents: i64) -> bool {
    let parsed = reported_gross.trim().parse::<f64>().unwrap_or(f64::NAN);
    if !parsed.is_finite() {
        return false;
    }
    (parsed * 100.0).round() as i64 == expected_cents
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn passphrase_appended_and_empty_skipped() {
        let ordered = vec![
            ("merchant_id".into(), "10000100".into()),
            ("amount".into(), "".into()),
            ("item_name".into(), "Test".into()),
        ];
        let s = build_param_string(&ordered, Some("secret"));
        assert_eq!(s, "merchant_id=10000100&item_name=Test&passphrase=secret");
    }
    #[test]
    fn cents_format() {
        assert_eq!(cents_to_payfast(6500), "65.00");
        assert!(amounts_match_cents("65.00", 6500));
        assert!(!amounts_match_cents("64.99", 6500));
    }
}
