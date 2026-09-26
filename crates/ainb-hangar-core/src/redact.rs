//! Secret scrubbing: keeps credentials out of disk, logs, frames and
//! transcripts.
//!
//! Lives here, below every consumer, because the transcript classifier in
//! `ainb-hangar-proto` must scrub before it cuts (#1187) and cannot reach
//! `ainb-app`, which re-exports this module where its callers always found it.
//!
//! The phone bridge persists its most-recent error to `daemons/bridge.json`
//! (`last_error`), which the `ainb fleet daemons` CLI verb and the TUI Daemons
//! screen render, and every channel also logs error diagnostics. A
//! `reqwest::Error`'s `Display` includes the request URL, and the Telegram Bot
//! API embeds the bot token IN the URL path
//! (`https://api.telegram.org/bot<TOKEN>/getUpdates`). Slack and Discord tokens
//! can likewise reach a diagnostic string. Letting any of those reach `last_error`
//! or a log leaks the token to disk and to anyone watching the surface.
//!
//! [`scrub`] is the single defense-in-depth sink shared by all three channels
//! (Telegram, Slack, Discord) and the heartbeat error recorder: every diagnostic
//! string is run through it before it is recorded or logged, so even a future
//! code path that forgets to build a clean message can't leak a known token
//! shape. It is a pure string transform (no allocation when nothing matches) so
//! it is cheap and exhaustively testable.
//!
//! The mirror frame (issue #983) reuses the same sink for text it cannot drop:
//! captured tmux scrollback, diff hunks and agent prose. So it also knows the
//! credential shapes an operator's terminal or repo carries (Anthropic, OpenAI,
//! GitHub, GitLab, AWS, Google, PEM private keys, JWTs, and a password in a URL),
//! and [`find_secret`] exposes the same table as a tripwire for tests over a
//! serialised frame.
//!
//! Named credentials (a header like `X-Api-Key`, a variable like
//! `CLIENT_SECRET`) share one name rule, [`is_secret_name`], between the text
//! shapes and [`scrub_json`]'s object keys, and one value rule,
//! `is_secret_value`: under a credential's name every value is the secret;
//! under a generic `_KEY` name (`SORT_KEY`, `X-Idempotency-Key`) only an
//! opaque one is. What it does NOT cover, on purpose or not yet:
//!
//! - lower-case and camel-case names in free text (`api_token=...`,
//!   `password: ...`): too close to ordinary code and prose to match by name.
//!   As JSON keys they are covered: the list in [`is_secret_json_key`]
//!   (`password`, `apiKey`, `access-token`, ...) ignoring case, `_` and `-`,
//!   and any camel-case or lower snake key whose upper snake case is a
//!   credential's name (`dbPassword`, `db_password`, `github_token`). A
//!   generic key (`session_key`, `sortKey`) and a paging cursor
//!   (`next_token`, `pageToken`) are not;
//! - YAML and other `NAME: value` forms of an environment name
//!   (`DB_PASSWORD: hunter2`), and a spaced `NAME = value`;
//! - bare `PASSWORD=`, `KEY=` and `PWD=` (only the suffixed forms, and bare
//!   `SECRET=` and `TOKEN=`);
//! - in free text, a value under 16 characters, or one starting with `/`,
//!   `~`, `.`, `$` or `<`;
//! - under a generic key's name, a value with no digit, a UUID or a
//!   snake_case name (a real secret filed under `SORT_KEY` stays);
//! - a number or boolean under a secret-named JSON key.

use std::sync::LazyLock;

use regex::Regex;

/// Telegram bot tokens: `bot<digits>:<base64ish>` (as they appear in the API
/// URL path) and the bare `<digits>:<base64ish>` token form.
static TELEGRAM_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"bot[0-9]+:[A-Za-z0-9_-]{20,}").expect("valid telegram token regex")
});
/// `[0-9]` rather than `\d` for the reason the Discord shape spells its class
/// out: `\d` is Unicode-aware, and a bounded repetition of it with no literal
/// prefix is the expensive shape over a long line. A token's id is ASCII
/// digits, so nothing that is a token stops matching.
static TELEGRAM_BARE_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[0-9]{6,}:[A-Za-z0-9_-]{20,}").expect("valid bare telegram token regex")
});
/// Slack tokens: bot (`xoxb-…`), the `xox*` families (user `xoxp-`, config
/// `xoxe-`, refresh `xoxr-`, …) AND app-level tokens (`xapp-…`), which use a
/// distinct `xapp` prefix rather than `xox`.
static SLACK_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:xox[bapcdrse]|xapp)-[A-Za-z0-9-]+").expect("valid slack token regex")
});
/// Discord bot tokens: three base64url segments of roughly 24+ / 6–12 / 27+
/// chars (`<user-id>.<timestamp>.<hmac>`). The middle (timestamp) segment is a
/// BOUNDED RANGE, not a fixed 6, because newer Discord tokens widen it (a
/// 7-char middle is already in the wild) and a fixed `{6}` silently failed to
/// redact those, leaking the token into `last_error`/logs. The `6,12` ceiling
/// keeps it conservative so it still won't eat a short dotted version string.
/// The class is spelled out rather than `\w` because `\w` is Unicode-aware:
/// with it, three bounded repetitions over a class of hundreds of thousands of
/// characters cost 3.5 ms on a 4,000-character line, which was 99% of a mirror
/// frame's whole scrub, and the same shape over ASCII costs 0.7 us. A token's
/// segments are base64url, so nothing that is a token stops matching; runs of
/// Unicode letters, which are not tokens, stop being false positives.
static DISCORD_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9_-]{24,}\.[A-Za-z0-9_-]{6,12}\.[A-Za-z0-9_-]{27,}")
        .expect("valid discord token regex")
});
/// A PEM private key: the whole armoured block when it is closed, the
/// header and everything after it when the capture cut it off.
static PEM_PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----(?s:.*?)(?:-----END [A-Z ]*PRIVATE KEY-----|\z)")
        .expect("valid pem regex")
});
/// Anthropic API and admin keys.
static ANTHROPIC_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"sk-ant-[A-Za-z0-9_-]{20,}").expect("valid anthropic key regex"));
/// `OpenAI` keys, legacy `sk-…` and project, service-account and admin
/// forms. Word-anchored: without `\b` a session id like `ainb-task-<uuid>`
/// or a branch like `fix-risk-assessment-…` matched from the `sk-` inside it.
static OPENAI_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_-]{32,}")
        .expect("valid openai key regex")
});
/// An AWS secret access key: 40 base64 characters carry no prefix of their
/// own, so the shape is the assignment that names one. Group 1 keeps the
/// name so a scrubbed `.env` line still says what was there.
static AWS_SECRET_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r#"((?i:aws_secret_access_key|aws_secret_key|secret_access_key|secretaccesskey)["']?\s*[:=]\s*["']?)[A-Za-z0-9/+=]{40}"#
    )
    .expect("valid aws secret key regex")
});
/// Stripe secret and restricted keys, live and test.
static STRIPE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{16,}").expect("valid stripe key regex")
});
/// npm access tokens.
static NPM_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bnpm_[A-Za-z0-9]{36}\b").expect("valid npm token regex"));
/// `PyPI` upload tokens (a macaroon, always `pypi-AgEIcHlwaS5vcmc…`).
static PYPI_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bpypi-AgEIcHlwaS5vcmc[A-Za-z0-9_-]{50,}").expect("valid pypi token regex")
});
/// Hugging Face user access tokens.
static HUGGING_FACE_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bhf_[A-Za-z0-9]{34,}\b").expect("valid hugging face token regex")
});
/// `DigitalOcean` personal access tokens.
static DIGITALOCEAN_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bdo[por]_v1_[a-f0-9]{64}\b").expect("valid digitalocean token regex")
});
/// `SendGrid` API keys: `SG.<22>.<43>`.
static SENDGRID_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bSG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43}\b").expect("valid sendgrid key regex")
});
/// PEM armour lines, for input that arrives one line at a time.
static PEM_BEGIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").expect("valid pem begin regex")
});
static PEM_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-----END [A-Z ]*PRIVATE KEY-----").expect("valid pem end regex"));
/// ANSI CSI and OSC escape sequences, as `tmux capture-pane -e` keeps them.
static ANSI_ESCAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b(?:\[[0-9;?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\))")
        .expect("valid ansi escape regex")
});
/// GitHub classic (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`) and fine-grained tokens.
static GITHUB_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{80,}")
        .expect("valid github token regex")
});
/// GitLab personal access tokens.
static GITLAB_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"glpat-[A-Za-z0-9_-]{20,}").expect("valid gitlab token regex"));
/// AWS access key ids (long-term `AKIA`, temporary `ASIA`).
static AWS_ACCESS_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b").expect("valid aws key regex"));
/// Google API keys.
static GOOGLE_API_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"AIza[0-9A-Za-z_-]{35}").expect("valid google key regex"));
/// JSON Web Tokens: base64url header and payload, both starting `ey`.
static JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"ey[A-Za-z0-9_-]{10,}\.ey[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]*")
        .expect("valid jwt regex")
});
/// A password (or token) in a URL's userinfo: `scheme://user:secret@host`.
/// Group 1 keeps the scheme so a scrubbed clone URL still reads as one.
static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([A-Za-z][A-Za-z0-9+.-]*://)[^/\s:@]+:[^/\s@]+@")
        .expect("valid url userinfo regex")
});
/// An HTTP `Authorization` (or `Proxy-Authorization`) header's credential,
/// as a curl flag, a header dump or a JSON field carries it. Group 1 keeps the
/// header name so the scrubbed text still says what was there. The value is
/// a scheme and a token of 8 or more characters, or, with no scheme, 20 or
/// more, so prose such as "authorization: required" is left alone.
static AUTHORIZATION_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b((?:proxy-)?authorization["']?\s*[:=]\s*["']?)(?:(?:bearer|basic|token|digest|negotiate)\s+[A-Za-z0-9._~+/=-]{8,}|[A-Za-z0-9._~+/=-]{20,})"#,
    )
    .expect("valid authorization header regex")
});
/// A bearer token outside a header (`Bearer <opaque>` in a config, a log, a
/// variable). Group 1 keeps the scheme word; 16 or more token characters, so
/// "bearer of bad news" is left alone.
static BEARER_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{16,}").expect("valid bearer token regex")
});
/// Header names that carry a credential by themselves: `Authorization`,
/// `X-Api-Key`, `X-Auth-Token`, any `X-...-Key`, `-Token` or `-Secret`, and the
/// bare `api-key`, `api-token`, `auth-token`, `access-token`, `private-token`
/// (GitLab) and lower-case `apikey`. Case-insensitive, as HTTP header names
/// are, except `apikey`: every other name carries a hyphen, so a camel-case
/// identifier in code (`apiKey: SomeType`) is never one.
///
/// Half of the one secret-name rule, with [`SECRET_ENV_NAME`]: the text
/// shapes embed both, and [`is_secret_name`] (which [`scrub_json`] asks of
/// every object key) matches the same two, whole.
const SECRET_HEADER_NAME: &str = r"(?i:(?:proxy-)?authorization|x-[a-z0-9-]*(?:key|token|secret)|api-key|api-token|auth-token|access-token|private-token)|apikey";
/// Environment-style names that carry a credential: upper case, ending in
/// `_TOKEN`, `_SECRET`, `_KEY`, `_PASSWORD`, `_PASSWD` or `_PWD`, or the bare
/// `SECRET` and `TOKEN`. The other half of the secret-name rule.
const SECRET_ENV_NAME: &str =
    r"[A-Z][A-Z0-9_]*_(?:TOKEN|SECRET|KEY|PASSWORD|PASSWD|PWD)|SECRET|TOKEN";

/// The whole-name form of the rule, for a JSON object key.
static SECRET_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!("^(?:{SECRET_HEADER_NAME}|{SECRET_ENV_NAME})$"))
        .expect("valid secret name regex")
});
/// A version 1 to 8 UUID, the canonical 8-4-4-4-12 hex form.
static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$")
        .expect("valid uuid regex")
});
/// A snake_case identifier: two or more lower-case words joined by `_`, each
/// word letters then optional trailing digits (`created_at_desc`,
/// `user_profile_v2`). A key's random body (`sk_live_51Hq...`,
/// `whsec_4f9k...`) has a word that starts with a digit or mixes them, so it
/// is never one.
static SNAKE_CASE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z]+[0-9]*(?:_[a-z]+[0-9]*)+$").expect("valid snake case regex")
});
/// A real environment reference, whole: `$VAR` or `${VAR}` with an
/// upper-case name, the environment's convention. A `${VAR:-x}` default, a
/// password that happens to start with `$` (`$ecretP4ss` is valid shell but
/// not a name anyone exports), or any other text is not one.
static ENV_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\$(?:[A-Z_][A-Z0-9_]*|\{[A-Z_][A-Z0-9_]*\})$").expect("valid env reference regex")
});

/// JSON object keys that name a credential, in normal form (see
/// [`json_key_normal_form`]) and compared whole: `api_key` and `apikey` are
/// one entry. JSON only: in free text these
/// words are ordinary prose and code (`password: ...` in a docstring,
/// `api_key = config.get(...)`), but as an object key they are what the value
/// is.
const SECRET_JSON_KEYS: [&str; 10] = [
    "password",
    "passwd",
    "secret",
    "clientsecret",
    "apikey",
    "accesstoken",
    "refreshtoken",
    "idtoken",
    "privatekey",
    "authtoken",
];

/// A JSON key in normal form for [`SECRET_JSON_KEYS`]: lower case, with `_`
/// and `-` removed, so `access_token`, `accessToken`, `Access-Token` and
/// `ACCESS_TOKEN` compare equal.
fn json_key_normal_form(key: &str) -> String {
    key.chars()
        .filter(|c| !matches!(c, '_' | '-'))
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// A camel-case or hyphenated name in the upper snake case the environment
/// rule reads: `dbPassword` is `DB_PASSWORD`, `X-Idempotency-Key` is
/// `X_IDEMPOTENCY_KEY`, and `SORT_KEY` is itself.
fn upper_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let mut previous: Option<char> = None;
    for c in name.chars() {
        if c.is_ascii_uppercase()
            && previous.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
        {
            out.push('_');
        }
        out.push(if c == '-' {
            '_'
        } else {
            c.to_ascii_uppercase()
        });
        previous = Some(c);
    }
    out
}

/// Whether a JSON object key names a credential: [`is_secret_name`] as
/// written, one of [`SECRET_JSON_KEYS`] in normal form (`password`, `apiKey`,
/// `access-token`), or a key whose upper snake case is a credential's name
/// (`dbPassword` and `db_password` as `DB_PASSWORD`, `clientSecret`,
/// `github_token`).
///
/// The upper snake case path stops at a credential's name: every lower snake
/// key reads as an environment name there, and a generic one (`session_key`
/// as `SESSION_KEY`, `sort_key`) is not taken as naming a secret.
#[must_use]
pub fn is_secret_json_key(key: &str) -> bool {
    is_secret_name(key) || SECRET_JSON_KEYS.contains(&json_key_normal_form(key).as_str()) || {
        let name = upper_snake(key);
        is_secret_name(&name) && !is_generic_key_name(&name)
    }
}

/// Whether `name`, in upper snake case, has `word` as one of its
/// `_`-delimited words: `CAPITAL_KEY` has `CAPITAL` and `KEY`, not `API`.
fn has_word(name: &str, words: &[&str]) -> bool {
    upper_snake(name).split('_').any(|part| words.contains(&part))
}

/// Words that make a `_KEY` or `X-...-Key` name a credential's name rather
/// than a generic key's (`API_KEY`, `PRIVATE_KEY`, `X-Api-Key` against
/// `SORT_KEY`, `PARTITION_KEY`, `X-Idempotency-Key`), matched as whole words
/// so `CAPITAL_KEY` is not an API key. `PASSWORD` and `PASSWD` are listed
/// with `PASS` because a whole-word match no longer finds `PASS` in them.
const CREDENTIAL_WORDS: [&str; 11] = [
    "API", "SECRET", "TOKEN", "PASS", "PASSWORD", "PASSWD", "PWD", "AUTH", "ACCESS", "PRIVATE",
    "CLIENT",
];

/// Words that mark a name as a paging cursor rather than a credential:
/// `NEXT_TOKEN`, `nextToken`, `pageToken`, `continuation_token`. A cursor is
/// opaque and ends in `TOKEN`, but it only says where a listing resumes.
const CURSOR_WORDS: [&str; 6] = ["NEXT", "PAGE", "PREV", "PREVIOUS", "CURSOR", "CONTINUATION"];

/// Whether a secret-named `name` is only a generic key: it ends in `_KEY`
/// (or is an `X-...-Key` header, the same in upper snake case) and has none
/// of [`CREDENTIAL_WORDS`] as a word. `SORT_KEY`, `PARTITION_KEY`,
/// `CAPITAL_KEY` and `X-Idempotency-Key` are; every other secret name is a
/// credential's.
fn is_generic_key_name(name: &str) -> bool {
    upper_snake(name).ends_with("_KEY") && !has_word(name, &CREDENTIAL_WORDS)
}

/// Whether a value is an identifier rather than a credential: a UUID or a
/// snake_case name.
fn is_identifier(value: &str) -> bool {
    UUID.is_match(value) || SNAKE_CASE.is_match(value)
}

/// Whether `value`, found under the secret name `name`, is the credential.
///
/// The one value rule for the text shapes and for a secret-named JSON key.
/// Under a credential's name (`API_KEY`, `client_secret`, `DB_PASSWORD`,
/// `X-Api-Key`) it is, whatever it looks like: a Heroku API key is a UUID,
/// and a passphrase can be `correct_horse_battery_staple`. Only under a
/// generic key's name ([`is_generic_key_name`]) does the value have to look
/// opaque: a digit, and not an identifier ([`is_identifier`]), so a sort key
/// or a tenant id stays.
fn is_secret_value(name: &str, value: &str) -> bool {
    !is_generic_key_name(name)
        || (value.bytes().any(|b| b.is_ascii_digit()) && !is_identifier(value))
}

/// Whether `name` (a header, a variable or a JSON key, whole) names a
/// credential: it matches [`SECRET_HEADER_NAME`] or [`SECRET_ENV_NAME`], and
/// has neither the word `PUBLIC` (`NEXT_PUBLIC_API_KEY`, `STRIPE_PUBLIC_KEY`,
/// a `X-Public-Key`), which by its own name is meant to be seen, nor a
/// [`CURSOR_WORDS`] word (`NEXT_TOKEN`, `PAGE_TOKEN`), which names a paging
/// cursor.
#[must_use]
pub fn is_secret_name(name: &str) -> bool {
    SECRET_NAME.is_match(name) && !has_word(name, &["PUBLIC"]) && !has_word(name, &CURSOR_WORDS)
}

/// An API key in a header named by [`SECRET_HEADER_NAME`], as a curl flag, a
/// header dump or a JSON-looking line writes it. Group 1 keeps the header
/// name; the value is 16 or more token characters and must pass
/// [`is_secret_value`], so "x-api-key: required", a placeholder and an
/// `X-Idempotency-Key` id are left alone, and so is a value followed by `(`,
/// which is a call (`apiKey: getKey()`). `Authorization` values are the
/// [`AUTHORIZATION_HEADER`] shape's, which runs first.
static API_KEY_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"\b(?P<keep>(?P<name>{SECRET_HEADER_NAME})["']?\s*:\s*["']?)(?P<value>[A-Za-z0-9_.~+/=-]{{16,}})(?P<call>\()?"#
    ))
    .expect("valid api key header regex")
});
/// A credential in an environment assignment named by [`SECRET_ENV_NAME`],
/// as a shell line, an `env` prefix or a `.env` file writes it. Group 1 keeps
/// the name. Upper-case names and no space around `=`, the shell's own form,
/// so a code assignment such as `SECRET_KEY = load_secret()` is left alone.
/// The value is 16 or more token characters, does not start with `/`, `~`,
/// `.`, `$` or `<` (a key file's path, a `$VAR` or `${VAR:-...}` expansion, a
/// placeholder), is not followed by `(` (a call:
/// `SECRET_KEY=load_secret_from_vault()` keeps the call it names), and must
/// pass [`is_secret_value`].
static SECRET_ENV_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"\b(?P<keep>(?:export\s+)?(?P<name>{SECRET_ENV_NAME})=["']?)(?P<value>[A-Za-z0-9+_-][A-Za-z0-9_.~+/=:@-]{{15,}})(?P<call>\()?"#
    ))
    .expect("valid secret env assignment regex")
});

/// Whether one match of the shape `name` is a secret. Most shapes are
/// decided by their regex alone; the two named-credential shapes also check
/// the captured name against [`is_secret_name`] (the `PUBLIC` and cursor
/// exceptions), refuse a value followed by `(`, and check the value against
/// [`is_secret_value`], which a regex without look-around cannot say.
fn accepted(name: &str, caps: &regex::Captures<'_>) -> bool {
    match name {
        "api key header" | "secret env assignment" => {
            if caps.name("call").is_some() {
                return false;
            }
            match (caps.name("name"), caps.name("value")) {
                (Some(n), Some(v)) => {
                    is_secret_name(n.as_str()) && is_secret_value(n.as_str(), v.as_str())
                }
                _ => false,
            }
        }
        _ => true,
    }
}

/// Whether a shape needs [`accepted`] to confirm a match.
fn checked(name: &str) -> bool {
    matches!(name, "api key header" | "secret env assignment")
}

/// Every credential shape [`scrub`] removes, by name, in the order it runs.
///
/// PEM runs first so a key block is removed whole before a narrower pattern
/// eats a line of its body, and the Anthropic shape runs before the `sk-` one
/// so an `sk-ant-` key is named for what it is. The header, bearer and
/// environment shapes run last, after every named token shape, so a known
/// token inside a header or an assignment is still named for what it is.
fn shapes() -> [(&'static str, &'static Regex); 24] {
    [
        ("pem private key", &PEM_PRIVATE_KEY),
        ("telegram bot token", &TELEGRAM_TOKEN),
        ("telegram token", &TELEGRAM_BARE_TOKEN),
        ("slack token", &SLACK_TOKEN),
        ("discord token", &DISCORD_TOKEN),
        ("anthropic key", &ANTHROPIC_KEY),
        ("openai key", &OPENAI_KEY),
        ("github token", &GITHUB_TOKEN),
        ("gitlab token", &GITLAB_TOKEN),
        ("aws access key", &AWS_ACCESS_KEY),
        ("aws secret key", &AWS_SECRET_KEY),
        ("google api key", &GOOGLE_API_KEY),
        ("stripe key", &STRIPE_KEY),
        ("npm token", &NPM_TOKEN),
        ("pypi token", &PYPI_TOKEN),
        ("hugging face token", &HUGGING_FACE_TOKEN),
        ("digitalocean token", &DIGITALOCEAN_TOKEN),
        ("sendgrid key", &SENDGRID_KEY),
        ("jwt", &JWT),
        ("url userinfo", &URL_USERINFO),
        ("authorization header", &AUTHORIZATION_HEADER),
        ("bearer token", &BEARER_TOKEN),
        ("api key header", &API_KEY_HEADER),
        ("secret env assignment", &SECRET_ENV_ASSIGNMENT),
    ]
}

/// The first credential shape found in `input`, as `(shape name, matched text)`.
///
/// The value-shaped tripwire over a serialised frame: independent of field
/// names, so it catches a secret that arrived through a path nobody listed.
#[must_use]
pub fn find_secret(input: &str) -> Option<(&'static str, String)> {
    shapes().into_iter().find_map(|(name, re)| {
        if checked(name) {
            re.captures_iter(input)
                .find(|caps| accepted(name, caps))
                .map(|caps| (name, caps[0].to_string()))
        } else {
            re.find(input).map(|m| (name, m.as_str().to_string()))
        }
    })
}

/// Replacement marker substituted for any matched secret. Stable so callers and
/// tests can assert on it.
pub const REDACTED: &str = "<redacted>";

/// Scrub every known credential shape (see [`find_secret`]) out of a string.
///
/// Runs before a string is persisted, logged or put in a mirror frame. Order matters: the
/// more specific `bot…:…` Telegram form is replaced before the bare
/// `digits:base64` form, so the `bot` prefix is also removed, and a URL keeps
/// its scheme and host with only the userinfo replaced. Returns a new string;
/// secret-free inputs round-trip unchanged.
///
/// Captured panes keep their colour codes, and an SGR sequence inside a token
/// (a highlighted `.env`, a coloured prompt) splits it past every shape. When
/// the text has escapes and a shape only appears once they are stripped, the
/// stripped text is scrubbed and returned: the colours are lost only on the
/// capture that actually held a secret.
#[must_use]
pub fn scrub(input: &str) -> String {
    let scrubbed = scrub_shapes(input);
    if !input.contains('\x1b') {
        return scrubbed;
    }
    let plain = ANSI_ESCAPE.replace_all(&scrubbed, "");
    if find_secret(&plain).is_some() {
        scrub_shapes(&plain)
    } else {
        scrubbed
    }
}

/// Scrub a sequence of lines that together form one text (a diff, an editor,
/// an argument list), keeping one output line per input line.
///
/// [`scrub`] on each line alone would redact a PEM header and let every base64
/// body line through, so a private-key block is tracked across lines: every
/// line from `BEGIN` to `END` (or to the last line) becomes [`REDACTED`].
#[must_use]
pub fn scrub_lines<S: AsRef<str>>(lines: &[S]) -> Vec<String> {
    let mut in_key = false;
    scrub_lines_from(lines, &mut in_key)
}

/// [`scrub_lines`] over one chunk of a longer text, carrying the key-block flag
/// in `in_key` so the caller can stop part way.
///
/// A caller that frames only what fits a budget would otherwise scrub the whole
/// text to throw most of it away; with this it scrubs a chunk at a time and
/// stops, and a key block still spans the chunk boundary.
#[must_use]
pub fn scrub_lines_from<S: AsRef<str>>(lines: &[S], in_key: &mut bool) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            let line = line.as_ref();
            if *in_key {
                if let Some(end) = PEM_END.find(line) {
                    *in_key = false;
                    return format!("{REDACTED}{}", scrub(&line[end.end()..]));
                }
                return REDACTED.to_string();
            }
            match PEM_BEGIN.find(line) {
                Some(begin) if !PEM_END.is_match(&line[begin.end()..]) => {
                    *in_key = true;
                    format!("{}{REDACTED}", scrub(&line[..begin.start()]))
                }
                _ => scrub(line),
            }
        })
        .collect()
}

/// Scrub every string value in a JSON document, in place.
///
/// For a structured payload that leaves the process as JSON (a transcript
/// chunk on the wire, #1199). Scrubbing each string on its own, rather than
/// the serialised text, keeps the document valid: an unclosed PEM block in
/// one value ends at that value instead of eating every field after it, and
/// a value is scrubbed as the text it decodes to, not its escaped form.
///
/// Object keys are left as they are. They are the producer's structure
/// (`content`, `rawInput`), not operator text, and rewriting one would change
/// the shape every reader parses against.
///
/// Keys are also read: a field whose key [`is_secret_json_key`]
/// (`X-Api-Key`, `CLIENT_SECRET`, `DB_PASSWORD` by the rule the text shapes
/// use, and `password`, `api_key`, `access_token` and the rest of the JSON
/// key list in any case) has the strings under it replaced, because a bare
/// value in a headers or env map (`{"CLIENT_SECRET": "..."}`) carries no
/// `NAME=` or `Name:` for a text shape to find.
pub fn scrub_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            if find_secret(text).is_some() {
                *text = scrub(text);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(scrub_json),
        serde_json::Value::Object(fields) => {
            for (key, field) in fields.iter_mut() {
                if is_secret_json_key(key) {
                    redact_every_string(key, field);
                } else {
                    scrub_json(field);
                }
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

/// Replace the strings under the field `key`, which [`is_secret_json_key`],
/// with [`REDACTED`]. Kept, since none is the credential itself: an empty
/// string, a whole `$VAR` or `${VAR}` reference and a `<placeholder>`.
/// Otherwise the key decides through [`is_secret_value`], the rule the text
/// shapes use: under a credential's name every value goes, a short, a
/// digit-less, a UUID or a snake_case one included; under a generic key's
/// name only an opaque one does. A value holding a known credential shape is
/// redacted whatever the key.
fn redact_every_string(key: &str, value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            let reference = text.is_empty()
                || ENV_REFERENCE.is_match(text)
                || (text.starts_with('<') && text.ends_with('>'));
            let kept = find_secret(text).is_none() && (reference || !is_secret_value(key, text));
            if !kept {
                *text = REDACTED.to_string();
            }
        }
        serde_json::Value::Array(items) => {
            items.iter_mut().for_each(|item| redact_every_string(key, item));
        }
        serde_json::Value::Object(fields) => {
            fields.values_mut().for_each(|item| redact_every_string(key, item));
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn scrub_shapes(input: &str) -> String {
    let mut out = std::borrow::Cow::Borrowed(input);
    for (name, re) in shapes() {
        if !re.is_match(&out) {
            continue;
        }
        let replaced = re
            .replace_all(&out, |caps: &regex::Captures<'_>| {
                if !accepted(name, caps) {
                    return caps[0].to_string();
                }
                match name {
                    "url userinfo" => format!("{}{REDACTED}@", &caps[1]),
                    "aws secret key"
                    | "authorization header"
                    | "bearer token"
                    | "api key header"
                    | "secret env assignment" => format!("{}{REDACTED}", &caps[1]),
                    _ => REDACTED.to_string(),
                }
            })
            .into_owned();
        out = std::borrow::Cow::Owned(replaced);
    }
    out.into_owned()
}

/// Telegram/Slack-oriented alias for [`scrub`]. Retained so the heartbeat error
/// sink and the Telegram/Slack channels read intent-fully; it scrubs every known
/// token shape, not just Telegram/Slack ones.
#[must_use]
pub fn scrub_secrets(input: &str) -> String {
    scrub(input)
}

/// Discord-oriented alias for [`scrub`]. Retained so the Discord channel reads
/// intent-fully; it scrubs every known token shape, not just Discord ones.
#[must_use]
pub fn scrub_token(text: &str) -> String {
    scrub(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_telegram_token_in_api_url() {
        // The exact leak shape: a reqwest error Display carrying the bot token in
        // the getUpdates URL path.
        let leak = "kind=connect status=None display=\"error sending request for url \
                    (https://api.telegram.org/bot123456789:ABC-DEF_ghiJKLmnopqrstuvwxyz012345/getUpdates)\" source=[]";
        let scrubbed = scrub_secrets(leak);
        assert!(
            scrubbed.contains(REDACTED),
            "expected redaction: {scrubbed}"
        );
        assert!(
            !scrubbed.contains("ABC-DEF_ghiJKLmnopqrstuvwxyz012345"),
            "token body leaked: {scrubbed}"
        );
        assert!(
            !scrubbed.contains("bot123456789:"),
            "token prefix leaked: {scrubbed}"
        );
    }

    #[test]
    fn scrubs_bare_telegram_token() {
        let leak = "getUpdates: 123456789:ABCdefGHIjklMNOpqrstuvwx failed";
        let scrubbed = scrub_secrets(leak);
        assert!(scrubbed.contains(REDACTED));
        assert!(!scrubbed.contains("ABCdefGHIjklMNOpqrstuvwx"));
        assert!(!scrubbed.contains("123456789:"));
    }

    #[test]
    fn scrubs_slack_bot_and_app_tokens() {
        let leak =
            "socket error: auth failed for xoxb-1111-2222-aaaaBBBBcccc and xapp-1-A0-99-deadbeef";
        let scrubbed = scrub_secrets(leak);
        assert!(
            !scrubbed.contains("xoxb-1111-2222-aaaaBBBBcccc"),
            "{scrubbed}"
        );
        assert!(!scrubbed.contains("xapp-1-A0-99-deadbeef"), "{scrubbed}");
        assert_eq!(scrubbed.matches(REDACTED).count(), 2);
    }

    #[test]
    fn leaves_secret_free_strings_untouched() {
        let clean = "kind=timeout status=Some(429) display=\"operation timed out\" source=[]";
        assert_eq!(scrub_secrets(clean), clean);
    }

    #[test]
    fn does_not_redact_innocuous_colon_numbers() {
        // A short `id:value` like an http status or a chat id must NOT be eaten:
        // the bare-token rule requires >=6 leading digits AND a >=20-char tail.
        let clean = "sendMessage HTTP 400: chat_id 42 not found";
        assert_eq!(scrub_secrets(clean), clean);
    }

    // Synthetic placeholders: each matches the Discord redaction regex (three
    // `[\w-]{24+}.{6}.{27+}` segments) but is obviously not a real token, so
    // secret scanners don't flag this file.
    #[test]
    fn redacts_a_discord_bot_token() {
        let token = "fake-user-id-segment-xxxx.tttttt.fake-hmac-segment-yyyyyyyyyy";
        let msg = format!("auth failed with Bot {token} (401)");
        let scrubbed = scrub_token(&msg);
        assert!(
            !scrubbed.contains(token),
            "token must not survive scrubbing"
        );
        assert!(scrubbed.contains(REDACTED));
        assert!(scrubbed.contains("(401)"), "non-token text is preserved");
    }

    #[test]
    fn redacts_token_anywhere_in_the_string() {
        let token = "placeholder-first-segment-zz.midseg.placeholder-third-segment-w";
        let scrubbed = scrub_token(&format!("prefix {token} suffix"));
        assert_eq!(scrubbed, format!("prefix {REDACTED} suffix"));
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        let msg = "HTTP 500 (code 50001: Missing Access)";
        assert_eq!(scrub_token(msg), msg);
        // A short dotted identifier (e.g. a version) is not token-shaped.
        assert_eq!(scrub_token("v10.0.1"), "v10.0.1");
    }

    #[test]
    fn redacts_discord_token_with_seven_char_middle_segment() {
        // REGRESSION: newer Discord tokens carry a 7-char (not 6) middle segment.
        // The old `[\w-]{6}` middle pinned exactly 6 and silently let these
        // through, leaking the token into last_error/logs. The bounded `{6,12}`
        // middle must now catch it.
        let token = "AAAAAAAAAAAAAAAAAAAAAAAAA.BBBBBBB.CCCCCCCCCCCCCCCCCCCCCCCCCCC";
        let scrubbed = scrub_token(&format!("auth failed with Bot {token} (401)"));
        assert!(
            !scrubbed.contains(token),
            "7-char-middle token must be redacted: {scrubbed}"
        );
        assert!(scrubbed.contains(REDACTED));
        assert!(scrubbed.contains("(401)"), "non-token text is preserved");
    }

    // Synthetic credential shapes, assembled at runtime so no literal in this
    // file matches a secret scanner.
    fn fake(prefix: &str, body: char, len: usize) -> String {
        format!("{prefix}{}", body.to_string().repeat(len))
    }

    /// Scrubbing twice is scrubbing once. Load-bearing: the daemon scrubs a
    /// transcript chunk before it ships (#1199) and the classifier scrubs the
    /// text again before it cuts (#1187), so a second pass that rewrote the
    /// marker or re-matched the text around it would change what renders.
    #[test]
    fn scrub_is_idempotent_over_every_shape() {
        let shapes = [
            fake("sk-ant-api03-", 'A', 40),
            fake("sk-", 'b', 48),
            fake("ghp_", 'C', 36),
            fake("github_pat_", 'd', 82),
            fake("glpat-", 'e', 20),
            fake("AKIA", 'F', 16),
            fake("AIza", 'g', 35),
            format!(
                "-----BEGIN RSA PRIVATE KEY-----\n{}\n-----END RSA PRIVATE KEY-----",
                fake("", 'h', 64)
            ),
            format!("-----BEGIN PRIVATE KEY-----\n{}", fake("", 'u', 64)),
            format!(
                "{}.{}.{}",
                fake("eyJ", 'i', 20),
                fake("eyJ", 'j', 20),
                fake("", 'k', 20)
            ),
            format!(
                "https://x-access-token:{}@github.com/o/r",
                fake("", 'l', 12)
            ),
            fake("sk_live_", 'S', 24),
            fake("rk_live_", 'R', 24),
            fake("npm_", 'N', 36),
            fake("pypi-AgEIcHlwaS5vcmc", 'P', 60),
            fake("hf_", 'H', 34),
            fake("dop_v1_", 'a', 64),
            format!("{}.{}", fake("SG.", 'G', 22), fake("", 'g', 43)),
            fake("xoxb-", '1', 40),
            fake("xapp-", '2', 40),
            fake("AWS_SECRET_ACCESS_KEY=", 'w', 40),
            format!(
                "https://api.telegram.org/{}/getUpdates",
                fake("bot123456789:", 'T', 35)
            ),
            fake("123456789:", 't', 35),
            format!(
                "{}.{}.{}",
                fake("", 'D', 24),
                fake("", 'e', 7),
                fake("", 'f', 27)
            ),
            fake("Authorization: Bearer ", 'o', 40),
            fake("Bearer ", 'b', 32),
            fake("X-Api-Key: k1", 'k', 30),
            fake("SERVICE_TOKEN=v1", 'v', 30),
            fake("DB_PASSWORD=p4", 'p', 30),
        ];
        for shape in &shapes {
            let text = format!("before {shape} after");
            let once = scrub(&text);
            assert_eq!(scrub(&once), once, "a second pass changed {once:?}");
            assert_eq!(find_secret(&once), None, "the first pass left {once:?}");
        }
        let all = shapes.join(" | ");
        let once = scrub(&all);
        assert_eq!(scrub(&once), once, "a second pass changed the joined text");

        let mut value = serde_json::json!({ "argv": shapes, "text": all });
        scrub_json(&mut value);
        let first = value.clone();
        scrub_json(&mut value);
        assert_eq!(
            value, first,
            "a second scrub_json pass changed the document"
        );
    }

    /// An opaque bearer token matches no named shape, so before these two
    /// patterns `Authorization: Bearer <opaque>` passed through verbatim. The
    /// header's credential goes in every spelling a transcript carries it; the
    /// header name and a bearer scheme word stay, and prose is left alone.
    #[test]
    fn scrubs_authorization_headers_and_bearer_tokens() {
        let opaque = "a8Fz0Qk2LrT9vYx7Wm3Nc5Pd";
        for (text, kept) in [
            (
                format!("curl -H 'Authorization: Bearer {opaque}' https://api.example"),
                "curl -H 'Authorization: <redacted>' https://api.example",
            ),
            (
                format!("authorization: Basic {opaque}=="),
                "authorization: <redacted>",
            ),
            (
                format!(r#"{{"Authorization": "Token {opaque}"}}"#),
                r#"{"Authorization": "<redacted>"}"#,
            ),
            (
                format!("Proxy-Authorization={opaque}"),
                "Proxy-Authorization=<redacted>",
            ),
            (
                format!("export API_AUTH=\"Bearer {opaque}\""),
                "export API_AUTH=\"Bearer <redacted>\"",
            ),
        ] {
            assert_eq!(scrub(&text), kept, "{text}");
            assert!(find_secret(&text).is_some(), "{text}");
        }
        for prose in [
            "authorization: required",
            "Authorization: Bearer <token>",
            "the bearer of bad news",
            "a bearer instrument",
        ] {
            assert_eq!(scrub(prose), prose);
        }
    }

    /// An opaque key in its own header, or in a `NAME_TOKEN=` / `NAME_SECRET=`
    /// / `NAME_KEY=` assignment, matches no named shape. The value goes in each
    /// spelling a curl flag, a header dump, a JSON field, a shell line or a
    /// `.env` file carries; the header or variable name stays.
    #[test]
    fn scrubs_api_key_headers_and_secret_env_assignments() {
        let opaque = "Hq4Zp9Wd2Lx7Tn5Rb8Mc3Vf6";
        for (text, kept) in [
            (
                format!("curl -H 'X-Api-Key: {opaque}' https://api.example"),
                "curl -H 'X-Api-Key: <redacted>' https://api.example",
            ),
            (format!("x-auth-token:{opaque}"), "x-auth-token:<redacted>"),
            (
                format!(r#"{{"X-Goog-Api-Key": "{opaque}"}}"#),
                r#"{"X-Goog-Api-Key": "<redacted>"}"#,
            ),
            (
                format!("PRIVATE-TOKEN: {opaque}"),
                "PRIVATE-TOKEN: <redacted>",
            ),
            (format!("apikey: {opaque}"), "apikey: <redacted>"),
            (
                format!("SERVICE_TOKEN={opaque}"),
                "SERVICE_TOKEN=<redacted>",
            ),
            (
                format!("export CLIENT_SECRET=\"{opaque}\""),
                "export CLIENT_SECRET=\"<redacted>\"",
            ),
            (
                format!("env DEPLOY_API_KEY='{opaque}' ./deploy.sh"),
                "env DEPLOY_API_KEY='<redacted>' ./deploy.sh",
            ),
            (
                format!("DB_KEY={opaque}\nOTHER=1"),
                "DB_KEY=<redacted>\nOTHER=1",
            ),
            (
                "WEBHOOK_SECRET=whsec_4f9Kq2_Zx8Lm1_Tp6Vb3".to_string(),
                "WEBHOOK_SECRET=<redacted>",
            ),
            (
                "X-Api-Key: live_7Hq2_Zp9Wd4Lx1Tn".to_string(),
                "X-Api-Key: <redacted>",
            ),
            (format!("DB_PASSWORD={opaque}"), "DB_PASSWORD=<redacted>"),
            (format!("SMTP_PASSWD={opaque}"), "SMTP_PASSWD=<redacted>"),
            (format!("MYSQL_PWD={opaque}"), "MYSQL_PWD=<redacted>"),
            (format!("SECRET={opaque}"), "SECRET=<redacted>"),
            (
                format!("TOKEN='{opaque}' ./run"),
                "TOKEN='<redacted>' ./run",
            ),
        ] {
            assert_eq!(scrub(&text), kept, "{text}");
            assert!(find_secret(&text).is_some(), "{text}");
        }
        for prose in [
            "x-api-key: required",
            "X-Api-Key: <your key>",
            "the api key: see the docs for how to mint one",
            "apiKey: ApiKeyCredentialProvider",
            "GITHUB_TOKEN=$GITHUB_TOKEN",
            "echo SECRET_KEY=${SECRET_KEY:-<absent>}",
            "API_KEY=<your key here>",
            "SSH_KEY=/home/dev/.ssh/id_ed25519",
            "SSH_KEY=~/.ssh/id_ed25519_signing",
            "SORT_KEY=created_at",
            "SECRET_KEY = load_secret_from_vault()",
            "set the NPM_TOKEN= variable before publishing",
            "SORT_KEY=created_at_desc_then_name",
            "TENANT_KEY=3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            "X-Idempotency-Key: 3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            "PARTITION_KEY=tenant_west_region_v2",
            "NEXT_PUBLIC_API_KEY=Hq4Zp9Wd2Lx7Tn5Rb8Mc3Vf6",
            "X-Public-Key: Hq4Zp9Wd2Lx7Tn5Rb8Mc3Vf6",
            "OLDPWD=Hq4Zp9Wd2Lx7Tn5Rb8Mc3Vf6",
            "db_password=Hq4Zp9Wd2Lx7Tn5Rb8Mc3Vf6",
            "DB_PASSWORD: Hq4Zp9Wd2Lx7Tn5Rb8Mc3Vf6",
        ] {
            assert_eq!(scrub(prose), prose);
            assert_eq!(find_secret(prose), None, "{prose}");
        }
    }

    /// Lead's case: a headers map and an env map carry bare values that no
    /// text shape can see. The key names the credential, so every string
    /// under it goes, a short digit-less password included; a `PUBLIC` name,
    /// a key the rule does not name, and a reference or placeholder stay.
    #[test]
    fn scrub_json_redacts_values_under_secret_named_keys() {
        let mut value = serde_json::json!({
            "headers": { "X-Api-Key": "Hq4Zp9Wd2Lx7Tn5Rb8Mc3Vf6", "Accept": "application/json" },
            "env": {
                "CLIENT_SECRET": "s3cr3t-client-value",
                "DB_PASSWORD": "hunter",
                "API_TOKENS": ["tok1-aaaaaaaaaaaaaaaa", "tok2-bbbbbbbbbbbbbbbb"],
                "ROTATING_TOKEN": ["old-1", { "next": "new-2" }, 7],
                "GITHUB_TOKEN": "${GITHUB_TOKEN}",
                "ANTHROPIC_API_KEY": "<set me>",
                "NEXT_PUBLIC_API_KEY": "pk_live_visible_by_design",
                "HOME": "/home/dev",
                "max_tokens": "4096",
            },
        });
        scrub_json(&mut value);
        assert_eq!(
            value,
            serde_json::json!({
                "headers": { "X-Api-Key": REDACTED, "Accept": "application/json" },
                "env": {
                    "CLIENT_SECRET": REDACTED,
                    "DB_PASSWORD": REDACTED,
                    "API_TOKENS": ["tok1-aaaaaaaaaaaaaaaa", "tok2-bbbbbbbbbbbbbbbb"],
                    "ROTATING_TOKEN": [REDACTED, { "next": REDACTED }, 7],
                    "GITHUB_TOKEN": "${GITHUB_TOKEN}",
                    "ANTHROPIC_API_KEY": "<set me>",
                    "NEXT_PUBLIC_API_KEY": "pk_live_visible_by_design",
                    "HOME": "/home/dev",
                    "max_tokens": "4096",
                },
            })
        );
        let first = value.clone();
        scrub_json(&mut value);
        assert_eq!(value, first, "a second pass changes nothing");
    }

    /// The JSON-only key list, matched whole after lower-casing and dropping
    /// `_` and `-`, and camel-case keys through the shared name rule
    /// (`dbPassword` reads as `DB_PASSWORD`): lower-case payloads
    /// (`{"password": ...}`, an OAuth token response) and camel-case ones
    /// name their credentials this way. Near misses stay: a longer key, a
    /// count, a plural.
    #[test]
    fn scrub_json_redacts_lower_case_credential_keys() {
        let mut value = serde_json::json!({
            "password": "hunter",
            "Passwd": "tr0ub4dor",
            "secret": "abc",
            "client_secret": "s3cr3t-client-value",
            "apiKey": "Hq4Zp9Wd2Lx7Tn5Rb8Mc3Vf6",
            "API_KEY": "Hq4Zp9Wd2Lx7Tn5Rb8Mc3Vf6",
            "oauth": {
                "access_token": "ya29.a0AfH6SMBx",
                "refresh_token": "1//0gL9-Zq",
                "id_token": "opaque-id-token-value",
                "token_type": "Bearer",
                "expires_in": 3599,
            },
            "private_key": "MIIEvQIBADANBgkqhkiG9w0BAQEFAASC",
            "auth_token": "t0k3n",
            "password_hint": "the usual",
            "accessToken": "Hq4Zp9Wd2Lx7Tn5Rb8Mc3Vf6",
            "dbPassword": "hunter",
            "clientSecret": "s3cr3t",
            "Access-Token": "t0k3n",
            "secret_count": "3",
            "maxTokens": "4096",
            "session_key": "claude:4f2a9c1e",
            "next_token": "c2VjcmV0X2N1cnNvcjE",
        });
        scrub_json(&mut value);
        assert_eq!(
            value,
            serde_json::json!({
                "password": REDACTED,
                "Passwd": REDACTED,
                "secret": REDACTED,
                "client_secret": REDACTED,
                "apiKey": REDACTED,
                "API_KEY": REDACTED,
                "oauth": {
                    "access_token": REDACTED,
                    "refresh_token": REDACTED,
                    "id_token": REDACTED,
                    "token_type": "Bearer",
                    "expires_in": 3599,
                },
                "private_key": REDACTED,
                "auth_token": REDACTED,
                "password_hint": "the usual",
                "accessToken": REDACTED,
                "dbPassword": REDACTED,
                "clientSecret": REDACTED,
                "Access-Token": REDACTED,
                "secret_count": "3",
                "maxTokens": "4096",
                "session_key": "claude:4f2a9c1e",
                "next_token": "c2VjcmV0X2N1cnNvcjE",
            })
        );
    }

    /// Lower snake JSON keys read like camel-case ones, through upper snake
    /// case and the generic filter; credential words match whole words; a
    /// paging cursor is not a credential; and a value followed by `(` is a
    /// call, not a secret.
    #[test]
    fn credential_words_are_whole_words_and_cursors_and_calls_stay() {
        let opaque = "Hq4Zp9Wd2Lx7Tn5Rb8Mc3Vf6";
        let cursor = "c2VjcmV0X2N1cnNvcjE9";
        for (key, value, kept) in [
            ("db_password", "hunter", false),
            ("smtp_password", "s3cr3t", false),
            ("github_token", "ghx_not_a_named_shape_1", false),
            ("session_key", "claude:4f2a9c1e", true),
            ("sort_key", opaque, true),
            ("next_token", cursor, true),
            ("nextToken", cursor, true),
            ("pageToken", cursor, true),
            ("NEXT_TOKEN", cursor, true),
            ("CAPITAL_KEY", "berlin_office", true),
            ("CAPITAL_KEY", opaque, false),
            ("RAPID_KEY", "3f2504e0-4f89-11d3-9a0c-0305e82c3301", true),
        ] {
            let mut json = serde_json::json!({ key: value });
            scrub_json(&mut json);
            let expected = if kept { value } else { REDACTED };
            assert_eq!(json[key], expected, "{key}: {value}");
        }
        for (text, kept) in [
            (
                "CAPITAL_KEY=berlin_office_hq_v2",
                "CAPITAL_KEY=berlin_office_hq_v2",
            ),
            (
                "NEXT_TOKEN=c2VjcmV0X2N1cnNvcjE9",
                "NEXT_TOKEN=c2VjcmV0X2N1cnNvcjE9",
            ),
            (
                "SECRET_KEY=load_secret_from_vault()",
                "SECRET_KEY=load_secret_from_vault()",
            ),
            (
                "X-Api-Key: fetchApiKeyFromVault()",
                "X-Api-Key: fetchApiKeyFromVault()",
            ),
            ("SECRET_KEY=load_secret_from_vault", "SECRET_KEY=<redacted>"),
            (
                "GITHUB_TOKEN=ghx_not_a_named_shape_1",
                "GITHUB_TOKEN=<redacted>",
            ),
        ] {
            assert_eq!(scrub(text), kept, "{text}");
        }
        assert!(is_secret_name("NEXTAUTH_SECRET"), "one word, not NEXT");
        assert!(!is_secret_name("NEXT_PUBLIC_API_KEY"));
    }

    /// Under a secret-named key, only a whole upper-case `$VAR` or
    /// `${VAR}` is a reference; text that merely starts with `$`, or a
    /// `${VAR:-default}` whose default is a literal, is the credential.
    #[test]
    fn under_a_secret_key_only_real_references_stay() {
        for (text, kept) in [
            ("$DB_PASSWORD", true),
            ("${DB_PASSWORD}", true),
            ("", true),
            ("<set me>", true),
            ("$ecretP4ss", false),
            ("$", false),
            ("${DB_PASSWORD:-hunter2}", false),
            ("${DB_PASSWORD}x", false),
            ("$(cat /run/secrets/db)", false),
        ] {
            let mut value = serde_json::json!({ "DB_PASSWORD": text });
            scrub_json(&mut value);
            let expected = if kept { text } else { REDACTED };
            assert_eq!(value["DB_PASSWORD"], expected, "{text}");
        }
    }

    /// #152 review probe: under a credential's name the value goes whatever
    /// it looks like (a Heroku API key is a UUID, a client secret can be one,
    /// a passphrase is snake_case), in text and in JSON alike. Only a generic
    /// key's name (`SORT_KEY`, `PARTITION_KEY`, `X-Idempotency-Key`) keeps an
    /// identifier, and even there an opaque value or a known token goes.
    #[test]
    fn identifiers_stay_only_under_generic_key_names() {
        let uuid = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
        let passphrase = "correct_horse_battery_staple";
        let github = fake("ghp_", 'C', 36);
        for (key, value, kept) in [
            ("HEROKU_API_KEY", uuid, false),
            ("client_secret", uuid, false),
            ("clientSecret", uuid, false),
            ("password", passphrase, false),
            ("DB_PASSWORD", passphrase, false),
            ("X-Api-Key", uuid, false),
            ("PRIVATE_KEY", passphrase, false),
            ("SORT_KEY", "created_at_desc", true),
            ("PARTITION_KEY", uuid, true),
            ("X-Idempotency-Key", uuid, true),
            ("sortKey", "a8Fz0Qk2LrT9vYx7", true),
            ("TENANT_KEY", "short", true),
            ("SORT_KEY", "a8Fz0Qk2LrT9vYx7", false),
            ("PARTITION_KEY", github.as_str(), false),
        ] {
            let mut json = serde_json::json!({ key: value });
            scrub_json(&mut json);
            let expected = if kept { value } else { REDACTED };
            assert_eq!(json[key], expected, "{key}: {value}");
        }
        for (text, kept) in [
            (
                format!("HEROKU_API_KEY={uuid}"),
                "HEROKU_API_KEY=<redacted>",
            ),
            (format!("CLIENT_SECRET={uuid}"), "CLIENT_SECRET=<redacted>"),
            (
                format!("DB_PASSWORD={passphrase}"),
                "DB_PASSWORD=<redacted>",
            ),
            (format!("X-Api-Key: {uuid}"), "X-Api-Key: <redacted>"),
            (
                "X-Api-Key: abcdefghijklmnopqrstuvwx".to_string(),
                "X-Api-Key: <redacted>",
            ),
            (
                format!("SORT_KEY={passphrase}"),
                "SORT_KEY=correct_horse_battery_staple",
            ),
            (
                format!("PARTITION_KEY={uuid}"),
                "PARTITION_KEY=3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            ),
            (
                format!("X-Idempotency-Key: {uuid}"),
                "X-Idempotency-Key: 3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            ),
            (
                "SORT_KEY=a8Fz0Qk2LrT9vYx7".to_string(),
                "SORT_KEY=<redacted>",
            ),
        ] {
            assert_eq!(scrub(&text), kept, "{text}");
        }
    }

    /// One rule names a credential for both the text shapes and JSON keys.
    #[test]
    fn the_secret_name_rule() {
        for name in [
            "X-Api-Key",
            "x-auth-token",
            "X-Goog-Api-Key",
            "Authorization",
            "PRIVATE-TOKEN",
            "apikey",
            "CLIENT_SECRET",
            "GITHUB_TOKEN",
            "DEPLOY_API_KEY",
            "DB_PASSWORD",
            "SMTP_PASSWD",
            "MYSQL_PWD",
            "SECRET",
            "TOKEN",
        ] {
            assert!(is_secret_name(name), "{name}");
        }
        for name in [
            "NEXT_PUBLIC_API_KEY",
            "STRIPE_PUBLIC_KEY",
            "X-Public-Key",
            "apiKey",
            "password",
            "client_secret",
            "PASSWORD",
            "KEY",
            "PWD",
            "OLDPWD",
            "API_TOKENS",
            "max_tokens",
            "Accept",
        ] {
            assert!(!is_secret_name(name), "{name}");
        }
    }

    #[test]
    fn scrub_json_scrubs_every_string_value_and_keeps_the_shape() {
        let github = fake("ghp_", 'C', 36);
        let pem = format!("-----BEGIN RSA PRIVATE KEY-----\n{}", "M".repeat(64));
        let mut value = serde_json::json!({
            "content": { "text": format!("token {github} here") },
            "argv": ["curl", format!("x-api-key: {}", fake("sk-ant-api03-", 'A', 40))],
            "rawOutput": pem,
            "after": "kept",
            "exitCode": 0,
            "ok": true,
            "none": null,
        });
        scrub_json(&mut value);
        assert_eq!(
            value,
            serde_json::json!({
                "content": { "text": format!("token {REDACTED} here") },
                "argv": ["curl", format!("x-api-key: {REDACTED}")],
                "rawOutput": REDACTED,
                "after": "kept",
                "exitCode": 0,
                "ok": true,
                "none": null,
            }),
            "an unclosed key ends at its own value, and nothing else changes"
        );
        assert_eq!(find_secret(&value.to_string()), None);
    }

    #[test]
    fn scrub_json_leaves_object_keys_alone() {
        let github = fake("ghp_", 'C', 36);
        let mut value = serde_json::json!({ github.clone(): "v" });
        scrub_json(&mut value);
        assert!(
            value.get(&github).is_some(),
            "keys are structure and are not rewritten"
        );
    }

    #[test]
    fn finds_and_scrubs_every_issue_983_shape() {
        let cases = [
            ("anthropic key", fake("sk-ant-api03-", 'A', 40)),
            ("openai key", fake("sk-", 'b', 48)),
            ("github token", fake("ghp_", 'C', 36)),
            ("github token", fake("github_pat_", 'd', 82)),
            ("gitlab token", fake("glpat-", 'e', 20)),
            ("aws access key", fake("AKIA", 'F', 16)),
            ("google api key", fake("AIza", 'g', 35)),
            (
                "pem private key",
                format!(
                    "-----BEGIN RSA PRIVATE KEY-----\n{}\n-----END RSA PRIVATE KEY-----",
                    fake("", 'h', 64)
                ),
            ),
            (
                "jwt",
                format!(
                    "{}.{}.{}",
                    fake("eyJ", 'i', 20),
                    fake("eyJ", 'j', 20),
                    fake("", 'k', 20)
                ),
            ),
            (
                "url userinfo",
                format!(
                    "https://x-access-token:{}@github.com/o/r",
                    fake("", 'l', 12)
                ),
            ),
            ("stripe key", fake("sk_live_", 'S', 24)),
            ("stripe key", fake("rk_live_", 'R', 24)),
            ("npm token", fake("npm_", 'N', 36)),
            ("pypi token", fake("pypi-AgEIcHlwaS5vcmc", 'P', 60)),
            ("hugging face token", fake("hf_", 'H', 34)),
            ("digitalocean token", fake("dop_v1_", 'a', 64)),
            (
                "sendgrid key",
                format!("{}.{}", fake("SG.", 'G', 22), fake("", 'g', 43)),
            ),
            ("slack token", fake("xoxc-", '1', 40)),
            ("slack token", fake("xoxd-", '2', 40)),
            ("aws secret key", fake("AWS_SECRET_ACCESS_KEY=", 'w', 40)),
        ];
        for (shape, secret) in cases {
            let text = format!("before {secret} after");
            let found = find_secret(&text).unwrap_or_else(|| panic!("{shape} not found"));
            assert_eq!(found.0, shape, "{secret}");
            let scrubbed = scrub(&text);
            assert!(
                find_secret(&scrubbed).is_none(),
                "{shape} survived: {scrubbed}"
            );
            assert!(
                scrubbed.starts_with("before ") && scrubbed.ends_with(" after"),
                "{scrubbed}"
            );
        }
    }

    #[test]
    fn session_ids_and_branch_names_are_not_mistaken_for_openai_keys() {
        for clean in [
            "ainb-task-123e4567-e89b-12d3-a456-426614174000",
            "agents/fix-risk-assessment-for-the-new-billing-flow-v2",
            "tmux session ainb-disk-cleanup-0123456789abcdef0123456789abcdef",
            "npm_config_cache=/tmp/npm and hf_home=/tmp/hf",
            "risk_live_update and task_test_runner",
        ] {
            assert_eq!(scrub(clean), clean);
            assert!(find_secret(clean).is_none(), "{clean}");
        }
    }

    #[test]
    fn a_key_block_split_across_lines_is_redacted_line_by_line() {
        let body = fake("", 'p', 64);
        let lines = vec![
            "+API=1".to_string(),
            "+-----BEGIN RSA PRIVATE KEY-----".to_string(),
            format!("+{body}"),
            format!("+{body}"),
            "+-----END RSA PRIVATE KEY----- tail".to_string(),
            "+DONE=1".to_string(),
        ];
        let out = scrub_lines(&lines);
        assert_eq!(out.len(), lines.len());
        assert_eq!(out[0], "+API=1");
        assert!(out[1..5].iter().all(|l| l.contains(REDACTED)), "{out:?}");
        assert!(out.iter().all(|l| !l.contains(&body)), "{out:?}");
        assert_eq!(out[4], format!("{REDACTED} tail"));
        assert_eq!(out[5], "+DONE=1");
        // A block the capture cut off redacts to the last line.
        let cut = scrub_lines(&["-----BEGIN OPENSSH PRIVATE KEY-----", &body]);
        assert_eq!(cut, vec![REDACTED.to_string(), REDACTED.to_string()]);
    }

    #[test]
    fn a_token_split_by_colour_codes_is_still_scrubbed() {
        let token = fake("ghp_", 'C', 36);
        let coloured = format!(
            "\x1b[32mexport GH={}\x1b[1m{}\x1b[0m",
            &token[..10],
            &token[10..]
        );
        let scrubbed = scrub(&coloured);
        assert!(!scrubbed.contains(&token[10..]), "{scrubbed:?}");
        assert!(scrubbed.contains(REDACTED));
        // Colour stays when there is nothing to scrub.
        let clean = "\x1b[32mcargo test\x1b[0m";
        assert_eq!(scrub(clean), clean);
    }

    #[test]
    fn an_aws_secret_key_keeps_its_name_and_loses_its_value() {
        let secret = fake("", 'W', 40);
        for line in [
            format!("export AWS_SECRET_ACCESS_KEY={secret}"),
            format!("aws_secret_access_key = {secret}"),
            format!("\"SecretAccessKey\": \"{secret}\""),
        ] {
            let scrubbed = scrub(&line);
            assert!(!scrubbed.contains(&secret), "{scrubbed}");
            assert!(scrubbed.contains(REDACTED), "{scrubbed}");
            assert!(
                scrubbed.to_ascii_lowercase().contains("secret"),
                "the name stays: {scrubbed}"
            );
        }
        // Forty base64 characters with no name are not claimed.
        assert!(find_secret(&secret).is_none());
    }

    #[test]
    fn a_scrubbed_clone_url_keeps_its_scheme_and_host() {
        let url = format!("https://oauth2:{}@gitlab.com/o/r.git", fake("", 'z', 20));
        assert_eq!(
            scrub(&url),
            format!("https://{REDACTED}@gitlab.com/o/r.git")
        );
        assert_eq!(scrub("https://github.com/o/r"), "https://github.com/o/r");
        assert_eq!(
            scrub("ssh://git@github.com/o/r"),
            "ssh://git@github.com/o/r"
        );
    }

    #[test]
    fn a_truncated_pem_block_is_removed_to_the_end() {
        let text = format!(
            "key:\n-----BEGIN OPENSSH PRIVATE KEY-----\n{}",
            fake("", 'q', 70)
        );
        assert_eq!(scrub(&text), format!("key:\n{REDACTED}"));
    }

    /// The Discord shape spells its character class out instead of using `\w`,
    /// which is Unicode-aware and enormously more expensive. A token is
    /// base64url either way, including one sitting against text that is not.
    #[test]
    fn a_discord_token_is_scrubbed_whatever_it_sits_against() {
        let token = format!(
            "{}.{}.{}",
            fake("", 'a', 24),
            fake("", 'b', 7),
            fake("", 'c', 27)
        );
        for line in [
            format!("Authorization: Bot {token}"),
            format!("réponse={token}"),
            format!("\"token\":\"{token}\""),
        ] {
            let scrubbed = scrub(&line);
            assert!(
                !scrubbed.contains(&token),
                "the token survived in {scrubbed}"
            );
        }
    }

    /// A long run of letters that are not ASCII is not a token, and the shapes
    /// that could once scan it character by character are the ones that cost
    /// milliseconds a line.
    #[test]
    fn a_long_non_ascii_run_matches_no_token_shape() {
        let text = "\u{4f60}\u{597d}".repeat(2_000);

        assert_eq!(
            scrub(&text),
            text,
            "a run of non-ASCII text is not a credential"
        );
        assert_eq!(find_secret(&text), None);

        // And the same run with a real token inside it still loses the token.
        let token = format!(
            "{}.{}.{}",
            fake("", 'a', 24),
            fake("", 'b', 7),
            fake("", 'c', 27)
        );
        let scrubbed = scrub(&format!("{text}{token}{text}"));
        assert!(!scrubbed.contains(&token), "the token survived");
    }

    /// A key block spans lines, so a caller scrubbing a chunk at a time hands
    /// the flag back in and the body below the header is still removed.
    #[test]
    fn a_key_block_survives_the_chunk_boundary() {
        let head = [
            "notes".to_string(),
            "-----BEGIN RSA PRIVATE KEY-----".to_string(),
        ];
        let tail = [
            fake("", 'd', 64),
            "-----END RSA PRIVATE KEY-----".to_string(),
        ];

        let mut in_key = false;
        let first = scrub_lines_from(&head, &mut in_key);
        assert!(in_key, "the block is open across the boundary");
        let second = scrub_lines_from(&tail, &mut in_key);

        assert_eq!(first[1], REDACTED);
        assert_eq!(second[0], REDACTED);
        assert!(!in_key, "and the END line closes it");
    }

    #[test]
    fn redacts_multiple_discord_tokens() {
        let t1 = "AAAAAAAAAAAAAAAAAAAAAAAAA.BBBBBB.CCCCCCCCCCCCCCCCCCCCCCCCCCC";
        let t2 = "DDDDDDDDDDDDDDDDDDDDDDDDD.EEEEEE.FFFFFFFFFFFFFFFFFFFFFFFFFFF";
        let scrubbed = scrub_token(&format!("{t1} and {t2}"));
        assert_eq!(scrubbed, format!("{REDACTED} and {REDACTED}"));
    }
}
