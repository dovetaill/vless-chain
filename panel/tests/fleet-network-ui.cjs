// All API and asset requests are intercepted; no real VPS or service is contacted.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(process.env.CHAIN_TEST_BROWSER);
const assets = path.resolve(process.argv[2]);
const screenshots = process.env.CHAIN_TEST_SCREENSHOTS;
(async () => {
  const server = await chromium.launchServer({ headless: true, executablePath: process.env.CHAIN_TEST_CHROMIUM });
  console.log(`BROWSER pid=${server.process().pid} endpoint=${server.wsEndpoint()}`);
  const browser = await chromium.connect(server.wsEndpoint());
  try {
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, locale: 'zh-CN' });
    page.setDefaultTimeout(10000);
    page.on('dialog', dialog => dialog.accept());
    const errors = [], requests = [];
    page.on('pageerror', error => errors.push(error.message));
    const now = Math.floor(Date.now() / 1000);
    const state = { viewer: { id: 'administrator', name: '模拟管理员', role: 'admin' }, version: '0.2.60', agent_version: '0.2.60', maintenance: true, maintenance_actions: ['network-tune'], nodes: [], lines: [], externals: [], checks: {}, jobs: [], busy: false, time: now, subscriptions: [], security: { totp_enabled: false, passkeys: [] }, alerts: [], customers: [], customer_plans: [], payments: [], renewal_contact: '', pending_imports: [] };
    const network = { congestion_control: 'cubic', qdisc: 'fq_codel', kernel: 'fixture', algorithms: ['bbr', 'cubic'], queues: ['fq'], interfaces: [{ dev: 'eth0', kind: 'fq_codel', parent: 'root' }], warning: '' };
    let localJob;
    const line = { id: 'same-line-id', name: '代理', kind: 'vless', hops: [{ node_id: 'local', port: 1443 }], enabled: true, published: true, chain: [], target: '',
      access: { host: '192.0.2.1', protocol: 'vless', transport: { network: 'tcp' }, tls: { mode: 'off' } }, limits: { monthly_gib: 100 }, rental_status: 'active', sync_status: 'synced' };
    const fleet = { controller: '', policy: null, tasks: {}, remotes: ['a', 'b'].map(id => ({ id, name: `VPS ${id}`, url: `https://${id}.example`, seen: now, error: '', snapshot: { ...state, version: '1.2.3', agent_version: '1.2.3', maintenance: true, maintenance_actions: ['network-tune'], local_node_id: 'local', lines: [structuredClone(line)],
      nodes: [{ id: 'local', name: '本机', host: '192.0.2.1', online: true, seen: now, report: { running: true, applied: true, error: '', revision: '', system: { sampled_at: now } } }] } })) };
    let rejectB = false;
    await page.route('**/*', async route => {
      const req = route.request(), url = new URL(req.url());
      if (url.hostname !== 'chain.test') return route.abort();
      if (!url.pathname.startsWith('/api/')) {
        const file = path.join(assets, url.pathname === '/admin' ? 'index.html' : url.pathname);
        return route.fulfill({ body: fs.readFileSync(file), contentType: { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css' }[path.extname(file)] || 'application/octet-stream' });
      }
      const body = req.method() === 'GET' ? null : req.postDataJSON();
      requests.push({ path: url.pathname, body });
      if (state.viewer.role !== 'admin' && (url.pathname === '/api/fleet' || url.pathname === '/api/fleet/batch'
          || (url.pathname === '/api/federation/task' && body?.action?.startsWith('network-')))) {
        return route.fulfill({ status: 403, json: { error: '运维成员仅能执行升级或重启' } });
      }
      let result;
      if (url.pathname === '/api/state') result = state;
      else if (url.pathname === '/api/security/members') result = { members: [] };
      else if (url.pathname.startsWith('/api/fleet/cores/')) result = { recommended: '1.2.3', versions: [{ version: '1.2.3', prerelease: false }] };
      else if (url.pathname === '/api/federation/task') {
        if (body?.action === 'network-status') result = network;
        else if (body?.action === 'network-tune') {
          localJob = { ...body, state: 'succeeded', network: { after: { ...network, congestion_control: 'bbr', warning: 'eth0 保留自定义队列' } }, detail: '无需重启服务器或服务' };
          return route.fulfill({ status: 502, json: { error: '模拟提交响应丢失' } });
        } else {
          assert.equal(req.method(), 'GET');
          assert.equal(url.searchParams.get('id'), localJob.id);
          result = localJob;
        }
      }
      else if (url.pathname === '/api/fleet') result = fleet;
      else if (url.pathname === '/api/fleet/release/latest') result = { version: '1.2.3' };
      else if (url.pathname === '/api/auth/options') result = { totp: false, passkey: false };
      else if (url.pathname === '/api/fleet/batch') {
        fleet.tasks[body.id] = { ...body, at: now, state: 'completed', targets: body.targets.map(id => id === 'local'
          ? { id, state: 'failed', result: { error: '模拟本机设置失败', maintenance_state: 'failed', verification: { state: 'pending' } } }
          : { id, state: 'succeeded' }) };
        result = { id: body.id };
      } else if (/^\/api\/fleet\/[ab]\/forward$/.test(url.pathname)) {
        const remote = fleet.remotes.find(r => url.pathname === `/api/fleet/${r.id}/forward`);
        if (body.path === '/state') result = remote.snapshot;
        else if (body.path === '/federation/task') {
          assert.equal(body.body.action, 'network-status');
          result = { congestion_control: 'cubic', qdisc: 'fq_codel', kernel: 'fixture', algorithms: ['bbr', 'cubic'], queues: remote.id === 'a' ? ['fq', 'cake'] : ['fq'], interfaces: [{ dev: 'eth0', kind: 'fq_codel', parent: 'root' }], warning: '' };
        } else if (body.path === '/lines/operate') {
          assert.deepEqual(body.body.line_ids, ['same-line-id']);
          if (rejectB && remote.id === 'b') return route.fulfill({ status: 400, json: { error: '模拟 VPS b 失败' } });
          result = { ok: true };
        } else if (body.path.endsWith('/export')) result = { share: `vless://${remote.id}`, node: `- name: ${remote.id}` };
        else if (body.path.startsWith('/access/')) result = { restricted: false, sources: [], domains: [], denied_ports: [], udp_ports: [], udp: 'allow' };
        else if (body.path === '/rules') result = [];
        else if (body.path.endsWith('/probe')) result = { ok: true, detail: 'fixture probe', latency_ms: 1 };
        else if (body.path.endsWith('/logs')) result = { text: 'fixture log', sources: ['xray'], sampled_at: now, detail: '' };
        else if (body.path === '/preview') result = { message: 'fixture preview', affected_lines: ['代理'] };
        else if (body.path === '/publish') result = { ok: true };
        else throw Error(`Unexpected remote request: ${body.path}`);
      } else throw Error(`Unexpected local request: ${url.pathname}`);
      return route.fulfill({ json: result });
    });
    const button = name => page.getByRole('button', { name, exact: true });
    const dialog = () => page.getByRole('dialog');
    const close = async () => { await dialog().getByRole('button', { name: '关闭', exact: true }).first().click(); await dialog().waitFor({ state: 'hidden' }); };
    const go = name => page.getByRole('navigation', { name: '总控功能' }).getByRole('button', { name, exact: true }).click();
    const action = async name => { await button('VPS b / 代理 更多操作').click(); await page.getByRole('menuitem', { name, exact: true }).click(); };
    const capture = async name => {
      if (!screenshots) return;
      await page.evaluate(() => document.fonts.ready);
      await page.screenshot({ path: path.join(screenshots, `${name}.png`), animations: 'disabled' });
    };
    const fits = async locator => {
      const bounds = await locator.boundingBox(), viewport = page.viewportSize();
      assert(bounds && bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= viewport.width + 1 && bounds.y + bounds.height <= viewport.height + 1);
      assert(await locator.evaluate(e => e.scrollWidth <= e.clientWidth + 1), 'Horizontal overflow');
    };
    await page.goto('http://chain.test/admin#settings');
    await button('网络算法 / BBR').click();
    await button('确认保存并应用').waitFor();
    await capture('01-admin-local');
    await fits(dialog());
    await button('确认保存并应用').click();
    await page.getByText('eth0 保留自定义队列', { exact: true }).waitFor();
    assert.equal(requests.filter(r => r.path === '/api/federation/task' && r.body?.action === 'network-tune').length, 1);
    assert.equal(localJob.congestion_control, 'bbr'); assert.equal(localJob.qdisc, 'fq');
    await page.goto('http://chain.test/admin#fleet');
    await go('所有代理');
    await action('调整额度 / 限速');
    await page.getByLabel('月额度 · GiB', { exact: true }).fill('200');
    await button('确认调整并下发').click(); await dialog().waitFor({ state: 'hidden' });
    assert(requests.some(r => r.path === '/api/fleet/b/forward' && r.body.path === '/lines/operate' && r.body.body.limits.monthly_gib === 200));
    await action('分享配置'); await page.getByLabel('分享链接').waitFor();
    assert.equal(await page.getByLabel('分享链接').inputValue(), 'vless://b');
    assert.equal(await button('订阅链接').count(), 0); await close();
    await action('网站 / 端口 / 连接限制'); await page.getByLabel('禁止目标端口（逗号或空格分隔）').fill('25');
    await button('保存并下发').click(); await dialog().waitFor({ state: 'hidden' });
    assert(requests.some(r => r.path === '/api/fleet/b/forward' && r.body.path === '/access/same-line-id' && r.body.body?.denied_ports[0] === 25));
    await action('检查连接'); await page.getByLabel('检查方式').selectOption('tcp'); await button('开始检查').click();
    await page.getByText('fixture probe', { exact: true }).waitFor(); await close();
    assert(requests.some(r => r.path === '/api/fleet/b/forward' && r.body.path.endsWith('/probe') && r.body.body.mode === 'tcp'));
    await action('日志与诊断'); await page.getByText('fixture log', { exact: true }).waitFor(); await close();
    await action('应用配置修改'); await button('确认并应用').click(); await dialog().waitFor({ state: 'hidden' });
    assert(requests.some(r => r.path === '/api/fleet/b/forward' && r.body.path === '/publish'));
    for (const id of ['a', 'b']) await page.getByLabel(`选择代理 VPS ${id} / 代理`, { exact: true }).check();
    await button('导出').click(); await button('读取已应用配置').click(); await page.getByText('已读取 2 条；未导出 0 条。', { exact: true }).waitFor(); await close();
    rejectB = true;
    await button('额度 / 限速').click(); await page.getByLabel('月额度 · GiB', { exact: true }).fill('300'); await button('确认执行').click();
    await page.getByText('VPS b：模拟 VPS b 失败', { exact: true }).waitFor();
    assert.equal(await dialog().getByRole('button', { name: '确认执行', exact: true }).count(), 0); await close();
    await go('VPS');
    assert.equal(await button('网络算法 / BBR').isDisabled(), true);
    await button('本机网络算法 / BBR').click();
    await button('确认保存并应用').waitFor(); await close();
    for (const id of ['a', 'b']) await page.getByLabel(`选择 VPS VPS ${id}`, { exact: true }).check();
    await page.getByLabel('网络批量设置包含本机', { exact: true }).check();
    await capture('02-admin-batch-entry');
    await button('网络算法 / BBR').click(); await button('确认保存并应用').waitFor();
    await dialog().getByText('本机（主控）', { exact: true }).waitFor();
    assert.deepEqual(await page.getByLabel('默认队列', { exact: true }).locator('option').allTextContents(), ['fq']);
    await capture('03-admin-batch-dialog');
    await button('确认保存并应用').click();
    await dialog().getByRole('heading', { name: '批量任务进度' }).waitFor();
    const localResult = page.getByRole('region', { name: 'VPS 任务：本机（主控）' });
    await localResult.getByText('模拟本机设置失败', { exact: true }).waitFor();
    assert.equal(await localResult.getByText('维护完成 · 等待连接验证', { exact: true }).count(), 0);
    await localResult.scrollIntoViewIfNeeded();
    await capture('04-batch-partial-failure');
    const payload = requests.find(r => r.path === '/api/fleet/batch').body;
    assert.deepEqual(payload.targets, ['a', 'b', 'local']); assert.equal(payload.action, 'network-tune');
    assert.equal(payload.congestion_control, 'bbr'); assert.equal(payload.qdisc, 'fq');
    assert(!requests.some(r => /^\/api\/(lines|access|rules|diagnostics|subscriptions)/.test(r.path)));
    await close();
    await page.setViewportSize({ width: 390, height: 844 });
    await go('VPS');
    await button('网络算法 / BBR').scrollIntoViewIfNeeded();
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Mobile page overflow');
    await capture('05-mobile-batch-entry');
    await button('网络算法 / BBR').click();
    await button('确认保存并应用').waitFor();
    await fits(dialog());
    await page.getByLabel('拥塞控制算法', { exact: true }).selectOption('cubic');
    await page.getByLabel('拥塞控制算法', { exact: true }).selectOption('bbr');
    await button('确认保存并应用').scrollIntoViewIfNeeded();
    await capture('06-mobile-batch-dialog');
    await button('确认保存并应用').click();
    await dialog().getByRole('heading', { name: '批量任务进度' }).waitFor();
    assert.equal(requests.filter(r => r.path === '/api/fleet/batch').length, 2);
    await close();
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.reload(); await go('维护任务');
    await page.locator('.fleet-task-row').first().getByRole('button', { name: '查看任务', exact: true }).click();
    await dialog().getByRole('heading', { name: '批量任务进度' }).waitFor();
    await button('选择失败及未执行对象').click();
    assert.equal(await page.getByLabel('网络批量设置包含本机', { exact: true }).isChecked(), true);
    for (const id of ['a', 'b']) assert.equal(await page.getByLabel(`选择 VPS VPS ${id}`, { exact: true }).isChecked(), false);
    await button('网络算法 / BBR').click();
    await dialog().getByRole('heading', { name: '网络算法 · 1 台 VPS', exact: true }).waitFor();
    await button('确认保存并应用').click();
    await dialog().getByRole('heading', { name: '批量任务进度' }).waitFor();
    assert.deepEqual(requests.filter(r => r.path === '/api/fleet/batch').at(-1).body.targets, ['local']);
    await close();
    await page.evaluate(() => localStorage.removeItem('chain-maintenance-task'));
    for (const role of ['ops', 'finance', 'viewer']) {
      state.viewer = { id: role, name: { ops: '模拟运维成员', finance: '模拟财务成员', viewer: '模拟只读成员' }[role], role };
      const start = requests.length;
      await page.goto('http://chain.test/admin#settings');
      await page.reload();
      await page.getByLabel('当前身份', { exact: true }).filter({ hasText: state.viewer.name }).waitFor();
      if (role === 'ops') {
        assert.equal(await button('网络算法 / BBR').isDisabled(), true);
        await page.getByText('网络算法设置仅管理员可操作。', { exact: true }).waitFor();
        await button('网络算法 / BBR').scrollIntoViewIfNeeded();
        await capture('07-ops-permissions');
      } else {
        await page.getByRole('alert').filter({ hasText: '当前身份无权访问此页面' }).waitFor();
        assert.equal(await button('网络算法 / BBR').count(), 0);
        await capture(`08-${role}-permissions`);
      }
      await page.goto('http://chain.test/admin#fleet');
      await page.getByLabel('当前身份', { exact: true }).waitFor();
      await page.getByRole('alert').filter({ hasText: '当前身份无权访问此页面' }).waitFor();
      assert.equal(await button('本机网络算法 / BBR').count(), 0);
      assert(!requests.slice(start).some(r => r.path === '/api/fleet' || r.body?.action?.startsWith('network-')));
    }
    state.viewer = { id: 'administrator', name: '模拟管理员', role: 'admin' };
    fleet.remotes[1].snapshot.maintenance_actions = [];
    await page.goto('http://chain.test/admin#fleet'); await page.reload(); await go('VPS');
    await page.getByLabel('选择 VPS VPS b', { exact: true }).check();
    await button('网络算法 / BBR').click();
    await dialog().getByText('请先升级该 VPS 的面板及维护组件', { exact: true }).waitFor();
    assert.equal(await button('确认保存并应用').count(), 0);
    await capture('09-unsupported-vps');
    assert.deepEqual(errors, []);
    assert(!requests.some(r => r.body?.action?.startsWith('restart')));
    console.log('PASS admin/ops/finance/viewer, desktop/mobile, local lost response, queue warning, mixed batch, partial failure, no restarts');
  } finally {
    await browser.close();
    await server.close();
    assert(server.process().exitCode !== null || server.process().signalCode !== null);
    console.log('CLEANUP browser stopped');
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
