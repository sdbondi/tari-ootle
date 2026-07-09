//   Copyright 2024 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::path::PathBuf;

use clap::Parser;

const DEFAULT_OUTPUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/value_lookup.bin");

#[derive(Debug, Parser)]
pub struct Cli {
    /// Path to output the lookup file
    #[clap(short = 'o', long, default_value = DEFAULT_OUTPUT)]
    pub output_file: PathBuf,
    /// The minimum value to include in the lookup table
    #[clap(short = 'm', long, default_value = "0")]
    pub min: u64,
    /// The maximum value to include in the lookup table
    #[clap(short = 'x', long)]
    pub max: Option<u64>,
    /// The number of worker threads to use
    #[clap(short = 'j', long)]
    pub jobs: Option<usize>,

    /// Number of leading point bytes stored as the search key (1..=8).
    #[clap(long, default_value = "8")]
    pub prefix_len: u8,
}

impl Cli {
    pub fn init() -> Self {
        Self::parse()
    }
}
