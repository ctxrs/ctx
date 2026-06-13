use std::future::Future;

use tokio::task::JoinHandle;

/// Race a JoinHandle against another future without re-polling the handle.
pub async fn race_join_handle<T, F>(
    mut join_handle: JoinHandle<T>,
    future: F,
) -> (Option<JoinHandle<T>>, Option<F::Output>)
where
    F: Future,
{
    tokio::pin!(future);

    tokio::select! {
        result = &mut join_handle => {
            let _ = result;
            (None, None)
        }
        output = future => {
            (Some(join_handle), Some(output))
        }
    }
}
