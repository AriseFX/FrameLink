"""Clock sign, timestamp joins, repeated-frame statistics and marker ABI checks."""
import ctypes as C
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools/latency"))
from analyze import ClockCorrection, distribution, join_frames, summarize
from receive import Video, marker_ticks


class AnalysisTests(unittest.TestCase):
    @staticmethod
    def calibration(when_ns, offset):
        return {
            "best": {"t1": when_ns - 100_000, "t4": when_ns + 100_000, "offset_ms": offset, "rtt_ms": .2},
            "offset_uncertainty_bound_ms": .1,
        }

    @staticmethod
    def inputs():
        codes = [100_000_000, 100_040_000, 100_080_000, 100_120_000]
        markers = [99_900_000, 99_900_000, 99_980_000, 99_980_000]
        sender = [dict(frame=i+1, ndi_timecode_100ns=code, source_present_100ns=code-30_000,
                       source_clock_uncertainty_100ns=1, desktop_updated=i % 2 == 0, refreshed=i % 2 == 0)
                  for i, code in enumerate(codes)]
        receiver = [dict(sender_timecode_100ns=str(code), receiver_utc_100ns=str(code+20_000),
                         render_marker_100ns=str(marker)) for code, marker in zip(codes, markers)]
        return sender, receiver

    def test_repeats_and_initial_window_boundary_are_not_new_updates(self):
        sender, receiver = self.inputs()
        result = summarize(join_frames(sender, receiver, ClockCorrection(), visual_markers=True), visual_markers=True)
        self.assertEqual(result["received_frames"], 4)
        self.assertEqual(result["distinct_updates_after_first_marker"], 1)
        self.assertEqual(result["fresh_update_first_arrival_ms"]["render_start_to_decoded_ms"]["median"], 12)
        self.assertEqual(result["all_arrivals_including_repeats_ms"]["ndi_to_decoded_ms"]["median"], 2)

    def test_default_measurement_needs_no_marker_and_deduplicates_desktop_repeats(self):
        sender, receiver = self.inputs()
        for row in receiver:
            row.pop("render_marker_100ns")
        # Repeated frames map the same QPC event to slightly different UTC values.
        sender[1]["source_present_100ns"] = sender[0]["source_present_100ns"] + 1
        sender[3]["source_present_100ns"] = sender[2]["source_present_100ns"] - 1
        # The receiver missed frame 3: the next repeat is still its first arrival.
        rows = join_frames(sender, receiver[:2] + receiver[3:], ClockCorrection())
        result = summarize(rows)
        self.assertEqual([row["first_arrival_of_update"] for row in rows], [False, False, True])
        self.assertEqual(result["updates_after_initial_sample"], 1)
        self.assertAlmostEqual(result["primary_latency_ms"]["median"], 9.0001)
        self.assertNotIn("frames_without_valid_marker", result)
        self.assertNotIn("render_start_to_decoded_ms", result["fresh_update_first_arrival_ms"])
        self.assertNotIn("render_marker_100ns", rows[0])

    def test_static_desktop_has_no_new_frame_latency_instead_of_aging_repeats(self):
        sender, receiver = self.inputs()
        for sample in sender[1:]:
            sample["desktop_updated"] = False
            sample["refreshed"] = False
        result = summarize(join_frames(sender, receiver, ClockCorrection()))
        self.assertEqual(result["updates_after_initial_sample"], 0)
        self.assertIsNone(result["primary_latency_ms"])

    def test_outside_crop_updates_do_not_reset_the_retained_frame_identity(self):
        sender, receiver = self.inputs()
        # Desktop changes outside the crop are acquired but do not replace pixels.
        sender[1].update(desktop_updated=True, refreshed=False,
                         source_present_100ns=sender[0]["source_present_100ns"])
        sender[3].update(desktop_updated=True, refreshed=False,
                         source_present_100ns=sender[2]["source_present_100ns"])
        rows = join_frames(sender, receiver, ClockCorrection())
        self.assertEqual([row["desktop_update_frame"] for row in rows], [1, 1, 3, 3])
        self.assertEqual([row["first_arrival_of_update"] for row in rows], [False, False, True, False])
        self.assertEqual(rows[1]["desktop_update_to_decoded_ms"], 9)
        self.assertEqual(summarize(rows)["updates_after_initial_sample"], 1)

    def test_receiver_minus_sender_offset_is_subtracted_with_drift(self):
        clock = ClockCorrection(self.calibration(9_000_000_000, 1000), self.calibration(11_000_000_000, 1002))
        sender, receiver = self.inputs()
        for row in receiver:
            code = int(row["sender_timecode_100ns"])
            row["receiver_utc_100ns"] = str(code + 20_000 + round(clock.offset_ms(code) * 10000))
        rows = join_frames(sender, receiver, clock)
        for row in rows:
            self.assertAlmostEqual(row["ndi_to_decoded_ms"], 2)
            self.assertAlmostEqual(row["desktop_update_to_decoded_ms"], 5)
        with self.assertRaises(ValueError):
            clock.offset_ms(120_000_000)
        with self.assertRaises(ValueError):
            ClockCorrection(self.calibration(10, 0), self.calibration(5, 0))

    def test_missing_and_duplicate_timecodes_fail_instead_of_guessing(self):
        sender, receiver = self.inputs()
        with self.assertRaises(ValueError):
            join_frames(sender[1:], receiver, ClockCorrection())
        with self.assertRaises(ValueError):
            join_frames(sender + sender[:1], receiver, ClockCorrection())

    def test_unknown_source_time_and_negative_latency_are_not_hidden(self):
        sender, receiver = self.inputs()
        for sample in sender:
            sample["source_present_100ns"] = None
        receiver[0]["receiver_utc_100ns"] = str(int(receiver[0]["sender_timecode_100ns"]) - 1)
        result = summarize(join_frames(sender, receiver, ClockCorrection()))
        self.assertEqual(result["negative_ndi_samples"], 1)
        self.assertIsNone(result["fresh_update_first_arrival_ms"]["desktop_update_to_decoded_ms"])
        self.assertIsNone(distribution([]))
        with self.assertRaises(ValueError):
            distribution([float("nan")])


class MarkerTests(unittest.TestCase):
    def test_bgrx_and_uyvy_padded_rows_decode_and_detect_corruption(self):
        for fourcc, pixel_size in [(0x58524742, 4), (0x59565955, 2)]:
            stride = 512 * pixel_size + 32
            pixels = (C.c_uint8 * (stride * 512))()
            frame = Video(width=512, height=512, fourcc=fourcc, data=C.addressof(pixels), stride=stride)
            def paint(x, y, bit):
                offset = y * stride + x * pixel_size
                if pixel_size == 2:
                    pixels[offset + 1] = 235 if bit else 16
                else:
                    for channel in range(3):
                        pixels[offset + channel] = 255 if bit else 0
            value = 17_937_634_557_040_800
            for i in range(16):
                paint(44+i*24, 312, (0xA55A >> i) & 1)
            for i in range(64):
                bit = (value >> i) & 1
                x, y = 44 + (i % 16) * 24, 44 + (i // 16) * 24
                paint(x, y, bit)
                paint(x, y+128, 1-bit)
            self.assertEqual(marker_ticks(frame), value)
            paint(44, 172, value & 1)
            self.assertIsNone(marker_ticks(frame))
            frame.data = None
            self.assertIsNone(marker_ticks(frame))


if __name__ == "__main__":
    unittest.main()
