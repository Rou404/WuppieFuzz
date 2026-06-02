//! Crash deduplication command entry point.

use std::path::Path;

use anyhow::{Result, bail};

use crate::configuration::Configuration;

/// Deduplicates crash files from `crash_directory` into `output_directory`.
pub fn dedup_crashes(crash_directory: &Path, output_directory: &Path) -> Result<()> {
    let config = Configuration::get().map_err(anyhow::Error::msg)?;
    crate::setup_logging(config);

    bail!(
        "Crash deduplication is not implemented yet (crash directory: {}, output directory: {})",
        crash_directory.display(),
        output_directory.display()
    )
}
