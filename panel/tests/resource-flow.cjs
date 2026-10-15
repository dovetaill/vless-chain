// Task-owned browser checks for local creation and external proxy management.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(process.env.CHAIN_TEST_BROWSER);
const assets = path.resolve(process.argv[2]);
(async () => {
  const server = await chromium.launchServer({ headless: true });
  const browser = await chromium.connect(server.wsEndpoint());
  const root = process.env.CHAIN_TEST_ROOT;
  if (root) fs.writeFileSync(path.join(root, 'ui-ownership.json'), JSON.stringify({ pid: process.pid, browser: server.process().pid, artifacts: root }));
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    const errors = [], writes = [];
    page.on('pageerror', e => errors.push(e.message));
    const node = { id: 'local', name: '本机', host: '192.0.2.20', online: true, target: 'www.microsoft.com:443', server_name: 'www.microsoft.com', seen: 0, report: { running: true, applied: true, error: '', revision: '', traffic: {}, system: {} } };
    const state = { nodes: [node], local_node_id: 'local', externals: [
      { id: 'a', name: '落地 A', url: 'vless://00000000-0000-4000-8000-000000000001@a.example:443?security=tls' },
      { id: 'b', name: '落地 B', url: 'socks5://user:password@b.example:1080' },
    ], lines: [], checks: {}, jobs: [], busy: false, time: 0, subscriptions: [], security: { totp_enabled: false, recovery_remaining: 0, origin: '', passkeys: [] }, alerts: [] };
    await page.route('http://chain.test/**', async route => {
      const request = route.request(), url = new URL(request.url());
      if (url.pathname.startsWith('/api/')) {
        let body = state;
        if (url.pathname.endsWith('/export')) body = { node: '- ' + JSON.stringify({ name: '落地', type: 'vless', server: 'a.example', port: 443 }), share: state.externals[0].url };
        if (request.method() !== 'GET') {
          const data = request.postDataJSON(); writes.push({ path: url.pathname, body: data });
          if (url.pathname === '/api/local-node') { state.local_node_id = 'local'; state.nodes = [{ ...node, host: data.host }]; body = { id: 'local' }; }
          else if (url.pathname === '/api/lines') { body = { ...data, id: data.id || String(state.lines.length + 1), published: false, traffic: null }; state.lines = [...state.lines.filter(l => l.id !== body.id), body]; }
          else if (url.pathname === '/api/preview') body = { affected_line_ids: [], affected_lines: [], message: '', nodes: [] };
          else if (url.pathname === '/api/publish') { state.lines.forEach(l => l.published = true); state.jobs = [{ id: 'job', state: 'applied', events: [] }]; body = { job_id: 'job' }; }
          else if (url.pathname === '/api/externals') { body = { id: 'imported', name: data.name || 'Mihomo 落地', url: state.externals[0].url }; state.externals.push(body); }
          else throw new Error('Unexpected mutation: ' + url.pathname);
        }
        return route.fulfill({ json: body });
      }
      const file = path.join(assets, url.pathname === '/' ? 'index.html' : url.pathname);
      return fs.existsSync(file) ? route.fulfill({ body: fs.readFileSync(file), contentType: { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css' }[path.extname(file)] || 'application/octet-stream' }) : route.fulfill({ status: 404 });
    });
    const button = name => page.getByRole('button', { name, exact: true });
    const close = () => page.getByRole('dialog').last().getByRole('button', { name: '关闭', exact: true }).first().click();
    const create = async name => { await button('新建代理').first().click(); await page.getByLabel('名称', { exact: true }).fill(name); };
    const save = async () => { await page.getByRole('dialog').getByRole('button', { name: '创建代理', exact: true }).click(); await page.getByRole('heading', { name: '分享代理', exact: true }).waitFor(); await close(); };
    await page.goto('http://chain.test/#lines');
    assert.equal(await page.locator('aside nav button').count(), 3);
    for (const name of ['代理', '外部代理', '设置']) assert.equal(await page.locator('aside nav').getByRole('button', { name, exact: true }).count(), 1);
    await create('VLESS A');
    assert.equal(await page.getByLabel('入口服务器', { exact: true }).count(), 0);
    assert.equal(await button('添加远程服务器').count(), 0);
    assert.equal(await page.getByText('使用购买的中转入口').count(), 0);
    assert.equal(await page.getByLabel('本机 IP 或域名').inputValue(), '192.0.2.20');
    assert.equal(await page.getByLabel('协议').inputValue(), 'vless');
    assert.equal(await page.getByLabel('出网方式').inputValue(), 'direct');
    if (root) await page.screenshot({ path: path.join(root, 'local-create.png') });
    await save();
    const first = writes.find(w => w.path === '/api/lines').body;
    assert.deepEqual(first.hops, [{ node_id: 'local', port: null }]);
    assert.equal(first.access.tls.mode, 'off');
    assert.equal(writes.filter(w => w.path === '/api/local-node').length, 0);
    await create('VLESS B');
    await page.getByLabel('出网方式').selectOption('chain');
    await page.getByLabel('落地 1', { exact: true }).selectOption('external:a');
    await button('添加下一跳').click();
    await page.getByLabel('落地 2', { exact: true }).selectOption('external:b');
    assert.match(await page.getByLabel('代理路径').innerText(), /本机.*落地 A.*落地 B.*互联网/);
    await page.getByLabel('本机 IP 或域名').fill('proxy.example.com');
    assert.match(await page.getByRole('dialog').innerText(), /使用 REALITY/);
    await page.getByText('传输安全 · REALITY', { exact: true }).click();
    assert.equal(await page.getByLabel('HTTPS / 证书').inputValue(), 'off');
    await page.getByLabel('协议').selectOption('trojan');
    assert.equal(await page.getByLabel('HTTPS / 证书').inputValue(), 'auto');
    await page.getByLabel('协议').selectOption('vless');
    assert.equal(await page.getByLabel('HTTPS / 证书').inputValue(), 'off');
    await page.getByLabel('HTTPS / 证书').selectOption('manual');
    await page.getByLabel('本机 IP 或域名').fill('changed.example.com');
    assert.equal(await page.getByLabel('HTTPS / 证书').inputValue(), 'manual');
    await page.getByLabel('HTTPS / 证书').selectOption('off');
    await page.getByLabel('本机 IP 或域名').fill('proxy.example.com');
    if (root) await page.screenshot({ path: path.join(root, 'proxy-chain.png') });
    await save();
    const chained = writes.filter(w => w.path === '/api/lines').at(-1).body;
    assert.deepEqual(chained.chain, ['external:a', 'external:b']);
    assert.equal(chained.access.tls.mode, 'off');
    assert.equal(chained.access.tls.domain, '');
    assert.equal(state.lines.length, 2);
    for (const protocol of ['vmess', 'hysteria2', 'tuic']) {
      await create(protocol);
      await page.getByLabel('协议').selectOption(protocol);
      await page.getByLabel('本机 IP 或域名').fill('proxy.example.com');
      await save();
      const body = writes.filter(w => w.path === '/api/lines').at(-1).body;
      assert.equal(body.access.protocol, protocol);
      assert.equal(body.access.tls.mode, 'auto');
      assert.equal(body.access.tls.domain, 'proxy.example.com');
    }
    await page.locator('aside nav').getByRole('button', { name: '外部代理', exact: true }).click();
    assert.equal(await button('添加中转入口').count(), 0);
    await button('链接 / Mihomo').first().click();
    await page.getByLabel('Mihomo 节点配置').waitFor();
    await button('链接 / 二维码').click();
    assert.match(await page.getByLabel('分享链接').inputValue(), /^vless:/);
    await close();
    await button('导入外部代理').click();
    const yaml = '- {name: imported, type: vless, server: a.example, port: 443, tls: true}';
    await page.getByLabel('链接或 Mihomo 节点').fill(yaml);
    await button('导入').click();
    await page.getByRole('heading', { name: '外部代理配置', exact: true }).waitFor();
    assert.equal(writes.find(w => w.path === '/api/externals').body.url, yaml);
    await close();
    // Reproduce the screenshot: an orphan local identity with no server row.
    state.nodes = []; state.local_node_id = 'orphan'; state.lines = [];
    await page.goto('http://chain.test/#lines'); await page.reload();
    await create('首次本机代理');
    await page.getByLabel('本机 IP 或域名').fill('192.0.2.20');
    await save();
    assert.equal(writes.filter(w => w.path === '/api/local-node').length, 1);
    assert.equal(writes.filter(w => w.path === '/api/lines').at(-1).body.hops[0].node_id, 'local');
    await create('手机布局');
    await page.setViewportSize({ width: 390, height: 844 });
    const box = await page.getByRole('dialog').boundingBox();
    assert.ok(box.x >= 0 && box.x + box.width <= 390);
    await close();
    assert.deepEqual(errors, []);
    console.log('PASS local defaults, domain REALITY, explicit TLS preserved, ordered chain, Mihomo import/export, orphan local identity and mobile form');
  } finally {
    await browser.close(); await server.close();
    console.log('CLEANUP browser pid=' + server.process().pid);
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
