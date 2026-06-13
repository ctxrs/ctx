impl WorkspaceCloseSignal {
    fn new() -> Self {
        Self {
            finished: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    async fn wait(&self) {
        if self.finished.load(Ordering::Acquire) {
            return;
        }
        let notified = self.notify.notified();
        if self.finished.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }

    fn finish(&self) {
        self.finished.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }
}
