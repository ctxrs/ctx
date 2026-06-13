use super::*;

mod assertions;
mod fixture;

#[tokio::test]
async fn session_state_exposes_artifact_metadata_and_session_scoped_downloads() {
    let fixture = fixture::DownloadHttpFixture::build().await;

    assertions::assert_session_state_metadata(&fixture).await;
    assertions::assert_session_scoped_downloads(&fixture).await;
    let etag = assertions::assert_range_downloads(&fixture).await;
    assertions::assert_invalid_ranges(&fixture).await;
    assertions::assert_full_and_conditional_downloads(&fixture, &etag).await;
}
