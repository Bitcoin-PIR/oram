use crate::{OramBlock, OramParams, Result};
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter},
    path::Path,
};

const STATE_MAGIC: &[u8; 8] = b"BPORAM01";

/// Trusted controller state needed to reopen an ORAM bucket image.
///
/// This contains the position map, stash, and RNG state. Treat it as TEE
/// private state; if written outside the TEE it must be encrypted or sealed.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OramState {
    magic: [u8; 8],
    /// Public ORAM sizing parameters.
    pub params: OramParams,
    /// Current logical-id to leaf-label position map.
    pub pos_map: Vec<u32>,
    /// Current stash contents.
    pub stash: Vec<OramBlock>,
    /// Current RNG state used for future remapping.
    pub rng: ChaCha20Rng,
}

impl OramState {
    /// Construct a state object.
    pub fn new(
        params: OramParams,
        pos_map: Vec<u32>,
        stash: Vec<OramBlock>,
        rng: ChaCha20Rng,
    ) -> Self {
        Self {
            magic: *STATE_MAGIC,
            params,
            pos_map,
            stash,
            rng,
        }
    }

    /// Persist state atomically via temp-file-and-rename.
    pub fn save_atomic(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let tmp = path.with_extension(format!(
            "{}tmp",
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| format!("{ext}."))
                .unwrap_or_default()
        ));
        {
            let file = File::create(&tmp)?;
            let writer = BufWriter::new(file);
            bincode::serialize_into(writer, self)?;
        }
        fs::rename(tmp, path)?;
        Ok(())
    }

    /// Load a state file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let state: Self = bincode::deserialize_from(reader)?;
        if &state.magic != STATE_MAGIC {
            return Err(crate::Error::InvalidInput(
                "invalid ORAM state magic".into(),
            ));
        }
        Ok(state)
    }
}
