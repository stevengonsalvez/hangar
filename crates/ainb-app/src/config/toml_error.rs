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
//! message is scrubbed too, by an allowlist ([`scrub`]): a rejected value is
//! reduced to its kind (`string`, `integer`, ...), any quoted name to
//! `<redacted>`, and what the schema expected is kept, because that is what
//! tells the reader how to fix the line.

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

/// `"<context>: <scrubbed message>"` as an error, for a failure to turn an
/// already-parsed TOML value into a typed struct (`Value::try_into`).
///
/// There is no source text and no span, so there is no line; the message is
/// still [`scrub`]bed, because serde's type errors quote the rejected value.
#[must_use]
pub fn toml_value_error(context: &str, error: &toml::de::Error) -> anyhow::Error {
    anyhow::anyhow!("{context}: {}", scrub(error.message().trim_end()))
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
            // Count lines up to the span, clamped to the text and rounded down
            // to a char boundary, so a span that is out of range or mid-char
            // still names a real line instead of falling back to the end.
            let mut start = span.start.min(text.len());
            while !text.is_char_boundary(start) {
                start -= 1;
            }
            let line = text[..start].matches('\n').count() + 1;
            format!("{message} (line {line})")
        },
    )
}

/// Drop everything a parser message quotes from the input.
///
/// An allowlist of what may stay, not a list of what to remove, so a message
/// shape this build has never seen (a toml upgrade, a custom `Deserialize`
/// error) fails closed:
///
/// - Everything before the first quote or backtick stays: that is the error's
///   category (`invalid type: string`, `unknown field`, `duplicate key`).
/// - A line that starts with `expected` is toml's own syntax list (``expected
///   `.`, `=` ``) and stays whole: toml never quotes input there.
/// - serde's four fixed forms end with the schema's `, expected ...`, which
///   stays, found by the LAST `, expected` so a value that itself holds that
///   text is still covered. The no-candidates form (`there are no fields`)
///   keeps nothing, because there the last `, expected` can sit in the name.
/// - Everything else after the first quote is replaced: a rejected value by
///   nothing (its kind was the text before the quote), a name by
///   `` `<redacted>` ``. Later lines of a multi-line message go with it; losing
///   a diagnostic is the safe direction.
pub(crate) fn scrub(message: &str) -> String {
    let Some(first) = message.find(['`', '"', '\'']) else {
        return message.to_string();
    };
    let line_start = message[..first].rfind('\n').map_or(0, |at| at + 1);
    if message[line_start..].starts_with("expected") {
        return message.to_string();
    }
    let prefix = &message[..first];
    let line_prefix = &message[line_start..first];
    let serde_form = SERDE_FORMS.iter().find(|form| line_prefix.starts_with(**form));
    let tail = serde_form.and_then(|_| {
        let rest = &message[first..];
        if rest.contains("there are no ") {
            return None;
        }
        let at = rest.rfind(", expected")?;
        let tail = &rest[at..];
        (!tail.contains('\n')).then_some(tail)
    });
    let kind_only =
        line_prefix.starts_with("invalid type: ") || line_prefix.starts_with("invalid value: ");
    let mut out = if kind_only {
        prefix.trim_end().to_string()
    } else {
        format!("{prefix}`{REDACTED}`")
    };
    if let Some(tail) = tail {
        out.push_str(tail);
    }
    out
}

/// What stands in for a name the message quoted.
const REDACTED: &str = "<redacted>";

/// serde's messages whose `, expected ...` tail is the schema, not input.
const SERDE_FORMS: [&str; 4] = [
    "invalid type: ",
    "invalid value: ",
    "unknown field ",
    "unknown variant ",
];

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

    /// Every quoted name goes, and with it anything after it that is not
    /// serde's schema tail: the table path after a duplicate key is built from
    /// user keys, and toml has shapes (dotted keys, table-less duplicates)
    /// with no fixed terminator.
    #[test]
    fn a_quoted_name_and_everything_after_it_is_redacted() {
        let cases = [
            (
                "duplicate key `S3CRETa` in document root",
                "duplicate key `<redacted>`",
            ),
            (
                "duplicate key `k` in table `t.S3CRETb`",
                "duplicate key `<redacted>`",
            ),
            (
                "duplicate key `k` in S3CRETc`",
                "duplicate key `<redacted>`",
            ),
            (
                "dotted key `S3CRETd` attempted to extend non-table type (integer)",
                "dotted key `<redacted>`",
            ),
            (
                "invalid table header\nduplicate key `S3CRETe` in document root",
                "invalid table header\nduplicate key `<redacted>`",
            ),
            (
                "unknown field `a`, expected S3CRETf`, there are no fields",
                "unknown field `<redacted>`",
            ),
            (
                "unknown field `a`, expected `x`\nunknown field `S3CRETg`, expected `y`",
                "unknown field `<redacted>`, expected `y`",
            ),
            (
                "some future shape `S3CRETh` with more text",
                "some future shape `<redacted>`",
            ),
            ("bad value 'S3CRETi' here", "bad value `<redacted>`"),
        ];
        for (raw, want) in cases {
            let got = scrub(raw);
            assert_eq!(got, want, "{raw}");
            assert!(!got.contains("S3CRET"), "{got}");
        }
    }

    /// toml's own syntax lists quote no input and stay whole.
    #[test]
    fn a_toml_expected_list_is_kept() {
        for message in ["expected `.`, `=`", "invalid key\nexpected `]`"] {
            assert_eq!(scrub(message), message);
        }
    }

    /// A backtick, the terminator text or a newline inside a name must not
    /// end the redaction early.
    #[test]
    fn a_backtick_inside_a_name_does_not_end_the_redaction() {
        let cases = [
            (
                "unknown field `S3`CRETj, expected leak`, expected `name` or `mode`",
                "unknown field `<redacted>`, expected `name` or `mode`",
            ),
            (
                "unknown variant `S3`CR`ETk`, expected `boss` or `agent`",
                "unknown variant `<redacted>`, expected `boss` or `agent`",
            ),
            (
                "unknown field `line\nS3CRETl`, expected `name`",
                "unknown field `<redacted>`, expected `name`",
            ),
        ];
        for (raw, want) in cases {
            let got = scrub(raw);
            assert_eq!(got, want, "{raw}");
            for fragment in ["S3", "CRET", "ETk", "leak"] {
                assert!(!got.contains(fragment), "{fragment} survived in {got}");
            }
        }
    }

    /// End to end: toml's dotted-key and table-path messages through both
    /// real parsers.
    #[test]
    fn real_dotted_key_and_table_path_errors_do_not_leak() {
        for text in [
            "\"sk-S3CRETm\" = 1\n\"sk-S3CRETm\".y = 1\n",
            "[t.\"S3CRETn\"]\nx = 1\nx = 2\n",
            "[a.\"k` in S3CRETo\"]\n[a]\n\"k` in S3CRETo\".y = 1\n",
        ] {
            let edit = text.parse::<toml_edit::DocumentMut>().expect_err("invalid");
            assert!(edit.message().contains("S3CRET"), "the premise: {edit}");
            let described = describe_toml_edit_error(text, &edit);
            assert!(!described.contains("S3CRET"), "{described}");
            let de = text.parse::<toml::Table>().expect_err("invalid");
            let described = describe_toml_error(text, &de);
            assert!(!described.contains("S3CRET"), "{described}");
        }
    }

    /// End to end through both real parsers: a key with a backtick in it,
    /// duplicated.
    #[test]
    fn a_real_duplicate_key_with_a_backtick_does_not_leak() {
        let text = "\"sk`s3cr3t, expected x`c\" = 1\n\"sk`s3cr3t, expected x`c\" = 2\n";
        let de = text.parse::<toml::Table>().expect_err("duplicate");
        assert!(
            de.message().contains("s3cr3t"),
            "the premise: toml names the key"
        );
        let described = describe_toml_error(text, &de);
        assert!(!described.contains("s3cr3t"), "{described}");
        assert!(
            described.contains("duplicate key `<redacted>`"),
            "{described}"
        );
        let edit = text.parse::<toml_edit::DocumentMut>().expect_err("duplicate");
        let described = describe_toml_edit_error(text, &edit);
        assert!(!described.contains("s3cr3t"), "{described}");
        assert!(described.contains("(line 2)"), "{described}");
    }
}
