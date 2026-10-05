use anyhow::{Result, bail};
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Dms,
    Dui,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Command {
    pub target: Target,
    pub command: String,
    pub query: bool,
    pub set: bool,
    pub data_type: String,
    pub description: String,
}

pub fn load() -> Result<Vec<Command>> {
    Ok(serde_json::from_str(include_str!("../docs/commands.json"))?)
}

pub fn lookup<'a>(catalog: &'a [Command], target: Target, name: &str) -> Option<&'a Command> {
    catalog
        .iter()
        .find(|entry| entry.target == target && entry.command == name)
}

/// Restrict a name to one control, not a query, assignment, or RCI meta-command.
/// Spaces and slashes are significant in documented control names.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.trim() != name {
        bail!("command name must be nonempty with no leading/trailing whitespace");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || " :/_-".contains(c))
    {
        bail!(
            "command name may contain only ASCII letters, digits, spaces, ':', '/', '_', or '-'; omit '?', '=', '.range?', and '.help?'"
        );
    }
    Ok(())
}

pub fn validate_value(value: &str) -> Result<()> {
    if value.is_empty() || value.chars().any(char::is_control) {
        bail!("value must be nonempty and must not contain control characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_catalog_is_complete_and_names_are_valid() {
        let entries = load().unwrap();
        assert_eq!(
            entries.iter().filter(|c| c.target == Target::Dms).count(),
            17
        );
        assert_eq!(
            entries.iter().filter(|c| c.target == Target::Dui).count(),
            264
        );
        for entry in &entries {
            validate_name(&entry.command).unwrap();
            assert!(entry.query || entry.set);
        }
        let gain = lookup(&entries, Target::Dui, "pid1:proportional").unwrap();
        assert!(gain.query && gain.set);
        assert!(!lookup(&entries, Target::Dui, "scope:graph").unwrap().set);
        assert!(!lookup(&entries, Target::Dms, "echo").unwrap().query);
    }

    #[test]
    fn names_cannot_inject_operations() {
        for invalid in [
            "",
            " pid1:enable",
            "pid1:enable ",
            "a?",
            "a=true",
            "a.help?",
            "a\r\nb",
            "a\0",
            "a\t",
            "a\u{7f}",
        ] {
            assert!(validate_name(invalid).is_err(), "{invalid:?}");
        }
        for valid in ["pid1:enable", "selected module", "visibility:li/pdh"] {
            validate_name(valid).unwrap();
        }
    }

    #[test]
    fn values_preserve_localized_numbers_but_reject_line_injection() {
        for valid in ["true", "main in", "0,25", "1.5", "a=b"] {
            validate_value(valid).unwrap();
        }
        for invalid in ["", "true\r\nprogram:exit=true", "\0", "\t", "\u{7f}"] {
            assert!(validate_value(invalid).is_err());
        }
    }
}
