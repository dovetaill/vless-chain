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
    let coreRequests = 0;
    let job, submitting = false, earlyPoll = false, loseResponse = true, failLatest = false, statusOffline = false;
    await page.route('**/*', async route => {
      const req = route.request(), url = new URL(req.url());
      if (url.hostname !== 'chain.test') return route.abort();
      if (url.pathname.startsWith('/api/')) {
        let result = {};
        if (req.method() !== 'GET') writes.push({path:url.pathname,body:req.postDataJSON()});
        if (url.pathname === '/api/state') result = state;
        else if (url.pathname === '/api/fleet') result = fleet;
        else if (url.pathname === '/api/security/members') result = {members:[]};
        else if (url.pathname.startsWith('/api/fleet/cores/')) { coreRequests++; result = {recommended:'26.10.21',versions:[{version:'26.10.21',prerelease:false}]}; }
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
        } else if (url.pathname === '/api/fleet/vps-a/forward') {
          const body=req.postDataJSON();assert.equal(body.path,'/federation/task');
          assert.equal(body.body.action,'core-confirm');
          const task=Object.values(fleet.tasks).find(t=>t.action==='core-upgrade');
          assert.equal(body.body.task_id,task.targets[0].task_id);assert.equal(body.body.core,'xray');
          task.targets[0].state='succeeded';task.state='succeeded';result={...body.body,state:'succeeded'};
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
    assert.equal(await button('升级并重启').isDisabled(),true);
    await page.getByText('其他维护',{exact:true}).click();
    assert.equal(await button('整套重启').isDisabled(),true);
    assert.match(await page.locator('footer').innerText(),/v1.2.3/);
    await page.getByText('面板与 Agent 版本不一致',{exact:true}).waitFor();
    state.maintenance=true; state.maintenance_actions=['upgrade','restart-all'];
    await page.reload();
    await page.getByText('可升级至 v1.2.4',{exact:true}).waitFor();
    await button('升级并重启').click();
    assert.equal(await page.getByRole('dialog').count(),0);
    await page.getByText(/暂时无法查询/).waitFor();
    assert.equal(earlyPoll,false);
    assert.equal(writes.filter(w=>w.path==='/api/federation/task').length,1);
    const id=job.id;
    assert.equal(job.version,'1.2.4');
    assert.equal(job.xray_channel,undefined);
    job.state='succeeded'; job.after={panel:'1.2.4',agent:'1.2.4'};
    state.version=state.agent_version='1.2.4'; statusOffline=false;
    await page.reload();
    await button('刷新页面使用新版界面').waitFor();
    assert.equal(job.id,id);
    assert.equal(writes.filter(w=>w.path==='/api/federation/task').length,1);
    loseResponse=false;
    await page.getByText('已是最新正式版',{exact:true}).waitFor();
    assert.equal(await button('升级并重启').isDisabled(),true);
    assert.equal(coreRequests,0);
    assert.equal(await page.getByRole('region',{name:'版本与维护',exact:true}).getByRole('combobox').count(),0);
    const components=page.getByRole('definition').filter({hasText:'26.3.27'});
    assert.equal(await components.count(),1);
    job={id:'00000000-0000-4000-8000-000000000002',action:'core-upgrade',core:'xray',state:'awaiting_verification',core_version:'26.10.21'};
    await page.evaluate(id=>localStorage.setItem('chain-maintenance-task',id),job.id);
    await page.reload();
    await button('原链接已验证通过').waitFor();
    await button('原链接已验证通过').click();await confirm();
    await page.getByText('维护任务：执行中',{exact:true}).waitFor();
    assert.equal(job.action,'core-confirm');
    job.state='succeeded';
    await page.getByText('其他维护',{exact:true}).click();
    await button('整套重启').waitFor();
    await page.waitForFunction(() => !Array.from(document.querySelectorAll('button')).find(b=>b.textContent==='整套重启')?.disabled);
    await button('整套重启').click(); await confirm();
    await page.getByText('维护任务：执行中',{exact:true}).waitFor();
    assert.equal(job.action,'restart-all');
    job.state='failed'; job.error='模拟服务启动失败';
    await page.getByText('模拟服务启动失败',{exact:true}).waitFor();
    failLatest=true; await button('刷新版本信息').click();
    await page.getByText('发行版本暂不可用',{exact:true}).waitFor();
    assert.equal(await button('升级并重启').isDisabled(),true);
    failLatest=false;
    await page.goto('http://chain.test/admin#fleet');
    await page.getByRole('navigation',{name:'总控功能'}).getByRole('button',{name:'VPS',exact:true}).click();
    await page.getByLabel('选择 VPS 测试 VPS',{exact:true}).check();
    await page.getByText('重启与维护说明',{exact:true}).click();
    await button('整套重启').click();
    await page.getByRole('dialog').getByText(/请先升级以支持整套重启/).waitFor();
    assert.equal(await page.getByRole('dialog').getByRole('button',{name:'确认执行',exact:true}).count(),0);
    await page.getByRole('dialog').getByRole('button',{name:'关闭',exact:true}).first().click();
    fleet.remotes[0].snapshot.maintenance_actions=['restart-all'];
    await button('刷新总控').click(); await button('整套重启').click(); await confirm();
    await page.getByRole('heading',{name:'批量任务进度',exact:true}).waitFor();
    assert.equal(writes.find(w=>w.path==='/api/fleet/batch').body.action,'restart-all');
    await page.getByRole('dialog').getByRole('button',{name:'关闭',exact:true}).first().click();
    await page.getByLabel('官方版本',{exact:true}).selectOption('26.10.21');
    await button('逐台升级所选 VPS 的 xray').click();
    assert.equal(await page.getByRole('dialog').getByRole('button',{name:'确认执行',exact:true}).count(),0);
    await page.getByRole('dialog').getByRole('button',{name:'关闭',exact:true}).first().click();
    fleet.remotes[0].snapshot.maintenance_actions.push('core-upgrade');
    await button('刷新总控').click(); await button('逐台升级所选 VPS 的 xray').click(); await confirm();
    await page.getByRole('heading',{name:'批量任务进度',exact:true}).waitFor();
    const coreBatch=writes.filter(w=>w.path==='/api/fleet/batch').at(-1).body;
    assert.equal(coreBatch.action,'core-upgrade');assert.equal(coreBatch.core,'xray');assert.equal(coreBatch.version,'26.10.21');
    assert.equal(coreBatch.security_update,true);
    const pending=fleet.tasks[coreBatch.id];pending.state='running';
    pending.targets[0]={id:'vps-a',state:'awaiting_verification',task_id:'00000000-0000-4000-8000-000000000001'};
    await button('原链接已验证，继续').waitFor();
    page.once('dialog', dialog=>dialog.accept());
    await button('原链接已验证，继续').click();
    await page.waitForFunction(()=>!Array.from(document.querySelectorAll('button')).some(b=>b.textContent==='原链接已验证，继续'));
    assert.equal(writes.filter(w=>w.path==='/api/fleet/vps-a/forward').length,1);
    assert.deepEqual(errors,[]);
    await browser.close();
    console.log('Maintenance UI checks passed');
  } finally { await server.close(); }
})().catch(error => { console.error(error); process.exitCode=1; });
