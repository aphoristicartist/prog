use prog_core::{
    CachePolicy, CallFlags, EffectSet, EvidenceGrade, Extra, OperationProfile, TrustSettings,
    cache_allowed, call_effect_warnings, check_call, check_discovery, cli_adapter_effects,
    effective_effects, http_adapter_effects, http_hardening_effects, stamp_evidence_grade,
    tighten_effects,
};
use proptest::prelude::*;
use serde_json::{Value, json};

#[test]
fn absent_effect_metadata_fails_closed_for_policy_checks() {
    let operation: OperationProfile = serde_json::from_value(json!({"id": "raw"})).unwrap();

    assert!(!operation.effects.read_only);
    assert!(operation.effects.mutating);
    assert!(operation.effects.shell);
    assert!(operation.effects.sensitive);
    assert!(!operation.effects.cacheable);
    assert!(operation.effects.requires_confirmation);

    assert_eq!(
        check_discovery(&operation, &TrustSettings::default())
            .unwrap_err()
            .kind(),
        "discovery_not_read_only"
    );
    assert_eq!(
        check_call(
            &operation,
            CallFlags { yes: false },
            &TrustSettings::default()
        )
        .unwrap_err()
        .kind(),
        "requires_confirmation"
    );
    assert_eq!(
        check_call(
            &operation,
            CallFlags { yes: true },
            &TrustSettings::default()
        )
        .unwrap_err()
        .kind(),
        "shell_not_trusted"
    );
}

#[test]
fn discovery_refuses_each_unsafe_effect_independently() {
    let not_read_only = operation(
        "not_read_only",
        EffectSet {
            read_only: false,
            mutating: false,
            network: false,
            shell: false,
            sensitive: false,
            cacheable: true,
            requires_confirmation: false,
            extra: Extra::new(),
        },
    );
    assert_eq!(
        check_discovery(&not_read_only, &TrustSettings::default())
            .unwrap_err()
            .kind(),
        "discovery_not_read_only"
    );

    let mutating = operation(
        "mutating",
        EffectSet {
            read_only: true,
            mutating: true,
            network: false,
            shell: false,
            sensitive: false,
            cacheable: false,
            requires_confirmation: false,
            extra: Extra::new(),
        },
    );
    assert_eq!(
        check_discovery(&mutating, &TrustSettings::default())
            .unwrap_err()
            .kind(),
        "discovery_mutating"
    );

    let confirmation = operation(
        "confirmation",
        EffectSet {
            read_only: true,
            mutating: false,
            network: false,
            shell: false,
            sensitive: false,
            cacheable: true,
            requires_confirmation: true,
            extra: Extra::new(),
        },
    );
    assert_eq!(
        check_discovery(&confirmation, &TrustSettings::default())
            .unwrap_err()
            .kind(),
        "discovery_requires_confirmation"
    );
}

#[test]
fn call_policy_requires_confirmation_and_shell_trust() {
    let mutating = operation(
        "write",
        EffectSet {
            read_only: false,
            mutating: true,
            network: true,
            shell: false,
            sensitive: false,
            cacheable: false,
            requires_confirmation: true,
            extra: Extra::new(),
        },
    );
    let error = check_call(
        &mutating,
        CallFlags { yes: false },
        &TrustSettings::default(),
    )
    .unwrap_err();
    assert_eq!(error.kind(), "requires_confirmation");
    assert!(error.to_string().contains("write"));
    assert!(error.hint().contains("--yes"));

    let shell = operation(
        "shell",
        EffectSet {
            read_only: true,
            mutating: false,
            network: false,
            shell: true,
            sensitive: false,
            cacheable: true,
            requires_confirmation: false,
            extra: Extra::new(),
        },
    );
    assert_eq!(
        check_call(&shell, CallFlags { yes: true }, &TrustSettings::default())
            .unwrap_err()
            .kind(),
        "shell_not_trusted"
    );
    assert!(
        check_call(
            &shell,
            CallFlags { yes: true },
            &TrustSettings {
                allow_shell: true,
                ..TrustSettings::default()
            }
        )
        .is_ok()
    );
}

#[test]
fn cache_policy_respects_enabled_cacheable_and_sensitive_flags() {
    let mut operation = operation(
        "read",
        EffectSet {
            read_only: true,
            mutating: false,
            network: false,
            shell: false,
            sensitive: false,
            cacheable: true,
            requires_confirmation: false,
            extra: Extra::new(),
        },
    );
    assert!(!cache_allowed(&operation, &CachePolicy::default()));

    let enabled = CachePolicy {
        enabled: true,
        ..CachePolicy::default()
    };
    assert!(cache_allowed(&operation, &enabled));

    operation.effects.sensitive = true;
    assert!(!cache_allowed(&operation, &enabled));

    operation.effects.sensitive = false;
    operation.effects.cacheable = false;
    assert!(!cache_allowed(&operation, &enabled));
}

#[test]
fn call_effect_warnings_surface_agent_visible_risks() {
    let operation = operation(
        "danger",
        EffectSet {
            read_only: false,
            mutating: true,
            network: true,
            shell: true,
            sensitive: true,
            cacheable: false,
            requires_confirmation: true,
            extra: Extra::new(),
        },
    );

    let warnings = call_effect_warnings(&operation);

    assert!(warnings.iter().any(|warning| warning.contains("mutating")));
    assert!(warnings.iter().any(|warning| warning.contains("sensitive")));
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("not cacheable"))
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("shell-backed"))
    );
}

#[test]
fn adapter_defaults_encode_conservative_source_facts() {
    let get = http_adapter_effects("GET");
    assert!(get.read_only);
    assert!(get.network);
    assert!(!get.shell);
    assert!(get.cacheable);

    let post = http_adapter_effects("POST");
    assert!(!post.read_only);
    assert!(post.mutating);
    assert!(post.network);
    assert!(post.requires_confirmation);

    let hard_post = http_hardening_effects("POST");
    let claimed_safe = EffectSet {
        read_only: true,
        mutating: false,
        network: false,
        shell: false,
        sensitive: false,
        cacheable: true,
        requires_confirmation: false,
        extra: Extra::new(),
    };
    let tightened = tighten_effects(&claimed_safe, &hard_post);
    assert!(!tightened.read_only);
    assert!(tightened.mutating);
    assert!(tightened.network);

    let cli = cli_adapter_effects(false);
    assert!(!cli.read_only);
    assert!(cli.mutating);
    assert!(!cli.network);
    assert!(!cli.shell);

    let shell_cli = cli_adapter_effects(true);
    assert!(shell_cli.shell);
}

#[test]
fn auto_upgrade_relaxes_proven_read_only_confirmation() {
    let mut effects = gated_read_only();
    effects
        .extra
        .insert("evidence_grade".to_string(), json!("proven"));
    let (relaxed, note) = effective_effects(&effects, &TrustSettings::default());
    assert!(!relaxed.requires_confirmation);
    assert!(note.is_some());
    assert_eq!(
        relaxed.extra.get("auto_upgrade").and_then(Value::as_str),
        Some("proven read-only evidence relaxed requires_confirmation")
    );
    // The grade is preserved on the way out.
    assert_eq!(
        EvidenceGrade::from_extra(&relaxed.extra),
        EvidenceGrade::Proven
    );
}

#[test]
fn auto_upgrade_never_relaxes_non_proven_or_mutating_or_shell_or_sensitive() {
    let trust = TrustSettings::default();
    // Assumed grade -> not relaxed.
    let mut assumed = gated_read_only();
    assumed
        .extra
        .insert("evidence_grade".to_string(), json!("assumed"));
    assert!(effective_effects(&assumed, &trust).0.requires_confirmation);
    // Unproven (no grade at all) -> not relaxed.
    assert!(
        effective_effects(&gated_read_only(), &trust)
            .0
            .requires_confirmation
    );

    let mut mutating = gated_read_only();
    mutating
        .extra
        .insert("evidence_grade".to_string(), json!("proven"));
    mutating.read_only = false;
    mutating.mutating = true;
    assert!(effective_effects(&mutating, &trust).0.requires_confirmation);

    let mut shell = gated_read_only();
    shell
        .extra
        .insert("evidence_grade".to_string(), json!("proven"));
    shell.shell = true;
    assert!(effective_effects(&shell, &trust).0.requires_confirmation);

    let mut sensitive = gated_read_only();
    sensitive
        .extra
        .insert("evidence_grade".to_string(), json!("proven"));
    sensitive.sensitive = true;
    assert!(
        effective_effects(&sensitive, &trust)
            .0
            .requires_confirmation
    );
}

#[test]
fn auto_upgrade_disabled_leaves_confirmation_in_place() {
    let mut effects = gated_read_only();
    effects
        .extra
        .insert("evidence_grade".to_string(), json!("proven"));
    let trust = TrustSettings {
        auto_upgrade: false,
        ..TrustSettings::default()
    };
    let (relaxed, note) = effective_effects(&effects, &trust);
    assert!(relaxed.requires_confirmation);
    assert!(note.is_none());
}

#[test]
fn check_call_skips_confirmation_for_proven_read_only_under_auto_upgrade() {
    let mut effects = gated_read_only();
    effects
        .extra
        .insert("evidence_grade".to_string(), json!("proven"));
    let operation = operation_with_effects(effects);
    assert!(
        check_call(
            &operation,
            CallFlags { yes: false },
            &TrustSettings::default()
        )
        .is_ok()
    );
}

#[test]
fn check_call_still_requires_confirmation_without_proven_grade() {
    let operation = operation_with_effects(gated_read_only());
    assert!(
        check_call(
            &operation,
            CallFlags { yes: false },
            &TrustSettings::default()
        )
        .is_err()
    );
}

#[test]
fn auto_upgrade_escape_hatch_re_gates_proven_read_only() {
    // trust.auto_upgrade=false is the per-source escape hatch to keep strict V1
    // behavior: even a Proven read-only op stays gated for both call and
    // discovery.
    let mut effects = gated_read_only();
    stamp_evidence_grade(&mut effects, EvidenceGrade::Proven);
    let operation = operation_with_effects(effects);
    let strict = TrustSettings {
        auto_upgrade: false,
        ..TrustSettings::default()
    };
    // Call requires --yes again.
    assert!(check_call(&operation, CallFlags { yes: false }, &strict).is_err());
    // Discovery refuses (I6 skip fires), where it would probe under default trust.
    assert_eq!(
        check_discovery(&operation, &strict).unwrap_err().kind(),
        "discovery_requires_confirmation"
    );
    // Under default trust the same op is probeable and callable without --yes.
    assert!(check_discovery(&operation, &TrustSettings::default()).is_ok());
    assert!(
        check_call(
            &operation,
            CallFlags { yes: false },
            &TrustSettings::default()
        )
        .is_ok()
    );
}

#[test]
fn check_call_surfaces_effective_effects_and_audit_for_proven_upgrade() {
    let mut effects = gated_read_only();
    stamp_evidence_grade(&mut effects, EvidenceGrade::Proven);
    let operation = operation_with_effects(effects);
    let (effective, audit) = check_call(
        &operation,
        CallFlags { yes: false },
        &TrustSettings::default(),
    )
    .expect("proven read-only call succeeds under default trust");
    assert!(!effective.requires_confirmation);
    assert!(audit.is_some());
    // The relaxed set carries its own auto_upgrade stamp.
    assert!(effective.extra.get("auto_upgrade").is_some());
}

proptest! {
    /// Law: effective_effects relaxes requires_confirmation IFF
    /// (auto_upgrade && grade==proven && read_only && !mutating && !shell &&
    /// !sensitive). Otherwise the returned set equals the input on every flag,
    /// and a relaxation differs ONLY in requires_confirmation.
    #[test]
    fn effective_effects_relaxes_only_proven_read_only_under_auto_upgrade(
        auto_upgrade in any::<bool>(),
        grade_kind in 0u8..3,
        read_only in any::<bool>(),
        mutating in any::<bool>(),
        shell in any::<bool>(),
        sensitive in any::<bool>(),
        requires_confirmation in any::<bool>(),
    ) {
        // Vary EVERY flag of the EffectSet so the full relaxation condition is
        // actually pinned (not just auto_upgrade && grade).
        let mut effects = gated_read_only();
        effects.read_only = read_only;
        effects.mutating = mutating;
        effects.shell = shell;
        effects.sensitive = sensitive;
        effects.requires_confirmation = requires_confirmation;
        let grade = match grade_kind {
            0 => EvidenceGrade::Proven,
            1 => EvidenceGrade::Assumed,
            _ => EvidenceGrade::Unproven,
        };
        stamp_evidence_grade(&mut effects, grade);
        let trust = TrustSettings { auto_upgrade, ..TrustSettings::default() };
        let (out, note) = effective_effects(&effects, &trust);
        let should_relax = auto_upgrade
            && grade == EvidenceGrade::Proven
            && effects.read_only
            && !effects.mutating
            && !effects.shell
            && !effects.sensitive
            && effects.requires_confirmation;
        prop_assert_eq!(note.is_some(), should_relax);
        // Relaxation clears requires_confirmation; otherwise it is unchanged.
        prop_assert_eq!(
            out.requires_confirmation,
            if should_relax {
                false
            } else {
                effects.requires_confirmation
            }
        );
        // No other flag ever moves (mutating/shell/sensitive can NEVER relax).
        prop_assert_eq!(out.read_only, effects.read_only);
        prop_assert_eq!(out.mutating, effects.mutating);
        prop_assert_eq!(out.shell, effects.shell);
        prop_assert_eq!(out.sensitive, effects.sensitive);
        prop_assert_eq!(out.cacheable, effects.cacheable);
        // Idempotent and never tightens: re-applying is a fixed point.
        let (again, _) = effective_effects(&out, &trust);
        prop_assert_eq!(again.requires_confirmation, out.requires_confirmation);
    }
}

fn gated_read_only() -> EffectSet {
    EffectSet {
        read_only: true,
        mutating: false,
        network: true,
        shell: false,
        sensitive: false,
        cacheable: true,
        requires_confirmation: true,
        extra: Extra::new(),
    }
}

fn operation_with_effects(effects: EffectSet) -> OperationProfile {
    OperationProfile {
        id: "op".to_string(),
        description: None,
        input_schema: Value::Null,
        output_shape: None,
        declared_output_schema: None,
        effects,
        cache: CachePolicy::default(),
        pagination: None,
        extra: Extra::new(),
    }
}

proptest! {
    #[test]
    fn tightening_is_monotone(left in arb_effects(), right in arb_effects()) {
        let tightened = tighten_effects(&left, &right);

        prop_assert_eq!(tightened.read_only, left.read_only && right.read_only);
        prop_assert_eq!(tightened.mutating, left.mutating || right.mutating);
        prop_assert_eq!(tightened.network, left.network || right.network);
        prop_assert_eq!(tightened.shell, left.shell || right.shell);
        prop_assert_eq!(tightened.sensitive, left.sensitive || right.sensitive);
        prop_assert_eq!(tightened.cacheable, left.cacheable && right.cacheable);
        prop_assert_eq!(
            tightened.requires_confirmation,
            left.requires_confirmation || right.requires_confirmation
        );
    }
}

fn arb_effects() -> impl Strategy<Value = EffectSet> {
    (
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(read_only, mutating, network, shell, sensitive, cacheable, requires_confirmation)| {
                EffectSet {
                    read_only,
                    mutating,
                    network,
                    shell,
                    sensitive,
                    cacheable,
                    requires_confirmation,
                    extra: Extra::new(),
                }
            },
        )
}

fn operation(id: &str, effects: EffectSet) -> OperationProfile {
    OperationProfile {
        id: id.to_string(),
        description: None,
        input_schema: Value::Null,
        output_shape: None,
        declared_output_schema: None,
        effects,
        cache: CachePolicy::default(),
        pagination: None,
        extra: Extra::new(),
    }
}
