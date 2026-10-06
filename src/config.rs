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
    use crate::{catalog, policy::Mode};
    use serde_json::json;
    use std::collections::BTreeSet;

    fn ta_pro_config() -> OperatorConfig {
        let config: OperatorConfig =
            serde_json::from_str(include_str!("../digilock-config.ta-pro-lock-in.json")).unwrap();
        config
            .write_policy
            .validate(&catalog::load().unwrap())
            .unwrap();
        config
    }

    fn command_aliases(command: &str) -> [String; 2] {
        [
            command.to_owned(),
            command.replace(' ', "").to_ascii_uppercase(),
        ]
    }

    #[test]
    fn ta_pro_configuration_is_allowlisted_with_complete_writable_dui_limits() {
        let config = ta_pro_config();
        let policy = &config.write_policy;
        let catalog = catalog::load().unwrap();
        assert_eq!(policy.mode, Mode::Allowlist);
        assert!(config.initial_values.is_empty());
        assert!(!policy.commands.is_empty());
        for (target, command) in policy
            .commands
            .iter()
            .map(|key| (key.target, key.command.as_str()))
            .chain(
                policy
                    .limits
                    .iter()
                    .map(|limit| (limit.target, limit.command.as_str())),
            )
        {
            assert_eq!(target, Target::Dui, "{command}");
            assert!(
                catalog::lookup(&catalog, target, command).unwrap().set,
                "{command}"
            );
        }
        let commands: BTreeSet<_> = policy.commands.iter().map(|key| &key.command).collect();
        let limits: BTreeSet<_> = policy.limits.iter().map(|limit| &limit.command).collect();
        assert_eq!(commands, limits);
    }

    #[test]
    fn ta_pro_numeric_limits_accept_endpoints_and_reject_outside_and_nonfinite() {
        let policy = ta_pro_config().write_policy;
        for limit in &policy.limits {
            if limit.min.is_none() && limit.max.is_none() {
                continue;
            }
            for name in command_aliases(&limit.command) {
                for bound in [limit.min, limit.max].into_iter().flatten() {
                    let value = bound.to_string();
                    assert!(
                        policy.check(limit.target, &name, &value).is_ok(),
                        "{name}={value}"
                    );
                }
                for outside in [
                    limit.min.map(|min| min - 1.0),
                    limit.max.map(|max| max + 1.0),
                ]
                .into_iter()
                .flatten()
                {
                    let value = outside.to_string();
                    assert!(
                        policy.check(limit.target, &name, &value).is_err(),
                        "{name}={value}"
                    );
                }
                for value in ["NaN", "inf", "-inf", "1e999"] {
                    assert!(
                        policy.check(limit.target, &name, value).is_err(),
                        "{name}={value}"
                    );
                }
            }
        }
    }

    #[test]
    fn ta_pro_allowed_values_are_accepted_and_matched_exactly() {
        let policy = ta_pro_config().write_policy;
        for limit in &policy.limits {
            let Some(values) = &limit.allowed_values else {
                continue;
            };
            for name in command_aliases(&limit.command) {
                for value in values {
                    assert!(
                        policy.check(limit.target, &name, value).is_ok(),
                        "{name}={value}"
                    );
                    for forbidden in [
                        value.to_ascii_uppercase(),
                        format!(" {value}"),
                        format!("{value} "),
                    ] {
                        assert!(
                            policy.check(limit.target, &name, &forbidden).is_err(),
                            "{name}={forbidden:?}"
                        );
                    }
                }
                assert!(
                    policy
                        .check(limit.target, &name, "not an allowed value")
                        .is_err(),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn ta_pro_routes_outputs_and_inputs_only_to_the_intended_channels() {
        let policy = ta_pro_config().write_policy;
        for (command, expected) in [
            ("scan:output", "scan out"),
            ("offset:output", "scan out"),
            ("pid2:output", "scan out"),
            ("li:modulation:output", "main out"),
            ("pid1:output", "main out"),
            ("li:input", "main in"),
            ("autolock:input", "li out"),
            ("pid1:input", "li out"),
            ("pid2:input", "li out"),
            ("autolock:spectrum", "main in"),
        ] {
            let limit = policy
                .limits
                .iter()
                .find(|limit| limit.command == command)
                .unwrap();
            assert_eq!(limit.allowed_values.as_deref().unwrap(), &[expected]);
            for name in command_aliases(command) {
                for value in [
                    "scan out", "main out", "aio1 out", "aio2 out", "main in", "li out", "pdh out",
                ] {
                    assert_eq!(
                        policy.check(Target::Dui, &name, value).is_ok(),
                        value == expected,
                        "{name}={value}"
                    );
                }
            }
        }
    }

    #[test]
    fn ta_pro_guards_cannot_be_disabled_and_relock_cannot_be_enabled() {
        let policy = ta_pro_config().write_policy;
        for (command, allowed, denied) in [
            ("pid1:limit enable", "true", "false"),
            ("pid2:limit:enable", "true", "false"),
            ("pid1:integral:cutoff:enable", "true", "false"),
            ("pid1:turnoff reset:manual", "true", "false"),
            ("pid1:turnoff reset:autolock", "true", "false"),
            ("pid2:turnoff reset:manual", "true", "false"),
            ("pid2:turnoff reset:autolock", "true", "false"),
            ("autolock:relock:enable", "false", "true"),
            ("pid1:relock:enable", "false", "true"),
            ("pid2:relock:enable", "false", "true"),
            ("autolock:controller:analog", "false", "true"),
        ] {
            for name in command_aliases(command) {
                assert!(
                    policy.check(Target::Dui, &name, allowed).is_ok(),
                    "{name}={allowed}"
                );
                assert!(
                    policy.check(Target::Dui, &name, denied).is_err(),
                    "{name}={denied}"
                );
            }
        }
    }

    #[test]
    fn ta_pro_blocks_dms_unknown_and_indirect_hardware_actions() {
        let policy = ta_pro_config().write_policy;
        let catalog = catalog::load().unwrap();
        for entry in catalog
            .iter()
            .filter(|entry| entry.target == Target::Dms && entry.set)
        {
            for name in command_aliases(&entry.command) {
                for value in ["true", "0"] {
                    assert!(
                        policy.check(Target::Dms, &name, value).is_err(),
                        "{name}={value}"
                    );
                }
            }
        }
        for key in &policy.commands {
            assert!(
                policy.check(Target::Dms, &key.command, "true").is_err(),
                "{}",
                key.command
            );
        }
        for target in [Target::Dms, Target::Dui] {
            assert!(
                policy
                    .check(target, "unknown:hardware action", "true")
                    .is_err()
            );
        }
        for command in [
            "simulation:pid:send parameters",
            "analog:lock:enable",
            "analog:proportional",
            "analog:sign",
            "analog:slope",
            "pdh:modulation:enable",
            "pdh:modulation:amplitude",
            "pdh:modulation:output",
            "response:start",
            "response:modulation:amplitude",
            "response:modulation:output",
            "pid1:lock:enable",
            "pid2:lock:enable",
            "autolock:lock:mode",
            "autolock:lock:strategy",
        ] {
            assert!(
                catalog::lookup(&catalog, Target::Dui, command).unwrap().set,
                "{command}"
            );
            for name in command_aliases(command) {
                for value in ["true", "false", "0", "main out"] {
                    assert!(
                        policy.check(Target::Dui, &name, value).is_err(),
                        "{name}={value}"
                    );
                }
            }
        }
    }

    #[test]
    fn ta_pro_scan_envelope_checks_peak_to_peak_amplitude_and_live_counterpart() {
        let policy = ta_pro_config().write_policy;
        let range = policy.scan_range.as_ref().unwrap();
        assert_eq!((range.min, range.max), (Some(-1.0), Some(1.0)));
        for name in command_aliases("offset:value") {
            assert_eq!(
                crate::policy::ScanRange::counterpart(&name),
                Some("scan:amplitude")
            );
            for offset in ["-0.5", "0", "0.5"] {
                policy.check(Target::Dui, &name, offset).unwrap();
                range.check_update(&name, offset, "1").unwrap();
            }
            for offset in ["-0.5", "0.5"] {
                assert!(
                    range.check_update(&name, offset, "1.02").is_err(),
                    "{name}={offset}"
                );
            }
            assert!(range.check_update(&name, "0", "-1").is_err());
        }
        for name in command_aliases("scan:amplitude") {
            assert_eq!(
                crate::policy::ScanRange::counterpart(&name),
                Some("offset:value")
            );
            policy.check(Target::Dui, &name, "1").unwrap();
            for offset in ["-0.5", "0", "0.5"] {
                range.check_update(&name, "1", offset).unwrap();
            }
            for offset in ["-0.51", "0.51"] {
                assert!(
                    range.check_update(&name, "1", offset).is_err(),
                    "{name}, offset={offset}"
                );
            }
            assert!(range.check_update(&name, "-1", "0").is_err());
        }
        for command in ["offset:value", "scan:amplitude"] {
            for invalid in ["NaN", "inf", "-inf"] {
                assert!(range.check_update(command, invalid, "0").is_err());
                assert!(range.check_update(command, "0", invalid).is_err());
            }
        }
    }

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
