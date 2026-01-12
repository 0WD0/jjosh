use std::borrow::Cow;
use std::fmt::{Debug, Display, Formatter};

/// An error that is further described in a message.
#[derive(Debug)]
pub struct Message(
    /// The error message.
    Cow<'static, str>,
);

/// Lifecycle
impl Message {
    /// Create a new instance that displays the given `message`.
    pub fn new(message: impl Into<Cow<'static, str>>) -> Self {
        Message(message.into())
    }
}

impl Display for Message {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_ref())
    }
}

impl std::error::Error for Message {}
