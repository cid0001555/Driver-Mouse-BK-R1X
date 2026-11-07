#!/usr/bin/env python3
"""Static Next.js / Webpack WebHID protocol extractor for `bk-r1x.yjx2012.com`."""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
import json
import pathlib
import re
import sys
import urllib.request

SITE_ORIGIN = "https://bk-r1x.yjx2012.com"
HTTP_HEADERS = {
    "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/130.0.0.0",
}
KNOWN_REPORT_SIZES = frozenset({16, 20, 32, 64, 65, 4032, 4096})
SENSOR_BIT_LABELS = {
    1: "Ripple Control",
    16: "Angle Snapping (Linear Correction)",
    32: "Motion Sync",
}

RE_SCRIPT_SRC = re.compile(r'src="(/_next/static/chunks/[^"]+\.js)"')
RE_DYNAMIC_CHUNKS = re.compile(r'static/chunks/"\+.\+"\."\+\{([^}]+)\}\[.\]\+"\.js"')
RE_DYNAMIC_PAIR = re.compile(r'(\d+):"([0-9a-f]+)"')
RE_FILTER_OBJ = re.compile(r"\{[^{}]*?(?:vendorId|usagePage)[^{}]*?\}")
RE_KV_INT = re.compile(r"\b(vendorId|productId|usagePage|usage)\s*:\s*(\d+|0x[0-9a-fA-F]+)")
RE_CONNECT_MODE = re.compile(
    r'getConnectMode\(\)\{return\s*(\d+)\s*==\s*this\.getVendorId\w*\(\)\s*\?\s*"([^"]+)"\s*:\s*"([^"]+)"'
)
RE_REPORT_ALLOC = re.compile(r"Array\((\d+)\)\.fill\(0\)")
RE_ASYNC_METHOD = re.compile(
    r"async\s+([A-Za-z0-9_$]+)\s*\(([^)]*)\)\s*\{([^{}]*(?:\{[^{}]*\}[^{}]*)*)\}"
)
RE_BYTE_ASSIGN = re.compile(r"\b([a-zA-Z_$][a-zA-Z0-9_$]*)\[(\d+)\]\s*=\s*([^,;]+)")
RE_INLINE_PACKET = re.compile(r"(?:sendDeviceData|getDeviceData)\(\[([^\]]+)\]\)")
RE_RX_HEADER = re.compile(
    r"(\d+)\s*==\s*[a-zA-Z_$]\[\s*0\s*\]\s*&&\s*(\d+)\s*==\s*[a-zA-Z_$]\[\s*1\s*\]"
)
RE_SENSOR_BIT = re.compile(
    r"\(\s*(\d+)\s*&\s*[a-zA-Z_$]+\s*\)\s*!=\s*0.*?(?:H|changeSensorFlag)\(\s*\1\b"
)
RE_EMBEDDED_JSON = re.compile(r"JSON\.parse\('(\{\"name\":\"BK-R1X[^']+)'\)")


@dataclass(frozen=True)
class DeviceFilterMatch:
    source_file: str
    vendor_id_dec: int | None
    vendor_id_hex: str | None
    product_id_dec: int | None
    product_id_hex: str | None
    usage_page_dec: int | None
    usage_page_hex: str | None
    usage_dec: int | None
    usage_hex: str | None
    inferred_mode: str


@dataclass(frozen=True)
class CommandSignature:
    method_name: str
    source_file: str
    report_id: int
    magic_header: int
    opcode: int
    opcode_hex: str
    auth_bytes_hex: list[str]
    buffer_len: int
    raw_assignments: dict[int, str]


def try_parse_int(token: str) -> int | None:
    cleaned = token.strip()
    if not cleaned:
        return None
    base = 16 if cleaned.lower().startswith("0x") else 10
    try:
        return int(cleaned, base)
    except ValueError:
        return None


def http_get_bytes(url: str, timeout_secs: int = 15) -> bytes:
    req = urllib.request.Request(url, headers=HTTP_HEADERS)
    with urllib.request.urlopen(req, timeout=timeout_secs) as resp:
        return resp.read()


def safe_chunk_filename(raw_name: str) -> str:
    sanitized = pathlib.Path(raw_name.replace("/", "_")).name
    if not sanitized or sanitized in {".", ".."}:
        raise ValueError(f"unsafe bundle chunk filename: {raw_name!r}")
    return sanitized


def fetch_bundles(target_dir: pathlib.Path) -> list[pathlib.Path]:
    target_dir.mkdir(parents=True, exist_ok=True)
    html = http_get_bytes(f"{SITE_ORIGIN}/").decode("utf-8", errors="replace")

    chunk_paths = sorted(set(RE_SCRIPT_SRC.findall(html)))
    downloaded: list[pathlib.Path] = []

    for rel in chunk_paths:
        out_name = safe_chunk_filename(rel.split("/chunks/")[-1])
        out_path = target_dir / out_name
        data = http_get_bytes(f"{SITE_ORIGIN}{rel}")
        out_path.write_bytes(data)
        downloaded.append(out_path)

        # Webpack's runtime manifest lists lazy-loaded chunks containing the default device JSON.
        if not out_name.startswith("webpack-"):
            continue
        txt = data.decode("utf-8", errors="replace")
        for match in RE_DYNAMIC_CHUNKS.finditer(txt):
            for cid, chash in RE_DYNAMIC_PAIR.findall(match.group(1)):
                dyn_name = safe_chunk_filename(f"{cid}.{chash}.js")
                dyn_out = target_dir / dyn_name
                dyn_out.write_bytes(http_get_bytes(f"{SITE_ORIGIN}/_next/static/chunks/{dyn_name}"))
                downloaded.append(dyn_out)

    return downloaded


def infer_filter_mode(vid: int | None, usb_vid: int | None) -> str:
    if vid is None:
        return "Generic Vendor Collection Filter"
    if usb_vid is None:
        return "Hardware VID/PID Filter"
    return "USB-C Wired" if vid == usb_vid else "2.4GHz Wireless Dongle"


def extract_filters(
    filename: str,
    text: str,
    usb_vid: int | None,
) -> list[DeviceFilterMatch]:
    results: list[DeviceFilterMatch] = []
    seen: set[tuple[int | None, int | None, int | None, int | None]] = set()

    for obj_match in RE_FILTER_OBJ.finditer(text):
        kvs = {
            key: parsed
            for key, raw_val in RE_KV_INT.findall(obj_match.group(0))
            if (parsed := try_parse_int(raw_val)) is not None
        }
        if not kvs:
            continue

        vid = kvs.get("vendorId")
        pid = kvs.get("productId")
        up = kvs.get("usagePage")
        usage = kvs.get("usage")
        dedup_key = (vid, pid, up, usage)
        if dedup_key in seen:
            continue
        seen.add(dedup_key)

        results.append(
            DeviceFilterMatch(
                source_file=filename,
                vendor_id_dec=vid,
                vendor_id_hex=f"0x{vid:04X}" if vid is not None else None,
                product_id_dec=pid,
                product_id_hex=f"0x{pid:04X}" if pid is not None else None,
                usage_page_dec=up,
                usage_page_hex=f"0x{up:04X}" if up is not None else None,
                usage_dec=usage,
                usage_hex=f"0x{usage:04X}" if usage is not None else None,
                inferred_mode=infer_filter_mode(vid, usb_vid),
            )
        )
    return results


def parse_indexed_command(
    filename: str,
    method_name: str,
    buf_len: int,
    assigns: dict[int, str],
) -> CommandSignature | None:
    if 1 not in assigns or 2 not in assigns:
        return None
    rpt_id = try_parse_int(assigns.get(0, "0"))
    magic = try_parse_int(assigns[1])
    opcode = try_parse_int(assigns[2])
    if rpt_id is None or magic is None or opcode is None:
        return None

    auth = [
        f"0x{val:02X}"
        for idx in (3, 4, 5)
        if idx in assigns and (val := try_parse_int(assigns[idx])) is not None
    ]
    return CommandSignature(
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


def parse_inline_command(
    filename: str,
    method_name: str,
    arr_expr: str,
) -> CommandSignature | None:
    parts = [p.strip() for p in arr_expr.split(",")]
    if len(parts) < 3:
        return None
    first = try_parse_int(parts[0])
    second = try_parse_int(parts[1])
    if first is None or second is None:
        return None

    raw_map = dict(enumerate(parts))
    if first == 0 and second == 0x55:
        op = try_parse_int(parts[2])
        if op is None:
            return None
        auth = [
            f"0x{val:02X}"
            for token in parts[3:6]
            if (val := try_parse_int(token)) is not None
        ]
        return CommandSignature(
            method_name=f"{method_name} (inline)",
            source_file=filename,
            report_id=0,
            magic_header=0x55,
            opcode=op,
            opcode_hex=f"0x{op:02X}",
            auth_bytes_hex=auth,
            buffer_len=65,
            raw_assignments=raw_map,
        )

    if first == 0x06:
        return CommandSignature(
            method_name=f"{method_name} (reportId=0x06)",
            source_file=filename,
            report_id=0x06,
            magic_header=0x06,
            opcode=second,
            opcode_hex=f"0x{second:02X}",
            auth_bytes_hex=[],
            buffer_len=65,
            raw_assignments=raw_map,
        )

    return None


def extract_commands(filename: str, text: str) -> list[CommandSignature]:
    commands: list[CommandSignature] = []
    for match in RE_ASYNC_METHOD.finditer(text):
        method_name, _, body = match.groups()
        if "sendDeviceData" not in body and "getDeviceData" not in body:
            continue

        alloc_match = RE_REPORT_ALLOC.search(body)
        buf_len = int(alloc_match.group(1)) if alloc_match else 65

        assigns: dict[int, str] = {}
        for _, idx_str, expr in RE_BYTE_ASSIGN.findall(body):
            assigns.setdefault(int(idx_str), expr.strip())

        indexed_cmd = parse_indexed_command(filename, method_name, buf_len, assigns)
        if indexed_cmd is not None:
            commands.append(indexed_cmd)

        for arr_expr in RE_INLINE_PACKET.findall(body):
            inline_cmd = parse_inline_command(filename, method_name, arr_expr)
            if inline_cmd is not None:
                commands.append(inline_cmd)

    return commands


def detect_checksum_strategy(
    all_text: str,
    commands: list[CommandSignature],
) -> dict[str, object]:
    has_crc_table = bool(re.search(r"0x1021|0x8005|crc16|crcTable", all_text, re.IGNORECASE))
    has_sum_comp = bool(re.search(r"0x55\s*-\s*[a-zA-Z_$]|255\s*-\s*sum", all_text))
    max_written_index = max(
        (max(cmd.raw_assignments.keys(), default=0) for cmd in commands),
        default=0,
    )
    if has_crc_table:
        strategy = "Crc16"
    elif has_sum_comp:
        strategy = "SumComplement"
    else:
        strategy = "None (Magic Header 0x55 + Auth Tokens 0xA5/0xAE, Zero-Padded 64B Payload)"

    return {
        "detected_strategy": strategy,
        "tx_magic_byte": "0x55 (85)",
        "rx_magic_byte": "0xAA (170)",
        "read_auth_token": "0xA5 0x0B",
        "write_keys_auth_token": "0xA5 0x22",
        "write_config_auth_token": "0xAE 0x0A",
        "highest_tx_index_assigned": max_written_index,
        "trailing_checksum_byte_written": max_written_index >= 63,
    }


def analyze_directory(bundle_dir: pathlib.Path) -> dict[str, object]:
    js_files = sorted(bundle_dir.glob("*.js"))
    if not js_files:
        raise FileNotFoundError(f"No .js files found in {bundle_dir}")

    usb_vid: int | None = None
    texts: dict[str, str] = {}
    for path in js_files:
        txt = path.read_text(encoding="utf-8", errors="replace")
        texts[path.name] = txt
        mode_match = RE_CONNECT_MODE.search(txt)
        if mode_match and mode_match.group(2).upper() == "USB":
            usb_vid = int(mode_match.group(1))

    filters: list[DeviceFilterMatch] = []
    commands: list[CommandSignature] = []
    report_allocs: set[int] = set()
    rx_headers: set[tuple[str, str]] = set()
    sensor_bits: set[int] = set()
    default_device_json: dict[str, object] | None = None

    for fname, txt in texts.items():
        filters.extend(extract_filters(fname, txt, usb_vid))
        commands.extend(extract_commands(fname, txt))
        for alloc_str in RE_REPORT_ALLOC.findall(txt):
            val = int(alloc_str)
            if val in KNOWN_REPORT_SIZES:
                report_allocs.add(val)
        for b0, b1 in RE_RX_HEADER.findall(txt):
            rx_headers.add((f"0x{int(b0):02X}", f"0x{int(b1):02X}"))
        for bit in RE_SENSOR_BIT.findall(txt):
            sensor_bits.add(int(bit))

        if default_device_json is None and (cfg_match := RE_EMBEDDED_JSON.search(txt)):
            try:
                default_device_json = json.loads(cfg_match.group(1))
            except json.JSONDecodeError:
                default_device_json = None

    combined = "\n".join(texts.values())
    return {
        "target": "Fire Phoenix BK-R1X (bk-r1x.yjx2012.com)",
        "usb_wired_vid_detected": f"0x{usb_vid:04X}" if usb_vid is not None else None,
        "hid_filters": [asdict(f) for f in filters],
        "buffer_constants_bytes": sorted(report_allocs),
        "wire_framing": {
            "webhid_array_size": 65,
            "report_id_byte_offset": 0,
            "hid_payload_bytes": 64,
            "macro_flash_bytes": 4096,
            "macro_chunk_data_bytes": 56,
        },
        "rx_notification_headers": sorted(rx_headers),
        "paw3311_sensor_flag_bits": {
            f"0x{bit:02X}": SENSOR_BIT_LABELS.get(bit, f"Bit {bit}")
            for bit in sorted(sensor_bits)
        },
        "commands": [asdict(c) for c in commands],
        "checksum_analysis": detect_checksum_strategy(combined, commands),
        "default_hardware_descriptor": default_device_json,
    }


def print_human_report(report: dict[str, object]) -> None:
    print("=" * 78)
    print(f" BK-R1X PROTOCOL EXTRACTION REPORT — {report['target']}")
    print("=" * 78)

    print("\n[1] USB / 2.4GHz HID DEVICE FILTERS")
    for f in report["hid_filters"]:  # type: ignore[union-attr]
        print(
            f"  • {f['inferred_mode']:<28} "
            f"VID={f['vendor_id_hex'] or '*':<6} "
            f"PID={f['product_id_hex'] or '*':<6} "
            f"UsagePage={f['usage_page_hex'] or '*':<6} "
            f"Usage={f['usage_hex'] or '*':<6} "
            f"({f['source_file']})"
        )

    wf: dict[str, int] = report["wire_framing"]  # type: ignore[assignment]
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
    for cmd in report["commands"]:  # type: ignore[union-attr]
        auth_str = " ".join(cmd["auth_bytes_hex"]) if cmd["auth_bytes_hex"] else "-"
        print(
            f"  • {cmd['opcode_hex']}  rpt=0x{cmd['report_id']:02X}  "
            f"hdr=0x{cmd['magic_header']:02X}  auth=[{auth_str:<14}]  "
            f"fn={cmd['method_name']}"
        )

    print("\n[4] ASYNC / RX INPUT REPORT HEADERS")
    for h0, h1 in report["rx_notification_headers"]:  # type: ignore[union-attr]
        print(f"  • [{h0}, {h1}]")

    print("\n[5] PAW3311 SENSOR_FLAG BITMASK")
    paw_bits: dict[str, str] = report["paw3311_sensor_flag_bits"]  # type: ignore[assignment]
    for mask_hex, label in paw_bits.items():
        print(f"  • {mask_hex} -> {label}")

    print("\n[6] CHECKSUM & AUTHENTICATION ANALYSIS")
    checksum: dict[str, object] = report["checksum_analysis"]  # type: ignore[assignment]
    for key, val in checksum.items():
        print(f"  • {key:<32}: {val}")
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
        out_path = pathlib.Path(args.json_out)
        out_path.write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8")
        print(f"\n[+] Saved JSON protocol specification to {out_path}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
