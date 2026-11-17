//! Content-addressed payload trees, independent of the database transport.

use std::{collections::HashSet, fmt};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;

use crate::digest;

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Chunk {
    Data(String),
    Children(Vec<String>),
}

impl Chunk {
    fn identity(&self) -> Result<String, serde_json::Error> {
        let encoded = serde_json::to_vec(self)?;
        Ok(format!("payload-chunk:{}", digest(&encoded)))
    }
}

pub struct Manifest {
    pub root: String,
    pub sha256: String,
    pub bytes: usize,
}

#[derive(Debug)]
pub enum ChunkError<E> {
    Storage(E),
    Invalid(&'static str),
}

impl<E: fmt::Display> fmt::Display for ChunkError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(f),
            Self::Invalid(error) => f.write_str(error),
        }
    }
}

/// Store the leaves before their reference pages. Only the caller publishes
/// the manifest, once every chunk has been confirmed durable.
pub fn write_payload<E>(
    raw: &[u8],
    chunk_bytes: usize,
    fanout: usize,
    mut create: impl FnMut(&str, Chunk) -> Result<(), E>,
) -> Result<Manifest, ChunkError<E>> {
    if chunk_bytes == 0 || fanout < 2 || raw.is_empty() {
        return Err(ChunkError::Invalid("invalid Cosmos payload chunk bounds"));
    }
    let mut store = |chunk: Chunk| {
        let identity = chunk
            .identity()
            .map_err(|_| ChunkError::Invalid("invalid Cosmos payload chunk"))?;
        create(&identity, chunk).map_err(ChunkError::Storage)?;
        Ok(identity)
    };
    let mut references: Vec<_> = raw
        .chunks(chunk_bytes)
        .map(|leaf| store(Chunk::Data(STANDARD.encode(leaf))))
        .collect::<Result<_, _>>()?;
    while references.len() > 1 {
        references = references
            .chunks(fanout)
            .map(|page| store(Chunk::Children(page.to_vec())))
            .collect::<Result<_, _>>()?;
    }
    let root = references
        .pop()
        .ok_or(ChunkError::Invalid("Cosmos payload has no root chunk"))?;
    Ok(Manifest {
        root,
        sha256: digest(raw),
        bytes: raw.len(),
    })
}

enum Visit {
    Enter(String),
    Exit(String),
}

/// Validate each reference before descending. Remove it from the active
/// ancestor set after descent so shared DAG leaves can appear more than once.
pub fn read_payload<E>(
    manifest: Manifest,
    fanout: usize,
    mut read: impl FnMut(&str) -> Result<Chunk, E>,
) -> Result<Vec<u8>, ChunkError<E>> {
    let mut pending = vec![Visit::Enter(manifest.root)];
    let mut ancestors = HashSet::new();
    let mut raw = Vec::new();
    while let Some(visit) = pending.pop() {
        let identity = match visit {
            Visit::Enter(identity) => identity,
            Visit::Exit(identity) => {
                ancestors.remove(&identity);
                continue;
            }
        };
        if ancestors.contains(&identity) {
            return Err(ChunkError::Invalid("cyclic Cosmos payload references"));
        }
        let chunk = read(&identity).map_err(ChunkError::Storage)?;
        if !chunk
            .identity()
            .is_ok_and(|stored_identity| stored_identity == identity)
        {
            return Err(ChunkError::Invalid("corrupt Cosmos payload chunk"));
        }
        match chunk {
            Chunk::Children(children) => {
                if children.is_empty() || children.len() > fanout {
                    return Err(ChunkError::Invalid("invalid Cosmos payload reference page"));
                }
                ancestors.insert(identity.clone());
                pending.push(Visit::Exit(identity));
                pending.extend(children.into_iter().rev().map(Visit::Enter));
            }
            Chunk::Data(data) => {
                let part = STANDARD
                    .decode(data)
                    .map_err(|_| ChunkError::Invalid("invalid Cosmos payload base64"))?;
                raw.extend(part);
            }
        }
    }
    if raw.len() != manifest.bytes || digest(&raw) != manifest.sha256 {
        return Err(ChunkError::Invalid("Cosmos payload digest mismatch"));
    }
    Ok(raw)
}
