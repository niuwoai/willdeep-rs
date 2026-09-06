use super::{Session, SessionError};
use sha2::{Digest, Sha256};

struct DigestWriter(Sha256);

impl std::io::Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn fingerprint(session: &Session) -> Result<[u8; 32], SessionError> {
    let mut writer = DigestWriter(Sha256::new());
    // These fields form one execution snapshot. Never merge messages separately
    // from their checkpoint or compression boundary.
    serde_json::to_writer(
        &mut writer,
        &(
            session.version,
            session.id,
            &session.workspace,
            session.created_at,
            &session.messages,
            &session.attention_read,
            session.runtime_event_cursor,
            session.runtime_managed,
            session.compression_generation,
            &session.compression_checkpoint,
            &session.manual_compression_usage,
            &session.execution_checkpoint,
        ),
    )?;
    Ok(writer.0.finalize().into())
}

pub(super) fn copy(source: &Session, target: &mut Session) {
    target.version = source.version;
    target.id = source.id;
    target.workspace = source.workspace.clone();
    target.created_at = source.created_at;
    target.messages = source.messages.clone();
    target.attention_read = source.attention_read.clone();
    target.runtime_event_cursor = source.runtime_event_cursor;
    target.runtime_managed = source.runtime_managed;
    target.compression_generation = source.compression_generation;
    target.compression_checkpoint = source.compression_checkpoint.clone();
    target.manual_compression_usage = source.manual_compression_usage.clone();
    target.execution_checkpoint = source.execution_checkpoint.clone();
}
