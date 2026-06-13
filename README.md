# BitcoinPIR ORAM

BitcoinPIR-shaped disk-backed Path ORAM prototype.

This repository is intentionally **not** a generic oblivious map. BitcoinPIR
already owns the public mapping from `scripthash` to PBC/cuckoo positions; this
crate provides an oblivious array layer that hides which logical block a TEE
accesses.

## Current Scope

Implemented:

- Path ORAM controller with in-TEE position map and stash.
- Trusted, non-oblivious bulk initialization for offline image creation.
- `MemPageStore` for tests.
- `FilePageStore` for NVMe/page-file backed storage.
- `AeadPageStore` wrapper using ChaCha20-Poly1305 per page.
- Trusted controller-state checkpoint/reopen (`OramState`).
- `oramctl` CLI for building deterministic test images and running random-read
  benchmarks.
- Fixed trace-shape tests: each logical access reads and rewrites a complete
  root-to-leaf path.

Intentionally not implemented yet:

- Recursive position map.
- Oblivious bulk initialization.
- Crash-safe checkpointing or WAL.
- Constant-time / CMOV hardening of the stash and eviction loops.
- SEV-SNP ciphertext-channel audit.
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

## Build

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

## CLI Smoke Test

Build an encrypted test image:

```bash
KEY_HEX=4242424242424242424242424242424242424242424242424242424242424242

cargo run --bin oramctl -- build \
  --image /tmp/bpir-oram.pages \
  --state /tmp/bpir-oram.state \
  --blocks 1024 \
  --block-size 64 \
  --encrypted \
  --key-hex "$KEY_HEX"
```

Run random reads against it:

```bash
cargo run --bin oramctl -- bench \
  --image /tmp/bpir-oram.pages \
  --state /tmp/bpir-oram.state \
  --ops 1000 \
  --encrypted \
  --key-hex "$KEY_HEX"
```

## Prototype Warning

This is a correctness and storage-shape prototype. Before production use inside
SEV-SNP, the hot loops need explicit constant-time hardening and assembly/trace
inspection. In particular, `find_flushable` currently scans the whole stash but
still uses ordinary Rust branch/`Option` selection.

The `.state` file contains the position map, stash, and RNG state. It is trusted
controller state. Do not write it to untrusted storage in plaintext in a real
deployment; it needs SEV-sealed storage or an AEAD wrapper before production use.
