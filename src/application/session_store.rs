use crate::domain::session::Session;

/// An owned lock whose lifetime covers an entire session transaction.
pub trait SessionLock {}

pub trait SessionStore {
    fn lock(&self, id: &str) -> anyhow::Result<Box<dyn SessionLock>>;
    fn list(&self) -> anyhow::Result<Vec<String>>;
    fn save(&mut self, session: &Session) -> anyhow::Result<()>;
    fn load(&self, id: &str) -> anyhow::Result<Option<Session>>;
}
