//! Operator-supplied startup values and set-operation restrictions.
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{catalog::Target, policy::WritePolicy};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorConfig {
    pub write_policy: WritePolicy,
    #[serde(default)]
    pub initial_values: Vec<InitialValue>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InitialValue {
    pub target: Target,
    pub port: Option<u16>,
    pub command: String,
    pub value: String,
}

impl OperatorConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("reading configuration {}", path.display()))?;
        serde_json::from_str(&contents)
            .with_context(|| format!("parsing configuration {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn example_configuration_is_valid_and_has_no_startup_writes() {
        let config: OperatorConfig =
            serde_json::from_str(include_str!("../digilock-config.example.json")).unwrap();
        config
            .write_policy
            .validate(&crate::catalog::load().unwrap())
            .unwrap();
        assert!(config.initial_values.is_empty());
    }

    #[test]
    fn policy_is_required_and_initial_values_are_optional() {
        assert!(serde_json::from_value::<OperatorConfig>(json!({})).is_err());
        let config: OperatorConfig = serde_json::from_value(json!({
            "write_policy": {"mode": "allowlist"}
        }))
        .unwrap();
        assert!(config.initial_values.is_empty());
    }

    #[test]
    fn unknown_fields_and_nonstring_initial_values_are_rejected() {
        for entry in [
            json!({"target":"dui", "command":"pid1:proportional", "value":1}),
            json!({"target":"dui", "command":"pid1:proportional", "value":"1", "typo":true}),
        ] {
            assert!(
                serde_json::from_value::<OperatorConfig>(json!({
                    "write_policy":{"mode":"allowlist"}, "initial_values":[entry]
                }))
                .is_err()
            );
        }
        assert!(
            serde_json::from_value::<OperatorConfig>(json!({
                "write_policy":{"mode":"allowlist"}, "initial_value":[]
            }))
            .is_err()
        );
    }
}
