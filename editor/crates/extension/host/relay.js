'use strict';
// Runs in a terminal of the dock, for a terminal an extension draws
// itself: `node relay.js <port> <token>`.
//
// Such a terminal has no program behind it. What is typed goes to an
// object in the extension's code, and what that object writes is shown. So
// the terminal runs this, which carries both ways between the terminal it
// was started in and the extension's host, on a port of this machine. The
// token is what the host made up for this terminal: nothing else on the
// machine that finds the port is taken for it.
//
// Each message is a letter, four bytes of length, and that many bytes: `t`
// the token, `d` what was typed or is to be shown, `r` the size of the
// terminal, `c` what the terminal ended with.

const net = require('net');

const [port, token] = process.argv.slice(2);
const socket = net.connect(Number(port), '127.0.0.1');

function send(type, payload) {
  const body = Buffer.from(payload);
  const head = Buffer.alloc(5);
  head.write(type, 0, 'latin1');
  head.writeUInt32BE(body.length, 1);
  socket.write(Buffer.concat([head, body]));
}
const size = () => JSON.stringify({ columns: process.stdout.columns || 80, rows: process.stdout.rows || 24 });
// Shown to the last byte before it goes.
const leave = (code) => process.stdout.write('', () => process.exit(code));

socket.on('connect', () => {
  send('t', token);
  send('r', size());
  // Keys as they are pressed, not a line at a time, and not shown twice.
  if (process.stdin.isTTY) process.stdin.setRawMode(true);
  process.stdin.on('data', (typed) => send('d', typed));
  process.stdin.on('end', () => socket.end());
  process.stdout.on('resize', () => send('r', size()));
});

let pending = Buffer.alloc(0);
socket.on('data', (chunk) => {
  pending = Buffer.concat([pending, chunk]);
  while (pending.length >= 5) {
    const length = pending.readUInt32BE(1);
    if (pending.length < 5 + length) return;
    const type = String.fromCharCode(pending[0]);
    const body = pending.subarray(5, 5 + length);
    pending = pending.subarray(5 + length);
    if (type === 'd') process.stdout.write(body);
    else if (type === 'c') leave(Number(body.toString()) || 0);
  }
});
socket.on('close', () => leave(0));
socket.on('error', () => leave(1));
