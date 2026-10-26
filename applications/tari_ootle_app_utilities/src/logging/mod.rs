//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Process logging through `tracing`, with each sink written by a background thread of its own.
//!
//! A thread that logs only formats the line and queues it; the sink's writer thread does the I/O. Logging threads
//! therefore never wait on one another for a file, which matters once many threads log at debug level. A sink whose
//! queue is full drops the line, so a caller never waits, and [`dropped_lines`] counts what each sink dropped.
//!
//! Records from the `log` crate are bridged into `tracing`, so `log` macros need no change.
//!
//! Queued lines reach their files when the [`LoggingGuard`] drops or [`flush_and_close`] runs. A process that aborts
//! on panic runs no destructors, so its panic hook must call [`flush_and_close`] itself.

mod rolling_file;

use std::{
    collections::BTreeMap,
    env,
    io,
    io::IsTerminal,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Mutex, OnceLock},
};

use serde::{Deserialize, Serialize};
use tracing::Subscriber;
use tracing_appender::non_blocking::{ErrorCounter, NonBlockingBuilder, WorkerGuard};
pub use tracing_subscriber::Registry;
use tracing_subscriber::{Layer, filter::Targets, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use self::rolling_file::RollingFile;

/// A layer in the process subscriber, beside the configured sinks.
pub type BoxedLayer = Box<dyn Layer<Registry> + Send + Sync>;

/// Where log events go. A filter is a comma-separated list of `target=level` directives plus an optional bare level
/// for every other target, e.g. `warn,tari::ootle::engine=debug`. A directive's target matches every target it
/// prefixes, and the longest matching directive decides. `off` silences a target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    /// The directory log files are written to. A relative path is resolved against the base path.
    pub directory: PathBuf,
    /// The events printed to stdout. `RUST_LOG`, when set, replaces it.
    pub console: String,
    /// File sinks by name. The sink named `<name>` writes `<directory>/<name>.log`.
    pub files: BTreeMap<String, LogFileConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogFileConfig {
    /// The events written to this file. `off` disables the file.
    pub filter: String,
    /// The size in megabytes at which the file is rotated.
    #[serde(default = "default_max_file_size_mb")]
    pub max_file_size_mb: u64,
    /// The number of rotated files kept beside the live one.
    #[serde(default = "default_max_rotated_files")]
    pub max_rotated_files: usize,
}

impl LogFileConfig {
    pub fn new<S: Into<String>>(filter: S) -> Self {
        Self {
            filter: filter.into(),
            max_file_size_mb: default_max_file_size_mb(),
            max_rotated_files: default_max_rotated_files(),
        }
    }

    pub fn with_max_file_size_mb(mut self, max_file_size_mb: u64) -> Self {
        self.max_file_size_mb = max_file_size_mb;
        self
    }
}

fn default_max_file_size_mb() -> u64 {
    100
}

fn default_max_rotated_files() -> usize {
    5
}

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("Invalid log filter for {sink}: '{filter}': {source}")]
    InvalidFilter {
        sink: String,
        filter: String,
        source: tracing_subscriber::filter::ParseError,
    },
    #[error("Could not open log file {}: {source}", .path.display())]
    OpenFile { path: PathBuf, source: io::Error },
    #[error("A global logger is already installed: {0}")]
    AlreadyInitialized(#[from] tracing_subscriber::util::TryInitError),
}

/// The name [`dropped_lines`] reports the console sink under.
pub const CONSOLE_SINK: &str = "console";

/// The background writers of a built subscriber. Dropping them drains every queued line to its sink.
pub struct LogWriters {
    guards: Vec<WorkerGuard>,
    dropped: Vec<(String, ErrorCounter)>,
}

/// Flushes and closes every sink when dropped. Hold it until the process is done logging.
#[must_use = "dropping the guard closes every log sink"]
pub struct LoggingGuard {
    _private: (),
}

impl Drop for LoggingGuard {
    fn drop(&mut self) {
        flush_and_close();
    }
}

static WRITER_GUARDS: Mutex<Vec<WorkerGuard>> = Mutex::new(Vec::new());
static DROPPED_LINES: OnceLock<Vec<(String, ErrorCounter)>> = OnceLock::new();

/// Installs the process-wide subscriber and the `log` bridge.
///
/// `extra_layers` join the configured sinks, e.g. a tokio-console layer.
pub fn init(
    config: &LoggingConfig,
    base_path: &Path,
    extra_layers: Vec<BoxedLayer>,
) -> Result<LoggingGuard, LoggingError> {
    let console_filter = env::var("RUST_LOG")
        .ok()
        .filter(|filter| !filter.trim().is_empty())
        .unwrap_or_else(|| config.console.clone());
    let (subscriber, writers) = build(config, &console_filter, base_path, extra_layers)?;
    // Also installs the `log` bridge, with `log`'s maximum level set to the most verbose level any sink accepts.
    subscriber.try_init()?;

    let LogWriters { guards, dropped } = writers;
    *WRITER_GUARDS.lock().unwrap_or_else(|e| e.into_inner()) = guards;
    let _ignore = DROPPED_LINES.set(dropped);
    Ok(LoggingGuard { _private: () })
}

/// Drains every queued line to its sink and closes the sinks; lines logged afterwards are dropped. Waits up to about a
/// second per sink.
///
/// The guards drop with the lock held, so a concurrent caller (a second panicking thread) returns only after the drain
/// has finished.
pub fn flush_and_close() {
    WRITER_GUARDS.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// The number of lines each sink has dropped because its queue was full, by sink name.
pub fn dropped_lines() -> impl Iterator<Item = (&'static str, usize)> {
    DROPPED_LINES
        .get()
        .into_iter()
        .flatten()
        .map(|(sink, counter)| (sink.as_str(), counter.dropped_lines()))
}

/// Builds the subscriber without installing it.
pub fn build(
    config: &LoggingConfig,
    console_filter: &str,
    base_path: &Path,
    extra_layers: Vec<BoxedLayer>,
) -> Result<(impl Subscriber + Send + Sync + 'static, LogWriters), LoggingError> {
    let directory = base_path.join(&config.directory);
    std::fs::create_dir_all(&directory).map_err(|source| LoggingError::OpenFile {
        path: directory.clone(),
        source,
    })?;

    let mut layers = extra_layers;
    let mut writers = LogWriters {
        guards: Vec::new(),
        dropped: Vec::new(),
    };

    for (name, file) in &config.files {
        if file.filter.trim() == "off" {
            continue;
        }
        let filter = parse_filter(name, &file.filter)?;
        let rolling = RollingFile::open(
            &directory,
            name,
            file.max_file_size_mb.saturating_mul(1024 * 1024),
            file.max_rotated_files,
        )
        .map_err(|source| LoggingError::OpenFile {
            path: directory.join(format!("{name}.log")),
            source,
        })?;
        let (writer, guard) = NonBlockingBuilder::default()
            .lossy(true)
            .thread_name(&format!("log-{name}"))
            .finish(rolling);
        writers.dropped.push((name.clone(), writer.error_counter()));
        writers.guards.push(guard);
        layers.push(
            fmt::Layer::new()
                .with_writer(writer)
                .with_ansi(false)
                .with_file(true)
                .with_line_number(true)
                .with_filter(filter)
                .boxed(),
        );
    }

    let filter = parse_filter(CONSOLE_SINK, console_filter)?;
    let (writer, guard) = NonBlockingBuilder::default()
        .lossy(true)
        .thread_name("log-console")
        .finish(io::stdout());
    writers.dropped.push((CONSOLE_SINK.to_string(), writer.error_counter()));
    writers.guards.push(guard);
    layers.push(
        fmt::Layer::new()
            .with_writer(writer)
            .with_ansi(io::stdout().is_terminal())
            .with_target(false)
            .with_filter(filter)
            .boxed(),
    );

    Ok((tracing_subscriber::registry().with(layers), writers))
}

fn parse_filter(sink: &str, filter: &str) -> Result<Targets, LoggingError> {
    Targets::from_str(filter).map_err(|source| LoggingError::InvalidFilter {
        sink: sink.to_string(),
        filter: filter.to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn config(files: &[(&str, &str)]) -> LoggingConfig {
        LoggingConfig {
            directory: PathBuf::from("log"),
            console: "off".to_string(),
            files: files
                .iter()
                .map(|(name, filter)| (name.to_string(), LogFileConfig::new(*filter)))
                .collect(),
        }
    }

    fn log_to(dir: &Path, config: &LoggingConfig, emit: impl FnOnce()) {
        let (subscriber, writers) = build(config, "off", dir, Vec::new()).unwrap();
        tracing::subscriber::with_default(subscriber, emit);
        drop(writers);
    }

    fn read(dir: &Path, name: &str) -> String {
        fs::read_to_string(dir.join("log").join(format!("{name}.log"))).unwrap_or_default()
    }

    #[test]
    fn routes_each_target_to_the_files_whose_filter_accepts_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(&[
            ("engine", "tari::ootle::engine=debug"),
            ("ootle", "tari::ootle=info,tari::ootle::engine=off"),
            ("disabled", "off"),
        ]);
        log_to(dir.path(), &config, || {
            tracing::debug!(target: "tari::ootle::engine::runtime", "engine debug");
            tracing::info!(target: "tari::ootle::engine", "engine info");
            tracing::debug!(target: "tari::ootle::consensus", "consensus debug");
            tracing::info!(target: "tari::ootle::consensus", "consensus info");
        });

        let engine = read(dir.path(), "engine");
        assert!(engine.contains("engine debug"), "{engine}");
        assert!(engine.contains("engine info"), "{engine}");
        assert!(!engine.contains("consensus"), "{engine}");

        let ootle = read(dir.path(), "ootle");
        assert!(ootle.contains("consensus info"), "{ootle}");
        assert!(!ootle.contains("consensus debug"), "{ootle}");
        assert!(!ootle.contains("engine"), "{ootle}");

        assert!(!dir.path().join("log").join("disabled.log").exists());
    }

    #[test]
    fn writes_file_lines_in_the_shape_log_readers_parse() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(&[("consensus", "tari::consensus=debug")]);
        log_to(dir.path(), &config, || {
            tracing::info!(target: "tari::consensus", "⏲️ prepare took 1.5ms");
        });

        let contents = read(dir.path(), "consensus");
        let line = contents.lines().next().expect("a line");
        let (timestamp, rest) = line.split_once(' ').unwrap();
        assert!(timestamp.ends_with('Z'), "{line}");
        let (date, time) = timestamp.split_once('T').unwrap();
        assert_eq!(date.len(), "2026-10-25".len(), "{line}");
        assert!(time.starts_with(|c: char| c.is_ascii_digit()), "{line}");
        let (location, message) = rest
            .trim_start()
            .strip_prefix("INFO tari::consensus: ")
            .and_then(|rest| rest.split_once(": "))
            .unwrap_or_else(|| panic!("{line}"));
        let (file, line_number) = location.rsplit_once(':').unwrap();
        assert_eq!(file, file!(), "{line}");
        assert!(line_number.parse::<u32>().is_ok(), "{line}");
        assert_eq!(message, "⏲️ prepare took 1.5ms", "{line}");
        println!("{line}");
    }

    #[test]
    fn rejects_a_malformed_filter_naming_its_sink() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(&[("engine", "tari::ootle::engine=loud")]);
        let err = build(&config, "off", dir.path(), Vec::new()).err().unwrap();
        assert!(
            matches!(&err, LoggingError::InvalidFilter { sink, .. } if sink == "engine"),
            "{err}"
        );
    }
}
