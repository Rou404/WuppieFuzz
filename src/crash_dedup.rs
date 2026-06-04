//! Crash deduplication command entry point.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use libafl::inputs::Input;
use serde::Serialize;
use walkdir::WalkDir;

use crate::{
    configuration::Configuration,
    crash_identity::CrashIdentity,
    crash_replay::{ObservedCrash, replay_input},
    input::OpenApiInput,
    openapi::parse_api_spec,
};

const DEDUP_PROGRESS_INTERVAL: usize = 100;

/// Deduplicates crash files from `crash_directory` into `output_directory`.
pub fn dedup_crashes(crash_directory: &Path, output_directory: &Path, force: bool) -> Result<()> {
    let output_directory = prepare_output_directory(crash_directory, output_directory, force)?;
    let config = Configuration::get().map_err(anyhow::Error::msg)?;
    crate::setup_logging(config);

    let api = parse_api_spec(config)?;
    let crash_files = get_crash_files(crash_directory)?;
    let mut clusters = BTreeMap::new();
    let mut non_reproducible = Vec::new();
    let mut skipped = Vec::new();

    for (index, crash_file) in crash_files.iter().enumerate() {
        let source_path = relative_string(crash_directory, crash_file);
        match OpenApiInput::from_file(crash_file) {
            Ok(input) => match replay_input(&input, &api, config) {
                Ok(Some(observed_crash)) => match fs::metadata(crash_file) {
                    Ok(metadata) => {
                        add_observed_crash_to_clusters(
                            &mut clusters,
                            crash_file,
                            source_path,
                            metadata.len(),
                            observed_crash,
                        );
                    }
                    Err(error) => {
                        skipped.push(CrashFileResult {
                            path: source_path,
                            reason: format!("Could not read crash input metadata: {error}"),
                        });
                    }
                },
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
            },
            Err(error) => {
                skipped.push(CrashFileResult {
                    path: source_path,
                    reason: format!("Could not read crash input: {error}"),
                });
            }
        };

        log_progress(
            index + 1,
            crash_files.len(),
            clusters.len(),
            non_reproducible.len(),
            skipped.len(),
        );
    }

    copy_unique_representatives(&output_directory, &mut clusters)?;

    let report = DedupReport::new(crash_files.len(), clusters, non_reproducible, skipped);
    write_report(&output_directory, &report)?;
    log_summary(&report.summary, &output_directory);
    Ok(())
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
    #[serde(skip)]
    representative_source: PathBuf,
    #[serde(skip)]
    representative_size: u64,
    representative: String,
    members: Vec<String>,
    member_count: usize,
    oracle_crash_ids: Vec<String>,
    oracle_mixed: bool,
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

fn prepare_output_directory(
    crash_directory: &Path,
    output_directory: &Path,
    force: bool,
) -> Result<PathBuf> {
    ensure_crash_directory(crash_directory)?;

    let crash_directory = std::path::absolute(crash_directory)?;
    let output_directory = std::path::absolute(output_directory)?;
    if output_directory.starts_with(&crash_directory) {
        return Err(anyhow!(
            "Output directory {} must not be inside or equal to crash directory {} because crash collection is recursive",
            output_directory.display(),
            crash_directory.display()
        ));
    }
    if output_directory.exists() && !output_directory.is_dir() {
        return Err(anyhow!(
            "Output path {} exists but is not a directory",
            output_directory.display()
        ));
    }
    if output_directory.exists() && !force && !is_empty_directory(&output_directory)? {
        return Err(anyhow!(
            "Output directory {} already exists and is not empty; use --force to overwrite previous dedup output",
            output_directory.display()
        ));
    }

    fs::create_dir_all(&output_directory)?;
    if force {
        remove_if_exists(&output_directory.join("clusters.json"))?;
        remove_if_exists(&output_directory.join("unique"))?;
    }

    Ok(output_directory)
}

fn is_empty_directory(directory: &Path) -> Result<bool> {
    let mut entries = fs::read_dir(directory)?;
    Ok(entries.next().transpose()?.is_none())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn get_crash_files(crash_directory: &Path) -> Result<Vec<PathBuf>> {
    ensure_crash_directory(crash_directory)?;

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

fn ensure_crash_directory(crash_directory: &Path) -> Result<()> {
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

    Ok(())
}

fn is_crash_input_file(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|file_name| file_name.to_str()) else {
        return false;
    };

    !file_name.starts_with('.') && !file_name.ends_with(".metadata")
}

fn add_observed_crash_to_clusters(
    clusters: &mut BTreeMap<String, CrashCluster>,
    source_file: &Path,
    source_path: String,
    source_size: u64,
    observed_crash: ObservedCrash,
) {
    let key = observed_crash.identity.cluster_key();
    match clusters.get_mut(&key) {
        Some(cluster) => {
            cluster.members.push(source_path.clone());
            cluster.member_count = cluster.members.len();
            record_oracle_crash_id(cluster, observed_crash.oracle_crash_id.as_deref());
            if source_size < cluster.representative_size {
                cluster.representative_source = source_file.to_path_buf();
                cluster.representative_size = source_size;
                cluster.representative = source_path;
                cluster.representative_crashing_request_index =
                    observed_crash.crashing_request_index;
            }
        }
        None => {
            let mut cluster = CrashCluster {
                key: key.clone(),
                representative_source: source_file.to_path_buf(),
                representative_size: source_size,
                representative: source_path.clone(),
                members: vec![source_path],
                member_count: 1,
                oracle_crash_ids: Vec::new(),
                oracle_mixed: false,
                representative_crashing_request_index: observed_crash.crashing_request_index,
                identity: SerializableCrashIdentity::from(&observed_crash.identity),
            };
            record_oracle_crash_id(&mut cluster, observed_crash.oracle_crash_id.as_deref());
            clusters.insert(key, cluster);
        }
    }
}

fn record_oracle_crash_id(cluster: &mut CrashCluster, oracle_crash_id: Option<&str>) {
    let Some(oracle_crash_id) = oracle_crash_id
        .map(str::trim)
        .filter(|oracle_crash_id| !oracle_crash_id.is_empty())
    else {
        return;
    };

    if !cluster
        .oracle_crash_ids
        .iter()
        .any(|existing| existing == oracle_crash_id)
    {
        cluster.oracle_crash_ids.push(oracle_crash_id.to_owned());
        cluster.oracle_crash_ids.sort();
    }
    cluster.oracle_mixed = cluster.oracle_crash_ids.len() > 1;
}

fn copy_unique_representatives(
    output_directory: &Path,
    clusters: &mut BTreeMap<String, CrashCluster>,
) -> Result<()> {
    let unique_directory = output_directory.join("unique");
    fs::create_dir_all(&unique_directory).with_context(|| {
        format!(
            "Creating dedup unique crash directory {}",
            unique_directory.display()
        )
    })?;

    for (index, cluster) in clusters.values_mut().enumerate() {
        let destination =
            unique_directory.join(unique_file_name(index, &cluster.representative_source));
        fs::copy(&cluster.representative_source, &destination).with_context(|| {
            format!(
                "Copying representative crash {} to {}",
                cluster.representative_source.display(),
                destination.display()
            )
        })?;
        cluster.representative = relative_string(output_directory, &destination);
    }

    Ok(())
}

fn unique_file_name(index: usize, source_path: &Path) -> String {
    let file_name = source_path
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .unwrap_or("crash");
    format!("{index:06}_{file_name}")
}

fn write_report(output_directory: &Path, report: &DedupReport) -> Result<()> {
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

fn log_progress(
    processed: usize,
    total: usize,
    unique_clusters: usize,
    non_reproducible: usize,
    skipped: usize,
) {
    if should_log_progress(processed, total) {
        log::info!(
            "Dedup progress: {processed}/{total} crash files processed, {unique_clusters} unique clusters, {non_reproducible} non-reproducible, {skipped} skipped"
        );
    }
}

fn should_log_progress(processed: usize, total: usize) -> bool {
    processed < total && processed % DEDUP_PROGRESS_INTERVAL == 0
}

fn log_summary(summary: &DedupSummary, output_directory: &Path) {
    log::info!(
        "Dedup processed {} crash files: {} reproduced, {} unique clusters, {} non-reproducible, {} skipped. Output: {}",
        summary.total_files,
        summary.reproduced,
        summary.unique_clusters,
        summary.non_reproducible,
        summary.skipped,
        output_directory.display()
    );
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
    fn prepare_output_directory_rejects_unsafe_outputs() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let crash_dir = temp_dir.path().join("crashes");
        let output_dir = temp_dir.path().join("dedup");
        fs::create_dir(&crash_dir)?;
        fs::create_dir(&output_dir)?;
        fs::write(output_dir.join("old-file"), b"old")?;

        let nested_error =
            prepare_output_directory(&crash_dir, &crash_dir.join("dedup"), false).unwrap_err();
        assert_error_contains(
            nested_error,
            "must not be inside or equal to crash directory",
        );

        let non_empty_error = prepare_output_directory(&crash_dir, &output_dir, false).unwrap_err();
        assert_error_contains(non_empty_error, "already exists and is not empty");
        Ok(())
    }

    #[test]
    fn prepare_output_directory_force_removes_stale_outputs() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let crash_dir = temp_dir.path().join("crashes");
        let output_dir = temp_dir.path().join("dedup");
        fs::create_dir(&crash_dir)?;
        fs::create_dir(&output_dir)?;
        fs::write(output_dir.join("clusters.json"), b"old report")?;
        fs::create_dir(output_dir.join("unique"))?;
        fs::write(output_dir.join("unique/stale-crash"), b"old crash")?;
        fs::write(output_dir.join("notes.txt"), b"keep")?;

        let prepared = prepare_output_directory(&crash_dir, &output_dir, true)?;

        assert_eq!(prepared, output_dir);
        assert!(!prepared.join("clusters.json").exists());
        assert!(!prepared.join("unique").exists());
        assert!(prepared.join("notes.txt").exists());
        assert!(prepared.is_dir());
        Ok(())
    }

    #[test]
    fn add_observed_crash_groups_by_cluster_key() {
        let mut clusters = BTreeMap::new();
        let first = observed_crash_with_oracle("GET /items", "BUG-001");
        let duplicate = observed_crash_with_oracle("GET /items", "BUG-002");

        add_observed_crash_to_clusters(
            &mut clusters,
            Path::new("first"),
            String::from("first"),
            10,
            first,
        );
        add_observed_crash_to_clusters(
            &mut clusters,
            Path::new("second"),
            String::from("second"),
            10,
            duplicate,
        );

        assert_eq!(clusters.len(), 1);
        let cluster = clusters.values().next().unwrap();
        assert_eq!(cluster.representative, "first");
        assert_eq!(cluster.member_count, 2);
        assert_eq!(cluster.members, vec!["first", "second"]);
        assert_eq!(cluster.oracle_crash_ids, vec!["BUG-001", "BUG-002"]);
        assert!(cluster.oracle_mixed);
    }

    #[test]
    fn add_observed_crash_prefers_smaller_representative() {
        let mut clusters = BTreeMap::new();
        let first = observed_crash_with_index("GET /items", 2);
        let smaller = observed_crash_with_index("GET /items", 1);

        add_observed_crash_to_clusters(
            &mut clusters,
            Path::new("larger"),
            String::from("larger"),
            100,
            first,
        );
        add_observed_crash_to_clusters(
            &mut clusters,
            Path::new("smaller"),
            String::from("smaller"),
            10,
            smaller,
        );

        let cluster = clusters.values().next().unwrap();
        assert_eq!(cluster.representative, "smaller");
        assert_eq!(cluster.representative_source, PathBuf::from("smaller"));
        assert_eq!(cluster.representative_size, 10);
        assert_eq!(cluster.representative_crashing_request_index, 1);
        assert_eq!(cluster.members, vec!["larger", "smaller"]);
    }

    #[test]
    fn add_observed_crash_splits_distinct_cluster_keys() {
        let mut clusters = BTreeMap::new();

        add_observed_crash_to_clusters(
            &mut clusters,
            Path::new("first"),
            String::from("first"),
            10,
            observed_crash("GET /items"),
        );
        add_observed_crash_to_clusters(
            &mut clusters,
            Path::new("second"),
            String::from("second"),
            10,
            observed_crash("POST /items"),
        );

        assert_eq!(clusters.len(), 2);
    }

    #[test]
    fn copy_unique_representatives_copies_files_and_updates_representative_paths() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let crash_dir = temp_dir.path().join("crashes");
        let output_dir = temp_dir.path().join("dedup");
        fs::create_dir(&crash_dir)?;
        let first_crash = crash_dir.join("same-name");
        let nested_dir = crash_dir.join("nested");
        fs::create_dir(&nested_dir)?;
        let second_crash = nested_dir.join("same-name");
        fs::write(&first_crash, b"first")?;
        fs::write(&second_crash, b"second")?;

        let mut clusters = BTreeMap::new();
        add_observed_crash_to_clusters(
            &mut clusters,
            &first_crash,
            String::from("same-name"),
            first_crash.metadata()?.len(),
            observed_crash("GET /items"),
        );
        add_observed_crash_to_clusters(
            &mut clusters,
            &second_crash,
            String::from("nested/same-name"),
            second_crash.metadata()?.len(),
            observed_crash("POST /items"),
        );

        copy_unique_representatives(&output_dir, &mut clusters)?;

        let representatives: Vec<_> = clusters
            .values()
            .map(|cluster| cluster.representative.as_str())
            .collect();
        assert_eq!(
            representatives,
            vec!["unique/000000_same-name", "unique/000001_same-name"]
        );
        assert_eq!(
            fs::read(output_dir.join("unique/000000_same-name"))?,
            b"first"
        );
        assert_eq!(
            fs::read(output_dir.join("unique/000001_same-name"))?,
            b"second"
        );

        Ok(())
    }

    fn observed_crash(endpoint: &str) -> ObservedCrash {
        observed_crash_with_index(endpoint, 0)
    }

    fn observed_crash_with_oracle(endpoint: &str, oracle_crash_id: &str) -> ObservedCrash {
        ObservedCrash {
            oracle_crash_id: Some(oracle_crash_id.to_string()),
            ..observed_crash_with_index(endpoint, 0)
        }
    }

    fn observed_crash_with_index(endpoint: &str, crashing_request_index: usize) -> ObservedCrash {
        ObservedCrash {
            identity: CrashIdentity {
                exit_kind: ObservedExitKind::Crash,
                crash_kind: CrashKind::Http5xx,
                http_status: Some(500),
                validation_error_discriminant: None,
                endpoint: Some(endpoint.to_string()),
                response_class: ResponseClass::Json,
            },
            crashing_request_index,
            oracle_crash_id: None,
        }
    }

    fn assert_error_contains(error: anyhow::Error, expected: &str) {
        assert!(error.to_string().contains(expected));
    }
}
