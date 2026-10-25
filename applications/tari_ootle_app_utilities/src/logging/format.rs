//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::fmt;

use chrono::{Datelike, Local, Timelike};
use tracing::{Event, Level, Subscriber};
use tracing_log::NormalizeEvent;
use tracing_subscriber::{
    fmt::{FmtContext, FormatEvent, FormatFields, format::Writer},
    registry::LookupSpan,
};

/// The shape of one log line.
///
/// A file line is `2026-10-25 15:59:22.123456789 [target] LEVEL message // file:line`, in local time. Log readers
/// (the swarm web UI, the operator scripts) parse this shape, so it must not change without them.
#[derive(Debug, Clone, Copy)]
pub enum LineFormat {
    File,
    /// `15:59 LEVEL message`, with the level coloured when the writer accepts ANSI escapes.
    Console,
}

impl<S, N> FormatEvent<S, N> for LineFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, ctx: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> fmt::Result {
        // Events bridged from the `log` crate carry their real target, file and line as fields.
        let normalized = event.normalized_metadata();
        let metadata = normalized.as_ref().unwrap_or_else(|| event.metadata());
        let now = Local::now().naive_local();

        match self {
            LineFormat::File => {
                write!(
                    writer,
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:09} [{}] {:<5} ",
                    now.year(),
                    now.month(),
                    now.day(),
                    now.hour(),
                    now.minute(),
                    now.second(),
                    // A leap second carries its overflow in the nanoseconds.
                    now.nanosecond() % 1_000_000_000,
                    metadata.target(),
                    metadata.level(),
                )?;
                ctx.format_fields(writer.by_ref(), event)?;
                match (metadata.file(), metadata.line()) {
                    (Some(file), Some(line)) => writeln!(writer, " // {file}:{line}"),
                    _ => writeln!(writer),
                }
            },
            LineFormat::Console => {
                write!(writer, "{:02}:{:02} ", now.hour(), now.minute())?;
                let level = metadata.level();
                if writer.has_ansi_escapes() {
                    write!(writer, "\x1b[{}m{:<5}\x1b[0m ", ansi_colour(*level), level)?;
                } else {
                    write!(writer, "{level:<5} ")?;
                }
                ctx.format_fields(writer.by_ref(), event)?;
                writeln!(writer)
            },
        }
    }
}

fn ansi_colour(level: Level) -> &'static str {
    match level {
        Level::ERROR => "31",
        Level::WARN => "33",
        Level::INFO => "32",
        Level::DEBUG => "34",
        Level::TRACE => "35",
    }
}
