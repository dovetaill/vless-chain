const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(process.env.CHAIN_TEST_BROWSER);
const assets = path.resolve(process.argv[2]);

(async () => {
  const root = process.env.CHAIN_TEST_ROOT;
  const ownership = { pid: process.pid, cwd: process.cwd(), artifacts: root, browser: null };
  const record = () => root && fs.writeFileSync(path.join(root, 'rentals-ui-ownership.json'), JSON.stringify(ownership));
  record();
  const server = await chromium.launchServer({ headless: true });
  ownership.browser = server.process().pid; record();
  let browser;
  try {
    browser = await chromium.connect(server.wsEndpoint());
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, locale: 'zh-CN', reducedMotion: 'reduce' });
    const errors = [], requests = [], writes = [];
    context.on('page', page => page.on('pageerror', error => errors.push(error.message)));
    const now = Math.floor(Date.now() / 1000);
    const traffic = { epoch: 'test', month: '2026-10', upload: 1000, download: 2000, total_upload: 1000, total_download: 2000, upload_bps: 200, download_bps: 300, connections: 2, sampled_at: now, blocked: false, cycle_start: now - 100, cycle_end: now + 86400, days: { [new Date((now + 28800) * 1000).toISOString().slice(0, 10)]: [1000, 2000] } };
    const rental = { customer_id: 'customer-a', prices: { monthly: 990, quarterly: 2500, yearly: null }, started_at: now - 100, expires_at: now + 86400, paid_months: 1, next_reset_at: now + 86400, query_link_exists: false, permanent: false };
    const makeLine = (id, name, protocol) => ({ id, name, kind: 'vless', hops: [{ node_id: 'node', port: 4000 + Number(id) }], enabled: true, published: true, external_id: null, chain: [], target: '', network: 'tcp', share: null,
      access: { transport: { network: 'tcp', path: '', host: '', mode: 'auto' }, reality: null, shared_port: false, fallback: '', host: 'proxy.example', protocol, username: '', password: 'not-searchable', method: '', public_host: '', public_port: null, tls: { mode: 'off', domain: '', cert: '', key: '' } },
      limits: { monthly_gib: 1, upload_mbps: null, download_mbps: null }, traffic: { ...traffic }, rental: { ...rental, prices: { ...rental.prices } }, rental_status: 'active', sync_status: 'synced' });
    const state = { nodes: [{ id: 'node', name: 'Tokyo', host: '192.0.2.1', online: true, seen: now, report: { running: true, applied: true, error: '', revision: '', traffic: {}, system: { cpu_percent: 12.5, memory_used: 1024 ** 3, memory_total: 4 * 1024 ** 3, disk_used: 20 * 1024 ** 3, disk_total: 80 * 1024 ** 3, network_rx: 300000, network_tx: 400000, network_rx_bps: 1024, network_tx_bps: 2048, sampled_at: now } } }],
      local_node_id: 'node', externals: [], lines: [makeLine('1', '东京 A', 'vless'), makeLine('2', '香港 B', 'trojan'), makeLine('3', '自用 C', 'vless')], checks: {}, jobs: [], busy: false, time: now, subscriptions: [], security: { totp_enabled: false, recovery_remaining: 0, origin: '', passkeys: [] }, alerts: [], customers: [{ id: 'customer-a', name: '客户甲', contact: '甲的联系', notes: '' }], payments: [], renewal_contact: '微信：operator' };
    state.lines[1].traffic.download = 0.95 * 1024 ** 3;
    state.lines[1].rental.expires_at = now + 60 * 86400;
    state.lines[2].rental = null;
    await context.route('http://chain.test/**', async route => {
      try {
      const request = route.request(), url = new URL(request.url());
      if (url.pathname.startsWith('/api/')) {
        const data = request.method() === 'GET' ? null : request.postData() ? request.postDataJSON() : null;
        requests.push({ path: url.pathname, data });
        if (request.method() !== 'GET') writes.push({ path: url.pathname, data });
        let body;
        if (url.pathname === '/api/state') body = state;
        else if (url.pathname === '/api/auth/options') body = { totp: false, passkey: false };
        else if (url.pathname === '/api/customers') { const customer = { ...data, id: data.id || 'customer-b' }; state.customers = [...state.customers.filter(item => item.id !== customer.id), customer]; body = customer; }
        else if (/^\/api\/rentals\/\d+$/.test(url.pathname)) {
          const line = state.lines.find(line => line.id === url.pathname.split('/').at(-1));
          line.rental = { ...(line.rental || { ...rental, started_at: 0, expires_at: 0 }), ...data }; body = { ok: true, results: [{ id: line.id, sync_status: 'synced' }] };
        } else if (url.pathname === '/api/lines/operate') {
          for (const id of data.line_ids) {
            const line = state.lines.find(line => line.id === id);
            if (data.action === 'disable' || data.action === 'enable') { line.enabled = data.action === 'enable'; line.rental_status = line.enabled ? 'active' : 'disabled'; }
            else if (data.action === 'limits') line.limits = data.limits;
            else if (data.action === 'unlimited') line.rental = { ...(line.rental || { ...rental, customer_id: '', prices: { monthly: null, quarterly: null, yearly: null }, started_at: now }), permanent: true, expires_at: 0 };
            else if (data.action === 'delete') state.lines = state.lines.filter(line => line.id !== id);
          }
          body = { ok: true, results: data.line_ids.map(id => ({ id, sync_status: 'synced' })) };
        } else if (url.pathname === '/api/rentals/renew') {
          for (const id of data.line_ids) {
            const line = state.lines.find(line => line.id === id);
            const payment = { id: String(state.payments.length), request_id: data.request_id, line_id: id, line_name: line.name, customer_id: line.rental.customer_id, customer_name: state.customers.find(customer => customer.id === line.rental.customer_id).name, term: data.term, months: 1, amount_fen: data.amounts[id], created: now, previous_expires_at: line.rental.expires_at, expires_at: now + 31 * 86400, note: data.note };
            state.payments.unshift(payment); line.rental = { ...line.rental, permanent: false, started_at: now, expires_at: payment.expires_at }; line.rental_status = 'active';
          }
          body = { ok: true, payments: state.payments };
        } else if (url.pathname.endsWith('/query-link')) body = { path: '/query#token=private-token' };
        else if (url.pathname.endsWith('/export')) body = { share: 'vless://test@proxy.example:443', node: '- {name: test, type: vless, server: proxy.example, port: 443}' };
        else if (url.pathname === '/api/public/lookup') {
          const lines = data.token ? [state.lines[2]] : [state.lines[0], state.lines[2]];
          body = { lines: lines.map(line => ({ id: line.id, name: line.name, status: line.rental_status, sync_status: line.sync_status, traffic: line.traffic, limits: line.limits, rental: line.rental })), unmatched: data.token ? [] : ['第 3 个代理'], renewal_contact: state.renewal_contact, time: now };
        } else throw new Error('Unexpected API: ' + url.pathname);
        return route.fulfill({ json: body });
      }
      const file = path.join(assets, ['/', '/admin', '/admin/', '/query', '/query/'].includes(url.pathname) ? 'index.html' : url.pathname);
      return fs.existsSync(file) ? route.fulfill({ body: fs.readFileSync(file), contentType: { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css' }[path.extname(file)] || 'application/octet-stream' }) : route.fulfill({ status: 404 });
      } catch (error) {
        errors.push(error.message);
        await route.fulfill({ status: 500, json: { error: error.message } }).catch(() => {});
      }
    });
    const page = await context.newPage();
    const button = name => page.getByRole('button', { name, exact: true });
    const nav = name => page.locator('aside nav').getByRole('button', { name, exact: true }).click();
    const close = async () => { await page.getByRole('dialog').getByRole('button', { name: '关闭', exact: true }).first().click(); await page.getByRole('dialog').waitFor({ state: 'hidden' }); };
    const layout = async label => {
      await page.evaluate(() => document.fonts.ready);
      if (root) await page.screenshot({ path: path.join(root, label + '.png'), fullPage: true, animations: 'disabled' });
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), label + ': page overflow');
      for (const header of await page.locator('[data-slot="card-header"]').all()) {
        assert.ok(await header.evaluate(el => ['paddingTop', 'paddingRight', 'paddingBottom', 'paddingLeft'].every(key => parseFloat(getComputedStyle(el)[key]) >= 16)), label + ': header padding');
      }
      for (const control of await page.locator('[data-slot="card"] button:visible').all()) {
        assert.ok(await control.evaluate(el => {
          if (el.closest('table')) return true;
          const card = el.closest('[data-slot="card"]').getBoundingClientRect(), box = el.getBoundingClientRect();
          return box.left >= card.left + 16 && box.right <= card.right - 16;
        }), label + ': action touches card edge');
      }
    };
    await page.goto('http://chain.test/');
    await page.getByRole('heading', { name: '查询我的代理', exact: true }).waitFor();
    assert.equal(requests.filter(request => request.path === '/api/state').length, 0, 'public home must not request admin state');
    assert.equal(await page.getByRole('link', { name: '管理登录' }).getAttribute('href'), '/admin');
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: 900 });
      await layout('public-home-' + width);
      assert.equal(await page.getByLabel('代理链接或 Mihomo 配置').evaluate(el => getComputedStyle(el).minHeight), '144px');
    }
    await page.getByRole('link', { name: '管理登录' }).click();
    await page.getByRole('heading', { name: '总览', exact: true }).waitFor();
    state.externals = [{ id: 'external', name: 'external'.repeat(10), url: 'socks5://example.com:1080' }];
    state.security.passkeys = [{ id: 'passkey', name: 'authenticator'.repeat(6), created: now }];
    state.subscriptions = [{ id: 'subscription', name: 'subscription'.repeat(6), line_ids: ['1'], created: now }];
    for (const width of [1440, 1024, 768, 390, 320]) {
      await page.setViewportSize({ width, height: 900 });
      for (const [route, title] of [['dashboard', '总览'], ['lines', '代理'], ['customers', '客户与收款'], ['nodes', '外部代理'], ['settings', '设置']]) {
        await page.goto('http://chain.test/admin#' + route);
        await page.getByRole('heading', { name: title, exact: true, level: 1 }).waitFor();
        await layout(route + '-' + width);
        if (route === 'lines') {
          const filters = page.getByRole('group', { name: '代理搜索与筛选' });
          const boxes = await Promise.all((await filters.locator('input, select').all()).map(el => el.boundingBox()));
          if (width >= 1280) assert.ok(boxes.every(box => Math.abs(box.y - boxes[0].y) < 1), 'desktop filters must share one row');
          assert.ok(boxes.every(box => Math.abs(box.height - boxes[0].height) < 1), 'toolbar controls must align');
          const input = filters.locator('input');
          assert.equal(await input.evaluate(el => getComputedStyle(el).fontSize), '13px', 'template input font must not be reset');
          const frame = await filters.boundingBox();
          assert.ok(boxes.every(box => box.y >= frame.y + 16 && box.y + box.height <= frame.y + frame.height - 16), 'filter padding');
        }
        if (route === 'customers') {
          assert.ok(await page.getByText('尚无收款记录。', { exact: true }).evaluate(el => parseFloat(getComputedStyle(el).paddingLeft) >= 20), 'empty payments need content padding');
          await button('新建客户').click();
          await layout('customer-dialog-' + width);
          const dialog = page.getByRole('dialog');
          assert.ok(await dialog.evaluate(el => el.scrollWidth <= el.clientWidth), 'customer dialog overflow');
          await close();
        }
      }
    }
    await page.setViewportSize({ width: 1440, height: 1000 });
    state.externals = [];
    state.security.passkeys = [];
    state.subscriptions = [];
    const originalLines = state.lines;
    state.lines = [];
    await page.goto('http://chain.test/admin#lines');
    await page.getByRole('heading', { name: '暂无代理', exact: true }).waitFor();
    await layout('lines-empty-1440');
    state.lines = originalLines;
    await page.goto('http://chain.test/admin#dashboard');
    await page.getByRole('heading', { name: '总览', exact: true }).waitFor();
    await page.getByText('12.5%', { exact: true }).waitFor();
    assert.equal(await page.getByRole('heading', { name: '近 30 天代理流量' }).count(), 1);
    await nav('客户与收款'); await button('新建客户').click();
    await page.getByLabel('客户名称', { exact: true }).fill('客户乙'); await page.getByLabel('联系方式', { exact: true }).fill('乙的联系');
    await button('保存客户').click(); await page.getByRole('dialog').waitFor({ state: 'hidden' });
    assert.equal(writes.find(write => write.path === '/api/customers').data.name, '客户乙');
    await nav('代理');
    const search = page.getByLabel('搜索代理名称、客户、服务器、地址或端口');
    await search.fill('客户甲 Tokyo 4001');
    await page.getByLabel('筛选协议').selectOption('vless');
    assert.equal(await page.locator('tbody tr').count(), 1);
    await page.getByLabel('选择 东京 A', { exact: true }).check();
    await page.getByLabel('批量操作').getByRole('button', { name: '停用', exact: true }).click();
    await page.getByRole('region', { name: '代理列表', exact: true }).getByText('手动停用', { exact: true }).waitFor();
    assert.deepEqual(writes.find(write => write.path === '/api/lines/operate').data, { line_ids: ['1'], action: 'disable' });
    assert.equal(state.lines[1].enabled, true);
    assert.equal(writes.some(write => write.path === '/api/publish'), false);
    await button('清除筛选').click();
    await page.getByLabel('筛选到期与余量').selectOption('low');
    assert.equal(await page.locator('tbody tr').count(), 1); assert.match(await page.locator('tbody').innerText(), /香港 B/);
    await button('清除筛选').click(); await search.fill('自用 C');
    await page.getByLabel('自用 C 更多操作').click(); await page.getByRole('menuitem', { name: '设为无限期（自用）', exact: true }).click();
    await button('确认设为无限期并下发').click(); await page.getByRole('dialog').waitFor({ state: 'hidden' });
    assert.equal(state.lines[2].rental.permanent, true); assert.equal(state.lines[2].rental.customer_id, '');
    await button('出租信息').click(); await page.getByLabel('所属客户').selectOption('customer-b');
    await page.getByLabel('月付报价 · 元').fill('12.34'); await button('保存归属与报价').click(); await page.getByRole('dialog').waitFor({ state: 'hidden' });
    assert.deepEqual(writes.find(write => write.path === '/api/rentals/3').data.prices, { monthly: 1234, quarterly: null, yearly: null });
    await button('收款 / 续费').click(); await page.getByText('本次收款将改为有限期并重新开通，流量恢复满额。').waitFor();
    await page.getByLabel('实收 · 元').fill('12.00'); await button('确认已收款并下发').click(); await page.getByRole('dialog').waitFor({ state: 'hidden' });
    const payment = writes.find(write => write.path === '/api/rentals/renew').data;
    assert.deepEqual(payment.line_ids, ['3']); assert.equal(payment.amounts['3'], 1200); assert.match(payment.request_id, /^[0-9a-f-]{36}$/);
    await page.getByLabel('自用 C 更多操作').click(); await page.getByRole('menuitem', { name: '客户私密查询链接', exact: true }).click();
    await button('生成查询链接').click(); assert.equal(await page.getByLabel('查询链接', { exact: true }).inputValue(), 'http://chain.test/query#token=private-token'); await close();
    await page.getByLabel('选择 自用 C', { exact: true }).check(); await page.getByLabel('批量操作').getByRole('button', { name: '设为无限期（自用）', exact: true }).click();
    await button('确认设为无限期并下发').click(); await page.getByRole('dialog').waitFor({ state: 'hidden' });
    await page.setViewportSize({ width: 390, height: 844 });
    await page.waitForFunction(() => getComputedStyle(document.querySelector('.app-main')).marginLeft === '0px');
    await layout('lines-filtered-390');
    await page.close();
    const adminRequests = requests.filter(request => request.path === '/api/state').length;
    const query = await context.newPage(); await query.clock.install();
    await query.goto('http://chain.test/query#token=private-token');
    await query.getByRole('heading', { name: '自用 C', exact: true }).waitFor();
    assert.equal(await query.getByText('无限期', { exact: true }).count(), 1);
    assert.equal(await query.getByRole('button', { name: '复制续费信息', exact: true }).count(), 0);
    assert.deepEqual(requests.filter(request => request.path === '/api/public/lookup').at(-1).data, { token: 'private-token' });
    await query.getByRole('button', { name: '使用代理配置查询', exact: true }).click();
    const config = 'proxies:\n  - {name: one}\n  - {name: two}\n  - {name: unmatched}';
    await query.getByLabel('代理链接或 Mihomo 配置').fill(config); await query.getByRole('button', { name: '查询服务', exact: true }).click();
    await query.getByText(/以下配置未匹配本平台出租服务：第 3 个代理/).waitFor();
    assert.equal(await query.getByRole('heading', { name: '东京 A', exact: true }).count(), 1);
    assert.equal(await query.getByRole('heading', { name: '自用 C', exact: true }).count(), 1);
    assert.equal(await query.getByRole('button', { name: '复制续费信息', exact: true }).count(), 1);
    await query.getByLabel('代理链接或 Mihomo 配置').fill('not-submitted');
    const poll = query.waitForResponse(response => new URL(response.url()).pathname === '/api/public/lookup');
    await query.clock.fastForward(10001); assert.deepEqual((await poll).request().postDataJSON(), { config });
    await query.evaluate(() => { Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' }); document.dispatchEvent(new Event('visibilitychange')); });
    const beforeHidden = requests.filter(request => request.path === '/api/public/lookup').length;
    await query.clock.fastForward(30000); assert.equal(requests.filter(request => request.path === '/api/public/lookup').length, beforeHidden);
    assert.equal(requests.filter(request => request.path === '/api/state').length, adminRequests, 'public page must not request admin state');
    await query.setViewportSize({ width: 390, height: 844 });
    assert.equal(await query.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, 'query mobile overflow');
    assert.deepEqual(errors, []);
    await context.close();
    console.log('PASS rentals dashboard, customer/pricing/payment, selected-ID operations, unlimited, credential query isolation, partial matches, visible-only polling and mobile layout');
  } finally {
    if (browser) await browser.close();
    await server.close();
    console.log('CLEANUP rentals browser pid=' + ownership.browser);
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
