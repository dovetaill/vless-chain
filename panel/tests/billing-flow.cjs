// Mocked billing & public lookup UI checks: no panel, Agent, payment provider or production API.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(process.env.CHAIN_TEST_BROWSER);
const assets = path.resolve(process.argv[2]);
const root = process.env.CHAIN_TEST_ROOT;
if (!root) throw new Error('CHAIN_TEST_ROOT must identify the owned verification directory');

(async () => {
  const browser = await chromium.launch({ headless: true, executablePath: process.env.CHAIN_TEST_CHROMIUM });
  try {
    const context = await browser.newContext({ viewport: { width: 1280, height: 900 }, locale: 'zh-CN', reducedMotion: 'reduce' });
    const page = await context.newPage();
    page.setDefaultTimeout(10000);
    const errors = [], writes = [], reads = [], requests = [];
    context.on('page', p => p.on('pageerror', error => errors.push(error.message)));
    page.on('dialog', dialog => dialog.accept());
    const now = 1800000000;
    const line = (id, name, eligible) => ({ id, name, kind: 'vless', hops: [{ node_id: 'local', port: 24443, uuid: 'test-credential' }], enabled: true, published: !eligible, plan_eligible: eligible, customer_plan_id: null,
      chain: [], share: null, traffic: null, rental: null, rental_status: eligible ? 'draft' : 'active', sync_status: eligible ? 'unpublished' : 'synced', limits: { monthly_gib: null, upload_mbps: null, download_mbps: null },
      access: { protocol: 'vless', host: '192.0.2.1', tls: { mode: 'off', domain: '' }, transport: { network: 'tcp' } } });
    const actor = { id: 'admin', name: '管理员', role: 'admin' };
    const state = { viewer: actor, nodes: [], local_node_id: null, externals: [], lines: [line('draft-a', '新草稿线路', true), line('old-line', '已有独立线路', false)],
      checks: {}, jobs: [], busy: false, time: now, subscriptions: [], security: { totp_enabled: false, recovery_remaining: 0, origin: '', passkeys: [] }, alerts: [],
      customers: [{ id: 'customer', name: '客户甲', contact: '', notes: '' }], payments: [], payment_adjustments: [], rental_orders: [], customer_plans: [], renewal_contact: '微信：operator' };
    let loseConfirmation = true, loseAdjustment = true;
    await context.route('**/*', async route => {
      try {
        const request = route.request(), url = new URL(request.url());
        if (url.hostname !== 'chain.test') return route.abort();
        if (!url.pathname.startsWith('/api/')) {
          const file = path.join(assets, ['/', '/admin', '/admin/', '/query', '/query/'].includes(url.pathname) ? 'index.html' : url.pathname);
          return fs.existsSync(file) ? route.fulfill({ body: fs.readFileSync(file), contentType: { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css' }[path.extname(file)] || 'application/octet-stream' }) : route.fulfill({ status: 404 });
        }
        let result;
        if (request.method() === 'GET') {
          reads.push(url.pathname);
          requests.push({ path: url.pathname, data: null });
          if (url.pathname === '/api/state') result = state;
          else if (url.pathname === '/api/auth/options') result = { totp: false, passkey: false };
          else if (url.pathname.startsWith('/api/rentals/orders/by-request/')) result = state.rental_orders.find(order => order.request_id === url.pathname.split('/').at(-1));
          else if (url.pathname.startsWith('/api/rentals/adjustments/by-request/')) result = state.payment_adjustments.find(adjustment => adjustment.request_id === url.pathname.split('/').at(-1));
          else throw new Error('Unexpected read: ' + url.pathname);
          return route.fulfill(result ? { json: result } : { status: 404, json: { error: '未找到请求' } });
        }
        const data = request.postDataJSON();
        requests.push({ path: url.pathname, data });
        if (url.pathname === '/api/public/lookup') {
          const unlimited = { ...line('public-free', '自用 C', false), rental: { customer_id: '', prices: { monthly: null, quarterly: null, yearly: null }, started_at: now - 100, expires_at: 0, permanent: true, next_reset_at: now + 86400, query_link_exists: true } };
          const active = { ...line('public-paid', '东京 A', false), rental: { customer_id: 'customer', prices: { monthly: 2000, quarterly: null, yearly: null }, started_at: now - 100, expires_at: now + 86400, permanent: false, next_reset_at: now + 86400, query_link_exists: false } };
          const matched = data.token ? [unlimited] : [active, unlimited];
          return route.fulfill({ json: {
            lines: matched.map(item => ({ id: item.id, name: item.name, status: item.rental_status, sync_status: item.sync_status, traffic: item.traffic, limits: item.limits, rental: item.rental })),
            unmatched: data.token ? [] : ['第 3 个代理'],
            renewal_contact: state.renewal_contact,
            time: now,
          } });
        }
        writes.push({ path: url.pathname, body: data });
        if (url.pathname === '/api/customer-plans') {
          const rental = { customer_id: data.customer_id, prices: data.prices, started_at: 0, expires_at: 0, permanent: false, next_reset_at: 0, query_link_exists: false };
          const plan = { ...data, id: 'plan-1', members: data.members.map((member, i) => ({ ...member, id: `member-${i}`, usage: { used: 0, reserved: 0 } })), usage: { used: 0, reserved: 0 }, rental };
          state.customer_plans.push(plan);
          state.lines[0].plan_eligible = false;
          state.lines[0].customer_plan_id = plan.id;
          state.lines[0].rental = rental;
          result = { ok: true };
        } else if (url.pathname === '/api/rentals/orders') {
          const plan = state.customer_plans[0];
          result = { id: 'order-1', request_id: data.request_id, created: now, confirmed_at: 0, cancelled_at: 0, term: data.term, note: data.note, created_by: actor,
            items: [{ line_id: `plan:${plan.id}`, line_name: plan.name, customer_id: plan.customer_id, customer_name: '客户甲', quoted_fen: plan.rental.prices[data.term], amount_fen: data.amounts[`plan:${plan.id}`], started_at: 0, expires_at: 0 }] };
          state.rental_orders.push(result);
        } else if (url.pathname === '/api/rentals/orders/order-1/confirm') {
          const order = state.rental_orders[0], item = order.items[0];
          if (!order.confirmed_at) {
            Object.assign(order, { confirmed_at: now + 1, confirmed_by: actor });
            Object.assign(state.customer_plans[0].rental, { started_at: now + 1, expires_at: now + 2678401, next_reset_at: now + 2678401 });
            state.payments.push({ id: 'payment-1', request_id: order.request_id, ...item, term: order.term, months: 1, created: now + 1, previous_expires_at: 0, expires_at: now + 2678401, note: order.note });
          }
          if (loseConfirmation) { loseConfirmation = false; return route.abort('failed'); }
          result = { ok: true };
        } else if (url.pathname === '/api/rentals/adjustments') {
          const previous = state.payment_adjustments.find(adjustment => adjustment.request_id === data.request_id);
          if (previous) result = previous;
          else {
            const net = state.payments[0].amount_fen + state.payment_adjustments.reduce((sum, adjustment) => sum + adjustment.delta_fen, 0);
            assert.equal(data.expected_net_fen, net);
            assert.equal(data.kind, 'refund');
            assert.ok(data.amount_fen > 0 && data.amount_fen <= net);
            result = { id: `adjustment-${state.payment_adjustments.length + 1}`, payment_id: data.payment_id, request_id: data.request_id, kind: data.kind, delta_fen: -data.amount_fen, created: now + 2, note: data.note, disable_line: data.disable_line, affected_line_ids: data.disable_line ? data.expected_line_ids : [], operator: actor };
            state.payment_adjustments.push(result);
            if (data.disable_line) state.lines.filter(line => data.expected_line_ids.includes(line.id)).forEach(line => { line.enabled = false; line.sync_status = 'pending'; });
          }
          if (loseAdjustment) { loseAdjustment = false; return route.abort('failed'); }
        } else throw new Error('Unexpected mutation: ' + url.pathname);
        return route.fulfill({ json: result });
      } catch (error) { errors.push(error.message); await route.fulfill({ status: 500, json: { error: error.message } }).catch(() => {}); }
    });
    const button = name => page.getByRole('button', { name, exact: true });
    const close = async () => { await page.getByRole('dialog').getByRole('button', { name: '关闭', exact: true }).first().click(); await page.getByRole('dialog').waitFor({ state: 'hidden' }); };

    await page.goto('http://chain.test/');
    await page.getByRole('heading', { name: '查询我的代理', exact: true }).waitFor();
    assert.equal(reads.filter(p => p === '/api/state').length, 0, 'public home must not request admin state');
    assert.equal(await page.getByRole('link', { name: '管理登录' }).getAttribute('href'), '/admin');

    await page.goto('http://chain.test/admin#customers');
    await button('新建共享套餐').click();
    await page.getByLabel('套餐名称', { exact: true }).fill('团队套餐');
    await page.getByLabel('所属客户', { exact: true }).selectOption('customer');
    await page.getByLabel('每周期共享总额度 · GiB', { exact: true }).fill('100');
    await page.getByLabel('月付报价 · 元', { exact: true }).fill('20');
    await page.getByLabel('成员 1 名称', { exact: true }).fill('成员甲');
    await page.getByLabel('新草稿线路', { exact: true }).check();
    assert.equal(await page.getByRole('dialog').getByLabel('已有独立线路', { exact: true }).count(), 0);
    await button('保存套餐与报价').click();
    await page.getByRole('dialog').waitFor({ state: 'hidden' });
    const create = writes.find(write => write.path === '/api/customer-plans').body;
    assert.deepEqual(create.members[0].line_ids, ['draft-a']);
    assert.equal(create.prices.monthly, 2000);
    assert.equal(state.payments.length, 0, '建立套餐不确认收款');

    const planOrderKey = 'chain-order:plan:plan-1';
    await button('开通 / 续费订单').click();
    await button('保存待收款订单').click();
    await page.getByRole('heading', { name: '核对开通 / 续费订单', exact: true }).waitFor();
    assert.equal(state.payments.length, 0, '建立待收款订单不授予权益');
    await page.getByLabel(/已逐项核对客户、周期和金额/).check();
    await button('确认已实际收款并下发权益').click();
    await page.getByRole('dialog').getByRole('alert').waitFor();
    await close();
    const writesBeforeRecovery = writes.length;
    await button('开通 / 续费订单').click();
    await page.getByText(/^已确认收款 ·/).waitFor();
    assert.equal(writes.length, writesBeforeRecovery, '响应丢失后重开只查询，不重新下单或收款');
    assert.equal(state.payments.length, 1);
    assert.ok(reads.some(read => read.startsWith('/api/rentals/orders/by-request/')));
    await page.evaluate(key => localStorage.setItem(key, 'newer-order-request'), planOrderKey);
    await button('已核对，关闭订单').click();
    await page.getByRole('dialog').waitFor({ state: 'hidden' });
    assert.equal(await page.evaluate(key => localStorage.getItem(key), planOrderKey), 'newer-order-request', '旧订单回执不可清除更新的待核对请求');
    await page.evaluate(([key, id]) => localStorage.setItem(key, id), [planOrderKey, state.rental_orders[0].request_id]);
    await button('开通 / 续费订单').click();
    await button('已核对，关闭订单').click();
    await page.getByRole('dialog').waitFor({ state: 'hidden' });
    assert.equal(await page.evaluate(key => localStorage.getItem(key), planOrderKey), null);
    await page.getByText('订单 order-1 · 流水 payment-1', { exact: true }).waitFor();
    const originalExpiry = state.customer_plans[0].rental.expires_at;

    const payment1Key = 'chain-adjustment:payment-1';
    await button('退款 / 纠错').click();
    await page.getByLabel('本次实际退款 · 元', { exact: true }).fill('5');
    await page.getByLabel('代理权益处理', { exact: true }).selectOption('keep');
    await page.getByLabel('原因 / 凭据', { exact: true }).fill('线下退款凭据 A');
    await page.getByLabel(/确认已经实际完成线下退款，保留原代理权益/).check();
    await button('确认追加记录').click();
    await page.getByRole('dialog').getByText(/该请求结果尚需核对/).waitFor();
    await page.getByRole('dialog').getByRole('alert').filter({ hasText: /fetch|请求|网络/i }).last().waitFor();
    await close();
    const adjustmentsBeforeRecovery = writes.filter(write => write.path === '/api/rentals/adjustments').length;

    state.payments.push({ ...state.payments[0], id: 'payment-2', request_id: 'another-order', line_id: 'old-line', line_name: '已有独立线路', amount_fen: 7900 });
    await page.reload();
    await page.getByRole('row').filter({ hasText: '流水 payment-2' }).getByRole('button', { name: '退款 / 纠错' }).click();
    const otherDialog = page.getByRole('dialog');
    await otherDialog.getByText(/当前可退/).waitFor();
    assert.equal(await otherDialog.getByRole('button', { name: '确认追加记录' }).count(), 1, 'payment-1 待核对回执不得锁死无关收款 payment-2');
    await close();

    await page.getByRole('row').filter({ hasText: '订单 order-1' }).getByRole('button', { name: '退款 / 纠错' }).click();
    await page.getByText(/^已登记 人工退款/).waitFor();
    await page.getByText('原收款流水 payment-1', { exact: true }).waitFor();
    await page.getByText('调整流水 adjustment-1', { exact: true }).waitFor();
    assert.equal(writes.filter(write => write.path === '/api/rentals/adjustments').length, adjustmentsBeforeRecovery);
    await page.evaluate(key => localStorage.setItem(key, 'newer-request'), payment1Key);
    await button('已核对原操作，关闭').click();
    await page.getByRole('dialog').waitFor({ state: 'hidden' });
    assert.equal(await page.evaluate(key => localStorage.getItem(key), payment1Key), 'newer-request', '旧回执不可清除后来登记的请求');
    assert.equal(state.lines[0].enabled, true, '仅退款不自动停用权益');
    assert.equal(state.customer_plans[0].rental.expires_at, originalExpiry);

    await page.getByRole('row').filter({ hasText: '订单 order-1' }).getByRole('button', { name: '退款 / 纠错' }).click();
    await button('已核对，重新填写').click();
    assert.ok(reads.includes('/api/rentals/adjustments/by-request/newer-request'));
    assert.equal(await page.evaluate(key => localStorage.getItem(key), payment1Key), null);
    await page.getByLabel('本次实际退款 · 元', { exact: true }).fill('5');
    await page.getByLabel(/确认已经实际完成线下退款，保留原代理权益/).check();
    await page.getByLabel('代理权益处理', { exact: true }).selectOption('disable');
    const confirmDisable = page.getByLabel(/确认已经实际完成线下退款，并提交停用所选关联线路的请求/);
    assert.equal(await confirmDisable.isChecked(), false, '扩大操作范围须重新确认');
    await page.getByRole('dialog').getByText('新草稿线路 · draft-a', { exact: true }).waitFor();
    await page.getByLabel('原因 / 凭据', { exact: true }).fill('线下退款凭据 B，并停用套餐');
    await confirmDisable.check();
    await button('确认追加记录').click();
    await page.getByRole('dialog').waitFor({ state: 'hidden' });
    const refunds = writes.filter(write => write.path === '/api/rentals/adjustments');
    assert.equal(refunds.length, 2);
    assert.equal(refunds[0].body.amount_fen, 500);
    assert.equal(refunds[0].body.disable_line, false);
    assert.equal(refunds[1].body.disable_line, true);
    assert.deepEqual(refunds[1].body.expected_line_ids, ['draft-a']);
    assert.notEqual(refunds[0].body.request_id, refunds[1].body.request_id);
    assert.equal(state.lines[0].enabled, false);
    assert.equal(state.lines[1].enabled, true, '不影响旧独立线路');
    assert.equal(state.customer_plans[0].rental.expires_at, originalExpiry);
    await page.getByText(/已提交停用请求，当前状态见代理列表/).first().waitFor();
    assert.equal(await page.getByText(/关联代理当前已同步停用/).count(), 0);
    state.lines[0].sync_status = 'synced';
    await page.reload();
    await page.getByText(/关联代理当前已同步停用/).first().waitFor();
    assert.deepEqual(errors, []);
    const downloading = page.waitForEvent('download');
    await button('导出筛选后的流水 CSV').click();
    const download = await downloading;
    const chunks = [];
    for await (const chunk of await download.createReadStream()) chunks.push(chunk);
    const csv = Buffer.concat(chunks).toString('utf8');
    assert.ok(csv.includes('已提交停用关联代理请求'), '历史导出只记录当时的操作');
    assert.ok(csv.includes('"-5.00"'), '退款导出为可参与对账求和的数值');
    assert.ok(!csv.includes('"\'-5.00"'), '内部生成的数值无需按外部文本转义');
    await download.delete();

    const batchLines = ['线路甲', '线路乙', '线路丙'].map((name, i) => ({
      ...line(`independent-${i}`, name, false), rental: state.customer_plans[0].rental,
    }));
    state.lines.push(...batchLines);
    const previousOrder = state.rental_orders[0];
    const batch = { ...previousOrder, id: 'pending-batch', request_id: 'batch-request',
      confirmed_at: 0, confirmed_by: null,
      items: batchLines.slice(0, 2).map(l => ({ ...previousOrder.items[0], line_id: l.id, line_name: l.name })) };
    state.rental_orders.push(batch);
    await page.evaluate(id => localStorage.setItem('chain-order:independent-0,independent-1', id), batch.request_id);
    await page.goto('http://chain.test/admin#lines');
    await page.reload();
    const writesBeforeReview = writes.length;
    await page.getByRole('row').filter({ hasText: '线路丙' }).getByRole('button', { name: '收款 / 续费', exact: true }).click();
    await page.getByRole('dialog').getByRole('button', { name: '保存待收款订单', exact: true }).waitFor();
    await close();
    await page.getByLabel('选择 线路甲', { exact: true }).check();
    await page.getByLabel('选择 线路乙', { exact: true }).check();
    await page.getByLabel('批量操作').getByRole('button', { name: '收款 / 续费', exact: true }).click();
    const batchDialog = page.getByRole('dialog');
    await batchDialog.getByRole('heading', { name: '核对开通 / 续费订单', exact: true }).waitFor();
    assert.equal(await batchDialog.getByRole('checkbox', { name: /已逐项核对/ }).isChecked(), false);
    await close();
    assert.equal(writes.length, writesBeforeReview, '核对已有待收款订单不得重复下单或确认收款');

    const adminReads = reads.filter(p => p === '/api/state').length;
    const queryToken = 'a'.repeat(64);
    const query = await context.newPage();
    await query.clock.install();
    await query.goto(`http://chain.test/query#token=${queryToken}`);
    await query.getByRole('heading', { name: '自用 C', exact: true }).waitFor();
    assert.equal(await query.getByText('无限期', { exact: true }).count(), 1);
    assert.equal(await query.getByRole('button', { name: '复制续费信息', exact: true }).count(), 0);
    assert.deepEqual(requests.filter(r => r.path === '/api/public/lookup').at(-1).data, { token: queryToken });
    await query.getByRole('button', { name: '使用代理配置查询', exact: true }).click();
    const config = 'proxies:\n  - {name: one}\n  - {name: two}\n  - {name: unmatched}';
    await query.getByLabel('代理链接或 Mihomo 配置').fill(config);
    await query.getByRole('button', { name: '查询服务', exact: true }).click();
    await query.getByText(/以下配置未匹配本平台出租服务：第 3 个代理/).waitFor();
    assert.equal(await query.getByRole('heading', { name: '东京 A', exact: true }).count(), 1);
    assert.equal(await query.getByRole('heading', { name: '自用 C', exact: true }).count(), 1);
    assert.equal(await query.getByRole('button', { name: '复制续费信息', exact: true }).count(), 1);
    await query.getByLabel('代理链接或 Mihomo 配置').fill('not-submitted');
    const poll = query.waitForResponse(response => new URL(response.url()).pathname === '/api/public/lookup');
    await query.clock.fastForward(10001);
    assert.deepEqual((await poll).request().postDataJSON(), { config });
    await query.evaluate(() => { Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' }); document.dispatchEvent(new Event('visibilitychange')); });
    const beforeHidden = requests.filter(r => r.path === '/api/public/lookup').length;
    await query.clock.fastForward(30000);
    assert.equal(requests.filter(r => r.path === '/api/public/lookup').length, beforeHidden);
    assert.equal(reads.filter(p => p === '/api/state').length, adminReads, 'public query must not request admin state');
    await query.setViewportSize({ width: 390, height: 844 });
    assert.equal(await query.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, 'query mobile overflow');
    await context.close();
    assert.deepEqual(errors, []);
    console.log('PASS customer plan draft binding, per-target idempotent recovery, explicit refund scope, preserved independent rights and public query isolation');
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
