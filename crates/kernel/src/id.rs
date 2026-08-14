use protocol::{IdGenerator, IdKind};

/// Production identifier generator backed by random Nano IDs.
#[derive(Debug, Clone, Copy, Default)]
pub struct NanoidIdGenerator;

impl IdGenerator for NanoidIdGenerator {
    /// Generates a prefixed random identifier suitable for persisted correlation.
    fn next(&self, kind: IdKind) -> String {
        format!("{}-{}", kind.prefix(), nanoid::nanoid!())
    }
}
