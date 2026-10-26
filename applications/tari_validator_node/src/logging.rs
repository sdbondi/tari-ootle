//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use log::*;
use tari_logging::{ConsoleFormat, LogFileConfig, LoggingConfig, dropped_lines};
use tari_shutdown::ShutdownSignal;

const LOG_TARGET: &str = "tari::validator_node::logging";

/// How often dropped log lines are reported.
const DROPPED_LINES_REPORT_INTERVAL: Duration = Duration::from_secs(30);

/// The validator node's sinks: one file per subsystem, so that the busiest (engine, consensus) can be read, rotated
/// and silenced on their own.
pub fn default_logging_config() -> LoggingConfig {
    let files = [
        (
            "ootle",
            LogFileConfig::new(directives(&[
                "tari::application=info",
                "tari::validator_node=debug",
                "tari::ootle::epoch_manager=debug",
                "tari::ootle::epoch_oracles=debug",
                "tari::ootle::p2p=debug",
            ])),
        ),
        (
            "consensus",
            LogFileConfig::new(directives(&[
                "tari::consensus=debug",
                "tari::ootle::consensus=debug",
                "tari::ootle::hotstuff=debug",
                "tari::ootle::rpc_state_sync=debug",
                "tari::ootle::storage=debug",
                "c::sidechain::validations=debug",
            ])),
        ),
        ("engine", LogFileConfig::new("tari::ootle::engine=debug")),
        (
            "network",
            LogFileConfig::new(directives(&["tari::networking=debug", "tari::ootle::p2p=debug"])),
        ),
        (
            "libp2p",
            LogFileConfig::new(directives(&[
                // Takes the warnings of the libp2p crates listed nowhere else, which the console leaves out.
                "libp2p=warn",
                "libp2p_core=info",
                "libp2p_gossipsub=info",
                "libp2p_identify=debug",
                "libp2p_kad=debug",
                "libp2p_mdns=debug",
                "libp2p_messaging=debug",
                "libp2p_noise=debug",
                "libp2p_peersync=debug",
                "libp2p_ping=debug",
                "libp2p_quic=debug",
                "libp2p_swarm=debug",
                "libp2p_tcp=debug",
                "quinn=info",
                "hickory_proto=info",
                "multistream_select=info",
            ]))
            .with_max_file_size_mb(10),
        ),
        (
            "other",
            LogFileConfig::new(directives(&[
                "tari::application=info",
                "regalloc=info",
                "cranelift_codegen=info",
                "wasmer_compiler_cranelift=warn",
                "hyper=info",
                "tower=info",
            ])),
        ),
    ];

    LoggingConfig {
        directory: PathBuf::from("log/validator-node"),
        // The third-party targets the files above take are kept off the console.
        console: directives(&[
            "warn",
            "tari=info",
            "c::sidechain=info",
            "regalloc=info",
            "cranelift_codegen=info",
            "wasmer_compiler_cranelift=off",
            "hyper=off",
            "tower=off",
            "libp2p=off",
            "quinn=off",
            "hickory_proto=off",
            "multistream_select=off",
        ]),
        console_format: ConsoleFormat::Text,
        files: files
            .into_iter()
            .map(|(name, file)| (name.to_string(), file))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn directives(directives: &[&str]) -> String {
    directives.join(",")
}

/// Warns, every [`DROPPED_LINES_REPORT_INTERVAL`], about each sink that dropped lines since the last report.
pub async fn report_dropped_lines(mut shutdown: ShutdownSignal) {
    let mut reported = BTreeMap::new();
    let mut interval = tokio::time::interval(DROPPED_LINES_REPORT_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = interval.tick() => {},
            _ = shutdown.wait() => return,
        }
        for (sink, dropped) in dropped_lines() {
            let previous = reported.insert(sink, dropped).unwrap_or(0);
            if dropped > previous {
                warn!(
                    target: LOG_TARGET,
                    "Dropped {} {sink} log lines in the last {}s because the {sink} log writer fell behind",
                    dropped - previous,
                    DROPPED_LINES_REPORT_INTERVAL.as_secs(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tari_logging::build;

    use super::*;

    #[test]
    fn a_configured_file_changes_only_the_keys_it_sets() {
        use tari_common::DefaultConfigLoader;

        use crate::ValidatorNodeConfig;

        let config = config::Config::builder()
            .set_override("validator_node.logging.files.engine.filter", "tari::ootle::engine=info")
            .unwrap()
            .set_override("validator_node.logging.files.audit.filter", "tari::audit=info")
            .unwrap()
            .set_override("validator_node.logging.console_format", "json")
            .unwrap()
            .build()
            .unwrap();
        let logging = ValidatorNodeConfig::load_from(&config).unwrap().logging;
        let defaults = default_logging_config();

        let engine = &logging.files["engine"];
        assert_eq!(engine.filter, "tari::ootle::engine=info");
        assert_eq!(engine.max_file_size_mb, defaults.files["engine"].max_file_size_mb);
        assert_eq!(logging.files["consensus"].filter, defaults.files["consensus"].filter);
        assert_eq!(logging.files["audit"].filter, "tari::audit=info");
        assert_eq!(logging.files.len(), defaults.files.len() + 1);
        assert_eq!(logging.console_format, ConsoleFormat::Json);
        assert_eq!(logging.console, defaults.console);
    }

    #[test]
    fn a_warning_from_an_unlisted_libp2p_crate_reaches_the_libp2p_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = default_logging_config();
        let (subscriber, writers) = build(&config, &config.console, dir.path(), Vec::new()).unwrap();
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "libp2p_relay::priv_client", "relay reservation failed");
        });
        drop(writers);

        let libp2p = std::fs::read_to_string(dir.path().join("log/validator-node/libp2p.log")).unwrap();
        assert!(libp2p.contains("relay reservation failed"), "{libp2p}");
    }

    #[test]
    fn default_filters_parse() {
        let dir = tempfile::tempdir().unwrap();
        let config = default_logging_config();
        let (_subscriber, _writers) = build(&config, &config.console, dir.path(), Vec::new()).unwrap();
        for name in config.files.keys() {
            assert!(
                dir.path()
                    .join("log/validator-node")
                    .join(format!("{name}.log"))
                    .exists()
            );
        }
    }
}
