use super::*;

impl Store {
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}
