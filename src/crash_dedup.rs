//! Crash deduplication command entry point.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use libafl::inputs::Input;
use serde::Serialize;
use walkdir::WalkDir;

use crate::{
    configuration::Configuration,
    crash_identity::CrashIdentity,
    crash_replay::{replay_input, ObservedCrash},
    input::OpenApiInput,
    openapi::parse_api_spec,
};

/// Deduplicates crash files from `crash_directory` into `output_directory`.
pub fn dedup_crashes(crash_directory: &Path, output_directory: &Path) -> Result<()> {
    let config = Configuration::get().map_err(anyhow::Error::msg)?;
    crate::setup_logging(config);

    let api = parse_api_spec(config)?;
    let crash_files = get_crash_files(crash_directory)?;
    let mut clusters = BTreeMap::new();
    let mut non_reproducible = Vec::new();
    let mut skipped = Vec::new();

    for crash_file in &crash_files {
        let source_path = relative_string(crash_directory, crash_file);
        let input = match OpenApiInput::from_file(crash_file) {
            Ok(input) => input,
            Err(error) => {
                skipped.push(CrashFileResult {
                    path: source_path,
                    reason: format!("Could not read crash input: {error}"),
                });
                continue;
            }
        };

        match replay_input(&input, &api, config) {
            Ok(Some(observed_crash)) => {
                add_observed_crash_to_clusters(&mut clusters, source_path, observed_crash);
            }
            Ok(None) => {
                non_reproducible.push(CrashFileResult {
                    path: source_path,
                    reason: String::from("Replay did not reproduce a crash"),
                });
            }
            Err(error) => {
                skipped.push(CrashFileResult {
                    path: source_path,
                    reason: format!("Could not replay crash input: {error}"),
                });
            }
        }
    }

    write_report(
        output_directory,
        DedupReport::new(crash_files.len(), clusters, non_reproducible, skipped),
    )
}

#[derive(Debug, Serialize)]
struct DedupReport {
    summary: DedupSummary,
    clusters: Vec<CrashCluster>,
    non_reproducible: Vec<CrashFileResult>,
    skipped: Vec<CrashFileResult>,
}

impl DedupReport {
    fn new(
        total_files: usize,
        clusters: BTreeMap<String, CrashCluster>,
        non_reproducible: Vec<CrashFileResult>,
        skipped: Vec<CrashFileResult>,
    ) -> Self {
        let clusters: Vec<_> = clusters.into_values().collect();
        let reproduced = clusters.iter().map(|cluster| cluster.member_count).sum();
        let unique_clusters = clusters.len();
        Self {
            summary: DedupSummary {
                total_files,
                reproduced,
                unique_clusters,
                non_reproducible: non_reproducible.len(),
                skipped: skipped.len(),
            },
            clusters,
            non_reproducible,
            skipped,
        }
    }
}

#[derive(Debug, Serialize)]
struct DedupSummary {
    total_files: usize,
    reproduced: usize,
    unique_clusters: usize,
    non_reproducible: usize,
    skipped: usize,
}

#[derive(Debug, Serialize)]
struct CrashCluster {
    key: String,
    representative: String,
    members: Vec<String>,
    member_count: usize,
    representative_crashing_request_index: usize,
    identity: SerializableCrashIdentity,
}

#[derive(Debug, Serialize)]
struct SerializableCrashIdentity {
    exit_kind: String,
    crash_kind: String,
    http_status: Option<u16>,
    validation_error_discriminant: Option<String>,
    endpoint: Option<String>,
    response_class: String,
}

impl From<&CrashIdentity> for SerializableCrashIdentity {
    fn from(identity: &CrashIdentity) -> Self {
        Self {
            exit_kind: identity.exit_kind.to_string(),
            crash_kind: identity.crash_kind.to_string(),
            http_status: identity.http_status,
            validation_error_discriminant: identity.validation_error_discriminant.clone(),
            endpoint: identity.endpoint.clone(),
            response_class: identity.response_class.to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
struct CrashFileResult {
    path: String,
    reason: String,
}

fn get_crash_files(crash_directory: &Path) -> Result<Vec<PathBuf>> {
    if !crash_directory.exists() {
        return Err(anyhow!(
            "Crash directory {} does not exist",
            crash_directory.display()
        ));
    }
    if !crash_directory.is_dir() {
        return Err(anyhow!(
            "Crash path {} is not a directory",
            crash_directory.display()
        ));
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(crash_directory).min_depth(1) {
        let entry = entry.with_context(|| {
            format!(
                "Walking crash directory {}",
                crash_directory.to_string_lossy()
            )
        })?;

        if entry.file_type().is_file() && is_crash_input_file(entry.path()) {
            files.push(entry.path().to_path_buf());
        }
    }

    files.sort();
    Ok(files)
}

fn is_crash_input_file(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|file_name| file_name.to_str()) else {
        return false;
    };

    !file_name.starts_with('.') && !file_name.ends_with(".metadata")
}

fn add_observed_crash_to_clusters(
    clusters: &mut BTreeMap<String, CrashCluster>,
    source_path: String,
    observed_crash: ObservedCrash,
) {
    let key = observed_crash.identity.cluster_key();
    match clusters.get_mut(&key) {
        Some(cluster) => {
            cluster.members.push(source_path);
            cluster.member_count = cluster.members.len();
        }
        None => {
            clusters.insert(
                key.clone(),
                CrashCluster {
                    key,
                    representative: source_path.clone(),
                    members: vec![source_path],
                    member_count: 1,
                    representative_crashing_request_index: observed_crash.crashing_request_index,
                    identity: SerializableCrashIdentity::from(&observed_crash.identity),
                },
            );
        }
    }
}

fn write_report(output_directory: &Path, report: DedupReport) -> Result<()> {
    fs::create_dir_all(output_directory).with_context(|| {
        format!(
            "Creating dedup output directory {}",
            output_directory.display()
        )
    })?;

    let report_path = output_directory.join("clusters.json");
    let report_file = fs::File::create(&report_path)
        .with_context(|| format!("Creating dedup report {}", report_path.display()))?;
    serde_json::to_writer_pretty(report_file, &report)
        .with_context(|| format!("Writing dedup report {}", report_path.display()))?;

    Ok(())
}

fn relative_string(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::crash_identity::{CrashKind, ObservedExitKind, ResponseClass};

    #[test]
    fn get_crash_files_ignores_metadata_and_hidden_files() -> Result<()> {
        let temp_dir = TempDir::new()?;
        fs::write(temp_dir.path().join("crash-a"), b"")?;
        fs::write(temp_dir.path().join("crash-b.metadata"), b"")?;
        fs::write(temp_dir.path().join(".hidden"), b"")?;

        let files = get_crash_files(temp_dir.path())?;

        assert_eq!(files, vec![temp_dir.path().join("crash-a")]);
        Ok(())
    }

    #[test]
    fn get_crash_files_recurses_and_sorts_results() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let nested = temp_dir.path().join("nested");
        fs::create_dir(&nested)?;
        fs::write(temp_dir.path().join("z-crash"), b"")?;
        fs::write(nested.join("a-crash"), b"")?;

        let files = get_crash_files(temp_dir.path())?;

        assert_eq!(
            files,
            vec![nested.join("a-crash"), temp_dir.path().join("z-crash")]
        );
        Ok(())
    }

    #[test]
    fn add_observed_crash_groups_by_cluster_key() {
        let mut clusters = BTreeMap::new();
        let first = observed_crash("GET /items");
        let duplicate = observed_crash("GET /items");

        add_observed_crash_to_clusters(&mut clusters, String::from("first"), first);
        add_observed_crash_to_clusters(&mut clusters, String::from("second"), duplicate);

        assert_eq!(clusters.len(), 1);
        let cluster = clusters.values().next().unwrap();
        assert_eq!(cluster.representative, "first");
        assert_eq!(cluster.member_count, 2);
        assert_eq!(cluster.members, vec!["first", "second"]);
    }

    #[test]
    fn add_observed_crash_splits_distinct_cluster_keys() {
        let mut clusters = BTreeMap::new();

        add_observed_crash_to_clusters(
            &mut clusters,
            String::from("first"),
            observed_crash("GET /items"),
        );
        add_observed_crash_to_clusters(
            &mut clusters,
            String::from("second"),
            observed_crash("POST /items"),
        );

        assert_eq!(clusters.len(), 2);
    }

    fn observed_crash(endpoint: &str) -> ObservedCrash {
        ObservedCrash {
            identity: CrashIdentity {
                exit_kind: ObservedExitKind::Crash,
                crash_kind: CrashKind::Http5xx,
                http_status: Some(500),
                validation_error_discriminant: None,
                endpoint: Some(endpoint.to_string()),
                response_class: ResponseClass::Json,
            },
            crashing_request_index: 0,
        }
    }
}
