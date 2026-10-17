#!/usr/bin/env python3
"""Merge scene evidence: COLD > HOT > unknown; preserve IRQ/code guards."""
import argparse
from collections import defaultdict
import json
from pathlib import Path


def merge(profiles):
    grouped = defaultdict(list)
    for profile in profiles:
        if profile.get('unknown') != 'grey' or profile.get('irq_policy') != 'static-grey':
            raise ValueError('each source must use unknown=grey and irq_policy=static-grey')
        for node in profile['nodes']:
            key = (node['pc'], node['bits'])
            grouped[key].append(node)
    rows = []
    conflicts = unknown_overrides = 0
    rank = {'grey': 0, 'fast': 1, 'slow': 2, 'irq': 3}
    for _, candidates in sorted(grouped.items()):
        tiers = {n['tier'] for n in candidates}
        # Preserve the counts/reason of the observation supplying the colour.
        chosen = dict(max(candidates, key=lambda n: rank[n['tier']]))
        reasons = {n.get('reason') for n in candidates}
        if len({tuple(n['bytes']) for n in candidates}) > 1 or 'code-conflict' in reasons:
            chosen['tier'], chosen['reason'] = 'grey', 'code-conflict'
            conflicts += 1
        elif 'irq' in tiers:
            chosen['tier'], chosen['reason'] = 'irq', 'interrupt-frame'
        elif 'interrupt' in reasons:
            chosen['tier'], chosen['reason'] = 'grey', 'interrupt'
        else:
            # Stable frequency in another scene does not refute observed growth.
            # Ordinary GREY is missing evidence, never a veto on a known colour.
            if {'fast', 'slow'} <= tiers:
                chosen['observed_tiers'] = ['fast', 'slow']
                conflicts += 1
            if 'grey' in tiers and len(tiers) > 1:
                unknown_overrides += 1
        rows.append(chosen)
    return {'unknown': 'grey', 'irq_policy': 'static-grey', 'classification_only': True,
            'policy': 'scene-evidence-cold-first', 'conflicts': conflicts,
            'unknown_overrides': unknown_overrides,
            'counts': {t: sum(n['tier'] == t for n in rows) for t in ('fast', 'slow', 'grey', 'irq')},
            'nodes': rows}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('profiles', nargs='+', type=Path)
    args = parser.parse_args()
    result = merge([json.loads(p.read_text()) for p in args.profiles])
    result['sources'] = [str(p) for p in args.profiles]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({k: v for k, v in result.items() if k != 'nodes'}, indent=2))


if __name__ == '__main__':
    main()
