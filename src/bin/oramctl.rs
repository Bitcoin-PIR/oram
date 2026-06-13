use bitcoinpir_oram::{
    AeadPageStore, Error, FilePageStore, FrontCachedPageStore, OramParams, OramState, PageStore,
    PathOram, Result, AEAD_OVERHEAD,
};
use clap::{Parser, Subcommand};
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Debug, Parser)]
#[command(about = "BitcoinPIR Path ORAM prototype utility")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build a trusted ORAM image containing deterministic test payloads.
    Build {
        /// ORAM bucket image path.
        #[arg(long)]
        image: PathBuf,
        /// Trusted controller-state output path.
        #[arg(long)]
        state: PathBuf,
        /// 32-byte hex state encryption key. If omitted, state is written in plaintext.
        #[arg(long)]
        state_key_hex: Option<String>,
        /// Number of logical blocks.
        #[arg(long)]
        blocks: usize,
        /// Payload bytes per logical block.
        #[arg(long)]
        block_size: usize,
        /// Optional explicit leaf count. Must be a power of two.
        #[arg(long)]
        leaves: Option<usize>,
        /// Physical blocks per bucket.
        #[arg(long, default_value_t = 4)]
        bucket_size: usize,
        /// Stash capacity.
        #[arg(long, default_value_t = 512)]
        stash_capacity: usize,
        /// Enable page AEAD.
        #[arg(long)]
        encrypted: bool,
        /// 32-byte hex page encryption key. Required with --encrypted.
        #[arg(long)]
        key_hex: Option<String>,
        /// Cache this many public top ORAM tree levels in trusted memory.
        #[arg(long, default_value_t = 0)]
        cache_levels: usize,
        /// 32-byte hex RNG seed. Defaults to all 0x07 for reproducible tests.
        #[arg(long)]
        seed_hex: Option<String>,
    },
    /// Run random reads against an existing image and update the state file.
    Bench {
        /// ORAM bucket image path.
        #[arg(long)]
        image: PathBuf,
        /// Trusted controller-state path.
        #[arg(long)]
        state: PathBuf,
        /// 32-byte hex state encryption key. Required if the state was encrypted.
        #[arg(long)]
        state_key_hex: Option<String>,
        /// Number of random reads.
        #[arg(long, default_value_t = 1000)]
        ops: usize,
        /// Enable page AEAD.
        #[arg(long)]
        encrypted: bool,
        /// 32-byte hex page encryption key. Required with --encrypted.
        #[arg(long)]
        key_hex: Option<String>,
        /// Cache this many public top ORAM tree levels in trusted memory.
        #[arg(long, default_value_t = 0)]
        cache_levels: usize,
        /// 32-byte hex query RNG seed. Defaults to all 0x03.
        #[arg(long)]
        query_seed_hex: Option<String>,
        /// Do not write back the updated trusted state.
        #[arg(long)]
        no_save: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Build {
            image,
            state,
            state_key_hex,
            blocks,
            block_size,
            leaves,
            bucket_size,
            stash_capacity,
            encrypted,
            key_hex,
            cache_levels,
            seed_hex,
        } => {
            if block_size < 8 {
                return Err(Error::InvalidInput(
                    "block-size must be at least 8 for deterministic CLI payloads".into(),
                ));
            }
            let params = match leaves {
                Some(leaves) => OramParams::with_leaves(blocks, block_size, leaves)?,
                None => OramParams::new(blocks, block_size)?,
            }
            .with_bucket_size(bucket_size)?
            .with_stash_capacity(stash_capacity)?;
            let seed = parse_seed(seed_hex.as_deref(), 0x07)?;
            let cached_pages = cached_pages_for_levels(&params, cache_levels)?;
            let store = open_file_store(
                &image,
                &params,
                encrypted,
                key_hex.as_deref(),
                cached_pages,
                false,
            )?;
            let payloads = deterministic_payloads(blocks, block_size);

            let started = Instant::now();
            let mut oram = PathOram::build_trusted(params.clone(), store, payloads, seed)?;
            oram.flush()?;
            save_state(&oram.snapshot(), &state, state_key_hex.as_deref())?;
            let elapsed = started.elapsed();

            println!("built=true");
            println!("image={}", image.display());
            println!("state={}", state.display());
            println!("logical_blocks={}", params.logical_blocks);
            println!("block_size={}", params.block_size);
            println!("bucket_size={}", params.bucket_size);
            println!("leaves={}", params.leaves);
            println!("tree_height={}", params.height());
            println!("bucket_pages={}", params.bucket_count());
            println!("plaintext_page_bytes={}", params.bucket_bytes());
            println!("cached_pages={cached_pages}");
            println!("stash_len={}", oram.stash_len());
            println!("state_encrypted={}", state_key_hex.is_some());
            println!("elapsed_ms={}", elapsed.as_millis());
        }
        Command::Bench {
            image,
            state,
            state_key_hex,
            ops,
            encrypted,
            key_hex,
            cache_levels,
            query_seed_hex,
            no_save,
        } => {
            let loaded = load_state(&state, state_key_hex.as_deref())?;
            let params = loaded.params.clone();
            if params.block_size < 8 {
                return Err(Error::InvalidInput(
                    "state block_size must be at least 8 for benchmark checksum".into(),
                ));
            }
            let cached_pages = cached_pages_for_levels(&params, cache_levels)?;
            let store = open_file_store(
                &image,
                &params,
                encrypted,
                key_hex.as_deref(),
                cached_pages,
                true,
            )?;
            let mut oram = PathOram::from_state(store, loaded)?;
            let query_seed = parse_seed(query_seed_hex.as_deref(), 0x03)?;
            let mut query_rng = ChaCha20Rng::from_seed(query_seed);

            let started = Instant::now();
            let mut checksum = 0u64;
            for _ in 0..ops {
                let logical_id = query_rng.next_u64() % params.logical_blocks as u64;
                let payload = oram.read(logical_id)?;
                checksum ^= u64::from_le_bytes(payload[..8].try_into().expect("payload >= 8"));
            }
            let elapsed = started.elapsed();
            oram.flush()?;
            if !no_save {
                save_state(&oram.snapshot(), &state, state_key_hex.as_deref())?;
            }

            println!("bench=true");
            println!("ops={ops}");
            println!("logical_blocks={}", params.logical_blocks);
            println!("tree_height={}", params.height());
            println!("cached_pages={cached_pages}");
            println!("stash_len={}", oram.stash_len());
            println!("state_encrypted={}", state_key_hex.is_some());
            println!("elapsed_ms={}", elapsed.as_millis());
            println!(
                "avg_us={:.3}",
                elapsed.as_secs_f64() * 1_000_000.0 / ops.max(1) as f64
            );
            println!("checksum={checksum}");
        }
    }
    Ok(())
}

fn load_state(path: &Path, state_key_hex: Option<&str>) -> Result<OramState> {
    match state_key_hex {
        Some(key_hex) => OramState::load_encrypted(path, parse_32_hex(key_hex)?),
        None => OramState::load(path),
    }
}

fn save_state(state: &OramState, path: &Path, state_key_hex: Option<&str>) -> Result<()> {
    match state_key_hex {
        Some(key_hex) => state.save_encrypted_atomic(path, parse_32_hex(key_hex)?),
        None => state.save_atomic(path),
    }
}

fn open_file_store(
    image: &Path,
    params: &OramParams,
    encrypted: bool,
    key_hex: Option<&str>,
    cached_pages: usize,
    load_cached_pages: bool,
) -> Result<Box<dyn PageStore>> {
    let store: Box<dyn PageStore> = if encrypted {
        let key = parse_required_key(key_hex)?;
        let file = FilePageStore::open(
            image,
            params.bucket_count(),
            params.bucket_bytes() + AEAD_OVERHEAD,
        )?;
        Box::new(AeadPageStore::new(file, key, params.bucket_bytes())?)
    } else {
        Box::new(FilePageStore::open(
            image,
            params.bucket_count(),
            params.bucket_bytes(),
        )?)
    };

    if cached_pages == 0 {
        Ok(store)
    } else if load_cached_pages {
        Ok(Box::new(FrontCachedPageStore::new(store, cached_pages)?))
    } else {
        Ok(Box::new(FrontCachedPageStore::new_zeroed(
            store,
            cached_pages,
        )?))
    }
}

fn cached_pages_for_levels(params: &OramParams, cache_levels: usize) -> Result<usize> {
    if cache_levels == 0 {
        return Ok(0);
    }
    if cache_levels > params.height() {
        return Err(Error::InvalidInput(format!(
            "cache-levels {} > tree height {}",
            cache_levels,
            params.height()
        )));
    }
    Ok((1usize << cache_levels) - 1)
}

fn parse_seed(seed_hex: Option<&str>, default_byte: u8) -> Result<[u8; 32]> {
    match seed_hex {
        Some(seed_hex) => parse_32_hex(seed_hex),
        None => Ok([default_byte; 32]),
    }
}

fn parse_required_key(key_hex: Option<&str>) -> Result<[u8; 32]> {
    let key_hex = key_hex
        .ok_or_else(|| Error::InvalidInput("--key-hex is required with --encrypted".into()))?;
    parse_32_hex(key_hex)
}

fn parse_32_hex(input: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(input)?;
    if bytes.len() != 32 {
        return Err(Error::InvalidInput(format!(
            "expected 32-byte hex string, got {} bytes",
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn deterministic_payloads(blocks: usize, block_size: usize) -> Vec<Vec<u8>> {
    debug_assert!(block_size >= 8);
    (0..blocks)
        .map(|logical_id| {
            let mut payload = vec![0u8; block_size];
            payload[..8].copy_from_slice(&(logical_id as u64).to_le_bytes());
            payload
        })
        .collect()
}
