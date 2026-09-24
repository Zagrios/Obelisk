#!/usr/bin/env python3
"""Run a signed, offline Rust Cloud probe against Forge's bundled Valve files.

No game, account credential, installed Steam client or existing save is used.
All native writes are restricted to a fresh temporary directory. The profile
denies external networking and all reads beneath the real user's home.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import socket
import struct
import subprocess
import tempfile
import time
import zipfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=["inventory", "storage-inventory", "local-storage", "local-projection", "local-cache-refresh", "local-live-mutation", "sync-controls"], default="inventory")
    parser.add_argument("--keep-scratch", action="store_true", help="Keep the disposable runtime for inspection")
    parser.add_argument("--fixture-appinfo", action="store_true", help="Seed synthetic offline app quota metadata")
    parser.add_argument("--fixture-identity", action="store_true", help="Request a test-only identity with all external networking denied")
    parser.add_argument("--fixture-cache", action="store_true", help="Project a synthetic RemoteStorage index with the Forge fixture")
    args = parser.parse_args()
    if args.fixture_cache and args.mode not in ["local-projection", "local-cache-refresh"]:
        parser.error("--fixture-cache requires a projection mode")
    if args.mode == "local-cache-refresh" and not all([args.fixture_appinfo, args.fixture_identity, args.fixture_cache]):
        parser.error("local-cache-refresh requires all three --fixture-* flags")
    if args.mode in ["local-live-mutation", "sync-controls"] and not (args.fixture_appinfo and args.fixture_identity):
        parser.error(f"{args.mode} requires --fixture-appinfo and --fixture-identity")
    project = Path(__file__).resolve().parent.parent
    subprocess.run([
        "cargo", "build", "--locked", "--offline", "--manifest-path",
        str(project / "crates/steam-native/Cargo.toml"), "--example", "cloud_storage_probe",
        "--target-dir", str(project / "build/steam-native-target"),
    ], check=True)
    binary = project / "build/steam-native-target/debug/examples/cloud_storage_probe"
    package = project / "build/xcode/Build/Products/Debug/Forge.app/Contents/Resources/SteamRuntime/runtime.zip"
    manifest = json.loads((project / "crates/game-runtime/runtime-manifest.json").read_text())
    signing = (project / "apps/macos/Config/Signing.local.xcconfig").read_text()
    identity = re.search(r"^CODE_SIGN_IDENTITY\s*=\s*([0-9A-Fa-f]{40})\s*$", signing, re.M)
    if identity is None:
        raise SystemExit("Configure the existing Apple signing identity first")
    scratch = Path(tempfile.mkdtemp(prefix="forge-cloud-probe-", dir="/private/tmp"))
    scratch.chmod(0o700)
    for name in ["runtime", "home", "tmp"]:
        (scratch / name).mkdir(mode=0o700)
    runtime = scratch / "runtime"
    if args.fixture_appinfo:
        # Test-only appinfo v40, generated without installed-Steam data. These
        # quota values are fixture inputs, not claims about App 480 on Steam.
        def kv(items):
            output = bytearray()
            for key, value in items.items():
                output += (b"\x00" if isinstance(value, dict) else b"\x01") + key.encode() + b"\x00"
                output += kv(value) if isinstance(value, dict) else str(value).encode() + b"\x00"
            return bytes(output) + b"\x08"
        data = kv({"appinfo": {"appid": "480", "common": {"name": "Forge offline fixture", "type": "Game"},
                              "ufs": {"quota": "1048576", "maxnumfiles": "16"}}})
        entry = struct.pack("<IIQ", 2, 1, 0) + bytes(20) + struct.pack("<I", 1) + hashlib.sha1(data).digest() + data
        cache = struct.pack("<IIII", 0x07564428, 1, 480, len(entry)) + entry + struct.pack("<I", 0)
        path = scratch / "home/Library/Application Support/Steam/appcache/appinfo.vdf"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(cache)
    with zipfile.ZipFile(package) as archive:
        for entry in manifest["files"]:
            payload = archive.read(entry["path"])
            if len(payload) != entry["size"] or hashlib.sha256(payload).hexdigest() != entry["sha256"]:
                raise SystemExit("Bundled runtime does not match the pinned manifest")
            target = runtime / entry["path"]
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(payload)
    executable = scratch / "cloud_storage_probe"
    shutil.copy2(binary, executable)
    subprocess.run([
        "/usr/bin/codesign", "--force", "--sign", identity.group(1), "--options", "runtime",
        "--identifier", "com.zagrios.forge.cloud-probe", "--entitlements",
        str(project / "packaging/steam-host.entitlements"), str(executable),
    ], check=True, capture_output=True)
    subprocess.run(["/usr/bin/codesign", "--verify", "--strict", str(executable)], check=True)
    # These variables belong only to the disposable child, not the shell/session.
    environment = {"PATH": "/usr/bin:/bin", "HOME": str(scratch / "home"),
                   "CFFIXED_USER_HOME": str(scratch / "home"), "TMPDIR": str(scratch / "tmp"),
                   "FORGE_CLOUD_PROBE_ROOT": str(scratch)}
    if args.fixture_identity:
        environment["FORGE_CLOUD_PROBE_IDENTITY"] = "fixture"
    if args.fixture_cache:
        environment["FORGE_CLOUD_PROBE_CACHE"] = "fixture"
    # Reserve distinct private endpoints while preparing the sandbox. No default
    # Steam endpoint or local discovery is used. Release just before exec.
    reservations = []
    network_rules = []
    if args.mode != "inventory":
        for key in ["Steam3Master", "SteamClientService"]:
            listener = socket.socket()
            listener.bind(("127.0.0.1", 0))
            reservations.append(listener)
            port = listener.getsockname()[1]
            environment[key] = f"127.0.0.1:{port}"
            network_rules.append(f'(allow network-outbound (remote ip "localhost:{port}"))')
        network_rules.append('(allow network-bind network-inbound (local ip "localhost:*"))')
        environment["SteamAppId"] = "480"
        environment["SteamGameId"] = "480"
    profile = "\n".join([
        "(version 1)", "(allow default)", "(deny network*)", "(deny file-write*)",
        *network_rules,
        f"(allow file-write* (subpath {json.dumps(str(scratch))}) (literal \"/dev/null\"))",
        f"(deny file-read* (subpath {json.dumps(str(Path.home()))}))",
        "(deny mach-lookup (global-name-regex \".*[Ss]team.*\") (global-name \"com.apple.coreservices.launchservicesd\") (global-name \"com.apple.lsd.mapdb\"))",
        "(deny process-exec)", f"(allow process-exec (literal {json.dumps(str(executable))}))",
    ])
    profile_path = scratch / "probe.sb"
    profile_path.write_text(profile)
    started = time.monotonic()
    for listener in reservations:
        listener.close()
    phases = {"local-storage": ["local-write", "local-read"],
              "local-projection": ["project-write", "project-read"],
              "local-live-mutation": ["live-mutate", "live-read"],
              "sync-controls": ["sync-download", "sync-upload", "sync-cancel"],
              "local-cache-refresh": ["project-write", "project-rename", "project-read-renamed"]}.get(args.mode, [args.mode])
    result = {"mode": args.mode, "exit_code": 0, "events": [], "phases": [], "stderr_present": False, "timeout": False}
    for phase in phases:
        try:
            run = subprocess.run([
                "/usr/bin/sandbox-exec", "-f", str(profile_path), str(executable), phase,
                str(runtime / "steamclient.dylib"),
            ], cwd=runtime, env=environment, capture_output=True, timeout=30, text=True)
            # Only typed JSON emitted through the Rust-owned pipe is retained.
            result["events"].extend({**json.loads(line), "phase": phase} for line in run.stdout.splitlines())
            result["phases"].append({"phase": phase, "exit_code": run.returncode})
            if run.returncode != 0:
                result["exit_code"] = run.returncode
            result["stderr_present"] |= bool(run.stderr)
        except subprocess.TimeoutExpired as error:
            result["timeout"] = True
            result["exit_code"] = None
            # Preserve completed typed events even if a later native call hangs.
            for line in (error.stdout or b"").splitlines():
                try:
                    result["events"].append({**json.loads(line), "phase": phase})
                except json.JSONDecodeError:
                    pass
        if result["timeout"] or (result["exit_code"] != 0 and args.mode not in ["local-projection", "local-cache-refresh"]):
            break
    result.update({"elapsed_seconds": round(time.monotonic() - started, 3),
                   "synthetic_appinfo": args.fixture_appinfo,
                   "synthetic_identity_requested": args.fixture_identity,
                   "synthetic_cache": args.fixture_cache,
                   "runtime_version": manifest["version"], "authenticated": False,
                   "external_network": "denied by sandbox", "real_home_reads": "denied by sandbox",
                   "scratch": str(scratch)})
    result["source_sha256"] = {
        path: hashlib.sha256((project / path).read_bytes()).hexdigest()
        for path in ["crates/steam-native/examples/cloud_storage_probe.rs",
                     "crates/steam-native/examples/cloud_storage_probe/live_storage.rs",
                     "crates/steam-native/examples/cloud_storage_probe/sync_controls.rs",
                     "scripts/probe-steam-cloud.py", "crates/game-runtime/runtime-manifest.json"]
    }
    cloud_log = scratch / "home/Library/Application Support/Steam/logs/cloud_log.txt"
    if cloud_log.is_file():
        result["native_cache_observations"] = [
            line for line in cloud_log.read_text(errors="replace").splitlines()
            if "[AppID 480]" in line and any(message in line for message in [
                "Init cache request complete", "Sync Failed, no user set", "(ValidateCache)",
                "YldWriteCacheDirectoryToFile",
                "Starting sync", "Sync failed", "Sync Failed", "Sync complete", "sync request complete",
                "Failed sync",
            ])
        ]
    account_component = 1 if args.fixture_identity else 0
    name = "forge-offline-probe-renamed.dat" if args.mode == "local-cache-refresh" else "forge-offline-probe.dat"
    if args.mode == "local-live-mutation":
        name = "forge-offline-live.dat"
    fixture = scratch / f"home/Library/Application Support/Steam/userdata/{account_component}/480/remote/{name}"
    if fixture.is_file():
        payload = fixture.read_bytes()
        result["fixture"] = {"bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest()}
    result["scratch_retained"] = args.keep_scratch
    suffix = "-appinfo" if args.fixture_appinfo else ""
    suffix += "-identity" if args.fixture_identity else ""
    suffix += "-cache" if args.fixture_cache else ""
    output = project / "build/steam-cloud-feasibility" / f"probe-{args.mode}{suffix}.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({**result, "events": [
        {key: value for key, value in event.items() if key != "methods"}
        for event in result["events"]
    ], "report": str(output)}, indent=2))
    if not args.keep_scratch:
        shutil.rmtree(scratch)
    return 0 if result.get("exit_code") == 0 and not result["timeout"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
