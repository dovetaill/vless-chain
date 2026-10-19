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
    const node = { id: 'local', name: '本机', host: '192.0.2.20', online: true, target: 'www.cloudflare.com:443', server_name: 'www.cloudflare.com', seen: 0, report: { running: true, applied: true, error: '', revision: '', traffic: {}, system: {} } };
    const state = { nodes: [node], local_node_id: 'local', externals: [
      { id: 'a', name: '落地 A', url: 'vless://00000000-0000-4000-8000-000000000001@a.example:443?security=tls' },
      { id: 'b', name: '落地 B', url: 'socks5://user:password@b.example:1080' },
    ], lines: [], checks: {}, jobs: [], busy: false, time: 0, subscriptions: [], security: { totp_enabled: false, recovery_remaining: 0, origin: '', passkeys: [] }, alerts: [], customers: [], payments: [], renewal_contact: '' };
    await page.route('http://chain.test/**', async route => {
      try {
      const request = route.request(), url = new URL(request.url());
      if (url.pathname.startsWith('/api/')) {
        let body = state;
        if (url.pathname === '/api/auth/options') body = { totp: false, passkey: false };
        if (url.pathname.endsWith('/export')) body = { node: '- ' + JSON.stringify({ name: '落地', type: 'vless', server: 'a.example', port: 443 }), share: state.externals[0].url };
        if (request.method() !== 'GET') {
          const data = request.postDataJSON(); writes.push({ path: url.pathname, body: data });
          if (url.pathname === '/api/local-node') { state.local_node_id = 'local'; state.nodes = [{ ...node, host: data.host }]; body = { id: 'local' }; }
          else if (url.pathname === '/api/lines') { body = { ...data, id: data.id || String(state.lines.length + 1), published: false, traffic: null, rental: null, rental_status: 'draft', sync_status: 'unpublished' }; state.lines = [...state.lines.filter(l => l.id !== body.id), body]; }
          else if (url.pathname === '/api/lines/batch') { body = data.map((line, index) => ({ ...line, id: String(state.lines.length + index + 1), published: false, traffic: null, rental: null, rental_status: 'draft', sync_status: 'unpublished' })); state.lines.push(...body); }
          else if (url.pathname === '/api/reality/check') body = { ok: true, detail: 'TLS 1.3、证书/SNI 与 H2 检查通过', latency_ms: 12 };
          else if (url.pathname === '/api/preview') body = { affected_line_ids: [], affected_lines: [], message: '', nodes: [] };
          else if (url.pathname === '/api/publish') { state.lines.forEach(l => { l.published = true; l.rental_status = 'active'; l.sync_status = 'synced'; }); state.jobs = [{ id: 'job', state: 'applied', events: [] }]; body = { job_id: 'job' }; }
          else if (url.pathname === '/api/externals') { body = { id: 'imported', name: data.name || 'Mihomo 落地', url: state.externals[0].url }; state.externals.push(body); }
          else throw new Error('Unexpected mutation: ' + url.pathname);
        }
        return route.fulfill({ json: body });
      }
      const file = path.join(assets, ['/', '/admin', '/admin/'].includes(url.pathname) ? 'index.html' : url.pathname);
      return fs.existsSync(file) ? route.fulfill({ body: fs.readFileSync(file), contentType: { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css' }[path.extname(file)] || 'application/octet-stream' }) : route.fulfill({ status: 404 });
      } catch (error) {
        errors.push(error.message);
        await route.fulfill({ status: 500, json: { error: error.message } }).catch(() => {});
      }
    });
    const button = name => page.getByRole('button', { name, exact: true });
    const close = () => page.getByRole('dialog').last().getByRole('button', { name: '关闭', exact: true }).first().click();
    const create = async name => { await button('新建代理').first().click(); await page.getByLabel('名称', { exact: true }).fill(name); };
    const save = async () => { await page.getByRole('dialog').getByRole('button', { name: '创建代理', exact: true }).click(); await page.getByRole('heading', { name: '分享代理', exact: true }).waitFor(); await close(); };
    await page.goto('http://chain.test/admin#lines');
    assert.equal(await page.locator('aside nav button').count(), 7);
    for (const name of ['总览', '代理', '客户与收款', '外部代理', '多机总控', '监控告警', '设置']) assert.equal(await page.locator('aside nav').getByRole('button', { name, exact: true }).count(), 1);
    await create('VLESS A');
    assert.equal(await page.getByLabel('入口服务器', { exact: true }).count(), 0);
    assert.equal(await button('添加远程服务器').count(), 0);
    assert.equal(await page.getByText('使用购买的中转入口').count(), 0);
    assert.equal(await page.getByLabel('本机 IP 或域名').inputValue(), '192.0.2.20');
    assert.equal(await page.getByLabel('协议', { exact: true }).inputValue(), 'vless');
    assert.equal(await page.getByLabel('出网方式').inputValue(), 'direct');
    const target = page.getByLabel('REALITY 目标地址', { exact: true });
    const sni = page.getByLabel('REALITY SNI', { exact: true });
    assert.equal(await target.inputValue(), 'www.cloudflare.com:443');
    assert.equal(await sni.inputValue(), 'www.cloudflare.com');
    for (const [name, host] of [
      ['微软', 'www.microsoft.com'], ['苹果', 'www.apple.com'], ['AWS', 'aws.amazon.com'],
      ['三星', 'www.samsung.com'], ['索尼', 'www.sony.com'], ['戴尔', 'www.dell.com'],
      ['惠普', 'www.hp.com'], ['Cloudflare', 'www.cloudflare.com'],
    ]) {
      await button(name).click();
      assert.equal(await target.inputValue(), `${host}:443`);
      assert.equal(await sni.inputValue(), host);
    }
    assert.equal(writes.filter(w => w.path === '/api/lines').length, 0);
    await button('检测目标与 SNI').click();
    const checked = page.getByText('通过：TLS 1.3、证书/SNI 与 H2 检查通过（12 ms）');
    await checked.waitFor();
    assert.deepEqual(writes.find(w => w.path === '/api/reality/check').body, { target: 'www.cloudflare.com:443', server_name: 'www.cloudflare.com' });
    await button('苹果').click();
    await checked.waitFor({ state: 'hidden' });
    if (root) await page.screenshot({ path: path.join(root, 'local-create.png') });
    await save();
    const first = writes.find(w => w.path === '/api/lines').body;
    assert.deepEqual(first.hops, [{ node_id: 'local', port: null }]);
    assert.equal(first.access.tls.mode, 'off');
    assert.deepEqual(first.access.reality, { target: 'www.apple.com:443', server_name: 'www.apple.com' });
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
    await page.getByLabel('协议', { exact: true }).selectOption('trojan');
    assert.equal(await page.getByLabel('HTTPS / 证书').inputValue(), 'auto');
    await page.getByLabel('协议', { exact: true }).selectOption('vless');
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
      await page.getByLabel('协议', { exact: true }).selectOption(protocol);
      await page.getByLabel('本机 IP 或域名').fill('proxy.example.com');
      await save();
      const body = writes.filter(w => w.path === '/api/lines').at(-1).body;
      assert.equal(body.access.protocol, protocol);
      assert.equal(body.access.tls.mode, 'auto');
      assert.equal(body.access.tls.domain, 'proxy.example.com');
    }
    await create('多组合');
    await page.getByLabel('一次创建多种组合').check();
    await page.getByLabel('本机 IP 或域名').fill('batch.example.com');
    await page.getByLabel('REALITY + XHTTP', { exact: true }).check();
    await page.getByLabel('TLS + XHTTP', { exact: true }).check();
    await button('微软').click();
    await page.getByLabel('REALITY 目标地址', { exact: true }).fill('cover.example.com:443');
    await page.getByLabel('REALITY SNI', { exact: true }).fill('cover.example.com');
    await button('检测目标与 SNI').click();
    await page.getByText('通过：TLS 1.3、证书/SNI 与 H2 检查通过（12 ms）').waitFor();
    await page.getByLabel('XHTTP 路径', { exact: true }).fill('/batch-path');
    await page.getByText('监听端口 · 自动分配', { exact: true }).click();
    await page.getByLabel('使用统一 443 入口（按 SNI 分流）').check();
    await button('创建所选组合').click();
    await page.getByText('相同 SNI 无法同时分流 TCP 和 XHTTP；请选择一种传输，或使用独立端口。', { exact: true }).waitFor();
    assert.equal(writes.filter(w => w.path === '/api/lines/batch').length, 0);
    await page.getByLabel('使用统一 443 入口（按 SNI 分流）').uncheck();
    assert.equal(await page.getByLabel('TLS 网站回落', { exact: true }).inputValue(), 'builtin');
    await page.getByLabel('TLS 网站回落', { exact: true }).fill('');
    await button('创建所选组合').click();
    await page.getByRole('dialog').waitFor({ state: 'hidden' });
    const batch = writes.find(w => w.path === '/api/lines/batch').body;
    assert.equal(batch.length, 4);
    assert.deepEqual(batch.map(l => [l.access.transport.network, l.access.tls.mode]), [['tcp', 'off'], ['tcp', 'auto'], ['xhttp', 'off'], ['xhttp', 'auto']]);
    for (const line of [batch[0], batch[2]]) assert.deepEqual(line.access.reality, { target: 'cover.example.com:443', server_name: 'cover.example.com' });
    assert.equal(batch[2].access.transport.path, '/batch-path');
    assert.equal(batch[1].access.fallback, '');
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
    await page.goto('http://chain.test/admin#lines'); await page.reload();
    await create('首次本机代理');
    await page.getByLabel('本机 IP 或域名').fill('192.0.2.20');
    await save();
    assert.equal(writes.filter(w => w.path === '/api/local-node').length, 1);
    assert.equal(writes.filter(w => w.path === '/api/lines').at(-1).body.hops[0].node_id, 'local');
    await create('手机布局');
    await page.setViewportSize({ width: 390, height: 844 });
    const presets = page.getByRole('group', { name: 'REALITY 目标快捷选择' });
    assert.equal(await presets.getByRole('button').count(), 8);
    assert.ok(await presets.evaluate(el => el.scrollWidth <= el.clientWidth));
    await presets.getByRole('button', { name: 'Cloudflare', exact: true }).click();
    assert.equal(await target.inputValue(), 'www.cloudflare.com:443');
    assert.equal(await sni.inputValue(), 'www.cloudflare.com');
    await page.getByLabel('本机 IP 或域名').fill('mobile.example.com');
    await page.getByText('传输安全 · REALITY', { exact: true }).click();
    await page.getByLabel('HTTPS / 证书').selectOption('auto');
    const hint = await page.getByText('域名需解析到本机，并放行 TCP 80 和监听端口。', { exact: true }).boundingBox();
    const box = await page.getByRole('dialog').boundingBox();
    assert.ok(box.x >= 0 && box.x + box.width <= 390);
    assert.ok(hint.x > box.x + 10 && hint.x + hint.width < box.x + box.width - 10);
    await close();
    assert.deepEqual(errors, []);
    console.log('PASS local defaults, REALITY presets and custom targets, domain REALITY, explicit TLS preserved, ordered chain, Mihomo import/export, orphan local identity and mobile form');
  } finally {
    await browser.close(); await server.close();
    console.log('CLEANUP browser pid=' + server.process().pid);
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
