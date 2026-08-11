//! Optional Claude (Anthropic) API integration for the command suggester
//! (DESIGN.md §13 / AI-integration feasibility doc).
//!
//! Turns a natural-language request — "describe what you want", or "given this
//! output, what command made it" — into a single shell command + a one-line
//! explanation, via one `POST /v1/messages` call with a structured-output schema
//! so parsing is guaranteed.
//!
//! **This is Sampa's only network surface, and it is opt-in.** The request/response
//! logic is pure and testable against a fake [`Transport`]; the real
//! [`UreqTransport`] is the sole place a socket is opened. The result is a
//! *suggestion* — the caller inserts it at the prompt, never auto-runs it (the same
//! insert-never-run boundary the command palette honors).

use serde::{Deserialize, Serialize};

const ANTHROPIC_VERSION: &str = "2023-06-01";

/// A model suggestion: the command line and a short human explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    pub command: String,
    pub explanation: String,
}

/// What the user is asking for, plus the environment the command runs in.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    /// The user's natural-language prompt (a desired behavior, or a description of
    /// output whose command they want).
    pub prompt: &'a str,
    /// Optional terminal context (recent output, cwd). Only sent when the user
    /// explicitly opts in — it can contain secrets and leaves the machine.
    pub context: Option<&'a str>,
    /// e.g. "linux"; steers the model to the right command dialect.
    pub os: &'a str,
    /// e.g. "zsh".
    pub shell: &'a str,
}

/// Endpoint + model + credential for one call. The `api_key` is supplied by the
/// caller from the environment — never stored by this crate.
#[derive(Debug, Clone)]
pub struct Params {
    pub model: String,
    pub endpoint: String,
    pub api_key: String,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiError {
    /// Non-2xx or transport failure (message is safe to show; no key echoed).
    Http(String),
    /// The API declined the request (`stop_reason: "refusal"`).
    Refused,
    /// Response wasn't the shape we expected.
    Parse(String),
}

impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AiError::Http(m) => write!(f, "request failed: {m}"),
            AiError::Refused => write!(f, "the model declined this request"),
            AiError::Parse(m) => write!(f, "unexpected response: {m}"),
        }
    }
}
impl std::error::Error for AiError {}

/// The one impure operation: POST a JSON body with headers, return the body text
/// (or an error string) for any 2xx / non-2xx. Abstracted so the core is tested
/// without a network.
pub trait Transport {
    fn post(&self, url: &str, headers: &[(&str, &str)], body: &str) -> Result<String, String>;
}

fn system_prompt(req: &Request) -> String {
    format!(
        "You translate a request into ONE shell command for the {shell} shell on \
{os}. The request may describe a desired action, or describe some output and ask \
which command produces it. Return the single most appropriate command line and a \
one-sentence explanation. Do not execute anything and do not wrap the command in a \
code fence. If the request implies a destructive or irreversible action (deleting, \
overwriting, force-pushing), still return the command but say so plainly in the \
explanation.",
        shell = req.shell,
        os = req.os
    )
}

/// Placeholder substituted for anything the redactor masks.
const REDACTED: &str = "‹redacted›";

/// Assignment keys whose *value* is masked (case-insensitive substring match), e.g.
/// `API_TOKEN=…`, `Authorization: …`.
const SECRET_KEY_HINTS: &[&str] = &[
    "TOKEN", "SECRET", "PASSWORD", "PASSWD", "APIKEY", "API_KEY", "ACCESS_KEY",
    "PRIVATE_KEY", "CREDENTIAL", "AUTHORIZATION", "SESSION_KEY",
];

/// Well-known credential token prefixes, masked wherever they appear.
const SECRET_PREFIXES: &[&str] = &[
    "sk-", "rk-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "glpat-",
    "xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-", "AKIA", "ASIA", "AIza", "ya29.", "npm_",
];

/// Best-effort redaction of obvious secret shapes before terminal context leaves the
/// machine (feasibility §4). This is **defense in depth, not a guarantee** — the primary
/// control is that `send_context` is off by default. It masks: values of secret-named
/// assignments (`TOKEN`/`SECRET`/`PASSWORD`/…, and `Authorization:` headers), tokens with
/// known credential prefixes (`sk-`, `ghp_`, `AKIA…`, `xox…`, …), PEM private-key blocks,
/// and long high-entropy tokens. It errs toward leaving ordinary output intact — lowercase
/// hex digests / git SHAs and normal words/paths are not masked.
pub fn redact(input: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_pem = false;
    for line in input.lines() {
        let upper = line.to_ascii_uppercase();
        // PEM private-key blocks: drop the body, leave a marker.
        if !in_pem && upper.contains("BEGIN") && upper.contains("PRIVATE KEY") {
            in_pem = true;
            out.push(format!("{REDACTED} (PEM private key)"));
            continue;
        }
        if in_pem {
            if upper.contains("END") && upper.contains("PRIVATE KEY") {
                in_pem = false;
            }
            continue;
        }
        // Secret-named assignment: mask the value, keep the key.
        if let Some(masked) = redact_assignment(line) {
            out.push(masked);
            continue;
        }
        // Otherwise scan tokens for credential shapes (split/join on ' ' is lossless).
        out.push(line.split(' ').map(redact_token).collect::<Vec<_>>().join(" "));
    }
    let mut s = out.join("\n");
    if input.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// Mask a `KEY=VALUE` / `KEY: VALUE` line when the key names a secret; else `None`.
/// The key is the last whitespace-delimited word before the separator, so a shell
/// prompt or `export ` prefix (`❯ echo TOKEN=…`) doesn't defeat the match.
fn redact_assignment(line: &str) -> Option<String> {
    let sep_idx = match (line.find('='), line.find(':')) {
        (Some(e), Some(c)) => e.min(c),
        (Some(e), None) => e,
        (None, Some(c)) => c,
        (None, None) => return None,
    };
    let key = line[..sep_idx].rsplit(char::is_whitespace).next().unwrap_or("").trim();
    // The key must read like an identifier/header name, and name a secret.
    if key.is_empty()
        || !key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return None;
    }
    let key_up = key.to_ascii_uppercase();
    if !SECRET_KEY_HINTS.iter().any(|h| key_up.contains(h)) {
        return None;
    }
    let prefix = &line[..=sep_idx]; // everything up to and including the separator
    let gap = if line[sep_idx + 1..].starts_with(' ') { " " } else { "" };
    Some(format!("{prefix}{gap}{REDACTED}"))
}

/// Mask a single whitespace token if its core looks like a credential.
fn redact_token(tok: &str) -> String {
    let core = tok.trim_matches(|c: char| {
        matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';')
    });
    if !core.is_empty() && looks_like_secret_token(core) {
        return tok.replacen(core, REDACTED, 1);
    }
    tok.to_string()
}

/// A token is a likely secret if it (or its value-part after a `=`/`:`, e.g.
/// `TOKEN=sk-…`) has a known prefix, or if it is a long high-entropy blob (secret
/// charset, mixed upper+lower+digit — so lowercase hashes / SHAs are spared).
fn looks_like_secret_token(tok: &str) -> bool {
    let value = tok.rsplit(['=', ':']).next().unwrap_or(tok);
    if SECRET_PREFIXES.iter().any(|p| tok.starts_with(p) || value.starts_with(p)) {
        return true;
    }
    if tok.len() < 32 {
        return false;
    }
    let charset_ok = tok
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'));
    let has_upper = tok.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = tok.chars().any(|c| c.is_ascii_lowercase());
    let has_digit = tok.chars().any(|c| c.is_ascii_digit());
    charset_ok && has_upper && has_lower && has_digit
}

fn user_content(req: &Request) -> String {
    let mut s = format!("<request>\n{}\n</request>", req.prompt);
    if let Some(ctx) = req.context {
        // The context is the data that leaves the machine — redact obvious secrets first.
        let safe = redact(ctx);
        s.push_str(&format!(
            "\n\n<terminal_context note=\"may be truncated; secrets redacted\">\n{safe}\n</terminal_context>"
        ));
    }
    s
}

/// Build the Messages API request body (structured output → guaranteed shape).
fn build_body(params: &Params, req: &Request) -> String {
    let body = serde_json::json!({
        "model": params.model,
        "max_tokens": params.max_tokens,
        "system": system_prompt(req),
        "messages": [{ "role": "user", "content": user_content(req) }],
        "output_config": {
            "format": {
                "type": "json_schema",
                "schema": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "explanation": { "type": "string" }
                    },
                    "required": ["command", "explanation"],
                    "additionalProperties": false
                }
            }
        }
    });
    body.to_string()
}

/// Parse a Messages API response: check for a refusal, then read the first text
/// block and decode it against the `{command, explanation}` schema.
fn parse_response(text: &str) -> Result<Suggestion, AiError> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| AiError::Parse(e.to_string()))?;
    if v.get("stop_reason").and_then(|s| s.as_str()) == Some("refusal") {
        return Err(AiError::Refused);
    }
    let content = v
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| AiError::Parse("no content array".into()))?;
    let text_block = content
        .iter()
        .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
        .and_then(|b| b.get("text"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| AiError::Parse("no text block".into()))?;
    serde_json::from_str::<Suggestion>(text_block).map_err(|e| AiError::Parse(e.to_string()))
}

/// Ask the model for a command suggestion. Generic over [`Transport`] so it is
/// unit-tested without a network.
pub fn suggest<T: Transport>(
    transport: &T,
    params: &Params,
    req: &Request,
) -> Result<Suggestion, AiError> {
    let body = build_body(params, req);
    let headers = [
        ("content-type", "application/json"),
        ("anthropic-version", ANTHROPIC_VERSION),
        ("x-api-key", params.api_key.as_str()),
    ];
    let resp = transport
        .post(&params.endpoint, &headers, &body)
        .map_err(AiError::Http)?;
    parse_response(&resp)
}

/// The real transport — the only code here that opens a socket.
pub struct UreqTransport;

impl Transport for UreqTransport {
    fn post(&self, url: &str, headers: &[(&str, &str)], body: &str) -> Result<String, String> {
        let mut req = ureq::post(url);
        for (k, v) in headers {
            req = req.set(k, v);
        }
        match req.send_string(body) {
            Ok(resp) => resp.into_string().map_err(|e| e.to_string()),
            // ureq surfaces non-2xx as Status; return the API's body so the caller
            // sees the real error (rate limit, bad key) — never the key itself.
            Err(ureq::Error::Status(code, resp)) => {
                let detail = resp.into_string().unwrap_or_default();
                Err(format!("HTTP {code}: {detail}"))
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Convenience: suggest over the real network transport.
pub fn suggest_over_network(params: &Params, req: &Request) -> Result<Suggestion, AiError> {
    suggest(&UreqTransport, params, req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records the last request and returns a canned response.
    struct FakeTransport {
        response: String,
        seen_body: RefCell<String>,
        seen_headers: RefCell<Vec<(String, String)>>,
    }
    impl FakeTransport {
        fn new(response: &str) -> Self {
            Self {
                response: response.into(),
                seen_body: RefCell::new(String::new()),
                seen_headers: RefCell::new(Vec::new()),
            }
        }
    }
    impl Transport for FakeTransport {
        fn post(&self, _url: &str, headers: &[(&str, &str)], body: &str) -> Result<String, String> {
            *self.seen_body.borrow_mut() = body.to_string();
            *self.seen_headers.borrow_mut() =
                headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            Ok(self.response.clone())
        }
    }

    fn params() -> Params {
        Params {
            model: "claude-opus-5".into(),
            endpoint: "https://example/v1/messages".into(),
            api_key: "sk-test-KEY".into(),
            max_tokens: 2048,
        }
    }
    fn req<'a>() -> Request<'a> {
        Request { prompt: "list files over 100MB", context: None, os: "linux", shell: "zsh" }
    }

    fn ok_response(command: &str, explanation: &str) -> String {
        // Structured output arrives as a JSON string inside a text block.
        let inner = serde_json::json!({ "command": command, "explanation": explanation }).to_string();
        serde_json::json!({
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": inner }]
        })
        .to_string()
    }

    #[test]
    fn parses_a_well_formed_suggestion() {
        let t = FakeTransport::new(&ok_response("find . -size +100M", "Finds files over 100 MB."));
        let s = suggest(&t, &params(), &req()).unwrap();
        assert_eq!(s.command, "find . -size +100M");
        assert!(s.explanation.contains("100 MB"));
    }

    #[test]
    fn sends_auth_and_version_headers_but_body_has_no_key() {
        let t = FakeTransport::new(&ok_response("ls", "lists"));
        suggest(&t, &params(), &req()).unwrap();
        let headers = t.seen_headers.borrow();
        assert!(headers.iter().any(|(k, v)| k == "x-api-key" && v == "sk-test-KEY"));
        assert!(headers.iter().any(|(k, v)| k == "anthropic-version" && v == "2023-06-01"));
        // The API key must never appear in the request body we build.
        assert!(!t.seen_body.borrow().contains("sk-test-KEY"));
    }

    #[test]
    fn body_requests_structured_output_and_carries_the_prompt() {
        let t = FakeTransport::new(&ok_response("ls", "lists"));
        suggest(&t, &params(), &req()).unwrap();
        let body = t.seen_body.borrow();
        assert!(body.contains("json_schema"));
        assert!(body.contains("list files over 100MB"));
        assert!(body.contains("claude-opus-5"));
    }

    #[test]
    fn context_is_included_only_when_present() {
        let without = FakeTransport::new(&ok_response("ls", "x"));
        suggest(&without, &params(), &req()).unwrap();
        assert!(!without.seen_body.borrow().contains("terminal_context"));

        let with = FakeTransport::new(&ok_response("ls", "x"));
        let mut r = req();
        r.context = Some("total 48\n-rw-r--r-- huge.bin");
        suggest(&with, &params(), &r).unwrap();
        assert!(with.seen_body.borrow().contains("terminal_context"));
        assert!(with.seen_body.borrow().contains("huge.bin"));
    }

    #[test]
    fn refusal_is_surfaced() {
        let refused = serde_json::json!({ "stop_reason": "refusal", "content": [] }).to_string();
        let t = FakeTransport::new(&refused);
        assert_eq!(suggest(&t, &params(), &req()), Err(AiError::Refused));
    }

    #[test]
    fn malformed_response_is_a_parse_error() {
        let t = FakeTransport::new("not json at all");
        assert!(matches!(suggest(&t, &params(), &req()), Err(AiError::Parse(_))));
    }

    #[test]
    fn http_failure_is_surfaced_without_leaking_the_key() {
        struct Boom;
        impl Transport for Boom {
            fn post(&self, _u: &str, _h: &[(&str, &str)], _b: &str) -> Result<String, String> {
                Err("HTTP 401: invalid x-api-key".into())
            }
        }
        match suggest(&Boom, &params(), &req()) {
            Err(AiError::Http(m)) => assert!(m.contains("401")),
            other => panic!("expected Http error, got {other:?}"),
        }
    }

    #[test]
    fn redacts_credential_prefixes_but_keeps_ordinary_output() {
        let input = "total 48\ndeploy sk-ant-api03-abc123DEF456ghi and ghp_ABCdef123456xyz\ncommit 8ab34f0c9e2d1122334455667788990011223344";
        let out = redact(input);
        assert!(!out.contains("sk-ant-api03"));
        assert!(!out.contains("ghp_ABCdef"));
        assert!(out.contains(REDACTED));
        assert!(out.contains("total 48")); // ordinary line untouched
        // A lowercase git SHA is not a secret shape — it must survive.
        assert!(out.contains("8ab34f0c9e2d1122334455667788990011223344"));
    }

    #[test]
    fn redacts_secret_named_assignments_and_auth_header() {
        let out = redact("PASSWORD=hunter2\nAuthorization: Bearer xyztokenvalue\nPATH=/usr/bin:/bin");
        assert!(out.contains(&format!("PASSWORD={REDACTED}")));
        assert!(out.contains(&format!("Authorization: {REDACTED}")));
        assert!(!out.contains("hunter2") && !out.contains("xyztokenvalue"));
        assert!(out.contains("PATH=/usr/bin:/bin")); // not secret-named → kept verbatim
    }

    #[test]
    fn redacts_pem_private_key_block() {
        let pem = "before\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXkAAAA\nQyNTUxOQAAACD\n-----END OPENSSH PRIVATE KEY-----\nafter";
        let out = redact(pem);
        assert!(!out.contains("b3BlbnNzaC1rZXk"));
        assert!(out.contains("PEM private key"));
        assert!(out.contains("before") && out.contains("after"));
    }

    #[test]
    fn redacts_secret_glued_in_a_prompt_prefixed_command_line() {
        // Real scrollback: a shell prompt + `echo` of a secret. Neither the prompt char
        // nor the `KEY=` gluing may let the secret through.
        let out = redact("❯ echo TOKEN=sk-live-SECRET123abcDEF");
        assert!(!out.contains("sk-live-SECRET123abcDEF"));
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn context_is_redacted_before_egress() {
        let t = FakeTransport::new(&ok_response("ls", "x"));
        let mut r = req();
        r.context = Some("run: export API_TOKEN=sk-live-SEKRET123abcDEF now");
        suggest(&t, &params(), &r).unwrap();
        let body = t.seen_body.borrow();
        assert!(body.contains("terminal_context"));
        assert!(!body.contains("sk-live-SEKRET123abcDEF"));
    }
}
