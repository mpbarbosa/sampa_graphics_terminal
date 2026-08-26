//! Parse a fetched web page into a link-preview (unfurl) model for the URL preview panel.
//!
//! Given a page fetched over HTTP, produce a Slack-style unfurl: title, description, site name,
//! a (resolved) preview image URL, and a plain-text snippet. The design mirrors `sampa-ai`: the
//! **one** impure operation — the fetch — is behind a [`Fetch`] trait, so the whole core is
//! unit-tested against a fake transport with **no network**. The real socket-opening fetcher
//! (with the SSRF / size / redirect / timeout guards) is the bridge's job and is deliberately
//! **not** in this crate.
//!
//! Pure — `std` + serde only, **no shell, no Tauri**. Fails safe: a page the parser can't make
//! sense of yields a best-effort [`Preview`] carrying just the final URL (never an error, never
//! garbage), mirroring the fail-safe discipline of the other cores.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// What kind of resource was previewed — drives how the frontend renders it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewKind {
    /// An HTML page — unfurled to a title/description/image card.
    Html,
    /// An image resource — the frontend can show it inline.
    Image,
    /// `text/plain` (or similar) — shown as a text snippet.
    Text,
    /// Anything else (pdf, octet-stream, …) — just the URL + content-type.
    Other,
}

/// A link-preview model. Every field beyond `url`/`kind` is best-effort; the frontend renders
/// whatever is present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    /// The final URL previewed (after the fetcher followed any redirects).
    pub url: String,
    pub kind: PreviewKind,
    pub content_type: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    /// An **absolute** preview-image URL (og:image / twitter:image, resolved against `url`).
    pub image_url: Option<String>,
    /// A short plain-text snippet (from the page body or a text/plain resource).
    pub text_snippet: Option<String>,
}

impl Preview {
    fn bare(url: &str, kind: PreviewKind, content_type: Option<String>) -> Self {
        Preview {
            url: url.to_string(),
            kind,
            content_type,
            title: None,
            description: None,
            site_name: None,
            image_url: None,
            text_snippet: None,
        }
    }
}

/// The one impure operation: fetch a URL and return its final URL, content-type, and body bytes.
/// Abstracted so the core is tested without a network; the real implementation (with SSRF /
/// size / redirect guards) lives in the bridge.
pub trait Fetch {
    fn get(&self, url: &str) -> Result<Fetched, String>;
}

/// The result of a fetch handed to [`fetch_preview`].
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The URL after redirects — preview fields (esp. relative images) resolve against this.
    pub final_url: String,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// How many bytes of body text to keep for a snippet.
const SNIPPET_MAX: usize = 400;

/// Fetch a URL and build its [`Preview`]. Generic over [`Fetch`] so it is exercised in tests
/// with a fake transport (mirroring `sampa_ai::suggest`). Fetch errors surface as `Err`; a
/// fetched-but-unparseable page still yields a best-effort `Preview`.
pub fn fetch_preview<F: Fetch>(fetcher: &F, url: &str) -> Result<Preview, String> {
    let page = fetcher.get(url)?;
    let ct = page.content_type.clone();
    let ct_l = ct.as_deref().unwrap_or("").to_ascii_lowercase();
    let kind = classify_content_type(&ct_l);
    let mut preview = Preview::bare(&page.final_url, kind, ct);
    match kind {
        PreviewKind::Html => {
            let html = String::from_utf8_lossy(&page.body);
            fill_from_html(&mut preview, &html, &page.final_url);
        }
        PreviewKind::Text => {
            let text = String::from_utf8_lossy(&page.body);
            preview.text_snippet = snippet(&text);
        }
        PreviewKind::Image | PreviewKind::Other => {}
    }
    Ok(preview)
}

/// Classify a (lowercased) `Content-Type` value into a [`PreviewKind`]. Parameters after `;`
/// (e.g. `; charset=utf-8`) are ignored. An empty/unknown type is treated as HTML — most sites
/// that omit a usable type are pages — so it still gets an unfurl attempt.
pub fn classify_content_type(ct: &str) -> PreviewKind {
    let base = ct.split(';').next().unwrap_or("").trim();
    if base.starts_with("image/") {
        PreviewKind::Image
    } else if base == "text/html" || base == "application/xhtml+xml" || base.is_empty() {
        PreviewKind::Html
    } else if base.starts_with("text/") {
        PreviewKind::Text
    } else {
        PreviewKind::Other
    }
}

/// Extract the unfurl fields from an HTML document into `preview`, resolving the image URL
/// against `base`. OpenGraph wins, then Twitter-card, then plain `<title>` / meta description /
/// a body-text snippet. Best-effort: missing fields stay `None`.
pub fn fill_from_html(preview: &mut Preview, html: &str, base: &str) {
    let metas = meta_map(html);
    let pick = |keys: &[&str]| -> Option<String> {
        keys.iter().find_map(|k| {
            metas
                .get(*k)
                .map(|v| decode_entities(v.trim()))
                .filter(|s| !s.is_empty())
        })
    };

    preview.title = pick(&["og:title", "twitter:title"])
        .or_else(|| title_tag(html).map(|t| decode_entities(t.trim())).filter(|s| !s.is_empty()));
    preview.description = pick(&["og:description", "twitter:description", "description"]);
    preview.site_name = pick(&["og:site_name", "application-name"]);
    preview.image_url =
        pick(&["og:image", "og:image:url", "twitter:image", "twitter:image:src"])
            .and_then(|img| resolve_url(base, &img));

    // Fall back to a body-text snippet when there's no description to show.
    if preview.description.is_none() {
        preview.text_snippet = snippet(&strip_tags(html));
    }
}

// --- HTML scanning (std only; targeted, not a full parser) ---------------------------------

/// Map every `<meta>` element's `property`/`name` (lowercased) to its `content` value. First
/// occurrence wins. Attribute values keep their original case; entities are decoded by callers.
fn meta_map(html: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for body in tag_bodies(html, "meta") {
        let attrs = parse_attrs(body);
        let get = |k: &str| attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        if let (Some(key), Some(content)) = (get("property").or_else(|| get("name")), get("content"))
        {
            map.entry(key.to_ascii_lowercase()).or_insert(content);
        }
    }
    map
}

/// The text inside the first `<title>…</title>` (case-insensitive), if any.
fn title_tag(html: &str) -> Option<&str> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let gt = html[open..].find('>')? + open + 1;
    let close = lower[gt..].find("</title>")? + gt;
    Some(&html[gt..close])
}

/// The attribute text of every `<name …>` element (the part between the tag name and its `>`),
/// respecting quoted values so a `>` inside an attribute doesn't end the tag early.
fn tag_bodies<'a>(html: &'a str, name: &str) -> Vec<&'a str> {
    let lower = html.to_ascii_lowercase();
    let needle = format!("<{name}");
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(rel) = lower[i..].find(&needle) {
        let start = i + rel + needle.len();
        // Reject `<metafoo`: the char after the tag name must delimit it.
        match lower[start..].chars().next() {
            Some(c) if c.is_whitespace() || c == '>' || c == '/' => {}
            _ => {
                i = start;
                continue;
            }
        }
        let mut quote: Option<char> = None;
        let mut end = None;
        for (off, ch) in html[start..].char_indices() {
            match quote {
                Some(q) => {
                    if ch == q {
                        quote = None;
                    }
                }
                None => match ch {
                    '"' | '\'' => quote = Some(ch),
                    '>' => {
                        end = Some(start + off);
                        break;
                    }
                    _ => {}
                },
            }
        }
        match end {
            Some(e) => {
                out.push(html[start..e].trim_end_matches('/').trim());
                i = e + 1;
            }
            None => break,
        }
    }
    out
}

/// Parse a tag's attribute text into `(key_lowercased, value)` pairs. Handles double/single/
/// unquoted values and valueless attributes.
fn parse_attrs(tag: &str) -> Vec<(String, String)> {
    let c: Vec<char> = tag.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        let ks = i;
        while i < c.len() && (c[i].is_ascii_alphanumeric() || matches!(c[i], '-' | ':' | '_')) {
            i += 1;
        }
        if i == ks {
            i += 1; // not a key char — advance to avoid stalling
            continue;
        }
        let key: String = c[ks..i].iter().collect::<String>().to_ascii_lowercase();
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        if i < c.len() && c[i] == '=' {
            i += 1;
            while i < c.len() && c[i].is_whitespace() {
                i += 1;
            }
            let val = if i < c.len() && (c[i] == '"' || c[i] == '\'') {
                let q = c[i];
                i += 1;
                let vs = i;
                while i < c.len() && c[i] != q {
                    i += 1;
                }
                let v: String = c[vs..i].iter().collect();
                if i < c.len() {
                    i += 1; // consume closing quote
                }
                v
            } else {
                let vs = i;
                while i < c.len() && !c[i].is_whitespace() {
                    i += 1;
                }
                c[vs..i].iter().collect()
            };
            out.push((key, val));
        } else {
            out.push((key, String::new()));
        }
    }
    out
}

/// Strip `<head>`/`<script>`/`<style>` blocks and all remaining tags, leaving whitespace-
/// collapsed text. Dropping `<head>` keeps its `<title>`/meta text out of the body snippet.
fn strip_tags(html: &str) -> String {
    let without_blocks =
        remove_block(&remove_block(&remove_block(html, "head"), "script"), "style");
    let mut out = String::with_capacity(without_blocks.len());
    let mut in_tag = false;
    for ch in without_blocks.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    decode_entities(&out)
}

/// Remove every `<name …>…</name>` block (case-insensitive) so its text/code isn't scraped.
fn remove_block(html: &str, name: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let open_needle = format!("<{name}");
    let close_needle = format!("</{name}>");
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < html.len() {
        if let Some(rel) = lower[i..].find(&open_needle) {
            let open = i + rel;
            out.push_str(&html[i..open]);
            // Find the matching close; if none, drop the rest.
            match lower[open..].find(&close_needle) {
                Some(crel) => i = open + crel + close_needle.len(),
                None => break,
            }
        } else {
            out.push_str(&html[i..]);
            break;
        }
    }
    out
}

/// Collapse whitespace and truncate to [`SNIPPET_MAX`] chars (on a char boundary, with an
/// ellipsis). `None` if empty.
fn snippet(text: &str) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    if collapsed.chars().count() <= SNIPPET_MAX {
        Some(collapsed)
    } else {
        let cut: String = collapsed.chars().take(SNIPPET_MAX).collect();
        Some(format!("{}…", cut.trim_end()))
    }
}

/// Decode the handful of HTML entities that show up in titles/descriptions, plus numeric
/// (`&#38;` / `&#x26;`) references. Unknown entities are left as-is.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some(semi) = s[i..].find(';').map(|p| i + p) {
                let ent = &s[i + 1..semi];
                let decoded = match ent {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" | "#39" => Some('\''),
                    "nbsp" => Some('\u{a0}'),
                    _ => ent
                        .strip_prefix('#')
                        .and_then(|num| {
                            num.strip_prefix(['x', 'X'])
                                .and_then(|h| u32::from_str_radix(h, 16).ok())
                                .or_else(|| num.parse::<u32>().ok())
                        })
                        .and_then(char::from_u32),
                };
                if let Some(ch) = decoded {
                    out.push(ch);
                    i = semi + 1;
                    continue;
                }
            }
        }
        // Not a recognised entity — copy this char through.
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

// --- URL resolution ------------------------------------------------------------------------

/// Resolve `href` against the page's `base` URL to an absolute `http(s)` URL. Handles absolute
/// URLs, protocol-relative (`//host/…`), root-relative (`/path`), and simple relative paths.
/// Returns `None` for unusable inputs (e.g. `data:`/`javascript:` or an unparseable base).
pub fn resolve_url(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(href.to_string());
    }
    let (scheme, host, path) = split_base(base)?;
    if let Some(rest) = href.strip_prefix("//") {
        return Some(format!("{scheme}://{rest}")); // protocol-relative
    }
    // A colon before the first path separator means a URI scheme (data:, javascript:, mailto:,
    // …). http(s) was handled above, so any other scheme is rejected.
    let sep = href.find(['/', '?', '#']).unwrap_or(href.len());
    if href[..sep].contains(':') {
        return None;
    }
    if href.starts_with('/') {
        return Some(format!("{scheme}://{host}{href}"));
    }
    // Relative to the base's directory.
    let dir = match path.rfind('/') {
        Some(p) => &path[..=p],
        None => "/",
    };
    Some(format!("{scheme}://{host}{dir}{href}"))
}

/// Split a `scheme://host/path` URL into `(scheme, host[:port], path)`. `path` includes the
/// leading `/` (defaulting to `/`), and drops any `?query`/`#fragment`.
fn split_base(url: &str) -> Option<(String, String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() || rest.is_empty() {
        return None;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..authority_end];
    if host.is_empty() {
        return None;
    }
    let after = &rest[authority_end..];
    let path_end = after.find(['?', '#']).unwrap_or(after.len());
    let path = &after[..path_end];
    let path = if path.is_empty() { "/" } else { path };
    Some((scheme.to_string(), host.to_string(), path.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake [`Fetch`] returning a canned page (or an error), so the core is exercised with no
    /// network — the URL-preview analog of `sampa_ai`'s `FakeTransport`.
    struct FakeFetch {
        final_url: String,
        content_type: Option<&'static str>,
        body: &'static str,
        error: Option<&'static str>,
    }
    impl FakeFetch {
        fn html(body: &'static str) -> Self {
            FakeFetch {
                final_url: "https://example.com/page".into(),
                content_type: Some("text/html; charset=utf-8"),
                body,
                error: None,
            }
        }
    }
    impl Fetch for FakeFetch {
        fn get(&self, _url: &str) -> Result<Fetched, String> {
            if let Some(e) = self.error {
                return Err(e.to_string());
            }
            Ok(Fetched {
                final_url: self.final_url.clone(),
                content_type: self.content_type.map(str::to_string),
                body: self.body.as_bytes().to_vec(),
            })
        }
    }

    #[test]
    fn unfurls_opengraph_over_title() {
        let html = r#"<html><head>
            <title>Fallback Title</title>
            <meta property="og:title" content="Real &amp; Proper Title">
            <meta property="og:description" content="A great page.">
            <meta property="og:site_name" content="Example">
            <meta property="og:image" content="https://cdn.example.com/card.png">
        </head><body>ignored</body></html>"#;
        let p = fetch_preview(&FakeFetch::html(html), "https://example.com/page").unwrap();
        assert_eq!(p.kind, PreviewKind::Html);
        assert_eq!(p.title.as_deref(), Some("Real & Proper Title")); // og wins + entity decoded
        assert_eq!(p.description.as_deref(), Some("A great page."));
        assert_eq!(p.site_name.as_deref(), Some("Example"));
        assert_eq!(p.image_url.as_deref(), Some("https://cdn.example.com/card.png"));
        assert!(p.text_snippet.is_none()); // had a description, so no body snippet
    }

    #[test]
    fn falls_back_to_title_and_twitter_and_resolves_relative_image() {
        let html = r#"<head>
            <title>  Just a Title  </title>
            <meta name="twitter:image" content="/img/thumb.jpg">
        </head><body><p>Hello   world.</p><script>var x = '<b>no</b>';</script></body>"#;
        let p = fetch_preview(&FakeFetch::html(html), "https://example.com/page").unwrap();
        assert_eq!(p.title.as_deref(), Some("Just a Title")); // trimmed <title>
        assert_eq!(p.description, None);
        // Relative twitter:image resolved against the final URL's host.
        assert_eq!(p.image_url.as_deref(), Some("https://example.com/img/thumb.jpg"));
        // No description → body snippet, with the <script> body removed.
        assert_eq!(p.text_snippet.as_deref(), Some("Hello world."));
    }

    #[test]
    fn image_content_type_is_an_image_preview() {
        let f = FakeFetch {
            final_url: "https://example.com/cat.png".into(),
            content_type: Some("image/png"),
            body: "PNG-binary-bytes-here",
            error: None,
        };
        let p = fetch_preview(&f, "https://example.com/cat.png").unwrap();
        assert_eq!(p.kind, PreviewKind::Image);
        assert_eq!(p.url, "https://example.com/cat.png");
        assert!(p.title.is_none() && p.text_snippet.is_none()); // not parsed as HTML
    }

    #[test]
    fn plain_text_becomes_a_snippet() {
        let f = FakeFetch {
            final_url: "https://example.com/readme.txt".into(),
            content_type: Some("text/plain"),
            body: "  line one\n\n   line   two  ",
            error: None,
        };
        let p = fetch_preview(&f, "https://example.com/readme.txt").unwrap();
        assert_eq!(p.kind, PreviewKind::Text);
        assert_eq!(p.text_snippet.as_deref(), Some("line one line two"));
    }

    #[test]
    fn snippet_is_truncated_with_ellipsis() {
        let long = "word ".repeat(200); // 1000 chars
        let html = format!("<body>{long}</body>");
        // Leak the String for the 'static fake body.
        let leaked: &'static str = Box::leak(html.into_boxed_str());
        let p = fetch_preview(&FakeFetch::html(leaked), "https://example.com/page").unwrap();
        let s = p.text_snippet.unwrap();
        assert!(s.ends_with('…'));
        assert!(s.chars().count() <= SNIPPET_MAX + 1);
    }

    #[test]
    fn unparseable_page_is_a_best_effort_preview_not_an_error() {
        let p = fetch_preview(&FakeFetch::html("not really html at all"), "https://x.test").unwrap();
        assert_eq!(p.kind, PreviewKind::Html);
        assert_eq!(p.url, "https://example.com/page"); // final_url carried through
        assert!(p.title.is_none());
        // No structured fields, but the body text becomes a snippet.
        assert_eq!(p.text_snippet.as_deref(), Some("not really html at all"));
    }

    #[test]
    fn fetch_error_surfaces() {
        let f = FakeFetch {
            final_url: String::new(),
            content_type: None,
            body: "",
            error: Some("connection refused"),
        };
        assert_eq!(fetch_preview(&f, "https://nope.test"), Err("connection refused".into()));
    }

    #[test]
    fn content_type_classification() {
        assert_eq!(classify_content_type("text/html; charset=utf-8"), PreviewKind::Html);
        assert_eq!(classify_content_type("image/jpeg"), PreviewKind::Image);
        assert_eq!(classify_content_type("text/plain"), PreviewKind::Text);
        assert_eq!(classify_content_type("application/pdf"), PreviewKind::Other);
        assert_eq!(classify_content_type(""), PreviewKind::Html); // unknown → attempt an unfurl
    }

    #[test]
    fn url_resolution_covers_the_common_shapes() {
        let base = "https://example.com/blog/post?x=1#frag";
        assert_eq!(
            resolve_url(base, "https://cdn.other.com/a.png").as_deref(),
            Some("https://cdn.other.com/a.png") // absolute untouched
        );
        assert_eq!(
            resolve_url(base, "//cdn.example.com/a.png").as_deref(),
            Some("https://cdn.example.com/a.png") // protocol-relative
        );
        assert_eq!(
            resolve_url(base, "/img/a.png").as_deref(),
            Some("https://example.com/img/a.png") // root-relative
        );
        assert_eq!(
            resolve_url(base, "thumb.png").as_deref(),
            Some("https://example.com/blog/thumb.png") // relative to base dir
        );
        assert_eq!(resolve_url(base, "data:image/png;base64,AAAA"), None); // rejected scheme
        assert_eq!(resolve_url(base, "javascript:alert(1)//"), None);
        assert_eq!(resolve_url("not a url", "/a.png"), None); // unparseable base
    }
}
