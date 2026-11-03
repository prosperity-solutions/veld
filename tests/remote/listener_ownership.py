"""A runner must not expose its identity when another process owns its API port."""

import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time


def check(binary: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="veld-listener-trial-") as directory:
        root = Path(directory)
        with socket.socket() as occupied, socket.socket() as remote_reservation:
            occupied.bind(("127.0.0.1", 0))
            occupied.listen()
            remote_reservation.bind(("127.0.0.1", 0))
            api_port = occupied.getsockname()[1]
            remote_port = remote_reservation.getsockname()[1]
            database = root / "veld.db"
            state = database.with_suffix(".remote")
            state.mkdir(mode=0o700)
            configuration = state / "config.json"
            configuration.write_text(json.dumps({
                "endpoint": f"https://listener-trial.example.ts.net:{remote_port}",
                "daemon_port": api_port, "remote_port": remote_port,
            }))
            configuration.chmod(0o600)
            environment = os.environ.copy()
            for name in ("VELD_INSTANCE", "VELD_MANAGEMENT_HOST"):
                environment.pop(name, None)
            environment.update(VELD_DB_PATH=str(database), VELD_DAEMON_PORT=str(api_port),
                               VELD_DAEMON_SOCK=str(root / "daemon.sock"))
            remote_reservation.close()
            with (root / "output.log").open("w+") as output:
                process = subprocess.Popen([str(binary), "--socket-path", str(root / "daemon.sock")],
                                           env=environment, stdout=output, stderr=subprocess.STDOUT)
                try:
                    deadline = time.monotonic() + 15
                    while process.poll() is None:
                        with socket.socket() as probe:
                            probe.settimeout(0.1)
                            assert probe.connect_ex(("127.0.0.1", remote_port)) != 0, "remote ingress exposed another daemon"
                        assert time.monotonic() < deadline, "daemon failed to exit after losing API ownership"
                        time.sleep(0.02)
                    assert process.returncode != 0, "occupied API port reported success"
                    output.seek(0)
                    assert "cannot own daemon API listener" in output.read()
                finally:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=5)
    print("PASS occupied API listener prevents remote ingress and exits nonzero")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/veld-daemon"))
    check(parser.parse_args().binary.resolve())
