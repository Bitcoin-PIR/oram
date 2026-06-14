//! Circuit ORAM design helpers.
//!
//! This module contains public scheduling state shared by the planned
//! disk-backed Circuit ORAM controller and the stash-pressure simulator. The
//! schedule is intentionally independent of logical addresses and stash
//! occupancy: real accesses add a fixed amount of public eviction debt, and
//! background work drains that debt in reverse-bit order.

use crate::{Error, OramParams, Result};
use serde::{Deserialize, Serialize};

/// Public eviction schedule for deterministic Circuit ORAM.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CircuitEvictionSchedule {
    leaf_bits: u32,
    evictions_per_access: u64,
    issued_accesses: u64,
    completed_evictions: u64,
}

impl CircuitEvictionSchedule {
    /// Circuit ORAM's deterministic variant uses two eviction paths per access.
    pub const DEFAULT_EVICTIONS_PER_ACCESS: u64 = 2;

    /// Construct a schedule for the leaf count in `params`.
    pub fn new(params: &OramParams) -> Self {
        Self::with_rate(params, Self::DEFAULT_EVICTIONS_PER_ACCESS)
    }

    /// Construct a schedule with an explicit public eviction rate.
    pub fn with_rate(params: &OramParams, evictions_per_access: u64) -> Self {
        debug_assert!(params.leaves.is_power_of_two());
        debug_assert!(params.leaves <= u32::MAX as usize);
        Self {
            leaf_bits: params.leaf_bits() as u32,
            evictions_per_access,
            issued_accesses: 0,
            completed_evictions: 0,
        }
    }

    /// Reconstruct a schedule from checkpointed public counters.
    pub fn from_counters(
        params: &OramParams,
        evictions_per_access: u64,
        issued_accesses: u64,
        completed_evictions: u64,
    ) -> Result<Self> {
        let schedule = Self {
            leaf_bits: params.leaf_bits() as u32,
            evictions_per_access,
            issued_accesses,
            completed_evictions,
        };
        schedule.pending_evictions()?;
        Ok(schedule)
    }

    /// Record one real ORAM access, adding public eviction debt.
    pub fn record_access(&mut self) -> Result<()> {
        self.issued_accesses = self
            .issued_accesses
            .checked_add(1)
            .ok_or_else(|| Error::InvalidInput("access counter overflow".into()))?;
        Ok(())
    }

    /// Number of completed real ORAM accesses.
    pub const fn issued_accesses(&self) -> u64 {
        self.issued_accesses
    }

    /// Number of completed background eviction paths.
    pub const fn completed_evictions(&self) -> u64 {
        self.completed_evictions
    }

    /// Public number of eviction paths scheduled per real access.
    pub const fn evictions_per_access(&self) -> u64 {
        self.evictions_per_access
    }

    /// Public eviction paths that have been scheduled but not yet processed.
    pub fn pending_evictions(&self) -> Result<u64> {
        let scheduled = self
            .issued_accesses
            .checked_mul(self.evictions_per_access)
            .ok_or_else(|| Error::InvalidInput("eviction counter overflow".into()))?;
        scheduled
            .checked_sub(self.completed_evictions)
            .ok_or_else(|| Error::InvalidInput("completed evictions exceed schedule".into()))
    }

    /// Leaf for the next deterministic eviction path, even if no debt is due.
    pub fn next_eviction_leaf(&self) -> u32 {
        reverse_bits_mod(self.completed_evictions, self.leaf_bits)
    }

    /// Drain one pending eviction path if any debt exists.
    pub fn complete_one_eviction(&mut self) -> Result<Option<u32>> {
        if self.pending_evictions()? == 0 {
            return Ok(None);
        }
        let leaf = self.next_eviction_leaf();
        self.completed_evictions = self
            .completed_evictions
            .checked_add(1)
            .ok_or_else(|| Error::InvalidInput("eviction counter overflow".into()))?;
        Ok(Some(leaf))
    }

    /// Drain up to `budget` public eviction paths.
    pub fn drain_evictions(&mut self, budget: u64) -> Result<Vec<u32>> {
        let mut leaves = Vec::new();
        for _ in 0..budget {
            match self.complete_one_eviction()? {
                Some(leaf) => leaves.push(leaf),
                None => break,
            }
        }
        Ok(leaves)
    }

    /// Leaf selected by the global deterministic eviction index.
    pub fn eviction_leaf_at(params: &OramParams, eviction_index: u64) -> u32 {
        reverse_bits_mod(eviction_index, params.leaf_bits() as u32)
    }
}

fn reverse_bits_mod(index: u64, leaf_bits: u32) -> u32 {
    debug_assert!(leaf_bits > 0);
    debug_assert!(leaf_bits <= 32);
    let mask = if leaf_bits == 32 {
        u32::MAX
    } else {
        (1u32 << leaf_bits) - 1
    };
    let low = (index as u32) & mask;
    low.reverse_bits() >> (32 - leaf_bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(leaves: usize) -> OramParams {
        OramParams::with_leaves(16, 32, leaves)
            .unwrap()
            .with_bucket_size(2)
            .unwrap()
            .with_stash_capacity(128)
            .unwrap()
    }

    #[test]
    fn deterministic_eviction_sequence_is_reverse_bit_order() {
        let params = params(8);
        let leaves = (0..8)
            .map(|i| CircuitEvictionSchedule::eviction_leaf_at(&params, i))
            .collect::<Vec<_>>();

        assert_eq!(leaves, vec![0, 4, 2, 6, 1, 5, 3, 7]);
    }

    #[test]
    fn each_access_adds_two_public_eviction_paths() {
        let params = params(8);
        let mut schedule = CircuitEvictionSchedule::new(&params);

        schedule.record_access().unwrap();
        schedule.record_access().unwrap();

        assert_eq!(schedule.issued_accesses(), 2);
        assert_eq!(schedule.pending_evictions().unwrap(), 4);
        assert_eq!(schedule.drain_evictions(3).unwrap(), vec![0, 4, 2]);
        assert_eq!(schedule.completed_evictions(), 3);
        assert_eq!(schedule.pending_evictions().unwrap(), 1);
        assert_eq!(schedule.drain_evictions(10).unwrap(), vec![6]);
        assert_eq!(schedule.pending_evictions().unwrap(), 0);
    }

    #[test]
    fn delayed_eviction_preserves_public_order() {
        let params = params(8);
        let mut immediate = CircuitEvictionSchedule::new(&params);
        let mut delayed = CircuitEvictionSchedule::new(&params);

        let mut immediate_leaves = Vec::new();
        for _ in 0..4 {
            immediate.record_access().unwrap();
            immediate_leaves.extend(immediate.drain_evictions(2).unwrap());
        }

        for _ in 0..4 {
            delayed.record_access().unwrap();
        }
        let delayed_leaves = delayed.drain_evictions(8).unwrap();

        assert_eq!(immediate_leaves, delayed_leaves);
    }

    #[test]
    fn checkpointed_counters_resume_debt() {
        let params = params(8);
        let mut schedule = CircuitEvictionSchedule::from_counters(&params, 2, 5, 7).unwrap();

        assert_eq!(schedule.evictions_per_access(), 2);
        assert_eq!(schedule.pending_evictions().unwrap(), 3);
        assert_eq!(schedule.drain_evictions(3).unwrap(), vec![7, 0, 4]);
        assert_eq!(schedule.pending_evictions().unwrap(), 0);
    }

    #[test]
    fn invalid_checkpoint_rejects_overcompleted_evictions() {
        let params = params(8);
        let err = CircuitEvictionSchedule::from_counters(&params, 2, 5, 11).unwrap_err();

        assert!(matches!(err, Error::InvalidInput(_)));
    }
}
