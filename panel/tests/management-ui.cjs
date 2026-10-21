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
  fs.writeFileSync(path.join(root, 'management-browser.json'), JSON.stringify({ pid: process.pid, browser: server.process().pid, pgid: Number(fs.readFileSync('/proc/self/stat','utf8').split(') ')[1].split(' ')[2]), endpoint: server.wsEndpoint(), artifacts: root }));
  const browser = await chromium.connect(server.wsEndpoint());
  try {
    const page = await browser.newPage();
    page.setDefaultTimeout(10000);
    const errors = [], writes = [];
    page.on('pageerror', e => errors.push(e.message));
    let acceptDialog = true;
    page.on('dialog', dialog => acceptDialog ? dialog.accept() : dialog.dismiss());
    const state = { nodes: [], lines: [], externals: [], checks: {}, jobs: [], busy: false, time: 1, subscriptions: [], security: { totp_enabled: false, recovery_remaining: 0, origin: '', passkeys: [] }, alerts: [], customers: [], payments: [], renewal_contact: '', pending_imports: [] };
    const policy = { enabled: true, interval: 300, failures: 3, recoveries: 2, offline: 60, reminder: 86400, url: 'https://example.com', quota: [80,95,100], expiry_days: [7,3,1,0] };
    const monitor = { local: policy, effective: policy, override: null, controller: '', channels: { telegram_token:'',telegram_chat:'',smtp_host:'',smtp_port:465,smtp_starttls:false,smtp_user:'',smtp_password:'',mail_from:'',mail_to:'' }, telegram_set:true,password_set:false,notices:[] };
    const now = Math.floor(Date.now()/1000);
    const line = (i) => ({id:`line-${i}`,name:`代理 ${i}`,kind:'vless',hops:[{node_id:'local',port:10000+i}],enabled:true,published:true,external_id:null,chain:[],target:'',network:'tcp',share:null,
      access:{host:'192.0.2.1',protocol:'vless',username:'',password:'',method:'',public_host:'',public_port:null,shared_port:false,fallback:'',transport:{network:'tcp',path:'/',host:'',mode:'auto'},reality:{target:'www.microsoft.com:443',server_name:'www.microsoft.com'},tls:{mode:'off',domain:'',cert:'',key:''}},
      limits:{upload_mbps:null,download_mbps:null,monthly_gib:100},traffic:{epoch:'',month:'',upload:1024,download:2048,total_upload:1024,total_download:2048,upload_bps:100,download_bps:200,connections:1,sampled_at:now,blocked:false,days:{},cycle_start:now-100,cycle_end:now+100},rental:null,rental_status:'active',sync_status:'synced'});
    const fleet = {remotes:Array.from({length:50},(_,i)=>({id:`vps-${i}`,name:`VPS ${String(i).padStart(2,'0')}`,url:`https://vps-${i}.example`,seen:now,error:'',snapshot:{...state,time:now,local_node_id:'local',version:'0.2.29',agent_version:i===0?'0.2.28':'0.2.29',agent_seen:now,maintenance:true,
      nodes:[{id:'local',name:'本机',host:'192.0.2.1',seen:now,online:true,report:{running:true,applied:true,error:'',revision:'',traffic:{},system:{sampled_at:now,network_rx_bps:1024,network_tx_bps:2048,cpu_percent:10,memory_total:4096,memory_used:1024}}}],
      lines:Array.from({length:10},(_,j)=>line(j)),externals:[{id:'ext-a',name:'外部落地',url:'socks5://a:b@example.com:1080'}]}})),controller:'',policy:null,tasks:{}};
    let failSave = false, failLatest = false, disconnected = false;

    await page.route('**/*', async route => {
      const request = route.request(), url = new URL(request.url());
      if (url.hostname !== 'chain.test') return route.abort();
      if (url.pathname.startsWith('/api/')) {
        if(disconnected) return route.abort('failed');
        let result = {};
        if (request.method() !== 'GET') writes.push({ path:url.pathname,body:request.postDataJSON() });
        if (url.pathname === '/api/state') result = state;
        else if (url.pathname === '/api/auth/options') result = {totp:false,passkey:false};
        else if (url.pathname === '/api/monitoring') result = monitor;
        else if (url.pathname === '/api/fleet') result = fleet;
        else if (url.pathname === '/api/fleet/release/latest') { if(failLatest) return route.fulfill({status:502,json:{error:'发行版本暂不可用'}}); result = {version:'0.2.30',xray:'26.3.27',mihomo:'1.19.32'}; }
        else if (url.pathname === '/api/fleet/invite') result = {code:'once-only-code'};
        else if (/^\/api\/fleet\/vps-\d+\/forward$/.test(url.pathname)) {
          const body = request.postDataJSON(), remote = fleet.remotes.find(r=>url.pathname.includes(`/${r.id}/`));
          if (body.path === '/state') result = remote.snapshot;
          else if (body.path === '/diagnostics/local/logs') result={text:'remote-agent-log',sampled_at:now,sources:['xray'],detail:''};
          else if (body.path === '/externals/ext-a') { const external=remote.snapshot.externals[0];Object.assign(external,body.body);result=external; }
          else if (body.path === '/lines') {
            if(failSave) return route.fulfill({status:400,json:{error:'模拟保存失败'}});
            const old=remote.snapshot.lines.find(l=>l.id===body.body.id);Object.assign(old,body.body);result=old;
          }
          else if (body.path === '/diagnostics/local/reality') result={ok:true,detail:'通过',latency_ms:10};
          else if (body.path === '/preview') result = {affected_line_ids:['line-0','line-1'],affected_lines:['代理 0','代理 1'],nodes:[],message:'影响两条代理'};
          else if (body.path !== '/publish') throw new Error('Unexpected remote operation: '+body.path);
        } else if(url.pathname==='/api/fleet/batch') {
          const body=request.postDataJSON(); fleet.tasks[body.id]={...body,at:now,state:'paused',targets:body.targets.map(id=>({id,state:'pending'}))}; result={id:body.id};
        } else if(url.pathname.endsWith('/archive')) {
          fleet.tasks[url.pathname.split('/').at(-2)].archived=true;
        } else if(url.pathname.endsWith('/resume')) {
          const id=url.pathname.split('/').at(-2); fleet.tasks[id].state='succeeded';fleet.tasks[id].targets.forEach(t=>{t.state='succeeded';delete t.error;if(t.result){delete t.result.error;if(t.result.verification)t.result.verification={state:'succeeded'};}});
        } else if(/^\/api\/fleet\/batch\/[^/]+\/verify\/[^/]+$/.test(url.pathname)) {
          const parts=url.pathname.split('/'), target=fleet.tasks[parts[4]].targets.find(t=>t.id===parts[6]);
          target.result.verification={state:'pending',retry_at:0};
        } else if (!['/api/fleet/policy','/api/monitoring/test','/api/federation/login'].includes(url.pathname)) throw new Error('Unexpected API: '+url.pathname);
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
    const nav=page.getByRole('navigation',{name:'总控功能'});
    const go=async name=>nav.getByRole('button',{name,exact:true}).click();
    const dialog=()=>page.getByRole('dialog');
    const closeDialog=async()=>{await dialog().getByRole('button',{name:'关闭',exact:true}).first().click();await dialog().waitFor({state:'hidden'});};
    await page.getByText('VPS 状态',{exact:true}).waitFor();
    await page.getByText('↑ 100.0 KiB/s · 有效 50/50 台',{exact:true}).waitFor();
    assert.equal(await page.getByText('接入与授权',{exact:true}).count(),0);
    await go('VPS');
    assert.equal(await page.getByRole('button',{name:'批量升级',exact:true}).count(),0);
    assert.equal(await page.getByLabel('目标正式版本').count(),0);
    await page.getByRole('region',{name:'VPS 列表',exact:true}).getByText('版本不一致',{exact:true}).waitFor();
    assert.equal(await page.getByRole('region',{name:'VPS 列表',exact:true}).locator('tbody tr').count(),20);
    await page.getByLabel('选择 VPS VPS 00',{exact:true}).check();
    await page.getByText('最新正式版 0.2.30',{exact:true}).waitFor();
    assert.equal(await page.getByRole('button',{name:'重启面板',exact:true}).isVisible(),false,'Low frequency maintenance starts collapsed');
    await page.getByRole('button',{name:'下一页',exact:true}).click();
    await page.getByLabel('选择 VPS VPS 20',{exact:true}).check();
    await go('所有代理');await go('VPS');await page.getByText('共 50 条 · 每页 20 条 · 第 2 / 3 页',{exact:true}).waitFor();
    await page.getByLabel('搜索总控列表').fill('VPS 49');
    if(!await page.getByRole('button',{name:'重启面板',exact:true}).isVisible()) await page.getByText('重启与维护说明',{exact:true}).click();
    await page.getByRole('button',{name:'重启面板',exact:true}).click();
    await dialog().getByText('VPS 00',{exact:true}).waitFor();await dialog().getByText('VPS 20',{exact:true}).waitFor();
    await dialog().getByRole('button',{name:'确认执行',exact:true}).click();
    await dialog().getByRole('heading',{name:'批量任务进度'}).waitFor();
    const batch = writes.find(w=>w.path==='/api/fleet/batch').body;
    assert.equal(batch.action,'restart-panel');assert.deepEqual(batch.targets,['vps-0','vps-20']);
    assert.match(batch.id,/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    await dialog().getByRole('button',{name:'继续未执行任务',exact:true}).click();
    await dialog().getByText('维护完成 2 / 2 · 验证未通过 0 · 操作失败 0',{exact:true}).waitFor();await closeDialog();
    await page.getByRole('button',{name:'批量升级',exact:true}).click();
    await dialog().getByRole('button',{name:'确认执行',exact:true}).click();
    await dialog().getByRole('heading',{name:'批量任务进度'}).waitFor();await closeDialog();
    assert.equal(writes.find(w=>w.path==='/api/fleet/batch'&&w.body.action==='upgrade').body.version,'0.2.30');
    failLatest=true;await page.getByRole('button',{name:'检查最新版本',exact:true}).click();await page.getByText('发行版本暂不可用',{exact:true}).waitFor();
    assert.equal(await page.getByRole('button',{name:'批量升级',exact:true}).isDisabled(),true);
    failLatest=false;await page.getByRole('button',{name:'检查最新版本',exact:true}).click();
    await page.getByText('最新正式版 0.2.30',{exact:true}).waitFor();
    fleet.remotes[0].snapshot.agent_version='0.2.29';
    await page.getByRole('button',{name:'清除筛选',exact:true}).click();
    await page.getByRole('button',{name:'刷新总控',exact:true}).click();
    await page.getByRole('region',{name:'VPS 列表',exact:true}).getByText('版本不一致',{exact:true}).waitFor({state:'hidden'});
    await page.getByRole('button',{name:'查看 VPS',exact:true}).first().click();
    await dialog().getByRole('button',{name:'日志与诊断',exact:true}).click();
    await dialog().getByText('remote-agent-log',{exact:true}).waitFor();
    assert.ok(writes.some(w=>w.path==='/api/fleet/vps-0/forward'&&w.body.path==='/diagnostics/local/logs'));
    assert.ok(!page.url().includes('remote='));
    await closeDialog();
    fleet.remotes[0].snapshot.agent_version=null;fleet.remotes[0].error='模拟远端失联';
    await page.getByRole('button',{name:'刷新总控',exact:true}).click();
    await page.getByText(/Agent 版本未知（尚未上报）/).waitFor();
    if(!await page.getByRole('button',{name:'重启面板',exact:true}).isVisible()) await page.getByText('重启与维护说明',{exact:true}).click();
    await page.getByRole('button',{name:'重启面板',exact:true}).click();
    await dialog().getByText(/失联或数据过期/).waitFor();assert.equal(await dialog().getByRole('button',{name:'确认执行',exact:true}).count(),0);await closeDialog();
    fleet.remotes[0].error='';fleet.remotes[0].snapshot.agent_version='0.2.28';
    await page.getByRole('button',{name:'刷新总控',exact:true}).click();
    await go('所有代理');
    const rows=page.getByRole('region',{name:'代理列表',exact:true}).locator('tbody tr');
    assert.equal(await rows.count(),20);await page.getByText('共 500 条 · 每页 20 条 · 第 1 / 25 页',{exact:true}).waitFor();
    fleet.remotes[0].snapshot.lines[0].enabled=false;fleet.remotes[0].snapshot.lines[0].sync_status='pending';
    await page.getByRole('button',{name:'刷新总控',exact:true}).click();await rows.first().getByText('停用待生效',{exact:true}).waitFor();
    fleet.remotes[0].snapshot.lines[0].sync_status='error';
    await page.getByRole('button',{name:'刷新总控',exact:true}).click();await rows.first().getByText('应用失败',{exact:true}).waitFor();
    await page.getByLabel('列表排序').selectOption('rate');
    assert.ok(!(await rows.first().innerText()).includes('应用失败'));
    await page.getByLabel('列表排序').selectOption('default');
    fleet.remotes[0].snapshot.lines[0].enabled=true;fleet.remotes[0].snapshot.lines[0].sync_status='synced';
    await page.getByRole('button',{name:'刷新总控',exact:true}).click();

    await page.getByRole('button',{name:'选择全部筛选结果',exact:true}).click();await page.getByText('已选 500 条代理 / 50 台 VPS（含隐藏项）',{exact:true}).waitFor();await page.getByRole('button',{name:'清空选择',exact:true}).click();
    await page.getByLabel('选择代理 VPS 00 / 代理 0',{exact:true}).check();
    await page.getByRole('button',{name:'下一页',exact:true}).click();
    await page.getByLabel('选择代理 VPS 02 / 代理 0',{exact:true}).check();
    await page.getByLabel('搜索总控列表').fill('VPS 49');
    await page.getByRole('button',{name:'停用所选代理',exact:true}).click();
    await dialog().getByText('VPS 00',{exact:true}).waitFor();await dialog().getByText('VPS 02',{exact:true}).waitFor();
    await dialog().getByRole('button',{name:'确认执行',exact:true}).click();await dialog().getByRole('heading',{name:'批量任务进度'}).waitFor();await closeDialog();
    const disable=writes.find(w=>w.path==='/api/fleet/batch'&&w.body.action==='disable').body;
    assert.deepEqual(disable.lines,{'vps-0':['line-0'],'vps-2':['line-0']});
    await page.getByLabel('搜索总控列表').fill('');
    await page.getByLabel('筛选 VPS').selectOption('vps-1');
    await rows.first().getByRole('button',{name:'编辑',exact:true}).click();
    await dialog().getByRole('heading',{name:'编辑代理 · VPS 01'}).waitFor();
    assert.ok(!page.url().includes('remote='));
    await page.waitForFunction(()=>{const r=document.querySelector('[role=dialog]').getBoundingClientRect();return Math.abs(r.right-window.innerWidth)<2 && r.width>600;});
    await dialog().getByRole('button',{name:'在节点验证 REALITY 握手',exact:true}).click();await dialog().getByText('通过：通过（10 ms）',{exact:true}).waitFor();
    await dialog().getByLabel('名称',{exact:true}).fill('原位修改');
    failSave=true;await dialog().getByRole('button',{name:'保存并预览',exact:true}).click();await dialog().getByText('模拟保存失败',{exact:true}).waitFor();
    assert.equal(await dialog().getByLabel('名称',{exact:true}).inputValue(),'原位修改');
    failSave=false;await dialog().getByRole('button',{name:'保存并预览',exact:true}).click();
    await dialog().getByRole('heading',{name:'应用修改 · VPS 01'}).waitFor();
    await closeDialog();await page.getByRole('button',{name:'继续应用',exact:true}).click();await dialog().getByRole('heading',{name:'应用修改 · VPS 01'}).waitFor();
    await dialog().getByRole('button',{name:'确认并应用',exact:true}).click();await dialog().waitFor({state:'hidden'});
    assert.equal(await page.getByLabel('筛选 VPS').inputValue(),'vps-1');
    await rows.first().getByText('原位修改',{exact:true}).waitFor();
    assert.equal(fleet.remotes[0].snapshot.lines[0].name,'代理 0');
    assert.equal(fleet.remotes[1].snapshot.lines[0].name,'原位修改');
    for(const path of ['/lines','/diagnostics/local/reality','/preview','/publish']) assert.ok(writes.some(w=>w.path==='/api/fleet/vps-1/forward'&&w.body.path===path),path);
    assert.equal(writes.filter(w=>['/api/lines','/api/publish','/api/diagnostics/local/reality'].includes(w.path)).length,0);
    await go('外部代理');await page.getByLabel('筛选 VPS').selectOption('vps-2');
    await page.getByRole('region',{name:'外部代理列表'}).getByRole('button',{name:'编辑',exact:true}).click();
    await dialog().getByLabel('名称',{exact:true}).fill('修改后的落地');acceptDialog=false;await page.keyboard.press('Escape');assert.equal(await dialog().getByLabel('名称',{exact:true}).inputValue(),'修改后的落地');acceptDialog=true;await dialog().getByRole('button',{name:'保存',exact:true}).click();
    await dialog().getByRole('heading',{name:'应用修改 · VPS 02'}).waitFor();await dialog().getByRole('button',{name:'确认并应用',exact:true}).click();await dialog().waitFor({state:'hidden'});
    assert.equal(fleet.remotes[2].snapshot.externals[0].name,'修改后的落地');assert.equal(fleet.remotes[1].snapshot.externals[0].name,'外部落地');
    await go('总控设置');await page.getByLabel('覆盖所有已接入 VPS 的本地策略').check();await page.getByRole('button',{name:'保存总控策略',exact:true}).click();
    await page.getByText('总控策略已保存。',{exact:true}).waitFor();assert.equal(writes.find(w=>w.path==='/api/fleet/policy').body.interval,300);
    await page.getByRole('button',{name:'生成本机接入码（10 分钟有效）',exact:true}).click();await page.getByText('once-only-code',{exact:true}).waitFor();
    // All samples expire: current speed must become unknown, never a fabricated zero.
    fleet.remotes.forEach(r=>{r.seen=Date.now()/1000+1;r.snapshot.nodes[0].report.system.sampled_at=1;r.snapshot.lines.forEach(l=>l.traffic.sampled_at=1);});
    await page.getByRole('button',{name:'刷新总控',exact:true}).click();await go('总览');
    await page.getByText('↑ — · 有效 0/50 台',{exact:true}).waitFor();await page.getByText('↑ — · 有效 0/500 条',{exact:true}).waitFor();
    await page.screenshot({path:path.join(root,'fleet-desktop.png'),fullPage:true});
    await page.setViewportSize({width:390,height:844});
    await page.waitForFunction(()=>getComputedStyle(document.querySelector('.app-main')).marginLeft==='0px');
    await page.screenshot({path:path.join(root,'fleet-mobile.png'),fullPage:true});
    assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),'mobile overview overflow');
    await go('VPS');assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),'mobile table must scroll within container');
    await page.getByRole('button',{name:'清空选择',exact:true}).click();
    assert.equal(await page.getByRole('button',{name:'批量升级',exact:true}).count(),0);
    await page.getByLabel('筛选总控状态').selectOption('mismatch');
    await page.getByText('共 1 条 · 每页 20 条 · 第 1 / 1 页',{exact:true}).waitFor();
    await page.getByRole('button',{name:'选择本页',exact:true}).click();
    await page.getByText('已选 1 台 VPS',{exact:true}).waitFor();
    await page.getByRole('button',{name:'清除筛选',exact:true}).click();
    await page.getByRole('button',{name:'下一页',exact:true}).click();
    await page.getByRole('button',{name:'选择本页',exact:true}).click();
    await page.getByText('已选 21 台 VPS',{exact:true}).waitFor();
    assert.equal(await page.getByRole('button',{name:'重启面板',exact:true}).isVisible(),false);
    await page.getByText('重启与维护说明',{exact:true}).click();
    await page.getByRole('button',{name:'重启 Agent',exact:true}).waitFor();
    assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'Mobile selection and expanded maintenance do not overflow');
    await page.getByText('重启与维护说明',{exact:true}).scrollIntoViewIfNeeded();
    await page.screenshot({path:path.join(root,'vps-toolbar-mobile.png'),animations:'disabled'});
    await page.setViewportSize({width:1280,height:900});
    await page.screenshot({path:path.join(root,'vps-toolbar-desktop.png'),animations:'disabled'});
    await page.setViewportSize({width:390,height:844});
    await page.waitForFunction(()=>getComputedStyle(document.querySelector('.app-main')).marginLeft==='0px');
    await page.getByRole('button',{name:'上一页',exact:true}).click();
    const mobileRow=page.getByRole('region',{name:'VPS 列表',exact:true}).locator('tbody tr').first();
    assert.equal(await mobileRow.evaluate(el=>getComputedStyle(el).display),'grid');
    await go('所有代理');
    const proxyRow=page.getByRole('region',{name:'代理列表',exact:true}).locator('tbody tr').first();
    await page.screenshot({path:path.join(root,'fleet-mobile-cards.png'),fullPage:true});
    const rowBox=await proxyRow.boundingBox();assert.ok(rowBox.x>=0 && rowBox.x+rowBox.width<=390);
    await proxyRow.getByRole('button',{name:'编辑',exact:true}).click();await dialog().getByRole('heading',{name:'编辑代理 · VPS 01'}).waitFor();await closeDialog();
    await page.setViewportSize({width:1280,height:900});
    // Total control-plane outage must still age cached metrics, then clear its own error on recovery.
    const fresh=Math.floor(await page.evaluate(()=>Date.now()/1000));
    fleet.remotes.forEach(r=>{r.seen=fresh+1;r.snapshot.time=fresh;r.snapshot.nodes[0].seen=fresh;r.snapshot.nodes[0].report.system.sampled_at=fresh;r.snapshot.lines.forEach(l=>l.traffic.sampled_at=fresh);});
    await go('总览');await page.getByRole('button',{name:'刷新总控',exact:true}).click();await page.getByText('↑ 100.0 KiB/s · 有效 50/50 台',{exact:true}).waitFor();
    await page.clock.install();disconnected=true;await page.clock.runFor(95000);
    await page.getByText('↑ — · 有效 0/50 台',{exact:true}).waitFor();await page.getByText(/总控连接中断：/).waitFor();
    disconnected=false;await page.clock.runFor(5000);await page.getByText(/总控连接中断：/).waitFor({state:'hidden'});
    fleet.tasks.failed={id:'failed',action:'upgrade',state:'paused',version:'0.2.30',at:now+10,targets:[{id:'vps-0',state:'succeeded'},{id:'vps-1',state:'failed',error:'升级失败'},{id:'vps-2',state:'unknown'},{id:'vps-3',state:'pending'}]};
    await page.getByRole('button',{name:'刷新总控',exact:true}).click();await go('维护任务');
    await page.locator('.fleet-tasks').getByRole('button',{name:'查看任务',exact:true}).first().click();await dialog().getByText(/核对原任务不会重新执行已失败操作/).waitFor();
    await dialog().getByRole('button',{name:'选择失败及未执行对象',exact:true}).click();
    await page.getByText('已选 2 台 VPS',{exact:true}).waitFor();
    assert.equal(await page.getByLabel('选择 VPS VPS 01',{exact:true}).isChecked(),true);assert.equal(await page.getByLabel('选择 VPS VPS 03',{exact:true}).isChecked(),true);
    assert.equal(await page.getByLabel('选择 VPS VPS 02',{exact:true}).isChecked(),false);


    // A completed upgrade with failed verification must not suggest installing it again.
    fleet.tasks.verify={id:'verify',action:'upgrade',state:'paused',version:'0.2.40',at:now+20,targets:[
      {id:'vps-0',state:'failed',error:'组件维护已完成，但整链验证失败：HTTP 探测超时',result:{maintenance_state:'succeeded',verification:{state:'failed',detail:'HTTP 探测超时（40 秒）：TLS 握手未完成'},after:{panel:'0.2.40',agent:'0.2.40',chainctl:'0.2.40'}}},
      {id:'vps-1',state:'failed',error:'组件维护已完成，但整链验证失败：旧版本连接超时',result:{after:{panel:'0.2.40',agent:'0.2.40'}}},
      {id:'vps-2',state:'pending'}]};
    await page.getByRole('button',{name:'刷新总控',exact:true}).click();await go('维护任务');
    await page.locator('.fleet-tasks').getByRole('button',{name:'查看任务',exact:true}).first().click();
    await dialog().getByText('维护完成 2 / 3 · 验证未通过 2 · 操作失败 0',{exact:true}).waitFor();
    const upgraded=dialog().getByRole('region',{name:'VPS 任务：VPS 00',exact:true});
    const legacy=dialog().getByRole('region',{name:'VPS 任务：VPS 01',exact:true});
    for(const card of [upgraded,legacy]) {
      await card.getByText('组件维护：已完成',{exact:true}).waitFor();
      await card.getByText('后续验证：未通过',{exact:true}).waitFor();
      await card.getByText('维护完成 · 自动复查中',{exact:true}).waitFor();
    }
    await upgraded.getByText('HTTP 探测超时（40 秒）：TLS 握手未完成',{exact:true}).waitFor();
    await legacy.getByText('旧版本连接超时',{exact:true}).waitFor();
    assert.equal(await dialog().getByRole('button',{name:'选择失败及未执行对象',exact:true}).count(),0,'Verification failures must not offer a new upgrade');
    await page.screenshot({path:path.join(root,'upgrade-verification-desktop.png'),animations:'disabled'});
    await upgraded.getByRole('button',{name:'查看此 VPS 日志',exact:true}).click();
    await dialog().getByRole('heading',{name:'日志与诊断',exact:true}).waitFor();
    await dialog().getByText('remote-agent-log',{exact:true}).waitFor();
    assert.equal(writes.filter(w=>w.path==='/api/fleet/vps-0/forward'&&w.body.path==='/diagnostics/local/logs').at(-1).body.body.source,'panel');
    await dialog().getByRole('button',{name:'关闭',exact:true}).last().click();
    await dialog().getByRole('heading',{name:'批量任务进度',exact:true}).waitFor();
    await upgraded.getByText('HTTP 探测超时（40 秒）：TLS 握手未完成',{exact:true}).waitFor();
    await page.setViewportSize({width:390,height:844});
    await page.waitForFunction(()=>getComputedStyle(document.querySelector('.app-main')).marginLeft==='0px');
    assert.ok(await dialog().evaluate(el=>el.scrollWidth<=el.clientWidth+1),'Upgrade task fits narrow screen');
    await upgraded.scrollIntoViewIfNeeded();
    await page.screenshot({path:path.join(root,'upgrade-verification-mobile.png'),animations:'disabled'});
    const retry=dialog().getByRole('button',{name:'继续未执行任务',exact:true});
    await retry.scrollIntoViewIfNeeded();
    await page.screenshot({path:path.join(root,'upgrade-verification-mobile-actions.png'),animations:'disabled'});
    const batchesBefore=writes.filter(w=>w.path==='/api/fleet/batch').length;
    await retry.click();
    await dialog().getByText('维护完成 3 / 3 · 验证未通过 0 · 操作失败 0',{exact:true}).waitFor();
    assert.equal(writes.filter(w=>w.path==='/api/fleet/batch').length,batchesBefore,'Resume must not submit another upgrade');
    assert.equal(writes.filter(w=>w.path==='/api/fleet/batch/verify/resume').length,1,'Resume uses original task ID');
    await closeDialog();


    for(const t of Object.values(fleet.tasks))t.archived=true;
    fleet.tasks.automatic={id:'automatic',action:'upgrade',state:'succeeded',at:now+25,targets:[{id:'vps-0',state:'succeeded',result:{maintenance_state:'succeeded',verification:{state:'failed',detail:'fixture reset',retry_at:now+300}}}]};
    await page.getByRole('button',{name:'刷新总控',exact:true}).click();await go('维护任务');
    await page.locator('.fleet-tasks').getByText(/维护完成 1 台 · 验证未通过 1 台/).waitFor();
    await page.locator('.fleet-tasks').getByRole('button',{name:'查看任务',exact:true}).first().click();
    await dialog().getByText(/系统会自动复查连接，不会重复升级，也不阻塞其他 VPS/).waitFor();
    await dialog().getByText('维护完成 · 连接待确认',{exact:true}).waitFor();
    assert.equal(await dialog().getByRole('button',{name:'继续未执行任务',exact:true}).count(),0);
    const beforeRecheck=writes.length;
    await dialog().getByRole('button',{name:'立即复查连接',exact:true}).click();
    await dialog().getByRole('button',{name:'等待连接复查',exact:true}).waitFor();
    assert.equal(writes.length,beforeRecheck+1);
    assert.equal(writes.at(-1).path,'/api/fleet/batch/automatic/verify/vps-0');
    const beforeAutomatic=writes.length;
    fleet.tasks.automatic.targets[0].result.verification={state:'succeeded',detail:'fixture HTTP 200'};
    await page.clock.runFor(5000);
    await dialog().getByText('后续验证：已通过',{exact:true}).waitFor();
    await dialog().getByText('fixture HTTP 200',{exact:true}).waitFor();
    assert.equal(writes.length,beforeAutomatic,'自动恢复无需触发操作');
    await closeDialog();
    assert.equal(await page.locator('.fleet-tasks .fleet-task-row').count(),0,'恢复后自动移入历史');
    fleet.tasks.archive={id:'archive',action:'upgrade',state:'paused',at:now+30,targets:[{id:'vps-0',state:'failed',error:'升级失败，原始错误保留'}]};
    await page.getByRole('button',{name:'刷新总控',exact:true}).click();await go('维护任务');
    await page.getByText('0 个任务执行中 · 1 个待处理',{exact:true}).waitFor();
    await page.locator('.fleet-tasks').getByRole('button',{name:'查看任务',exact:true}).first().click();
    acceptDialog=false;await dialog().getByRole('button',{name:'忽略并归档',exact:true}).click();
    assert.equal(writes.filter(w=>w.path==='/api/fleet/batch/archive/archive').length,0);
    acceptDialog=true;await dialog().getByRole('button',{name:'忽略并归档',exact:true}).click();
    await dialog().getByText(/此任务已忽略并归档/).waitFor();
    await dialog().getByText('升级失败，原始错误保留',{exact:true}).waitFor();
    assert.equal(fleet.tasks.archive.state,'paused');assert.equal(fleet.tasks.archive.targets[0].state,'failed');
    assert.equal(await dialog().getByRole('button',{name:'重新核对原任务结果',exact:true}).count(),0);
    assert.equal(await dialog().getByRole('button',{name:'选择失败及未执行对象',exact:true}).count(),0);
    await closeDialog();assert.equal(await page.locator('.fleet-running-strip').count(),0);
    assert.equal(await page.locator('.fleet-tasks .fleet-task-row').count(),0,'归档默认移出列表');
    await page.getByLabel('显示历史任务',{exact:true}).check();
    await page.locator('.fleet-tasks .fleet-task-row').first().getByText('已归档',{exact:true}).waitFor();
    await page.getByLabel('显示历史任务',{exact:true}).uncheck();
    fleet.tasks.active={id:'active',action:'upgrade',state:'running',at:now+40,targets:[{id:'vps-0',state:'running'}]};
    await page.getByRole('button',{name:'刷新总控',exact:true}).click();
    await page.locator('.fleet-tasks').getByRole('button',{name:'查看任务',exact:true}).first().click();
    assert.equal(await dialog().getByRole('button',{name:'忽略并归档',exact:true}).count(),0,'Running task cannot be archived');
    await closeDialog();

    // Every fleet tab must remain usable at desktop and phone widths after restyling.
    for(const viewport of [{width:1280,height:900},{width:390,height:844}]) {
      await page.setViewportSize(viewport);
      if(viewport.width===390) await page.waitForFunction(()=>getComputedStyle(document.querySelector('.app-main')).marginLeft==='0px');
      for(const [tab,slug,names] of [
        ['总览','overview',['查看代理状态']],
        ['VPS','vps',['选择本页','查看 VPS']],
        ['所有代理','lines',['选择本页','编辑']],
        ['外部代理','externals',['编辑']],
        ['维护任务','tasks',['查看任务']],
        ['总控设置','settings',['生成本机接入码（10 分钟有效）','接入面板','保存总控策略']],
      ]) {
        await go(tab);
        const clear=page.getByRole('button',{name:'清除筛选',exact:true});
        if(await clear.count()) await clear.click();
        if(tab==='VPS' && viewport.width===1280) assert.ok(await page.getByRole('region',{name:'VPS 列表',exact:true}).evaluate(el=>el.scrollWidth<=el.clientWidth),'Desktop VPS table actions need no horizontal scroll');
        assert.equal(await nav.getByRole('button',{name:tab,exact:true}).getAttribute('aria-current'),'page');
        for(const name of names) {
          const control=page.getByRole('button',{name,exact:true}).first();
          await control.scrollIntoViewIfNeeded();
          assert.ok(await control.isVisible(),`${tab}: ${name} visible`);
          const box=await control.boundingBox();
          assert.ok(box.x>=0&&box.x+box.width<=viewport.width+1&&box.y>=0&&box.y+box.height<=viewport.height+1,`${tab}: ${name} reachable at ${viewport.width}px`);
        }
        assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),`${tab} must not overflow at ${viewport.width}px`);
        await page.evaluate(()=>scrollTo(0,0));
        await page.screenshot({path:path.join(root,`fleet-tab-${slug}-${viewport.width}.png`),fullPage:true,animations:'disabled'});
      }
    }


    await page.goto('about:blank');
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
