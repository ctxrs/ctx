use super::fixture::{get_artifact_response, DownloadHttpFixture};
use super::*;

pub(super) async fn assert_session_state_metadata(fixture: &DownloadHttpFixture) {
    let session_state = get_session_state(fixture.app(), fixture.session_id()).await;
    assert_eq!(
        session_state["artifacts"][0]["id"].as_str(),
        Some(fixture.artifact_id.as_str())
    );
    let state_artifact_path = session_state["artifacts"][0]["absolute_path"]
        .as_str()
        .expect("artifact absolute path");
    assert_eq!(
        std::fs::canonicalize(state_artifact_path).unwrap(),
        std::fs::canonicalize(&fixture.artifact_path).unwrap()
    );
}

pub(super) async fn assert_session_scoped_downloads(fixture: &DownloadHttpFixture) {
    let res = get_artifact_response(fixture, fixture.wrong_session.id, &[]).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

pub(super) async fn assert_range_downloads(fixture: &DownloadHttpFixture) -> String {
    let res = get_artifact_response(
        fixture,
        fixture.session_id(),
        &[(header::RANGE.as_str(), "bytes=0-7")],
    )
    .await;
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        res.headers()
            .get(header::ACCEPT_RANGES)
            .and_then(|value| value.to_str().ok()),
        Some("bytes")
    );
    assert_eq!(
        res.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("private, max-age=0, must-revalidate")
    );
    assert_eq!(
        res.headers()
            .get(header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok()),
        Some("bytes 0-7/14")
    );
    let etag = res
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .expect("artifact range etag");
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), b"artifact");

    let res = get_artifact_response(
        fixture,
        fixture.session_id(),
        &[(header::RANGE.as_str(), "Bytes= 0 - 7")],
    )
    .await;
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), b"artifact");

    let res = get_artifact_response(
        fixture,
        fixture.session_id(),
        &[
            (header::RANGE.as_str(), "bytes=0-7"),
            (header::IF_RANGE.as_str(), etag.as_str()),
        ],
    )
    .await;
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), b"artifact");

    etag
}

pub(super) async fn assert_invalid_ranges(fixture: &DownloadHttpFixture) {
    let res = get_artifact_response(
        fixture,
        fixture.session_id(),
        &[(header::RANGE.as_str(), "bytes=999-1000")],
    )
    .await;
    assert_eq!(res.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok()),
        Some("bytes */14")
    );

    let res = get_artifact_response(
        fixture,
        fixture.session_id(),
        &[(header::RANGE.as_str(), "bytes=999999999999999999999-")],
    )
    .await;
    assert_eq!(res.status(), StatusCode::RANGE_NOT_SATISFIABLE);

    let res = get_artifact_response(
        fixture,
        fixture.session_id(),
        &[(header::RANGE.as_str(), "items=0-1")],
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), b"artifact-body\n");

    let res = get_artifact_response(
        fixture,
        fixture.session_id(),
        &[(header::RANGE.as_str(), "bytes=0-0,2-2")],
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), b"artifact-body\n");
}

pub(super) async fn assert_full_and_conditional_downloads(
    fixture: &DownloadHttpFixture,
    etag: &str,
) {
    let res = get_artifact_response(fixture, fixture.session_id(), &[]).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/plain")
    );
    assert_eq!(
        res.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("private, max-age=0, must-revalidate")
    );
    assert_eq!(
        res.headers()
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok()),
        Some(etag)
    );
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), b"artifact-body\n");

    let res = get_artifact_response(
        fixture,
        fixture.session_id(),
        &[(header::IF_NONE_MATCH.as_str(), etag)],
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        res.headers()
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok()),
        Some(etag)
    );
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert!(body.is_empty());

    let res = get_artifact_response(
        fixture,
        fixture.session_id(),
        &[
            (header::RANGE.as_str(), "bytes=0-7"),
            (header::IF_NONE_MATCH.as_str(), etag),
        ],
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert!(body.is_empty());
}
