#!/usr/bin/env python3
"""Real localhost SSH smoke test of Solder's first-party project worker.

Run with --worker pointing at src/remote/worker.py in the parent checkout.
Only stdlib and installed system ssh/ssh-keygen/sshd/python3 are used. All
keys, configs, project bytes and logs live in one private temporary folder.
Exit 0 means passed, 77 means unavailable/restricted, and 1 means a test failed.
This sidecar does not exercise Rust's integrated channel or pull Docker images.
"""

import argparse
import errno
import hashlib
import json
import os
from pathlib import Path
import pwd
import selectors
import shutil
import signal
import socket
import subprocess
import tempfile
import time


IO_TIMEOUT = 5
TOTAL_TIMEOUT = 45
FRAME_LIMIT = 1024 * 1024


class Blocked(Exception):
    pass


def quoted(text):
    """The same POSIX quoting as remote::quoted; argv remains data."""
    return "'" + str(text).replace("'", "'\\''") + "'"


def target_command(cwd, program, args, env=()):
    command = "cd -- " + quoted(cwd) + " && exec "
    if env:
        command += "env " + " ".join(quoted(f"{key}={value}") for key, value in env) + " "
    return command + " ".join(quoted(value) for value in [program, *args])


def tail(path):
    if not path.exists():
        return ""
    with path.open("rb") as stream:
        stream.seek(max(0, path.stat().st_size - 16384))
        return stream.read().decode("utf-8", "replace").strip()


def stop(child):
    if child.poll() is None:
        # Each launch owns its session, so termination never reaches a user's
        # existing SSH session or system daemon. Closing SSH tears down its worker.
        try:
            os.killpg(child.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            child.wait(timeout=2)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait(timeout=2)
    for stream in (child.stdin, child.stdout, child.stderr):
        if stream is not None:
            stream.close()


class Channel:
    def __init__(self, child, deadline):
        self.child = child
        self.deadline = deadline
        self.buffer = bytearray()
        self.next_id = 1
        self.selector = selectors.DefaultSelector()
        self.selector.register(child.stdout, selectors.EVENT_READ)
        os.set_blocking(child.stdout.fileno(), False)

    def fill(self, until):
        remaining = min(until, self.deadline) - time.monotonic()
        if remaining <= 0 or not self.selector.select(remaining):
            raise TimeoutError("SSH worker did not answer within the bounded timeout")
        chunk = os.read(self.child.stdout.fileno(), 65536)
        if not chunk:
            raise EOFError("SSH closed before the worker reply")
        self.buffer.extend(chunk)
        if len(self.buffer) > FRAME_LIMIT:
            raise AssertionError("Worker frame exceeded the smoke-test limit")

    def request(self, method, params=None, payload=b""):
        request_id = self.next_id
        self.next_id += 1
        frame = json.dumps({"id": request_id, "method": method,
                            "params": params or {}, "bytes": len(payload)}).encode() + b"\n"
        self.child.stdin.write(frame + payload)
        self.child.stdin.flush()
        until = time.monotonic() + IO_TIMEOUT
        while b"\n" not in self.buffer:
            self.fill(until)
        header, _, rest = self.buffer.partition(b"\n")
        reply = json.loads(header)
        self.buffer = bytearray(rest)
        assert reply["id"] == request_id, reply
        length = reply["bytes"]
        assert type(length) is int and 0 <= length <= FRAME_LIMIT, reply
        while len(self.buffer) < length:
            self.fill(until)
        data = bytes(self.buffer[:length])
        del self.buffer[:length]
        return reply, data

    def success(self, method, params=None, payload=b""):
        reply, data = self.request(method, params, payload)
        assert "error" not in reply, reply
        return reply["result"], data

    def close(self):
        self.selector.close()
        if self.child.stdin and not self.child.stdin.closed:
            self.child.stdin.close()
        try:
            self.child.wait(timeout=2)
        except subprocess.TimeoutExpired:
            pass


def exercise(channel, root, scratch, ssh_argv, deadline, passed):
    hello, _ = channel.success("hello")
    assert hello == {"version": 1, "root": str(root)}, hello
    passed.append("handshake")

    path = root / "bytes.bin"
    original = bytes(range(256)) + b"\x00\xff\xfe\r\n"
    path.write_bytes(original)
    path.chmod(0o640)
    stamp, data = channel.success("read", {"path": "bytes.bin"})
    assert data == original
    assert stamp["length"] == len(original)
    assert stamp["hash"] == hashlib.sha256(original).hexdigest()
    passed.append("binary read")

    changed = bytes(reversed(range(256))) + b"\x00\x80new\n"
    with path.open("rb") as old:
        old_inode = os.fstat(old.fileno()).st_ino
        saved, _ = channel.success("write", {"path": "bytes.bin", "expected": stamp}, changed)
        assert old.read() == original
        assert path.stat().st_ino != old_inode
    assert path.read_bytes() == changed
    assert path.stat().st_mode & 0o777 == 0o640
    assert saved["hash"] == hashlib.sha256(changed).hexdigest()
    assert not list(root.glob(".solder-save-*"))
    passed.append("atomic CAS replacement")

    # Keep length and mtime identical: conflict detection must consult the hash.
    intervening = b"x" * len(changed)
    path.write_bytes(intervening)
    os.utime(path, ns=(path.stat().st_atime_ns, saved["mtime"]))
    reply, _ = channel.request("write", {"path": "bytes.bin", "expected": saved}, b"overwrite")
    assert reply.get("conflict") is True and "error" in reply, reply
    assert path.read_bytes() == intervening
    assert not list(root.glob(".solder-save-*"))
    passed.append("same-stamp hash conflict")

    channel.success("write", {"path": "new.bin", "expected": {"missing": True}}, original)
    reply, _ = channel.request("write", {"path": "new.bin", "expected": {"missing": True}}, b"overwrite")
    assert reply.get("conflict") is True, reply
    assert (root / "new.bin").read_bytes() == original
    assert not list(root.glob(".solder-save-*"))
    passed.append("exclusive create CAS")

    outside = scratch / "outside.bin"
    outside.write_bytes(b"first-party outside marker")
    (root / "escape").symlink_to(scratch, target_is_directory=True)
    (root / "outside-link").symlink_to(outside)
    for path_name in ["../outside.bin", str(outside), "escape/outside.bin", "outside-link"]:
        for method, payload in [("read", b""), ("write", b"not allowed")]:
            reply, data = channel.request(method, {"path": path_name}, payload)
            assert "error" in reply and data == b"", (method, path_name, reply)
    assert outside.read_bytes() == b"first-party outside marker"
    assert not list(root.glob(".solder-save-*"))
    assert channel.success("hello")[0] == hello
    passed.append("path and symlink escape refusal")

    cwd = root / "cwd with ' quote"
    cwd.mkdir()
    caller = scratch / "local caller"
    caller.mkdir()
    expression = "import json,os; print(json.dumps([os.getcwd(),os.environ['SOLDER_SMOKE']]))"
    remote = target_command(cwd, "python3", ["-c", expression], [("SOLDER_SMOKE", "a' b 日本語")])
    child = subprocess.Popen([*ssh_argv, remote], cwd=caller, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, start_new_session=True)
    try:
        output, errors = child.communicate(timeout=max(0.01, min(10, deadline - time.monotonic())))
        assert child.returncode == 0, errors.decode("utf-8", "replace")
        assert json.loads(output) == [str(cwd), "a' b 日本語"], output
    finally:
        stop(child)
    passed.append("target command cwd, argv quoting and environment")
    return passed


def smoke(worker, result):
    if os.geteuid() == 0:
        raise Blocked("Refusing to launch a root daemon; run as an ordinary user")
    tools = {name: shutil.which(name) for name in ("ssh", "ssh-keygen", "python3")}
    tools["sshd"] = "/usr/sbin/sshd" if Path("/usr/sbin/sshd").is_file() else shutil.which("sshd")
    missing = [name for name, path in tools.items() if not path]
    if missing:
        raise Blocked("Missing installed system tools: " + ", ".join(missing))
    source = worker.read_text(encoding="utf-8")
    result["worker"] = str(worker)
    result["worker_sha256"] = hashlib.sha256(source.encode()).hexdigest()
    deadline = time.monotonic() + TOTAL_TIMEOUT
    with tempfile.TemporaryDirectory(prefix="solder-ssh-smoke-") as temporary:
        scratch = Path(temporary).resolve()
        scratch.chmod(0o700)
        result["temporary_directory"] = str(scratch)
        root = scratch / "project space ' 日本語"
        root.mkdir(mode=0o700)
        shell_dir = scratch / "shell"
        shell_dir.mkdir(mode=0o700)
        host_key, client_key = scratch / "host_key", scratch / "client_key"
        for key in (host_key, client_key):
            subprocess.run([tools["ssh-keygen"], "-q", "-t", "ed25519", "-N", "",
                            "-C", "solder-local-smoke", "-f", str(key)],
                           check=True, capture_output=True, timeout=10)
        authorized = scratch / "authorized_keys"
        authorized.write_text("restrict " + client_key.with_suffix(".pub").read_text())
        authorized.chmod(0o600)
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as reservation:
                reservation.bind(("127.0.0.1", 0))
                port = reservation.getsockname()[1]
        except OSError as error:
            if error.errno in (errno.EPERM, errno.EACCES):
                raise Blocked(f"Loopback socket bind denied: {error}") from error
            raise
        result["endpoint"] = f"127.0.0.1:{port}"
        username = pwd.getpwuid(os.geteuid()).pw_name
        known_hosts = scratch / "known_hosts"
        host_public = " ".join(host_key.with_suffix(".pub").read_text().split()[:2])
        known_hosts.write_text(f"[127.0.0.1]:{port} {host_public}\n")
        known_hosts.chmod(0o600)
        client_config = scratch / "ssh_config"
        client_config.write_text(f"Host 127.0.0.1\n  HostName 127.0.0.1\n  User {username}\n"
                                 "  ForwardAgent no\n  ClearAllForwardings yes\n"
                                 "  ControlMaster no\n  ControlPath none\n  RequestTTY no\n")
        python_path = str(Path(tools["python3"]).parent)
        daemon_config = scratch / "sshd_config"
        daemon_config.write_text(
            f"Port {port}\nListenAddress 127.0.0.1\nAddressFamily inet\n"
            f'HostKey "{host_key}"\nPidFile "{scratch / "sshd.pid"}"\n'
            f'AuthorizedKeysFile "{authorized}"\nAllowUsers {username}\n'
            # /tmp ancestors can be world-writable; the test keys themselves
            # are private, generated here, and never shared with another user.
            "StrictModes no\nPubkeyAuthentication yes\nAuthenticationMethods publickey\n"
            "PasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\n"
            "PermitRootLogin no\nPermitEmptyPasswords no\nAllowAgentForwarding no\n"
            "AllowTcpForwarding no\nX11Forwarding no\nPermitTunnel no\nPermitTTY no\n"
            "LoginGraceTime 10\nMaxAuthTries 1\nLogLevel VERBOSE\n"
            f'SetEnv "PATH={python_path}:/usr/bin:/bin:/usr/sbin:/sbin" '
            f'"ZDOTDIR={shell_dir}" "BASH_ENV=/dev/null" "ENV=/dev/null"\n'
        )
        checked = subprocess.run([tools["sshd"], "-t", "-f", str(daemon_config)],
                                 capture_output=True, timeout=5)
        if checked.returncode:
            raise Blocked("Unprivileged sshd config check rejected: " + checked.stderr.decode("utf-8", "replace").strip())
        ssh_argv = [tools["ssh"], "-F", str(client_config), "-i", str(client_key),
                    "-o", f"UserKnownHostsFile={known_hosts}", "-o", "GlobalKnownHostsFile=none",
                    "-o", "IdentitiesOnly=yes", "-o", "IdentityAgent=none",
                    "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
                    "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=15",
                    "-o", "ServerAliveCountMax=2", "-T", "-p", str(port), "--", "127.0.0.1"]
        daemon_log, client_log = scratch / "sshd.log", scratch / "ssh.log"
        daemon = client = channel = None
        try:
            with daemon_log.open("wb") as errors:
                daemon = subprocess.Popen([tools["sshd"], "-D", "-e", "-f", str(daemon_config)],
                                          stdout=subprocess.DEVNULL, stderr=errors, start_new_session=True)
            ready_until = min(deadline, time.monotonic() + 5)
            while "Server listening on 127.0.0.1" not in tail(daemon_log):
                if daemon.poll() is not None:
                    raise Blocked("Unprivileged sshd exited before listening: " + tail(daemon_log))
                if time.monotonic() >= ready_until:
                    raise Blocked("Unprivileged sshd did not listen within 5 seconds: " + tail(daemon_log))
                time.sleep(0.02)
            result["daemon_listened"] = True
            command = target_command(root, "python3", ["-I", "-u", "-c", source, str(root)])
            with client_log.open("wb") as errors:
                client = subprocess.Popen([*ssh_argv, command], cwd=scratch, stdin=subprocess.PIPE,
                                          stdout=subprocess.PIPE, stderr=errors, bufsize=0, start_new_session=True)
            channel = Channel(client, deadline)
            try:
                exercise(channel, root, scratch, ssh_argv, deadline, result["passed"])
            except (EOFError, BrokenPipeError, TimeoutError) as error:
                # A failed handshake here can be a platform privilege/PAM
                # restriction. Keep exact server/client diagnostics in the result.
                if not result["passed"]:
                    raise Blocked(f"SSH/helper channel unavailable before handshake: {error}") from error
                raise AssertionError(f"Channel failed after handshake: {error}") from error
            result["status"] = "passed"
        finally:
            if channel:
                channel.close()
            if client:
                stop(client)
            if daemon:
                stop(daemon)
            result["sshd_log"] = tail(daemon_log)
            result["ssh_stderr"] = tail(client_log)
    result["cleanup"] = "launched processes reaped; temporary directory removed"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker", required=True, type=Path,
                        help="first-party parent editor/crates/solder/src/remote/worker.py")
    args = parser.parse_args()
    result = {"status": "failed", "passed": []}
    code = 1
    try:
        smoke(args.worker.resolve(strict=True), result)
        code = 0
    except Blocked as error:
        result.update(status="blocked", reason=str(error))
        code = 77
    except (OSError, subprocess.SubprocessError, AssertionError, ValueError) as error:
        result["reason"] = f"{type(error).__name__}: {error}"
    finally:
        temporary = result.get("temporary_directory")
        if temporary and not Path(temporary).exists():
            result.setdefault("cleanup", "temporary directory removed; launched processes stopped in finally")
        print(json.dumps(result, ensure_ascii=False, indent=2))
    return code


if __name__ == "__main__":
    raise SystemExit(main())
