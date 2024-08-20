use std::fmt;

use async_nats::StatusCode;

mod into_response;

pub use self::into_response::IntoResponse;

#[derive(Debug, Default)]
pub struct Response {
    head: Parts,
    body: (),
}

#[derive(Default)]
#[non_exhaustive]
pub struct Parts {
    pub status: StatusCode,
}

impl Response {
    #[inline]
    pub fn new(body: ()) -> Self {
        Self {
            head: Parts::new(),
            body,
        }
    }

    pub fn server_error() -> Self {
        Self {
            head: Parts {
                status: StatusCode::from_u16(500).expect("status code is in valid range"),
            },
            body: (),
        }
    }

    pub fn status(&self) -> StatusCode {
        self.head.status
    }

    pub fn status_mut(&mut self) -> &mut StatusCode {
        &mut self.head.status
    }
}

impl Parts {
    fn new() -> Self {
        Self {
            status: StatusCode::default(),
        }
    }
}

impl fmt::Debug for Parts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Parts")
            .field("status", &self.status)
            .finish()
    }
}

pub type Result<T, E = ErrorResponse> = std::result::Result<T, E>;

impl<T> IntoResponse for Result<T>
where
    T: IntoResponse,
{
    fn into_response(self) -> Response {
        match self {
            Ok(ok) => ok.into_response(),
            Err(err) => err.0,
        }
    }
}

#[derive(Debug)]
pub struct ErrorResponse(Response);

impl<T> From<T> for ErrorResponse
where
    T: IntoResponse,
{
    fn from(value: T) -> Self {
        #[allow(clippy::unit_arg)]
        Self(value.into_response())
    }
}
