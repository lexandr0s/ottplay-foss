"""Pure PCM oracle regressions; no codec, media process or device execution."""
from array import array
import importlib.util
import math
from pathlib import Path
import sys
import unittest

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    'multiview_unix_media_under_test', ROOT / 'scripts/ci-multiview-unix-media.py')
MEDIA = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MEDIA)


def selected_pcm(shifted):
    """65 s: clean first 50 ms, then 440 Hz plus a 1/8-amplitude 1000 Hz tone.

    Both tones repeat every 400 samples at 16 kHz. Building that exact period
    bounds the fixture allocation without retaining a million Python integers.
    The negative advances only the selected tone by 320 samples (20 ms).
    """
    rate = 16000
    period = 400
    advance = 320 if shifted else 0
    clean = array('h', (round(2000 * math.sin(2 * math.pi * 440 * i / rate))
                        for i in range(period)))
    mixed = array('h', (round(2000 * math.sin(2 * math.pi * 440 * (i + advance) / rate)
                             + 250 * math.sin(2 * math.pi * 1000 * i / rate))
                        for i in range(period)))
    samples = mixed * (65 * rate // period)
    samples[:800] = clean * 2
    if sys.byteorder != 'little':
        samples.byteswap()
    return samples.tobytes()


class SelectedPhaseContracts(unittest.TestCase):
    def analyze(self, raw):
        return (MEDIA._audio(raw, 2, 60), MEDIA._original_b_diagnostic(raw))

    def test_mixed_accepted_windows_reject_twenty_ms_phase_jump(self):
        adapter, original = self.analyze(selected_pcm(True))
        self.assertTrue(MEDIA._agreement(adapter, original))
        for name, result, bad_key in (
                ('adapter', adapter, 'bad_windows'),
                ('original', original, 'bad_bin_positions')):
            with self.subTest(analyzer=name):
                rows = result['windows_50ms']
                self.assertEqual(len(rows), 1300)
                self.assertGreater(rows[0]['coherence'], .99)
                self.assertTrue(all(.97 <= row['coherence'] < .99 for row in rows[1:]))
                self.assertTrue(result['full_selected_audio']['pass'])
                self.assertEqual(result[bad_key], [])
                self.assertFalse(result['pass'], 'Accepted energy must not bypass phase continuity')
                self.assertEqual(len(result['phase_steps']), 1)
                step = result['phase_steps'][0]
                self.assertEqual((step['before_seconds'], step['after_seconds']), (0., .05))
                self.assertAlmostEqual(abs(step['delta']), .4 * math.pi, places=3)

    def test_unshifted_mixed_accepted_windows_remain_valid(self):
        adapter, original = self.analyze(selected_pcm(False))
        self.assertTrue(MEDIA._agreement(adapter, original))
        for name, result in (('adapter', adapter), ('original', original)):
            with self.subTest(analyzer=name):
                self.assertTrue(result['pass'])
                self.assertEqual(result['phase_steps'], [])
        self.assertEqual(adapter['first_pure_bin'], 0.)
        self.assertEqual(adapter['selected_seconds'], 65.)
        self.assertTrue(adapter['exact_end_covered'])

    def test_existing_pure_hole_phase_tail_and_aac_boundary_controls(self):
        self.assertTrue(MEDIA.self_test()['pass'])


class VideoWindowCoverageContracts(unittest.TestCase):
    def test_all_planned_pts_must_have_decoded_rgb_frames(self):
        pts = [index / MEDIA.FPS for index in range(72)]
        stamps = MEDIA._validate_video_window_timestamps(pts, 0.0, 8.0, 72)
        self.assertEqual(len(stamps), 72)
        with self.assertRaisesRegex(ValueError, 'does not cover all planned PTS'):
            MEDIA._validate_video_window_timestamps(pts, 0.0, 8.0, 65)


if __name__ == '__main__':
    unittest.main(verbosity=2)
