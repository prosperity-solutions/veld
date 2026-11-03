# /// script
# requires-python = ">=3.11"
# dependencies = ["websockets==15.0.1", "cryptography>=46,<47"]
# ///
"""Exercise an explicitly installed trial runner; never use the installed Veld DB.

Run with uv run tests/remote/runner_trial.py --binary target/debug/veld.
--loopback-transport tests the ingress locally when Tailscale publication is unavailable.
This creates and removes its own repository, pairing registrations, and terminals.
"""

import argparse
import asyncio
import base64
import json
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
from websockets.asyncio.client import connect


def decoded(value: str) -> bytes:
    return base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))


class Trial:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.binary = str(Path(args.binary).resolve())
        self.descriptor = json.loads(self.admin("status"))["descriptor"]
        self.origin = self.descriptor["endpoint"]
        self.authority = urllib.parse.urlsplit(self.origin).netloc
        self.base = f"http://127.0.0.1:{args.port}" if args.loopback_transport else self.origin
        self.clients: list[dict] = []
        self.sessions: list[str] = []
        self.results: list[str] = []

    def admin(self, *arguments: str) -> str:
        return subprocess.run([self.binary, "runner", "--name", self.args.name, *arguments],
                              check=True, capture_output=True, text=True).stdout.strip()

    def request(self, method: str, path: str, body=None, client=None, key=None, expected=200):
        headers = {"Host": self.authority, "X-Veld-Request": "1"}
        if client:
            headers["Authorization"] = "Bearer " + client["credential"]
        if key:
            headers["Idempotency-Key"] = key
        data = None if body is None else json.dumps(body).encode()
        if data is not None:
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(self.base + path, data=data, method=method, headers=headers)
        try:
            response = urllib.request.urlopen(request, timeout=40)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            payload = response.read(2 * 1024 * 1024)
            assert response.status == expected, f"{method} {path}: expected {expected}, got {response.status}"
            if not payload:
                return None
            return json.loads(payload)

    def pair(self) -> dict:
        invitation = json.loads(decoded(self.admin("pair").removeprefix("veld-pair-v1:")))
        nonce = base64.urlsafe_b64encode(uuid.uuid4().bytes + uuid.uuid4().bytes).rstrip(b"=").decode()
        proof = self.request("GET", "/remote/descriptor?nonce=" + nonce)
        assert proof["descriptor"] == invitation["descriptor"] == self.descriptor
        assert proof["nonce"] == nonce
        message = f'veld-remote-v1\n{self.descriptor["instance_id"]}\n{self.origin}\n{nonce}'.encode()
        Ed25519PublicKey.from_public_bytes(decoded(self.descriptor["public_key"])).verify(decoded(proof["signature"]), message)
        body = {"secret": invitation["secret"], "label": "automated trial"}
        client = self.request("POST", "/remote/pair", body)
        self.clients.append(client)
        self.request("POST", "/remote/pair", body, expected=401)
        return client

    async def socket(self, client: dict, worktree: int, session: str):
        ticket = await asyncio.to_thread(self.request, "POST", "/api/pty/tickets",
                                        {"worktree_id": worktree, "session_id": session}, client)
        scheme = "ws" if self.args.loopback_transport else "wss"
        options = {"host": "127.0.0.1", "port": self.args.port} if self.args.loopback_transport else {}
        socket = await connect(f"{scheme}://{self.authority}/api/pty/attach?ticket={ticket['ticket']}",
                               origin=self.origin, additional_headers={"Authorization": "Bearer " + client["credential"]},
                               open_timeout=10, **options)
        async with asyncio.timeout(10):
            while True:
                message = await socket.recv()
                if isinstance(message, str) and json.loads(message).get("type") == "ready":
                    return socket

    def passed(self, name: str):
        self.results.append(name)
        print("PASS", name, flush=True)

    async def wait_for_file(self, path: Path, minimum: int = 1):
        async with asyncio.timeout(15):
            while not path.exists() or path.stat().st_size < minimum:
                await asyncio.sleep(0.1)

    async def run(self):
        client = await asyncio.to_thread(self.pair)
        other = await asyncio.to_thread(self.pair)
        self.passed("fresh identity proof, one-use invitations, and two paired clients")
        self.request("GET", "/api/health", expected=401)
        self.request("GET", "/api/health", client=client)
        self.passed("authenticated ingress")
        with tempfile.TemporaryDirectory(prefix="veld-remote-trial-") as directory:
            root = Path(directory)
            for arguments in (["init", "-b", "main"], ["-c", "commit.gpgsign=false", "-c", "user.name=Veld trial",
                              "-c", "user.email=trial@example.invalid", "commit", "--allow-empty", "-m", "trial fixture"]):
                subprocess.run(["git", "-C", directory, *arguments], check=True, capture_output=True)
            config = {"schemaVersion": "3", "name": "runner-trial", "nodes": {"probe": {"variants": {
                "local": {"type": "command", "argv": ["python3", "-c", "from pathlib import Path; import time; Path('dispatches').open('a').write('once\\n'); time.sleep(2)"]}}}}}
            (root / "veld.json").write_text(json.dumps(config))
            repo = self.request("POST", "/api/repos/import", {"path": directory}, client)
            worktree = next(row["id"] for row in repo["worktrees"] if row["path"] == directory)
            try:
                body = {"request": {"project_root": directory, "kind": {"kind": "start", "run": "trial",
                        "selections": ["probe:local"], "oneshot": "probe", "all_logs": False}}}
                key = str(uuid.uuid4())
                first = self.request("POST", "/api/engine/actions", body, client, key, expected=202)
                second = self.request("POST", "/api/engine/actions", body, client, key, expected=202)
                assert first == second
                action_path = "/api/engine/actions/" + first["action_id"]
                self.request("GET", action_path, client=other, expected=404)
                async with asyncio.timeout(30):
                    while True:
                        receipt = self.request("GET", action_path, client=client)
                        if receipt["status"] not in {"running", "pending"}:
                            assert receipt["status"] == "succeeded", "fixture engine action did not succeed"
                            break
                        await asyncio.sleep(0.1)
                assert (root / "dispatches").read_text() == "once\n"
                self.passed("duplicate engine submission executes once and scopes receipts")
                session = str(uuid.uuid4())
                self.sessions.append(session)
                socket = await self.socket(client, worktree, session)
                command = "python3 -u -c 'from pathlib import Path; import time; Path(\"terminal-launches\").open(\"a\").write(\"once\\n\"); exec(\"while True:\\n Path(\\\"ticks\\\").open(\\\"a\\\").write(\\\".\\\"); time.sleep(0.1)\")'\n"
                await socket.send(command.encode())
                await self.wait_for_file(root / "ticks")
                await socket.close()
                before = (root / "ticks").stat().st_size
                await self.wait_for_file(root / "ticks", before + 3)
                socket = await self.socket(client, worktree, session)
                self.passed("terminal job survives disconnect and reattaches without replacement")
                if self.args.restart_service:
                    await socket.close()
                    subprocess.run(["systemctl", "--user", "restart", f"veld-runner-{self.args.name}.service"], check=True, capture_output=True)
                    async with asyncio.timeout(15):
                        while True:
                            try:
                                self.request("GET", "/api/health", client=client)
                                break
                            except (OSError, AssertionError):
                                await asyncio.sleep(0.2)
                    socket = await self.socket(client, worktree, session)
                    assert self.request("POST", "/api/engine/actions", body, client, key, expected=202) == first
                    self.passed("daemon restart preserves identity, engine receipt, and live terminal")
                assert (root / "terminal-launches").read_text() == "once\n"
                self.admin("revoke", client["client_id"])
                self.request("GET", "/api/health", client=client, expected=401)
                async with asyncio.timeout(5):
                    await socket.wait_closed()
                before = (root / "ticks").stat().st_size
                await self.wait_for_file(root / "ticks", before + 3)
                self.passed("revocation closes the socket and rejects requests while host work continues")
            finally:
                for session in self.sessions:
                    request = urllib.request.Request(f"http://127.0.0.1:{self.args.daemon_port}/api/pty/sessions/{session}",
                                                     method="DELETE", headers={"X-Veld-Request": "1"})
                    with urllib.request.urlopen(request, timeout=5):
                        pass
                self.request("DELETE", "/api/repos", {"root": directory}, other, expected=204)
        for client in self.clients:
            self.admin("revoke", client["client_id"])
        print(json.dumps({"transport": "loopback" if self.args.loopback_transport else "tailscale-https", "passed": self.results}, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/debug/veld")
    parser.add_argument("--name", default="remote-trial")
    parser.add_argument("--port", type=int, default=19896)
    parser.add_argument("--daemon-port", type=int, default=19895)
    parser.add_argument("--loopback-transport", action="store_true")
    parser.add_argument("--restart-service", action="store_true")
    args = parser.parse_args()
    if not args.name.endswith("trial"):
        parser.error("use an isolated runner name ending in trial")
    trial = Trial(args)
    try:
        asyncio.run(trial.run())
    finally:
        for client in trial.clients:
            trial.admin("revoke", client["client_id"])


if __name__ == "__main__":
    main()
