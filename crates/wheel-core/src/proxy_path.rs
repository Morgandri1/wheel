// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Upstream URLs for the API and host proxies, built from a caller-supplied path suffix.
//!
//! Each proxy receives a wildcard suffix that axum has already percent-decoded once, and forwards
//! it beneath a base that names one project. Concatenating the two and handing the string to a URL
//! parser gives that parser a second decode: a double-encoded `..`, or a backslash, becomes a real
//! dot segment and the request climbs out of one project's prefix into another's. So the suffix is
//! refused if any segment could be reinterpreted, appended one segment at a time so the parser
//! treats each as opaque, and the finished URL is checked against its base before it is used.

pub use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProxyPathError {
    #[error("a path segment is `.` or `..`")]
    DotSegment,
    #[error("a path segment still contains `%` after decoding")]
    Percent,
    #[error("a path segment contains a backslash or a control character")]
    ForbiddenChar,
    #[error("the path has an empty segment")]
    EmptySegment,
    #[error("the upstream base is not a hierarchical URL")]
    BadBase,
    #[error("the upstream URL left its base")]
    Escaped,
}

/// Split a decoded wildcard suffix into segments, refusing any that a URL parser could reinterpret.
///
/// Refused: a `.` or `..` segment; a `%` anywhere, because the caller encoded twice and a later
/// decode would produce what this check never saw; a backslash, which parsers read as `/` in `http`
/// and `ws` URLs; any control character, because parsers strip tabs and newlines before testing a
/// segment for `..`; and an empty segment anywhere but the end, where it is a trailing slash.
pub fn proxy_segments(rest: &str) -> Result<Vec<&str>, ProxyPathError> {
    let segments: Vec<&str> = rest.split('/').collect();
    let last = segments.len() - 1;
    for (i, seg) in segments.iter().enumerate() {
        if seg.is_empty() && i != last {
            return Err(ProxyPathError::EmptySegment);
        }
        if matches!(*seg, "." | "..") {
            return Err(ProxyPathError::DotSegment);
        }
        if seg.contains('%') {
            return Err(ProxyPathError::Percent);
        }
        if seg.chars().any(|c| c == '\\' || c.is_control()) {
            return Err(ProxyPathError::ForbiddenChar);
        }
    }
    Ok(segments)
}

/// `base` with the decoded suffix `rest` appended beneath it, and `query` attached as given.
///
/// `base` is trusted configuration that already names one project. Only `rest` and `query` come
/// from the caller; `query` is the raw, still-encoded query string of the inbound request.
pub fn upstream_url(base: &str, rest: &str, query: Option<&str>) -> Result<Url, ProxyPathError> {
    let segments = proxy_segments(rest)?;
    let base = Url::parse(base).map_err(|_| ProxyPathError::BadBase)?;
    let url = append(&base, &segments, query)?;
    pin(&base, &url)?;
    Ok(url)
}

fn append(base: &Url, segments: &[&str], query: Option<&str>) -> Result<Url, ProxyPathError> {
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|_| ProxyPathError::BadBase)?
        .pop_if_empty()
        .extend(segments);
    url.set_query(query);
    url.set_fragment(None);
    Ok(url)
}

/// The finished URL must keep the base's scheme and authority, sit strictly beneath its path, and
/// read back with the same path when the next hop parses it.
fn pin(base: &Url, url: &Url) -> Result<(), ProxyPathError> {
    let prefix = format!("{}/", base.path().trim_end_matches('/'));
    let same_authority = url.scheme() == base.scheme()
        && url.username() == base.username()
        && url.host_str() == base.host_str()
        && url.port() == base.port();
    let stable = Url::parse(url.as_str()).is_ok_and(|again| again.path() == url.path());
    if same_authority && stable && url.path().starts_with(&prefix) {
        Ok(())
    } else {
        Err(ProxyPathError::Escaped)
    }
}

/// Path and query as an HTTP/1.1 request line carries them, for transports that take a request
/// target rather than a URL.
pub fn origin_form(url: &Url) -> &str {
    &url[url::Position::BeforePath..url::Position::AfterQuery]
}

/// The same URL with its scheme swapped for the WebSocket equivalent, or `None` if it has none.
pub fn websocket_url(mut url: Url) -> Option<Url> {
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        "ws" | "wss" => return Some(url),
        _ => return None,
    };
    url.set_scheme(scheme).ok()?;
    Some(url)
}

/// `host[:port]`, for a `Host` header.
pub fn authority(url: &Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "http://host.internal:7100/host/v1/projects/aaaa/engine";
    const PREFIX: &str = "/host/v1/projects/aaaa/engine/";

    #[test]
    fn ordinary_suffixes_land_beneath_the_base() {
        for (rest, tail) in [
            ("v1/board", "v1/board"),
            ("v1/board/", "v1/board/"),
            ("", ""),
            ("hook/a b", "hook/a%20b"),
            ("hook/a?b#c", "hook/a%3Fb%23c"),
            ("hook/..a/a..", "hook/..a/a.."),
            ("hook/\u{2026}", "hook/%E2%80%A6"),
        ] {
            let url = upstream_url(BASE, rest, None).unwrap();
            assert_eq!(url.path(), format!("{PREFIX}{tail}"), "{rest:?}");
            assert_eq!(url.query(), None, "{rest:?}");
        }
    }

    #[test]
    fn the_query_is_carried_verbatim() {
        let url = upstream_url(BASE, "v1/nodes", Some("dry_run=1&tag=a%2Fb&p=%2e%2e")).unwrap();
        assert_eq!(
            url.as_str(),
            "http://host.internal:7100/host/v1/projects/aaaa/engine/v1/nodes?dry_run=1&tag=a%2Fb&p=%2e%2e"
        );
        let bare = upstream_url(BASE, "v1", Some("")).unwrap();
        assert_eq!(bare.query(), Some(""));
    }

    #[test]
    fn a_trailing_slash_on_the_base_does_not_double_up() {
        let url = upstream_url("http://engine:7000/", "v1/board", None).unwrap();
        assert_eq!(url.as_str(), "http://engine:7000/v1/board");
        let url = upstream_url("http://h/tenant/aaaa/", "v1", None).unwrap();
        assert_eq!(url.path(), "/tenant/aaaa/v1");
    }

    #[test]
    fn dot_segments_are_refused() {
        for rest in [
            ".",
            "..",
            "v1/..",
            "../bbbb",
            "v1/./board",
            "v1/board/.",
            "v1/board/..",
        ] {
            assert_eq!(
                proxy_segments(rest),
                Err(ProxyPathError::DotSegment),
                "{rest:?}"
            );
        }
    }

    #[test]
    fn a_percent_left_after_decoding_is_refused() {
        // What `%252e%252e`, `%252f` and friends look like once axum has decoded them once.
        for rest in [
            "%2e%2e",
            "%2E%2e",
            ".%2e",
            "%2e.",
            "%2e",
            "..%2f..",
            "v1/%2e%2e/bbbb",
            "a%5cb",
            "100%",
            "%",
        ] {
            assert_eq!(
                proxy_segments(rest),
                Err(ProxyPathError::Percent),
                "{rest:?}"
            );
        }
    }

    #[test]
    fn backslashes_and_control_characters_are_refused() {
        for rest in [
            "..\\..\\bbbb",
            "v1\\board",
            "v1/bo\0ard",
            "\t..",
            "..\n",
            "v1/\r",
            "v1/\u{7f}",
        ] {
            assert_eq!(
                proxy_segments(rest),
                Err(ProxyPathError::ForbiddenChar),
                "{rest:?}"
            );
        }
    }

    #[test]
    fn empty_segments_are_refused_except_as_a_trailing_slash() {
        for rest in ["/v1", "v1//board", "//"] {
            assert_eq!(
                proxy_segments(rest),
                Err(ProxyPathError::EmptySegment),
                "{rest:?}"
            );
        }
        assert_eq!(proxy_segments("v1/board/"), Ok(vec!["v1", "board", ""]));
        assert_eq!(proxy_segments(""), Ok(vec![""]));
    }

    #[test]
    fn upstream_url_refuses_what_the_predicate_refuses() {
        for rest in ["..", "%2e%2e/bbbb", "a\\b", "v1//x", "\t.."] {
            assert!(upstream_url(BASE, rest, None).is_err(), "{rest:?}");
        }
    }

    #[test]
    fn appended_segments_are_opaque_to_the_parser() {
        let base = Url::parse(BASE).unwrap();
        for segments in [
            &["%2e%2e", "%2e%2e", "bbbb", "engine"][..],
            &[".%2E", "bbbb"],
            &["..\\..\\bbbb"],
            &["../../bbbb"],
        ] {
            let url = append(&base, segments, None).unwrap();
            assert_eq!(pin(&base, &url), Ok(()), "{segments:?} became {url}");
            assert!(
                !url.path().contains("/projects/bbbb"),
                "{segments:?} became {url}"
            );
        }
    }

    /// url 2.5 strips a tab from a segment before testing it for `..`, so `"\t.."` gets past
    /// `extend`'s own dot check and pops a segment. The predicate refuses it first; this proves the
    /// pin would too.
    #[test]
    fn the_pin_catches_a_segment_the_parser_still_normalises() {
        let base = Url::parse(BASE).unwrap();
        let url = append(&base, &["\t..", "\t..", "bbbb"], None).unwrap();
        assert_eq!(pin(&base, &url), Err(ProxyPathError::Escaped), "{url}");
    }

    #[test]
    fn the_pin_refuses_anything_outside_the_base() {
        let base = Url::parse(BASE).unwrap();
        for outside in [
            "http://host.internal:7100/host/v1/projects/bbbb/engine/v1/board",
            "http://host.internal:7100/host/v1/projects/aaaa/engine",
            "http://host.internal:7100/host/v1/projects/aaaa/engine-b/v1",
            "http://elsewhere:7100/host/v1/projects/aaaa/engine/v1",
            "http://host.internal:7101/host/v1/projects/aaaa/engine/v1",
            "https://host.internal:7100/host/v1/projects/aaaa/engine/v1",
            "http://user@host.internal:7100/host/v1/projects/aaaa/engine/v1",
        ] {
            assert_eq!(
                pin(&base, &Url::parse(outside).unwrap()),
                Err(ProxyPathError::Escaped),
                "{outside}"
            );
        }
        let inside = Url::parse(&format!("{BASE}/v1/board")).unwrap();
        assert_eq!(pin(&base, &inside), Ok(()));
    }

    #[test]
    fn a_base_that_cannot_hold_a_path_is_refused() {
        assert_eq!(
            upstream_url("not a url", "v1", None),
            Err(ProxyPathError::BadBase)
        );
        assert_eq!(
            upstream_url("mailto:ops@example.test", "v1", None),
            Err(ProxyPathError::BadBase)
        );
    }

    #[test]
    fn websocket_urls_swap_only_the_scheme() {
        let url = upstream_url("http://h:7000/tenant/aaaa", "v1/events", Some("since=3")).unwrap();
        let ws = websocket_url(url).unwrap();
        assert_eq!(ws.as_str(), "ws://h:7000/tenant/aaaa/v1/events?since=3");
        assert_eq!(authority(&ws), "h:7000");
        assert_eq!(origin_form(&ws), "/tenant/aaaa/v1/events?since=3");

        let tls = websocket_url(Url::parse("https://h/x").unwrap()).unwrap();
        assert_eq!(tls.as_str(), "wss://h/x");
        assert_eq!(authority(&tls), "h");
        assert!(websocket_url(Url::parse("unix:///run/e.sock").unwrap()).is_none());
    }
}
