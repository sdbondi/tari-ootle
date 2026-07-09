//   Copyright 2024 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    fs,
    fs::File,
    io,
    io::{BufWriter, Write},
    path::Path,
    time::{Duration, Instant},
};

use futures::{StreamExt, stream::FuturesOrdered};
use human_bytes::human_bytes;
use tari_crypto::{
    keys::PublicKey,
    ristretto::{RistrettoPublicKey, RistrettoSecretKey},
    tari_utilities::ByteArray,
};
use tari_ootle_wallet_crypto::SortedLookupHeader;

use crate::cli::Cli;
mod cli;

#[tokio::main]
async fn main() -> io::Result<()> {
    let cli = Cli::init();
    let dest_file = cli.output_file.clone();

    let max = cli
        .max
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "--max is required when generating a table"))?;
    if max < cli.min {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("max ({}) must be >= min ({})", max, cli.min),
        ));
    }

    // Determine number of workers
    let jobs = cli
        .jobs
        .unwrap_or_else(|| tokio::runtime::Handle::current().metrics().num_workers());

    generate_sorted(&dest_file, cli.min, max, cli.prefix_len, jobs).await?;

    println!();
    let metadata = fs::metadata(&dest_file)?;
    println!(
        "Output written to {} ({})",
        dest_file.display(),
        human_bytes(metadata.len() as f64),
    );

    Ok(())
}

/// Generates the sorted prefix-index (`VLK2`) table: EC points are computed in parallel, sorted by point
/// prefix, then written. Sorting is in-memory, so this is bounded by available RAM.
async fn generate_sorted(dest: &Path, min: u64, max: u64, prefix_len: u8, num_threads: usize) -> io::Result<()> {
    if !(1..=8).contains(&prefix_len) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "prefix_len must be in 1..=8",
        ));
    }

    let value_len = SortedLookupHeader::required_value_len(min, max);
    let header = SortedLookupHeader::new(min, max, prefix_len, value_len);
    let count = header.count();
    let file_size = SortedLookupHeader::SIZE as u64 + count * header.stride() as u64;

    println!(
        "Generating sorted prefix-index table from {min} to {max} -> {} ({}). prefix_len={prefix_len}, \
         value_len={value_len}.",
        dest.display(),
        human_bytes(file_size as f64),
    );
    println!(
        "In-memory sort needs ~{} of RAM; for tables larger than RAM an external/bucketed sort is required.\n",
        human_bytes(count as f64 * 16.0),
    );

    // Phase 1: parallel EC point generation.
    println!("Phase 1/2: generating {count} points on {num_threads} worker threads.");
    let mut entries = generate_sorted_entries(min, max, num_threads).await?;

    // Phase 2: sort by prefix key (u64 tuple sorts by key then offset).
    println!("\nPhase 2/2: sorting {} entries.", entries.len());
    let timer = Instant::now();
    entries.sort_unstable();
    println!(
        "Sorted in {}.",
        humantime::format_duration(Duration::from_secs(timer.elapsed().as_secs()))
    );

    let mut writer = BufWriter::new(File::create(dest)?);
    header.encode_into(&mut writer)?;
    let mut record = vec![0u8; header.stride()];
    for (key, offset) in entries {
        record[..prefix_len as usize].copy_from_slice(&key.to_be_bytes()[..prefix_len as usize]);
        record[prefix_len as usize..].copy_from_slice(&offset.to_le_bytes()[..value_len as usize]);
        writer.write_all(&record)?;
    }
    writer.flush()?;
    Ok(())
}

/// Computes `(prefix_key, value_offset)` for every value in `min..=max` using `num_threads` blocking workers.
///
/// `prefix_key` is the first 8 bytes of the compressed point read big-endian, so numeric ordering of the key
/// equals lexicographic ordering of the stored prefix bytes.
async fn generate_sorted_entries(min: u64, max: u64, num_threads: usize) -> io::Result<Vec<(u64, u64)>> {
    const CHUNK_SIZE: u64 = 50_000;
    let count = (max - min + 1) as usize;
    let mut entries: Vec<(u64, u64)> = Vec::with_capacity(count);

    let mut chunks = (min..=max).step_by(CHUNK_SIZE as usize).map(|chunk_start| {
        let chunk_end = std::cmp::min(chunk_start + CHUNK_SIZE - 1, max);
        (chunk_start, chunk_end)
    });

    let mut handles = FuturesOrdered::new();
    let mut done = 0usize;
    let timer = Instant::now();
    let mut last_report = 0usize;

    loop {
        while handles.len() < num_threads {
            match chunks.next() {
                Some((chunk_start, chunk_end)) => {
                    handles.push_back(tokio::task::spawn_blocking(move || {
                        let mut out = Vec::with_capacity((chunk_end - chunk_start + 1) as usize);
                        for v in chunk_start..=chunk_end {
                            let pk = RistrettoPublicKey::from_secret_key(&RistrettoSecretKey::from(v));
                            let mut key = [0u8; 8];
                            key.copy_from_slice(&pk.as_bytes()[..8]);
                            out.push((u64::from_be_bytes(key), v - min));
                        }
                        out
                    }));
                },
                None => break,
            }
        }

        if handles.is_empty() {
            break;
        }

        let chunk = handles.next().await.expect("handles stream end")?;
        done += chunk.len();
        entries.extend(chunk);

        if done - last_report > count / 20 {
            last_report = done;
            let elapsed = timer.elapsed();
            let eta = Duration::from_secs(
                ((count - done) as f64 / done.max(1) as f64 * elapsed.as_secs_f64()).round() as u64,
            );
            println!(
                "{:.1}% ({done}/{count}) generated in {}, ETA {}",
                done as f64 / count as f64 * 100.0,
                humantime::format_duration(Duration::from_secs(elapsed.as_secs())),
                humantime::format_duration(eta),
            );
        }
    }

    Ok(entries)
}
