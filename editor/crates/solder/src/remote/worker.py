"""Solder's project filesystem over an authenticated stdio channel.

Only Python's standard library is used. Bytes have their own frame: file
encoding and arbitrary output never pass through JSON's text conversion.
"""
import errno
import json
import hashlib
import os
import stat
import subprocess
import shutil
import time
import tempfile
import sys
import secrets

VERSION = 1
HEADER_LIMIT = 16 * 1024 * 1024
FILE_LIMIT = 512 * 1024 * 1024
ENTRY_LIMIT = 50000
EXCLUDED = {'.git', '.solder', 'node_modules', 'target', '.next', 'dist', '.turbo', '__pycache__', '.venv', 'venv'}
ROOT = os.path.realpath(os.path.expanduser(sys.argv[1]))
ROOT_FD = os.open(ROOT, os.O_RDONLY | os.O_DIRECTORY)


def parts(relative):
    if not isinstance(relative, str) or relative.startswith('/') or '\0' in relative:
        raise ValueError('Expected a relative project path')
    components = relative.split('/') if relative else []
    if any(p in ('', '.', '..') for p in components):
        raise ValueError('The path leaves the project')
    relative.encode('utf-8')
    return components


def directory(components, create=False):
    fd = os.dup(ROOT_FD)
    try:
        for component in components:
            if create:
                try:
                    os.mkdir(component, dir_fd=fd)
                except FileExistsError:
                    pass
            next_fd = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = next_fd
        return fd
    except BaseException:
        os.close(fd)
        raise


def metadata(relative):
    components = parts(relative)
    if not components:
        s = os.fstat(ROOT_FD)
    else:
        fd = directory(components[:-1])
        try:
            s = os.stat(components[-1], dir_fd=fd, follow_symlinks=False)
        finally:
            os.close(fd)
    return {'path': relative, 'length': s.st_size, 'mtime': s.st_mtime_ns,
            'kind': 'link' if stat.S_ISLNK(s.st_mode) else 'dir' if stat.S_ISDIR(s.st_mode) else 'file' if stat.S_ISREG(s.st_mode) else 'other'}


def read(relative):
    components = parts(relative)
    if not components:
        raise ValueError('Expected a file')
    parent = directory(components[:-1])
    try:
        fd = os.open(components[-1], os.O_RDONLY | os.O_NOFOLLOW, dir_fd=parent)
    finally:
        os.close(parent)
    with os.fdopen(fd, 'rb') as f:
        s = os.fstat(f.fileno())
        if not stat.S_ISREG(s.st_mode):
            raise ValueError('Not a regular file')
        if s.st_size > FILE_LIMIT:
            raise ValueError('The file is too large')
        data = f.read(FILE_LIMIT + 1)
        if len(data) > FILE_LIMIT:
            raise ValueError('The file is too large')
        after = os.fstat(f.fileno())
        if (s.st_mtime_ns, s.st_size) != (after.st_mtime_ns, after.st_size):
            raise ValueError('The file changed while it was read')
        return {'length': s.st_size, 'mtime': s.st_mtime_ns, 'hash': hashlib.sha256(data).hexdigest()}, data


class ConflictError(ValueError):
    pass


def write(relative, data, expected):
    components = parts(relative)
    if not components:
        raise ValueError('Expected a file')
    parent = directory(components[:-1])
    temporary = None
    try:
        try:
            old = os.stat(components[-1], dir_fd=parent, follow_symlinks=False)
            if not stat.S_ISREG(old.st_mode):
                raise ValueError('Not a regular file')
        except FileNotFoundError:
            old = None
        if expected is not None and ((expected.get('missing') and old is not None) or (not expected.get('missing') and (old is None or old.st_mtime_ns != expected['mtime'] or old.st_size != expected['length']))):
            raise ConflictError('The file changed on the server; reopen it before saving')
        if expected is not None and expected.get('hash') and old is not None:
            original = os.open(components[-1], os.O_RDONLY | os.O_NOFOLLOW, dir_fd=parent)
            with os.fdopen(original, 'rb') as f:
                hashed = hashlib.sha256()
                for chunk in iter(lambda: f.read(1024 * 1024), b''):
                    hashed.update(chunk)
            if hashed.hexdigest() != expected['hash']:
                raise ConflictError('The file changed on the server; reopen it before saving')
        # Creation uses exclusive open; replacing preserves permissions but
        # cannot inherit setuid/setgid bits or follow a renamed symlink.
        for _ in range(100):
            temporary = '.solder-save-' + secrets.token_hex(12)
            try:
                fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600, dir_fd=parent)
                break
            except FileExistsError:
                temporary = None
        else:
            raise OSError('Could not create a temporary file')
        with os.fdopen(fd, 'wb') as f:
            if old is not None:
                os.fchmod(f.fileno(), stat.S_IMODE(old.st_mode) & 0o777)
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        if expected is not None and not expected.get('missing'):
            current = os.stat(components[-1], dir_fd=parent, follow_symlinks=False)
            if (current.st_mtime_ns, current.st_size) != (old.st_mtime_ns, old.st_size):
                raise ConflictError('The file changed on the server; reopen it before saving')
        if expected is not None and expected.get('missing'):
            os.link(temporary, components[-1], src_dir_fd=parent, dst_dir_fd=parent, follow_symlinks=False)
            os.unlink(temporary, dir_fd=parent)
        else:
            os.replace(temporary, components[-1], src_dir_fd=parent, dst_dir_fd=parent)
        temporary = None
        os.fsync(parent)
    finally:
        if temporary is not None:
            os.unlink(temporary, dir_fd=parent)
        os.close(parent)
    result = metadata(relative)
    result['hash'] = hashlib.sha256(data).hexdigest()
    return result, b''


def listed(scope):
    components = parts(scope)
    fd = directory(components)
    os.close(fd)
    start = os.path.join(ROOT, *components)
    entries, skipped = [], 0
    visible = None
    try:
        out = subprocess.run(['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z'], cwd=start,
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5)
        if out.returncode == 0 and len(out.stdout) <= HEADER_LIMIT:
            visible = {os.path.join(scope, f.decode('utf-8')) if scope else f.decode('utf-8') for f in out.stdout.split(b'\0') if f}
    except (OSError, UnicodeError, subprocess.TimeoutExpired):
        pass
    for parent, dirs, files in os.walk(start, followlinks=False):
        dirs[:] = sorted(d for d in dirs if d not in EXCLUDED and not os.path.islink(os.path.join(parent, d)))
        for name in dirs + sorted(files):
            path = os.path.join(parent, name)
            relative = os.path.relpath(path, ROOT)
            try:
                item = metadata(relative)
            except (UnicodeError, OSError, ValueError):
                skipped += 1
                continue
            if item['kind'] not in ('file', 'dir'):
                skipped += 1
                continue
            item['ignored'] = visible is not None and item['kind'] == 'file' and relative not in visible
            entries.append(item)
            if len(entries) >= ENTRY_LIMIT:
                return {'entries': entries, 'limited': True, 'skipped': skipped, 'git': git_stamp()}
    return {'entries': entries, 'limited': False, 'skipped': skipped, 'git': git_stamp()}


def git_stamp():
    try:
        output = subprocess.run(['git', '--no-optional-locks', 'status', '--porcelain=v2', '-z', '--branch'],
                                cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5)
        if output.returncode:
            return None
        import hashlib
        return hashlib.sha256(output.stdout).hexdigest()
    except (OSError, subprocess.TimeoutExpired):
        return None


def delete_at(parent, name):
    s = os.stat(name, dir_fd=parent, follow_symlinks=False)
    if stat.S_ISDIR(s.st_mode):
        fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
        try:
            for child in os.listdir(fd):
                delete_at(fd, child)
        finally:
            os.close(fd)
        os.rmdir(name, dir_fd=parent)
    else:
        os.unlink(name, dir_fd=parent)


def search(params):
    scope = params.get('path', '')
    components = parts(scope)
    fd = directory(components)
    os.close(fd)
    cwd = os.path.join(ROOT, *components)
    rg = shutil.which('rg')
    if rg is None:
        raise ValueError('Project search needs ripgrep on the target')
    argv = [rg, '--json', '--color=never', '--no-messages', '--line-buffered']
    if not params.get('regex'):
        argv.append('--fixed-strings')
    if not params.get('case'):
        argv.append('--ignore-case')
    if params.get('word'):
        argv.append('--word-regexp')
    argv.extend(['-e', params['query'], '--', '.'])
    matches, limited, amount, text_size = [], False, 0, 0
    with tempfile.TemporaryFile() as errors:
        child = subprocess.Popen(argv, cwd=cwd, stdout=subprocess.PIPE, stderr=errors)
        try:
            for raw in child.stdout:
                if len(raw) > 1024 * 1024:
                    limited = True
                    break
                item = json.loads(raw)
                if item.get('type') != 'match':
                    continue
                data = item['data']
                path = data['path'].get('text')
                text = data['lines'].get('text')
                if path is None or text is None:
                    continue
                relative = path.removeprefix('./')
                parts(relative)
                ranges = [[m['start'], m['end']] for m in data['submatches'] if m['end'] > m['start']]
                amount += len(ranges)
                matches.append({'path': relative, 'row': data['line_number'] - 1, 'text': text.rstrip('\r\n'), 'ranges': ranges})
                text_size += len(text)
                if amount >= 10000 or text_size > 8 * 1024 * 1024:
                    limited = True
                    break
            if limited:
                child.terminate()
            status = child.wait(timeout=5)
            if not limited and status not in (0, 1):
                errors.seek(0)
                raise ValueError(errors.read(8192).decode('utf-8', 'replace'))
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            child.stdout.close()
    return {'matches': matches, 'limited': limited}


def handle(method, params, data):
    relative = params.get('path', '')
    if method == 'hello':
        return {'version': VERSION, 'root': ROOT}, b''
    if method == 'list':
        return listed(relative), b''
    if method == 'search':
        return search(params), b''
    if method == 'which':
        program = params['program']
        found = shutil.which(program)
        if found is None and '/' not in program:
            for folder in (os.path.join(ROOT, 'node_modules', '.bin'), os.path.expanduser('~/.cargo/bin'), os.path.expanduser('~/.local/bin')):
                candidate = os.path.join(folder, program)
                if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
                    found = candidate
                    break
        return found, b''
    if method == 'stat':
        return metadata(relative), b''
    if method == 'read':
        return read(relative)
    if method == 'write':
        return write(relative, data, params.get('expected'))
    if method == 'mkdir':
        fd = directory(parts(relative), create=True)
        os.close(fd)
        return None, b''
    if method == 'remove':
        components = parts(relative)
        if not components:
            raise ValueError('Cannot remove the project root')
        parent = directory(components[:-1])
        try:
            if not params.get('recursive') and stat.S_ISDIR(os.stat(components[-1], dir_fd=parent, follow_symlinks=False).st_mode):
                raise IsADirectoryError('Expected a file')
            delete_at(parent, components[-1])
        finally:
            os.close(parent)
        return None, b''
    if method == 'rename':
        source, dest = parts(relative), parts(params['to'])
        if not source or not dest:
            raise ValueError('Cannot rename the project root')
        a, b = directory(source[:-1]), directory(dest[:-1])
        try:
            try:
                os.stat(dest[-1], dir_fd=b, follow_symlinks=False)
            except FileNotFoundError:
                pass
            else:
                raise FileExistsError('The destination already exists')
            os.rename(source[-1], dest[-1], src_dir_fd=a, dst_dir_fd=b)
        finally:
            os.close(a)
            os.close(b)
        return None, b''
    raise ValueError('Unknown project operation')


def exact(stream, length):
    out = bytearray()
    while len(out) < length:
        part = stream.read(min(length - len(out), 1024 * 1024))
        if not part:
            raise EOFError('The channel closed inside a frame')
        out.extend(part)
    return bytes(out)


for line in iter(lambda: sys.stdin.buffer.readline(HEADER_LIMIT + 1), b''):
    request_id = None
    try:
        if len(line) > HEADER_LIMIT or not line.endswith(b'\n'):
            raise ValueError('Invalid frame header')
        request = json.loads(line)
        request_id = request['id']
        length = request.get('bytes', 0)
        if not isinstance(length, int) or not 0 <= length <= FILE_LIMIT:
            raise ValueError('Invalid frame length')
        data = exact(sys.stdin.buffer, length)
        result, payload = handle(request['method'], request.get('params', {}), data)
        response = {'id': request_id, 'result': result, 'bytes': len(payload)}
    except (OSError, ValueError, KeyError, EOFError, UnicodeError) as error:
        payload = b''
        response = {'id': request_id, 'error': str(error), 'code': getattr(error, 'errno', None), 'conflict': isinstance(error, ConflictError), 'bytes': 0}
    sys.stdout.buffer.write(json.dumps(response).encode('utf-8') + b'\n')
    sys.stdout.buffer.write(payload)
    sys.stdout.buffer.flush()
os.close(ROOT_FD)
