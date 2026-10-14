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
      externals: [], lines: [], checks: {}, jobs: [], busy: false, time: 0,
      subscriptions: [], security: { totp_enabled: false, recovery_remaining: 0, origin: '', passkeys: [] }, alerts: [],
    };
    const writes = [];
    await page.route('http://chain.test/**', async route => {
      const request = route.request();
      const url = new URL(request.url());
      if (url.pathname.startsWith('/api/')) {
        let body = state;
        if (request.method() !== 'GET') {
          writes.push({ path: url.pathname, body: request.postDataJSON() });
          if (url.pathname === '/api/nodes') body = { id: 'new', token: 'test-token' };
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
    const open = name => page.getByRole('button', { name: new RegExp('^' + name) }).click();
    const close = () => page.getByRole('dialog').getByRole('button', { name: '关闭', exact: true }).last().click();
    await page.goto('http://chain.test/#nodes');
    await open('使用控制台本机');
    assert.equal(await button('复制安装命令').count(), 0);
    await page.getByLabel('选择已接入的本机节点', { exact: true }).selectOption('local');
    await button('在此服务器创建代理').click();
    await heading('新建代理接入').waitFor();
    assert.equal(await page.getByLabel('入口服务器', { exact: true }).inputValue(), 'local');
    assert.equal(writes.length, 0, 'Using local server must not create another node');
    await close();
    state.nodes[1].online = false;
    await open('使用控制台本机');
    await page.getByLabel('选择已接入的本机节点', { exact: true }).selectOption('local');
    await button('在此服务器创建代理').click();
    await page.getByRole('alert').filter({ hasText: '所选服务器尚未在线' }).waitFor();
    await close();
    await open('接入另一台服务器');
    await page.getByLabel('服务器名称', { exact: true }).fill('新 B');
    await page.getByLabel('公网 IP 或域名', { exact: true }).fill('192.0.2.30');
    await button('创建并获取接入方式').click();
    await heading('安装并接入服务器').waitFor();
    assert.ok((await page.locator('pre.secret').innerText()).endsWith('sudo bash -s -- install --component agent'));
    assert.ok((await page.getByRole('dialog').innerText()).includes('192.0.2.30'));
    await button('通过 SSH 远程安装到 B').click();
    await page.getByLabel('root SSH 密码').waitFor();
    assert.equal(await button('复制安装命令').count(), 0);
    await button('在 B 的终端安装').click();
    await page.getByLabel('节点接入码', { exact: true }).waitFor();
    await close();
    await open('导入已有代理');
    await heading('导入已有代理').waitFor();
    assert.equal(await page.getByLabel('root SSH 密码').count(), 0);
    await page.getByLabel('资源名称').fill('B 上已有 VLESS');
    const url = 'vless://11111111-1111-4111-8111-111111111111@192.0.2.10:443?security=tls&type=tcp#B';
    await page.getByLabel('完整分享链接', { exact: true }).fill(url);
    await button('导入').click();
    await heading('代理已导入').waitFor();
    assert.deepEqual(writes.map(write => write.path), ['/api/nodes', '/api/externals']);
    assert.equal(writes[1].body.url, url);
    await close();
    state.nodes = [];
    await page.reload();
    await open('使用控制台本机');
    await page.getByText('还没有已接入的节点', { exact: true }).waitFor();
    assert.equal(await button('在此服务器创建代理').count(), 0);
    assert.equal(await button('复制安装命令').count(), 0);
    assert.deepEqual(errors, []);
    console.log('PASS local reuse/selection/offline/empty states, remote Agent command and SSH toggle, existing VLESS import');
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
