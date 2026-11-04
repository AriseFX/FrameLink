"""Estimate receiver-minus-sender UTC offset over an authenticated SSH connection."""
import argparse
import getpass
import json
from pathlib import Path
import time

from receive import now_ticks

# The remote side needs only Python 3. No files, services or clock settings change.
SERVER = "python3 -u -c 'import sys,time,json\nfor line in sys.stdin:\n t2=time.time_ns(); t3=time.time_ns(); print(json.dumps([t2,t3]),flush=True)'"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", required=True)
    parser.add_argument("--user", required=True)
    parser.add_argument("--port", type=int, default=22)
    parser.add_argument("--password", action="store_true", help="Prompt without echo; otherwise use SSH keys/agent")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.output.suffix.lower() != ".json":
        parser.error("--output must end in .json")
    if not 1 <= args.port <= 65535:
        parser.error("Invalid SSH port")
    if args.output.exists():
        parser.error("Output already exists; choose a new calibration filename")
    try:
        import paramiko
    except ImportError:
        parser.error("SSH calibration requires paramiko: python -m pip install paramiko")
    client = paramiko.SSHClient()
    client.load_system_host_keys(str(Path.home() / ".ssh/known_hosts"))
    client.set_missing_host_key_policy(paramiko.RejectPolicy())
    password = getpass.getpass("SSH password: ") if args.password else None
    try:
        client.connect(
            args.host, port=args.port, username=args.user, password=password,
            allow_agent=not args.password, look_for_keys=not args.password,
            timeout=10, banner_timeout=10, auth_timeout=10,
        )
        password = None
        stdin, stdout, stderr = client.exec_command(SERVER, timeout=15)
        samples = []
        for _ in range(100):
            t1 = now_ticks() * 100
            stdin.write("ping\n")
            stdin.flush()
            t2, t3 = json.loads(stdout.readline())
            t4 = now_ticks() * 100
            samples.append({
                "t1": t1, "t2": t2, "t3": t3, "t4": t4,
                "rtt_ms": ((t4-t1)-(t3-t2)) / 1e6,
                "offset_ms": ((t2-t1)+(t3-t4)) / 2e6,
            })
            time.sleep(.005)
        stdin.channel.shutdown_write()
        if stdout.channel.recv_exit_status() != 0:
            raise RuntimeError(stderr.read().decode("utf-8", "replace"))
        valid = [sample for sample in samples[5:] if sample["rtt_ms"] >= 0]
        if not valid:
            raise RuntimeError("No valid clock samples")
        best = min(valid, key=lambda sample: sample["rtt_ms"])
        report = {
            "scope": "receiver UTC minus sender UTC; run this client on the sending machine",
            "best": best,
            "offset_uncertainty_bound_ms": best["rtt_ms"] / 2,
            "samples": samples,
        }
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(json.dumps({"offset_ms": best["offset_ms"], "uncertainty_ms": best["rtt_ms"] / 2}))
    finally:
        password = None
        client.close()


if __name__ == "__main__":
    main()
