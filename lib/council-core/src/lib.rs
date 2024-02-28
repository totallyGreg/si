use std::{collections::HashMap, fmt, ops, result, str::FromStr};

use serde::{Deserialize, Serialize};
use si_data_nats::async_nats::jetstream;
use strum::{AsRefStr, Display};
use ulid::Ulid;

mod kv;

const NATS_KV_BUCKET_NAME: &str = "COUNCIL";
const NATS_INCOMING_STREAM_NAME: &str = "COUNCIL_INCOMING";
const NATS_INCOMING_STREAM_SUBJECTS: &[&str] = &["council.incoming.>"];

pub use self::kv::OperationEntry;

#[remain::sorted]
#[derive(AsRefStr, Clone, Copy, Debug, Deserialize, Display, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AttributeValueStatus {
    Available { client: ClientId },
    Failed { client: ClientId },
    Orphaned,
    Pending { client: ClientId },
    Processing { client: ClientId },
}

pub async fn council_incoming_stream(
    context: &jetstream::Context,
    prefix: Option<&str>,
) -> Result<jetstream::stream::Stream, jetstream::context::CreateStreamError> {
    let subjects: Vec<_> = NATS_INCOMING_STREAM_SUBJECTS
        .iter()
        .map(|suffix| subject::nats_subject(prefix, suffix).to_string())
        .collect();

    let stream = context
        .get_or_create_stream(jetstream::stream::Config {
            name: nats_stream_name(prefix, NATS_INCOMING_STREAM_NAME),
            description: Some("Council internal and external incoming updates".to_owned()),
            retention: jetstream::stream::RetentionPolicy::WorkQueue,
            discard: jetstream::stream::DiscardPolicy::New,
            subjects,
            ..Default::default()
        })
        .await?;

    Ok(stream)
}

pub async fn council_kv(
    context: &jetstream::Context,
    prefix: Option<&str>,
) -> Result<jetstream::kv::Store, jetstream::context::CreateKeyValueError> {
    let bucket = nats_stream_name(prefix, NATS_KV_BUCKET_NAME);

    let kv = context
        .create_key_value(jetstream::kv::Config {
            bucket,
            description: "Council internal state and external statuses".to_owned(),
            history: 10,
            ..Default::default()
        })
        .await?;

    Ok(kv)
}

fn nats_stream_name(prefix: Option<&str>, suffix: impl AsRef<str>) -> String {
    let suffix = suffix.as_ref();

    match prefix {
        Some(prefix) => format!("{prefix}_{suffix}"),
        None => suffix.to_owned(),
    }
}

pub mod subject {
    use si_data_nats::Subject;

    use crate::{ChangeSetPk, ClientId};

    pub const INCOMING_PREFIX_WITH_DOT: &str = "council.incoming.";
    const INCOMING_SUBJECT: &str = "council.incoming.*.*";
    pub const INCOMING_CLIENT_PREFIX: &str = "council.incoming.client";
    pub const INCOMING_CS_PREFIX: &str = "council.incoming.cs";
    pub const INCOMING_STEP_PREFIX: &str = "council.incoming.step";
    const INCOMING_FIND_READY_TO_PROCESS_SUBJECT: &str =
        "council.incoming.step.find_ready_to_process";

    #[inline]
    pub fn incoming(prefix: Option<&str>) -> Subject {
        nats_subject(prefix, INCOMING_SUBJECT)
    }

    #[inline]
    pub fn incoming_for_change_set(prefix: Option<&str>, change_set_pk: ChangeSetPk) -> Subject {
        nats_subject(prefix, format!("{INCOMING_CS_PREFIX}.{change_set_pk}"))
    }

    #[inline]
    pub fn find_ready_to_process(prefix: Option<&str>) -> Subject {
        nats_subject(prefix, INCOMING_FIND_READY_TO_PROCESS_SUBJECT)
    }

    #[inline]
    pub fn incoming_for_client(prefix: Option<&str>, client_id: ClientId) -> Subject {
        nats_subject(prefix, format!("{INCOMING_CLIENT_PREFIX}.{client_id}"))
    }

    pub(crate) fn nats_subject(prefix: Option<&str>, suffix: impl AsRef<str>) -> Subject {
        let suffix = suffix.as_ref();
        match prefix {
            Some(prefix) => Subject::from(format!("{prefix}.{suffix}")),
            None => Subject::from(suffix),
        }
    }

    pub mod key {
        pub mod state {
            use crate::ChangeSetPk;

            pub const STATE_CS_PREFIX: &str = "state.cs";

            #[inline]
            pub fn change_set(change_set_pk: ChangeSetPk) -> String {
                format!("{STATE_CS_PREFIX}.{change_set_pk}")
            }
        }

        pub mod status {
            use crate::{AttributeValueId, ChangeSetPk};

            pub const STATUS_CS_PREFIX: &str = "status.cs";

            #[inline]
            pub fn attribute_value(
                change_set_pk: ChangeSetPk,
                attribute_value_id: AttributeValueId,
            ) -> String {
                format!("{STATUS_CS_PREFIX}.{change_set_pk}.av.{attribute_value_id}")
            }

            #[inline]
            pub fn change_set_active(change_set_pk: ChangeSetPk) -> String {
                format!("{STATUS_CS_PREFIX}.{change_set_pk}.active")
            }

            #[inline]
            pub fn change_set_statuses(change_set_pk: ChangeSetPk) -> String {
                format!("{STATUS_CS_PREFIX}.{change_set_pk}.>")
            }
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize, Hash)]
pub struct ClientId(Ulid);

impl ClientId {
    pub fn into_inner(self) -> Ulid {
        self.0
    }
}

impl Default for ClientId {
    fn default() -> Self {
        Self(Ulid::new())
    }
}

impl FromStr for ClientId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> result::Result<Self, Self::Err> {
        Ok(Self(Ulid::from_str(s)?))
    }
}

impl fmt::Debug for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ClientId")
            .field(&self.0.to_string())
            .finish()
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize, Hash)]
pub struct ChangeSetPk(Ulid);

impl ChangeSetPk {
    pub fn into_inner(self) -> Ulid {
        self.0
    }
}

impl Default for ChangeSetPk {
    fn default() -> Self {
        Self(Ulid::new())
    }
}

impl FromStr for ChangeSetPk {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> result::Result<Self, Self::Err> {
        Ok(Self(Ulid::from_str(s)?))
    }
}

impl From<Ulid> for ChangeSetPk {
    fn from(value: Ulid) -> Self {
        Self(value)
    }
}

impl fmt::Debug for ChangeSetPk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ChangeSetId")
            .field(&self.0.to_string())
            .finish()
    }
}

impl fmt::Display for ChangeSetPk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize, Hash)]
pub struct AttributeValueId(Ulid);

impl AttributeValueId {
    pub fn into_inner(self) -> Ulid {
        self.0
    }
}

impl Default for AttributeValueId {
    fn default() -> Self {
        Self(Ulid::new())
    }
}

impl FromStr for AttributeValueId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> result::Result<Self, Self::Err> {
        Ok(Self(Ulid::from_str(s)?))
    }
}

impl From<Ulid> for AttributeValueId {
    fn from(value: Ulid) -> Self {
        Self(value)
    }
}

impl fmt::Debug for AttributeValueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AttributeValueId")
            .field(&self.0.to_string())
            .finish()
    }
}

impl fmt::Display for AttributeValueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DependencyGraph(HashMap<AttributeValueId, Vec<AttributeValueId>>);

impl DependencyGraph {
    pub fn into_inner(self) -> HashMap<AttributeValueId, Vec<AttributeValueId>> {
        self.0
    }
}

impl ops::Deref for DependencyGraph {
    type Target = HashMap<AttributeValueId, Vec<AttributeValueId>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ops::DerefMut for DependencyGraph {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<HashMap<AttributeValueId, Vec<AttributeValueId>>> for DependencyGraph {
    fn from(value: HashMap<AttributeValueId, Vec<AttributeValueId>>) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DependencyGraphRequest {
    pub client_id: ClientId,
    pub graph: DependencyGraph,
}

#[remain::sorted]
#[derive(AsRefStr, Clone, Copy, Debug, Deserialize, Display, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ClientUpdateRequest {
    /// An acknowledgement that a client will start to process an attribute value.
    AckPending {
        change_set_pk: ChangeSetPk,
        attribute_value_id: AttributeValueId,
    },
    /// An acknowledgement that an attribute value has been successfully processed by a client.
    AckProcessed {
        change_set_pk: ChangeSetPk,
        attribute_value_id: AttributeValueId,
    },
    /// An negative acknowledgement (i.e. a "nack") messaging that an attribute value has failed to
    /// be processed by a client.
    NackProcessed {
        change_set_pk: ChangeSetPk,
        attribute_value_id: AttributeValueId,
    },
    /// A client has messaged that it is departing from collaboration on a change set.
    Shutdown { change_set_pk: ChangeSetPk },
}
