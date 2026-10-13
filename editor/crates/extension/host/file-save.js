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

async function save(file, expected, bytes) {
  const source = await read(file);
  if (!same(expected, source.revision)) throw changed();
  await writable(source);
  const resolved = expected.resolved;
  const temp = path.join(path.dirname(resolved), `.${path.basename(resolved)}.solder-${crypto.randomBytes(16).toString('hex')}.tmp`);
  let created = false;
  let staged;
  let identity;
  try {
    staged = await fs.promises.open(temp, 'wx', 0o600);
    created = true;
    identity = await staged.stat({ bigint: true });
    if (process.platform === 'darwin' || process.platform === 'linux') {
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
    if (copied.uid !== source.stat.uid || copied.gid !== source.stat.gid) {
      throw new Error('Could not preserve the notebook file owner and group');
    }
    if ((copied.mode & 0o222n) === 0n) throw new Error('The notebook file is read-only');
    await staged.truncate(0);
    await staged.writeFile(bytes);
    await staged.chmod(Number(source.stat.mode & 0o777n));
    await staged.sync();
    const next = revision(resolved, await staged.stat({ bigint: true }), hash(bytes));
    await staged.close();
    staged = undefined;
    const current = await read(file);
    if (!same(expected, current.revision) || current.stat.mode !== source.stat.mode
      || current.stat.uid !== source.stat.uid || current.stat.gid !== source.stat.gid
      || current.stat.ctimeNs !== source.stat.ctimeNs) throw changed();
    await writable(current);
    const commit = await fs.promises.lstat(temp, { bigint: true });
    if (!commit.isFile() || commit.dev !== identity.dev || commit.ino !== identity.ino
      || commit.nlink !== 1n || commit.size !== next.size || commit.mtimeNs !== next.mtime) {
      throw new Error('The notebook staging file changed');
    }
    // Node cannot lock an independent writer across this check and rename.
    await fs.promises.rename(temp, resolved);
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

module.exports = { read, save };
