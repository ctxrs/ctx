use tokio::sync::broadcast;

pub fn channel<T: Clone>(capacity: usize) -> broadcast::Sender<T> {
    let (tx, _) = broadcast::channel(capacity);
    tx
}
