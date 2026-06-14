//! BitcoinPIR-shaped Path ORAM prototype.
//!
//! This crate intentionally implements an oblivious *array*, not a generic
//! oblivious map. BitcoinPIR already owns the public mapping from scripthash to
//! PBC/cuckoo positions; this layer hides which logical block the SEV guest
//! reads from disk-backed storage.

pub mod aead;
pub mod block;
pub mod circuit;
pub mod ct;
pub mod cuckoo;
pub mod error;
pub mod oram;
pub mod params;
pub mod state;
pub mod store;
pub mod stress;

pub use aead::{AeadPageStore, AEAD_OVERHEAD};
pub use block::{Bucket, OramBlock};
pub use circuit::{
    circuit_meta_page_bytes, circuit_payload_page_bytes, CircuitEvictionSchedule, CircuitMetaSlot,
    CircuitOram,
};
pub use cuckoo::{CuckooOramEstimate, CuckooOramSizing, CuckooTableInfo};
pub use error::{Error, Result};
pub use oram::PathOram;
pub use params::OramParams;
pub use state::OramState;
pub use store::{FilePageStore, FrontCachedPageStore, MemPageStore, PageStore, TracingStore};
pub use stress::{stress_circuit, CircuitStressConfig, CircuitStressPattern, CircuitStressReport};
