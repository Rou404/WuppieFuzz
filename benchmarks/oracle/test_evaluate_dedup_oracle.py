#!/usr/bin/env python3

from pathlib import Path
from unittest import TestCase, main

from evaluate_dedup_oracle import evaluate


def cluster(key: str, member_count: int, oracle_counts: dict[str, int]) -> dict:
    return {
        "key": key,
        "representative": f"{key}.json",
        "member_count": member_count,
        "oracle_crash_id_counts": oracle_counts,
    }


class EvaluateDedupOracleTests(TestCase):
    def test_clean_clusters_have_perfect_purity(self) -> None:
        report = {
            "summary": {"reproduced": 3, "unique_clusters": 2},
            "clusters": [
                cluster("first", 2, {"BUG-001": 2}),
                cluster("second", 1, {"BUG-002": 1}),
            ],
        }

        metrics = evaluate(report, Path("clusters.json"))["metrics"]

        self.assertEqual(metrics["false_merge_clusters"], 0)
        self.assertEqual(metrics["false_split_bug_ids"], 0)
        self.assertEqual(metrics["weighted_cluster_purity"], 1.0)
        self.assertAlmostEqual(metrics["dedup_reduction"], 1 / 3)

    def test_multiple_oracle_ids_are_reported_as_false_merge(self) -> None:
        report = {
            "clusters": [
                cluster("mixed", 3, {"BUG-001": 2, "BUG-002": 1}),
            ]
        }

        results = evaluate(report, Path("clusters.json"))

        self.assertEqual(results["metrics"]["false_merge_clusters"], 1)
        self.assertEqual(len(results["false_merges"]), 1)
        self.assertAlmostEqual(results["metrics"]["weighted_cluster_purity"], 2 / 3)

    def test_one_oracle_id_in_multiple_clusters_is_false_split(self) -> None:
        report = {
            "clusters": [
                cluster("first", 1, {"BUG-001": 1}),
                cluster("second", 2, {"BUG-001": 2}),
            ]
        }

        results = evaluate(report, Path("clusters.json"))

        self.assertEqual(results["metrics"]["false_split_bug_ids"], 1)
        self.assertEqual(results["false_splits"][0]["oracle_crash_id"], "BUG-001")
        self.assertEqual(results["false_splits"][0]["cluster_count"], 2)


if __name__ == "__main__":
    main()
