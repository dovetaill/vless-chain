// Isolated resource onboarding UI checks; never connect to real servers.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(process.env.CHAIN_TEST_BROWSER);
const assets = path.resolve(process.argv[2]);

(async () => {
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    const state = {
      nodes: [
        { id: 'remote', name: '远程 B', host: '192.0.2.10', online: true },
        { id: 'local', name: '改过名字的本机', host: '192.0.2.20', online: true },
      ].map(node => ({ ...node, target: 'www.microsoft.com:443', server_name: 'www.microsoft.com', seen: 0,
        report: { running: false, applied: false, error: '', revision: '', traffic: {}, system: {} } })),
      local_node_id: 'local', externals: [], lines: [], checks: {}, jobs: [], busy: false, time: 0,
      subscriptions: [], security: { totp_enabled: false, recovery_remaining: 0, origin: '', passkeys: [] }, alerts: [],
    };
    const writes = [];
    await page.route('http://chain.test/**', async route => {
      const request = route.request();
      const url = new URL(request.url());
      if (url.pathname.startsWith('/api/')) {
        let body = url.pathname === '/api/lines/created/export'
          ? { node: '- ' + JSON.stringify({ name: 'test', type: state.lines[0]?.access.protocol === 'shadowsocks' ? 'ss' : 'vless', server: '192.0.2.30', port: 443 }), share: 'ss://test@192.0.2.30:443' } : state;
        if (request.method() !== 'GET') {
          writes.push({ path: url.pathname, body: request.postDataJSON() });
          if (url.pathname === '/api/nodes') {
            state.nodes.push({ ...state.nodes[0], id: 'new', name: request.postDataJSON().name, host: request.postDataJSON().host, online: false });
            body = { id: 'new', token: 'test-token' };
          }
          else if (url.pathname === '/api/nodes/new/ssh-keys') body = { keys: [{ fingerprint: 'SHA256:test', algorithm: 'ssh-ed25519' }] };
          else if (url.pathname === '/api/nodes/new/install') { state.nodes.find(node => node.id === 'new').online = true; body = {}; }
          else if (url.pathname === '/api/local-node') {
            state.local_node_id = 'local';
            state.nodes.push({ id: 'local', name: '本机', host: request.postDataJSON().host, online: true });
            body = { id: 'local' };
          }
          else if (url.pathname === '/api/lines') {
            body = { ...request.postDataJSON(), id: 'created', published: false, traffic: null };
            state.lines = [body];
          }
          else if (url.pathname === '/api/preview') body = { affected_line_ids: ['created'], affected_lines: [], message: '', nodes: [] };
          else if (url.pathname === '/api/publish') { state.lines[0].published = true; state.jobs = [{ id: 'test', state: 'applied', events: [] }]; body = { job_id: 'test' }; }
          else if (url.pathname === '/api/externals') body = { url: request.postDataJSON().url };
          else throw new Error(`Unexpected mutation: ${url.pathname}`);
        }
        return route.fulfill({ json: body });
      }
      const file = path.join(assets, url.pathname === '/' ? 'index.html' : url.pathname);
      const contentType = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml' }[path.extname(file)] || 'application/octet-stream';
      return fs.existsSync(file) ? route.fulfill({ body: fs.readFileSync(file), contentType }) : route.fulfill({ status: 404 });
    });
    const button = name => page.getByRole('button', { name, exact: true });
    const heading = name => page.getByRole('heading', { name, exact: true });
    const close = () => page.getByRole('dialog').last().getByRole('button', { name: '关闭', exact: true }).last().click();
    const create = async () => {
      await button('创建代理').first().click();
      await page.getByRole('dialog').getByRole('heading', { name: '创建代理', exact: true }).waitFor();
    };
    await page.goto('http://chain.test/#lines');
    assert.equal(await page.locator('aside nav button').count(), 4);
    for (const name of ['代理', '端口转发', '服务器', '设置'])
      assert.equal(await page.locator('aside nav').getByRole('button', { name: new RegExp('^' + name) }).count(), 1);
    await create();
    assert.equal(await page.getByLabel('入口服务器', { exact: true }).inputValue(), 'local', 'Use explicit local ID, not first remote server');
    assert.equal(writes.length, 0, 'Opening local creation must not duplicate the server');
    await page.getByLabel('名称', { exact: true }).fill('保留我的代理');
    await page.getByLabel('客户端协议', { exact: true }).selectOption('shadowsocks');
    await button('添加远程服务器').click();
    await close();
    assert.equal(await page.getByLabel('名称', { exact: true }).inputValue(), '保留我的代理', 'Cancel SSH setup preserves the draft');
    await button('添加远程服务器').click();
    await page.getByLabel('服务器名称').fill('新 B');
    await page.getByLabel('公网 IP 或域名').fill('192.0.2.30');
    await button('下一步：SSH 安装').click();
    await heading('安装并接入服务器').waitFor();
    await page.getByLabel('控制台连接地址', { exact: true }).fill('https://panel.example.com');
    await page.getByLabel('SSH 密码').fill('test-password');
    assert.equal(await button('安装并连接').isDisabled(), true, 'Host identity must be verified first');
    await button('验证服务器').click();
    await page.getByLabel('信任此服务器指纹').check();
    await button('安装并连接').click();
    await heading('安装并接入服务器').waitFor({ state: 'hidden' });
    assert.equal(await page.getByLabel('入口服务器', { exact: true }).inputValue(), 'new');
    assert.equal(await page.getByLabel('名称', { exact: true }).inputValue(), '保留我的代理');
    assert.equal(await page.getByLabel('客户端协议', { exact: true }).inputValue(), 'shadowsocks');
    await page.getByLabel('代理路径', { exact: true }).click();
    await page.getByLabel('出口', { exact: true }).selectOption('node:remote');
    await button('添加中转').click();
    assert.match(await page.getByLabel('代理路径').innerText(), /新 B（入口）.*改过名字的本机（中转）.*远程 B（出口）/);
    await page.getByRole('dialog').getByRole('button', { name: '创建', exact: true }).click();
    await heading('分享代理').waitFor();
    await page.waitForFunction(() => document.querySelector('textarea[aria-label="Mihomo 节点配置"]')?.value.startsWith('- '));
    assert.equal(JSON.parse((await page.getByLabel('Mihomo 节点配置').inputValue()).slice(2)).type, 'ss');
    await close();
    const created = writes.find(write => write.path === '/api/lines').body;
    assert.deepEqual(created.hops.map(hop => hop.node_id), ['new', 'local', 'remote']);
    assert.equal(created.access.protocol, 'shadowsocks');
    assert.equal(created.name, '保留我的代理');
    await page.getByRole('switch', { name: '停用 保留我的代理', exact: true }).click();
    await page.getByRole('switch', { name: '启用 保留我的代理', exact: true }).waitFor();
    assert.equal(writes.filter(write => write.path === '/api/lines').at(-1).body.enabled, false);
    await page.getByRole('switch', { name: '启用 保留我的代理', exact: true }).click();
    await page.getByRole('switch', { name: '停用 保留我的代理', exact: true }).waitFor();
    assert.equal(writes.filter(write => write.path === '/api/lines').at(-1).body.enabled, true);
    const writesBeforeLocal = writes.filter(write => write.path === '/api/lines').length;
    // Missing local installation must remain a reachable create flow.
    state.nodes = [];
    state.lines = [];
    state.jobs = [];
    state.local_node_id = null;
    await page.reload();
    await create();
    await page.getByLabel('名称', { exact: true }).fill('本机首次创建');
    await page.getByRole('dialog').getByRole('button', { name: '创建', exact: true }).click();
    await heading('设置本机').waitFor();
    await close();
    assert.equal(await page.getByLabel('名称', { exact: true }).inputValue(), '本机首次创建', 'Cancel setup preserves the draft');
    assert.equal(writes.filter(write => write.path === '/api/lines').length, writesBeforeLocal, 'Cancelled setup must not create a proxy');
    await page.getByRole('dialog').getByRole('button', { name: '创建', exact: true }).click();
    await page.getByLabel('本机公网 IP 或域名').fill('192.0.2.20');
    await button('启用本机').click();
    await heading('分享代理').waitFor();
    await close();
    const local = writes.filter(write => write.path === '/api/lines').at(-1).body;
    assert.equal(local.hops[0].node_id, 'local');
    assert.equal(local.name, '本机首次创建');
    assert.deepEqual(errors, []);
    console.log('PASS local identity/default/setup/cancel, inline SSH identity verification and draft preservation, ordered proxy chain');
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
