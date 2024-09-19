use naxum::{
    async_trait, composite_rejection, define_rejection,
    extract::{FromMessage, FromMessageHead},
    Head, MessageHead,
};
use rebaser_core::{
    api_types::{ApiVersionsWrapper, ApiWrapper},
    nats,
};
use si_data_nats::Subject;
use telemetry::prelude::*;

define_rejection! {
    #[status_code = 400]
    #[body = "Invalid decoding string as utf8"]
    pub struct InvalidUtf8Error(Error);
}

#[derive(Debug)]
pub struct HeaderReply(pub Option<Subject>);

#[async_trait]
impl<S> FromMessageHead<S> for HeaderReply {
    type Rejection = InvalidUtf8Error;

    async fn from_message_head(head: &mut Head, _state: &S) -> Result<Self, Self::Rejection> {
        let maybe_value = head.headers.as_ref().and_then(|headers| {
            headers
                .get(nats::NATS_HEADER_REPLY_INBOX_NAME)
                .map(|value| value.to_string())
        });

        match maybe_value {
            Some(value) => Ok(Self(Some(
                Subject::from_utf8(value).map_err(InvalidUtf8Error::from_err)?,
            ))),
            None => Ok(Self(None)),
        }
    }
}

define_rejection! {
    #[status_code = 400]
    #[body = "Headers are required for content info but none was found"]
    pub struct HeadersMissing;
}

define_rejection! {
    #[status_code = 400]
    #[body = "Failed to parse content info from headers"]
    pub struct HeadersParseError(Error);
}

composite_rejection! {
    pub enum ContentInfoRejection {
        HeadersMissing,
        HeadersParseError,
    }
}

#[derive(Debug)]
pub struct ContentInfo(pub rebaser_core::ContentInfo<'static>);

#[async_trait]
impl<S> FromMessageHead<S> for ContentInfo {
    type Rejection = ContentInfoRejection;

    async fn from_message_head(head: &mut Head, _state: &S) -> Result<Self, Self::Rejection> {
        let headers = head.headers.as_ref().ok_or(HeadersMissing)?;
        let content_info =
            rebaser_core::ContentInfo::try_from(headers).map_err(HeadersParseError::from_err)?;

        Ok(Self(content_info))
    }
}

define_rejection! {
    #[status_code = 400]
    #[body = "failed to deserialize message payload"]
    pub struct DeserializeError(Error);
}

define_rejection! {
    #[status_code = 400]
    #[body = "failed to upgrade message to current version"]
    pub struct MessageUpgradeError(Error);
}

define_rejection! {
    #[status_code = 415]
    #[body = "unsupported content type"]
    pub struct UnsupportedContentTypeError;
}

define_rejection! {
    #[status_code = 406]
    #[body = "unsupported message type"]
    pub struct UnsupportedMessageTypeError;
}

define_rejection! {
    #[status_code = 406]
    #[body = "unsupported message version"]
    pub struct UnsupportedMessageVersionError;
}

composite_rejection! {
    pub enum ApiTypesNegotiateRejection {
        ContentInfoRejection,
        DeserializeError,
        MessageUpgradeError,
        UnsupportedContentTypeError,
        UnsupportedMessageTypeError,
        UnsupportedMessageVersionError,
    }
}

#[derive(Clone, Copy, Debug, Default)]
#[must_use]
pub struct ApiTypesNegotiate<T>(pub T);

#[async_trait]
impl<T, S, R> FromMessage<S, R> for ApiTypesNegotiate<T>
where
    T: ApiWrapper,
    R: MessageHead + Send + 'static,
    S: Send + Sync,
{
    type Rejection = ApiTypesNegotiateRejection;

    async fn from_message(req: R, state: &S) -> Result<Self, Self::Rejection> {
        let (mut head, payload) = req.into_parts();
        let ContentInfo(content_info) = ContentInfo::from_message_head(&mut head, state).await?;

        if !T::is_content_type_supported(content_info.content_type.as_str()) {
            return Err(UnsupportedContentTypeError.into());
        }
        if !T::is_message_type_supported(content_info.message_type.as_str()) {
            return Err(UnsupportedMessageTypeError.into());
        }
        if !T::is_message_version_supported(content_info.message_version.as_u64()) {
            return Err(UnsupportedMessageVersionError.into());
        }

        let deserialized_versions = T::from_slice(content_info.content_type.as_str(), &payload)
            .map_err(DeserializeError::from_err)?;
        let current_version = deserialized_versions
            .into_current_version()
            .map_err(MessageUpgradeError::from_err)?;

        Ok(Self(current_version))
    }
}
