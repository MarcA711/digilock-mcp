//! Optional restrictions on set operations. Call `validate` when loading a policy
//! and `check` before sending a set; queries must not be passed through this policy.
//! Values are inspected without modifying the string sent to the RCI endpoint.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::catalog::{self, Target};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WritePolicy {
    pub mode: Mode,
    #[serde(default)]
    pub commands: Vec<CommandKey>,
    #[serde(default)]
    pub limits: Vec<ValueLimit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Allowlist,
    Denylist,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandKey {
    pub target: Target,
    pub command: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValueLimit {
    pub target: Target,
    pub command: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub allowed_values: Option<Vec<String>>,
}

/// Compare command spellings ignoring ASCII case and ASCII spaces only.
///
/// This deliberately treats spaced and unspaced names as aliases so a spelling
/// accepted by RCI cannot bypass a denylist or a value limit. Targets are matched
/// separately; values are never normalized this way.
pub fn same_command(a: &str, b: &str) -> bool {
    a.bytes()
        .filter(|byte| *byte != b' ')
        .map(|byte| byte.to_ascii_lowercase())
        .eq(b
            .bytes()
            .filter(|byte| *byte != b' ')
            .map(|byte| byte.to_ascii_lowercase()))
}

impl WritePolicy {
    pub fn validate(&self, catalog: &[catalog::Command]) -> Result<()> {
        for (index, key) in self.commands.iter().enumerate() {
            catalog::validate_name(&key.command)
                .with_context(|| format!("invalid policy command {:?}", key.command))?;
            if self.commands[..index].iter().any(|other| {
                other.target == key.target && same_command(&other.command, &key.command)
            }) {
                bail!("duplicate policy command {:?}:{}", key.target, key.command);
            }
            validate_catalog_key(catalog, key.target, &key.command, false)?;
        }

        for (index, limit) in self.limits.iter().enumerate() {
            catalog::validate_name(&limit.command)
                .with_context(|| format!("invalid limit command {:?}", limit.command))?;
            if self.limits[..index].iter().any(|other| {
                other.target == limit.target && same_command(&other.command, &limit.command)
            }) {
                bail!("duplicate value limit {:?}:{}", limit.target, limit.command);
            }
            let numeric = limit.min.is_some() || limit.max.is_some();
            validate_catalog_key(catalog, limit.target, &limit.command, numeric)?;
            if !numeric && limit.allowed_values.is_none() {
                bail!(
                    "value limit for {} must contain at least one constraint",
                    limit.command
                );
            }
            for bound in [limit.min, limit.max].into_iter().flatten() {
                if !bound.is_finite() {
                    bail!("numeric bounds for {} must be finite", limit.command);
                }
            }
            if let (Some(min), Some(max)) = (limit.min, limit.max)
                && min > max
            {
                bail!("minimum exceeds maximum for {}", limit.command);
            }
            if let Some(values) = &limit.allowed_values {
                if values.is_empty() {
                    bail!("allowed_values for {} must not be empty", limit.command);
                }
                for (index, value) in values.iter().enumerate() {
                    catalog::validate_value(value)
                        .with_context(|| format!("invalid allowed value for {}", limit.command))?;
                    if values[..index].contains(value) {
                        bail!("duplicate allowed value {value:?} for {}", limit.command);
                    }
                }
            }
        }
        Ok(())
    }

    /// Authorize one set operation. This does not authorize raw commands or queries,
    /// and does not replace the server's global write guard or catalog checks.
    pub fn check(&self, target: Target, name: &str, value: &str) -> Result<()> {
        catalog::validate_name(name)?;
        catalog::validate_value(value)?;
        let listed = self
            .commands
            .iter()
            .any(|key| key.target == target && same_command(&key.command, name));
        match self.mode {
            Mode::Allowlist if !listed => bail!("write to {name} is not allowlisted"),
            Mode::Denylist if listed => bail!("write to {name} is denylisted"),
            _ => {}
        }
        for limit in &self.limits {
            if limit.target != target || !same_command(&limit.command, name) {
                continue;
            }
            if let Some(values) = &limit.allowed_values
                && !values.iter().any(|allowed| allowed == value)
            {
                bail!("value {value:?} is not allowed for {name}");
            }
            if limit.min.is_some() || limit.max.is_some() {
                let number = parse_number(value)
                    .with_context(|| format!("invalid numeric value for {name}"))?;
                if let Some(min) = limit.min
                    && number < min
                {
                    bail!("value for {name} is below minimum {min}");
                }
                if let Some(max) = limit.max
                    && number > max
                {
                    bail!("value for {name} exceeds maximum {max}");
                }
            }
        }
        Ok(())
    }
}

fn validate_catalog_key(
    catalog: &[catalog::Command],
    target: Target,
    name: &str,
    numeric: bool,
) -> Result<()> {
    // Check every matching entry: a future catalog with ambiguous aliases must
    // not make validation depend on which entry happens to appear first.
    for entry in catalog
        .iter()
        .filter(|entry| entry.target == target && same_command(&entry.command, name))
    {
        if !entry.set {
            bail!("{name} is read-only according to the manual");
        }
        if numeric && entry.data_type != "numeric" {
            bail!(
                "numeric bounds require a numeric command, but {name} is {}",
                entry.data_type
            );
        }
    }
    Ok(())
}

fn parse_number(value: &str) -> Result<f64> {
    if value.chars().any(char::is_whitespace) {
        bail!("numeric values must not contain whitespace");
    }
    let commas = value.bytes().filter(|byte| *byte == b',').count();
    if commas > 1 || (commas == 1 && value.contains('.')) {
        bail!("use a decimal dot or a single decimal comma, not mixed separators");
    }
    let number: f64 = if commas == 1 {
        value.replace(',', ".").parse()
    } else {
        value.parse()
    }
    .context("expected a number, optionally in scientific notation")?;
    if !number.is_finite() {
        bail!("numeric values must be finite");
    }
    Ok(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy(mode: Mode) -> WritePolicy {
        WritePolicy {
            mode,
            commands: vec![],
            limits: vec![],
        }
    }

    fn key(target: Target, command: &str) -> CommandKey {
        CommandKey {
            target,
            command: command.into(),
        }
    }

    fn limit(target: Target, command: &str) -> ValueLimit {
        ValueLimit {
            target,
            command: command.into(),
            min: None,
            max: None,
            allowed_values: None,
        }
    }

    fn catalog_entry(
        target: Target,
        command: &str,
        set: bool,
        data_type: &str,
    ) -> catalog::Command {
        catalog::Command {
            target,
            command: command.into(),
            query: true,
            set,
            data_type: data_type.into(),
            description: String::new(),
        }
    }

    #[test]
    fn serde_defaults_round_trip_and_mode_names() {
        for (name, mode) in [("allowlist", Mode::Allowlist), ("denylist", Mode::Denylist)] {
            let parsed: WritePolicy = serde_json::from_value(json!({"mode": name})).unwrap();
            assert_eq!(parsed.mode, mode);
            assert!(parsed.commands.is_empty());
            assert!(parsed.limits.is_empty());
            parsed.validate(&[]).unwrap();
        }
        let source = json!({
            "mode": "allowlist",
            "commands": [{"target": "dui", "command": "gain"}],
            "limits": [{"target": "dui", "command": "gain", "min": -1.0,
                        "max": 1.0, "allowed_values": ["0", "1"]}]
        });
        let parsed: WritePolicy = serde_json::from_value(source.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), source);
    }

    #[test]
    fn serde_rejects_unknown_fields_and_malformed_shapes() {
        for invalid in [
            json!({"mode": "denylist", "typo": true}),
            json!({"mode": "denylist", "commands": [{"target": "dms", "command": "a", "typo": 1}]}),
            json!({"mode": "denylist", "limits": [{"target": "dms", "command": "a", "typo": 1}]}),
            json!({}),
            json!({"mode": "Allowlist"}),
            json!({"mode": "other"}),
            json!({"mode": "denylist", "commands": null}),
            json!({"mode": "denylist", "limits": null}),
            json!({"mode": "denylist", "commands": [{"target": "other", "command": "a"}]}),
            json!({"mode": "denylist", "commands": [{"target": "dms"}]}),
            json!({"mode": "denylist", "limits": [{"target": "dui", "command": "a", "min": "1"}]}),
            json!({"mode": "denylist", "limits": [{"target": "dui", "command": "a", "allowed_values": [1]}]}),
        ] {
            assert!(
                serde_json::from_value::<WritePolicy>(invalid.clone()).is_err(),
                "{invalid}"
            );
        }
        let parsed: WritePolicy = serde_json::from_value(json!({
            "mode": "denylist", "limits": [{"target": "dui", "command": "gain", "max": 1}]
        }))
        .unwrap();
        assert!(parsed.limits[0].min.is_none());
        assert!(parsed.limits[0].allowed_values.is_none());
    }

    #[test]
    fn allowlist_and_denylist_membership_is_target_specific() {
        for mode in [Mode::Allowlist, Mode::Denylist] {
            let mut p = policy(mode);
            assert_eq!(
                p.check(Target::Dui, "gain", "text").is_ok(),
                mode == Mode::Denylist
            );
            p.commands.push(key(Target::Dui, "gain"));
            p.validate(&[]).unwrap();
            assert_eq!(
                p.check(Target::Dui, "gain", "text").is_ok(),
                mode == Mode::Allowlist
            );
            for (target, name) in [(Target::Dms, "gain"), (Target::Dui, "other")] {
                assert_eq!(
                    p.check(target, name, "text").is_ok(),
                    mode == Mode::Denylist
                );
            }
        }
    }

    #[test]
    fn aliases_cannot_bypass_membership_or_limits() {
        assert!(same_command("Selected Module", "sElEcTeDmOdUlE"));
        assert!(same_command(" A B ", "ab"));
        for different in ["a\tb", "a\u{a0}b", "a/b", "a_b", "a:b"] {
            assert!(!same_command("ab", different));
        }
        assert!(!same_command("Ä", "ä"));
        for mode in [Mode::Allowlist, Mode::Denylist] {
            let mut p = policy(mode);
            p.commands.push(key(Target::Dms, "selected module"));
            for name in [
                "SELECTED MODULE",
                "selectedmodule",
                "s e l e c t e d m o d u l e",
            ] {
                assert_eq!(
                    p.check(Target::Dms, name, "2").is_ok(),
                    mode == Mode::Allowlist
                );
            }
        }
        let mut p = policy(Mode::Denylist);
        let mut l = limit(Target::Dms, "selected module");
        l.max = Some(2.0);
        p.limits.push(l);
        assert!(p.check(Target::Dms, "SELECTEDMODULE", "3").is_err());
        assert!(p.check(Target::Dui, "SELECTEDMODULE", "3").is_ok());
    }

    #[test]
    fn numeric_bounds_are_inclusive_and_support_decimal_comma_and_exponents() {
        let mut p = policy(Mode::Denylist);
        let mut l = limit(Target::Dui, "gain");
        l.min = Some(-1.25);
        l.max = Some(2.5);
        p.limits.push(l);
        p.validate(&[]).unwrap();
        for value in [
            "-1.25", "-1,25", "2.5", "2,5", "0", "+0", "-0", "2.5e0", "2,5E+0", "25e-1", "1e-3",
            ".5", "1.",
        ] {
            p.check(Target::Dui, "gain", value).unwrap();
        }
        for value in ["-1.2501", "2.5001", "-1,251", "2,501", "25e1"] {
            assert!(p.check(Target::Dui, "gain", value).is_err(), "{value}");
        }
        assert!(p.check(Target::Dui, "other", "arbitrary text").is_ok());
    }

    #[test]
    fn numeric_constraints_reject_nonfinite_and_malformed_values() {
        let mut p = policy(Mode::Denylist);
        let mut l = limit(Target::Dui, "gain");
        l.min = Some(-f64::MAX);
        l.max = Some(f64::MAX);
        p.limits.push(l);
        for value in [
            "",
            " ",
            " 1",
            "1 ",
            "1 2",
            "1\t",
            "\n1",
            "1\u{a0}",
            "NaN",
            "nan",
            "inf",
            "-inf",
            "+inf",
            "Infinity",
            "-Infinity",
            "1e9999",
            "text",
            "true",
            "1V",
            "0x10",
            "1_000",
            "1,2.3",
            "1.2,3",
            "1,2,3",
            ".",
            ",",
            "1e",
            "--1",
            "1e1,2",
            "1/2",
        ] {
            assert!(p.check(Target::Dui, "gain", value).is_err(), "{value:?}");
        }
    }

    #[test]
    fn one_sided_and_equal_bounds_work() {
        for (min, max, allowed, denied) in [
            (Some(1.0), None, "1e2", "0.9"),
            (None, Some(1.0), "-1e2", "1.1"),
            (Some(1.0), Some(1.0), "1,0", "1.0001"),
        ] {
            let mut p = policy(Mode::Denylist);
            let mut l = limit(Target::Dui, "gain");
            l.min = min;
            l.max = max;
            p.limits.push(l);
            p.validate(&[]).unwrap();
            p.check(Target::Dui, "gain", allowed).unwrap();
            assert!(p.check(Target::Dui, "gain", denied).is_err());
        }
    }

    #[test]
    fn allowed_values_are_exact_and_both_constraints_apply() {
        let mut p = policy(Mode::Denylist);
        let mut l = limit(Target::Dui, "gain");
        l.allowed_values = Some(vec!["1".into(), "1,0".into(), "2".into(), "text".into()]);
        p.limits.push(l);
        p.validate(&[]).unwrap();
        for value in ["1", "1,0", "2", "text"] {
            p.check(Target::Dui, "gain", value).unwrap();
        }
        for value in ["1.0", "1e0", "TEXT", " text", "text "] {
            assert!(p.check(Target::Dui, "gain", value).is_err());
        }
        p.limits[0].min = Some(0.5);
        p.limits[0].max = Some(1.5);
        p.validate(&[]).unwrap();
        p.check(Target::Dui, "gain", "1,0").unwrap();
        for value in ["2", "text", "1.0"] {
            assert!(p.check(Target::Dui, "gain", value).is_err());
        }
    }

    #[test]
    fn limits_do_not_grant_allowlist_membership() {
        let mut p = policy(Mode::Allowlist);
        let mut l = limit(Target::Dui, "gain");
        l.max = Some(1.0);
        p.limits.push(l);
        p.validate(&[]).unwrap();
        assert!(p.check(Target::Dui, "gain", "0").is_err());
        p.commands.push(key(Target::Dui, "gain"));
        p.check(Target::Dui, "gain", "0").unwrap();
        assert!(p.check(Target::Dui, "gain", "2").is_err());
    }

    #[test]
    fn names_and_values_cannot_inject_rci_operations() {
        for name in [
            "",
            " gain",
            "gain ",
            "gain?",
            "gain=1",
            "gain.help?",
            "a\nb",
            "a\tb",
            "é",
        ] {
            let mut p = policy(Mode::Denylist);
            p.commands.push(key(Target::Dui, name));
            assert!(p.validate(&[]).is_err(), "{name:?}");
            p.commands.clear();
            let mut l = limit(Target::Dui, name);
            l.max = Some(1.0);
            p.limits.push(l);
            assert!(p.validate(&[]).is_err(), "{name:?}");
            assert!(p.check(Target::Dui, name, "0").is_err(), "{name:?}");
        }
        let p = policy(Mode::Denylist);
        for value in ["", "1\r\nprogram:exit=true", "\0", "\t", "\u{7f}"] {
            assert!(p.check(Target::Dui, "gain", value).is_err());
        }
    }

    #[test]
    fn duplicate_keys_include_case_and_space_aliases_but_not_other_targets() {
        for name in [
            "selected module",
            "SELECTED MODULE",
            "selectedmodule",
            "selected  module",
        ] {
            let mut p = policy(Mode::Denylist);
            p.commands = vec![key(Target::Dms, "selected module"), key(Target::Dms, name)];
            assert!(p.validate(&[]).is_err(), "{name}");
            p.commands[1].target = Target::Dui;
            p.validate(&[]).unwrap();
            p.commands.clear();
            for command in ["selected module", name] {
                let mut l = limit(Target::Dms, command);
                l.max = Some(1.0);
                p.limits.push(l);
            }
            assert!(p.validate(&[]).is_err(), "{name}");
            p.limits[1].target = Target::Dui;
            p.validate(&[]).unwrap();
        }
    }

    #[test]
    fn malformed_limits_are_rejected() {
        let mut p = policy(Mode::Denylist);
        p.limits.push(limit(Target::Dui, "gain"));
        assert!(p.validate(&[]).is_err());
        for bound in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            p.limits[0].min = Some(bound);
            assert!(p.validate(&[]).is_err());
            p.limits[0].min = None;
            p.limits[0].max = Some(bound);
            assert!(p.validate(&[]).is_err());
            p.limits[0].max = None;
        }
        p.limits[0].min = Some(2.0);
        p.limits[0].max = Some(1.0);
        assert!(p.validate(&[]).is_err());
        p.limits[0].min = None;
        p.limits[0].max = None;
        for values in [
            vec![],
            vec![""],
            vec!["x\ny"],
            vec!["\0"],
            vec!["\t"],
            vec!["a", "a"],
        ] {
            p.limits[0].allowed_values = Some(values.into_iter().map(String::from).collect());
            assert!(p.validate(&[]).is_err());
        }
        p.limits[0].allowed_values = Some(vec!["a".into(), "A".into(), " a".into(), "a ".into()]);
        p.validate(&[]).unwrap();
    }

    #[test]
    fn known_read_only_commands_and_nonnumeric_bounds_are_rejected() {
        let catalog = vec![
            catalog_entry(Target::Dui, "read only", false, "numeric"),
            catalog_entry(Target::Dui, "enabled", true, "boolean"),
            catalog_entry(Target::Dui, "gain", true, "numeric"),
        ];
        for mode in [Mode::Allowlist, Mode::Denylist] {
            let mut p = policy(mode);
            p.commands.push(key(Target::Dui, "READONLY"));
            assert!(p.validate(&catalog).is_err());
            p.commands.clear();
            let mut l = limit(Target::Dui, "Read Only");
            l.allowed_values = Some(vec!["1".into()]);
            p.limits.push(l);
            assert!(p.validate(&catalog).is_err());
        }
        let mut p = policy(Mode::Denylist);
        let mut l = limit(Target::Dui, "ENABLED");
        l.min = Some(0.0);
        p.limits.push(l);
        assert!(p.validate(&catalog).is_err());
        p.limits[0].min = None;
        p.limits[0].max = Some(1.0);
        assert!(p.validate(&catalog).is_err());
        p.limits[0].max = None;
        p.limits[0].allowed_values = Some(vec!["true".into(), "false".into()]);
        p.validate(&catalog).unwrap();
        p.limits[0].min = Some(0.0);
        p.limits[0].command = "GAIN".into();
        p.validate(&catalog).unwrap();
        p.limits[0].command = "unknown".into();
        p.validate(&catalog).unwrap();
        p.limits[0].command = "read only".into();
        p.limits[0].target = Target::Dms;
        p.validate(&catalog).unwrap();
    }

    #[test]
    fn all_catalog_alias_matches_are_checked() {
        let mut p = policy(Mode::Denylist);
        p.commands.push(key(Target::Dui, "ab"));
        let catalog = vec![
            catalog_entry(Target::Dui, "ab", true, "numeric"),
            catalog_entry(Target::Dui, "a b", false, "numeric"),
        ];
        assert!(p.validate(&catalog).is_err());
    }

    #[test]
    fn query_support_does_not_change_set_policy_and_real_catalog_validates() {
        let mut entry = catalog_entry(Target::Dui, "gain", true, "numeric");
        let mut p = policy(Mode::Allowlist);
        p.commands.push(key(Target::Dui, "gain"));
        for query in [false, true] {
            entry.query = query;
            p.validate(std::slice::from_ref(&entry)).unwrap();
            p.check(Target::Dui, "gain", "1").unwrap();
        }
        let catalog = catalog::load().unwrap();
        p.commands = vec![key(Target::Dui, "PID1: PROPORTIONAL")];
        let mut l = limit(Target::Dui, "pid1:proportional");
        l.min = Some(0.0);
        p.limits = vec![l];
        p.validate(&catalog).unwrap();
        p.commands = vec![key(Target::Dui, "SCOPE:GRAPH")];
        assert!(p.validate(&catalog).is_err());
    }
}
