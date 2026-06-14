# Crash Deduplication Oracle Benchmark

This benchmark runs WuppieFuzz against a synthetic FastAPI service whose
intentional failures include an `X-Benchmark-Crash-ID` response header. The
header is used only to evaluate deduplication quality after clustering.

## Setup

From the repository root:

```bash
python3 -m venv benchmarks/oracle/.venv
benchmarks/oracle/.venv/bin/pip install -r benchmarks/oracle/requirements.txt
```

The runner also requires Cargo, `curl`, and `jq`.

## Run

```bash
benchmarks/oracle/run.sh --runs 5 --timeout 300
```

The runner starts and stops the FastAPI target for each fuzzing and replay
phase. Results are written below `benchmark_runs/oracle_<timestamp>/`.

Use `--help` to override the target, configuration, corpus, run count, timeout,
or output directory. Pass `--no-manage-target` when the target is managed
externally.

## Reports

Evaluate an existing deduplication result:

```bash
python3 benchmarks/oracle/evaluate_dedup_oracle.py crash_dedup/clusters.json
```

Create a human-readable cluster report:

```bash
python3 benchmarks/oracle/create_cluster_report.py crash_dedup/clusters.json
```
