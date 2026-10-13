// Runs against the isolated controller created by smoke.py, never a live deployment.
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const { chromium } = require(process.env.CHAIN_TEST_BROWSER);
const [base, tokenFile, artifacts] = process.argv.slice(2);

(async () => {
  const record = { cwd: process.cwd(), pid: process.pid, artifactDirectory: artifacts, browserPid: null, websocket: null };
  const recordFile = path.join(artifacts, 'browser-ownership.json');
  fs.writeFileSync(recordFile, JSON.stringify(record));
  const server = await chromium.launchServer({ headless: true });
  record.browserPid = server.process().pid;
  record.websocket = server.wsEndpoint();
  fs.writeFileSync(recordFile, JSON.stringify(record));
  let browser;
  try {
    browser = await chromium.connect(server.wsEndpoint());
    const context = await browser.newContext({ viewport: { width: 1440, height: 1050 } });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));
    page.on('console', message => {
      if (message.type() === 'error' && !message.text().includes('401') && !message.text().includes('favicon')) errors.push(message.text());
    });
    const close = () => page.getByRole('dialog').getByRole('button', { name: '关闭', exact: true }).last().click();
    await page.goto(base);
    await page.getByLabel('管理令牌').fill('incorrect');
    await page.getByRole('button', { name: '进入控制台' }).click();
    await page.getByRole('alert').filter({ hasText: '管理令牌不正确' }).waitFor();
    await page.getByLabel('管理令牌').fill(fs.readFileSync(tokenFile, 'utf8').trim());
    await page.getByRole('button', { name: '进入控制台' }).click();
    await page.getByRole('heading', { name: '代理线路', exact: true }).waitFor();
    await page.getByText('三跳代理', { exact: true }).waitFor();
    await page.screenshot({ path: path.join(artifacts, 'desktop.png'), fullPage: true });

    await page.getByRole('button', { name: '新建线路', exact: true }).click();
    await page.getByLabel('线路名称').fill('浏览器测试线路');
    await page.getByRole('button', { name: '添加下一跳' }).click();
    await page.getByRole('button', { name: '添加下一跳' }).click();
    const second = await page.getByLabel('第 2 跳服务器').inputValue();
    await page.getByRole('button', { name: '上移第 3 跳' }).click();
    assert.equal(await page.getByLabel('第 3 跳服务器').inputValue(), second);
    await page.getByRole('button', { name: '保存草稿', exact: true }).click();
    await page.getByText('浏览器测试线路', { exact: true }).waitFor();
    await page.getByLabel('搜索线路').fill('浏览器测试线路');
    assert.equal(await page.locator('tbody tr').count(), 1);
    await page.getByRole('button', { name: '接入', exact: true }).click();
    assert.match(await page.getByRole('dialog').innerText(), /vless:\/\//);
    await close();
    await page.getByRole('button', { name: '编辑 浏览器测试线路', exact: true }).click();
    assert.equal(await page.getByLabel('第 3 跳服务器').inputValue(), second);
    await page.screenshot({ path: path.join(artifacts, 'editor.png'), fullPage: true });
    await close();
    await page.getByRole('button', { name: '删除 浏览器测试线路', exact: true }).click();
    await page.getByRole('button', { name: '确认删除', exact: true }).click();
    await page.getByRole('heading', { name: '没有匹配的线路', exact: true }).waitFor();
    await page.getByRole('button', { name: '清除搜索', exact: true }).click();

    await page.getByRole('button', { name: '预览并发布', exact: true }).click();
    await page.getByRole('heading', { name: '预览配置发布', exact: true }).waitFor();
    await page.getByRole('button', { name: '发布此版本', exact: true }).click();
    await page.getByRole('heading', { name: '诊断与发布', exact: true }).waitFor();
    await page.waitForFunction(() => !document.body.innerText.includes('正在发布…'));

    await page.getByRole('link', { name: '服务器', exact: true }).click();
    await page.getByRole('button', { name: '添加服务器', exact: true }).click();
    await page.getByLabel('服务器名称', { exact: true }).fill('浏览器测试节点');
    await page.getByLabel('服务器 IP / 域名', { exact: true }).fill('127.0.0.1');
    await page.getByLabel('REALITY 目标', { exact: true }).fill('localhost:443');
    await page.getByLabel('REALITY Server Name', { exact: true }).fill('localhost');
    await page.getByRole('button', { name: '创建服务器', exact: true }).click();
    await page.getByRole('heading', { name: '接入服务器', exact: true }).waitFor();
    assert.equal(await page.getByRole('button', { name: '安装并接入 Agent', exact: true }).isDisabled(), true);
    await page.getByLabel('节点可以访问的控制台地址').fill('https://panel.example.com');
    await page.getByRole('button', { name: '使用脚本安装', exact: true }).click();
    const code = await page.getByLabel('节点接入码', { exact: true }).inputValue();
    assert.ok(code.startsWith('chain-agent-v1:'));
    const connection = JSON.parse(Buffer.from(code.split(':')[1], 'base64url').toString());
    assert.equal(connection.panel_url, 'https://panel.example.com');
    assert.ok(connection.token.length >= 32);
    assert.ok(!await page.getByRole('dialog').innerText().then(text => text.includes('CHAIN_NODE_TOKEN_FILE')));
    const { execFileSync } = require('node:child_process');
    const decoded = JSON.parse(execFileSync('python3', ['-B', '-c', 'import sys,json; sys.path.insert(0,sys.argv[1]); from chain_install import decode_connection_code; print(json.dumps(decode_connection_code(sys.stdin.read())))', path.resolve(__dirname, '../../scripts')], { input: code, encoding: 'utf8' }));
    assert.deepEqual(decoded, connection, 'Web connection code must work in the actual installer');
    const heartbeat = await context.request.post(`${base}/api/agent/${connection.node_id}/sync`, {
      headers: { Authorization: `Bearer ${connection.token}` }, data: { running: false, applied: false, revision: '', error: '' },
    });
    assert.equal(heartbeat.status(), 200);
    await page.getByText('节点已在线，可以创建线路了', { exact: true }).waitFor();
    await close();
    await page.getByRole('button', { name: '删除 浏览器测试节点', exact: true }).click();
    await page.getByRole('button', { name: '确认删除', exact: true }).click();
    await page.getByRole('dialog').waitFor({ state: 'hidden' });

    await page.setViewportSize({ width: 390, height: 844 });
    await page.getByRole('button', { name: '打开导航', exact: true }).click();
    await page.getByRole('link', { name: '端口转发', exact: true }).filter({ visible: true }).click();
    await page.getByRole('heading', { name: '端口转发', exact: true }).waitFor();
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, 'mobile page overflow');
    await page.screenshot({ path: path.join(artifacts, 'mobile.png'), fullPage: true });
    await page.getByRole('button', { name: '新建转发', exact: true }).click();
    await page.getByLabel('线路名称').fill('移动端草稿');
    await page.getByRole('button', { name: '添加下一跳' }).click();
    const dialog = await page.getByRole('dialog').boundingBox();
    assert.ok(dialog.x >= 0 && dialog.x + dialog.width <= 390, 'mobile dialog overflow');
    await page.screenshot({ path: path.join(artifacts, 'mobile-editor.png'), fullPage: true });
    await close();
    await page.getByRole('button', { name: '退出登录', exact: true }).click();
    await page.getByRole('heading', { name: '登录网络控制台', exact: true }).waitFor();
    assert.deepEqual(errors, [], 'Browser console errors');
    await context.close();
    console.log('PASS browser login, real API CRUD, three-hop reorder, sharing, publish, desktop and mobile layout, logout');
  } finally {
    if (browser) await browser.close();
    await server.close();
    console.log(`CLEANUP browser pid=${record.browserPid} websocket=${record.websocket}`);
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
