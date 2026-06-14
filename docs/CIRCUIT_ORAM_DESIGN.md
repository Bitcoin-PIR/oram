# Circuit ORAM Design

This document fixes the next ORAM backend direction for BitcoinPIR: a
disk-backed Circuit ORAM controller tuned for the existing DPF/Harmony cuckoo
tables.

The current `PathOram` implementation remains the correctness baseline. The
Circuit ORAM work should reuse its page-store, AEAD page, fixed stash, and
cuckoo sizing code, but change the tree maintenance algorithm and storage
layout.

## Goals

- Hide the logical cuckoo block touched by a SEV-SNP guest.
- Target `Z=2` buckets, because Circuit ORAM's deterministic eviction supports
  much lower bucket capacity than vanilla Path ORAM in practice.
- Keep the ORAM tree outside trusted memory; trusted state should be position
  map, fixed-capacity stash, scheduler state, and a small public top cache.
- Preserve BitcoinPIR's existing public KV mapping and PBC batching. ORAM is an
  oblivious array over packed cuckoo bins, not a generic map.
- Support delayed/background eviction without secret-dependent timing.

## Non-goals

- OnionPIR database support.
- Recursive position map in the first implementation.
- Data-dependent stash-drain heuristics.
- A generic oblivious map API.

## Parameters

Initial candidate for the production-ish DPF/Harmony tables:

```text
pack = 16
bucket_size Z = 2
leaf_divisor = 4
evictions_per_access = 2
stash_capacity = stress-test result, not a fixed assumption yet
```

This keeps the tree slot load around 63 percent for the current checkpoint
sizes, similar to the aggressive `Z=4, leaf_divisor=8` Path ORAM estimate but
with a design that is meant to operate at `Z=2`.

Reject configurations whose tree slot load exceeds 100 percent. They may look
small in a simple sizing pass but cannot hold all logical blocks.

## Logical Block Layout

The ORAM logical address is a packed cuckoo-bin block:

```text
logical_block_id = table-local packed bin id
payload = pack consecutive cuckoo bins
```

For `pack=16`:

```text
INDEX payload = 16 * 52 B = 832 B
CHUNK payload = 16 * 132 B = 2112 B
```

INDEX and CHUNK should remain separate ORAM instances. Snapshot and delta can
also remain separate instances initially, matching the current DPF/Harmony DB
directory boundary.

## Storage Layout

Circuit ORAM's benefit depends on making metadata scans cheap. Do not store the
only copy of metadata inside the payload page if that forces metadata-only
eviction planning to read full payload buckets.

Use two physical stores per ORAM instance:

```text
metadata store:
  per bucket slot:
    occupied bit
    logical id
    leaf label
    dummy/freshness metadata needed by the controller

payload store:
  per bucket slot:
    encrypted packed cuckoo payload
```

Both stores must be accessed in a fixed public shape for the selected operation.
Metadata pages can be much smaller than payload pages, so Circuit ORAM's
`deepest` and `target` scans do not pay the full payload I/O cost.

## Access State Machine

Each real read has an online phase and an eviction phase.

Online phase:

```text
old_leaf = position_map[logical_block_id]
read and remove matching block from old_leaf path into stash
full-scan stash to select the requested block
position_map[logical_block_id] = random_leaf()
stash updated block with new leaf
record one issued access
return payload
```

Eviction phase:

```text
for each due public eviction path:
  leaf = bit_reverse(completed_evictions mod leaves)
  metadata scan 1: compute deepest candidate movement
  metadata scan 2: compute target slot decisions
  payload scan: move at most one block per level according to target decisions
  completed_evictions += 1
```

The deterministic Circuit ORAM schedule adds two eviction paths per real
access:

```text
access t schedules paths bit_reverse(2t) and bit_reverse(2t + 1)
```

The implementation should store only public counters:

```text
issued_accesses
completed_evictions
```

Pending eviction debt is derived:

```text
pending = issued_accesses * 2 - completed_evictions
```

No secret-dependent queue is needed.

## Deferred Eviction Policy

Eviction may be delayed only if the delay policy is public. Acceptable policies:

- Drain exactly `D` eviction paths after each real access.
- Drain up to a fixed public budget on a fixed public timer.
- Bound public debt with `max_eviction_debt`, where request admission waits
  when the public debt counter reaches the cap.

Avoid policies whose external timing depends on stash length, hit/miss behavior,
UTXO count, or logical address. A stash high-watermark can be useful for local
debugging, but it is not a production scheduling signal unless hidden behind a
fixed-shape public schedule.

## Stash Risk

Circuit ORAM's published proof covers deterministic eviction with `Z >= 4`.
The `Z=2` choice is supported by empirical results in the paper and by later
TEE-oriented systems, not by the same clean theorem. For BitcoinPIR this means
`Z=2` is a candidate that must pass workload-specific stress testing.

The `oramctl stress-circuit` tool now simulates:

```text
random query sequence
round-robin worst-case sequence
bursty deferred eviction with max debt Q
snapshot and delta tables separately
INDEX and CHUNK tables separately
pack=16, Z=2, leaf_divisor=4
```

Required output:

```text
max stash occupancy
p99/p999 stash occupancy
overflow count for configured capacity
eviction debt distribution
metadata and payload page I/O per logical access
```

The simulator is metadata-only. It tracks logical ids, leaf labels, bucket
slots, stash occupancy, and public eviction debt, and it models Circuit ORAM's
deterministic eviction schedule with greedy path eviction. This is the right
tool for parameter exploration, but it is not a replacement for implementing
the real `deepest`/`target` metadata scans and auditing the final page trace.

## Crash Consistency

The trusted state checkpoint must include:

```text
position map
stash slots
RNG state
issued_accesses
completed_evictions
metadata store root/version
payload store root/version
```

If online reads can return before queued evictions are flushed, the checkpoint
must be able to replay or resume the exact public eviction debt. Production
should use a small WAL or epoch checkpoint protocol before allowing async
eviction to cross a durable boundary.

## Implementation Milestones

1. Add deterministic Circuit ORAM scheduler and tests. Done.
2. Add `oramctl stress-circuit` simulator over real cuckoo table sizes. Done.
3. Split metadata/payload page models in the simulator and sizing output.
4. Implement a `CircuitOram` controller using the existing `PageStore` traits.
5. Add encrypted metadata and payload stores with fixed-shape trace tests.
6. Add a build path from existing DPF/Harmony cuckoo tables into ORAM images.
7. Run release assembly and SEV-SNP page-trace audit on the hot loops.

## Current Design Choice

Proceed with Circuit ORAM, not full Ring ORAM, for the first BitcoinPIR ORAM
backend.

Ring ORAM also separates reads and background eviction, and it has an explicit
public `A` parameter for one eviction after every `A` reads. However, it needs
extra dummy slots, per-bucket read counters, and EarlyReshuffle machinery. Those
features optimize online bandwidth, but they add substantial controller
complexity. Circuit ORAM directly targets the main pressure point for the
current servers: lower storage with `Z=2`.
