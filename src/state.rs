use crate::{Error, OramBlock, OramParams, Result};
use chacha20poly1305::{
    aead::{Aead, KeyInit, OsRng},
    ChaCha20Poly1305, Nonce,
};
use rand::RngCore;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Write},
    path::Path,
};
use zeroize::Zeroize;

const STATE_MAGIC: &[u8; 8] = b"BPORAM01";
const SEALED_STATE_MAGIC: &[u8; 8] = b"BPORAMS1";
const STATE_NONCE_LEN: usize = 12;

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
        let tmp = tmp_path(path);
        {
            let file = File::create(&tmp)?;
            let mut writer = BufWriter::new(file);
            bincode::serialize_into(&mut writer, self)?;
            writer.flush()?;
        }
        fs::rename(tmp, path)?;
        Ok(())
    }

    /// Persist an AEAD-encrypted state file atomically.
    pub fn save_encrypted_atomic(&self, path: impl AsRef<Path>, key: [u8; 32]) -> Result<()> {
        let mut plaintext = bincode::serialize(self)?;
        let mut nonce_bytes = [0u8; STATE_NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);

        let cipher = ChaCha20Poly1305::new((&key).into());
        let mut ciphertext = cipher.encrypt(
            Nonce::from_slice(&nonce_bytes),
            chacha20poly1305::aead::Payload {
                msg: &plaintext,
                aad: SEALED_STATE_MAGIC,
            },
        )?;
        plaintext.zeroize();

        let mut envelope =
            Vec::with_capacity(SEALED_STATE_MAGIC.len() + nonce_bytes.len() + ciphertext.len());
        envelope.extend_from_slice(SEALED_STATE_MAGIC);
        envelope.extend_from_slice(&nonce_bytes);
        envelope.extend_from_slice(&ciphertext);
        ciphertext.zeroize();

        let path = path.as_ref();
        let tmp = tmp_path(path);
        let mut writer = BufWriter::new(File::create(&tmp)?);
        writer.write_all(&envelope)?;
        writer.flush()?;
        drop(writer);
        envelope.zeroize();
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

    /// Load an AEAD-encrypted state file.
    pub fn load_encrypted(path: impl AsRef<Path>, key: [u8; 32]) -> Result<Self> {
        let mut envelope = fs::read(path)?;
        if envelope.len() < SEALED_STATE_MAGIC.len() + STATE_NONCE_LEN + 16 {
            return Err(Error::InvalidInput(
                "encrypted state file is too short".into(),
            ));
        }
        if &envelope[..SEALED_STATE_MAGIC.len()] != SEALED_STATE_MAGIC {
            return Err(Error::InvalidInput(
                "invalid encrypted ORAM state magic".into(),
            ));
        }

        let nonce_start = SEALED_STATE_MAGIC.len();
        let ciphertext_start = nonce_start + STATE_NONCE_LEN;
        let mut nonce = [0u8; STATE_NONCE_LEN];
        nonce.copy_from_slice(&envelope[nonce_start..ciphertext_start]);

        let cipher = ChaCha20Poly1305::new((&key).into());
        let mut plaintext = cipher.decrypt(
            Nonce::from_slice(&nonce),
            chacha20poly1305::aead::Payload {
                msg: &envelope[ciphertext_start..],
                aad: SEALED_STATE_MAGIC,
            },
        )?;
        envelope.zeroize();

        let state: Self = bincode::deserialize(&plaintext)?;
        plaintext.zeroize();
        if &state.magic != STATE_MAGIC {
            return Err(Error::InvalidInput("invalid ORAM state magic".into()));
        }
        Ok(state)
    }
}

fn tmp_path(path: &Path) -> std::path::PathBuf {
    path.with_extension(format!(
        "{}tmp",
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| format!("{ext}."))
            .unwrap_or_default()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OramParams;
    use rand::SeedableRng;

    fn sample_state() -> OramState {
        OramState::new(
            OramParams::with_leaves(4, 8, 4).unwrap(),
            vec![0, 1, 2, 3],
            Vec::new(),
            ChaCha20Rng::from_seed([3; 32]),
        )
    }

    #[test]
    fn encrypted_state_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("controller.state");
        let state = sample_state();
        state.save_encrypted_atomic(&path, [9; 32]).unwrap();

        let loaded = OramState::load_encrypted(&path, [9; 32]).unwrap();
        assert_eq!(loaded.params, state.params);
        assert_eq!(loaded.pos_map, state.pos_map);
    }

    #[test]
    fn encrypted_state_rejects_wrong_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("controller.state");
        sample_state()
            .save_encrypted_atomic(&path, [9; 32])
            .unwrap();

        assert!(OramState::load_encrypted(&path, [8; 32]).is_err());
    }
}
