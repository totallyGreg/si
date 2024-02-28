use std::sync::Arc;

use naxum::extract::FromRef;

use crate::{StateMachine, StateStore, StatusStore};

#[derive(Clone, Debug)]
pub struct Prefix(pub Arc<Option<String>>);

impl From<Option<String>> for Prefix {
    fn from(value: Option<String>) -> Self {
        Self(Arc::new(value))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AttributeValueMaxAttempts(pub u16);

impl From<u16> for AttributeValueMaxAttempts {
    fn from(value: u16) -> Self {
        Self(value)
    }
}

#[derive(Clone)]
pub struct AppState {
    prefix: Prefix,
    state: StateStore,
    status: StatusStore,
    state_machine: StateMachine,
    attribute_value_max_attempts: AttributeValueMaxAttempts,
}

impl AppState {
    pub fn new(
        prefix: Prefix,
        state_machine: StateMachine,
        state: StateStore,
        status: StatusStore,
        attribute_value_max_attempts: AttributeValueMaxAttempts,
    ) -> Self {
        Self {
            prefix,
            state_machine,
            state,
            status,
            attribute_value_max_attempts,
        }
    }

    pub fn prefix(&self) -> Option<&str> {
        self.prefix.0.as_deref()
    }

    pub fn state_machine(&self) -> &StateMachine {
        &self.state_machine
    }

    pub fn state(&self) -> &StateStore {
        &self.state
    }

    pub fn status(&self) -> &StatusStore {
        &self.status
    }

    pub fn attribute_value_max_attempts(&self) -> AttributeValueMaxAttempts {
        self.attribute_value_max_attempts
    }
}

impl FromRef<AppState> for Prefix {
    fn from_ref(input: &AppState) -> Self {
        input.prefix.clone()
    }
}

impl FromRef<AppState> for StateMachine {
    fn from_ref(input: &AppState) -> Self {
        input.state_machine.clone()
    }
}

impl FromRef<AppState> for StateStore {
    fn from_ref(input: &AppState) -> Self {
        input.state.clone()
    }
}

impl FromRef<AppState> for StatusStore {
    fn from_ref(input: &AppState) -> Self {
        input.status.clone()
    }
}
