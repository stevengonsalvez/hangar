//! TOML parse errors that never quote the source.
//!
//! `toml::de::Error` and `toml_edit::TomlError` both render the offending
//! source line under a caret in their `Display`. ainb's `config.toml` holds bot
//! tokens (`[fleet.bridge.*]`) and API keys (`[skills]`), so a syntax error on
//! one of those lines would print the secret into the error chain, the
//! startup log and anything that forwards either. These helpers keep the
//! parser's message and turn its span into a 1-based line number, which is
//! enough to find the mistake. The snippet is never formatted.
//!
//! The message itself is not safe either: serde's type errors quote the value
//! they rejected (`invalid type: string "API_KEY=sk-...", expected a map`), and
//! its unknown-name errors quote the name (``unknown field `sk-...` ``). So the
//! message is scrubbed too: a rejected value is reduced to its kind (`string`,
//! `integer`, ...) and an unknown name to `<redacted>`. What the schema expected
//! is kept, because that is what tells the reader how to fix the line.

use std::ops::Range;

/// `"<message> (line N)"` for a `toml` parse failure of `text`.
#[must_use]
pub fn describe_toml_error(text: &str, error: &toml::de::Error) -> String {
    located(text, error.message(), error.span())
}

/// `"<message> (line N)"` for a `toml_edit` parse failure of `text`.
#[must_use]
pub fn describe_toml_edit_error(text: &str, error: &toml_edit::TomlError) -> String {
    located(text, error.message(), error.span())
}

/// `"<context>: <message> (line N)"` as an error, for a `toml` parse failure.
#[must_use]
pub fn toml_parse_error(context: &str, text: &str, error: &toml::de::Error) -> anyhow::Error {
    anyhow::anyhow!("{context}: {}", describe_toml_error(text, error))
}

/// `"<context>: <message> (line N)"` as an error, for a `toml_edit` parse
/// failure.
#[must_use]
pub fn toml_edit_parse_error(
    context: &str,
    text: &str,
    error: &toml_edit::TomlError,
) -> anyhow::Error {
    anyhow::anyhow!("{context}: {}", describe_toml_edit_error(text, error))
}

fn located(text: &str, message: &str, span: Option<Range<usize>>) -> String {
    let message = scrub(message.trim_end());
    span.map_or_else(
        || message.clone(),
        |span| {
            let before = text.get(..span.start).unwrap_or(text);
            let line = before.matches('\n').count() + 1;
            format!("{message} (line {line})")
        },
    )
}

/// Remove every value and name a parser message quotes from the input.
///
/// - `invalid type: <value>, expected ...` and `invalid value: <value>,
///   expected ...`: `<value>` becomes its kind, the words before its first
///   quote or backtick (`string "..."` → `string`, `` integer `5` `` →
///   `integer`, `map` stays `map`). The value runs to the LAST `, expected`,
///   so a secret that itself contains `, expected` is still covered.
/// - ``unknown field `<name>` `` and ``unknown variant `<name>` ``: `<name>`
///   becomes `<redacted>`; the `expected one of ...` list that follows is the
///   schema and stays.
fn scrub(message: &str) -> String {
    let mut out = message.to_string();
    for prefix in ["invalid type: ", "invalid value: "] {
        let mut from = 0;
        while let Some(found) = out[from..].find(prefix) {
            let start = from + found + prefix.len();
            let end = out[start..].rfind(", expected").map_or(out.len(), |at| start + at);
            let value = &out[start..end];
            let kind =
                value.split(['"', '`', '\'']).next().unwrap_or_default().trim_end().to_string();
            let kind = if kind.is_empty() {
                "value".to_string()
            } else {
                kind
            };
            out.replace_range(start..end, &kind);
            from = start + kind.len();
        }
    }
    for prefix in ["unknown field `", "unknown variant `"] {
        let mut from = 0;
        while let Some(found) = out[from..].find(prefix) {
            let start = from + found + prefix.len();
            let Some(close) = out[start..].find('`') else {
                out.replace_range(start.., "<redacted>`");
                break;
            };
            out.replace_range(start..start + close, "<redacted>");
            from = start + "<redacted>`".len();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "[fleet.bridge.telegram]\ntoken = \"123:s3cr3tUnterminated\nuser_id = 1\n";

    /// Both parsers' own `Display` quotes the line, and ours does not.
    #[test]
    fn neither_parser_error_is_quoted() {
        let de = TEXT.parse::<toml::Table>().expect_err("unterminated");
        assert!(
            de.to_string().contains("s3cr3t"),
            "the premise: toml quotes the line"
        );
        let described = describe_toml_error(TEXT, &de);
        assert!(!described.contains("s3cr3t"), "{described}");
        assert!(described.contains("(line 2)"), "{described}");

        let edit = TEXT.parse::<toml_edit::DocumentMut>().expect_err("unterminated");
        assert!(
            edit.to_string().contains("s3cr3t"),
            "the premise: toml_edit quotes the line"
        );
        let described = describe_toml_edit_error(TEXT, &edit);
        assert!(!described.contains("s3cr3t"), "{described}");
        assert!(described.contains("(line 2)"), "{described}");
    }

    #[test]
    fn the_error_carries_its_context_and_no_snippet() {
        let de = TEXT.parse::<toml::Table>().expect_err("unterminated");
        let error = toml_parse_error("config does not parse", TEXT, &de);
        for rendered in [
            format!("{error}"),
            format!("{error:#}"),
            format!("{error:?}"),
        ] {
            assert!(!rendered.contains("s3cr3t"), "{rendered}");
            assert!(
                rendered.starts_with("config does not parse: "),
                "{rendered}"
            );
        }
    }

    /// serde quotes the rejected value; the scrub keeps only its kind and
    /// what the schema expected.
    #[test]
    fn a_rejected_value_is_reduced_to_its_kind() {
        let cases = [
            (
                r#"invalid type: string "API_KEY=sk-s3cr3t", expected a map"#,
                "invalid type: string, expected a map",
            ),
            (
                "invalid type: integer `12345`, expected a string",
                "invalid type: integer, expected a string",
            ),
            (
                "invalid type: floating point `1.5`, expected a string",
                "invalid type: floating point, expected a string",
            ),
            (
                "invalid type: map, expected a string",
                "invalid type: map, expected a string",
            ),
            (
                r#"invalid value: string "s3cr3t", expected one of `a`, `b`"#,
                "invalid value: string, expected one of `a`, `b`",
            ),
            (
                r#"invalid type: string "s3cr3t, expected leak", expected a map"#,
                "invalid type: string, expected a map",
            ),
            (
                "unknown field `sk-s3cr3t`, expected one of `name`, `environment`",
                "unknown field `<redacted>`, expected one of `name`, `environment`",
            ),
            (
                "unknown variant `s3cr3t`, expected `boss` or `agent`",
                "unknown variant `<redacted>`, expected `boss` or `agent`",
            ),
            ("invalid basic string", "invalid basic string"),
        ];
        for (raw, want) in cases {
            assert_eq!(scrub(raw), want, "{raw}");
        }
    }

    /// The lead's case, end to end through the real parser: a map field given
    /// a string holding a key.
    #[test]
    fn a_wrong_type_environment_does_not_quote_the_value() {
        #[derive(serde::Deserialize, Debug)]
        struct Preset {
            #[allow(dead_code)]
            environment: std::collections::HashMap<String, String>,
        }
        let text = "environment = \"API_KEY=sk-s3cr3tWrongType\"\n";
        let error = toml::from_str::<Preset>(text).expect_err("a string is not a map");
        assert!(
            error.to_string().contains("s3cr3tWrongType"),
            "the premise: serde quotes the value"
        );
        let described = describe_toml_error(text, &error);
        assert!(!described.contains("s3cr3t"), "{described}");
        assert!(described.contains("invalid type: string"), "{described}");
        assert!(described.contains("(line 1)"), "{described}");
    }
}
