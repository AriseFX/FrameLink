"""Measure sender-UTC submission to decoded receive; clocks must be calibrated across hosts."""
import argparse
import ctypes as C
import csv
import json
import math
import os
from pathlib import Path
import platform
import statistics
import time

class Source(C.Structure):
    _fields_ = [('name', C.c_char_p), ('url', C.c_char_p)]

class FindCreate(C.Structure):
    _fields_ = [('show_local', C.c_bool), ('groups', C.c_char_p), ('extra_ips', C.c_char_p)]

class RecvCreate(C.Structure):
    _fields_ = [
        ('source', Source), ('color', C.c_int32), ('bandwidth', C.c_int32),
        ('fields', C.c_bool), ('name', C.c_char_p),
    ]

class Video(C.Structure):
    _fields_ = [
        ('width', C.c_int32), ('height', C.c_int32), ('fourcc', C.c_uint32),
        ('rate_n', C.c_int32), ('rate_d', C.c_int32), ('aspect', C.c_float),
        ('format', C.c_int32), ('timecode', C.c_int64), ('data', C.c_void_p),
        ('stride', C.c_int32), ('metadata', C.c_char_p), ('timestamp', C.c_int64),
    ]

assert C.sizeof(Video) == 72 and C.sizeof(RecvCreate) == 40
if os.name == 'nt':
    precise = C.WinDLL('kernel32').GetSystemTimePreciseAsFileTime
    precise.argtypes = [C.POINTER(C.c_uint64)]
    precise.restype = None

def now_ticks():
    if os.name != 'nt':
        return time.time_ns() // 100
    value = C.c_uint64()
    precise(C.byref(value))
    return value.value - 116444736000000000

def marker_ticks(frame):
    if frame.width < 440 or frame.height < 330 or frame.fourcc not in (0x41524742, 0x58524742, 0x59565955):
        return None
    pixel_size = 2 if frame.fourcc == 0x59565955 else 4
    if not frame.data or frame.stride < frame.width * pixel_size:
        return None
    def bit_at(x, y):
        if frame.fourcc == 0x59565955:  # UYVY: U0,Y0,V0,Y1; marker is grayscale.
            value = C.c_uint8.from_address(frame.data + y * frame.stride + x * 2 + 1).value
        else:
            pixel = C.string_at(frame.data + y * frame.stride + x * 4, 3)
            value = sum(pixel) / 3
        return 1 if value > 190 else 0 if value < 65 else -1
    signature = [bit_at(44 + bit * 24, 312) for bit in range(16)]
    if any(bit < 0 for bit in signature) or sum(bit << i for i, bit in enumerate(signature)) != 0xA55A:
        return None
    ticks = 0
    for i in range(64):
        x, y = 44 + (i % 16) * 24, 44 + (i // 16) * 24
        bit, inverse = bit_at(x, y), bit_at(x, y + 128)
        if bit < 0 or inverse < 0 or bit + inverse != 1:
            return None
        ticks |= bit << i
    return ticks

def summarize(values):
    ordered = sorted(values)
    def percentile(p):
        import math
        return ordered[max(0, math.ceil(p * len(ordered)) - 1)]
    return dict(n=len(values), min=min(values), median=statistics.median(values), p95=percentile(.95), p99=percentile(.99), max=max(values), mean=statistics.mean(values))

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--runtime', '--dll', dest='dll', required=True)
    ap.add_argument('--source', required=True)
    ap.add_argument('--seconds', type=float, default=30)
    ap.add_argument('--warmup', type=float, default=3)
    ap.add_argument('--color', choices=['bgrx', 'fastest'], default='bgrx')
    ap.add_argument('--output', required=True)
    ap.add_argument('--url')
    ap.add_argument('--marker', action='store_true')
    args = ap.parse_args()
    if not math.isfinite(args.seconds) or not 1 <= args.seconds <= 60:
        ap.error('--seconds must be between 1 and 60')
    if not math.isfinite(args.warmup) or not 0 <= args.warmup <= 30:
        ap.error('--warmup must be between 0 and 30')
    dll = Path(args.dll).resolve()
    output = Path(args.output).resolve()
    if output.suffix.lower() != '.json':
        ap.error('--output must end in .json')
    if output.exists() or output.with_suffix('.csv').exists():
        ap.error('Output already exists; choose a new report filename')
    output.parent.mkdir(parents=True, exist_ok=True)
    directory = os.add_dll_directory(str(dll.parent)) if os.name == 'nt' else None
    ndi = C.CDLL(str(dll))
    def bind(name, result, *parameters):
        function = getattr(ndi, name)
        function.restype, function.argtypes = result, list(parameters)
        return function
    init = bind('NDIlib_initialize', C.c_bool)
    destroy = bind('NDIlib_destroy', None)
    version = bind('NDIlib_version', C.c_char_p)
    find_create = bind('NDIlib_find_create_v2', C.c_void_p, C.POINTER(FindCreate))
    find_wait = bind('NDIlib_find_wait_for_sources', C.c_bool, C.c_void_p, C.c_uint32)
    find_sources = bind('NDIlib_find_get_current_sources', C.POINTER(Source), C.c_void_p, C.POINTER(C.c_uint32))
    find_destroy = bind('NDIlib_find_destroy', None, C.c_void_p)
    recv_create = bind('NDIlib_recv_create_v3', C.c_void_p, C.POINTER(RecvCreate))
    capture = bind('NDIlib_recv_capture_v3', C.c_int32, C.c_void_p, C.POINTER(Video), C.c_void_p, C.c_void_p, C.c_uint32)
    free = bind('NDIlib_recv_free_video_v2', None, C.c_void_p, C.POINTER(Video))
    recv_destroy = bind('NDIlib_recv_destroy', None, C.c_void_p)
    if not init():
        raise RuntimeError('NDI initialization failed')
    finder, receiver = None, None
    try:
        if not args.url:
            finder = find_create(C.byref(FindCreate(True, None, None)))
            if not finder:
                raise RuntimeError('NDI discovery creation failed')
        deadline = time.perf_counter() + 12
        chosen = Source(args.source.encode(), args.url.encode()) if args.url else None
        while chosen is None and time.perf_counter() < deadline:
            count = C.c_uint32()
            sources = find_sources(finder, C.byref(count))
            for i in range(count.value):
                name = sources[i].name or b''
                if name.decode('utf-8', errors='replace').casefold() == args.source.casefold():
                    chosen = Source(bytes(name), bytes(sources[i].url or b''))
                    break
            if chosen:
                break
            find_wait(finder, 500)
        if not chosen:
            raise RuntimeError(f'NDI source not found: {args.source}')
        color = 0 if args.color == 'bgrx' else 100
        receiver = recv_create(C.byref(RecvCreate(chosen, color, 100, False, b'FrameLink latency measurement')))
        if not receiver:
            raise RuntimeError('NDI receiver creation failed')
        started = time.perf_counter()
        rows, shape = [], None
        while time.perf_counter() - started < args.seconds + args.warmup:
            frame = Video()
            kind = capture(receiver, C.byref(frame), None, None, 500)
            received_ticks = now_ticks()
            elapsed = time.perf_counter() - started
            if kind != 1:
                continue
            try:
                current_shape = dict(
                    width=frame.width, height=frame.height,
                    fourcc=int(frame.fourcc).to_bytes(4, 'little').decode('ascii', errors='replace'),
                    rate_n=frame.rate_n, rate_d=frame.rate_d,
                )
                if rows and current_shape != shape:
                    raise RuntimeError('Video format changed during measurement; repeat under stable settings')
                shape = current_shape
                if elapsed >= args.warmup:
                    marker = marker_ticks(frame) if args.marker else None
                    rows.append((
                        elapsed, frame.timecode, received_ticks,
                        (received_ticks - frame.timecode) / 10000, marker,
                    ))
            finally:
                free(receiver, C.byref(frame))
        if not rows:
            raise RuntimeError('No measured video frames')
        with output.with_suffix('.csv').open('w', newline='', encoding='utf-8') as stream:
            writer = csv.writer(stream)
            writer.writerow([
                'elapsed_seconds', 'sender_timecode_100ns', 'receiver_utc_100ns',
                'uncalibrated_delta_ms', 'render_marker_100ns',
            ])
            writer.writerows(rows)
        report = dict(
            scope='uncalibrated_sender_submit_to_decoded_receive',
            receiver_host=platform.node(), runtime=version().decode(),
            source=chosen.name.decode(), url=(chosen.url or b'').decode(),
            receive_bandwidth=100, receive_color=args.color, video=shape,
            clock='Windows precise FILETIME / Linux CLOCK_REALTIME; uncalibrated cross-host deltas',
            cross_host_clock_correction_ms=None,
            warmup_seconds=args.warmup, measurement_seconds=args.seconds,
            frames=len(rows), received_fps=len(rows)/args.seconds,
            negative_samples=sum(row[3] < 0 for row in rows),
            uncalibrated_latency_ms=summarize([row[3] for row in rows]),
            excluded=['screen_capture_before_submission', 'application_processing_after_receive', 'display_scanout'],
            raw_samples=output.with_suffix('.csv').name,
        )
        if args.marker:
            seen, unique, all_markers = set(), [], []
            initial_marker = next((row[4] for row in rows if row[4] is not None), None)
            for row in rows:
                if row[4] is not None:
                    delta = (row[2] - row[4]) / 10000
                    all_markers.append(delta)
                    if row[4] != initial_marker and row[4] not in seen:
                        seen.add(row[4])
                        unique.append(delta)
            report['marker'] = dict(
                scope='test_pattern_render_start_to_decoded_frame',
                valid_frames=len(all_markers), invalid_frames=len(rows)-len(all_markers),
                unique_frames=len(unique), unique_fps=len(unique)/args.seconds,
                first_arrival_uncalibrated_ms=summarize(unique) if unique else None,
                all_arrivals_uncalibrated_ms=summarize(all_markers) if all_markers else None,
                excluded=['receiver_display_scanout'],
            )
        output.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
        print(json.dumps(report, indent=2), flush=True)
    finally:
        if receiver:
            recv_destroy(receiver)
        if finder:
            find_destroy(finder)
        destroy()
        if directory:
            directory.close()

if __name__ == '__main__':
    main()
