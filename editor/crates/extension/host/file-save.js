'use strict';

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const childProcess = require('child_process');

const copy = (args) => new Promise((resolve, reject) => {
  childProcess.execFile('/bin/cp', args, (error) => {
    if (error) reject(error);
    else resolve();
  });
});

const changed = () => new Error('The notebook file changed on disk');
const hash = (bytes) => crypto.createHash('sha256').update(bytes).digest('hex');

function revision(resolved, stat, sha256) {
  return { resolved, size: stat.size, mtime: stat.mtimeNs, dev: stat.dev, ino: stat.ino, sha256 };
}

function same(left, right) {
  return left.resolved === right.resolved && left.size === right.size
    && left.mtime === right.mtime && left.dev === right.dev && left.ino === right.ino
    && left.sha256 === right.sha256;
}

// Keep the revision of the bytes handed to the serializer, before it can
// wait or mutate them. A path rebound during the read is not that source.
async function read(file) {
  const resolved = await fs.promises.realpath(file);
  const source = await fs.promises.open(resolved, 'r');
  try {
    const before = await source.stat({ bigint: true });
    if (!before.isFile()) throw new Error('The notebook is not a regular file');
    const bytes = await source.readFile();
    const after = await source.stat({ bigint: true });
    const expected = revision(resolved, after, hash(bytes));
    if (before.size !== after.size || before.mtimeNs !== after.mtimeNs
      || before.ctimeNs !== after.ctimeNs || BigInt(bytes.length) !== after.size
      || await fs.promises.realpath(file) !== resolved) throw changed();
    const named = await fs.promises.stat(resolved, { bigint: true });
    if (!same(expected, revision(resolved, named, expected.sha256))
      || named.ctimeNs !== after.ctimeNs) throw changed();
    return { bytes, revision: expected, stat: after };
  } finally {
    await source.close();
  }
}

async function writable(source) {
  if ((source.stat.mode & 0o222n) === 0n) throw new Error('The notebook file is read-only');
  if (source.stat.nlink !== 1n) throw new Error('The notebook file has hard links');
  await fs.promises.access(source.revision.resolved, fs.constants.W_OK);
}

// A missing name is create-only. A dangling symlink is still an existing
// name and must never be replaced as though it were missing.
async function destination(file) {
  try {
    const source = await read(file);
    return { resolved: source.revision.resolved, revision: source.revision };
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    try {
      await fs.promises.lstat(file);
    } catch (missing) {
      if (missing.code !== 'ENOENT') throw missing;
      const parent = await fs.promises.realpath(path.dirname(file));
      return { resolved: path.join(parent, path.basename(file)), revision: null };
    }
    throw changed();
  }
}

async function distinct(source, original, target) {
  const names = new Set([original.resolved, path.resolve(source)]);
  let current;
  try {
    const resolved = await fs.promises.realpath(source);
    names.add(resolved);
    current = await fs.promises.stat(resolved, { bigint: true });
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    // Recovery can outlive the source directory. Where the parent remains,
    // follow dangling source symlinks too: creating their target is still
    // an export back to the source, rather than an independent copy.
    let name = path.resolve(source);
    for (let hop = 0; hop < 40; hop += 1) {
      let parent;
      let entry;
      try {
        parent = await fs.promises.realpath(path.dirname(name));
        name = path.join(parent, path.basename(name));
        names.add(name);
        entry = await fs.promises.lstat(name);
      } catch (missing) {
        if (missing.code !== 'ENOENT') throw missing;
        break;
      }
      if (!entry.isSymbolicLink()) break;
      name = path.resolve(parent, await fs.promises.readlink(name));
      names.add(name);
      if (hop === 39) throw changed();
    }
  }
  const stamp = target.revision;
  if (names.has(target.resolved) || (stamp && (
    (stamp.dev === original.dev && stamp.ino === original.ino)
    || (current && stamp.dev === current.dev && stamp.ino === current.ino)
  ))) throw new Error('A notebook copy cannot replace its source');
}

async function prepareCopy(source, original, file) {
  const target = await destination(file);
  await distinct(source, original, target);
  return target;
}

const writes = new Map();
function queued(target, work) {
  const pending = (writes.get(target) || Promise.resolve()).then(work);
  const tail = pending.catch(() => {}).finally(() => {
    if (writes.get(target) === tail) writes.delete(target);
  });
  writes.set(target, tail);
  return pending;
}

function save(file, expected, bytes) {
  return queued(expected.resolved, () => write(file, { resolved: expected.resolved, revision: expected }, bytes));
}

function writeCopy(source, original, file, expected, bytes) {
  return queued(expected.resolved, () => write(file, expected, bytes, async (current) => {
    await distinct(source, original, current);
  }));
}

async function write(file, target, bytes, checkCopy) {
  const expected = target.revision;
  const source = expected ? await read(file) : undefined;
  if (expected) {
    if (!same(expected, source.revision)) throw changed();
    await writable(source);
  } else {
    const current = await destination(file);
    if (current.revision || current.resolved !== target.resolved) throw changed();
  }
  if (checkCopy) await checkCopy(target);
  const resolved = target.resolved;
  const temp = path.join(path.dirname(resolved), `.${path.basename(resolved)}.solder-${crypto.randomBytes(16).toString('hex')}.tmp`);
  let created = false;
  let staged;
  let identity;
  try {
    staged = await fs.promises.open(temp, 'wx', 0o600);
    created = true;
    identity = await staged.stat({ bigint: true });
    if (source && (process.platform === 'darwin' || process.platform === 'linux')) {
      // An existing destination keeps the exclusively created inode. GNU
      // --preserve=all may ignore xattr failures, so name the attributes.
      const options = process.platform === 'darwin' ? ['-p']
        : ['--attributes-only', '--preserve=mode,ownership,timestamps,xattr'];
      await copy([...options, resolved, temp]);
    }
    // Other platforms preserve basic mode only, with no ACL/xattr copier.
    const copied = await staged.stat({ bigint: true });
    const named = await fs.promises.lstat(temp, { bigint: true });
    if (copied.dev !== identity.dev || copied.ino !== identity.ino
      || named.dev !== identity.dev || named.ino !== identity.ino
      || !named.isFile() || copied.nlink !== 1n) throw new Error('The notebook staging file changed');
    if (source && (copied.uid !== source.stat.uid || copied.gid !== source.stat.gid)) {
      throw new Error('Could not preserve the notebook file owner and group');
    }
    if ((copied.mode & 0o222n) === 0n) throw new Error('The notebook file is read-only');
    await staged.truncate(0);
    await staged.writeFile(bytes);
    if (source) await staged.chmod(Number(source.stat.mode & 0o777n));
    await staged.sync();
    const next = revision(resolved, await staged.stat({ bigint: true }), hash(bytes));
    await staged.close();
    staged = undefined;
    if (expected) {
      const current = await read(file);
      if (!same(expected, current.revision) || current.stat.mode !== source.stat.mode
        || current.stat.uid !== source.stat.uid || current.stat.gid !== source.stat.gid
        || current.stat.ctimeNs !== source.stat.ctimeNs) throw changed();
      await writable(current);
    } else {
      const current = await destination(file);
      if (current.revision || current.resolved !== resolved) throw changed();
    }
    if (checkCopy) await checkCopy(target);
    const commit = await fs.promises.lstat(temp, { bigint: true });
    if (!commit.isFile() || commit.dev !== identity.dev || commit.ino !== identity.ino
      || commit.nlink !== 1n || commit.size !== next.size || commit.mtimeNs !== next.mtime) {
      throw new Error('The notebook staging file changed');
    }
    // Node cannot lock an independent writer across this check and rename.
    if (expected) await fs.promises.rename(temp, resolved);
    else {
      // Publishing a complete inode with link is exclusive even if another
      // writer creates this name after our final missing-name check.
      await fs.promises.link(temp, resolved);
      await fs.promises.unlink(temp);
    }
    created = false;
    return next;
  } finally {
    try {
      if (staged) await staged.close();
    } finally {
      // An exclusive-open failure owns no file, including a name collision.
      if (created) {
        try {
          const named = await fs.promises.lstat(temp, { bigint: true });
          if (!identity || (named.dev === identity.dev && named.ino === identity.ino)) {
            await fs.promises.unlink(temp);
          }
        } catch (error) {
          if (error.code !== 'ENOENT') throw error;
        }
      }
    }
  }
}

module.exports = { read, save, prepareCopy, writeCopy };
