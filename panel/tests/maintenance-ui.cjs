// Mock every request; this test never upgrades or restarts a real service.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(process.env.CHAIN_TEST_BROWSER);
const assets = path.resolve(process.argv[2]);
(async () => {
  const server = await chromium.launchServer({ headless: true, executablePath: process.env.CHAIN_TEST_CHROMIUM });
  fs.writeFileSync(path.join(process.env.CHAIN_TEST_ROOT, 'maintenance-browser.json'), JSON.stringify({ pid: process.pid, browser: server.process().pid, endpoint: server.wsEndpoint() }));
  try {
    const browser = await chromium.connect(server.wsEndpoint());
    const page = await browser.newPage();
    page.setDefaultTimeout(10000);
    const errors = [], writes = [];
    page.on('pageerror', error => errors.push(error.message));
    const state = { version: '1.2.3', agent_version: '1.2.2', maintenance: false, maintenance_actions: [], cores: {xray:'26.3.27',mihomo:'1.19.32'},
      local_node_id:null,nodes:[],lines:[],externals:[],checks:{},jobs:[],busy:false,time:1,subscriptions:[],security:{totp_enabled:false,recovery_remaining:0,origin:'',passkeys:[]},alerts:[],customers:[],payments:[],renewal_contact:'' };
    const fleet = {remotes:[{id:'vps-a',name:'测试 VPS',url:'https://vps.example',seen:Date.now()/1000,error:'',snapshot:{...state,maintenance:true}}],tasks:{},controller:'',policy:null};
    let job, submitting = false, earlyPoll = false, loseResponse = true, failLatest = false, statusOffline = false;
    await page.route('**/*', async route => {
      const req = route.request(), url = new URL(req.url());
      if (url.hostname !== 'chain.test') return route.abort();
      if (url.pathname.startsWith('/api/')) {
        let result = {};
        if (req.method() !== 'GET') writes.push({path:url.pathname,body:req.postDataJSON()});
        if (url.pathname === '/api/state') result = state;
        else if (url.pathname === '/api/fleet') result = fleet;
        else if (url.pathname === '/api/fleet/release/latest') {
          if (failLatest) return route.fulfill({status:502,json:{error:'发行版本暂不可用'}});
          result = {version:'1.2.4',xray:'26.3.27',mihomo:'1.19.32'};
        } else if (url.pathname === '/api/federation/task') {
          if (req.method() === 'POST') {
            submitting = true;
            await new Promise(resolve => setTimeout(resolve, 200));
            job = {...req.postDataJSON(),state:'running'};
            submitting = false;
            if (loseResponse) { statusOffline = true; return route.abort(); }
            result = job;
          } else {
            earlyPoll ||= submitting;
            if (statusOffline) return route.abort();
            assert.equal(url.searchParams.get('id'), job.id);
            result = job;
          }
        } else if (url.pathname === '/api/fleet/batch') {
          const body=req.postDataJSON(); fleet.tasks[body.id]={...body,state:'succeeded',targets:body.targets.map(id=>({id,state:'succeeded'}))}; result={id:body.id};
        } else if (url.pathname === '/api/auth/options') result = {totp:false,passkey:false};
        else throw new Error('Unexpected API: '+url.pathname);
        return route.fulfill({json:result});
      }
      const file=path.join(assets,url.pathname==='/admin'?'index.html':url.pathname);
      return route.fulfill({body:fs.readFileSync(file),contentType:{'.html':'text/html','.js':'text/javascript','.css':'text/css'}[path.extname(file)]||'application/octet-stream'});
    });
    const button = name => page.getByRole('button',{name,exact:true});
    const confirm = () => page.getByRole('dialog').getByRole('button',{name:'确认执行',exact:true}).click();
    await page.goto('http://chain.test/admin#settings');
    await page.getByText(/当前部署未提供网页维护/).waitFor();
    assert.equal(await button('升级并自动重启').isDisabled(),true);
    assert.equal(await button('整套重启').isDisabled(),true);
    assert.match(await page.locator('footer').innerText(),/v1.2.3/);
    await page.getByText('面板与 Agent 版本不一致',{exact:true}).waitFor();
    state.maintenance=true; state.maintenance_actions=['upgrade','restart-all'];
    await page.reload();
    await button('升级并自动重启').click(); await confirm();
    await page.getByText(/暂时无法查询/).waitFor();
    assert.equal(earlyPoll,false);
    assert.equal(writes.filter(w=>w.path==='/api/federation/task').length,1);
    const id=job.id;
    assert.equal(job.version,'1.2.4');
    job.state='succeeded'; job.after={panel:'1.2.4',agent:'1.2.4'};
    state.version=state.agent_version='1.2.4'; statusOffline=false;
    await page.reload();
    await button('刷新页面使用新版界面').waitFor();
    assert.equal(job.id,id);
    assert.equal(writes.filter(w=>w.path==='/api/federation/task').length,1);
    loseResponse=false;
    await button('整套重启').click(); await confirm();
    await page.getByText('维护任务：执行中',{exact:true}).waitFor();
    assert.equal(job.action,'restart-all');
    job.state='failed'; job.error='模拟服务启动失败';
    await page.getByText('模拟服务启动失败',{exact:true}).waitFor();
    failLatest=true; await button('检查最新版本').click();
    await page.getByText('发行版本暂不可用',{exact:true}).waitFor();
    assert.equal(await button('升级并自动重启').isDisabled(),true);
    failLatest=false;
    await page.goto('http://chain.test/admin#fleet');
    await page.getByRole('navigation',{name:'总控功能'}).getByRole('button',{name:'VPS',exact:true}).click();
    await page.getByLabel('选择 VPS 测试 VPS',{exact:true}).check();
    await button('整套重启').click();
    await page.getByRole('dialog').getByText(/请先升级以支持整套重启/).waitFor();
    assert.equal(await page.getByRole('dialog').getByRole('button',{name:'确认执行',exact:true}).count(),0);
    await page.getByRole('dialog').getByRole('button',{name:'关闭',exact:true}).first().click();
    fleet.remotes[0].snapshot.maintenance_actions=['restart-all'];
    await button('刷新总控').click(); await button('整套重启').click(); await confirm();
    await page.getByRole('heading',{name:'逐台任务进度',exact:true}).waitFor();
    assert.equal(writes.find(w=>w.path==='/api/fleet/batch').body.action,'restart-all');
    assert.deepEqual(errors,[]);
    await browser.close();
    console.log('Maintenance UI checks passed');
  } finally { await server.close(); }
})().catch(error => { console.error(error); process.exitCode=1; });
