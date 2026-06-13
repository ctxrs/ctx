use super::*;

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static CONNECTION_HTTP_CLIENT_BUILD_COUNT: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_connection_http_client_build_count() {
    CONNECTION_HTTP_CLIENT_BUILD_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(super) fn connection_http_client_build_count() -> usize {
    CONNECTION_HTTP_CLIENT_BUILD_COUNT.with(Cell::get)
}

fn build_connection_http_client() -> Result<reqwest::blocking::Client> {
    #[cfg(test)]
    CONNECTION_HTTP_CLIENT_BUILD_COUNT.with(|count| count.set(count.get() + 1));
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .context("building http client")
}

pub(super) fn get_connection_http_client(
    client: &std::sync::OnceLock<reqwest::blocking::Client>,
) -> Result<reqwest::blocking::Client> {
    if let Some(existing) = client.get() {
        return Ok(existing.clone());
    }
    let built = build_connection_http_client()?;
    let _ = client.set(built);
    client
        .get()
        .cloned()
        .context("connection http client missing after initialization")
}
