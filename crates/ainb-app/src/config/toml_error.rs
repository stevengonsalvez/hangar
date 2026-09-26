//! TOML parse errors that never quote the source.
//!
//! `toml::de::Error` and `toml_edit::TomlError` both render the offending
//! source line under a caret in their `Display`. ainb's `config.toml` holds bot
//! tokens (`[fleet.bridge.*]`) and API keys (`[skills]`), so a syntax error on
//! one of those lines would print the secret into the error chain, the
//! startup log and anything that forwards either. These helpers keep the
//! parser's message and turn its span into a 1-based line number, which is
//! enough to find the mistake. The snippet is never formatted.

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
    let message = message.trim_end();
    span.map_or_else(
        || message.to_string(),
        |span| {
            let before = text.get(..span.start).unwrap_or(text);
            let line = before.matches('\n').count() + 1;
            format!("{message} (line {line})")
        },
    )
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
}
