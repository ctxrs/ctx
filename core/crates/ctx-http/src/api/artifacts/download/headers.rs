use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;

use super::SessionArtifactDownloadMetadata;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionArtifactRange {
    Ignore,
    Satisfiable { start: u64, end: u64 },
    Unsatisfiable,
}

pub(super) enum SessionArtifactRangeDecision {
    Full,
    Partial { start: u64, end: u64 },
    NotModified(Response),
    RangeNotSatisfiable(Response),
}

pub(super) fn session_artifact_range_decision(
    request_headers: &HeaderMap,
    metadata: &SessionArtifactDownloadMetadata<'_>,
) -> SessionArtifactRangeDecision {
    let range_header = request_headers.get(header::RANGE);
    if metadata.etag.is_some_and(|current_etag| {
        session_artifact_if_none_match_matches(
            header_to_str(request_headers.get(header::IF_NONE_MATCH)),
            current_etag,
        )
    }) {
        return SessionArtifactRangeDecision::NotModified(not_modified_response(metadata));
    }

    let should_ignore_range = range_header.is_some()
        && request_headers.contains_key(header::IF_RANGE)
        && !session_artifact_if_range_allows_range_request(
            header_to_str(request_headers.get(header::IF_RANGE)),
            metadata.etag,
            metadata.last_modified,
        );
    if should_ignore_range {
        return SessionArtifactRangeDecision::Full;
    }

    match parse_session_artifact_range_header(header_to_str(range_header), metadata.size) {
        SessionArtifactRange::Ignore => SessionArtifactRangeDecision::Full,
        SessionArtifactRange::Satisfiable { start, end } => {
            SessionArtifactRangeDecision::Partial { start, end }
        }
        SessionArtifactRange::Unsatisfiable => SessionArtifactRangeDecision::RangeNotSatisfiable(
            range_not_satisfiable_response(metadata),
        ),
    }
}

pub(super) fn apply_session_artifact_download_headers(
    headers: &mut HeaderMap,
    metadata: &SessionArtifactDownloadMetadata<'_>,
    content_length: u64,
    content_range: Option<&str>,
) {
    apply_session_artifact_response_headers(headers);
    apply_session_artifact_metadata_headers(headers, metadata);
    if let Ok(value) = HeaderValue::from_str(&content_length.to_string()) {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    if let Some(content_range) = content_range {
        if let Ok(value) = HeaderValue::from_str(content_range) {
            headers.insert(header::CONTENT_RANGE, value);
        }
    }
    if let Ok(value) = HeaderValue::from_str(metadata.mime_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Some(name) = metadata.name {
        let value = format!("inline; filename=\"{}\"", name.replace('"', ""));
        if let Ok(v) = value.parse() {
            headers.insert(header::CONTENT_DISPOSITION, v);
        }
    }
}

fn not_modified_response(metadata: &SessionArtifactDownloadMetadata<'_>) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::NOT_MODIFIED;
    apply_session_artifact_response_headers(resp.headers_mut());
    apply_session_artifact_metadata_headers(resp.headers_mut(), metadata);
    resp
}

fn range_not_satisfiable_response(metadata: &SessionArtifactDownloadMetadata<'_>) -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
    apply_session_artifact_response_headers(resp.headers_mut());
    apply_session_artifact_metadata_headers(resp.headers_mut(), metadata);
    if let Ok(value) = HeaderValue::from_str(&format!("bytes */{}", metadata.size)) {
        resp.headers_mut().insert(header::CONTENT_RANGE, value);
    }
    resp
}

fn apply_session_artifact_response_headers(headers: &mut HeaderMap) {
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=0, must-revalidate"),
    );
}

fn apply_session_artifact_metadata_headers(
    headers: &mut HeaderMap,
    metadata: &SessionArtifactDownloadMetadata<'_>,
) {
    if let Some(etag) = metadata.etag {
        if let Ok(value) = HeaderValue::from_str(etag) {
            headers.insert(header::ETAG, value);
        }
    }
    if let Some(last_modified) = metadata.last_modified {
        if let Ok(value) = HeaderValue::from_str(last_modified) {
            headers.insert(header::LAST_MODIFIED, value);
        }
    }
}

fn header_to_str(value: Option<&HeaderValue>) -> Option<&str> {
    value.and_then(|header| header.to_str().ok())
}

fn normalize_entity_tag(tag: &str) -> &str {
    tag.trim().strip_prefix("W/").unwrap_or(tag.trim())
}

fn session_artifact_if_none_match_matches(value: Option<&str>, etag: &str) -> bool {
    value.is_some_and(|raw| {
        raw.split(',').any(|part| {
            let candidate = part.trim();
            candidate == "*" || normalize_entity_tag(candidate) == normalize_entity_tag(etag)
        })
    })
}

fn session_artifact_if_range_allows_range_request(
    value: Option<&str>,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> bool {
    let Some(raw) = value else {
        return true;
    };
    let candidate = raw.trim();
    if candidate.starts_with('"') {
        return etag.is_some_and(|current_etag| candidate == current_etag);
    }
    if candidate.starts_with("W/") || candidate == "*" {
        return false;
    }
    let Some(current_last_modified) = last_modified else {
        return false;
    };
    let Ok(if_range_time) = chrono::DateTime::parse_from_rfc2822(candidate) else {
        return false;
    };
    let Ok(last_modified_time) = chrono::DateTime::parse_from_rfc2822(current_last_modified) else {
        return false;
    };
    last_modified_time <= if_range_time
}

fn parse_decimal_u64(raw: &str) -> Option<Result<u64, ()>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(trimmed.parse::<u64>().map_err(|_| ()))
}

fn parse_session_artifact_range_header(
    range_header: Option<&str>,
    size: u64,
) -> SessionArtifactRange {
    let Some(header) = range_header else {
        return SessionArtifactRange::Ignore;
    };
    let Some((unit, range)) = header.trim().split_once('=') else {
        return SessionArtifactRange::Ignore;
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return SessionArtifactRange::Ignore;
    }
    let range = range.trim();
    if range.contains(',') {
        return SessionArtifactRange::Ignore;
    }
    let Some((start_raw, end_raw)) = range.split_once('-') else {
        return SessionArtifactRange::Ignore;
    };
    if start_raw.is_empty() {
        let suffix = match parse_decimal_u64(end_raw) {
            Some(Ok(value)) => value,
            Some(Err(())) => {
                if size == 0 {
                    return SessionArtifactRange::Unsatisfiable;
                }
                return SessionArtifactRange::Satisfiable {
                    start: 0,
                    end: size.saturating_sub(1),
                };
            }
            None => {
                return SessionArtifactRange::Ignore;
            }
        };
        if suffix == 0 || size == 0 {
            return SessionArtifactRange::Unsatisfiable;
        }
        return SessionArtifactRange::Satisfiable {
            start: size.saturating_sub(suffix),
            end: size.saturating_sub(1),
        };
    }
    let start = match parse_decimal_u64(start_raw) {
        Some(Ok(value)) => value,
        Some(Err(())) => {
            return SessionArtifactRange::Unsatisfiable;
        }
        None => {
            return SessionArtifactRange::Ignore;
        }
    };
    if start >= size {
        return SessionArtifactRange::Unsatisfiable;
    }
    let end = if end_raw.is_empty() {
        size.saturating_sub(1)
    } else {
        match parse_decimal_u64(end_raw) {
            Some(Ok(value)) => value.min(size.saturating_sub(1)),
            Some(Err(())) => size.saturating_sub(1),
            None => {
                return SessionArtifactRange::Ignore;
            }
        }
    };
    if start > end {
        return SessionArtifactRange::Unsatisfiable;
    }
    SessionArtifactRange::Satisfiable { start, end }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_session_artifact_range_header, session_artifact_if_none_match_matches,
        session_artifact_if_range_allows_range_request, SessionArtifactRange,
    };

    #[test]
    fn if_none_match_matches_strong_weak_and_wildcard_tags() {
        assert!(session_artifact_if_none_match_matches(
            Some("\"other\", W/\"abc\""),
            "\"abc\""
        ));
        assert!(session_artifact_if_none_match_matches(Some("*"), "\"abc\""));
        assert!(!session_artifact_if_none_match_matches(
            Some("\"other\""),
            "\"abc\""
        ));
    }

    #[test]
    fn if_range_accepts_current_strong_etag_only() {
        assert!(session_artifact_if_range_allows_range_request(
            Some("\"abc\""),
            Some("\"abc\""),
            None
        ));
        assert!(!session_artifact_if_range_allows_range_request(
            Some("W/\"abc\""),
            Some("\"abc\""),
            None
        ));
    }

    #[test]
    fn if_range_accepts_new_enough_date() {
        assert!(session_artifact_if_range_allows_range_request(
            Some("Thu, 01 Jan 1970 00:00:01 GMT"),
            None,
            Some("Thu, 01 Jan 1970 00:00:00 GMT")
        ));
        assert!(!session_artifact_if_range_allows_range_request(
            Some("Wed, 31 Dec 1969 23:59:59 GMT"),
            None,
            Some("Thu, 01 Jan 1970 00:00:00 GMT")
        ));
    }

    #[test]
    fn range_header_parses_bounded_and_suffix_ranges() {
        assert_eq!(
            parse_session_artifact_range_header(Some("bytes=2-4"), 10),
            SessionArtifactRange::Satisfiable { start: 2, end: 4 }
        );
        assert_eq!(
            parse_session_artifact_range_header(Some("bytes=-3"), 10),
            SessionArtifactRange::Satisfiable { start: 7, end: 9 }
        );
    }

    #[test]
    fn range_header_rejects_unsatisfiable_ranges() {
        assert_eq!(
            parse_session_artifact_range_header(Some("bytes=10-20"), 10),
            SessionArtifactRange::Unsatisfiable
        );
        assert_eq!(
            parse_session_artifact_range_header(Some("bytes=-0"), 10),
            SessionArtifactRange::Unsatisfiable
        );
    }

    #[test]
    fn range_header_ignores_unsupported_shapes() {
        assert_eq!(
            parse_session_artifact_range_header(Some("items=1-2"), 10),
            SessionArtifactRange::Ignore
        );
        assert_eq!(
            parse_session_artifact_range_header(Some("bytes=1-2,4-5"), 10),
            SessionArtifactRange::Ignore
        );
    }
}
