#[macro_use]
pub(crate) mod macros;

mod cancellation;
mod error;
pub mod error_handling;
pub mod extract;
pub mod handler;
mod json;
mod make_service;
mod message;
pub mod middleware;
pub mod response;
pub mod serve;
mod service_ext;

pub use self::cancellation::wait_on_cancelled;
pub use self::error::Error;
pub use self::json::Json;
pub use self::make_service::IntoMakeService;
pub use self::message::{Head, MessageHead};
pub use self::serve::{serve, serve_with_incoming_limit};
pub use self::service_ext::ServiceExt;

pub use async_nats::StatusCode;
pub use async_trait::async_trait;
pub use tower::ServiceBuilder;

/// Alias for a type-erased error type.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
