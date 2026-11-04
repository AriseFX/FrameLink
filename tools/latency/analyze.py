"""Measure desktop-update to decoded-memory latency without drawing a test window."""
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


def join_frames(sender_samples, receiver_rows, clock, visual_markers=False):
    traces = {}
    desktop_frame = None
    for sample in sorted(sender_samples, key=lambda sample: sample["frame"]):
        code = sample.get("ndi_timecode_100ns")
        if code is None:
            continue
        if code in traces:
            raise ValueError("Duplicate sender NDI timecode")
        if sample["desktop_updated"] and sample["refreshed"]:
            desktop_frame = sample["frame"]
        traces[code] = (sample, desktop_frame)
    joined, seen, initial_update = [], set(), None
    for row in receiver_rows:
        code = int(row["sender_timecode_100ns"])
        match = traces.get(code)
        if match is None:
            raise ValueError(f"No sender trace for received timecode {code}")
        trace, desktop_frame = match
        arrived = int(row["receiver_utc_100ns"])
        offset = clock.offset_ms(code)
        present = trace.get("source_present_100ns")
        marker = int(row["render_marker_100ns"]) if visual_markers and row.get("render_marker_100ns") else None
        # UTC pairing can vary slightly for repeats of the same DXGI update.
        # Use the originating sender frame, not equality of mapped UTC timestamps.
        update = marker if visual_markers else desktop_frame if present is not None else None
        if initial_update is None and update is not None:
            initial_update = update
        first = update is not None and update != initial_update and update not in seen
        if update is not None:
            seen.add(update)
        if visual_markers and first and not (trace["desktop_updated"] and trace["refreshed"]):
            raise ValueError("A new visual marker has no corresponding refreshed desktop frame")
        result = {
            **{key: value for key, value in row.items() if key != "render_marker_100ns"},
            "sender_frame": trace["frame"],
            "desktop_update_frame": desktop_frame,
            "first_arrival_of_update": first,
            "clock_correction_ms": offset,
            "source_clock_uncertainty_100ns": trace.get("source_clock_uncertainty_100ns"),
            "ndi_to_decoded_ms": (arrived - code) / 10000 - offset,
            "desktop_update_to_submit_ms": (code - present) / 10000 if present is not None else None,
            "desktop_update_to_decoded_ms": (arrived - present) / 10000 - offset if present is not None else None,
        }
        if visual_markers:
            result["render_marker_100ns"] = marker
            result["render_start_to_decoded_ms"] = (arrived - marker) / 10000 - offset if marker is not None else None
        joined.append(result)
    if not joined:
        raise ValueError("No received frames")
    return joined


def summarize(rows, visual_markers=False):
    keys = METRICS + (("render_start_to_decoded_ms",) if visual_markers else ())

    def metrics(samples):
        return {
            key: distribution(row[key] for row in samples if row[key] is not None)
            for key in keys
        }
    fresh = [row for row in rows if row["first_arrival_of_update"]]
    fresh_metrics = metrics(fresh)
    result = {
        "update_identity": "visual_marker" if visual_markers else "sender_desktop_update_frame",
        "primary_metric": "desktop_update_to_decoded_ms",
        "primary_latency_ms": fresh_metrics["desktop_update_to_decoded_ms"],
        "received_frames": len(rows),
        "updates_after_initial_sample": len(fresh),
        "frames_without_desktop_timestamp": sum(row["desktop_update_to_decoded_ms"] is None for row in rows),
        "negative_ndi_samples": sum(row["ndi_to_decoded_ms"] < 0 for row in rows),
        "negative_desktop_samples": sum(row["desktop_update_to_decoded_ms"] is not None and row["desktop_update_to_decoded_ms"] < 0 for row in rows),
        "all_arrivals_including_repeats_ms": metrics(rows),
        "fresh_update_first_arrival_ms": fresh_metrics,
    }
    if visual_markers:
        result["distinct_updates_after_first_marker"] = len(fresh)
        result["frames_without_valid_marker"] = sum(not row.get("render_marker_100ns") for row in rows)
    return result


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
    parser.add_argument("--marker", action="store_true", help="Also analyze optional visual markers from historical test-window recordings")
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
        rows = join_frames(sender["samples"], csv.DictReader(stream), clock, args.marker)
    report = {
        "startpoint": "DXGI LastPresentTime: desktop update available for capture",
        "endpoint": "Complete decoded CPU pixel buffer returned by NDI, before application processing",
        "video": receiver["video"],
        "sender_config": sender["config"],
        "measurement_seconds": receiver["measurement_seconds"],
        "clock_offset_uncertainty_bound_ms": clock.uncertainty_ms,
        **summarize(rows, args.marker),
        "notes": [
            "All latency units are milliseconds; sender timecodes are Unix UTC in 100 ns units.",
            "Source timestamp is the whole-monitor DXGI timestamp of the update copied into the crop; outside-only updates retain the previous crop timestamp.",
            "The initial update is omitted from first-arrival statistics because it may have arrived during warmup.",
            "Updates are identified from sender refresh events. Older builds copy every desktop update; region-aware builds skip updates proven to be outside the crop. Coalesced dirty rectangles can still trigger a copy of unchanged pixels.",
            "First-arrival statistics describe delivered updates. A null primary latency means no eligible new desktop updates were measured.",
            "Primary latency excludes source application drawing and desktop composition before LastPresentTime.",
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
