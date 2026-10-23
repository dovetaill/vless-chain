// Task-owned browser checks for local creation and external proxy management.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(process.env.CHAIN_TEST_BROWSER);
const assets = path.resolve(process.argv[2]);
(async () => {
  const server = await chromium.launchServer({ headless: true, executablePath: process.env.CHAIN_TEST_CHROMIUM });
  const browser = await chromium.connect(server.wsEndpoint());
  const root = process.env.CHAIN_TEST_ROOT;
  if (root) fs.writeFileSync(path.join(root, 'ui-ownership.json'), JSON.stringify({ pid: process.pid, browser: server.process().pid, artifacts: root }));
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    page.setDefaultTimeout(10000);
    const errors = [], writes = [], reads = [], members = [];
    let acceptDialog = true, failPreview = false, unverifiedPublish = false;
    let signedIn = true;
    let authOptions = { totp: false, passkey: false };
    const dialogMessages = [];
    page.on('dialog', dialog => { dialogMessages.push(dialog.message()); return acceptDialog ? dialog.accept() : dialog.dismiss(); });
    page.on('pageerror', e => errors.push(e.message));
    const node = { id: 'local', name: '本机', host: '192.0.2.20', online: true, target: 'www.cloudflare.com:443', server_name: 'www.cloudflare.com', seen: 0, report: { running: true, applied: true, error: '', revision: '', traffic: {}, system: {} } };
    const state = { nodes: [node], local_node_id: 'local', externals: [
      { id: 'a', name: '落地 A', url: 'vless://00000000-0000-4000-8000-000000000001@a.example:443?security=tls' },
      { id: 'b', name: '落地 B', url: 'socks5://user:password@b.example:1080' },
    ], lines: [], checks: {}, jobs: [], busy: false, time: 0, subscriptions: [], security: { totp_enabled: false, recovery_remaining: 0, origin: '', passkeys: [] }, alerts: [], customers: [], payments: [], renewal_contact: '' };
    await page.route('**/*', async route => {
      try {
      const request = route.request(), url = new URL(request.url());
      if (url.hostname !== 'chain.test') return route.abort();
      if (url.pathname.startsWith('/api/')) {
        if (request.method() === 'GET') reads.push(url.pathname);
        if (url.pathname === '/api/state' && !signedIn) return route.fulfill({ status: 401, json: { error: '请登录' } });
        if (url.pathname === '/api/diagnostics/local/logs' && request.method() === 'POST') {
          assert.deepEqual(Object.keys(request.postDataJSON()).sort(), ['archive', 'line_id', 'source']);
          reads.push(url.pathname);
          return route.fulfill({ json: { text: '', sampled_at: 1, sources: ['panel', 'xray'], detail: '', clock: { detail: '模拟时钟已同步' } } });
        }
        let body = state;
        if (url.pathname === '/api/auth/options') body = authOptions;
        if (url.pathname === '/api/security/members') body = { members };
        if (url.pathname === '/api/fleet/release/latest') body = { version: '0.2.53', xray: 'test', mihomo: 'test' };
        if (url.pathname.endsWith('/export')) body = { node: '- ' + JSON.stringify({ name: '落地', type: 'vless', server: 'a.example', port: 443 }), share: state.externals[0].url };
        if (request.method() !== 'GET') {
          const data = request.postDataJSON(); writes.push({ path: url.pathname, body: data });
          if (url.pathname === '/api/logout') { signedIn = false; body = { ok: true }; }
          else if (url.pathname === '/api/login') { signedIn = true; state.viewer = { id: 'member-viewer', name: '只读成员', role: 'viewer' }; body = { ok: true }; }
          else if (url.pathname === '/api/security/members') { const member = { id: 'member-finance', name: data.name, role: data.role, created_at: 1, revoked_at: 0 }; members.push(member); body = { member, token: 'mock-once-only-member-key' }; }
          else if (url.pathname === '/api/security/members/member-finance/revoke') { members[0].revoked_at = 2; body = { ok: true }; }
          else if (url.pathname === '/api/local-node') { state.local_node_id = 'local'; state.nodes = [{ ...node, host: data.host }]; body = { id: 'local' }; }
          else if (url.pathname === '/api/lines') { body = { ...data, id: data.id || String(state.lines.length + 1), published: false, plan_eligible: true, traffic: null, rental: null, rental_status: 'draft', sync_status: 'unpublished' }; state.lines = [...state.lines.filter(l => l.id !== body.id), body]; }
          else if (url.pathname === '/api/lines/batch') { body = data.map((line, index) => ({ ...line, id: String(state.lines.length + index + 1), published: false, traffic: null, rental: null, rental_status: 'draft', sync_status: 'unpublished' })); state.lines.push(...body); }
          else if (url.pathname === '/api/diagnostics/local/reality') body = { ok: true, detail: 'TLS 1.3、证书/SNI 与 H2 检查通过', latency_ms: 12 };
          else if (url.pathname === '/api/preview') {
            if (failPreview) { failPreview = false; return route.fulfill({ status: 503, json: { error: '模拟预览失败' } }); }
            body = { affected_line_ids: [], affected_lines: [], message: '', nodes: [] };
          }
          else if (url.pathname === '/api/publish') {
            if (unverifiedPublish) return route.fulfill({ status: 409, json: { error: 'REALITY_UNVERIFIED：模拟检测未完成' } });
            state.lines.forEach(l => { l.published = true; l.rental_status = 'active'; l.sync_status = 'synced'; }); state.jobs = [{ id: 'job', state: 'applied', events: [] }]; body = { job_id: 'job' };
          }
          else if (url.pathname === '/api/subscriptions') { state.subscriptions.push({ id: 'subscription', ...data, created: 1 }); body = { path: '/subscription/test-private-token' }; }
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
    await page.locator('aside nav').getByRole('button', { name: '代理', exact: true }).waitFor();
    assert.equal(await page.locator('aside nav button').count(), 9);
    for (const name of ['总览', '代理', '客户与收款', '外部代理', '资源额度', '网站规则', '多机总控', '监控告警', '设置']) assert.equal(await page.locator('aside nav').getByRole('button', { name, exact: true }).count(), 1);
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
    await button('检测 REALITY 握手与回落风险').click();
    const checked = page.getByText(/握手通过：TLS 1.3、证书\/SNI 与 H2 检查通过（12 ms）/);
    await checked.waitFor();
    assert.deepEqual(writes.find(w => w.path === '/api/diagnostics/local/reality').body, { target: 'www.cloudflare.com:443', server_name: 'www.cloudflare.com' });
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
    await button('检测 REALITY 握手与回落风险').click();
    await page.getByText(/握手通过：TLS 1.3、证书\/SNI 与 H2 检查通过（12 ms）/).waitFor();
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
    await page.getByLabel('分享链接').waitFor();
    assert.equal(await button('订阅链接').count(), 0, '外部代理不显示本地订阅创建');
    await button('Mihomo 节点').click();
    await page.getByLabel('Mihomo 节点配置').waitFor();
    await page.getByText(/这是单节点片段，不是完整客户端配置/).waitFor();
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

    const beforeDraftWrites = writes.length;
    await create('套餐候选草稿');
    await button('仅保存草稿').click();
    await page.getByText('草稿已保存，可在客户与收款中绑定共享套餐，或在代理列表继续应用。', { exact: true }).waitFor();
    assert.deepEqual(writes.slice(beforeDraftWrites).map(write => write.path), ['/api/lines'], '仅保存草稿不预览、不发布');
    assert.equal(state.lines.at(-1).published, false);
    assert.equal(state.lines.at(-1).plan_eligible, true);

    // A successful save stays saved if the following preview or publish fails.
    for (const failure of ['cancel-publish', 'preview']) {
      const before = writes.filter(w => w.path === '/api/lines').length;
      const count = state.lines.length;
      await create(failure);
      acceptDialog = failure !== 'cancel-publish';
      unverifiedPublish = failure === 'cancel-publish';
      failPreview = failure === 'preview';
      await page.getByRole('dialog').getByRole('button', { name: '创建代理', exact: true }).click();
      await page.getByText(/配置已保存，尚未应用：.*无需重新创建/).waitFor();
      assert.equal(await page.getByRole('dialog').count(), 0);
      assert.equal(state.lines.length, count + 1);
      assert.equal(state.lines.at(-1).published, false);
      if (unverifiedPublish) assert.match(dialogMessages.at(-1), /REALITY_UNVERIFIED/);
      acceptDialog = true; unverifiedPublish = false;
      await Promise.all([page.waitForResponse(response => response.url().endsWith('/api/publish')), button('应用待处理配置修改').click()]);
      await button('应用待处理配置修改').waitFor({ state: 'hidden' });
      assert.equal(writes.filter(w => w.path === '/api/lines').length, before + 1, '继续应用不得再创建代理');
    }

    // Button-only edits are unsaved changes too, even without a text-input event.
    await button('新建代理').first().click();
    await button('苹果').click();
    acceptDialog = false;
    await page.keyboard.press('Escape');
    assert.match(dialogMessages.at(-1), /放弃尚未保存/);
    assert.equal(await target.inputValue(), 'www.apple.com:443');
    acceptDialog = true;
    await close();
    await page.getByRole('dialog').waitFor({ state: 'hidden' });

    await button('首次本机代理 更多操作').click();
    await page.getByRole('menuitem', { name: '分享配置', exact: true }).click();
    await page.getByLabel('分享链接').waitFor();
    await button('订阅链接').click();
    await page.getByLabel('订阅名称', { exact: true }).fill('客户甲的订阅');
    await button('创建订阅链接').click();
    assert.equal(await page.getByLabel('订阅地址', { exact: true }).inputValue(), 'http://chain.test/subscription/test-private-token');
    assert.deepEqual(writes.find(w => w.path === '/api/subscriptions').body, { name: '客户甲的订阅', line_ids: [state.lines[0].id] });
    const confirms = dialogMessages.length;
    await close();
    assert.equal(dialogMessages.length, confirms, '创建成功后的订阅窗口不再提示放弃修改');

    const existingData = JSON.stringify(state);
    const writesBeforeModes = writes.length;
    const mode = page.locator('aside').getByLabel('使用模式', { exact: true });
    await mode.selectOption('personal');
    assert.equal(await page.locator('aside nav > button').first().getAttribute('aria-label'), '代理');
    assert.equal(await page.locator('aside nav').getByRole('button', { name: '客户与收款', exact: true }).count(), 0);
    await page.evaluate(() => { location.hash = 'customers'; });
    await page.getByRole('heading', { name: '客户与收款', exact: true }).waitFor();
    assert.equal(await page.locator('aside nav').getByRole('button', { name: '客户与收款', exact: true }).count(), 1, '自用模式旧hash仍可访问客户与收款');
    await page.reload();
    assert.equal(await mode.inputValue(), 'personal', '模式是本浏览器偏好');
    await mode.selectOption('business');
    assert.equal(await page.locator('aside nav > button').nth(1).getAttribute('aria-label'), '客户与收款');
    await mode.selectOption('small');
    await page.locator('aside nav').getByRole('button', { name: '代理', exact: true }).click();
    assert.equal(writes.length, writesBeforeModes, '模式切换不发业务写请求');
    assert.equal(JSON.stringify(state), existingData, '模式切换不改已有服务或经营数据');

    state.lines[0].customer_plan_id = 'plan-demo';
    await page.reload();
    const planRow = page.getByRole('region', { name: '代理列表', exact: true }).locator('tbody tr').filter({ hasText: '首次本机代理' });
    await planRow.getByRole('button', { name: '前往客户套餐', exact: true }).waitFor();
    assert.equal(await planRow.getByRole('button', { name: '出租信息', exact: true }).count(), 0);
    assert.equal(await planRow.getByRole('button', { name: '收款 / 续费', exact: true }).count(), 0);
    await button('首次本机代理 更多操作').click();
    assert.equal(await page.getByRole('menuitem', { name: '设为无限期（自用）', exact: true }).count(), 0);
    assert.equal(await page.getByRole('menuitem', { name: '删除代理', exact: true }).count(), 0);
    assert.equal(await page.getByRole('menuitem', { name: '客户私密查询链接', exact: true }).count(), 0, '套餐查询使用客户套餐入口，不调用单线租赁API');
    assert.equal(await page.getByRole('menuitem', { name: '编辑代理配置', exact: true }).count(), 1);
    await page.getByRole('menuitem', { name: '日志与诊断', exact: true }).click();
    await page.getByText('所选节点上报：模拟时钟已同步', { exact: true }).waitFor();
    assert.equal(await button('启用本 VPS 自动校时').count(), 1, '原管理员仍可自动校时');
    await close();
    await page.getByRole('dialog').waitFor({ state: 'hidden' });
    await page.getByLabel('选择 首次本机代理', { exact: true }).check();
    const batchActions = page.getByLabel('批量操作', { exact: true });
    assert.equal(await batchActions.getByRole('button', { name: '收款 / 续费', exact: true }).count(), 0);
    assert.equal(await batchActions.getByRole('button', { name: '设为无限期（自用）', exact: true }).count(), 0);
    assert.equal(await batchActions.getByRole('button', { name: '删除', exact: true }).count(), 0);
    await batchActions.getByText('包含共享套餐线路，请到客户与收款按套餐收款。', { exact: true }).waitFor();
    await button('取消选择').click();
    await button('套餐候选草稿 更多操作').click();
    assert.equal(await page.getByRole('menuitem', { name: '删除代理', exact: true }).count(), 1, '独立线路仍可删除');
    assert.equal(await page.getByRole('menuitem', { name: '客户私密查询链接', exact: true }).count(), 1, '独立线路保留原查询入口');
    await page.keyboard.press('Escape');

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
    await page.getByRole('dialog').waitFor({ state: 'hidden' });
    const table = page.getByRole('region', { name: '代理列表', exact: true });
    assert.ok(await table.evaluate(el => el.scrollWidth <= el.clientWidth), '手机代理卡片无需横向滚动');
    const actions = await button('首次本机代理 更多操作').boundingBox();
    assert.ok(actions.x >= 0 && actions.x + actions.width <= 390);
    assert.equal(await page.locator('aside').isVisible(), false, '关闭导航后侧栏不在视觉和键盘导航中');
    const navToggle = button('打开导航');
    await navToggle.click();
    const navigation = page.getByRole('dialog', { name: '管理导航', exact: true });
    await navigation.waitFor();
    await navigation.getByLabel('使用模式', { exact: true }).selectOption('personal');
    assert.equal(await navigation.locator('nav > button').first().getAttribute('aria-label'), '代理');
    await navigation.getByLabel('使用模式', { exact: true }).selectOption('small');
    for (let i = 0; i < 14; i++) {
      await page.keyboard.press('Tab');
      assert.ok(await navigation.evaluate(el => el.contains(document.activeElement)), '焦点不能穿出手机导航');
    }
    await page.keyboard.press('Escape');
    await navigation.waitFor({ state: 'hidden' });
    assert.ok(await navToggle.evaluate(el => el === document.activeElement), 'Esc 后焦点回到打开导航按钮');
    await navToggle.click();
    await navigation.getByRole('button', { name: '外部代理', exact: true }).click();
    await navigation.waitFor({ state: 'hidden' });
    await page.getByRole('heading', { name: '外部代理', exact: true }).first().waitFor();

    // Roles must work with credential-free line data and reject old page hashes.
    await page.setViewportSize({ width: 1280, height: 900 });
    const adminState = structuredClone(state);
    const beforeRoleWrites = writes.length;
    for (const [role, label, expectedNav] of [
      ['finance', '财务', ['总览', '代理', '客户与收款']],
      ['ops', '运维', ['总览', '代理', '监控告警', '设置']],
      ['viewer', '只读', ['总览', '代理']],
    ]) {
      state.viewer = { id: `member-${role}`, name: `${label}成员`, role };
      state.lines = adminState.lines.map(line => ({ ...line, access: { protocol: line.access.protocol }, hops: line.hops.map(hop => ({ node_id: hop.node_id, port: hop.port })), rental: role === 'finance' ? line.rental : null, share: null, chain: [] }));
      await page.goto('http://chain.test/admin#lines'); await page.reload();
      await page.getByRole('region', { name: '代理列表', exact: true }).waitFor();
      assert.match(await page.getByLabel('当前身份', { exact: true }).innerText(), new RegExp(label));
      assert.deepEqual(await page.locator('aside nav button').evaluateAll(nodes => nodes.map(node => node.getAttribute('aria-label'))), expectedNav);
      assert.equal(await button('新建代理').count(), 0);
      assert.equal(await button('出租信息').count(), role === 'finance' ? state.lines.filter(line => !line.customer_plan_id).length : 0);
      if (role === 'viewer') assert.equal(await button('首次本机代理 更多操作').count(), 0);
      else {
        await button('首次本机代理 更多操作').click();
        for (const forbidden of ['分享配置', '编辑代理配置', '调整额度 / 限速', '手动停用', '删除代理', '客户私密查询链接']) assert.equal(await page.getByRole('menuitem', { name: forbidden, exact: true }).count(), 0);
        assert.equal(await page.getByRole('menuitem', { name: '检查连接', exact: true }).count(), role === 'ops' ? 1 : 0);
        if (role === 'ops') {
          await page.getByRole('menuitem', { name: '日志与诊断', exact: true }).click();
          await page.getByText('所选节点上报：模拟时钟已同步', { exact: true }).waitFor();
          assert.equal(await button('启用本 VPS 自动校时').count(), 0, '运维可查看时钟状态，但不显示无权执行的自动校时');
          await close();
          await page.getByRole('dialog').waitFor({ state: 'hidden' });
        } else await page.keyboard.press('Escape');
      }
      await page.getByLabel('搜索代理名称、客户、服务器、地址或端口').fill('首次');
      await page.getByText('首次本机代理', { exact: true }).waitFor();
      const beforeForbiddenReads = reads.length;
      await page.evaluate(() => { location.hash = 'nodes'; });
      await page.getByText('当前身份无权访问此页面。', { exact: false }).waitFor();
      await page.evaluate(() => { location.hash = 'settings'; });
      if (role !== 'ops') await page.getByText('当前身份无权访问此页面。', { exact: false }).waitFor();
      await page.evaluate(() => { location.hash = 'dashboard'; });
      await page.getByRole('heading', { name: '总览', exact: true }).waitFor();
      assert.equal(await button('首次本机代理').count(), role === 'finance' ? 1 : 0);
      assert.equal(reads.slice(beforeForbiddenReads).some(path => path === '/api/security/members' || path.endsWith('/export')), false);
    }
    assert.equal(writes.length, beforeRoleWrites, '只读/权限检查不发出业务写请求');

    Object.assign(state, adminState); delete state.viewer;
    await page.goto('http://chain.test/admin#settings'); await page.reload();
    await button('新建成员').waitFor();
    assert.equal(await page.locator('aside nav button').count(), 9, '旧管理员state缺viewer字段时保持原权限');
    await button('新建成员').click();
    await page.getByLabel('成员名称', { exact: true }).fill('财务甲');
    await page.getByLabel('成员角色', { exact: true }).selectOption('finance');
    await page.getByLabel('当前管理员密钥', { exact: true }).fill('mock-admin-proof');
    await button('生成成员密钥').click();
    await page.getByText('mock-once-only-member-key', { exact: true }).waitFor();
    assert.deepEqual(writes.find(write => write.path === '/api/security/members').body, { name: '财务甲', role: 'finance', token: 'mock-admin-proof', code: '' });
    await close();
    assert.equal(await page.getByText('mock-once-only-member-key', { exact: true }).count(), 0, '密钥不留在成员列表中');
    await button('撤销成员').click();
    await page.getByLabel('当前管理员密钥', { exact: true }).fill('mock-admin-proof');
    await button('确认撤销成员').click();
    await page.getByRole('dialog').waitFor({ state: 'hidden' });
    assert.equal(await button('撤销成员').count(), 0);
    assert.equal(members[0].revoked_at, 2);

    authOptions = { totp: true, passkey: false };
    await button('退出登录').click();
    await page.getByLabel('管理员密钥', { exact: true }).fill('mock-member-key');
    await page.getByLabel('动态验证码或恢复码', { exact: true }).waitFor();
    await button('登录').click();
    await page.getByLabel('当前身份', { exact: true }).waitFor();
    assert.equal(writes.find(write => write.path === '/api/login').body.code, '', '成员登录不要求管理员的二步验证码');
    assert.deepEqual(errors, []);
    console.log('PASS local creation recovery, unsaved edits, subscription delivery, mobile navigation, workspace presets, redacted role views, member key creation/revocation and member login');
  } finally {
    await browser.close(); await server.close();
    console.log('CLEANUP browser pid=' + server.process().pid);
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
