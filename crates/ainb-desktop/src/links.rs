//! The one rule a link the window asks to open passes (`open_url`): a web
//! page and nothing else.
//!
//! A terminal prints whatever its program sends, so the URL under a click is
//! untrusted text. The page asks; the host parses it with a real URL parser,
//! keeps only an absolute `http` or `https` URL with a host and no
//! credentials, and hands the OS the parser's own serialization, never the
//! text it was sent. Anything else is refused before the OS is asked.

use url::Url;

/// The longest URL opened, the same bound the page links by (Orca's
/// `TERMINAL_HTTP_URL_MAX_LENGTH`).
pub const MAX_URL_BYTES: usize = 2048;

/// Why a link was not opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Empty, or longer than [`MAX_URL_BYTES`].
    Length(usize),
    /// Whitespace, a control character or an invisible format character
    /// anywhere: the parser would quietly strip some of them, and a bidi
    /// override turns text around, so what it read would not be what the
    /// terminal showed.
    Hidden,
    /// Not an absolute URL.
    Unparsable(url::ParseError),
    /// A scheme other than `http` or `https`.
    Scheme(String),
    /// A user name or password before the host, which can dress one host up
    /// as another (`https://bank.example@evil.example/`).
    Credentials,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Length(bytes) => write!(f, "a {bytes}-byte URL is outside 1..={MAX_URL_BYTES}"),
            Self::Hidden => f.write_str("the URL carries whitespace or an invisible character"),
            Self::Unparsable(error) => write!(f, "not an absolute URL: {error}"),
            Self::Scheme(scheme) => write!(
                f,
                "the `{scheme}` scheme is not opened, only http and https"
            ),
            Self::Credentials => f.write_str("the URL carries a user name or password"),
        }
    }
}

impl std::error::Error for Refused {}

/// `raw` as the web URL to open, or why it is refused.
pub fn web_url(raw: &str) -> Result<Url, Refused> {
    if raw.is_empty() || raw.len() > MAX_URL_BYTES {
        return Err(Refused::Length(raw.len()));
    }
    if raw.chars().any(|c| c.is_whitespace() || c.is_control() || invisible(c)) {
        return Err(Refused::Hidden);
    }
    let url = Url::parse(raw).map_err(Refused::Unparsable)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Refused::Scheme(url.scheme().to_string()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Refused::Credentials);
    }
    // `http` and `https` are special schemes: the parser has already refused
    // one without a host, so a parsed URL here always names one.
    Ok(url)
}

/// Hand `raw` to `opener` if [`web_url`] passes it, as the parser serializes
/// it; a refused URL never reaches `opener`.
///
/// # Errors
/// Why `raw` was refused, or what `opener` failed with.
pub fn open<E: std::fmt::Display>(
    raw: &str,
    opener: impl FnOnce(&str) -> Result<(), E>,
) -> Result<(), String> {
    let url = web_url(raw).map_err(|refused| refused.to_string())?;
    opener(url.as_str()).map_err(|error| error.to_string())
}

/// The Unicode format characters that draw nothing or reorder text: a soft
/// hyphen, the zero-width and bidi marks, the bidi overrides and isolates,
/// the invisible operators, and the byte order mark.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{61c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
    )
}

#[cfg(test)]
mod tests {
    use super::{MAX_URL_BYTES, Refused, open, web_url};

    fn opens(raw: &str) -> String {
        web_url(raw)
            .unwrap_or_else(|refused| panic!("{raw:?} was refused: {refused}"))
            .into()
    }

    fn refused(raw: &str) -> Refused {
        match web_url(raw) {
            Ok(url) => panic!("{raw:?} was opened as {url}"),
            Err(refused) => refused,
        }
    }

    #[test]
    fn http_and_https_urls_open_as_the_parser_serializes_them() {
        assert_eq!(opens("https://example.com"), "https://example.com/");
        assert_eq!(opens("http://example.com/"), "http://example.com/");
        assert_eq!(opens("HTTPS://Example.COM/A"), "https://example.com/A");
        assert_eq!(
            opens("http://localhost:5173/runs/42?tab=logs&x=1#top"),
            "http://localhost:5173/runs/42?tab=logs&x=1#top"
        );
        assert_eq!(opens("https://127.0.0.1:8443/"), "https://127.0.0.1:8443/");
        assert_eq!(opens("http://[::1]:8080/a"), "http://[::1]:8080/a");
        assert_eq!(
            opens(
                "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e"
            ),
            "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e"
        );
    }

    #[test]
    fn an_international_host_opens_as_its_ascii_form() {
        assert_eq!(
            opens("https://münchen.de/straße"),
            "https://xn--mnchen-3ya.de/stra%C3%9Fe"
        );
        assert_eq!(opens("https://例え.jp/"), "https://xn--r8jz45g.jp/");
    }

    #[test]
    fn every_other_scheme_is_refused() {
        for (raw, scheme) in [
            ("file:///etc/passwd", "file"),
            ("javascript:alert(1)", "javascript"),
            ("JavaScript:alert(1)", "javascript"),
            ("data:text/html,<script>alert(1)</script>", "data"),
            ("mailto:someone@example.com", "mailto"),
            ("ftp://example.com/file", "ftp"),
            ("vscode://file/etc/passwd", "vscode"),
            (
                "x-apple.systempreferences:com.apple.preference",
                "x-apple.systempreferences",
            ),
            ("ssh://host", "ssh"),
            ("blob:https://example.com/uuid", "blob"),
        ] {
            assert_eq!(refused(raw), Refused::Scheme(scheme.to_string()), "{raw}");
        }
    }

    #[test]
    fn a_relative_or_empty_url_is_refused() {
        assert_eq!(refused(""), Refused::Length(0));
        for raw in [
            "example.com",
            "/etc/passwd",
            "//example.com/a",
            "?q=1",
            "https://",
            "http://:80/",
        ] {
            assert!(matches!(refused(raw), Refused::Unparsable(_)), "{raw}");
        }
    }

    #[test]
    fn whitespace_or_control_characters_anywhere_are_refused_not_stripped() {
        // The parser would drop each of these and read a web URL, or a
        // `javascript:` one the scheme check then sees: refused before either.
        for raw in [
            " https://example.com",
            "https://example.com ",
            "\thttps://example.com",
            "https://example.com\n",
            "ht\ttps://example.com",
            "java\nscript:alert(1)",
            "\u{0}javascript:alert(1)",
            "https://exa\rmple.com",
            "https://example.com/\u{a0}",
            "\u{feff}https://example.com",
            "https://exa\u{200b}mple.com/",
            "https://exa\u{ad}mple.com/",
            "https://example.com/\u{202e}txt.exe",
        ] {
            let got = refused(raw);
            assert!(
                matches!(got, Refused::Hidden | Refused::Unparsable(_)),
                "{raw:?}: {got:?}"
            );
        }
        assert_eq!(refused(" javascript:alert(1)"), Refused::Hidden);
    }

    #[test]
    fn a_user_name_or_password_before_the_host_is_refused() {
        assert_eq!(
            refused("https://bank.example@evil.example/"),
            Refused::Credentials
        );
        assert_eq!(
            refused("https://user:secret@example.com/"),
            Refused::Credentials
        );
        assert_eq!(refused("http://:secret@example.com/"), Refused::Credentials);
    }

    #[test]
    fn a_url_over_the_bound_is_refused_and_one_at_it_opens() {
        let prefix = "https://example.com/";
        let at_bound = format!("{prefix}{}", "a".repeat(MAX_URL_BYTES - prefix.len()));
        assert_eq!(opens(&at_bound), at_bound);
        let over = format!("{at_bound}a");
        assert_eq!(refused(&over), Refused::Length(MAX_URL_BYTES + 1));
    }

    #[test]
    fn only_a_passed_url_reaches_the_opener_and_as_the_parser_wrote_it() {
        let mut opened = Vec::new();
        let mut opener = |url: &str| {
            opened.push(url.to_string());
            Ok::<(), String>(())
        };
        assert_eq!(
            open("HTTPS://Example.com/a b", &mut opener),
            Err(Refused::Hidden.to_string())
        );
        assert!(open("javascript:alert(1)", &mut opener).is_err());
        assert!(open("file:///etc/passwd", &mut opener).is_err());
        assert_eq!(open("HTTPS://Example.com/a", &mut opener), Ok(()));
        assert_eq!(opened, ["https://example.com/a"]);

        let failed = open("https://example.com/", |_| Err("no browser"));
        assert_eq!(failed, Err("no browser".to_string()));
    }
}
