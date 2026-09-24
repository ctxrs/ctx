use super::*;

impl<'a> QueryBudget<'a> {
    pub(super) fn install(conn: &'a Connection) -> Result<Self> {
        Self::with_steps(conn, 2_000)
    }
    pub(super) fn with_steps(conn: &'a Connection, max_callbacks: usize) -> Result<Self> {
        let start = Instant::now();
        let mut callbacks = 0;
        conn.progress_handler(
            1_000,
            Some(move || {
                callbacks += 1;
                callbacks >= max_callbacks || start.elapsed() >= Duration::from_secs(2)
            }),
        )?;
        Ok(Self(conn))
    }
}
