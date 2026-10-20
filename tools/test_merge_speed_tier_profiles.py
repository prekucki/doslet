import unittest
from itertools import permutations
from merge_speed_tier_profiles import merge


def profile(tier, byte=0x90, reason='stable'):
    return dict(unknown='grey', irq_policy='static-grey', nodes=[dict(pc=1, bits=32, bytes=[byte]*15, tier=tier, reason=reason)])


class MergeTests(unittest.TestCase):
    def test_conflicts_cannot_be_reinstated_by_later_sources(self):
        for guard in (profile('fast', 0xcc), profile('grey', reason='interrupt')):
            for sources in permutations([profile('fast'), guard, profile('slow')]):
                result = merge(sources)
                self.assertEqual(result['nodes'][0]['tier'], 'grey')

    def test_unknown_never_erases_a_colour(self):
        for tier in ('fast', 'slow'):
            for sources in permutations([profile('grey'), profile(tier), profile('grey')]):
                result = merge(sources)
                self.assertEqual(result['nodes'][0]['tier'], tier)
                self.assertEqual(result['conflicts'], 0)
                self.assertEqual(result['unknown_overrides'], 1)

    def test_growth_wins_over_stability_in_another_scene(self):
        hot = profile('fast')
        cold = profile('slow', reason='growing')
        cold['nodes'][0].update(low_count=92920, high_count=287123)
        for sources in permutations([hot, cold, profile('grey'), hot]):
            result = merge(sources)
            node = result['nodes'][0]
            self.assertEqual(node['tier'], 'slow')
            self.assertEqual(node['reason'], 'growing')
            self.assertEqual(node['high_count'], 287123)
            self.assertEqual(node['observed_tiers'], ['fast', 'slow'])
            self.assertEqual(result['conflicts'], 1)
        self.assertEqual(hot['nodes'][0]['tier'], 'fast')
        self.assertNotIn('observed_tiers', cold['nodes'][0])

    def test_code_conflict_survives_merging_an_already_merged_profile(self):
        conflicted = merge([profile('fast'), profile('fast', 0xcc)])
        result = merge([conflicted, profile('slow')])
        self.assertEqual(result['nodes'][0]['tier'], 'grey')
        self.assertEqual(result['nodes'][0]['reason'], 'code-conflict')

    def test_agreement_and_missing_nodes(self):
        result = merge([profile('slow'), profile('slow')])
        self.assertEqual(result['counts'], dict(fast=0, slow=1, grey=0, irq=0))

    def test_confirmed_irq_is_distinct_from_old_graph_guesses(self):
        for sources in permutations([profile('irq', reason='interrupt-frame'),
                                     profile('grey', reason='interrupt'), profile('slow')]):
            result = merge(sources)
            self.assertEqual(result['nodes'][0]['tier'], 'irq')
            self.assertEqual(result['nodes'][0]['reason'], 'interrupt-frame')
            self.assertEqual(result['counts']['irq'], 1)

    def test_requires_conservative_inputs(self):
        source = profile('fast')
        source['unknown'] = 'fast'
        with self.assertRaises(ValueError):
            merge([source])
