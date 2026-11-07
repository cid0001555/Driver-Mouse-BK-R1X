#!/usr/bin/env python3
"""
Fire Phoenix BK-R1X — WebHID Bundle AST/Regex Protocol Extractor
================================================================
Scans downloaded Next.js / Webpack JS bundles from `https://bk-r1x.yjx2012.com/`
(or fetches them directly with `--fetch`) and automatically extracts:
  1. USB/2.4GHz VID, PID, UsagePage, and Usage filter tables.
  2. HID report buffer sizes & chunk framing constants.
  3. Command opcodes (TX `0x55` / ReportID `0x06`, RX `0xAA` telemetry/ACKs).
  4. Register/byte offsets (DPI stages, Polling Rate, LOD, Debounce, PAW3311 flags,
     Sleep timeout, LED modes, Button mappings, Macro structure).
  5. Packet checksum/authentication strategy analysis.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys
import urllib.request
from dataclasses import dataclass, asdict
from typing import Dict, List, Optional


SITE_ORIGIN = "https://bk-r1x.yjx2012.com"


@dataclass
class DeviceFilterMatch:
    source_file: str
    vendor_id_dec: Optional[int]
    vendor_id_hex: Optional[str]
    product_id_dec: Optional[int]
    product_id_hex: Optional[str]
    usage_page_dec: Optional[int]
    usage_page_hex: Optional[str]
    usage_dec: Optional[int]
    usage_hex: Optional[str]
    inferred_mode: str


@dataclass
class CommandSignature:
    method_name: str
    source_file: str
    report_id: int
    magic_header: int
    opcode: int
    opcode_hex: str
    auth_bytes_hex: List[str]
    buffer_len: int
    raw_assignments: Dict[int, str]


# Regex patterns targeting minified Next.js / Webpack WebHID bundles
RE_SCRIPT_SRC = re.compile(r'src="(/_next/static/chunks/[^"]+\.js)"')
RE_DYNAMIC_CHUNKS = re.compile(
    r'static/chunks/"\+.\+"\."\+\{([^}]+)\}\[.\]\+"\.js"'
)
RE_DYNAMIC_PAIR = re.compile(r'(\d+):"([0-9a-f]+)"')

RE_FILTER_OBJ = re.compile(
    r"\{[^{}]*?(?:vendorId|usagePage)[^{}]*?\}"
)
RE_KV_INT = re.compile(r"\b(vendorId|productId|usagePage|usage)\s*:\s*(\d+|0x[0-9a-fA-F]+)")

RE_CONNECT_MODE = re.compile(
    r"getConnectMode\(\)\{return\s*(\d+)\s*==\s*this\.getVendorId\w*\(\)\s*\?\s*\"([^\"]+)\"\s*:\s*\"([^\"]+)\""
)

RE_REPORT_ALLOC = re.compile(r"Array\((\d+)\)\.fill\(0\)")

RE_ASYNC_METHOD = re.compile(
    r"async\s+([A-Za-z0-9_$]+)\s*\(([^)]*)\)\s*\{([^{}]*(?:\{[^{}]*\}[^{}]*)*)\}"
)

RE_BYTE_ASSIGN = re.compile(r"\b([a-zA-Z_$][a-zA-Z0-9_$]*)\[(\d+)\]\s*=\s*([^,;]+)")

RE_RX_HEADER = re.compile(
    r"(\d+)\s*==\s*[a-zA-Z_$]\[\s*0\s*\]\s*&&\s*(\d+)\s*==\s*[a-zA-Z_$]\[\s*1\s*\]"
)

RE_SENSOR_BIT = re.compile(
    r"\(\s*(\d+)\s*&\s*[a-zA-Z_$]+\s*\)\s*!=\s*0.*?(?:H|changeSensorFlag)\(\s*\1\b"
)


def parse_int(val: str) -> int:
    return int(val, 16) if val.lower().startswith("0x") else int(val, 10)


def fetch_bundles(target_dir: pathlib.Path) -> List[pathlib.Path]:
    target_dir.mkdir(parents=True, exist_ok=True)
    req = urllib.request.Request(
        SITE_ORIGIN + "/",
        headers={"User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/130.0.0.0"},
    )
    with urllib.request.urlopen(req, timeout=15) as resp:
        html = resp.read().decode("utf-8", errors="replace")

    chunk_paths = sorted(set(RE_SCRIPT_SRC.findall(html)))
    downloaded: List[pathlib.Path] = []

    for rel in chunk_paths:
        out_name = rel.split("/chunks/")[-1].replace("/", "_")
        out_path = target_dir / out_name
        with urllib.request.urlopen(SITE_ORIGIN + rel, timeout=15) as c_resp:
            data = c_resp.read()
        out_path.write_bytes(data)
        downloaded.append(out_path)

        # Also inspect webpack runtime chunk for lazy-loaded chunks (e.g. default config JSON)
        if out_name.startswith("webpack-"):
            txt = data.decode("utf-8", errors="replace")
            for m in RE_DYNAMIC_CHUNKS.finditer(txt):
                for cid, chash in RE_DYNAMIC_PAIR.findall(m.group(1)):
                    dyn_rel = f"/_next/static/chunks/{cid}.{chash}.js"
                    dyn_out = target_dir / f"{cid}.{chash}.js"
                    with urllib.request.urlopen(SITE_ORIGIN + dyn_rel, timeout=15) as d_resp:
                        dyn_out.write_bytes(d_resp.read())
                    downloaded.append(dyn_out)

    return downloaded


def extract_filters(filename: str, text: str, usb_vid: Optional[int]) -> List[DeviceFilterMatch]:
    results: List[DeviceFilterMatch] = []
    seen = set()
    for obj_match in RE_FILTER_OBJ.finditer(text):
        snippet = obj_match.group(0)
        kvs = {k: parse_int(v) for k, v in RE_KV_INT.findall(snippet)}
        if not kvs:
            continue
        vid = kvs.get("vendorId")
        pid = kvs.get("productId")
        up = kvs.get("usagePage")
        u = kvs.get("usage")
        key = (vid, pid, up, u)
        if key in seen:
            continue
        seen.add(key)

        if vid is not None and usb_vid is not None:
            mode = "USB-C Wired" if vid == usb_vid else "2.4GHz Wireless Dongle"
        elif vid is not None:
            mode = "Hardware VID/PID Filter"
        else:
            mode = "Generic Vendor Collection Filter"

        results.append(
            DeviceFilterMatch(
                source_file=filename,
                vendor_id_dec=vid,
                vendor_id_hex=f"0x{vid:04X}" if vid is not None else None,
                product_id_dec=pid,
                product_id_hex=f"0x{pid:04X}" if pid is not None else None,
                usage_page_dec=up,
                usage_page_hex=f"0x{up:04X}" if up is not None else None,
                usage_dec=u,
                usage_hex=f"0x{u:04X}" if u is not None else None,
                inferred_mode=mode,
            )
        )
    return results


def extract_commands(filename: str, text: str) -> List[CommandSignature]:
    commands: List[CommandSignature] = []
    for m in RE_ASYNC_METHOD.finditer(text):
        method_name, _args, body = m.group(1), m.group(2), m.group(3)
        if "sendDeviceData" not in body and "getDeviceData" not in body:
            continue

        alloc_match = RE_REPORT_ALLOC.search(body)
        buf_len = int(alloc_match.group(1)) if alloc_match else 65

        assigns: Dict[int, str] = {}
        for var_name, idx_str, expr in RE_BYTE_ASSIGN.findall(body):
            idx = int(idx_str)
            if idx not in assigns:
                assigns[idx] = expr.strip()

        # Check inline array packets such as sendDeviceData([0,85,13,...]) or getDeviceData([6,12,...])
        inline_arr = re.findall(
            r"(?:sendDeviceData|getDeviceData)\(\[([^\]]+)\]\)", body
        )

        if 1 in assigns and 2 in assigns:
            try:
                rpt_id = parse_int(assigns.get(0, "0"))
                magic = parse_int(assigns[1])
                opcode = parse_int(assigns[2])
            except ValueError:
                continue
            auth = []
            for k in (3, 4, 5):
                if k in assigns and assigns[k].isdigit():
                    auth.append(f"0x{int(assigns[k]):02X}")
            commands.append(
                CommandSignature(
                    method_name=method_name,
                    source_file=filename,
                    report_id=rpt_id,
                    magic_header=magic,
                    opcode=opcode,
                    opcode_hex=f"0x{opcode:02X}",
                    auth_bytes_hex=auth,
                    buffer_len=buf_len,
                    raw_assignments=assigns,
                )
            )

        for arr_expr in inline_arr:
            parts = [p.strip() for p in arr_expr.split(",")]
            if len(parts) >= 3 and parts[0].isdigit() and parts[1].isdigit():
                first = int(parts[0])
                second = int(parts[1])
                if first == 0 and second == 85 and parts[2].isdigit():
                    op = int(parts[2])
                    commands.append(
                        CommandSignature(
                            method_name=f"{method_name} (inline)",
                            source_file=filename,
                            report_id=0,
                            magic_header=0x55,
                            opcode=op,
                            opcode_hex=f"0x{op:02X}",
                            auth_bytes_hex=[
                                f"0x{int(x):02X}" for x in parts[3:6] if x.isdigit()
                            ],
                            buffer_len=65,
                            raw_assignments={i: v for i, v in enumerate(parts)},
                        )
                    )
                elif first == 6:
                    commands.append(
                        CommandSignature(
                            method_name=f"{method_name} (reportId=0x06)",
                            source_file=filename,
                            report_id=6,
                            magic_header=0x06,
                            opcode=second,
                            opcode_hex=f"0x{second:02X}",
                            auth_bytes_hex=[],
                            buffer_len=65,
                            raw_assignments={i: v for i, v in enumerate(parts)},
                        )
                    )
    return commands


def detect_checksum_strategy(all_text: str, commands: List[CommandSignature]) -> Dict[str, object]:
    has_crc_table = bool(re.search(r"0x1021|0x8005|crc16|crcTable", all_text, re.I))
    has_sum_comp = bool(re.search(r"0x55\s*-\s*[a-zA-Z_$]|255\s*-\s*sum", all_text))
    max_written_index = max(
        (max(c.raw_assignments.keys(), default=0) for c in commands),
        default=0,
    )
    return {
        "detected_strategy": (
            "Crc16"
            if has_crc_table
            else "SumComplement"
            if has_sum_comp
            else "None (Magic Header 0x55 + Auth Tokens 0xA5/0xAE, Zero-Padded 64B Payload)"
        ),
        "tx_magic_byte": "0x55 (85)",
        "rx_magic_byte": "0xAA (170)",
        "read_auth_token": "0xA5 0x0B",
        "write_keys_auth_token": "0xA5 0x22",
        "write_config_auth_token": "0xAE 0x0A",
        "highest_tx_index_assigned": max_written_index,
        "trailing_checksum_byte_written": max_written_index >= 63,
    }


def analyze_directory(bundle_dir: pathlib.Path) -> Dict[str, object]:
    js_files = sorted(bundle_dir.glob("*.js"))
    if not js_files:
        raise FileNotFoundError(f"No .js files found in {bundle_dir}")

    usb_vid: Optional[int] = None
    texts: Dict[str, str] = {}
    for path in js_files:
        txt = path.read_text(encoding="utf-8", errors="replace")
        texts[path.name] = txt
        m = RE_CONNECT_MODE.search(txt)
        if m and m.group(2).upper() == "USB":
            usb_vid = int(m.group(1))

    filters: List[DeviceFilterMatch] = []
    commands: List[CommandSignature] = []
    report_allocs: set[int] = set()
    rx_headers: set[tuple[str, str]] = set()
    sensor_bits: set[int] = set()
    default_device_json: Optional[Dict[str, object]] = None

    for fname, txt in texts.items():
        filters.extend(extract_filters(fname, txt, usb_vid))
        commands.extend(extract_commands(fname, txt))
        for n in RE_REPORT_ALLOC.findall(txt):
            val = int(n)
            if val in (16, 20, 32, 64, 65, 4032, 4096):
                report_allocs.add(val)
        for b0, b1 in RE_RX_HEADER.findall(txt):
            rx_headers.add((f"0x{int(b0):02X}", f"0x{int(b1):02X}"))
        for bit in RE_SENSOR_BIT.findall(txt):
            sensor_bits.add(int(bit))

        # Check for embedded default hardware profile JSON (chunk 968)
        cfg_match = re.search(
            r"JSON\.parse\('(\{\"name\":\"BK-R1X[^\']+)'\)", txt
        )
        if cfg_match:
            default_device_json = json.loads(cfg_match.group(1))

    combined = "\n".join(texts.values())
    checksum_info = detect_checksum_strategy(combined, commands)

    return {
        "target": "Fire Phoenix BK-R1X (bk-r1x.yjx2012.com)",
        "usb_wired_vid_detected": f"0x{usb_vid:04X}" if usb_vid else None,
        "hid_filters": [asdict(f) for f in filters],
        "buffer_constants_bytes": sorted(report_allocs),
        "wire_framing": {
            "webhid_array_size": 65,
            "report_id_byte_offset": 0,
            "hid_payload_bytes": 64,
            "macro_flash_bytes": 4096,
            "macro_chunk_data_bytes": 56,
        },
        "rx_notification_headers": sorted(list(rx_headers)),
        "paw3311_sensor_flag_bits": {
            f"0x{b:02X}": (
                "Ripple Control"
                if b == 1
                else "Angle Snapping (Linear Correction)"
                if b == 16
                else "Motion Sync"
                if b == 32
                else f"Bit {b}"
            )
            for b in sorted(sensor_bits)
        },
        "commands": [asdict(c) for c in commands],
        "checksum_analysis": checksum_info,
        "default_hardware_descriptor": default_device_json,
    }


def print_human_report(report: Dict[str, object]) -> None:
    print("=" * 78)
    print(f" BK-R1X PROTOCOL EXTRACTION REPORT — {report['target']}")
    print("=" * 78)

    print("\n[1] USB / 2.4GHz HID DEVICE FILTERS")
    for f in report["hid_filters"]:
        print(
            f"  • {f['inferred_mode']:<28} "
            f"VID={f['vendor_id_hex'] or '*':<6} "
            f"PID={f['product_id_hex'] or '*':<6} "
            f"UsagePage={f['usage_page_hex'] or '*':<6} "
            f"Usage={f['usage_hex'] or '*':<6} "
            f"({f['source_file']})"
        )

    wf = report["wire_framing"]
    print("\n[2] REPORT LENGTH & MEMORY CONSTANTS")
    print(f"  • Buffer sizes found : {report['buffer_constants_bytes']}")
    print(
        f"  • HID Output Frame   : {wf['webhid_array_size']} bytes "
        f"(1B ReportID [0x00] + {wf['hid_payload_bytes']}B Payload)"
    )
    print(
        f"  • Macro Flash Space  : {wf['macro_flash_bytes']} bytes "
        f"(streamed in {wf['macro_chunk_data_bytes']}B chunks)"
    )

    print("\n[3] EXTRACTED COMMAND OPCODES")
    for cmd in report["commands"]:
        auth_str = " ".join(cmd["auth_bytes_hex"]) if cmd["auth_bytes_hex"] else "-"
        print(
            f"  • {cmd['opcode_hex']}  rpt=0x{cmd['report_id']:02X}  "
            f"hdr=0x{cmd['magic_header']:02X}  auth=[{auth_str:<14}]  "
            f"fn={cmd['method_name']}"
        )

    print("\n[4] ASYNC / RX INPUT REPORT HEADERS")
    for h0, h1 in report["rx_notification_headers"]:
        print(f"  • [{h0}, {h1}]")

    print("\n[5] PAW3311 SENSOR_FLAG BITMASK")
    for mask_hex, label in report["paw3311_sensor_flag_bits"].items():
        print(f"  • {mask_hex} -> {label}")

    print("\n[6] CHECKSUM & AUTHENTICATION ANALYSIS")
    for k, v in report["checksum_analysis"].items():
        print(f"  • {k:<32}: {v}")
    print("=" * 78)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Extract WebHID VID/PID, opcodes, register offsets, and checksum rules from bk-r1x.yjx2012.com bundles."
    )
    parser.add_argument(
        "bundle_dir",
        nargs="?",
        default="./bundles",
        help="Directory containing downloaded .js bundle chunks (or destination when --fetch is passed).",
    )
    parser.add_argument(
        "--fetch",
        action="store_true",
        help="Download live Next.js chunks directly from https://bk-r1x.yjx2012.com/ before scanning.",
    )
    parser.add_argument(
        "--json",
        dest="json_out",
        help="Optional file path to write the full machine-readable JSON extraction report.",
    )
    args = parser.parse_args()

    target_dir = pathlib.Path(args.bundle_dir)
    if args.fetch:
        print(f"[*] Fetching live bundles from {SITE_ORIGIN} -> {target_dir} ...")
        fetch_bundles(target_dir)

    report = analyze_directory(target_dir)
    print_human_report(report)

    if args.json_out:
        out_p = pathlib.Path(args.json_out)
        out_p.write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8")
        print(f"\n[+] Saved JSON protocol specification to {out_p}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
