import json
from pathlib import Path
import tempfile
import unittest

from compare_frame_probes import compare


class FrameTimingTests(unittest.TestCase):
    def fixture(self, directory, start, frames):
        directory.mkdir()
        (directory / 'audio.s16le').write_bytes(b'\x01\x00\x02\x00')
        (directory / 'probes.json').write_text(json.dumps({
            'start_ns': start,
            'events': [{'pc': 10, 'after_ns': start + ns,
                        'frame': {'rgba_fnv64': digest}} for ns, digest in frames]}))

    def test_equal_outputs_preserve_phase_drift_instead_of_claiming_deadline_success(self):
        with tempfile.TemporaryDirectory() as tmp:
            a, b = Path(tmp) / 'a', Path(tmp) / 'b'
            self.fixture(a, 1_000_000_000, [(10, 'a'), (20, 'b')])
            self.fixture(b, 2_000_000_000, [(1_000_010, 'a'), (2_000_020, 'b')])
            result = compare(a, b)
            self.assertTrue(result['frame_sequence_equal'])
            self.assertTrue(result['pcm_equal'])
            self.assertEqual(result['completion_delta_ms'], [1, 2])
            self.assertIn('not evaluated', result['deadline_acceptance'])

    def test_reordered_frames_and_empty_capture_are_not_equal_output_evidence(self):
        for frames in [[(10, 'b'), (20, 'a')], []]:
            with tempfile.TemporaryDirectory() as tmp:
                a, b = Path(tmp) / 'a', Path(tmp) / 'b'
                self.fixture(a, 0, [(10, 'a'), (20, 'b')])
                self.fixture(b, 0, frames)
                self.assertFalse(compare(a, b)['frame_sequence_equal'])


if __name__ == '__main__':
    unittest.main()
