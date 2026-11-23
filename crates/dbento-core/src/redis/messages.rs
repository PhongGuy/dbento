//! The message API over Redis Streams.

use crate::message::{MessageError, MessagePage, MessageRead, plan_read};
use crate::redis::driver::RedisSession;

impl RedisSession {
    /// One page of a stream, browsed either way or followed forward.
    pub(crate) async fn read_messages(&self, r: &MessageRead) -> Result<MessagePage, MessageError> {
        plan_read(r, self.shared.timeout())?;
        Err(MessageError::Unsupported {
            reason: "stream reads are not built yet".to_owned(),
        })
    }
}
