"""Opt-in tests on a provisioned, disposable Android 15+ emulator.

No account reset or permission grant is performed by this runner. Install/start
Shizuku or Magisk and authorize the test installation before running it.
"""
import argparse
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--serial", required=True)
parser.add_argument("--method", choices=("shizuku", "root"), required=True)
parser.add_argument("--account", required=True)
parser.add_argument("--app-apk", type=Path, required=True)
parser.add_argument("--test-apk", type=Path, required=True)
parser.add_argument("--fixture-apk", type=Path, required=True)
args = parser.parse_args()
password = os.environ.get("DAAT_LOCUS_PHONE_TEST_PASSWORD", "")
if not args.account.startswith("phone-test-") or len(password) < 12:
    parser.error("Use a phone-test-* account and DAAT_LOCUS_PHONE_TEST_PASSWORD (12+ characters)")
adb = [os.environ.get("ADB", "adb"), "-s", args.serial]
for apk in (args.app_apk, args.test_apk, args.fixture_apk):
    if not apk.is_file():
        parser.error(f"Missing APK: {apk}")
    result = subprocess.run(adb + ["install", "-r", str(apk)], capture_output=True, text=True)
    if result.returncode:
        raise SystemExit(f"APK installation failed: {apk.name}\n{result.stdout}{result.stderr}")
for persistence in (False, True):
    subprocess.run(adb + ["shell", "am", "force-stop", "io.daat_locus.app"], check=True)
    extra = ["-e", "verify_display", "true"] if persistence else []
    try:
        result = subprocess.run(adb + ["shell", "am", "instrument", "-w", "-e", "mode", "phone",
            "-e", "method", args.method, "-e", "account", args.account, "-e", "password", password] + extra + [
            "io.daat_locus.app.test/io.daat_locus.app.ReleaseStartupTest"],
            capture_output=True, text=True, timeout=240)
    except subprocess.TimeoutExpired:
        # Do not print subprocess arguments containing the disposable credential.
        raise SystemExit("PhoneUse instrumentation timed out") from None
    print(result.stdout)
    expected = "PhoneUse display persistence" if persistence else "PhoneUse native contract"
    if result.returncode or f"OK ({expected})" not in result.stdout:
        raise SystemExit(f"{expected} failed or prerequisites were unavailable")
