#!/usr/bin/env python3
"""Evaluate deduplication clusters against benchmark oracle crash IDs."""

from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def load_report(path: Path) -> dict[str, Any]:
    try:
        with path.open("r", encoding="utf-8") as handle:
            report = json.load(handle)
    except FileNotFoundError:
        fail(f"input file does not exist: {path}")
    except json.JSONDecodeError as error:
        fail(f"input file is not valid JSON: {error}")

    if not isinstance(report, dict):
        fail("input JSON must be an object")
    if not isinstance(report.get("clusters"), list):
        fail("input JSON must contain a 'clusters' array")

    return report


def oracle_counts(cluster: dict[str, Any], index: int) -> dict[str, int]:
    counts = cluster.get("oracle_crash_id_counts", {})
    if counts is None:
        return {}
    if not isinstance(counts, dict):
        fail(f"cluster {index} has non-object 'oracle_crash_id_counts'")

    clean_counts = {}
    for bug_id, count in counts.items():
        if not isinstance(bug_id, str) or not bug_id.strip():
            fail(f"cluster {index} has an invalid oracle crash ID")
        if not isinstance(count, int) or count < 0:
            fail(f"cluster {index} has an invalid count for oracle ID {bug_id!r}")
        if count > 0:
            clean_counts[bug_id] = count

    return clean_counts


def evaluate(report: dict[str, Any], input_path: Path) -> dict[str, Any]:
    summary = report.get("summary", {})
    if summary is not None and not isinstance(summary, dict):
        fail("'summary' must be an object when present")

    clusters = report["clusters"]
    oracle_totals: defaultdict[str, int] = defaultdict(int)
    oracle_locations: defaultdict[str, list[dict[str, Any]]] = defaultdict(list)
    false_merges = []

    total_members = 0
    oracle_labeled_crashes = 0
    oracle_unlabeled_crashes = 0
    majority_labeled_crashes = 0
    labeled_clusters = 0

    for index, cluster in enumerate(clusters):
        if not isinstance(cluster, dict):
            fail(f"cluster {index} must be an object")

        member_count = cluster.get("member_count")
        if not isinstance(member_count, int) or member_count < 0:
            fail(f"cluster {index} has an invalid 'member_count'")

        counts = oracle_counts(cluster, index)
        labeled_count = sum(counts.values())
        if labeled_count > member_count:
            fail(
                f"cluster {index} has more oracle-labeled crashes "
                f"({labeled_count}) than members ({member_count})"
            )

        key = cluster.get("key", f"cluster-{index}")
        representative = cluster.get("representative")

        total_members += member_count
        oracle_labeled_crashes += labeled_count
        oracle_unlabeled_crashes += member_count - labeled_count

        if counts:
            labeled_clusters += 1
            majority_labeled_crashes += max(counts.values())

        if len(counts) > 1:
            false_merges.append(
                {
                    "cluster_index": index,
                    "key": key,
                    "representative": representative,
                    "member_count": member_count,
                    "oracle_crash_id_counts": counts,
                }
            )

        for bug_id, count in counts.items():
            oracle_totals[bug_id] += count
            oracle_locations[bug_id].append(
                {
                    "cluster_index": index,
                    "key": key,
                    "representative": representative,
                    "count": count,
                }
            )

    false_splits = []
    for bug_id, locations in sorted(oracle_locations.items()):
        if len(locations) > 1:
            false_splits.append(
                {
                    "oracle_crash_id": bug_id,
                    "cluster_count": len(locations),
                    "total_count": oracle_totals[bug_id],
                    "clusters": locations,
                }
            )

    reproduced = summary.get("reproduced", total_members)
    if not isinstance(reproduced, int) or reproduced < 0:
        fail("'summary.reproduced' must be a non-negative integer when present")

    unique_clusters = summary.get("unique_clusters", len(clusters))
    if not isinstance(unique_clusters, int) or unique_clusters < 0:
        fail("'summary.unique_clusters' must be a non-negative integer when present")

    weighted_purity = None
    if oracle_labeled_crashes > 0:
        weighted_purity = majority_labeled_crashes / oracle_labeled_crashes

    dedup_reduction = None
    if reproduced > 0:
        dedup_reduction = 1 - (unique_clusters / reproduced)

    metrics = {
        "total_files": summary.get("total_files"),
        "reproduced": reproduced,
        "dedup_clusters": unique_clusters,
        "dedup_reduction": dedup_reduction,
        "oracle_labeled_crashes": oracle_labeled_crashes,
        "oracle_unlabeled_crashes": oracle_unlabeled_crashes,
        "oracle_labeled_clusters": labeled_clusters,
        "oracle_bug_ids_observed": len(oracle_totals),
        "false_merge_clusters": len(false_merges),
        "false_split_bug_ids": len(false_splits),
        "weighted_cluster_purity": weighted_purity,
    }

    return {
        "input": str(input_path),
        "metrics": metrics,
        "false_merges": false_merges,
        "false_splits": false_splits,
    }


def write_results(path: Path, results: dict[str, Any]) -> None:
    with path.open("w", encoding="utf-8") as handle:
        json.dump(results, handle, indent=2, sort_keys=True)
        handle.write("\n")


def print_summary(results: dict[str, Any], output_path: Path) -> None:
    metrics = results["metrics"]
    purity = metrics["weighted_cluster_purity"]
    purity_text = "n/a" if purity is None else f"{purity:.4f}"
    reduction = metrics["dedup_reduction"]
    reduction_text = "n/a" if reduction is None else f"{reduction:.4f}"

    print(f"wrote {output_path}")
    print(f"dedup clusters: {metrics['dedup_clusters']}")
    print(f"oracle bug IDs observed: {metrics['oracle_bug_ids_observed']}")
    print(f"false merge clusters: {metrics['false_merge_clusters']}")
    print(f"false split bug IDs: {metrics['false_split_bug_ids']}")
    print(f"weighted cluster purity: {purity_text}")
    print(f"dedup reduction: {reduction_text}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Evaluate WuppieFuzz dedup clusters against benchmark oracle IDs."
    )
    parser.add_argument("clusters_json", type=Path, help="Path to dedup clusters.json")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    output_path = Path("results.json")
    report = load_report(args.clusters_json)
    results = evaluate(report, args.clusters_json)
    write_results(output_path, results)
    print_summary(results, output_path)


if __name__ == "__main__":
    main()
