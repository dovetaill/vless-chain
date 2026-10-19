// Mocked control-plane UI checks: no panel, Agent, SMTP or Telegram traffic.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(process.env.CHAIN_TEST_BROWSER);
const assets = path.resolve(process.argv[2]);
const root = process.env.CHAIN_TEST_ROOT;
(async () => {
  fs.writeFileSync(path.join(root, 'management-browser.json'), JSON.stringify({ pid: process.pid, cwd: process.cwd(), artifacts: root }));
  const server = await chromium.launchServer({ headless: true, executablePath: process.env.CHAIN_TEST_CHROMIUM });
  fs.writeFileSync(path.join(root, 'management-browser.json'), JSON.stringify({ pid: process.pid, browser: server.process().pid, endpoint: server.wsEndpoint(), artifacts: root }));
  const browser = await chromium.connect(server.wsEndpoint());
  try {
    const page = await browser.newPage();
    const errors = [], writes = [];
    page.on('pageerror', e => errors.push(e.message));
    page.on('dialog', dialog => dialog.accept());
    const state = { nodes: [], lines: [], externals: [], checks: {}, jobs: [], busy: false, time: 1, subscriptions: [], security: { totp_enabled: false, recovery_remaining: 0, origin: '', passkeys: [] }, alerts: [], customers: [], payments: [], renewal_contact: '', pending_imports: [] };
    const policy = { enabled: true, interval: 300, failures: 3, recoveries: 2, offline: 60, reminder: 86400, url: 'https://example.com', quota: [80,95,100], expiry_days: [7,3,1,0] };
    const monitor = { local: policy, effective: policy, override: null, controller: '', channels: { telegram_token:'',telegram_chat:'',smtp_host:'',smtp_port:465,smtp_starttls:false,smtp_user:'',smtp_password:'',mail_from:'',mail_to:'' }, telegram_set:true,password_set:false,notices:[] };
    const fleet = { remotes: [{ id:'vps-a',name:'VPS A',url:'https://a.example',seen:1,error:'',snapshot:{...state,version:'0.2.29',maintenance:true,externals:[{id:'ext-a',name:'外部落地',url:'socks5://a:b@example.com:1080'}]} }],controller:'',policy:null,tasks:{} };
    await page.route('**/*', async route => {
      const request = route.request(), url = new URL(request.url());
      if (url.hostname !== 'chain.test') return route.abort();
      if (url.pathname.startsWith('/api/')) {
        let result = {};
        if (request.method() !== 'GET') writes.push({ path:url.pathname,body:request.postDataJSON() });
        if (url.pathname === '/api/state') result = state;
        else if (url.pathname === '/api/auth/options') result = {totp:false,passkey:false};
        else if (url.pathname === '/api/monitoring') result = monitor;
        else if (url.pathname === '/api/fleet') result = fleet;
        else if (url.pathname === '/api/fleet/invite') result = {code:'once-only-code'};
        else if (url.pathname === '/api/fleet/vps-a/forward') {
          const body = request.postDataJSON();
          if (body.path === '/state') result = {...state,externals:fleet.remotes[0].snapshot.externals};
          else if (body.path === '/externals/ext-a') result = {id:'ext-a',...body.body};
          else if (body.path === '/preview') result = {affected_line_ids:[],affected_lines:[],nodes:[],message:''};
          else if (body.path !== '/publish') throw new Error('Unexpected remote operation: '+body.path);
        } else if (!['/api/fleet/policy','/api/fleet/batch','/api/monitoring/test','/api/federation/login'].includes(url.pathname)) throw new Error('Unexpected API: '+url.pathname);
        return route.fulfill({json:result});
      }
      const file = path.join(assets, url.pathname === '/admin' ? 'index.html' : url.pathname);
      return route.fulfill({body:fs.readFileSync(file),contentType:{'.html':'text/html','.js':'text/javascript','.css':'text/css'}[path.extname(file)] || 'application/octet-stream'});
    });
    await page.goto('http://chain.test/admin#monitoring');
    await page.getByRole('button',{name:'保存配置',exact:true}).waitFor();
    await page.getByLabel('检测间隔（秒）',{exact:true}).fill('600');
    await page.getByRole('button',{name:'保存配置',exact:true}).click();
    await page.getByText('配置已保存',{exact:true}).waitFor();
    assert.equal(writes.find(w => w.path === '/api/monitoring').body.local.interval,600);
    assert.equal(writes.find(w => w.path === '/api/monitoring').body.channels.telegram_token,'');
    await page.locator('aside nav').getByRole('button',{name:'多机总控',exact:true}).click();
    await page.getByText('接入与授权',{exact:true}).waitFor();
    await page.getByLabel('覆盖所有已接入 VPS 的本地策略').check();
    await page.getByRole('button',{name:'保存总控策略',exact:true}).click();
    await page.waitForTimeout(50);
    assert.equal(writes.find(w => w.path === '/api/fleet/policy').body.interval,300);
    await page.getByRole('button',{name:'生成本机接入码（10 分钟有效）',exact:true}).click();
    await page.getByText('once-only-code',{exact:true}).waitFor();
    await page.getByRole('checkbox',{name:/VPS A/}).check();
    await page.getByRole('button',{name:'重启面板',exact:true}).click();
    await page.getByText('任务已提交；请查看逐台结果。暂停后先确认实际状态，再选择失败机器重新提交。',{exact:true}).waitFor();
    const batch = writes.find(w => w.path === '/api/fleet/batch').body;
    assert.equal(batch.action,'restart-panel');
    assert.deepEqual(batch.targets,['vps-a']);
    assert.match(batch.id,/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    await page.getByRole('link',{name:'管理外部代理',exact:true}).click();
    await page.getByRole('button',{name:'编辑',exact:true}).click();
    await page.getByRole('dialog').getByLabel('名称',{exact:true}).fill('修改后的落地');
    await page.getByRole('dialog').getByRole('button',{name:'保存',exact:true}).click();
    await page.waitForFunction(() => !document.querySelector('[role=dialog]'));
    const mutation = writes.find(w => w.path === '/api/fleet/vps-a/forward' && w.body.path === '/externals/ext-a');
    assert.equal(mutation.body.body.name,'修改后的落地');
    assert.equal(await page.locator('aside nav button').count(),2);
    assert.ok(writes.some(w => w.body?.path === '/publish'));
    await page.goto('http://chain.test/admin#fleet-ticket=one-time-ticket');
    await page.locator('aside').waitFor();
    assert.ok(!page.url().includes('ticket'));
    assert.equal(writes.find(w => w.path === '/api/federation/login').body.ticket,'one-time-ticket');
    assert.deepEqual(errors,[]);
    console.log('management UI checks passed');
  } finally {
    await browser.close();
    await server.close();
  }
})().catch(error => { console.error(error); process.exitCode=1; });
