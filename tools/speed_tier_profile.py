#!/usr/bin/env python3
"""Build experimental per-PC speed tiers from equal-duration, same-state CPU sweeps."""
import argparse
import json
from pathlib import Path


def interrupt_nodes(trace):
    """Conservative observed reachability from IRQ entries, stopping at IRET.

    Shared callees and their observed continuations may be included as grey.
    No runtime return-address tracker is required to apply the saved profile.
    """
    by_pc = {}
    for node in trace['nodes']:
        by_pc.setdefault(node['pc'], []).append(node)
    pending = [int(pc) for pc in trace.get('irq_entries', {})]
    visited, result = set(), set()
    prefixes = {0x26, 0x2e, 0x36, 0x3e, 0x64, 0x65, 0x66, 0x67, 0xf0, 0xf2, 0xf3}
    while pending:
        pc = pending.pop()
        if pc in visited:
            continue
        visited.add(pc)
        for node in by_pc.get(pc, []):
            result.add((pc, node['bits']))
            opcode = next((b for b in node['bytes'] if b not in prefixes), None)
            if opcode != 0xcf:
                pending.extend(int(dst) for dst in node.get('successors', {}))
    return result


def build(low, high, policy='three'):
    if policy not in ('stable-hot', 'growing', 'three'):
        raise ValueError('unknown classification policy')
    lm, hm = [json.loads((p / 'metrics.json').read_text()) for p in (low, high)]
    lt, ht = [json.loads((p / 'trace.json').read_text()) for p in (low, high)]
    if not (lm['duration_complete'] and hm['duration_complete']
            and lm['requested_guest_ms'] == hm['requested_guest_ms']
            and lt['start_ns'] == ht['start_ns'] and lt['start_pc'] == ht['start_pc']
            and hm['mhz'] >= 2 * lm['mhz']):
        raise ValueError('need same initial boundary/duration and at least 2x CPU ratio')
    # Both dumps are taken before execution, including conventional RAM.
    for name in ('low.bin', 'extended.bin'):
        if (low / name).read_bytes() != (high / name).read_bytes():
            raise ValueError('initial RAM differs')
    a = {(n['pc'], n['bits']): n for n in lt['nodes']}
    irq = interrupt_nodes(lt) | interrupt_nodes(ht)
    rows = []
    for n in ht['nodes']:
        old = a.get((n['pc'], n['bits']))
        if old is None or old['changed'] or n['changed'] or old['bytes'] != n['bytes']:
            continue
        first = old['count'] * 1e9 / lm['guest_ns']
        second = n['count'] * 1e9 / hm['guest_ns']
        hot = min(first, second) >= 32
        growing = hot and second >= 1.5 * first
        stable = hot and .8 * first <= second <= 1.2 * first
        slow = not stable if policy == 'stable-hot' else growing
        tier = 'slow' if slow else 'fast'
        if policy == 'three' and not growing and not stable:
            tier = 'grey'
        interrupt = (n['pc'], n['bits']) in irq
        if interrupt:
            tier = 'grey'
        rows.append({'pc': n['pc'], 'bits': n['bits'], 'bytes': n['bytes'],
                     'tier': tier,
                     'reason': 'interrupt' if interrupt else 'growing' if growing else 'stable' if stable else 'uncertain',
                     'low_count': old['count'], 'high_count': n['count']})
    return {'low': str(low), 'high': str(high), 'low_mhz': lm['mhz'], 'high_mhz': hm['mhz'],
            'guest_ms': lm['requested_guest_ms'], 'start_ns': lt['start_ns'],
            'policy': policy, 'unknown': {'stable-hot': 'slow', 'growing': 'fast', 'three': 'grey'}[policy],
            'classification_only': True,
            'irq_policy': 'static-grey',
            'pcm_equal': (low / 'audio.s16le').read_bytes() == (high / 'audio.s16le').read_bytes(),
            'final_png_equal': (low / 'final.png').read_bytes() == (high / 'final.png').read_bytes(),
            'slow_nodes': sum(n['tier'] == 'slow' for n in rows),
            'grey_nodes': sum(n['tier'] == 'grey' for n in rows), 'nodes': rows}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('low', type=Path)
    p.add_argument('high', type=Path)
    p.add_argument('output', type=Path)
    p.add_argument('--policy', choices=('three', 'stable-hot', 'growing'), default='three')
    a = p.parse_args()
    result = build(a.low, a.high, a.policy)
    a.output.parent.mkdir(parents=True, exist_ok=True)
    a.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({k: v for k, v in result.items() if k != 'nodes'}, indent=2))


if __name__ == '__main__':
    main()
