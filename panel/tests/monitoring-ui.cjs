// Exercise automatic alert updates in the browser; all API traffic is intercepted.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(process.env.CHAIN_TEST_BROWSER);
const assets = path.resolve(process.argv[2]);
(async () => {
  const browser = await chromium.launch({ headless: true, executablePath: process.env.CHAIN_TEST_CHROMIUM });
  try {
    const page = await browser.newPage({ viewport: { width: 390, height: 844 } });
    page.setDefaultTimeout(15000);
    const now = Math.floor(Date.now() / 1000), writes = [], errors = [];
    page.on('pageerror', e => errors.push(e.message));
    const monitoring = {
      effective: { interval: 300, recoveries: 2, url: 'https://google.com/' },
      conditions: { 'probe:local:line': { active: true, good: 0, message: 'REALITY：本机入口 127.0.0.1:20000 · SNI example.com 未通过' } },
      checks: { 'probe:local:line': { ok: false, checked_at: now, detail: 'Connection reset by peer\n自动分段检查：出口及后续链路通过', outbound: { ok: true, detail: 'HTTP 200' } } },
    };
    const state = { time: now, nodes: [], lines: [], externals: [], checks: {}, jobs: [], busy: false, subscriptions: [], alerts: [], customers: [], payments: [], pending_imports: [], security: { totp_enabled: false, passkeys: [] } };
    const snapshot = { ...state, version: '0.2.43', agent_version: '0.2.43', local_node_id: 'node', monitoring,
      nodes: [{ id: 'node', name: '本机', seen: now, online: true, report: { running: true, applied: true, error: '', system: {} } }] };
    const fleet = { server_now: now, controller: '', policy: null, tasks: {}, remotes: [{ id: 'vps', name: 'VPS', url: 'https://vps.example', seen: now, error: '', snapshot }] };
    await page.route('**/*', async route => {
      const request = route.request(), url = new URL(request.url());
      if (url.hostname !== 'chain.test') return route.abort();
      if (url.pathname.startsWith('/api/')) {
        if (request.method() !== 'GET') writes.push(url.pathname);
        if (url.pathname === '/api/fleet/vps/forward') {
          const body = request.postDataJSON();
          if (body.path === '/state') return route.fulfill({ json: snapshot });
          assert.equal(body.path, '/lines/line/probe');
          assert.deepEqual(body.body, { mode: 'http', url: monitoring.effective.url });
          monitoring.checks['probe:local:line'] = { ok: true, checked_at: now + 600, detail: 'HTTP 200 after redirect' };
          monitoring.conditions['probe:local:line'].good += 1;
          monitoring.conditions['probe:local:line'].active = monitoring.conditions['probe:local:line'].good < 2;
          return route.fulfill({ json: monitoring.checks['probe:local:line'] });
        }
        return route.fulfill({ json: url.pathname === '/api/fleet' ? fleet : url.pathname === '/api/state' ? state : {} });
      }
      const file = path.join(assets, url.pathname === '/admin' ? 'index.html' : url.pathname);
      return route.fulfill({ body: fs.readFileSync(file), contentType: { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css' }[path.extname(file)] || 'application/octet-stream' });
    });
    await page.goto('http://chain.test/admin#fleet');
    await page.getByText('VPS · 入口检测异常，出口正常', { exact: true }).waitFor();
    await page.getByRole('button', { name: '查看 VPS', exact: true }).first().click();
    const dialog = page.getByRole('dialog');
    await dialog.getByText(/20000.*SNI example.com.*自动复查/).waitFor();
    await dialog.getByText('自动检测详情', { exact: true }).click();
    await dialog.getByText(/Connection reset by peer/).waitFor();
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
    monitoring.checks['probe:local:line'] = { ok: true, checked_at: now + 300, detail: 'HTTP 200' };
    monitoring.conditions['probe:local:line'].good = 1;
    await dialog.getByText('入口恢复确认中', { exact: true }).first().waitFor();
    await dialog.getByText(/自动确认恢复（1\/2）/).waitFor();
    monitoring.conditions['probe:local:line'].active = false;
    await dialog.getByText('入口恢复确认中', { exact: true }).first().waitFor({ state: 'hidden' });
    assert.deepEqual(writes, [], '恢复过程不能要求点击检测或修改业务配置');
    monitoring.checks['probe:local:line'] = { ok: false, checked_at: now + 400, detail: 'fixture failure' };
    monitoring.conditions['probe:local:line'].good = 0;
    await dialog.getByText('连接检测待确认', { exact: true }).first().waitFor();
    monitoring.conditions['probe:local:line'].active = true;
    await dialog.getByRole('button', { name: '立即复查', exact: true }).click();
    await dialog.getByRole('status').getByText(/本次连接检测通过.*恢复确认 1\/2/).waitFor();
    await dialog.getByRole('button', { name: '立即复查', exact: true }).click();
    await dialog.getByRole('button', { name: '立即复查', exact: true }).waitFor({ state: 'hidden' });
    await dialog.getByRole('status').getByText('本次连接检测通过。', { exact: true }).waitFor();
    assert.deepEqual(writes, Array(4).fill('/api/fleet/vps/forward'), '手动复查只检测并刷新状态');
    assert.deepEqual(errors, []);
    console.log('monitoring UI: automatic recovery, pending status and manual recheck passed');
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
