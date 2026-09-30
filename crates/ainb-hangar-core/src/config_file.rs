//! What a reader of the hangar home's `config/config.toml`
//! ([`crate::paths::config_path`]) may say about it: where a parse error is,
//! and which entries of a table are malformed. Never the file's text.
//!
//! Lives here because every reader of that file depends on this crate: the
//! daemon, and the notifyd and session-reader plugins, which must not depend on
//! the daemon. One implementation, so no reader can drift back to logging the
//! errors verbatim.
//!
//! Why not verbatim: toml's `Display` for a parse error quotes the offending
//! source line, and serde's message for a wrong-typed value quotes the value.
//! Either can be a token, and every reader sends its error to a log.
//!
//! Pure: the callers do their own reading, and own their answer for each
//! file state.

use serde::de::DeserializeOwned;

/// Parse `text` as a TOML table.
///
/// # Errors
///
/// A description of what is wrong and where: toml's message (a multi-line one
/// joined with `; `) and a 1-based line and column, for example
/// `invalid basic string at line 3, column 23`. It never quotes `text`.
pub fn parse(text: &str) -> Result<toml::Table, String> {
    text.parse().map_err(|error| describe(&error, text))
}

/// `error` as its message and a 1-based line and column in `text`, on one line.
fn describe(error: &toml::de::Error, text: &str) -> String {
    let message = error.message().trim_end().replace('\n', "; ");
    let Some(span) = error.span() else {
        return message;
    };
    let before = &text.as_bytes()[..span.start.min(text.len())];
    let line_start = before.iter().rposition(|byte| *byte == b'\n').map_or(0, |at| at + 1);
    let line = before.split(|byte| *byte == b'\n').count();
    let column = String::from_utf8_lossy(&before[line_start..]).chars().count() + 1;
    format!("{message} at line {line}, column {column}")
}

/// Deserialize `value` (a table read from the file) as a `T`.
///
/// # Errors
///
/// The names of the entries of `value` that do not decode as part of a `T` on
/// their own, in the table's iteration order; empty when `value` is not a table at all. Never
/// serde's message, which quotes the value.
///
/// Exact for a `T` that accepts any subset of the entries: a struct whose
/// fields all default, or a map. For any other `T` the names may include
/// well-formed entries too.
pub fn decode<T: DeserializeOwned>(value: &toml::Value) -> Result<T, Vec<String>> {
    value.clone().try_into().map_err(|_| malformed_entries::<T>(value))
}

fn malformed_entries<T: DeserializeOwned>(value: &toml::Value) -> Vec<String> {
    let Some(table) = value.as_table() else {
        return Vec::new();
    };
    table
        .iter()
        .filter(|&(name, entry)| {
            let mut alone = toml::Table::new();
            alone.insert(name.clone(), entry.clone());
            toml::Value::Table(alone).try_into::<T>().is_err()
        })
        .map(|(name, _)| name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Debug, Default, PartialEq, serde::Deserialize)]
    #[serde(default)]
    struct Knobs {
        window_days: u32,
        label: String,
    }

    #[derive(Debug, serde::Deserialize)]
    struct Adapter {
        #[serde(default)]
        command: Option<String>,
    }

    #[test]
    fn a_parse_error_is_located_not_quoted() {
        let text = "[codex]\napp_server = \"own\"\ntoken = \"sk-SECRET-123\n";
        let description = parse(text).expect_err("an unterminated string");
        assert!(!description.contains("sk-SECRET"), "{description}");
        assert_eq!(description, "invalid basic string at line 3, column 23");
    }

    /// A multi-line message (what was wrong, then what was expected) is one
    /// log line.
    #[test]
    fn a_multi_line_message_is_one_line() {
        let description = parse("token = sk-SECRET-123\n").expect_err("an unquoted string");
        assert!(!description.contains('\n'), "{description:?}");
        assert!(!description.contains("sk-SECRET"), "{description}");
        assert!(description.contains("; expected "), "{description}");
        assert!(
            description.ends_with(" at line 1, column 9"),
            "{description}"
        );
    }

    #[test]
    fn a_valid_file_parses() {
        let table = parse("[codex]\napp_server = \"own\"\n").expect("valid");
        assert_eq!(table["codex"]["app_server"].as_str(), Some("own"));
    }

    #[test]
    fn a_wrong_typed_field_is_named_not_quoted() {
        let value = toml::Value::Table(
            parse("window_days = \"sk-SECRET-456\"\nlabel = \"fine\"\n").unwrap(),
        );
        let names = decode::<Knobs>(&value).expect_err("a string is not a u32");
        assert_eq!(names, ["window_days"]);
    }

    #[test]
    fn a_malformed_map_entry_is_named_by_its_key() {
        let value = toml::Value::Table(
            parse("[fine]\ncommand = \"/bin/fine\"\n[leaky]\ncommand = [\"sk-SECRET-789\"]\n")
                .unwrap(),
        );
        let names = decode::<HashMap<String, Adapter>>(&value).expect_err("an array command");
        assert_eq!(names, ["leaky"]);

        let fine = toml::Value::Table(parse("[fine]\ncommand = \"/bin/fine\"\n").unwrap());
        let adapters = decode::<HashMap<String, Adapter>>(&fine).expect("well formed");
        assert_eq!(adapters["fine"].command.as_deref(), Some("/bin/fine"));
    }

    #[test]
    fn a_value_that_is_not_a_table_names_nothing() {
        let value = toml::Value::String("sk-SECRET-000".into());
        assert_eq!(decode::<Knobs>(&value), Err(Vec::new()));
    }

    #[test]
    fn a_well_formed_table_decodes() {
        let value = toml::Value::Table(parse("window_days = 7\n").unwrap());
        assert_eq!(
            decode::<Knobs>(&value),
            Ok(Knobs {
                window_days: 7,
                label: String::new()
            })
        );
    }
}
