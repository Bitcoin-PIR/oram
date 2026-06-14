use bitcoinpir_oram::{
    stress_circuit, AeadPageStore, CircuitStressConfig, CircuitStressPattern, CircuitStressReport,
    CuckooOramEstimate, CuckooOramSizing, CuckooTableInfo, Error, FilePageStore,
    FrontCachedPageStore, OramParams, OramState, PageStore, PathOram, Result, AEAD_OVERHEAD,
};
use clap::{Parser, Subcommand, ValueEnum};
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
    /// Estimate ORAM sizes for existing DPF/Harmony cuckoo table directories.
    SizeCuckoo {
        /// BitcoinPIR DB directory containing batch_pir_cuckoo.bin and chunk_pir_cuckoo.bin.
        #[arg(long = "db-dir", required = true)]
        db_dirs: Vec<PathBuf>,
        /// Comma-separated bins packed into one ORAM logical block.
        #[arg(long, value_delimiter = ',', default_value = "8")]
        packs: Vec<usize>,
        /// Comma-separated divisors for leaves = next_power_of_two(ceil(blocks / divisor)).
        #[arg(long, value_delimiter = ',', default_value = "1,2,4,8")]
        leaf_divisors: Vec<usize>,
        /// Physical blocks per ORAM bucket.
        #[arg(long, default_value_t = 4)]
        bucket_size: usize,
        /// Fixed stash capacity in trusted memory.
        #[arg(long, default_value_t = 512)]
        stash_capacity: usize,
        /// Public top ORAM tree levels cached in trusted memory.
        #[arg(long, default_value_t = 5)]
        cache_levels: usize,
    },
    /// Stress-test Circuit ORAM stash pressure over DPF/Harmony cuckoo table sizes.
    StressCircuit {
        /// BitcoinPIR DB directory containing batch_pir_cuckoo.bin and chunk_pir_cuckoo.bin.
        #[arg(long = "db-dir", required = true)]
        db_dirs: Vec<PathBuf>,
        /// Comma-separated bins packed into one ORAM logical block.
        #[arg(long, value_delimiter = ',', default_value = "16")]
        packs: Vec<usize>,
        /// Comma-separated divisors for leaves = next_power_of_two(ceil(blocks / divisor)).
        #[arg(long, value_delimiter = ',', default_value = "4")]
        leaf_divisors: Vec<usize>,
        /// Physical blocks per Circuit ORAM bucket.
        #[arg(long, default_value_t = 2)]
        bucket_size: usize,
        /// Fixed stash capacity in trusted memory.
        #[arg(long, default_value_t = 512)]
        stash_capacity: usize,
        /// Measured real accesses per table/config.
        #[arg(long, default_value_t = 100_000)]
        ops: usize,
        /// Warm-up accesses excluded from stash percentiles.
        #[arg(long, default_value_t = 10_000)]
        warmup_ops: usize,
        /// Public logical-id sequence.
        #[arg(long, value_enum, default_value_t = StressPatternArg::Random)]
        pattern: StressPatternArg,
        /// Public eviction paths drained after each real access.
        #[arg(long, default_value_t = 2)]
        drain_per_access: u64,
        /// Public burst interval. Zero disables burst draining.
        #[arg(long, default_value_t = 0)]
        burst_interval: usize,
        /// Public eviction paths drained when --burst-interval fires.
        #[arg(long, default_value_t = 0)]
        burst_budget: u64,
        /// Optional public maximum pending eviction debt.
        #[arg(long)]
        max_debt: Option<u64>,
        /// 32-byte hex simulator RNG seed. Defaults to all 0x05.
        #[arg(long)]
        seed_hex: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum StressPatternArg {
    Random,
    RoundRobin,
}

impl From<StressPatternArg> for CircuitStressPattern {
    fn from(value: StressPatternArg) -> Self {
        match value {
            StressPatternArg::Random => Self::Random,
            StressPatternArg::RoundRobin => Self::RoundRobin,
        }
    }
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
        Command::SizeCuckoo {
            db_dirs,
            packs,
            leaf_divisors,
            bucket_size,
            stash_capacity,
            cache_levels,
        } => {
            print_cuckoo_sizes(
                &db_dirs,
                &packs,
                &leaf_divisors,
                bucket_size,
                stash_capacity,
                cache_levels,
            )?;
        }
        Command::StressCircuit {
            db_dirs,
            packs,
            leaf_divisors,
            bucket_size,
            stash_capacity,
            ops,
            warmup_ops,
            pattern,
            drain_per_access,
            burst_interval,
            burst_budget,
            max_debt,
            seed_hex,
        } => {
            print_circuit_stress(
                &db_dirs,
                &packs,
                &leaf_divisors,
                bucket_size,
                stash_capacity,
                ops,
                warmup_ops,
                pattern.into(),
                drain_per_access,
                burst_interval,
                burst_budget,
                max_debt,
                parse_seed(seed_hex.as_deref(), 0x05)?,
            )?;
        }
    }
    Ok(())
}

fn print_cuckoo_sizes(
    db_dirs: &[PathBuf],
    packs: &[usize],
    leaf_divisors: &[usize],
    bucket_size: usize,
    stash_capacity: usize,
    cache_levels: usize,
) -> Result<()> {
    if packs.is_empty() {
        return Err(Error::InvalidInput(
            "at least one --packs value is required".into(),
        ));
    }
    if leaf_divisors.is_empty() {
        return Err(Error::InvalidInput(
            "at least one --leaf-divisors value is required".into(),
        ));
    }

    println!("size_cuckoo=true");
    println!("bucket_size={bucket_size}");
    println!("stash_capacity={stash_capacity}");
    println!("cache_levels={cache_levels}");

    for db_dir in db_dirs {
        let tables = CuckooTableInfo::load_pair(db_dir)?;
        let original_cuckoo_bytes: u64 = tables.iter().map(|t| t.file_bytes).sum();
        println!(
            "db db_dir={} original_cuckoo_bytes={} original_cuckoo_gib={:.3}",
            db_dir.display(),
            original_cuckoo_bytes,
            gib(original_cuckoo_bytes)
        );
        for table in &tables {
            println!(
                "table db_dir={} level={} file_bytes={} file_gib={:.3} anchor={} data_offset={} k={} bins_per_table={} slots_per_bin={} slot_size={} bin_size={} total_bins={} table_byte_size={} master_seed=0x{:016x} tag_seed={}",
                db_dir.display(),
                table.level,
                table.file_bytes,
                gib(table.file_bytes),
                table.anchor_kind,
                table.data_offset,
                table.k,
                table.bins_per_table,
                table.slots_per_bin,
                table.slot_size,
                table.bin_size(),
                table.total_bins(),
                table.table_byte_size(),
                table.master_seed,
                table
                    .tag_seed
                    .map(|seed| format!("0x{seed:016x}"))
                    .unwrap_or_else(|| "none".to_string())
            );
        }

        for &pack in packs {
            for &leaf_divisor in leaf_divisors {
                let sizing = CuckooOramSizing {
                    bins_per_block: pack,
                    leaf_divisor,
                    bucket_size,
                    stash_capacity,
                    cache_levels,
                };
                let mut total_image_plaintext = 0u64;
                let mut total_image_aead = 0u64;
                let mut total_pos_map = 0u64;
                let mut total_trusted_state_floor = 0u64;
                let mut total_front_cache_plaintext = 0u64;
                let mut total_front_cache_aead = 0u64;

                for table in &tables {
                    let estimate = sizing.estimate(table)?;
                    print_estimate(db_dir, &estimate);
                    total_image_plaintext += estimate.image_plaintext_bytes;
                    total_image_aead += estimate.image_aead_bytes;
                    total_pos_map += estimate.pos_map_bytes;
                    total_trusted_state_floor += estimate.trusted_state_floor_bytes;
                    total_front_cache_plaintext += estimate.front_cache_plaintext_bytes;
                    total_front_cache_aead += estimate.front_cache_aead_bytes;
                }

                println!(
                    "total db_dir={} pack={} leaf_divisor={} image_plaintext_bytes={} image_plaintext_gib={:.3} image_aead_bytes={} image_aead_gib={:.3} original_cuckoo_bytes={} original_cuckoo_gib={:.3} expansion_aead_vs_cuckoo={:.3} pos_map_bytes={} pos_map_mib={:.3} trusted_state_floor_bytes={} trusted_state_floor_mib={:.3} front_cache_plaintext_bytes={} front_cache_plaintext_mib={:.3} front_cache_aead_bytes={} front_cache_aead_mib={:.3}",
                    db_dir.display(),
                    pack,
                    leaf_divisor,
                    total_image_plaintext,
                    gib(total_image_plaintext),
                    total_image_aead,
                    gib(total_image_aead),
                    original_cuckoo_bytes,
                    gib(original_cuckoo_bytes),
                    total_image_aead as f64 / original_cuckoo_bytes.max(1) as f64,
                    total_pos_map,
                    mib(total_pos_map),
                    total_trusted_state_floor,
                    mib(total_trusted_state_floor),
                    total_front_cache_plaintext,
                    mib(total_front_cache_plaintext),
                    total_front_cache_aead,
                    mib(total_front_cache_aead),
                );
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn print_circuit_stress(
    db_dirs: &[PathBuf],
    packs: &[usize],
    leaf_divisors: &[usize],
    bucket_size: usize,
    stash_capacity: usize,
    ops: usize,
    warmup_ops: usize,
    pattern: CircuitStressPattern,
    drain_per_access: u64,
    burst_interval: usize,
    burst_budget: u64,
    max_debt: Option<u64>,
    seed: [u8; 32],
) -> Result<()> {
    if packs.is_empty() {
        return Err(Error::InvalidInput(
            "at least one --packs value is required".into(),
        ));
    }
    if leaf_divisors.is_empty() {
        return Err(Error::InvalidInput(
            "at least one --leaf-divisors value is required".into(),
        ));
    }

    println!("stress_circuit=true");
    println!("bucket_size={bucket_size}");
    println!("stash_capacity={stash_capacity}");
    println!("ops={ops}");
    println!("warmup_ops={warmup_ops}");
    println!("pattern={}", pattern.label());
    println!("drain_per_access={drain_per_access}");
    println!("burst_interval={burst_interval}");
    println!("burst_budget={burst_budget}");
    println!("seed_hex={}", hex::encode(seed));
    println!(
        "max_debt={}",
        max_debt
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string())
    );

    for db_dir in db_dirs {
        let tables = CuckooTableInfo::load_pair(db_dir)?;
        for table in &tables {
            for &pack in packs {
                for &leaf_divisor in leaf_divisors {
                    let sizing = CuckooOramSizing {
                        bins_per_block: pack,
                        leaf_divisor,
                        bucket_size,
                        stash_capacity,
                        cache_levels: 0,
                    };
                    let estimate = sizing.estimate(table)?;
                    let params = OramParams::with_leaves(
                        estimate.logical_blocks,
                        estimate.block_payload_bytes,
                        estimate.leaves,
                    )?
                    .with_bucket_size(bucket_size)?
                    .with_stash_capacity(stash_capacity)?;
                    let report = stress_circuit(CircuitStressConfig {
                        params,
                        ops,
                        warmup_ops,
                        pattern,
                        seed,
                        drain_per_access,
                        burst_interval,
                        burst_budget,
                        max_debt,
                    })?;
                    print_stress_report(db_dir, table, &estimate, &report);
                }
            }
        }
    }

    Ok(())
}

fn print_stress_report(
    db_dir: &Path,
    table: &CuckooTableInfo,
    estimate: &CuckooOramEstimate,
    report: &CircuitStressReport,
) {
    println!(
        "stress db_dir={} level={} anchor={} pack={} leaf_divisor={} total_bins={} logical_blocks={} block_payload_bytes={} bucket_size={} leaves={} height={} tree_slots={} tree_slot_load_percent={:.3} ops={} warmup_ops={} pattern={} stash_capacity={} init_stash={} final_stash={} max_stash={} avg_stash={:.3} p50_stash={} p99_stash={} p999_stash={} overflow_samples={} evictions_per_access={} drain_per_access={} burst_interval={} burst_budget={} max_debt_cap={} max_eviction_debt={} final_eviction_debt={} completed_evictions={} scheduled_evictions={} metadata_path_scans_per_access={:.3} payload_path_scans_per_access={:.3}",
        db_dir.display(),
        table.level,
        table.anchor_kind,
        estimate.bins_per_block,
        estimate.leaf_divisor,
        estimate.total_bins,
        report.logical_blocks,
        estimate.block_payload_bytes,
        report.bucket_size,
        report.leaves,
        report.height,
        report.tree_slots,
        report.tree_slot_load_percent,
        report.ops,
        report.warmup_ops,
        report.pattern.label(),
        report.stash_capacity,
        report.init_stash,
        report.final_stash,
        report.max_stash,
        report.avg_stash,
        report.p50_stash,
        report.p99_stash,
        report.p999_stash,
        report.overflow_samples,
        report.evictions_per_access,
        report.drain_per_access,
        report.burst_interval,
        report.burst_budget,
        report
            .max_debt_cap
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        report.max_eviction_debt,
        report.final_eviction_debt,
        report.completed_evictions,
        report.scheduled_evictions,
        report.metadata_path_scans_per_access,
        report.payload_path_scans_per_access,
    );
}

fn print_estimate(db_dir: &Path, e: &CuckooOramEstimate) {
    println!(
        "estimate db_dir={} level={} pack={} leaf_divisor={} total_bins={} logical_blocks={} block_payload_bytes={} bucket_size={} leaves={} height={} bucket_pages={} page_plaintext_bytes={} page_aead_bytes={} image_plaintext_bytes={} image_plaintext_gib={:.3} image_aead_bytes={} image_aead_gib={:.3} pos_map_bytes={} pos_map_mib={:.3} trusted_stash_bytes={} trusted_stash_mib={:.3} trusted_state_floor_bytes={} trusted_state_floor_mib={:.3} cached_pages={} front_cache_plaintext_bytes={} front_cache_plaintext_mib={:.3} front_cache_aead_bytes={} front_cache_aead_mib={:.3} disk_pages_per_access_no_flush={} disk_aead_bytes_per_access_no_flush={} disk_aead_mib_per_access_no_flush={:.3} tree_slot_load_percent={:.3}",
        db_dir.display(),
        e.level,
        e.bins_per_block,
        e.leaf_divisor,
        e.total_bins,
        e.logical_blocks,
        e.block_payload_bytes,
        e.bucket_size,
        e.leaves,
        e.height,
        e.bucket_pages,
        e.page_plaintext_bytes,
        e.page_aead_bytes,
        e.image_plaintext_bytes,
        gib(e.image_plaintext_bytes),
        e.image_aead_bytes,
        gib(e.image_aead_bytes),
        e.pos_map_bytes,
        mib(e.pos_map_bytes),
        e.trusted_stash_bytes,
        mib(e.trusted_stash_bytes),
        e.trusted_state_floor_bytes,
        mib(e.trusted_state_floor_bytes),
        e.cached_pages,
        e.front_cache_plaintext_bytes,
        mib(e.front_cache_plaintext_bytes),
        e.front_cache_aead_bytes,
        mib(e.front_cache_aead_bytes),
        e.disk_pages_per_access_no_flush,
        e.disk_aead_bytes_per_access_no_flush,
        mib(e.disk_aead_bytes_per_access_no_flush),
        e.tree_slot_load_percent,
    );
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0 / 1024.0
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
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
