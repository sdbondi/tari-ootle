//   Copyright 2024 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

const DEFAULT_OUTPUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/value_lookup.bin");

/// On-disk lookup table format to generate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Sorted prefix index (`VLK2`/self-describing) — O(log n) reverse lookup via binary search.
    Sorted,
    /// Legacy dense array (`VLKP`) indexed by value — O(n) reverse lookup via scan. Kept for comparison.
    Dense,
}

#[derive(Debug, Parser)]
pub struct Cli {
    /// Path to output the lookup file
    #[clap(short = 'o', long, default_value = DEFAULT_OUTPUT)]
    pub output_file: PathBuf,
    /// The minimum value to include in the lookup table
    #[clap(short = 'm', long, default_value = "0")]
    pub min: u64,
    /// The maximum value to include in the lookup table. Required when generating; ignored when validating.
    #[clap(short = 'x', long)]
    pub max: Option<u64>,
    /// The number of worker threads to use
    #[clap(short = 'j', long)]
    pub jobs: Option<usize>,

    /// Table format to generate. Defaults to the sorted prefix index.
    #[clap(short = 'f', long, value_enum, default_value_t = Format::Sorted)]
    pub format: Format,

    /// Number of leading point bytes stored as the search key in the sorted format (1..=8).
    #[clap(long, default_value = "8")]
    pub prefix_len: u8,
}

impl Cli {
    pub fn init() -> Self {
        Self::parse()
    }
}
