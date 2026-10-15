use nhlanganiso_api::integrations::payfast as pf;

#[test]
fn valid_sandbox_signature_verifies() {
    let ordered = vec![
        ("merchant_id".to_string(), "10000100".to_string()),
        ("m_payment_id".to_string(), "ORDER-1".to_string()),
        ("amount".to_string(), "65.00".to_string()),
        ("item_name".to_string(), "Order NHL-1".to_string()),
    ];
    let sig = pf::generate_signature(&ordered, Some("secret"));
    let mut with_sig = ordered.clone();
    with_sig.push(("signature".to_string(), sig.clone()));
    let expected = pf::generate_signature(&with_sig, Some("secret"));
    assert!(pf::signatures_match(&expected, &sig));
}

#[test]
fn invalid_signature_detected() {
    let ordered = vec![("merchant_id".to_string(), "10000100".to_string())];
    let sig = pf::generate_signature(&ordered, Some("secret"));
    assert!(!pf::signatures_match(&sig, "deadbeefdeadbeefdeadbeefdeadbeef"));
}

#[test]
fn amount_must_match_exactly() {
    assert!(pf::amounts_match_cents("65.00", 6500));
    assert!(!pf::amounts_match_cents("64.99", 6500));
    assert!(!pf::amounts_match_cents("not-a-number", 6500));
}

#[test]
fn cents_formatting() {
    assert_eq!(pf::cents_to_payfast(6500), "65.00");
    assert_eq!(pf::cents_to_payfast(0), "0.00");
}

#[test]
fn empty_fields_excluded_from_signature() {
    let a = vec![
        ("merchant_id".to_string(), "1".to_string()),
        ("amount".to_string(), "".to_string()),
    ];
    let b = vec![("merchant_id".to_string(), "1".to_string())];
    assert_eq!(
        pf::generate_signature(&a, Some("p")),
        pf::generate_signature(&b, Some("p"))
    );
}
