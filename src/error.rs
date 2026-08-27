//! Two error types, for two audiences.
//!
//! [`BoxError`] is for startup and internals, where a failure is printed once and read by an
//! operator. [`ApiError`] is for anything that reaches an HTTP caller, where the message is part
//! of the contract and the status code carries the meaning.

/// A boxed error, used everywhere a failure is going to be printed rather than matched on.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
/// The crate's result type for internal operations.
pub type Result<T> = std::result::Result<T, BoxError>;

/// Builds a [`BoxError`] from a message.
pub fn err(message: impl Into<String>) -> BoxError {
    message.into().into()
}

/// Adds a description to a failure.
///
/// The cause is folded into the message rather than kept as a source chain, because every one of
/// these is printed once at startup and a chain would only be flattened again.
pub trait Context<T> {
    /// Describes the failure with a fixed message.
    fn context(self, message: impl std::fmt::Display) -> Result<T>;
    /// Describes the failure with a message built only if there is one.
    fn with_context<F, D>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> D,
        D: std::fmt::Display;
}

impl<T, E: std::fmt::Display> Context<T> for std::result::Result<T, E> {
    fn context(self, message: impl std::fmt::Display) -> Result<T> {
        self.map_err(|e| err(format!("{message}: {e}")))
    }

    fn with_context<F, D>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> D,
        D: std::fmt::Display,
    {
        self.map_err(|e| err(format!("{}: {e}", f())))
    }
}

/// A failure on its way to an HTTP caller.
///
/// The [`Display`](std::fmt::Display) text is what the caller receives in `{"error": "..."}`, so
/// these strings are part of the API contract. [`Internal`](ApiError::Internal) deliberately
/// renders a fixed message so no cause ever leaks.
#[derive(Debug)]
pub enum ApiError {
    /// No payout is stored under that reference. `404`.
    NotFound,
    /// The `x-api-secret` header is missing or wrong. `401`.
    Unauthorized,
    /// The caller exceeded the payout rate limit. `429`.
    TooManyRequests,
    /// The request is malformed: a bad field, an unknown field, an invalid personnummer. `400`.
    BadRequest(String),
    /// The reference is already spent. `409`, and never a reason to resubmit.
    Conflict(String),
    /// Swish or the database is unreachable, or Swish refused the payout. `503`.
    ServiceUnavailable(String),
    /// Swish answered, and the answer was a rejection. Becomes `503`.
    SwishRejected {
        /// The HTTP status Swish returned.
        code: u16,
        /// Swish's response body, which carries the error code.
        body: String,
    },
    /// An internal fault. `500`, with a fixed message.
    Internal(BoxError),
}

// These strings reach API callers, so they are part of the contract. Internal deliberately
// says nothing about the cause.
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::NotFound => f.write_str("Not found."),
            ApiError::Unauthorized => f.write_str("Unauthorized."),
            ApiError::TooManyRequests => f.write_str("Too many requests."),
            ApiError::BadRequest(message)
            | ApiError::Conflict(message)
            | ApiError::ServiceUnavailable(message) => f.write_str(message),
            ApiError::SwishRejected { code, body } => {
                write!(f, "Swish rejected the request ({code}): {body}")
            }
            ApiError::Internal(_) => f.write_str("Internal server error."),
        }
    }
}

impl std::error::Error for ApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ApiError::Internal(cause) => Some(&**cause),
            _ => None,
        }
    }
}
