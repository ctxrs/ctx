pub(in crate::daemon) mod events;
pub(in crate::daemon) mod runtime;

#[derive(Debug)]
pub enum SessionAuthError {
    NotFound(&'static str),
    BadRequest(String),
    Forbidden(String),
    Internal(String),
    AuthenticationFailed { redacted_message: String },
}
