#!/usr/bin/env python3
"""Compare offline traces per guest second, preserving raw evidence and PCM.

Input: a directory containing one state_graph_trace output directory per run.
Output: node/edge count CSVs and a summary JSON. No wait classification is inferred.
"""
import argparse
import csv
import hashlib
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    runs = []
    for metrics in args.directory.glob("*/metrics.json"):
        path = metrics.parent
        m = json.loads(metrics.read_text())
        if not m["duration_complete"]:
            raise ValueError(f"Incomplete run: {path}")
        trace = json.loads((path / "trace.json").read_text())
        counts, edges = {}, {}
        for node in trace["nodes"]:
            key = (node["pc"], node["bits"])
            counts[key] = node["count"]
            for target, count in node["successors"].items():
                edges[(*key, int(target))] = count
        irq_steps = sum(trace["irq_entries"].values())
        executed = trace.get("executed_scheduler_steps", m.get("executed_scheduler_steps", trace["steps"]))
        if sum(counts.values()) + irq_steps != executed:
            raise ValueError(f"Trace accounting mismatch: {path}")
        pcm = (path / "audio.s16le").read_bytes()
        if len(pcm) != m["pcm_frames"] * 4 or m["pcm_frames"] != m["pcm_generated"]:
            raise ValueError(f"PCM accounting mismatch: {path}")
        block_bytes = 1024 * 4
        blocks = [pcm[start:start + block_bytes]
                  for start in range(0, len(pcm) - block_bytes + 1, block_bytes)]
        m.update({"run": path.name, "pcm_sha256": hashlib.sha256(pcm).hexdigest(),
                  "irq_entry_steps": irq_steps, "observed_nodes": len(counts),
                  "changed_code_windows": sum(n["changed"] for n in trace["nodes"]),
                  "zero_pcm_1024_frame_blocks": sum(not any(block) for block in blocks),
                  "repeated_pcm_blocks_by_lag": {
                      str(lag): sum(blocks[i] == blocks[i-lag]
                                    for i in range(lag, len(blocks)))
                      for lag in (1, 2, 4, 8)},
                  "repeated_nonzero_pcm_blocks_by_lag": {
                      str(lag): sum(blocks[i] == blocks[i-lag] and any(blocks[i])
                                    for i in range(lag, len(blocks)))
                      for lag in (1, 2, 4, 8)}})
        runs.append((m, counts, edges))
    runs.sort(key=lambda row: (row[0]["mhz"], row[0]["run"]))
    if not runs:
        raise ValueError("No completed traces found")
    for filename, field_names, index in [("nodes.csv", ["pc", "bits"], 1),
                                         ("edges.csv", ["pc", "bits", "target"], 2)]:
        keys = sorted(set().union(*(row[index] for row in runs)))
        with (args.directory / filename).open("w", newline="") as file:
            writer = csv.writer(file)
            columns = [f'{row[0]["run"]}_{suffix}' for row in runs
                       for suffix in ("count", "per_guest_second")]
            writer.writerow(field_names + columns)
            for key in keys:
                values = [hex(k) if field != "bits" else k for field, k in zip(field_names, key)]
                for metrics, counts, edges in runs:
                    count = (counts if index == 1 else edges).get(key, 0)
                    values += [count, count * 1e9 / metrics["guest_ns"]]
                writer.writerow(values)
    summary = [row[0] for row in runs]
    (args.directory / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    for row in summary:
        print(f'{row["run"]}: {row["guest_ns"]/1e9:.3f} guest s, '
              f'{row["observed_nodes"]} PCs, PCM {row["pcm_sha256"][:16]}, '
              f'DMA misses {row["dma_misses"]}/{row["dma_checks"]}')


if __name__ == "__main__":
    main()
