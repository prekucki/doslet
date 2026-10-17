import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from speed_tier_profile import build, interrupt_nodes


class SpeedTierProfileTests(unittest.TestCase):
    def setUp(self):
        self.temp = TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.low, self.high = [Path(self.temp.name) / name for name in ('low', 'high')]
        for directory, mhz in ((self.low, 133), (self.high, 266)):
            directory.mkdir()
            metrics = dict(duration_complete=True, requested_guest_ms=5000,
                           guest_ns=5_000_000_000, mhz=mhz)
            (directory / 'metrics.json').write_text(json.dumps(metrics))
            for name in ('low.bin', 'extended.bin', 'audio.s16le', 'final.png'):
                (directory / name).write_bytes(b'same')
            counts = [200, 200, 10, 200] if mhz == 133 else [220, 400, 20, 220]
            nodes = [dict(pc=pc, bits=32, bytes=[pc] * 15, changed=False, count=count)
                     for pc, count in enumerate(counts)]
            if mhz == 266:
                nodes[-1]['bytes'][0] = 99
            (directory / 'trace.json').write_text(json.dumps(
                dict(start_ns=10, start_pc=0, nodes=nodes)))

    def test_stable_hot_policy_slows_cold_and_unknown_but_rejects_changed_code(self):
        profile = build(self.low, self.high, 'stable-hot')
        self.assertEqual(profile['unknown'], 'slow')
        self.assertEqual([(n['pc'], n['tier']) for n in profile['nodes']],
                         [(0, 'fast'), (1, 'slow'), (2, 'slow')])

    def test_growing_policy_keeps_uncertain_paths_fast(self):
        profile = build(self.low, self.high, 'growing')
        self.assertEqual(profile['unknown'], 'fast')
        self.assertEqual([n['tier'] for n in profile['nodes']], ['fast', 'slow', 'fast'])

    def test_three_tiers_leave_rare_and_unknown_evidence_grey(self):
        profile = build(self.low, self.high)
        self.assertEqual(profile['unknown'], 'grey')
        self.assertEqual([n['tier'] for n in profile['nodes']], ['fast', 'slow', 'grey'])
        self.assertEqual(profile['grey_nodes'], 1)

    def test_interrupt_reachability_stops_at_prefixed_iret(self):
        trace = {'irq_entries': {'100': 1}, 'nodes': [
            {'pc': 100, 'bits': 32, 'bytes': [0x90], 'successors': {'101': 2, '102': 1}},
            {'pc': 101, 'bits': 32, 'bytes': [0x66, 0xcf], 'successors': {'200': 1}},
            {'pc': 102, 'bits': 32, 'bytes': [0x90], 'successors': {'100': 1}},
            {'pc': 200, 'bits': 32, 'bytes': [0x90], 'successors': {'201': 1}},
        ]}
        self.assertEqual(interrupt_nodes(trace), {(100, 32), (101, 32), (102, 32)})

    def test_irq_code_is_saved_as_grey_even_when_counts_are_stable_or_growing(self):
        for directory in (self.low, self.high):
            path = directory / 'trace.json'
            trace = json.loads(path.read_text())
            trace['irq_entries'] = {'0': 1}
            trace['nodes'][0]['successors'] = {'1': 1}
            trace['nodes'][1]['bytes'] = [0xcf] * 15
            trace['nodes'][1]['successors'] = {'2': 1}
            path.write_text(json.dumps(trace))
        profile = build(self.low, self.high)
        self.assertEqual(profile['irq_policy'], 'static-grey')
        self.assertEqual([n['tier'] for n in profile['nodes']], ['grey', 'grey', 'grey'])
        self.assertEqual([n['reason'] for n in profile['nodes']],
                         ['interrupt', 'interrupt', 'uncertain'])

    def test_different_initial_ram_is_not_a_valid_comparison(self):
        (self.high / 'extended.bin').write_bytes(b'different')
        with self.assertRaisesRegex(ValueError, 'initial RAM differs'):
            build(self.low, self.high)

    def test_incomplete_trace_is_rejected(self):
        path = self.high / 'metrics.json'
        metrics = json.loads(path.read_text())
        metrics['duration_complete'] = False
        path.write_text(json.dumps(metrics))
        with self.assertRaises(ValueError):
            build(self.low, self.high)


if __name__ == '__main__':
    unittest.main()
