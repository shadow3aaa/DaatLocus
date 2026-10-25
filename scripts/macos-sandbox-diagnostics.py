"""Print bounded OS crash reasons for native sandbox failures on CI runners."""

import json
from pathlib import Path


def main():
    processes = {"sh", "bash", "cat", "wc", "mv", "rm", "mkdir", "sleep", "sandbox-exec"}
    decoder = json.JSONDecoder()
    reports = []
    for directory in [Path.home() / "Library/Logs/DiagnosticReports", Path("/Library/Logs/DiagnosticReports")]:
        if directory.is_dir():
            reports.extend(path for path in directory.glob("*.ips") if path.name.split("-")[0] in processes)
    for path in sorted(reports, key=lambda item: item.stat().st_mtime, reverse=True)[:16]:
        with path.open(encoding="utf-8", errors="replace") as handle:
            data = handle.read(4 * 1024 * 1024 + 1)
        if len(data) > 4 * 1024 * 1024:
            continue
        offset = 0
        while offset < len(data):
            offset += len(data[offset:]) - len(data[offset:].lstrip("\ufeff \r\n\t"))
            if offset == len(data):
                break
            try:
                report, offset = decoder.raw_decode(data, offset)
            except json.JSONDecodeError:
                break
            if isinstance(report, dict) and report.get("procName") in processes:
                # Exclude environment, process arguments and unrelated report data.
                print(json.dumps({key: report[key] for key in ("procName", "exception", "termination", "asi") if key in report}))


if __name__ == "__main__":
    main()
