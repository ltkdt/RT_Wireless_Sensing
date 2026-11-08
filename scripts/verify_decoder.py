#!/usr/bin/env python3
"""Verify the Rust V8 decoder against ESPectre's Python reference parser.

The script runs the Rust CLI in JSON mode, decodes the same capture with
``tools.lib.csi_io.parse_csi_record``, and compares every metadata field plus a
whole-payload checksum. It is a development tool, not part of the Rust build.

Run it with a Python that can import ESPectre's tools package, usually the
repository's virtualenv:

    /home/ltkdt/Development/espectre/.venv/bin/python scripts/verify_decoder.py

Exit status is 0 on parity and 1 on any difference.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

# Repository root: this file lives in <root>/scripts/.
PROJECT_ROOT = Path(__file__).resolve().parents[1]

# FNV-1a 64 constants, matching the Rust CLI's payload checksum.
FNV1A64_OFFSET_BASIS = 0xCBF29CE484222325
FNV1A64_PRIME = 0x100000001B3
U64_MASK = 0xFFFFFFFFFFFFFFFF


def fnv1a64(data: bytes) -> int:
    """Return the FNV-1a 64-bit hash of *data*."""
    value = FNV1A64_OFFSET_BASIS
    for byte in data:
        value ^= byte
        value = (value * FNV1A64_PRIME) & U64_MASK
    return value


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--capture",
        type=Path,
        default=PROJECT_ROOT / "capture_v8.bin",
        help="raw capture to decode (default: <repo>/capture_v8.bin)",
    )
    parser.add_argument(
        "--espectre-root",
        type=Path,
        default=Path("/home/ltkdt/Development/espectre"),
        help="ESPectre checkout containing tools/lib/csi_io.py",
    )
    parser.add_argument(
        "--json-output",
        type=Path,
        default=None,
        help="optional path to save the Rust JSON Lines output",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()

    # Make `tools.lib.csi_io` importable without installing ESPectre.
    sys.path.insert(0, str(args.espectre_root))
    try:
        from tools.lib import csi_io
    except Exception as error:  # noqa: BLE001 - report any import failure clearly
        print(
            f"error: cannot import ESPectre tools from {args.espectre_root}: {error}",
            file=sys.stderr,
        )
        print(
            "hint: run this with the ESPectre virtualenv Python, for example "
            "<espectre>/.venv/bin/python scripts/verify_decoder.py",
            file=sys.stderr,
        )
        return 2

    data = args.capture.read_bytes()

    # Decode the same file with Rust. `--quiet` keeps cargo progress off stderr;
    # only stdout is JSON Lines.
    completed = subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "--bin",
            "decode",
            "--",
            str(args.capture),
            "--json",
        ],
        cwd=PROJECT_ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    if args.json_output is not None:
        args.json_output.write_text(completed.stdout)

    lines = [json.loads(line) for line in completed.stdout.splitlines() if line.strip()]
    rust_records = [item for item in lines if item.get("type") == "record"]
    rust_summary = next(
        (item for item in lines if item.get("type") == "summary"), None
    )
    if rust_summary is None:
        print("error: Rust output has no summary object", file=sys.stderr)
        return 1

    offset = 0
    index = 0
    payload = bytearray()
    mismatches: list[tuple[int, str, object, object]] = []
    previous_sequence: int | None = None
    first_sequence: int | None = None
    last_sequence: int | None = None
    gaps = 0
    missing = 0
    comparisons = 0

    while offset < len(data):
        (
            prefix_magic,
            prefix_version,
            prefix_record_version,
            prefix_header_len,
            session_id,
            stream_sequence,
            record_len,
            prefix_flags,
            fresh_total,
            drop_total,
            backpressure_total,
        ) = csi_io.RAW_CSI_HTTP_FRAME_STRUCT.unpack_from(data, offset)

        frame = data[offset : offset + prefix_header_len + record_len]
        packet, next_offset = csi_io.parse_csi_record(
            frame, prefix_header_len, derive_complex=False
        )
        if packet is None or next_offset != len(frame):
            print(
                f"error: Python reference rejected frame {index} at byte {offset}",
                file=sys.stderr,
            )
            return 1

        # Raw flags are not exposed on CSIPacket, so unpack the header again.
        record = csi_io.CSI_HEADER_STRUCT.unpack_from(frame, prefix_header_len)
        record_flags = record[4]

        payload.extend(packet.iq_raw.tobytes())

        rust = rust_records[index]
        expected = {
            "index": index,
            "stream_sequence": stream_sequence,
            "seq_num": packet.seq_num,
            "session_id": session_id.hex(),
            "device_id": packet.device_id or 0,
            "device_ticks_us": packet.device_ticks_us or 0,
            "wifi_rx_ts_us": packet.wifi_rx_ts_us,
            "wifi_rx_start_ts_ns": packet.wifi_rx_start_ts_ns,
            "channel": packet.channel,
            "rssi_dbm": packet.rssi_dbm,
            "noise_floor_dbm": packet.noise_floor_dbm,
            "chip": packet.chip,
            "phy_mode": packet.phy_mode,
            "ltf_type": packet.ltf_type,
            "channel_width": packet.channel_width,
            "num_subcarriers": packet.num_subcarriers,
            "csi_len_bytes": int(packet.iq_raw.size),
            "flags": record_flags,
            "first_word_invalid": bool(record_flags & 0x01),
            "fresh": bool(packet.csi_fresh),
            "fresh_record_total": fresh_total,
            "raw_drop_total": drop_total,
            "raw_send_backpressure_total": backpressure_total,
        }
        for key, want in expected.items():
            found = rust.get(key)
            if found != want:
                mismatches.append((index, key, want, found))
        comparisons += len(expected)

        if previous_sequence is not None:
            if stream_sequence > previous_sequence + 1:
                gaps += 1
                missing += stream_sequence - previous_sequence - 1
        else:
            first_sequence = stream_sequence
        previous_sequence = stream_sequence
        last_sequence = stream_sequence

        offset += prefix_header_len + record_len
        index += 1

    payload_hash = fnv1a64(bytes(payload))

    summary_checks = {
        "frames": index,
        "first_stream_sequence": first_sequence,
        "last_stream_sequence": last_sequence,
        "sequence_gaps": gaps,
        "missing_records": missing,
        "payload_fnv1a64": f"0x{payload_hash:016x}",
    }

    if index != len(rust_records):
        mismatches.append((-1, "record_count", index, len(rust_records)))
    for key, want in summary_checks.items():
        found = rust_summary.get(key)
        if found != want:
            mismatches.append((-1, f"summary.{key}", want, found))
    comparisons += len(summary_checks)

    print(f"capture        : {args.capture}")
    print(f"records        : python {index}, rust {len(rust_records)}")
    print(f"payload bytes  : {len(payload)}")
    print(f"payload fnv1a64: 0x{payload_hash:016x}")
    print(f"compared fields: {comparisons}")
    print(f"mismatches     : {len(mismatches)}")
    for frame_index, key, want, found in mismatches[:20]:
        print(f"  frame {frame_index}: {key}: python={want!r} rust={found!r}")

    if mismatches:
        print("FAIL")
        return 1
    print("PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
