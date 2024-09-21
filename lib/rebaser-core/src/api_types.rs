pub mod enqueue_updates_request;
pub mod enqueue_updates_response;

use serde::{de::DeserializeOwned, Serialize};
use strum::VariantNames;
use thiserror::Error;

const CONTENT_TYPE_CBOR: &str = "application/cbor";
const CONTENT_TYPE_JSON: &str = "application/json";

/// Alias for a type-erased error type.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Error)]
#[error("error serializing message: {0}")]
pub struct SerializeError(#[source] BoxError);

impl SerializeError {
    pub fn from_err<E>(err: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self(Box::new(err))
    }
}

#[derive(Debug, Error)]
#[error("error derializing message: {0}")]
pub struct DeserializeError(#[source] BoxError);

impl DeserializeError {
    pub fn from_err<E>(err: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self(Box::new(err))
    }
}

#[derive(Debug, Error)]
#[error("error upgrading message: {0}")]
pub struct UpgradeError(#[source] BoxError);

impl UpgradeError {
    pub fn from_err<E>(err: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self(Box::new(err))
    }
}

// TODO(fnichol): Guess what? We should consolidate these `id!` macros! Here's the current catch:
// most of our "event" ids require serializing for over the wire transmission but don't always need
// to write into the Postgres database. A lot of our `id!` macros out of dal (and now si-events)
// get coupled to requiring Postgres marshalling so we just tack that on. For the moment this id
// type has no Postgres requirements and so wil purposfully *omit* the `postgres_types::FromSql`
// trait implementations. It think there's a way to split these concners up, but it requires a
// couple more small crates and I'm just not down to do that right this moment. Cheers!
#[macro_export]
macro_rules! id {
    (
        $(#[$($attrss:tt)*])*
        $name:ident
    ) => {
        $(#[$($attrss)*])*
        #[allow(missing_docs)]
        #[derive(
            Eq,
            PartialEq,
            PartialOrd,
            Ord,
            Copy,
            Clone,
            Hash,
            Default,
            derive_more::From,
            derive_more::Into,
            derive_more::Display,
            serde::Serialize,
            serde::Deserialize,
        )]
        pub struct $name(::ulid::Ulid);

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_tuple(stringify!($name)).field(&self.0.to_string()).finish()
            }
        }

        impl $name {
            /// Length of a string-encoded ID in bytes.
            pub const ID_LEN: usize = ::ulid::ULID_LEN;

            /// Generates a new key which is virtually guaranteed to be unique.
            pub fn generate() -> Self {
                Self(::ulid::Ulid::new())
            }

            pub fn new() -> Self {
                Self::generate()
            }

            pub fn array_to_str<'buf>(&self, buf: &'buf mut [u8; ::ulid::ULID_LEN]) -> &'buf mut str {
                self.0.array_to_str(buf)
            }

            pub fn array_to_str_buf() -> [u8; ::ulid::ULID_LEN] {
                [0; ::ulid::ULID_LEN]
            }

            /// Constructs a new instance of Self from the given raw identifier.
            ///
            /// This function is typically used to consume ownership of the specified identifier.
            pub fn from_raw_id(value: ::ulid::Ulid) -> Self {
                Self(value)
            }

            /// Extracts the raw identifier.
            ///
            /// This function is typically used to borrow an owned idenfier.
            pub fn as_raw_id(&self) -> ::ulid::Ulid {
                self.0
            }

            /// Consumes this object, returning the raw underlying identifier.
            ///
            /// This function is typically used to transfer ownership of the underlying identifier
            /// to the caller.
            pub fn into_raw_id(self) -> ::ulid::Ulid {
                self.0
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                ulid::Ulid::from(id.0).into()
            }
        }

        impl<'a> From<&'a $name> for $name {
            fn from(id: &'a $name) -> Self {
                *id
            }
        }

        impl std::str::FromStr for $name {
            type Err = ::ulid::DecodeError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(::ulid::Ulid::from_string(s)?))
            }
        }

    };
}

id!(RequestId);

pub trait ApiWrapper: Serialize + strum::VariantNames {
    type VersionsTarget: ApiVersionsWrapper<Target = Self> + DeserializeOwned + strum::VariantNames;
    type Current;

    const MESSAGE_TYPE: &'static str;

    fn id(&self) -> RequestId;

    fn new_current(current: Self::Current) -> Self;

    fn is_content_type_supported(ty: &str) -> bool {
        matches!(ty, CONTENT_TYPE_CBOR | CONTENT_TYPE_JSON)
    }

    fn is_message_type_supported(ty: &str) -> bool {
        Self::MESSAGE_TYPE == ty
    }

    fn is_message_version_supported(version: u64) -> bool {
        let variant = format!("v{version}");

        Self::VersionsTarget::VARIANTS.iter().any(|v| *v == variant)
    }

    fn default_content_type() -> &'static str {
        CONTENT_TYPE_CBOR
    }

    fn message_type() -> &'static str {
        Self::MESSAGE_TYPE
    }

    #[allow(clippy::panic)] // TODO(fnichol): this is better solved with a procedural macro, later
    fn message_version() -> u64 {
        if Self::VARIANTS.len() != 1 {
            panic!(
                "{} must only have variant; this is a bug!",
                Self::MESSAGE_TYPE
            );
        }
        let version_str = Self::VARIANTS
            .first()
            .and_then(|s| s.strip_prefix('v'))
            .expect("number of variants is one, already checked");
        version_str
            .parse()
            .expect("variant must be of the form `vN`; this is a bug!")
    }

    fn from_json_slice(bytes: &[u8]) -> Result<Self::VersionsTarget, DeserializeError> {
        serde_json::from_slice(bytes).map_err(DeserializeError::from_err)
    }

    fn from_cbor_slice(bytes: &[u8]) -> Result<Self::VersionsTarget, DeserializeError> {
        ciborium::from_reader(bytes).map_err(DeserializeError::from_err)
    }

    #[inline]
    fn from_slice(
        content_type: &str,
        bytes: &[u8],
    ) -> Result<Self::VersionsTarget, DeserializeError> {
        match content_type {
            CONTENT_TYPE_CBOR => Self::from_cbor_slice(bytes),
            CONTENT_TYPE_JSON => Self::from_json_slice(bytes),
            unexpected => unimplemented!("TODO: {unexpected} content type is unsupported!"),
        }
    }

    fn to_json_vec(&self) -> Result<Vec<u8>, SerializeError> {
        serde_json::to_vec(self).map_err(SerializeError::from_err)
    }

    fn to_cbor_vec(&self) -> Result<Vec<u8>, SerializeError> {
        let mut bytes = Vec::new();
        ciborium::into_writer(self, &mut bytes).map_err(SerializeError::from_err)?;
        Ok(bytes)
    }

    #[inline]
    fn to_vec(&self) -> Result<Vec<u8>, SerializeError> {
        self.to_cbor_vec()
    }
}

pub trait ApiVersionsWrapper {
    type Target;

    fn into_current_version(self) -> Result<Self::Target, UpgradeError>;

    fn id(&self) -> RequestId;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        assert!(CoolRequest::is_message_type_supported("CoolRequest"));
        assert!(!CoolRequest::is_message_type_supported("Nope"));

        assert!(CoolRequest::is_content_type_supported("application/cbor"));
        assert!(!CoolRequest::is_content_type_supported("text/plain"));

        assert!(CoolRequest::is_message_version_supported(1));
        assert!(CoolRequest::is_message_version_supported(2));
        assert!(CoolRequest::is_message_version_supported(3));
        assert!(CoolRequest::is_message_version_supported(4));

        assert!(!CoolRequest::is_message_version_supported(0));
        assert!(!CoolRequest::is_message_version_supported(5));
        assert!(!CoolRequest::is_message_version_supported(42));

        let req_old = CoolRequestVersions::V2(CoolRequestV2 {
            id: RequestId::new(),
        });
        dbg!(&req_old);

        let mut req = req_old.into_current_version().expect("failed to upgrade");
        dbg!(&req);

        dbg!(&req.organization);
        req.organization = Some("acme".to_string());
        dbg!(&req.organization);

        dbg!(&req);

        publish(req).expect("failed to publish");
    }

    mod v1 {
        use serde::{Deserialize, Serialize};

        use super::RequestId;

        #[derive(Clone, Debug, Deserialize, Eq, Serialize, PartialEq)]
        #[serde(rename_all = "camelCase")]
        pub struct CoolRequestV1 {
            pub id: RequestId,
        }
    }

    mod v2 {
        use serde::{Deserialize, Serialize};

        use super::{v1::CoolRequestV1, RequestId, UpgradeError};

        #[derive(Clone, Debug, Deserialize, Eq, Serialize, PartialEq)]
        #[serde(rename_all = "camelCase")]
        pub struct CoolRequestV2 {
            pub id: RequestId,
        }

        impl TryFrom<CoolRequestV1> for CoolRequestV2 {
            type Error = UpgradeError;

            fn try_from(value: CoolRequestV1) -> Result<Self, Self::Error> {
                Ok(Self { id: value.id })
            }
        }
    }

    mod v3 {
        use serde::{Deserialize, Serialize};

        use super::{v2::CoolRequestV2, RequestId, UpgradeError};

        #[derive(Clone, Debug, Deserialize, Eq, Serialize, PartialEq)]
        #[serde(rename_all = "camelCase")]
        pub struct CoolRequestV3 {
            pub id: RequestId,
            pub name: String,
        }

        impl TryFrom<CoolRequestV2> for CoolRequestV3 {
            type Error = UpgradeError;

            fn try_from(value: CoolRequestV2) -> Result<Self, Self::Error> {
                Ok(Self {
                    id: value.id,
                    name: "<unknown>".to_string(),
                })
            }
        }
    }

    mod v4 {
        use serde::{Deserialize, Serialize};

        use super::{v3::CoolRequestV3, RequestId, UpgradeError};

        #[derive(Clone, Debug, Deserialize, Eq, Serialize, PartialEq)]
        #[serde(rename_all = "camelCase")]
        pub struct CoolRequestV4 {
            pub id: RequestId,
            pub name: String,
            pub organization: Option<String>,
        }

        impl TryFrom<CoolRequestV3> for CoolRequestV4 {
            type Error = UpgradeError;

            fn try_from(value: CoolRequestV3) -> Result<Self, Self::Error> {
                Ok(Self {
                    id: value.id,
                    name: value.name,
                    organization: None,
                })
            }
        }
    }

    use std::{
        fmt,
        ops::{Deref, DerefMut},
    };

    use serde::{Deserialize, Serialize};
    use strum::{AsRefStr, EnumDiscriminants, EnumIs, EnumString, VariantNames};
    use thiserror::Error;

    use self::{v1::CoolRequestV1, v2::CoolRequestV2, v3::CoolRequestV3, v4::CoolRequestV4};

    pub type CoolRequestVCurrent = CoolRequestV4;

    #[derive(Debug, Error)]
    #[error("failed to convert CoolRequest into {0}")]
    pub struct CoolRequestConversionError(&'static str);

    #[derive(Clone, Eq, Serialize, PartialEq, VariantNames)]
    #[serde(rename_all = "camelCase")]
    pub enum CoolRequest {
        V4(CoolRequestV4),
    }

    impl ApiWrapper for CoolRequest {
        type VersionsTarget = CoolRequestVersions;
        type Current = CoolRequestVCurrent;

        const MESSAGE_TYPE: &'static str = "CoolRequest";

        fn id(&self) -> RequestId {
            match self {
                Self::V4(inner) => inner.id,
            }
        }

        fn new_current(current: Self::Current) -> Self {
            Self::V4(current)
        }
    }

    impl CoolRequest {}

    impl fmt::Debug for CoolRequest {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::V4(inner) => inner.fmt(f),
            }
        }
    }

    impl Deref for CoolRequest {
        type Target = CoolRequestVCurrent;

        fn deref(&self) -> &Self::Target {
            match self {
                Self::V4(inner) => inner,
            }
        }
    }

    impl DerefMut for CoolRequest {
        fn deref_mut(&mut self) -> &mut Self::Target {
            match self {
                CoolRequest::V4(inner) => inner,
            }
        }
    }

    #[remain::sorted]
    #[derive(Clone, Debug, Deserialize, EnumDiscriminants, EnumIs, Eq, PartialEq, VariantNames)]
    #[serde(rename_all = "camelCase")]
    #[strum(serialize_all = "camelCase")]
    #[strum_discriminants(strum(serialize_all = "camelCase"), derive(AsRefStr, EnumString))]
    pub enum CoolRequestVersions {
        V1(CoolRequestV1),
        V2(CoolRequestV2),
        V3(CoolRequestV3),
        V4(CoolRequestV4),
    }

    impl ApiVersionsWrapper for CoolRequestVersions {
        type Target = CoolRequest;

        fn id(&self) -> RequestId {
            match self {
                Self::V1(CoolRequestV1 { id })
                | Self::V2(CoolRequestV2 { id })
                | Self::V3(CoolRequestV3 { id, .. })
                | Self::V4(CoolRequestV4 { id, .. }) => *id,
            }
        }

        fn into_current_version(mut self) -> Result<Self::Target, UpgradeError> {
            loop {
                match self {
                    Self::V1(inner) => self = Self::V2(CoolRequestV2::try_from(inner)?),
                    Self::V2(inner) => self = Self::V3(CoolRequestV3::try_from(inner)?),
                    Self::V3(inner) => self = Self::V4(CoolRequestV4::try_from(inner)?),
                    Self::V4(inner) => return Ok(Self::Target::V4(inner)),
                }
            }
        }
    }

    fn publish(req: CoolRequest) -> Result<(), BoxError> {
        let bytes = req.to_json_vec()?;
        let s = String::from_utf8(bytes)?;
        dbg!(s);
        Ok(())
    }
}
