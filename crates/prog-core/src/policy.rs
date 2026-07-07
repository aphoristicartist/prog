use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CachePolicy, CoreError, EffectSet, Extra, OperationProfile, Result, TrustSettings};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CallFlags {
    #[serde(default)]
    pub yes: bool,
}

pub fn check_discovery(operation: &OperationProfile) -> Result<()> {
    let effects = &operation.effects;
    if !effects.read_only {
        return Err(CoreError::DiscoveryNotReadOnly {
            operation: operation.id.clone(),
            effects: describe_effects(effects),
        });
    }
    if effects.mutating {
        return Err(CoreError::DiscoveryMutating {
            operation: operation.id.clone(),
            effects: describe_effects(effects),
        });
    }
    if effects.requires_confirmation {
        return Err(CoreError::DiscoveryRequiresConfirmation {
            operation: operation.id.clone(),
            effects: describe_effects(effects),
        });
    }
    Ok(())
}

/// Evidence strength recorded by an importer for a derived operation's
/// effects. Only `Proven` read-only evidence can relax confirmation, and only
/// when `trust.auto_upgrade` is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceGrade {
    /// Descriptor explicitly declares the effect (HTTP GET, MCP
    /// `readOnlyHint` with no `destructiveHint`, an explicit `--read-only`).
    Proven,
    /// Effect inferred from method or shape, not explicitly declared.
    Assumed,
    /// Ambiguous or absent effect information.
    Unproven,
}

impl EvidenceGrade {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proven => "proven",
            Self::Assumed => "assumed",
            Self::Unproven => "unproven",
        }
    }

    /// Read the grade from an `EffectSet`'s `extra["evidence_grade"]`, defaulting
    /// to `Unproven` when unset so legacy/ungauged operations keep their current
    /// behavior.
    pub fn from_extra(extra: &Extra) -> Self {
        match extra.get("evidence_grade").and_then(Value::as_str) {
            Some("proven") => Self::Proven,
            Some("assumed") => Self::Assumed,
            _ => Self::Unproven,
        }
    }
}

/// Apply trust auto-upgrade to an operation's effects. A *proven* read-only
/// operation on a source with `trust.auto_upgrade` has its
/// `requires_confirmation` cleared. Mutating, shell-backed, and sensitive
/// operations are never relaxed (I7 preserved); non-proven grades are never
/// relaxed. Returns the (possibly relaxed) effects and, when a change was made,
/// a human-readable note for the audit trail.
pub fn apply_auto_upgrade(
    effects: &EffectSet,
    trust: &TrustSettings,
) -> (EffectSet, Option<String>) {
    let grade = EvidenceGrade::from_extra(&effects.extra);
    if trust.auto_upgrade
        && grade == EvidenceGrade::Proven
        && effects.read_only
        && !effects.mutating
        && !effects.shell
        && !effects.sensitive
        && effects.requires_confirmation
    {
        let mut relaxed = effects.clone();
        relaxed.requires_confirmation = false;
        relaxed.extra.insert(
            "auto_upgrade".to_string(),
            Value::String("proven read-only evidence relaxed requires_confirmation".to_string()),
        );
        (
            relaxed,
            Some(
                "proven read-only evidence relaxed confirmation under trust.auto_upgrade"
                    .to_string(),
            ),
        )
    } else {
        (effects.clone(), None)
    }
}

pub fn check_call(
    operation: &OperationProfile,
    flags: CallFlags,
    trust: &TrustSettings,
) -> Result<()> {
    let effects = apply_auto_upgrade(&operation.effects, trust).0;
    if (effects.mutating || effects.requires_confirmation) && !flags.yes {
        return Err(CoreError::RequiresConfirmation {
            operation: operation.id.clone(),
            class: call_confirmation_class(&effects).to_string(),
            effects: describe_effects(&effects),
        });
    }
    if effects.shell && !trust.allow_shell {
        return Err(CoreError::ShellNotTrusted {
            operation: operation.id.clone(),
        });
    }
    Ok(())
}

pub fn cache_allowed(operation: &OperationProfile, policy: &CachePolicy) -> bool {
    policy.enabled && operation.effects.cacheable && !operation.effects.sensitive
}

pub fn call_effect_warnings(operation: &OperationProfile) -> Vec<String> {
    let effects = &operation.effects;
    let mut warnings = Vec::new();
    if effects.mutating {
        warnings.push(format!(
            "operation '{}' is mutating; execution required explicit confirmation",
            operation.id
        ));
    }
    if effects.sensitive {
        warnings.push(format!(
            "operation '{}' may handle sensitive data; persistence is disabled",
            operation.id
        ));
    }
    if !effects.cacheable {
        warnings.push(format!(
            "operation '{}' is not cacheable under its effect policy",
            operation.id
        ));
    }
    if effects.shell {
        warnings.push(format!(
            "operation '{}' is shell-backed and requires source trust",
            operation.id
        ));
    }
    warnings
}

pub fn tighten_effects(left: &EffectSet, right: &EffectSet) -> EffectSet {
    let mut extra = left.extra.clone();
    for (key, value) in &right.extra {
        extra.entry(key.clone()).or_insert_with(|| value.clone());
    }

    EffectSet {
        read_only: left.read_only && right.read_only,
        mutating: left.mutating || right.mutating,
        network: left.network || right.network,
        shell: left.shell || right.shell,
        sensitive: left.sensitive || right.sensitive,
        cacheable: left.cacheable && right.cacheable,
        requires_confirmation: left.requires_confirmation || right.requires_confirmation,
        extra,
    }
}

pub fn http_adapter_effects(method: &str) -> EffectSet {
    let read_only = method.eq_ignore_ascii_case("GET");
    if read_only {
        read_effects(true, false)
    } else {
        unsafe_effects(true, false)
    }
}

pub fn http_hardening_effects(method: &str) -> EffectSet {
    let mut effects = effect_identity();
    effects.network = true;
    if !method.eq_ignore_ascii_case("GET") {
        effects.read_only = false;
        effects.mutating = true;
        effects.cacheable = false;
        effects.requires_confirmation = true;
    }
    effects
}

pub fn cli_adapter_effects(shell: bool) -> EffectSet {
    unsafe_effects(false, shell)
}

pub fn cli_hardening_effects(shell: bool) -> EffectSet {
    let mut effects = effect_identity();
    effects.shell = shell;
    effects
}

pub fn mcp_tool_annotation_effects(
    read_only_hint: Option<bool>,
    destructive_hint: Option<bool>,
) -> EffectSet {
    let read_only = read_only_hint.unwrap_or(false) && destructive_hint != Some(true);
    if read_only {
        read_effects(false, false)
    } else {
        unsafe_effects(false, false)
    }
}

pub fn mcp_read_effects() -> EffectSet {
    read_effects(false, false)
}

pub fn effect_identity() -> EffectSet {
    EffectSet {
        read_only: true,
        mutating: false,
        network: false,
        shell: false,
        sensitive: false,
        cacheable: true,
        requires_confirmation: false,
        extra: Extra::new(),
    }
}

pub fn describe_effects(effects: &EffectSet) -> String {
    format!(
        "read_only={}, mutating={}, network={}, shell={}, sensitive={}, cacheable={}, requires_confirmation={}",
        effects.read_only,
        effects.mutating,
        effects.network,
        effects.shell,
        effects.sensitive,
        effects.cacheable,
        effects.requires_confirmation
    )
}

fn read_effects(network: bool, shell: bool) -> EffectSet {
    EffectSet {
        read_only: true,
        mutating: false,
        network,
        shell,
        sensitive: false,
        cacheable: true,
        requires_confirmation: false,
        extra: Extra::new(),
    }
}

fn unsafe_effects(network: bool, shell: bool) -> EffectSet {
    EffectSet {
        read_only: false,
        mutating: true,
        network,
        shell,
        sensitive: false,
        cacheable: false,
        requires_confirmation: true,
        extra: Extra::new(),
    }
}

fn call_confirmation_class(effects: &EffectSet) -> &'static str {
    if effects.mutating {
        "mutating"
    } else {
        "confirmation-required"
    }
}
