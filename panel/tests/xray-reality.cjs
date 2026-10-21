// Real cores, isolated loopback listeners and a TLS certificate larger than 8 KiB.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const net = require('node:net');
const tls = require('node:tls');
const http = require('node:http');
const crypto = require('node:crypto');
const { spawn, execFileSync } = require('node:child_process');
const root = fs.mkdtempSync(path.join(process.env.CHAIN_TEST_ROOT, 'reality-'));
const children = [], servers = [], sockets = new Set(), ports = [];
const listen = server => new Promise((resolve, reject) => {
  servers.push(server);
  server.on('connection', socket => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); });
  server.once('error', reject).listen(0, '127.0.0.1', () => {
    const port = server.address().port; ports.push(port); resolve(port);
  });
});
const freePort = async () => {
  const server = net.createServer(); const port = await listen(server);
  await new Promise(resolve => server.close(resolve)); return port;
};
const stop = async child => {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const done = new Promise(resolve => child.once('exit', resolve));
  child.kill('SIGTERM');
  const timer = setTimeout(() => child.kill('SIGKILL'), 2000);
  await done; clearTimeout(timer);
};
async function probe(binary, target, name) {
  const { privateKey, publicKey } = crypto.generateKeyPairSync('x25519');
  const secret = privateKey.export({ type: 'pkcs8', format: 'der' }).subarray(-32).toString('base64url');
  const key = publicKey.export({ type: 'spki', format: 'der' }).subarray(-32).toString('base64url');
  const uuid = crypto.randomUUID(), sid = crypto.randomBytes(8).toString('hex');
  const serverPort = await freePort(), socksPort = await freePort();
  const marker = crypto.randomUUID();
  const echo = http.createServer((_, res) => res.end(marker));
  const echoPort = await listen(echo);
  const configs = [
    { log: { loglevel: 'debug' }, inbounds: [{ listen: '127.0.0.1', port: serverPort, protocol: 'vless',
      settings: { clients: [{ id: uuid, flow: 'xtls-rprx-vision' }], decryption: 'none' },
      streamSettings: { network: 'tcp', security: 'reality', realitySettings: {
        show: true, target, serverNames: [name], privateKey: secret, shortIds: [sid] } } }],
      outbounds: [{ protocol: 'freedom' }] },
    { log: { loglevel: 'debug' }, inbounds: [{ listen: '127.0.0.1', port: socksPort, protocol: 'socks', settings: { auth: 'noauth' } }],
      outbounds: [{ protocol: 'vless', settings: { vnext: [{ address: '127.0.0.1', port: serverPort,
        users: [{ id: uuid, encryption: 'none', flow: 'xtls-rprx-vision' }] }] },
      streamSettings: { network: 'tcp', security: 'reality', realitySettings: {
        serverName: name, publicKey: key, shortId: sid, fingerprint: 'chrome' } } }] }
  ];
  const pair = [];
  try {
    for (const [index, config] of configs.entries()) {
      const file = path.join(root, `${children.length}-${index}.json`);
      fs.writeFileSync(file, JSON.stringify(config), { mode: 0o600 });
      const child = spawn(binary, ['run', '-config', file], { stdio: ['ignore', 'pipe', 'pipe'] });
      child.log = ''; child.stdout.on('data', b => child.log += b); child.stderr.on('data', b => child.log += b);
      children.push(child); pair.push(child);
    }
    fs.writeFileSync(path.join(root, 'processes.json'), JSON.stringify({ pid: process.pid, cwd: root, children: children.map(p => p.pid), ports }));
    await new Promise(resolve => setTimeout(resolve, 6500));
    const result = await new Promise((resolve, reject) => {
      const child = spawn('curl', ['--silent', '--show-error', '--max-time', '15', '--noproxy', '',
        '--socks5-hostname', `127.0.0.1:${socksPort}`, `http://127.0.0.1:${echoPort}/`]);
      children.push(child); let body = ''; child.stdout.on('data', b => body += b); child.stderr.resume();
      child.once('error', reject); child.once('exit', code => resolve({ code, body }));
    });
    return { ok: result.code === 0 && result.body === marker,
      records: [...pair[0].log.matchAll(/Certificate:\s*(\d+)/g)].map(m => Number(m[1])) };
  } finally {
    await Promise.all(pair.map(stop));
    echo.closeAllConnections(); await new Promise(resolve => echo.close(resolve));
  }
}
(async () => {
  const cert = path.join(root, 'cert.pem'), key = path.join(root, 'key.pem');
  const names = ['DNS:localhost', ...Array.from({ length: 500 }, (_, i) => `DNS:certificate-buffer-${i}.test`)];
  execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', key, '-out', cert,
    '-days', '1', '-subj', '/CN=localhost', '-addext', `subjectAltName=${names.join(',')}`], { stdio: 'ignore' });
  const port = await listen(tls.createServer({ key: fs.readFileSync(key), cert: fs.readFileSync(cert),
    minVersion: 'TLSv1.3', ALPNProtocols: ['h2', 'http/1.1'] }));
  const patched = await probe(process.env.CHAIN_TEST_XRAY, `127.0.0.1:${port}`, 'localhost');
  assert(patched.ok, 'patched core must exchange authenticated data with a large target certificate');
  assert(patched.records.some(size => size > 8192), `expected certificate record > 8192: ${patched.records}`);
  console.log('patched large-certificate handshake passed; record sizes:', patched.records);
  if (process.env.CHAIN_TEST_OLD_XRAY) {
    const old = await probe(process.env.CHAIN_TEST_OLD_XRAY, `127.0.0.1:${port}`, 'localhost');
    assert(!old.ok, 'unpatched baseline must reproduce the failure');
    assert(old.records.some(size => size > 8192));
    console.log('unpatched baseline reproduced the failure; record sizes:', old.records);
  }
  if (process.env.CHAIN_TEST_REALITY_TARGET) {
    const name = process.env.CHAIN_TEST_REALITY_TARGET;
    const result = await probe(process.env.CHAIN_TEST_XRAY, `${name}:443`, name);
    assert(result.ok, `${name} REALITY handshake and data exchange failed`);
    console.log(`${name} handshake and data exchange passed; record sizes:`, result.records);
  }
})().catch(error => { console.error(error.message); process.exitCode = 1; }).finally(async () => {
  await Promise.all(children.map(stop));
  for (const socket of sockets) socket.destroy();
  await Promise.all(servers.filter(s => s.listening).map(s => new Promise(resolve => s.close(resolve))));
  fs.rmSync(root, { recursive: true });
  console.log('Cleaned test processes, loopback listeners and files:', root);
});
