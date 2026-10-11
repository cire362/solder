"""Real interactive shell checks, using only Python's standard library."""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import sys
import tempfile
import termios
import time


SOURCE = Path(sys.argv[1])
TOKEN = "pty-test-token"
PROMPT = b"SOLDER_READY> "
OSC = re.compile(rb"\x1b]0;([^\x07]*)\x07")


class Shell:
    def __init__(self, program, home, hooks, login, zdotdir=None):
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "TERM": "dumb",
               "HOME": str(home), "LC_ALL": "C.UTF-8", "SHELL": program,
               "SOLDER_SHELL_TOKEN": TOKEN, "SOLDER_SHELL_DIR": str(hooks)}
        args = [program]
        if Path(program).name == "bash":
            if login:
                env["SOLDER_SHELL_HOME"] = str(home)
                env["HOME"] = str(hooks)
                args += ["-l", "-i"]
            else:
                args += ["--rcfile", str(hooks / "bashrc"), "-i"]
        else:
            env["SOLDER_SHELL_ZDOTDIR_SET"] = "1" if zdotdir else "0"
            env["SOLDER_SHELL_ZDOTDIR"] = str(zdotdir or "")
            env["ZDOTDIR"] = str(hooks)
            args += ["-li" if login else "-i"]
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(home)
            os.execve(program, args, env)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 200, 0, 0))
        settings = termios.tcgetattr(self.fd)
        settings[3] &= ~termios.ECHO
        termios.tcsetattr(self.fd, termios.TCSANOW, settings)
        os.set_blocking(self.fd, False)
        self.pending = b""
        try:
            self.initial = self.read_prompt()
        except BaseException:
            self.close()
            raise

    def close(self):
        try:
            os.kill(self.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.waitpid(self.pid, 0)
        os.close(self.fd)

    def read_prompt(self):
        end = time.monotonic() + 8
        while time.monotonic() < end:
            if PROMPT in self.pending:
                before, self.pending = self.pending.split(PROMPT, 1)
                return before
            if select.select([self.fd], [], [], .05)[0]:
                try:
                    self.pending += os.read(self.fd, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    break
        raise AssertionError(f"no prompt; output={self.pending!r}")

    def run(self, command):
        data = memoryview(command.encode() + b"\n")
        end = time.monotonic() + 8
        # ZLE can redraw while reading a long input even when terminal echo is
        # off. Drain those redraws while writing, so neither side fills the PTY.
        while data and time.monotonic() < end:
            readable, writable, _ = select.select([self.fd], [self.fd], [], .05)
            if readable:
                self.pending += os.read(self.fd, 65536)
            if writable:
                data = data[os.write(self.fd, data[:1024]):]
        assert not data, "timed out writing shell input"
        output = self.read_prompt()
        reports = []
        for match in OSC.finditer(output):
            title = match[1].decode()
            prefix = f"solder-command;{TOKEN};"
            if title.startswith(prefix):
                payload = title[len(prefix):]
                assert len(payload.encode()) <= 16384
                assert ";" not in payload, "JSON would exceed VTE's OSC parameter limit"
                reports.append(json.loads(payload))
        return reports, output


def write(path, text):
    path.write_text(text)


def check(program, login=False, array=False, redirect=False):
    with tempfile.TemporaryDirectory(prefix="solder-shell-pty-") as scratch:
        root = Path(scratch).resolve()
        home = root / "home"
        home.mkdir()
        hooks = root / "hooks"
        hooks.mkdir()
        for name in ["json.sh", "bash.sh", "bashrc", "zsh.sh"]:
            shutil.copyfile(SOURCE / name, hooks / name)
        shutil.copyfile(SOURCE / "bashrc", hooks / ".bash_profile")
        for name in ["zshenv", "zprofile", "zshrc", "zlogin"]:
            shutil.copyfile(SOURCE / name, hooks / ("." + name))
        kind = Path(program).name
        if kind == "bash":
            name = "array-a" if array else "hook"
            first_hook = 'printf "' + name + r':%s\npipes:%s:%s\n" "$?" "${PIPESTATUS[*]}" "$?" >> "$HOME/seen"; ' + ("false" if array else "true")
            prompt_hook = "PROMPT_COMMAND='" + first_hook + "'\n"
            if array:
                second_hook = r'printf "array-b:%s\n" "$?" >> "$HOME/seen"; true'
                prompt_hook = "PROMPT_COMMAND=('" + first_hook + "' '" + second_hook + "')\n"
            write(home / ".bashrc", "PS1='SOLDER_READY> '\nPS2='CONT> '\nHISTFILE=/dev/null\nHISTCONTROL=ignorespace:ignoredups\nshopt -s lithist\nprintf 'rc\\n' >> \"$HOME/startup\"\n" + prompt_hook)
            write(home / ".bash_profile", 'printf "profile\\n" >> "$HOME/startup"\nsource "$HOME/.bashrc"\n')
            originals = {p: p.read_bytes() for p in home.iterdir()}
            zdotdir = None
        else:
            initial_dir = home
            zdotdir = None
            if redirect:
                initial_dir = root / "original-zdotdir"
                initial_dir.mkdir()
                zdotdir = initial_dir
            final_dir = home
            env_text = 'print -r -- env >> "$HOME/startup"\n'
            if redirect:
                final_dir = home / "new-zdotdir"
                final_dir.mkdir()
                env_text += 'ZDOTDIR="$HOME/new-zdotdir"\n'
            write(initial_dir / ".zshenv", env_text)
            write(final_dir / ".zprofile", 'print -r -- profile >> "$HOME/startup"\n')
            write(final_dir / ".zlogin", 'print -r -- login >> "$HOME/startup"\n')
            write(final_dir / ".zshrc", "PS1='SOLDER_READY> '\nPS2='CONT> '\nHISTFILE=/dev/null\nHISTSIZE=0\nprint -r -- rc >> \"$HOME/startup\"\npreexec(){ print -r -- \"exec:$1:$PWD\" >> \"$HOME/seen\"; }\nprecmd(){ print -r -- \"hook:$?\" >> \"$HOME/seen\"; true; }\nother_precmd(){ print -r -- array >> \"$HOME/seen\"; }\nprecmd_functions=(other_precmd)\n")
            originals = {p: p.read_bytes() for p in root.rglob(".z*") if p.is_file() and hooks not in p.parents}
        shell = Shell(program, home, hooks, login, zdotdir)
        try:
            assert not OSC.findall(shell.initial), shell.initial
            expected_startup = ["rc"] if kind == "bash" else ["env", "rc"]
            if login:
                expected_startup = ["profile", "rc"] if kind == "bash" else ["env", "profile", "rc", "login"]
            assert (home / "startup").read_text().splitlines() == expected_startup
            def expect(command, status, cwd=home):
                reports, output = shell.run(command)
                assert reports == [{"command": command, "cwd": str(cwd), "status": status}], (command, reports, output)
                return output
            expect("false", 1)
            seen = (home / "seen").read_text()
            assert ("array-a:1\npipes:1:1\narray-b:1" if array else "hook:1") in seen, seen
            if kind == "zsh":
                assert "exec:false:" in seen and "array" in seen, seen
            expect("true", 0)
            if kind == "bash":
                # The hook runs before integration bookkeeping, retaining
                # both the native last status and all pipeline components.
                expect('false | true', 0)
                assert "pipes:1 0:0" in (home / "seen").read_text()
                expect('false | false', 1)
                assert "pipes:1 1:1" in (home / "seen").read_text()
            expect("true; " * 24 + "false", 1)
            expect("printf '%s\\n' 'quote\" back\\slash'; (exit 7)", 7)
            expect("printf '%s\\n' 'line\nbreak'; false", 1)
            expect("[[ -z ${SOLDER_SHELL_TOKEN+x} ]]", 0)
            if kind == "bash" and login:
                expect("shopt -q login_shell", 0)
            if kind == "zsh":
                expect('[[ $ZDOTDIR == "$HOME/new-zdotdir" ]]' if redirect else '[[ -z ${ZDOTDIR+x} ]]', 0)
            target = home / 'space " quote\\ newline\n café'
            target.mkdir()
            expect("cd '" + str(target) + "'", 0)
            expect("false", 1, target)
            output = expect("printf '\\033]0;ordinary-title\\007'; false", 1, target)
            assert b"ordinary-title" in output
            expect("(exit 255)", 255, target)
            assert not shell.run("")[0]
            if kind == "bash":
                assert not shell.run(" false")[0], "ignored command was attributed to stale history"
                assert not shell.run("set +o history")[0]
                assert not shell.run("(exit 9)")[0]
                assert not shell.run("set -o history")[0]
                expect("(exit 23)", 23, target)
                assert not shell.run("(exit 23)")[0], "duplicate history was attributed again"
            assert not shell.run("false #" + "x" * 8200)[0], "oversized command was emitted"
            for path, content in originals.items():
                assert path.read_bytes() == content, f"modified user startup file: {path}"
        finally:
            shell.close()
    print(f"PASS {kind} login={login} array={array} redirect={redirect}", flush=True)


check("/bin/bash")
check("/bin/bash", array=True)
check("/bin/bash", login=True)
zsh = shutil.which("zsh")
if zsh:
    check(zsh)
    check(zsh, login=True)
    check(zsh, login=True, redirect=True)
else:
    print("SKIP zsh: not installed", flush=True)
