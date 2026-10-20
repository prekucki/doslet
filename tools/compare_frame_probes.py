#!/usr/bin/env python3
"""Compare ordered image-copy outputs and their completion times, without inferring deadlines."""
import argparse
import json
from pathlib import Path
from statistics import median


def compare(reference, experiment):
    def load(directory):
        data = json.loads((directory / 'probes.json').read_text())
        return [(e['pc'], e['frame'], e['after_ns'] - data['start_ns'])
                for e in data['events'] if 'frame' in e]
    a, b = load(reference), load(experiment)
    deltas = [(y[2] - x[2]) / 1e6 for x, y in zip(a, b)]
    return {'reference': str(reference), 'experiment': str(experiment),
            'copy_completions': [len(a), len(b)],
            'frame_sequence_equal': bool(a) and [(e[0], e[1]) for e in a] == [(e[0], e[1]) for e in b],
            'pcm_equal': (reference / 'audio.s16le').read_bytes() == (experiment / 'audio.s16le').read_bytes(),
            'completion_delta_ms': deltas,
            'minimum_delta_ms': min(deltas, default=None),
            'median_delta_ms': median(deltas) if deltas else None,
            'maximum_delta_ms': max(deltas, default=None),
            'deadline_acceptance': 'not evaluated; no independent deadlines supplied'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('reference', type=Path)
    parser.add_argument('experiment', type=Path)
    args = parser.parse_args()
    result = compare(args.reference, args.experiment)
    (args.experiment / 'frame-comparison.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
