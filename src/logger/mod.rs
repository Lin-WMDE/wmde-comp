// SPDX-License-Identifier: GPL-3.0-only

use std::str::FromStr;

use anyhow::Result;

use tracing::{debug, info, warn};
use tracing_journald as journald;
use tracing_subscriber::{EnvFilter, filter::Directive, fmt, prelude::*};

pub fn init_logger() -> Result<()> {
    let level = if cfg!(debug_assertions) {
        "debug"
    } else {
        "warn"
    };
    // The per-crate directives are DEFAULTS, and are only applied when RUST_LOG says
    // nothing. Appending them to a parsed RUST_LOG - which is what happened here - pins
    // smithay, calloop, cosmic_text and wmde_comp at those levels whatever the environment
    // asks for, so `RUST_LOG=smithay::backend::drm::compositor=trace` produced no smithay
    // output at all and looked like the events did not exist.
    let filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => EnvFilter::new(if cfg!(debug_assertions) { "info" } else { "warn" })
            .add_directive(Directive::from_str("cosmic_text=error").unwrap())
            .add_directive(Directive::from_str("calloop=error").unwrap())
            .add_directive(Directive::from_str(&format!("smithay={level}")).unwrap())
            .add_directive(Directive::from_str(&format!("wmde_comp={level}")).unwrap()),
    };

    let fmt_layer = fmt::layer().compact();

    match journald::layer() {
        Ok(journald_layer) => tracing_subscriber::registry()
            .with(fmt_layer)
            .with(journald_layer)
            .with(filter)
            .init(),
        Err(err) => {
            tracing_subscriber::registry()
                .with(fmt_layer)
                .with(filter)
                .init();
            warn!(?err, "Failed to init journald logging.");
        }
    };
    log_panics::init();

    info!("Version: {}", std::env!("CARGO_PKG_VERSION"));
    if cfg!(feature = "debug") {
        debug!(
            "Debug build ({})",
            std::option_env!("GIT_HASH").unwrap_or("Unknown")
        );
    }

    Ok(())
}
