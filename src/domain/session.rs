use crate::domain::message::Message;

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub messages: Vec<Message>,
}
