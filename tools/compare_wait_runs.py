#!/usr/bin/env python3
"""Compare a city wait experiment with the same-clock reference, including work timing."""
import argparse
import hashlib
import json
from pathlib import Path
from city_deadlines import analyze


def report(reference, experiment):
    def read(path, name):
        return json.loads((path / name).read_text())
    a, b = [read(p, 'metrics.json') for p in (reference, experiment)]
    da, db = [analyze(p) for p in (reference, experiment)]
    frames_a, frames_b = [[e['frame'] for e in read(p, 'probes.json')['events'] if 'frame' in e]
                          for p in (reference, experiment)]
    audio_a, audio_b = [(p / 'audio.s16le').read_bytes() for p in (reference, experiment)]
    result = {
        'reference': str(reference), 'experiment': str(experiment),
        'same_mhz': a['mhz'] == b['mhz'],
        'complete_duration': a['duration_complete'] and b['duration_complete'],
        'same_requested_duration': a['requested_guest_ms'] == b['requested_guest_ms'],
        'pcm_equal': audio_a == audio_b,
        'pcm_sha256': hashlib.sha256(audio_b).hexdigest(),
        'final_png_equal': (reference / 'final.png').read_bytes() == (experiment / 'final.png').read_bytes(),
        'work_frame_count': len(frames_b),
        'work_frame_sequence_equal': bool(frames_a) and frames_a == frames_b,
        'reference_animation': da['animation'], 'experiment_animation': db['animation'],
        'reference_audio': da['audio'], 'experiment_audio': db['audio'],
        'same_timer_callback_count': da['timer']['counter_callbacks'] == db['timer']['counter_callbacks'],
        'idle_fraction': b['wait_regulator']['idle_ns'] / b['guest_ns'],
        'executed_steps_saved': 1 - b['executed_scheduler_steps'] / a['executed_scheduler_steps'],
        'regulator': b['wait_regulator'],
    }
    result['accepted_fixture_run'] = all(result[k] for k in (
        'same_mhz', 'complete_duration', 'same_requested_duration', 'pcm_equal',
        'final_png_equal', 'work_frame_sequence_equal', 'same_timer_callback_count')) and (
        da['animation']['episodes'] == db['animation']['episodes']
        and db['animation']['resets_after_next_tick'] == 0
        and b['dma_misses'] == 0 and b['pcm_gaps'] == 0 and b['pcm_overlaps'] == 0
        and db['audio']['block_windows_without_copy_completion'] == 0
        and result['idle_fraction'] > 0)
    (experiment / 'comparison.json').write_text(json.dumps(result, indent=2) + '\n')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('reference', type=Path)
    parser.add_argument('experiment', type=Path)
    parser.add_argument('--require-pass', action='store_true')
    args = parser.parse_args()
    result = report(args.reference, args.experiment)
    print(json.dumps(result, indent=2))
    if args.require_pass and not result['accepted_fixture_run']:
        raise SystemExit('wait experiment failed acceptance; inspect comparison.json')


if __name__ == '__main__':
    main()
