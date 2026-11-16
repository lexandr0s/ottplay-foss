"""Pure PCM oracle regressions; no codec, media process or device execution."""
from array import array
import importlib.util
import math
from fractions import Fraction
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
    def test_filter_expression_escapes_only_the_two_filter_argument_commas(self):
        self.assertEqual(MEDIA._select_filter_expression(59, 138),
                         r'select=between(n\,59\,138)')
        for first, last in ((-1, 2), (3, 2), (0, 1100)):
            with self.subTest(first=first, last=last), self.assertRaises(ValueError):
                MEDIA._select_filter_expression(first, last)

    def test_all_planned_pts_must_have_decoded_rgb_frames(self):
        pts = [Fraction(index, MEDIA.FPS) for index in range(72)]
        row = {'first_frame_index': 0, 'last_frame_index': 71, 'expected_frames': 72,
               'start_pts': '0/1', 'end_pts': '8/1', 'first_pts': '0/1', 'last_pts': '71/10'}
        stamps = MEDIA._validate_video_window_timestamps(pts, row, 72)
        self.assertEqual(len(stamps), 72)
        with self.assertRaisesRegex(ValueError, 'does not cover all planned PTS'):
            MEDIA._validate_video_window_timestamps(pts, row, 65)
        with self.assertRaisesRegex(ValueError, 'does not cover all planned PTS'):
            MEDIA._validate_video_window_timestamps(pts, row, 73)

    def test_half_open_window_boundaries_on_vfr_pts_select_contiguous_ordinals(self):
        pts = [Fraction(value, 100) for value in (0, 10, 20, 35, 50, 90)]
        first, last, selected = MEDIA._window_frame_range(pts, Fraction(0), Fraction(1, 2))
        self.assertEqual((first, last, selected), (0, 3, pts[:4]))
        second, end, selected = MEDIA._window_frame_range(pts, Fraction(1, 2), Fraction(1))
        self.assertEqual((second, end, selected), (4, 5, pts[4:]))
        self.assertEqual(last + 1, second, 'Adjacent half-open windows must partition at the boundary')

    def test_full_plan_rederives_vfr_indices_and_exact_byte_budget(self):
        origin = Fraction(1421333, 1000000)
        pts = [origin + Fraction(index, 10) for index in range(850)]
        span = pts[-1] - pts[0] + Fraction(1, MEDIA.FPS)
        plan = MEDIA._video_plan(Fraction(117, 20), span, pts)
        self.assertEqual(plan, MEDIA._video_plan(Fraction(117, 20), span, pts))
        self.assertEqual(len(plan), 11)
        self.assertEqual([row['expected_frames'] for row in plan], [80] * 10 + [20])
        self.assertEqual((plan[-1]['first_frame_index'], plan[-1]['last_frame_index']), (830, 849))
        total = sum(row['expected_frames'] * 384 * 288 * 3 for row in plan)
        self.assertEqual(total, 272056320)
        self.assertLessEqual(total, 384 * 1024 * 1024)
        for row in plan:
            first, last, selected = MEDIA._window_frame_range(
                pts, Fraction(row['start_pts']), Fraction(row['end_pts']))
            self.assertEqual((first, last, len(selected)),
                             (row['first_frame_index'], row['last_frame_index'], row['expected_frames']))

    def test_nonmonotonic_duplicate_and_missing_pts_rejected(self):
        base = {'streams': [{'time_base': '1/10'}]}
        parsed = MEDIA._probe_frames({**base, 'frames': [
            {'best_effort_timestamp': 142}, {'best_effort_timestamp': 143},
            {'best_effort_timestamp': 145}]})
        self.assertEqual(parsed, [Fraction(71, 5), Fraction(143, 10), Fraction(29, 2)])
        for frame_rows in (
                [{'best_effort_timestamp': n} for n in (0, 1, 1)],
                [{'best_effort_timestamp': n} for n in (0, 2, 1)],
                [{'best_effort_timestamp': 0}, {}]):
            with self.subTest(frame_rows=frame_rows), self.assertRaises(ValueError):
                MEDIA._probe_frames({**base, 'frames': frame_rows})

    def test_exact_rgb_frame_count_rejects_truncation_and_extra_frames(self):
        MEDIA._validate_decoded_frame_count(72, 72)
        for observed in (65, 73):
            with self.subTest(observed=observed), self.assertRaisesRegex(
                    ValueError, 'exact planned selection'):
                MEDIA._validate_decoded_frame_count(observed, 72)


if __name__ == '__main__':
    unittest.main(verbosity=2)
