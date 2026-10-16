// Live operator/customer workflows. Mutations are limited to this run's named resources.
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const { createHash, randomUUID } = require('node:crypto');
const { chromium } = require(process.env.CHAIN_TEST_BROWSER);
const [address, tokenFile, artifactDirectory] = process.argv.slice(2);
assert.ok(address && tokenFile && artifactDirectory, 'Usage: rentals-live-ui.cjs <controller-url> <admin-token-file> <artifact-directory>');
const base = address.replace(/\/$/, '');
const origin = new URL(base).origin;
const prefix = `live-ui-${Date.now()}-${randomUUID().slice(0, 8)}`;
const output = path.resolve(artifactDirectory, 'rentals-live-ui', prefix);
fs.mkdirSync(output, { recursive: true, mode: 0o700 });
const recordFile = path.join(output, 'ownership.json');
const record = { pid: process.pid, cwd: process.cwd(), prefix, browserPid: null, controller: origin, output, lineIds: [], nodeIds: [], customerIds: [], paymentIds: [], requestIds: [], ports: [], cleaned: false, preserved: 'Existing controller/Agent and pre-existing resources; test payment records require owner cleanup.' };
const saveRecord = () => fs.writeFileSync(recordFile, JSON.stringify(record, null, 2), { mode: 0o600 });
saveRecord();

(async () => {
  let server, browser, context, customerContext, page, initial;
  const errors = [], mutations = [], cleanupErrors = [];
  const headers = { 'X-Chain-Request': '1', Origin: origin };
  const owned = value => value.startsWith(prefix);
  const fingerprints = state => state.lines.filter(line => !owned(line.name)).map(line => [line.id, createHash('sha256').update(JSON.stringify({
    name: line.name, kind: line.kind, hops: line.hops, enabled: line.enabled, access: line.access, limits: line.limits, external_id: line.external_id, chain: line.chain, target: line.target, network: line.network,
    rental: line.rental && { customer_id: line.rental.customer_id, prices: line.rental.prices, started_at: line.rental.started_at, expires_at: line.rental.expires_at, paid_months: line.rental.paid_months, permanent: line.rental.permanent },
  })).digest('hex')]).sort(([a], [b]) => a.localeCompare(b));
  async function state() {
    const response = await context.request.get(`${base}/api/state`, { headers });
    assert.equal(response.status(), 200, 'Authenticated state must be available');
    const value = await response.json();
    for (const line of value.lines.filter(line => owned(line.name))) {
      if (!record.lineIds.includes(line.id)) record.lineIds.push(line.id);
      for (const hop of line.hops) {
        if (!record.ports.includes(hop.port)) record.ports.push(hop.port);
        if (!record.nodeIds.includes(hop.node_id)) record.nodeIds.push(hop.node_id);
      }
    }
    for (const customer of value.customers.filter(customer => owned(customer.name))) if (!record.customerIds.includes(customer.id)) record.customerIds.push(customer.id);
    for (const payment of value.payments.filter(payment => owned(payment.line_name) || record.lineIds.includes(payment.line_id))) {
      if (!record.paymentIds.includes(payment.id)) record.paymentIds.push(payment.id);
      if (!record.requestIds.includes(payment.request_id)) record.requestIds.push(payment.request_id);
    }
    saveRecord();
    return value;
  }
  async function waitState(predicate, description) {
    const deadline = Date.now() + 60000;
    let latest;
    do {
      latest = await state();
      if (predicate(latest)) return latest;
      await new Promise(resolve => setTimeout(resolve, 500));
    } while (Date.now() < deadline);
    const statuses = latest.lines.filter(line => owned(line.name)).map(line => ({ id: line.id, status: line.rental_status, sync: line.sync_status, published: line.published }));
    throw new Error(`${description}: ${JSON.stringify({ busy: latest.busy, lines: statuses })}`);
  }
  const ready = ids => waitState(value => !value.busy && ids.every(id => value.lines.some(line => line.id === id && line.sync_status === 'synced')), 'Agent did not confirm the selected operations');
  async function cleanup() {
    if (!context || !initial) return;
    try {
      let current = await state();
      if (!record.lineIds.length && !record.customerIds.length) {
        record.cleaned = true; saveRecord(); return;
      }
      current = await waitState(value => !value.busy, 'Controller remained busy during cleanup');
      const ids = current.lines.filter(line => owned(line.name) && record.lineIds.includes(line.id)).map(line => line.id);
      if (ids.length) {
        const response = await context.request.post(`${base}/api/lines/operate`, { headers, data: { line_ids: ids, action: 'delete' } });
        assert.equal(response.status(), 200, 'Could not remove this run’s proxy resources');
      }
      current = await waitState(value => !value.busy && !value.lines.some(line => owned(line.name)) && record.nodeIds.every(id => value.nodes.some(node => node.id === id && node.online && node.report.applied && node.report.revision === node.desired_revision)), 'Deleted proxy configuration was not confirmed by Agent');
      for (const customer of current.customers.filter(customer => owned(customer.name) && record.customerIds.includes(customer.id))) {
        const response = await context.request.delete(`${base}/api/customers/${customer.id}`, { headers });
        assert.equal(response.status(), 200, `Could not remove customer ${customer.id}`);
      }
      current = await state();
      assert.equal(current.lines.some(line => owned(line.name)), false);
      assert.equal(current.customers.some(customer => owned(customer.name)), false);
      assert.deepEqual(fingerprints(current), fingerprints(initial), 'Pre-existing proxy configuration changed');
      record.cleaned = true;
      saveRecord();
    } catch (error) {
      cleanupErrors.push(error.message);
      record.cleanupErrors = cleanupErrors;
      saveRecord();
    }
  }
  try {
    server = await chromium.launchServer({ headless: true });
    record.browserPid = server.process().pid;
    saveRecord();
    browser = await chromium.connect(server.wsEndpoint());
    context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, locale: 'zh-CN' });
    context.on('page', current => current.on('pageerror', error => errors.push(error.message)));
    page = await context.newPage();
    page.setDefaultTimeout(30000);
    page.on('request', request => {
      const pathname = new URL(request.url()).pathname;
      if (pathname.startsWith('/api/') && request.method() !== 'GET') {
        const body = request.postData() ? request.postDataJSON() : null;
        if (pathname !== '/api/login') mutations.push({ path: pathname, body });
      }
    });
    const button = name => page.getByRole('button', { name, exact: true });
    const dialog = () => page.getByRole('dialog').last();
    const close = async () => { await dialog().getByRole('button', { name: '关闭', exact: true }).first().click(); await page.getByRole('dialog').waitFor({ state: 'hidden' }); };
    const nav = async name => { await page.locator('aside nav').getByRole('button', { name, exact: true }).click(); await page.getByRole('heading', { name, exact: true, level: 1 }).waitFor(); };
    const shot = async name => { await page.screenshot({ path: path.join(output, `${name}.png`), fullPage: false, animations: 'disabled' }); };
    const search = () => page.getByLabel('搜索代理名称、客户、服务器、地址或端口', { exact: true });
    const rows = () => page.getByRole('region', { name: '代理列表', exact: true }).locator('tbody tr');
    const row = name => rows().filter({ has: page.getByText(name, { exact: true }) });
    async function focusLine(name) { await search().fill(name); await row(name).waitFor(); }
    async function menu(name, action) { await row(name).getByRole('button', { name: `${name} 更多操作`, exact: true }).click(); await page.getByRole('menuitem', { name: action, exact: true }).click(); }
    async function submit(name) {
      await dialog().getByRole('button', { name, exact: true }).click();
      await page.getByRole('dialog').waitFor({ state: 'hidden', timeout: 60000 });
    }
    async function selectBoth() { await search().fill(prefix); assert.equal(await rows().count(), 2); await page.getByLabel('选择全部筛选结果', { exact: true }).check(); }
    const batch = name => page.getByLabel('批量操作', { exact: true }).getByRole('button', { name, exact: true });
    async function downloadFile(name, destination) {
      const event = page.waitForEvent('download');
      await dialog().getByRole('button', { name, exact: true }).click();
      const download = await event;
      await download.saveAs(path.join(output, destination));
      return fs.readFileSync(path.join(output, destination), 'utf8');
    }

    await page.goto(base + '/admin');
    await page.getByRole('heading', { name: '登录控制台', exact: true }).waitFor();
    await page.getByLabel('管理员密钥', { exact: true }).fill(fs.readFileSync(tokenFile, 'utf8').trim());
    await button('登录').click();
    await page.getByRole('heading', { name: '总览', exact: true }).waitFor();
    initial = await state();
    assert.equal(initial.busy, false, 'Run this workflow after other controller tests finish');
    assert.equal(initial.lines.some(line => !line.published), false, 'Existing unpublished drafts must not be published by this test');
    const local = initial.nodes.find(node => node.id === initial.local_node_id);
    assert.ok(local?.online, 'A real online Agent must be registered as local_node_id');
    await page.getByRole('heading', { name: '服务器实时状态', exact: true }).waitFor();
    await page.getByRole('heading', { name: '近 30 天代理流量', exact: true }).waitFor();
    await shot('01-dashboard');
    console.log(`START live rentals UI prefix=${prefix}`);

    const customerName = `${prefix} 客户`;
    await nav('客户与收款'); await button('新建客户').click();
    await dialog().getByLabel('客户名称', { exact: true }).fill(customerName);
    await dialog().getByLabel('联系方式', { exact: true }).fill('仅本轮验收，不联系真实客户');
    await dialog().getByLabel('内部备注', { exact: true }).fill(prefix);
    await submit('保存客户');
    const customer = (await state()).customers.find(customer => customer.name === customerName);
    assert.ok(customer);
    await nav('代理');
    const names = [`${prefix} A`, `${prefix} B`], lines = [], shares = [];
    for (const name of names) {
      await button('新建代理').first().click();
      await dialog().getByLabel('名称', { exact: true }).fill(name);
      await dialog().getByLabel('协议', { exact: true }).selectOption('socks');
      await dialog().getByRole('button', { name: '创建代理', exact: true }).click();
      await page.getByRole('heading', { name: /^(分享代理|应用这些修改)$/ }).waitFor({ timeout: 60000 });
      const confirmApply = dialog().getByRole('button', { name: '确认并应用', exact: true });
      if (await confirmApply.isVisible()) await confirmApply.click();
      await page.getByRole('heading', { name: '分享代理', exact: true }).waitFor({ timeout: 60000 });
      const node = page.getByLabel('Mihomo 节点配置', { exact: true });
      await node.waitFor(); assert.equal(JSON.parse((await node.inputValue()).trim().slice(2)).type, 'socks5');
      await dialog().getByRole('button', { name: '链接 / 二维码', exact: true }).click();
      const share = await page.getByLabel('分享链接', { exact: true }).inputValue();
      assert.match(share, /^socks5:\/\//); shares.push(share);
      await close();
      const line = (await state()).lines.find(line => line.name === name);
      assert.ok(line); lines.push(line);
      await ready([line.id]);
    }
    const lineIds = lines.map(line => line.id);
    const publishCount = mutations.filter(write => write.path === '/api/publish').length;
    for (let index = 0; index < names.length; index++) {
      await focusLine(names[index]);
      await row(names[index]).getByRole('button', { name: '出租信息', exact: true }).click();
      await dialog().getByLabel('所属客户', { exact: true }).selectOption(customer.id);
      await dialog().getByLabel('月付报价 · 元', { exact: true }).fill(index ? '20.00' : '12.34');
      await dialog().getByLabel('季付报价 · 元', { exact: true }).fill(index ? '50.00' : '30.00');
      await dialog().getByLabel('年付报价 · 元', { exact: true }).fill(index ? '' : '120.00');
      await submit('保存归属与报价'); await ready([lineIds[index]]);
    }
    let current = await state();
    assert.deepEqual(current.lines.find(line => line.id === lineIds[0]).rental.prices, { monthly: 1234, quarterly: 3000, yearly: 12000 });
    assert.equal(current.lines.find(line => line.id === lineIds[1]).rental.prices.yearly, null);

    await focusLine(names[0]); await row(names[0]).getByRole('button', { name: '收款 / 续费', exact: true }).click();
    await dialog().getByLabel('付费周期', { exact: true }).selectOption('monthly');
    await dialog().getByLabel('实收 · 元', { exact: true }).fill('12.00');
    await dialog().getByLabel('收款备注', { exact: true }).fill(`${prefix} 首次收款`);
    await submit('确认已收款并下发');
    current = await ready([lineIds[0]]);
    const firstRental = current.lines.find(line => line.id === lineIds[0]).rental;
    assert.ok(firstRental.started_at > 0 && firstRental.expires_at > firstRental.started_at);
    assert.equal(current.payments.find(payment => payment.note === `${prefix} 首次收款`).amount_fen, 1200);

    await selectBoth(); await batch('收款 / 续费').click();
    assert.equal(await dialog().getByLabel('付费周期', { exact: true }).locator('option[value="yearly"]').count(), 0, 'A closed price must not be offered for a batch');
    await dialog().getByLabel('付费周期', { exact: true }).selectOption('quarterly');
    await dialog().locator(`input[name="amount:${lineIds[0]}"]`).fill('29.99');
    await dialog().locator(`input[name="amount:${lineIds[1]}"]`).fill('49.50');
    await dialog().getByLabel('收款备注', { exact: true }).fill(`${prefix} 批量季付`);
    await submit('确认已收款并下发'); current = await ready(lineIds);
    assert.equal(current.lines.find(line => line.id === lineIds[0]).rental.started_at, firstRental.started_at);
    assert.ok(current.lines.find(line => line.id === lineIds[0]).rental.expires_at > firstRental.expires_at);
    assert.deepEqual(current.payments.filter(payment => payment.note === `${prefix} 批量季付`).map(payment => payment.amount_fen).sort((a, b) => a - b), [2999, 4950]);

    await focusLine(names[0]); await row(names[0]).getByRole('button', { name: '收款 / 续费', exact: true }).click();
    await dialog().getByLabel('付费周期', { exact: true }).selectOption('yearly');
    await dialog().getByLabel('实收 · 元', { exact: true }).fill('119.99');
    await dialog().getByLabel('收款备注', { exact: true }).fill(`${prefix} 年付续费`);
    await submit('确认已收款并下发'); await ready([lineIds[0]]);
    await row(names[0]).getByRole('button', { name: '出租信息', exact: true }).click();
    assert.equal(await dialog().getByLabel('所属客户', { exact: true }).isDisabled(), true, 'An active customer assignment must remain locked');
    await close();
    console.log('PASS live customer, monthly/quarterly/yearly collection and exact amounts');

    await search().fill(`${customerName} ${local.host} ${lines[0].hops[0].port}`);
    await page.getByLabel('筛选协议', { exact: true }).selectOption('socks');
    assert.equal(await rows().count(), 1);
    await page.getByLabel(`选择 ${names[0]}`, { exact: true }).check(); await batch('停用').click();
    current = await ready([lineIds[0]]);
    assert.equal(current.lines.find(line => line.id === lineIds[0]).enabled, false);
    assert.equal(current.lines.find(line => line.id === lineIds[1]).enabled, true);
    await page.getByLabel('筛选状态', { exact: true }).selectOption('disabled');
    assert.equal(await rows().count(), 1);
    await page.getByLabel(`选择 ${names[0]}`, { exact: true }).check(); await batch('启用').click(); await ready([lineIds[0]]);
    await button('清除筛选').click();
    await selectBoth(); await batch('额度 / 限速').click();
    await dialog().getByLabel('月额度 · GiB', { exact: true }).fill('0.5');
    await dialog().getByLabel('上传上限 · Mbps', { exact: true }).fill('2.5');
    await dialog().getByLabel('下载上限 · Mbps', { exact: true }).fill('5.25');
    await submit('确认调整并下发'); current = await ready(lineIds);
    for (const id of lineIds) assert.deepEqual(current.lines.find(line => line.id === id).limits, { monthly_gib: 0.5, upload_mbps: 2.5, download_mbps: 5.25 });
    await focusLine(names[1]); await menu(names[1], '设为无限期（自用）'); await submit('确认设为无限期并下发');
    current = await ready([lineIds[1]]);
    assert.equal(current.lines.find(line => line.id === lineIds[1]).rental.permanent, true);
    assert.equal(current.lines.find(line => line.id === lineIds[1]).rental.expires_at, 0);
    assert.equal(current.payments.filter(payment => lineIds.includes(payment.line_id)).length, 4);
    assert.equal(mutations.filter(write => write.path === '/api/publish').length, publishCount, 'Rental operations must not publish unrelated drafts');
    await focusLine(names[0]); await menu(names[0], '客户私密查询链接');
    await dialog().getByRole('button', { name: '生成查询链接', exact: true }).click();
    const privateUrl = await dialog().getByLabel('查询链接', { exact: true }).inputValue();
    assert.equal(new URL(privateUrl).search, ''); assert.ok(new URL(privateUrl).hash.startsWith('#token='));
    await close();
    await selectBoth(); await batch('导出').click();
    await dialog().getByRole('button', { name: '读取已应用配置', exact: true }).click();
    await dialog().getByText('已读取 2 条；未导出 0 条。', { exact: true }).waitFor();
    const exportedYaml = await downloadFile('下载 Mihomo 节点列表', 'proxies.yaml');
    const exportedLinks = await downloadFile('下载链接', 'proxy-links.txt');
    assert.ok(JSON.stringify(exportedLinks.trim().split('\n').sort()) === JSON.stringify([...shares].sort()), 'Batch export must contain exactly the selected proxy credentials');
    const exportedNodes = exportedYaml.split('\n').filter(line => line.trimStart().startsWith('- ')).map(line => JSON.parse(line.trim().slice(2)));
    assert.deepEqual(exportedNodes.map(node => node.name).sort(), [...names].sort());
    await close(); await shot('02-proxy-operations');
    console.log('PASS live selected-ID operations, quota/speed limits, unlimited and batch export');

    customerContext = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale: 'zh-CN' });
    const publicRequests = [];
    customerContext.on('page', current => current.on('pageerror', error => errors.push(error.message)));
    const query = await customerContext.newPage();
    query.on('request', request => publicRequests.push(new URL(request.url()).pathname));
    await query.goto(`${base}/query`);
    assert.equal((await customerContext.cookies()).length, 0, 'Customer context must not contain an administrator session');
    await query.getByLabel('代理链接或 Mihomo 配置', { exact: true }).fill(shares[0]);
    await query.getByRole('button', { name: '查询服务', exact: true }).click();
    await query.getByRole('heading', { name: names[0], exact: true }).waitFor();
    assert.equal(await query.getByRole('heading', { name: names[1], exact: true }).count(), 0);
    await query.getByLabel('代理链接或 Mihomo 配置', { exact: true }).fill(exportedYaml);
    await query.getByRole('button', { name: '查询服务', exact: true }).click();
    await query.getByRole('heading', { name: names[1], exact: true }).waitFor();
    assert.equal(await query.getByRole('heading', { name: names[0], exact: true }).count(), 1);
    assert.equal(await query.getByText('无限期', { exact: true }).count(), 1);
    assert.equal(await query.getByRole('button', { name: '复制续费信息', exact: true }).count(), 1);
    await query.screenshot({ path: path.join(output, '03-customer-mihomo.png'), animations: 'disabled' });
    await query.goto(privateUrl); await query.getByRole('heading', { name: names[0], exact: true }).waitFor();
    assert.equal(await query.getByRole('heading', { name: names[1], exact: true }).count(), 0);
    assert.equal(publicRequests.some(request => request === '/api/state' || request === '/api/auth/options'), false, 'Customer query must be independent of administrator APIs');
    await query.setViewportSize({ width: 390, height: 844 });
    assert.equal(await query.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, 'Public page overflows mobile viewport');
    await query.screenshot({ path: path.join(output, '04-customer-mobile.png'), animations: 'disabled' });
    await query.close(); await customerContext.close(); customerContext = null;

    await nav('客户与收款');
    const history = page.getByRole('region', { name: '收款记录', exact: true });
    assert.equal(await history.locator('tbody tr').filter({ hasText: prefix }).count(), 4);
    await shot('05-payments');
    await nav('代理'); await selectBoth(); await batch('删除').click(); await submit('确认删除并下发');
    current = await waitState(value => !value.busy && lineIds.every(id => !value.lines.some(line => line.id === id)), 'Batch deletion did not remove selected lines');
    assert.equal(current.payments.filter(payment => lineIds.includes(payment.line_id)).length, 4, 'Deleting a proxy must preserve its payment history');
    await nav('客户与收款');
    const customerRow = page.getByRole('region', { name: '客户档案表', exact: true }).locator('tbody tr').filter({ has: page.getByText(customerName, { exact: true }) });
    await customerRow.getByRole('button', { name: '删除', exact: true }).click(); await submit('确认删除');
    current = await state();
    assert.equal(current.customers.some(customer => customer.id === record.customerIds[0]), false);
    assert.equal(current.payments.filter(payment => lineIds.includes(payment.line_id)).length, 4);
    assert.deepEqual(fingerprints(current), fingerprints(initial), 'Existing resources were changed by the workflow');
    assert.deepEqual(errors, [], 'Browser page errors');
    console.log('PASS live anonymous configuration/private-link queries, mobile layout, batch delete and preserved receipt history');
  } catch (error) {
    record.failure = error.message; saveRecord();
    if (page && !page.isClosed()) await page.screenshot({ path: path.join(output, 'failure.png'), fullPage: false, animations: 'disabled' }).catch(() => {});
    throw error;
  } finally {
    await cleanup();
    if (customerContext) await customerContext.close().catch(error => cleanupErrors.push(error.message));
    if (context) await context.close().catch(error => cleanupErrors.push(error.message));
    if (browser) await browser.close().catch(error => cleanupErrors.push(error.message));
    if (server) await server.close().catch(error => cleanupErrors.push(error.message));
    record.cleanupErrors = cleanupErrors; saveRecord();
    console.log(`CLEANUP live rentals browser=${record.browserPid} resources_removed=${record.cleaned} ports=${record.ports.join(',')} receipts_preserved=${record.paymentIds.length} manifest=${recordFile}`);
    if (cleanupErrors.length) throw new Error(`Cleanup incomplete: ${cleanupErrors.join('; ')}`);
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
