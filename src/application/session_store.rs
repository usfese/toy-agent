use crate::domain::session::Session;

pub trait SessionStore {
    fn list(&self) -> anyhow::Result<Vec<String>>;
    fn save(&mut self, session: &Session) -> anyhow::Result<()>;
    fn load(&self, id: &str) -> anyhow::Result<Option<Session>>;
}
