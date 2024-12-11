// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2023 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Logic for setting up a `tracing` subscriber according to our preferences
//! and [OpenTelemetry conventions](https://opentelemetry.io/docs/reference/specification/logs/).

use tracing::error;
use tracing_core::{Event, Level, Subscriber};
use tracing_log::NormalizeEvent;
use tracing_subscriber::{
    fmt::{format::Writer, time::FormatTime, FmtContext, FormatFields, FormattedFields},
    layer::SubscriberExt,
    registry::LookupSpan,
    Layer,
};

struct FormatSystemd;

struct ChronoTimer;

impl FormatTime for ChronoTimer {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        const TIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.6f";
        write!(w, "{}", chrono::Local::now().format(TIME_FORMAT))
    }
}

fn systemd_prefix(level: Level) -> &'static str {
    if level >= Level::TRACE {
        "<7>" // SD_DEBUG
    } else if level >= Level::DEBUG {
        "<6>" // SD_INFO
    } else if level >= Level::INFO {
        "<5>" // SD_NOTICE
    } else if level >= Level::WARN {
        "<4>" // SD_WARN
    } else {
        "<3>" // SD_ERROR
    }
}

impl<S, N> tracing_subscriber::fmt::FormatEvent<S, N> for FormatSystemd
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let normalized_meta = event.normalized_metadata();
        let meta = normalized_meta.as_ref().unwrap_or_else(|| event.metadata());
        let prefix = systemd_prefix(*meta.level());

        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("unnamed-thread");
        write!(writer, "{prefix}{thread_name} ")?;
        if let Some(scope) = ctx.event_scope() {
            let mut seen = false;

            for span in scope.from_root() {
                write!(writer, "{}", span.metadata().name())?;
                seen = true;

                let ext = span.extensions();
                if let Some(fields) = &ext.get::<FormattedFields<N>>() {
                    if !fields.is_empty() {
                        write!(writer, "{{{fields}}}")?;
                    }
                }
                writer.write_char(':')?;
            }

            if seen {
                writer.write_char(' ')?;
            }
        }

        write!(writer, "{}: ", meta.target())?;
        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// Custom panic hook that logs instead of directly writing to stderr.
///
/// This means it includes a timestamp, follows [OpenTelemetry Semantic
/// Conventions for Exceptions](https://opentelemetry.io/docs/reference/specification/logs/semantic_conventions/exceptions/),
/// etc.
fn panic_hook(p: &std::panic::PanicHookInfo) {
    let payload: Option<&str> = if let Some(s) = p.payload().downcast_ref::<&str>() {
        Some(*s)
    } else if let Some(s) = p.payload().downcast_ref::<String>() {
        Some(s)
    } else {
        None
    };
    error!(
        target: std::env!("CARGO_CRATE_NAME"),
        location = p.location().map(tracing::field::display),
        payload = payload.map(tracing::field::display),
        backtrace = %std::backtrace::Backtrace::force_capture(),
        "panic",
    );
}

pub fn install() {
    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
        .with_env_var("MOONFIRE_LOG")
        .from_env_lossy();
    tracing_log::LogTracer::init().unwrap();

    match std::env::var("MOONFIRE_FORMAT") {
        Ok(s) if s == "systemd" => {
            let sub = tracing_subscriber::registry().with(
                tracing_subscriber::fmt::Layer::new()
                    .with_writer(std::io::stderr)
                    .with_ansi(false)
                    .event_format(FormatSystemd)
                    .with_filter(filter),
            );
            tracing::subscriber::set_global_default(sub).unwrap();
        }
        Ok(s) if s == "json" => {
            let sub = tracing_subscriber::registry().with(
                tracing_subscriber::fmt::Layer::new()
                    .with_writer(std::io::stderr)
                    .with_thread_names(true)
                    .json()
                    .with_filter(filter),
            );
            tracing::subscriber::set_global_default(sub).unwrap();
        }
        _ => {
            let sub = tracing_subscriber::registry().with(
                tracing_subscriber::fmt::Layer::new()
                    .with_writer(std::io::stderr)
                    .with_timer(ChronoTimer)
                    .with_thread_names(true)
                    .with_filter(filter),
            );
            tracing::subscriber::set_global_default(sub).unwrap();
        }
    }

    let use_panic_hook = ::std::env::var("MOONFIRE_PANIC_HOOK")
        .map(|s| s != "false" && s != "0")
        .unwrap_or(true);
    if use_panic_hook {
        std::panic::set_hook(Box::new(&panic_hook));
    }
}

pub fn install_for_tests() {
    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
        .with_env_var("MOONFIRE_LOG")
        .from_env_lossy();
    tracing_log::LogTracer::init().unwrap();
    let sub = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::Layer::new()
            .with_test_writer()
            .with_timer(ChronoTimer)
            .with_thread_names(true)
            .with_filter(filter),
    );
    tracing::subscriber::set_global_default(sub).unwrap();
}
