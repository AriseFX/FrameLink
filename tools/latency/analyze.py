"""Join FrameLink sender traces with decoded frames and calibrated receiver clocks."""
import argparse
import csv
import json
import math
from pathlib import Path
import statistics

METRICS = (
    "ndi_to_decoded_ms",
    "desktop_update_to_submit_ms",
    "desktop_update_to_decoded_ms",
    "render_start_to_decoded_ms",
)


def distribution(values):
    values = list(values)
    if not values:
        return None
    if not all(math.isfinite(value) for value in values):
        raise ValueError("Non-finite latency sample")
    ordered = sorted(values)
    return {
        "n": len(values),
        "min": ordered[0],
        "median": statistics.median(ordered),
        "p95": ordered[math.ceil(len(ordered) * .95) - 1],
        "p99": ordered[math.ceil(len(ordered) * .99) - 1],
        "max": ordered[-1],
        "mean": statistics.mean(ordered),
    }


class ClockCorrection:
    """Receiver-minus-sender UTC offset, interpolated within two observations."""

    def __init__(self, before=None, after=None):
        if (before is None) != (after is None):
            raise ValueError("Both clock calibrations are required")
        self.before = before["best"] if before else None
        self.after = after["best"] if after else None
        self.uncertainty_ms = 0
        if before:
            for item in (self.before, self.after):
                if not math.isfinite(item["offset_ms"]) or not math.isfinite(item["rtt_ms"]) or item["rtt_ms"] < 0 or item["t4"] < item["t1"]:
                    raise ValueError("Invalid clock observation")
            self.start = (self.before["t1"] + self.before["t4"]) // 2
            self.end = (self.after["t1"] + self.after["t4"]) // 2
            if self.end <= self.start:
                raise ValueError("Clock observations are not ordered")
            self.uncertainty_ms = max(
                before["offset_uncertainty_bound_ms"],
                after["offset_uncertainty_bound_ms"],
            )
            for observation in (before, after):
                error = observation["offset_uncertainty_bound_ms"]
                if not math.isfinite(error) or error < 0:
                    raise ValueError("Invalid clock uncertainty")

    def offset_ms(self, sender_timecode):
        if self.before is None:
            return 0
        weight = (sender_timecode * 100 - self.start) / (self.end - self.start)
        if not 0 <= weight <= 1:
            raise ValueError("Clock observations must bracket every measured frame")
        return self.before["offset_ms"] + weight * (
            self.after["offset_ms"] - self.before["offset_ms"]
        )


def join_frames(sender_samples, receiver_rows, clock):
    traces = {}
    for sample in sender_samples:
        code = sample.get("ndi_timecode_100ns")
        if code is None:
            continue
        if code in traces:
            raise ValueError("Duplicate sender NDI timecode")
        traces[code] = sample
    joined, seen, initial_marker = [], set(), None
    for row in receiver_rows:
        code = int(row["sender_timecode_100ns"])
        trace = traces.get(code)
        if trace is None:
            raise ValueError(f"No sender trace for received timecode {code}")
        arrived = int(row["receiver_utc_100ns"])
        offset = clock.offset_ms(code)
        present = trace.get("source_present_100ns")
        marker = int(row["render_marker_100ns"]) if row.get("render_marker_100ns") else None
        if initial_marker is None and marker is not None:
            initial_marker = marker
        first = marker is not None and marker != initial_marker and marker not in seen
        if marker is not None:
            seen.add(marker)
        if first and not (trace["desktop_updated"] and trace["refreshed"]):
            raise ValueError("A new visual marker has no corresponding refreshed desktop frame")
        joined.append({
            **row,
            "sender_frame": trace["frame"],
            "first_arrival_of_update": first,
            "clock_correction_ms": offset,
            "source_clock_uncertainty_100ns": trace.get("source_clock_uncertainty_100ns"),
            "ndi_to_decoded_ms": (arrived - code) / 10000 - offset,
            "desktop_update_to_submit_ms": (code - present) / 10000 if present is not None else None,
            "desktop_update_to_decoded_ms": (arrived - present) / 10000 - offset if present is not None else None,
            "render_start_to_decoded_ms": (arrived - marker) / 10000 - offset if marker is not None else None,
        })
    if not joined:
        raise ValueError("No received frames")
    return joined


def summarize(rows):
    def metrics(samples):
        return {
            key: distribution(row[key] for row in samples if row[key] is not None)
            for key in METRICS
        }
    fresh = [row for row in rows if row["first_arrival_of_update"]]
    return {
        "received_frames": len(rows),
        "distinct_updates_after_first_marker": len(fresh),
        "frames_without_valid_marker": sum(not row.get("render_marker_100ns") for row in rows),
        "negative_ndi_samples": sum(row["ndi_to_decoded_ms"] < 0 for row in rows),
        "all_arrivals_including_repeats_ms": metrics(rows),
        "fresh_update_first_arrival_ms": metrics(fresh),
    }


def read_json(path):
    return Path(path).read_text(encoding="utf-8-sig")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sender", type=Path, required=True)
    parser.add_argument("--receiver", type=Path, required=True)
    parser.add_argument("--samples", type=Path, help="Defaults to the receiver report's .csv sibling")
    parser.add_argument("--clock-before", type=Path)
    parser.add_argument("--clock-after", type=Path)
    parser.add_argument("--same-clock", action="store_true", help="Only for sender and receiver on the same OS instance")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.output.suffix.lower() != ".json":
        parser.error("--output must end in .json")
    if args.same_clock:
        if args.clock_before or args.clock_after:
            parser.error("--same-clock cannot be combined with cross-host calibration")
        clock = ClockCorrection()
    else:
        if not args.clock_before or not args.clock_after:
            parser.error("Provide both clock observations, or explicitly use --same-clock")
        clock = ClockCorrection(
            json.loads(read_json(args.clock_before)), json.loads(read_json(args.clock_after))
        )
    if args.output.exists() or args.output.with_suffix(".csv").exists():
        parser.error("Output already exists; choose a new report filename")
    sender, receiver = json.loads(read_json(args.sender)), json.loads(read_json(args.receiver))
    if not sender["measurement_succeeded"] or sender["missing_diagnostic_samples"]:
        raise ValueError("Sender failed or has missing diagnostic traces")
    with (args.samples or args.receiver.with_suffix(".csv")).open(newline="", encoding="utf-8-sig") as stream:
        rows = join_frames(sender["samples"], csv.DictReader(stream), clock)
    report = {
        "endpoint": "Complete decoded CPU pixel buffer returned by NDI, before marker inspection",
        "video": receiver["video"],
        "sender_config": sender["config"],
        "measurement_seconds": receiver["measurement_seconds"],
        "clock_offset_uncertainty_bound_ms": clock.uncertainty_ms,
        **summarize(rows),
        "notes": [
            "All latency units are milliseconds; sender timecodes are Unix UTC in 100 ns units.",
            "Source desktop timestamp describes the entire monitor, not only the cropped content.",
            "The first visible marker is omitted from fresh-update statistics because it may have arrived during warmup.",
            "First-arrival statistics describe delivered updates, not delivery of every rendered update.",
            "Cross-host calibration assumes no intermediate clock steps. Negative samples require investigation.",
            "Native UYVY reception excludes RGB conversion; compare matching pixel formats for equal work.",
            "Receiver display, application-owned copies, AI preprocessing, GPU upload and inference are excluded.",
        ],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    with args.output.with_suffix(".csv").open("w", newline="", encoding="utf-8") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    print(json.dumps(report, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
