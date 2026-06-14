# BitcoinPIR ORAM

BitcoinPIR-shaped disk-backed ORAM prototype.

This repository is intentionally **not** a generic oblivious map. BitcoinPIR
already owns the public mapping from `scripthash` to PBC/cuckoo positions; this
crate provides an oblivious array layer that hides which logical block a TEE
accesses.

## Current Scope

Implemented:

- Path ORAM controller with in-TEE position map and stash.
- Fixed-capacity stash slots with full-slot scans for path read/write.
- Trusted, non-oblivious bulk initialization for offline image creation.
- `MemPageStore` for tests.
- `FilePageStore` for NVMe/page-file backed storage.
- `AeadPageStore` wrapper using ChaCha20-Poly1305 per page.
- Trusted controller-state checkpoint/reopen (`OramState`), with optional
  ChaCha20-Poly1305 state-file encryption.
- Fixed-prefix front cache (`FrontCachedPageStore`) for keeping public top ORAM
  tree levels in trusted memory.
- Mask-based CMOV-style helpers for stash lookup, stash insert, and path
  eviction selection.
- `oramctl` CLI for building deterministic test images and running random-read
  benchmarks.
- `oramctl size-cuckoo` for estimating ORAM images over existing DPF/Harmony
  cuckoo tables.
- Fixed trace-shape tests: each logical access reads and rewrites a complete
  root-to-leaf path.
- Circuit ORAM deterministic eviction scheduler and design notes.
- `oramctl stress-circuit` metadata-only stash-pressure simulator for the
  planned Circuit ORAM controller.

Intentionally not implemented yet:

- Circuit ORAM controller.
- Recursive position map.
- Oblivious bulk initialization.
- Crash-safe checkpointing or WAL.
- Release assembly / SEV-SNP ciphertext-channel audit of the constant-shape hot
  loops.
- Multi-client sharding.

## Design

The runtime shape is:

```text
trusted memory:
  position map: logical_id -> current random leaf
  stash
  ORAM controller

disk / untrusted storage:
  encrypted bucket pages
```

Each access:

1. Reads every bucket on the old random root-to-leaf path.
2. Moves real blocks into the stash.
3. Assigns the target logical block a fresh random leaf.
4. Rewrites every bucket on the same path from the stash.

The backing store sees random ORAM paths, not BitcoinPIR logical ids.

The planned production direction is Circuit ORAM with deterministic delayed
eviction, `Z=2`, and packed DPF/Harmony cuckoo bins. See
[`docs/CIRCUIT_ORAM_DESIGN.md`](docs/CIRCUIT_ORAM_DESIGN.md).

## Build

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

## CLI Smoke Test

Build an encrypted test image:

```bash
KEY_HEX=4242424242424242424242424242424242424242424242424242424242424242
STATE_KEY_HEX=7373737373737373737373737373737373737373737373737373737373737373

cargo run --bin oramctl -- build \
  --image /tmp/bpir-oram.pages \
  --state /tmp/bpir-oram.state \
  --state-key-hex "$STATE_KEY_HEX" \
  --blocks 1024 \
  --block-size 64 \
  --encrypted \
  --key-hex "$KEY_HEX" \
  --cache-levels 4
```

Run random reads against it:

```bash
cargo run --bin oramctl -- bench \
  --image /tmp/bpir-oram.pages \
  --state /tmp/bpir-oram.state \
  --state-key-hex "$STATE_KEY_HEX" \
  --ops 1000 \
  --encrypted \
  --key-hex "$KEY_HEX" \
  --cache-levels 4
```

`--cache-levels 4` caches `(1 << 4) - 1 = 15` public top-tree pages in trusted
memory. Use `0` to disable this wrapper.

## Cuckoo Table Sizing

Estimate ORAM images for the DPF/Harmony cuckoo tables in one or more existing
BitcoinPIR DB directories:

```bash
cargo run --bin oramctl -- size-cuckoo \
  --db-dir /Volumes/Bitcoin/data/checkpoints/948454 \
  --db-dir /Volumes/Bitcoin/data/deltas/940611_948454 \
  --packs 4,8,16 \
  --leaf-divisors 1,2,4,8 \
  --cache-levels 5
```

`pack` is the number of consecutive cuckoo bins stored in one logical ORAM
block. INDEX bins are 52 B and CHUNK bins are 132 B, so `pack=8` uses 416 B
INDEX blocks and 1056 B CHUNK blocks. `leaf_divisor` controls tree density:
`leaves = next_power_of_two(ceil(logical_blocks / leaf_divisor))`. Higher values
reduce disk size but increase stash pressure and must be stress-tested before
production use.

## Circuit ORAM Stress Simulation

Run a metadata-only Circuit ORAM stash-pressure simulation over DPF/Harmony
cuckoo table sizes:

```bash
cargo run --bin oramctl -- stress-circuit \
  --db-dir /Volumes/Bitcoin/data/checkpoints/940611 \
  --packs 16 \
  --leaf-divisors 4 \
  --bucket-size 2 \
  --stash-capacity 4096 \
  --ops 100000 \
  --warmup-ops 10000 \
  --pattern random \
  --drain-per-access 2
```

To model public delayed eviction, reduce `--drain-per-access` and set a public
debt cap:

```bash
cargo run --bin oramctl -- stress-circuit \
  --db-dir /Volumes/Bitcoin/data/checkpoints/940611 \
  --drain-per-access 0 \
  --max-debt 128 \
  --ops 100000
```

The simulator stores only logical block ids, leaf labels, tree slots, and stash
entries. It uses greedy path eviction as a stress model for Circuit ORAM's
deterministic eviction schedule. It is useful for choosing `Z`, stash capacity,
tree density, and public eviction-debt bounds; it is not a proof and it does
not replace the controller trace audit.

## Prototype Warning

This is a correctness and storage-shape prototype. Before production use inside
SEV-SNP, the hot loops still need release assembly and trace inspection on the
target build. The stash is fixed-capacity, and online stash lookup, stash insert,
and path eviction selection now use full-slot scans plus mask-based CMOV-style
helpers. That is an implementation hardening step, not a formal constant-time
guarantee from Rust or LLVM.

The `.state` file contains the position map, stash, and RNG state. It is trusted
controller state. Do not write it to untrusted storage in plaintext in a real
deployment. Use `--state-key-hex` for prototype AEAD protection; production
should replace that key path with SEV-sealed storage.
