#!/usr/bin/env python3
"""Create a human-readable report from WuppieFuzz dedup clusters.json."""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
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
    if "summary" in report and not isinstance(report["summary"], dict):
        fail("'summary' must be an object when present")

    return report


def clean_text(value: Any, default: str = "n/a") -> str:
    if value is None:
        return default
    text = str(value).replace("\n", " ").strip()
    return text if text else default


def table_row(values: list[Any]) -> str:
    return "| " + " | ".join(clean_text(value).replace("|", "\\|") for value in values) + " |"


def counter_from_clusters(clusters: list[dict[str, Any]], key_path: tuple[str, ...]) -> Counter[str]:
    values: Counter[str] = Counter()
    for cluster in clusters:
        value: Any = cluster
        for key in key_path:
            if not isinstance(value, dict):
                value = None
                break
            value = value.get(key)
        values[clean_text(value)] += 1
    return values


def member_count(cluster: dict[str, Any]) -> int:
    value = cluster.get("member_count")
    if isinstance(value, int) and value >= 0:
        return value
    members = cluster.get("members")
    if isinstance(members, list):
        return len(members)
    return 0


def oracle_text(cluster: dict[str, Any]) -> str:
    counts = cluster.get("oracle_crash_id_counts")
    if not isinstance(counts, dict) or not counts:
        ids = cluster.get("oracle_crash_ids")
        if isinstance(ids, list) and ids:
            return ", ".join(clean_text(item) for item in ids)
        return "n/a"

    pairs = []
    for bug_id, count in sorted(counts.items(), key=lambda item: (-int(item[1]), str(item[0]))):
        pairs.append(f"{bug_id}:{count}")
    return ", ".join(pairs)


def summary_value(summary: dict[str, Any], key: str, fallback: Any) -> Any:
    value = summary.get(key)
    return fallback if value is None else value


def format_counter_section(title: str, counter: Counter[str], limit: int) -> list[str]:
    lines = [f"## {title}", "", "| Value | Clusters |", "|---|---:|"]
    for value, count in counter.most_common(limit):
        lines.append(table_row([value, count]))
    if not counter:
        lines.append(table_row(["n/a", 0]))
    lines.append("")
    return lines


def build_markdown(report: dict[str, Any], input_path: Path, top: int) -> str:
    raw_clusters = report["clusters"]
    clusters = [cluster for cluster in raw_clusters if isinstance(cluster, dict)]
    skipped_clusters = len(raw_clusters) - len(clusters)
    summary = report.get("summary") or {}

    total_members = sum(member_count(cluster) for cluster in clusters)
    unique_clusters = summary_value(summary, "unique_clusters", len(clusters))
    reproduced = summary_value(summary, "reproduced", total_members)
    total_files = summary_value(summary, "total_files", "n/a")
    non_reproducible = summary_value(
        summary,
        "non_reproducible",
        len(report.get("non_reproducible") or []),
    )
    skipped = summary_value(summary, "skipped", len(report.get("skipped") or []))

    reduction = "n/a"
    if isinstance(reproduced, int) and reproduced > 0 and isinstance(unique_clusters, int):
        reduction = f"{1 - (unique_clusters / reproduced):.4f}"

    lines = [
        "# WuppieFuzz Dedup Cluster Report",
        "",
        f"Input: `{input_path}`",
        "",
        "## Summary",
        "",
        "| Metric | Value |",
        "|---|---:|",
        table_row(["total files", total_files]),
        table_row(["reproduced", reproduced]),
        table_row(["unique clusters", unique_clusters]),
        table_row(["dedup reduction", reduction]),
        table_row(["non-reproducible", non_reproducible]),
        table_row(["skipped", skipped]),
    ]

    if skipped_clusters:
        lines.append(table_row(["invalid cluster entries ignored", skipped_clusters]))
    lines.append("")

    lines.extend(
        format_counter_section(
            "Clusters By Endpoint",
            counter_from_clusters(clusters, ("identity", "endpoint")),
            top,
        )
    )
    lines.extend(
        format_counter_section(
            "Clusters By HTTP Status",
            counter_from_clusters(clusters, ("identity", "http_status")),
            top,
        )
    )
    lines.extend(
        format_counter_section(
            "Clusters By Crash Kind",
            counter_from_clusters(clusters, ("identity", "crash_kind")),
            top,
        )
    )
    lines.extend(
        format_counter_section(
            "Clusters By Response Class",
            counter_from_clusters(clusters, ("identity", "response_class")),
            top,
        )
    )

    lines.extend(
        [
            "## Largest Clusters",
            "",
            "| Rank | Members | Endpoint | Status | Kind | Response | Representative | Oracle IDs |",
            "|---:|---:|---|---:|---|---|---|---|",
        ]
    )

    largest = sorted(clusters, key=member_count, reverse=True)[:top]
    for rank, cluster in enumerate(largest, start=1):
        identity = cluster.get("identity") if isinstance(cluster.get("identity"), dict) else {}
        lines.append(
            table_row(
                [
                    rank,
                    member_count(cluster),
                    identity.get("endpoint"),
                    identity.get("http_status"),
                    identity.get("crash_kind"),
                    identity.get("response_class"),
                    cluster.get("representative"),
                    oracle_text(cluster),
                ]
            )
        )
    if not largest:
        lines.append(table_row(["n/a", 0, "n/a", "n/a", "n/a", "n/a", "n/a", "n/a"]))

    lines.append("")
    return "\n".join(lines)


def default_output_path(input_path: Path) -> Path:
    return input_path.with_name("cluster_report.md")


def write_output(path: Path | None, content: str) -> None:
    if path is None:
        print(content, end="")
        return
    with path.open("w", encoding="utf-8") as handle:
        handle.write(content)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Create a Markdown report from WuppieFuzz dedup clusters.json."
    )
    parser.add_argument("clusters_json", type=Path, help="Path to clusters.json")
    parser.add_argument(
        "-o",
        "--output",
        type=Path,
        help="Output Markdown path. Defaults to cluster_report.md next to the input. Use '-' for stdout.",
    )
    parser.add_argument(
        "--top",
        type=int,
        default=20,
        help="Number of rows to include in ranked sections. Default: 20",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.top < 1:
        fail("--top must be a positive integer")

    report = load_report(args.clusters_json)
    content = build_markdown(report, args.clusters_json, args.top)

    output_path = None if args.output == Path("-") else args.output
    if output_path is None and args.output is None:
        output_path = default_output_path(args.clusters_json)

    write_output(output_path, content)
    if output_path is not None:
        print(f"wrote {output_path}")


if __name__ == "__main__":
    main()
