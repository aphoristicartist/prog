use crate::*;
use prog_core::{RouteAssessment, RoutePolicy, classify_route};

pub(crate) fn route_command(args: &RouteArgs) -> Result<RouteAssessment> {
    let policy = match &args.policy {
        Some(path) => {
            let bytes = std::fs::read(path)?;
            let policy: RoutePolicy =
                serde_json::from_slice(&bytes).map_err(|error| CoreError::BadArgs {
                    operation: "route --policy".to_string(),
                    reason: format!("invalid RoutePolicy JSON: {error}"),
                })?;
            validate_policy(&policy)?;
            policy
        }
        None => RoutePolicy::default(),
    };
    Ok(classify_route(&args.command, &policy))
}

fn validate_policy(policy: &RoutePolicy) -> Result<()> {
    let mut ids = BTreeSet::new();
    let mut commands = BTreeSet::new();
    for rule in &policy.rules {
        if rule.id.trim().is_empty() {
            return Err(CoreError::BadArgs {
                operation: "route --policy".to_string(),
                reason: "route rule ids must not be empty".to_string(),
            });
        }
        if rule.argv.is_empty() {
            return Err(CoreError::BadArgs {
                operation: "route --policy".to_string(),
                reason: format!("route rule '{}' must declare a non-empty argv", rule.id),
            });
        }
        if !ids.insert(rule.id.as_str()) {
            return Err(CoreError::BadArgs {
                operation: "route --policy".to_string(),
                reason: format!("duplicate route rule id '{}'", rule.id),
            });
        }
        if !commands.insert(&rule.argv) {
            return Err(CoreError::BadArgs {
                operation: "route --policy".to_string(),
                reason: "multiple route rules declare the same exact argv".to_string(),
            });
        }
    }
    Ok(())
}
