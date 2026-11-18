//! Durable admission logic over transactional storage ports.

use sha2::{Digest, Sha256};

mod chunks;
mod fences;
mod leases;
mod local_projection;
mod notifications;
mod timeline;

pub use chunks::{Chunk, ChunkError, Manifest, read_payload, write_payload};
pub use fences::commit_fenced;
pub use leases::{acquire_mutation, lease_active};
pub use local_projection::{
    ReadProjectionError, create_projection, projection_parent, read_projection, replace_projection,
};
pub use notifications::{CommitVersions, ConversationKey};
pub use timeline::{
    AppendFailure, EventConflict, EventIntent, TimelineAppend, append, append_transaction,
    reconcile_event,
};

pub fn document_identity(prefix: &str, value: &str) -> String {
    format!("{prefix}{}", digest(value.as_bytes()))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
