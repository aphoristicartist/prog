use prog_core::{CoreError, ObligationDeclarer, Store, VerificationObligation};
use serde_json::json;

fn declaration() -> VerificationObligation {
    serde_json::from_value(json!({
        "schema": "prog.verification", "id": "check", "session_id": "session-1",
        "required": true, "intended_check": "verify the full suite", "required_scope": "full-suite",
        "expected_operation": {"argv": ["cargo", "test", "--workspace"]},
        "created_at": "2026-09-05T00:00:00Z"
    }))
    .unwrap()
}

#[test]
fn obligation_metadata_is_redacted_before_storage_and_returned_safely() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut input = declaration();
    input.intended_check = "Verify password=PROG_SYNTHETIC_DESCRIPTION".to_string();
    input
        .extra
        .insert("api_key".to_string(), json!("PROG_SYNTHETIC_EXTRA"));
    input.advisory_actions = serde_json::from_value(json!([{
        "kind": "verify", "reason": "password=PROG_SYNTHETIC_REASON",
        "argv": ["cargo", "test"], "exactness": "exact",
        "api_key": "PROG_SYNTHETIC_ACTION_EXTRA"
    }]))
    .unwrap();
    let safe = store.put_obligation(&input).unwrap();
    assert_eq!(safe.session_id, input.session_id);
    assert_eq!(safe.expected_operation, input.expected_operation);
    assert_eq!(safe.required_scope, input.required_scope);
    assert_eq!(
        safe.advisory_actions[0].argv,
        input.advisory_actions[0].argv
    );
    assert!(safe.intended_check.contains("[REDACTED:"));
    assert!(
        !serde_json::to_string(&safe)
            .unwrap()
            .contains("PROG_SYNTHETIC")
    );
    let second_dir = tempfile::tempdir().unwrap();
    assert_eq!(
        Store::open(second_dir.path())
            .unwrap()
            .put_obligation(&safe)
            .unwrap(),
        safe
    );
    // Immutable safe declarations cannot be overwritten, even by their original input.
    assert!(store.put_obligation(&input).is_err());
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(
        reopened.get_obligation("session-1", "check").unwrap(),
        Some(safe)
    );
    assert!(
        !serde_json::to_string(&reopened.list_obligations(Some("session-1")).unwrap())
            .unwrap()
            .contains("PROG_SYNTHETIC")
    );
    let disk = std::fs::read(dir.path().join("cache/data.redb")).unwrap();
    assert!(!String::from_utf8_lossy(&disk).contains("PROG_SYNTHETIC"));
}

#[test]
fn obligation_semantic_fields_reject_secrets_without_persisting_or_echoing_them() {
    let base = serde_json::to_value(declaration()).unwrap();
    for (path, value) in [
        (
            "/expected_operation",
            json!({"argv":["env", "password=PROG_SYNTHETIC_ARG"]}),
        ),
        (
            "/expected_operation",
            json!({"argv":["tool", "--password", "PROG_SYNTHETIC_FLAG"]}),
        ),
        (
            "/expected_operation",
            json!({"source_operation":"password=PROG_SYNTHETIC_OPERATION"}),
        ),
        ("/id", json!("password=PROG_SYNTHETIC_ID")),
        ("/session_id", json!("password=PROG_SYNTHETIC_SESSION")),
        ("/required_scope", json!("password=PROG_SYNTHETIC_SCOPE")),
        (
            "/comparison_family",
            json!("password=PROG_SYNTHETIC_FAMILY"),
        ),
        (
            "/origin_observation_id",
            json!("password=PROG_SYNTHETIC_ORIGIN"),
        ),
        (
            "/evidence_observation_id",
            json!("password=PROG_SYNTHETIC_EVIDENCE"),
        ),
        (
            "/expected_absent_fingerprint",
            json!("password=PROG_SYNTHETIC_FINGERPRINT"),
        ),
        ("/created_at", json!("password=PROG_SYNTHETIC_TIME")),
        (
            "/advisory_actions",
            json!([{"kind":"verify", "argv":["env", "password=PROG_SYNTHETIC_ADVISORY"]}]),
        ),
        (
            "/advisory_actions",
            json!([{"kind":"verify", "cwd":"password=PROG_SYNTHETIC_CWD"}]),
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let mut input = base.clone();
        input[path.trim_start_matches('/')] = value;
        let obligation: VerificationObligation = serde_json::from_value(input).unwrap();
        let error = store.put_obligation(&obligation).expect_err(path);
        assert!(
            matches!(error, CoreError::BadArgs { .. }),
            "{path}: {error}"
        );
        assert!(!error.to_string().contains("PROG_SYNTHETIC"));
        assert!(
            store
                .list_obligations(Some(&obligation.session_id))
                .unwrap()
                .obligations
                .is_empty()
        );
        let disk = std::fs::read(dir.path().join("cache/data.redb")).unwrap();
        assert!(
            !String::from_utf8_lossy(&disk).contains("PROG_SYNTHETIC"),
            "{path}"
        );
    }
}

#[test]
fn safe_obligation_declarations_preserve_authority_and_exact_operations() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut obligation = declaration();
    obligation.comparison_family = Some("cargo-suite".to_string());
    assert_eq!(store.put_obligation(&obligation).unwrap(), obligation);
    for declarer in [
        ObligationDeclarer::Recipe,
        ObligationDeclarer::Normalizer,
        ObligationDeclarer::Harness,
    ] {
        obligation.id = format!("generated-{declarer:?}");
        obligation.declared_by = declarer;
        assert!(store.put_obligation(&obligation).is_err());
        obligation.required = false;
        assert_eq!(store.put_obligation(&obligation).unwrap(), obligation);
        obligation.required = true;
    }
}

#[test]
fn attaching_receipts_cannot_bypass_obligation_metadata_redaction() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let obligation = declaration();
    store.put_obligation(&obligation).unwrap();
    for (evidence, receipt) in [
        (Some("password=PROG_SYNTHETIC_EVIDENCE"), "receipt-1"),
        (Some("observation-1"), "password=PROG_SYNTHETIC_RECEIPT"),
    ] {
        let error = store
            .attach_readback_receipt("session-1", "check", evidence, receipt)
            .unwrap_err();
        assert!(!error.to_string().contains("PROG_SYNTHETIC"));
        assert_eq!(
            store.get_obligation("session-1", "check").unwrap(),
            Some(obligation.clone())
        );
    }
    let safe = store
        .attach_readback_receipt("session-1", "check", Some("observation-1"), "receipt-1")
        .unwrap();
    assert_eq!(
        safe.evidence_observation_id.as_deref(),
        Some("observation-1")
    );
    assert_eq!(safe.extra["readback_receipt_id"], "receipt-1");
    let disk = std::fs::read(dir.path().join("cache/data.redb")).unwrap();
    assert!(!String::from_utf8_lossy(&disk).contains("PROG_SYNTHETIC"));
}
