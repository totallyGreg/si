use std::convert::Infallible;

use council_core::subject;
use naxum::{async_trait, extract::FromMessageHead, Head};
use si_data_nats::Subject;

use crate::app_state::AppState;

pub struct IncomingSubject(pub Subject);

#[async_trait]
impl FromMessageHead<AppState> for IncomingSubject {
    type Rejection = Infallible;

    async fn from_message_head(head: &mut Head, state: &AppState) -> Result<Self, Self::Rejection> {
        let subject_str = match state.prefix() {
            Some(prefix) => head
                .subject
                .strip_prefix(prefix)
                .unwrap_or_else(|| head.subject.as_str()),
            None => head.subject.as_str(),
        };

        Ok(Self(Subject::from(
            subject_str
                .strip_prefix(subject::INCOMING_PREFIX_WITH_DOT)
                .unwrap_or(subject_str),
        )))
    }
}
