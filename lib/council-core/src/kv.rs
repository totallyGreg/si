use si_data_nats::async_nats::jetstream::kv::{self, Entry};

#[derive(Debug, Clone)]
pub enum OperationEntry {
    Put(Entry),
    Delete(Entry),
    Purge(Entry),
}

impl OperationEntry {
    pub fn key(&self) -> &str {
        match self {
            OperationEntry::Put(inner) => inner.key.as_str(),
            OperationEntry::Delete(inner) => inner.key.as_str(),
            OperationEntry::Purge(inner) => inner.key.as_str(),
        }
    }

    pub fn is_removed(&self) -> bool {
        match self {
            Self::Delete(_) | Self::Purge(_) => true,
            Self::Put(_) => false,
        }
    }

    pub fn is_updated(&self) -> bool {
        match self {
            Self::Put(_) => true,
            Self::Delete(_) | Self::Purge(_) => false,
        }
    }

    pub fn into_inner(self) -> Entry {
        match self {
            Self::Put(inner) => inner,
            Self::Delete(inner) => inner,
            Self::Purge(inner) => inner,
        }
    }
}

impl From<Entry> for OperationEntry {
    fn from(value: Entry) -> Self {
        match value.operation {
            kv::Operation::Put => Self::Put(value),
            kv::Operation::Delete => Self::Delete(value),
            kv::Operation::Purge => Self::Purge(value),
        }
    }
}
