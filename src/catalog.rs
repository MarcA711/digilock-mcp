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

#[cfg(test)]
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
    fn embedded_manual_catalog_counts_and_names_are_valid() {
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
    fn manual_irregularities_are_not_silently_reinterpreted() {
        let entries = load().unwrap();
        // M-038 v05, pp. 15–18: these spellings and access types are intentional
        // transcriptions, not inferred from analogous controls or graph names.
        let pid1_limit = lookup(&entries, Target::Dui, "pid1:limit enable").unwrap();
        assert!(pid1_limit.query && pid1_limit.set);
        assert_eq!(pid1_limit.data_type, "boolean");
        assert!(lookup(&entries, Target::Dui, "pid1:limit:enable").is_none());
        assert!(lookup(&entries, Target::Dui, "pid2:limit:enable").is_some());
        assert!(lookup(&entries, Target::Dui, "scan:enable").is_some());
        assert!(lookup(&entries, Target::Dui, "scan:enabble").is_none());
        for (command, query, set, data_type) in [
            ("li:modulation:frequency act", true, false, "numeric"),
            ("li:modulation:frequency set", true, true, "numeric"),
            ("pdh:modulation:frequency set", true, true, "enum"),
            ("response:graph", true, true, "2D array"),
            ("scope:graph", true, false, "2D array"),
            ("spectrum:graph", true, false, "2D array"),
        ] {
            let entry = lookup(&entries, Target::Dui, command).unwrap();
            assert_eq!((entry.query, entry.set), (query, set), "{command}");
            assert_eq!(entry.data_type, data_type, "{command}");
        }
    }

    #[test]
    fn catalog_duplicates_preserve_only_the_manual_description_ambiguity() {
        let entries = load().unwrap();
        // M-038 v05, p. 20 repeats each FFT hold row verbatim, but gives two
        // distinct descriptions for visibility:lock. Only the latter is retained.
        for (index, entry) in entries.iter().enumerate() {
            for previous in &entries[..index] {
                if entry.target == previous.target && entry.command == previous.command {
                    assert_eq!(entry.target, Target::Dui);
                    assert_eq!(entry.command, "visibility:lock");
                    assert_eq!(entry.query, previous.query);
                    assert_eq!(entry.set, previous.set);
                    assert_eq!(entry.data_type, previous.data_type);
                    assert_ne!(entry.description, previous.description);
                }
            }
        }
        let visibility: Vec<_> = entries
            .iter()
            .filter(|entry| entry.target == Target::Dui && entry.command == "visibility:lock")
            .collect();
        assert_eq!(visibility.len(), 2);
        assert_eq!(
            visibility[0].description,
            "show/hide lock navigation button"
        );
        assert_eq!(
            visibility[1].description,
            "show/hide advanced AutoLock controls"
        );
        for command in ["spectrum:ch1:hold", "spectrum:ch2:hold"] {
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| entry.target == Target::Dui && entry.command == command)
                    .count(),
                1
            );
        }
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
