# Direct-entry ORAM batching plan

## Motivation

The current deployed ORAM image wraps the DPF/Harmony `INDEX + CHUNK` PBC
cuckoo tables. That preserves compatibility, but it also preserves the PBC
storage expansion. For a TEE-backed ORAM lookup, the server can execute the
key lookup internally and return results over the encrypted channel, so the
PBC group expansion is no longer necessary.

The next ORAM format should be built from the direct intermediate records:

- `utxo_chunks_index_nodust.bin`: 25-byte records
  `[20B script_hash][4B start_chunk_id][1B num_chunks]`.
- `utxo_chunks_nodust.bin`: direct 40-byte chunk payloads addressed by
  `chunk_id`.

The delta pipeline uses the same direct index/chunk record shape.

## Position-map scan benchmark

`oramctl bench-pos-map` measures three trusted position-map access shapes:

- `direct`: ordinary indexed load.
- `scan`: branchless full scan to select one leaf.
- `scan_update`: branchless full scan that selects the old leaf and writes a
  new leaf with a constant-shape store to every entry.

VPSBG results, `ops=200`, `warmup_ops=20`:

| Position-map entries | Map size | Scan | Scan + update |
| ---: | ---: | ---: | ---: |
| 249,760 | 0.95 MiB | 52.5 us | 84.1 us |
| 561,660 | 2.14 MiB | 117.5 us | 189.1 us |
| 2,660,429 | 10.15 MiB | 556.4 us | 894.8 us |
| 5,334,640 | 20.35 MiB | 1.11 ms | 1.80 ms |

Estimated direct-entry packed sizes are similar to today's largest maps if we
keep `pack=16`:

| Estimated direct table | Packed ORAM blocks | Map size | Scan | Scan + update |
| --- | ---: | ---: | ---: | ---: |
| FULL direct index | ~3.37M | 12.86 MiB | 615.1 us | 1.15 ms |
| FULL direct chunks | ~5.07M | 19.34 MiB | 1.02 ms | 1.71 ms |

Conclusion: full-scan position-map access is acceptable for the current packed
block counts. It is not acceptable if we make every 40-byte chunk its own ORAM
logical block and grow the position map by 16x.

## Direct-entry layout

Keep separate direct ORAM tables:

1. `direct_index`
   - Payload slot: collision-free
     `[1B occupied][20B script_hash][4B start_chunk_id][1B num_chunks]`.
   - Lookup structure: non-PBC cuckoo table over all script hashes, with a small
     fixed number of candidate bins.
   - ORAM payload block packs consecutive direct index bins, for example
     `pack=16`.

2. `direct_chunk`
   - Payload record: direct 40-byte chunk payload.
   - Logical key: `chunk_id`.
   - ORAM payload block packs consecutive chunk records, for example `pack=16`.

The index table may still use cuckoo hashing, but only as a direct dictionary.
It should not be split into 75 PBC groups, and each entry should be stored once
rather than once per candidate PBC group.

## Batched request shape

Add a native ORAM batch endpoint with a fixed public access budget:

```text
OramDirectBatchRequest {
    db_id,
    script_hashes[],
    access_budget = 50,
}
```

The server should execute a fixed schedule:

1. For each script hash, perform a fixed number of direct-index ORAM reads
   against candidate direct-index bins.
2. Decode matches inside the TEE.
3. Build a private list of required chunk IDs.
4. Execute up to the remaining fixed chunk-read budget.
5. Fill unused access slots with deterministic dummy reads.
6. If real chunks exceed the budget, return a continuation token/state so the
   client can request another fixed-budget batch.

For a single-address lookup with two direct-index candidates, a 50-access budget
gives up to 48 chunk reads. For a multi-address batch, the budget should be
allocated as:

```text
index_reads = direct_index_candidates * script_hashes.len()
chunk_budget = access_budget - index_reads
```

The request should reject or split batches when `index_reads > access_budget`.

## Engineering sequence

1. Add direct table builders in the standalone ORAM repo:
   - parse direct index/chunk intermediate files;
   - emit `direct-index.*` and `direct-chunk.*` ORAM images plus metadata;
   - keep the same tiered Merkle/auth-state machinery.
2. Add an `oramctl size-direct` estimator before building images. (Done.)
3. Add an `oramctl build-direct` command for FULL and DELTA. (Done for the
   standalone ORAM image format.)
4. Add a runtime direct-ORAM reader beside the current cuckoo reader.
5. Add a native direct batch request/response type in `pir-sdk-client` and
   `unified_server`.
6. Benchmark fixed budgets: 16, 32, 50, 64 ORAM accesses.
7. Once direct lookup is verified, make PBC-cuckoo ORAM a compatibility mode
   rather than the primary ORAM backend.
