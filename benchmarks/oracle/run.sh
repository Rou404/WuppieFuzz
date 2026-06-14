#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  cat <<'USAGE'
Run repeated WuppieFuzz oracle deduplication benchmark campaigns.

Usage:
  benchmarks/oracle/run.sh [options]

Options:
  --runs N                 Number of runs to execute. Default: 5
  --timeout SECONDS        Fuzzing timeout per run. Default: 300
  --output-dir DIR         Directory for benchmark outputs. Default: benchmark_runs/oracle_<timestamp>
  --config FILE            WuppieFuzz config file. Default: benchmarks/oracle/config.yaml
  --openapi FILE           OpenAPI specification. Default: benchmarks/oracle/openapi.json
  --initial-corpus DIR     Initial corpus directory. Default: benchmarks/oracle/initial_corpus
  --crash-criteria NAME    Crash criterion passed to fuzz. Default: Status5xxNotSpecified
  --coverage-format NAME   Coverage format passed to fuzz. Default: lcov
  --coverage-host HOST     Coverage host passed to fuzz. Default: 0.0.0.0:3001
  --profile dev|release    Cargo profile. Default: release
  --target URL             Override target URL for fuzz and dedup.
  --target-dir DIR         Directory to start FastAPI from. Default: benchmarks/oracle
  --target-app APP         Uvicorn app module. Default: app:app
  --target-host HOST       FastAPI bind host. Default: 0.0.0.0
  --target-port PORT       FastAPI port. Default: 8080
  --health-url URL         URL used to wait for target startup. Default: http://127.0.0.1:8080/openapi.json
  --coverage-rc FILE       COVERAGE_PROCESS_START file. Default: benchmarks/oracle/.coveragerc
  --venv DIR               Optional virtualenv to activate before starting FastAPI.
  --no-manage-target       Do not start/stop FastAPI between runs.
  --keep-queue             Preserve queue/ in each run directory. Default: false
  --no-clean-first         Do not remove existing root crashes/queue/crash_dedup/results.json before run 1.
  --help                   Show this help.

Examples:
  # Five one-hour runs, suitable for about five hours of downtime:
  benchmarks/oracle/run.sh --runs 5 --timeout 3600

  # Ten 30-minute runs:
  benchmarks/oracle/run.sh --runs 10 --timeout 1800
USAGE
}

repo_root() {
  local script_dir
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  cd "${script_dir}/../.." && pwd
}

require_file() {
  local path="$1"
  if [[ ! -f "${path}" ]]; then
    echo "error: required file does not exist: ${path}" >&2
    exit 1
  fi
}

require_dir() {
  local path="$1"
  if [[ ! -d "${path}" ]]; then
    echo "error: required directory does not exist: ${path}" >&2
    exit 1
  fi
}

abs_path() {
  local path="$1"
  if [[ -d "${path}" ]]; then
    (cd "${path}" && pwd)
  else
    local dir
    dir="$(dirname "${path}")"
    printf '%s/%s\n' "$(cd "${dir}" && pwd)" "$(basename "${path}")"
  fi
}

run_and_log() {
  local log_file="$1"
  shift
  echo "+ $*" | tee "${log_file}"
  "$@" 2>&1 | tee -a "${log_file}"
}

json_metric() {
  local file="$1"
  local key="$2"
  jq -r --arg key "${key}" '.metrics[$key] // ""' "${file}"
}

wait_for_target() {
  local health_url="$1"
  local pid="$2"
  local log_file="$3"
  local deadline=$((SECONDS + 30))

  until curl --silent --fail --max-time 2 "${health_url}" >/dev/null 2>&1; do
    if ! kill -0 "${pid}" >/dev/null 2>&1; then
      echo "error: FastAPI target exited before becoming healthy" >&2
      echo "last target log lines:" >&2
      tail -60 "${log_file}" >&2 || true
      exit 1
    fi
    if [[ "${SECONDS}" -ge "${deadline}" ]]; then
      echo "error: FastAPI target did not become healthy at ${health_url}" >&2
      echo "last target log lines:" >&2
      tail -60 "${log_file}" >&2 || true
      exit 1
    fi
    sleep 1
  done
}

stop_target() {
  local pid="${TARGET_PID:-}"
  TARGET_PID=""
  if [[ -z "${pid}" ]]; then
    return 0
  fi
  if kill -0 "${pid}" >/dev/null 2>&1; then
    kill "${pid}" >/dev/null 2>&1 || true
    for _ in $(seq 1 10); do
      if ! kill -0 "${pid}" >/dev/null 2>&1; then
        wait "${pid}" 2>/dev/null || true
        return 0
      fi
      sleep 1
    done
    kill -9 "${pid}" >/dev/null 2>&1 || true
    wait "${pid}" 2>/dev/null || true
  fi
}

RUNS=5
TIMEOUT=300
OUTPUT_DIR=""
BENCHMARK_DIR="benchmarks/oracle"
CONFIG="${BENCHMARK_DIR}/config.yaml"
OPENAPI="${BENCHMARK_DIR}/openapi.json"
INITIAL_CORPUS="${BENCHMARK_DIR}/initial_corpus"
CRASH_CRITERIA="Status5xxNotSpecified"
COVERAGE_FORMAT="lcov"
COVERAGE_HOST="0.0.0.0:3001"
PROFILE="release"
TARGET=""
MANAGE_TARGET=1
TARGET_DIR="${BENCHMARK_DIR}"
TARGET_APP="app:app"
TARGET_HOST="0.0.0.0"
TARGET_PORT="8080"
HEALTH_URL="http://127.0.0.1:8080/openapi.json"
COVERAGE_RC="${BENCHMARK_DIR}/.coveragerc"
VENV_DIR=""
KEEP_QUEUE=0
CLEAN_FIRST=1
TARGET_PID=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --runs)
      RUNS="$2"
      shift 2
      ;;
    --timeout)
      TIMEOUT="$2"
      shift 2
      ;;
    --output-dir)
      OUTPUT_DIR="$2"
      shift 2
      ;;
    --config)
      CONFIG="$2"
      shift 2
      ;;
    --openapi)
      OPENAPI="$2"
      shift 2
      ;;
    --initial-corpus)
      INITIAL_CORPUS="$2"
      shift 2
      ;;
    --crash-criteria)
      CRASH_CRITERIA="$2"
      shift 2
      ;;
    --coverage-format)
      COVERAGE_FORMAT="$2"
      shift 2
      ;;
    --coverage-host)
      COVERAGE_HOST="$2"
      shift 2
      ;;
    --profile)
      PROFILE="$2"
      shift 2
      ;;
    --target)
      TARGET="$2"
      shift 2
      ;;
    --target-dir)
      TARGET_DIR="$2"
      shift 2
      ;;
    --target-app)
      TARGET_APP="$2"
      shift 2
      ;;
    --target-host)
      TARGET_HOST="$2"
      shift 2
      ;;
    --target-port)
      TARGET_PORT="$2"
      shift 2
      ;;
    --health-url)
      HEALTH_URL="$2"
      shift 2
      ;;
    --coverage-rc)
      COVERAGE_RC="$2"
      shift 2
      ;;
    --venv)
      VENV_DIR="$2"
      shift 2
      ;;
    --no-manage-target)
      MANAGE_TARGET=0
      shift
      ;;
    --keep-queue)
      KEEP_QUEUE=1
      shift
      ;;
    --no-clean-first)
      CLEAN_FIRST=0
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if ! [[ "${RUNS}" =~ ^[0-9]+$ ]] || [[ "${RUNS}" -lt 1 ]]; then
  echo "error: --runs must be a positive integer" >&2
  exit 1
fi

if ! [[ "${TIMEOUT}" =~ ^[0-9]+$ ]] || [[ "${TIMEOUT}" -lt 1 ]]; then
  echo "error: --timeout must be a positive integer" >&2
  exit 1
fi

case "${PROFILE}" in
  dev|release) ;;
  *)
    echo "error: --profile must be 'dev' or 'release'" >&2
    exit 1
    ;;
esac

cd "$(repo_root)"

if [[ "${MANAGE_TARGET}" -eq 1 && -z "${VENV_DIR}" ]]; then
  if [[ -f "${BENCHMARK_DIR}/venv/bin/activate" ]]; then
    VENV_DIR="${BENCHMARK_DIR}/venv"
  elif [[ -f "${BENCHMARK_DIR}/.venv/bin/activate" ]]; then
    VENV_DIR="${BENCHMARK_DIR}/.venv"
  fi
fi

require_file "${CONFIG}"
require_file "${OPENAPI}"
require_dir "${INITIAL_CORPUS}"
require_file "${BENCHMARK_DIR}/evaluate_dedup_oracle.py"
if [[ "${MANAGE_TARGET}" -eq 1 ]]; then
  require_dir "${TARGET_DIR}"
  require_file "${COVERAGE_RC}"
  if [[ -n "${VENV_DIR}" ]]; then
    require_file "${VENV_DIR}/bin/activate"
  fi
  if ! command -v curl >/dev/null 2>&1; then
    echo "error: curl is required for target health checks" >&2
    exit 1
  fi
fi

if ! grep -Eq '^[[:space:]]*crash[_-]criteria:' "${CONFIG}"; then
  cat >&2 <<EOF_CONFIG_ERROR
error: ${CONFIG} does not define crash_criteria.

Dedup replay reads crash criteria from the config file. Add this to keep fuzz
and dedup aligned:

crash_criteria:
  - ${CRASH_CRITERIA}
EOF_CONFIG_ERROR
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required for benchmark summaries" >&2
  exit 1
fi

TARGET_DIR_ABS="$(abs_path "${TARGET_DIR}")"
COVERAGE_RC_ABS="$(abs_path "${COVERAGE_RC}")"
VENV_ACTIVATE_ABS=""
if [[ -n "${VENV_DIR}" ]]; then
  VENV_ACTIVATE_ABS="$(abs_path "${VENV_DIR}/bin/activate")"
fi

if [[ -z "${OUTPUT_DIR}" ]]; then
  OUTPUT_DIR="benchmark_runs/oracle_$(date +%Y%m%d_%H%M%S)"
fi
mkdir -p "${OUTPUT_DIR}"

CARGO_ARGS=(cargo run)
if [[ "${PROFILE}" == "release" ]]; then
  CARGO_ARGS+=(--release)
fi
CARGO_ARGS+=(--)

TARGET_ARGS=()
if [[ -n "${TARGET}" ]]; then
  TARGET_ARGS+=(--target "${TARGET}")
fi

summary_file="${OUTPUT_DIR}/summary.tsv"
cat > "${summary_file}" <<'SUMMARY'
run	total_files	reproduced	dedup_clusters	dedup_reduction	oracle_labeled_crashes	oracle_unlabeled_crashes	oracle_bug_ids_observed	false_merge_clusters	false_split_bug_ids	weighted_cluster_purity
SUMMARY

cat > "${OUTPUT_DIR}/benchmark_config.txt" <<EOF_CONFIG
runs=${RUNS}
timeout=${TIMEOUT}
config=${CONFIG}
openapi=${OPENAPI}
initial_corpus=${INITIAL_CORPUS}
crash_criteria=${CRASH_CRITERIA}
coverage_format=${COVERAGE_FORMAT}
coverage_host=${COVERAGE_HOST}
profile=${PROFILE}
target=${TARGET}
manage_target=${MANAGE_TARGET}
target_dir=${TARGET_DIR}
target_app=${TARGET_APP}
target_host=${TARGET_HOST}
target_port=${TARGET_PORT}
health_url=${HEALTH_URL}
coverage_rc=${COVERAGE_RC}
venv_dir=${VENV_DIR}
keep_queue=${KEEP_QUEUE}
started_at=$(date --iso-8601=seconds)
EOF_CONFIG

trap stop_target EXIT
trap 'stop_target; exit 130' INT TERM

if [[ "${CLEAN_FIRST}" -eq 1 ]]; then
  rm -rf crashes queue crash_dedup results.json
fi

for run_number in $(seq 1 "${RUNS}"); do
  run_name="run-$(printf '%03d' "${run_number}")"
  run_dir="${OUTPUT_DIR}/${run_name}"
  mkdir -p "${run_dir}"

  echo "== ${run_name}/${RUNS}: starting at $(date --iso-8601=seconds) =="
  rm -rf crashes queue crash_dedup results.json

  cp "${CONFIG}" "${run_dir}/config.yaml"
  cp "${OPENAPI}" "${run_dir}/openapi.json"

  if [[ "${MANAGE_TARGET}" -eq 1 ]]; then
    stop_target
    target_log="${run_dir}/target.log"
    echo "+ uvicorn ${TARGET_APP} --host ${TARGET_HOST} --port ${TARGET_PORT}" > "${target_log}"
    (
      cd "${TARGET_DIR_ABS}"
      if [[ -n "${VENV_DIR}" ]]; then
        # shellcheck disable=SC1090
        source "${VENV_ACTIVATE_ABS}"
      fi
      COVERAGE_PROCESS_START="${COVERAGE_RC_ABS}" \
        uvicorn "${TARGET_APP}" --host "${TARGET_HOST}" --port "${TARGET_PORT}" --log-level warning
    ) >> "${target_log}" 2>&1 &
    TARGET_PID=$!
    echo "${TARGET_PID}" > "${run_dir}/target.pid"
    wait_for_target "${HEALTH_URL}" "${TARGET_PID}" "${target_log}"
  fi

  fuzz_cmd=(
    "${CARGO_ARGS[@]}"
    fuzz
    --config "${CONFIG}"
    "${OPENAPI}"
    --timeout "${TIMEOUT}"
    --coverage-format "${COVERAGE_FORMAT}"
    --coverage-host "${COVERAGE_HOST}"
    --initial-corpus "${INITIAL_CORPUS}"
    --crash-criteria "${CRASH_CRITERIA}"
    --report
    "${TARGET_ARGS[@]}"
  )

  dedup_cmd=(
    "${CARGO_ARGS[@]}"
    dedup
    crashes
    --output crash_dedup
    --openapi-spec "${OPENAPI}"
    --config "${CONFIG}"
    --force
    "${TARGET_ARGS[@]}"
  )

  eval_cmd=(python3 "${BENCHMARK_DIR}/evaluate_dedup_oracle.py" crash_dedup/clusters.json)

  printf '%q ' "${fuzz_cmd[@]}" > "${run_dir}/fuzz_command.txt"
  printf '\n' >> "${run_dir}/fuzz_command.txt"
  printf '%q ' "${dedup_cmd[@]}" > "${run_dir}/dedup_command.txt"
  printf '\n' >> "${run_dir}/dedup_command.txt"
  printf '%q ' "${eval_cmd[@]}" > "${run_dir}/eval_command.txt"
  printf '\n' >> "${run_dir}/eval_command.txt"

  run_and_log "${run_dir}/fuzz.log" "${fuzz_cmd[@]}"
  if [[ "${MANAGE_TARGET}" -eq 1 ]]; then
    stop_target
    target_log="${run_dir}/target-dedup.log"
    echo "+ uvicorn ${TARGET_APP} --host ${TARGET_HOST} --port ${TARGET_PORT}" > "${target_log}"
    (
      cd "${TARGET_DIR_ABS}"
      if [[ -n "${VENV_DIR}" ]]; then
        # shellcheck disable=SC1090
        source "${VENV_ACTIVATE_ABS}"
      fi
      COVERAGE_PROCESS_START="${COVERAGE_RC_ABS}" \
        uvicorn "${TARGET_APP}" --host "${TARGET_HOST}" --port "${TARGET_PORT}" --log-level warning
    ) >> "${target_log}" 2>&1 &
    TARGET_PID=$!
    echo "${TARGET_PID}" > "${run_dir}/target-dedup.pid"
    wait_for_target "${HEALTH_URL}" "${TARGET_PID}" "${target_log}"
  fi
  run_and_log "${run_dir}/dedup.log" "${dedup_cmd[@]}"
  if [[ "${MANAGE_TARGET}" -eq 1 ]]; then
    stop_target
  fi
  run_and_log "${run_dir}/eval.log" "${eval_cmd[@]}"

  mv crash_dedup "${run_dir}/crash_dedup"
  mv results.json "${run_dir}/results.json"
  if [[ -d crashes ]]; then
    mv crashes "${run_dir}/crashes"
  fi
  if [[ "${KEEP_QUEUE}" -eq 1 && -d queue ]]; then
    mv queue "${run_dir}/queue"
  else
    rm -rf queue
  fi

  results_file="${run_dir}/results.json"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${run_name}" \
    "$(json_metric "${results_file}" total_files)" \
    "$(json_metric "${results_file}" reproduced)" \
    "$(json_metric "${results_file}" dedup_clusters)" \
    "$(json_metric "${results_file}" dedup_reduction)" \
    "$(json_metric "${results_file}" oracle_labeled_crashes)" \
    "$(json_metric "${results_file}" oracle_unlabeled_crashes)" \
    "$(json_metric "${results_file}" oracle_bug_ids_observed)" \
    "$(json_metric "${results_file}" false_merge_clusters)" \
    "$(json_metric "${results_file}" false_split_bug_ids)" \
    "$(json_metric "${results_file}" weighted_cluster_purity)" \
    >> "${summary_file}"

  echo "== ${run_name}: completed at $(date --iso-8601=seconds); results in ${run_dir} =="
done

rm -rf crashes queue crash_dedup results.json

echo "Benchmark completed. Summary: ${summary_file}"
column -t -s $'\t' "${summary_file}" || cat "${summary_file}"
