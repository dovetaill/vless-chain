// Real React and browser; all API traffic is intercepted, including system time actions.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(process.env.CHAIN_TEST_BROWSER);
const assets = path.resolve(process.argv[2]);
const root = process.env.CHAIN_TEST_ROOT;
(async () => {
  const server = await chromium.launchServer({ headless:true, executablePath:process.env.CHAIN_TEST_CHROMIUM });
  fs.writeFileSync(path.join(root,'diagnostics-flow-browser.json'),JSON.stringify({pid:process.pid,pgid:Number(fs.readFileSync('/proc/self/stat','utf8').split(') ')[1].split(' ')[2]),browser:server.process().pid,endpoint:server.wsEndpoint(),artifacts:root}));
  const browser = await chromium.connect(server.wsEndpoint());
  try {
    const page = await browser.newPage({viewport:{width:1440,height:1000},timezoneId:'America/New_York'});
    page.setDefaultTimeout(10000);
    const errors=[]; page.on('pageerror',error=>errors.push(error.message));
    const now=Math.floor(Date.now()/1000), remoteTime=now+360;
    const state={nodes:[],lines:[],externals:[],checks:{},jobs:[],busy:false,time:now,subscriptions:[],security:{totp_enabled:false,recovery_remaining:0,origin:'',passkeys:[]},alerts:[],customers:[],payments:[],pending_imports:[]};
    const snapshot={...state,time:remoteTime,local_node_id:'local',version:'0.2.39',agent_version:'0.2.39',agent_seen:remoteTime,maintenance:true,
      nodes:[{id:'local',name:'本机节点',host:'192.0.2.1',seen:remoteTime,online:true,report:{running:true,applied:true,error:'',revision:'',traffic:{},system:{sampled_at:remoteTime,network_rx_bps:1024,network_tx_bps:2048,cpu_percent:12,memory_total:4096,memory_used:1024}}}],
      monitoring:{conditions:{'ntp:local':{active:true,message:'本机节点：自动时间同步未启用'}}}};
    const fleet={time:now,remotes:[{id:'vps-1',name:'blo-wave',url:'https://example.invalid',seen:now,error:'',snapshot}],controller:'',policy:null,tasks:{}};
    let active=0,maxActive=0,reads=0,holdNext=false,releaseRead,heldSource,taskWrites=0;
    const sources=[];let extraLogs='';
    await page.route('**/*',async route=>{
      const url=new URL(route.request().url());
      if(url.hostname!=='localhost') return route.abort();
      if(!url.pathname.startsWith('/api/')) {
        const file=path.join(assets,url.pathname==='/admin'?'index.html':url.pathname);
        return route.fulfill({body:fs.readFileSync(file),contentType:{'.html':'text/html','.js':'text/javascript','.css':'text/css'}[path.extname(file)]||'application/octet-stream'});
      }
      let result={};
      if(url.pathname==='/api/state') result=state;
      else if(url.pathname==='/api/fleet') result=fleet;
      else if(url.pathname==='/api/auth/options') result={totp:false,passkey:false};
      else if(url.pathname==='/api/fleet/release/latest') result={version:'0.2.39'};
      else if(url.pathname==='/api/fleet/vps-1/forward') {
        const body=route.request().postDataJSON();
        if(body.path==='/diagnostics/local/logs') {
          reads++;active++;maxActive=Math.max(maxActive,active);sources.push(body.body.source);
          if(holdNext) {holdNext=false;heldSource=body.body.source;await new Promise(resolve=>{releaseRead=resolve;});}
          active--;
          result={text:`${now} ${body.body.source==='xray'?'2026/10/21 09:01:00.123456 ':''}${body.body.source} log entry\n`+'long-line-'.repeat(80)+extraLogs,sampled_at:now,sources:['xray','mihomo'],detail:'',clock:{ntp_enabled:false,timezone:'Asia/Shanghai',detail:'未发现可用时间服务'}};
        } else if(body.path==='/federation/task') {
          if(body.body.action==='status') result={state:'failed',error:'自动时间同步失败\napt-get update: Temporary failure resolving deb.debian.org\n未安装 chrony；请检查 DNS 后重试。',detail:'apt-get update: Temporary failure resolving deb.debian.org\n未安装 chrony；请检查 DNS 后重试。'};
          else {taskWrites++;result={};}
        } else throw new Error('Unexpected forward: '+body.path);
      } else throw new Error('Unexpected API: '+url.pathname);
      return route.fulfill({json:result});
    });
    const button=name=>page.getByRole('button',{name,exact:true});
    const dialog=()=>page.getByRole('dialog');
    const log=()=>page.getByLabel('日志记录',{exact:true});
    const open=async()=>{await button('查看 VPS').click();await dialog().getByRole('button',{name:'日志与诊断',exact:true}).click();await log().filter({hasText:/panel log entry/}).waitFor();};
    const close=async()=>{await dialog().getByRole('button',{name:'关闭',exact:true}).last().click();await dialog().waitFor({state:'hidden'});};
    await page.goto('http://localhost/admin#fleet');
    await page.getByText(/远端快照时间比总控快约 360 秒/).waitFor();
    await page.getByText(/本机节点：自动时间同步未启用/).waitFor();
    await page.getByText('↑ 2.0 KiB/s · 有效 1/1 台',{exact:true}).waitFor();
    assert.doesNotMatch(await page.locator('body').innerText(),/undefined\/s/);
    await page.screenshot({path:path.join(root,'overview-diagnostics-desktop.png'),fullPage:true});
    await open();
    assert.equal(await page.getByLabel('日志来源',{exact:true}).inputValue(),'panel');
    await button('暂停跟随').click();
    const paused=reads;await page.waitForTimeout(3200);assert.equal(reads,paused,'Paused log must not poll');
    holdNext=true;await button('刷新日志').click();
    for(let i=0;!releaseRead && i<100;i++) await page.waitForTimeout(10);
    assert.ok(releaseRead,'Expected held log request');
    assert.equal(heldSource,'panel');
    assert.equal(await button('读取中…').isDisabled(),true);
    await page.getByLabel('日志来源',{exact:true}).selectOption('agent');
    await page.getByLabel('日志来源',{exact:true}).selectOption('xray');
    const heldReads=reads;await page.waitForTimeout(100);assert.equal(reads,heldReads,'Source changes wait for pending remote read');
    releaseRead();releaseRead=undefined;
    await log().filter({hasText:/xray log entry/}).waitFor();
    assert.equal(maxActive,1,'One diagnostic request at a time');
    assert.equal(sources.at(-1),'xray');
    assert.ok(!sources.includes('agent'),'Superseded source is not fetched');
    await button('继续跟随').click();await log().filter({hasText:/xray log entry/}).waitFor();
    holdNext=true;await button('刷新日志').click();
    for(let i=0;!releaseRead && i<100;i++) await page.waitForTimeout(10);
    assert.ok(releaseRead,'Expected held log request');
    await button('暂停跟随').click();releaseRead();releaseRead=undefined;
    await log().filter({hasText:/xray log entry/}).waitFor();
    const afterPause=reads;await page.waitForTimeout(3200);assert.equal(reads,afterPause,'Pause retains in-flight response without another request');
    assert.doesNotMatch(await log().innerText(),/2026\/10\/21 09:01:00/,'Xray duplicate timestamp is hidden');
    await button('清屏').click();await log().filter({hasText:'已清屏，等待新日志'}).waitFor();
    await button('刷新日志').click();await button('刷新日志').waitFor();
    assert.doesNotMatch(await log().innerText(),/xray log entry/,'Refresh does not restore cleared history');
    extraLogs=`\n${now} 2026/10/21 09:01:00.123456 new-after-clear`;
    await button('继续跟随').click();await log().filter({hasText:'new-after-clear'}).waitFor();
    assert.doesNotMatch(await log().innerText(),/xray log entry|2026\/10\/21 09:01:00/);
    extraLogs+=`\n${now} 2026/10/21 09:01:00.123456 new-after-clear`;
    await page.waitForFunction(()=>document.querySelector('[aria-label="日志记录"]').textContent.split('new-after-clear').length===3);
    await button('暂停跟随').click();
    await button('显示历史').click();await log().filter({hasText:'xray log entry'}).waitFor();
    await button('清屏').click();await page.getByLabel('日志来源',{exact:true}).selectOption('agent');
    await log().filter({hasText:'agent log entry'}).waitFor();
    assert.equal(await button('显示历史').count(),0,'Changing source resets clear state');
    page.once('dialog',d=>d.dismiss());await button('启用本 VPS 自动校时').click();assert.equal(taskWrites,0);
    page.once('dialog',d=>d.accept());await button('启用本 VPS 自动校时').click();
    const failure=page.getByRole('alert').filter({hasText:'自动校时未完成'});
    await failure.getByText(/Temporary failure resolving deb.debian.org/).waitFor();
    assert.match(await failure.innerText(),/自动时间同步失败/);assert.equal(taskWrites,1);
    assert.equal((await failure.innerText()).match(/Temporary failure resolving/g).length,1,'Task detail already included in error is shown only once');
    await page.screenshot({path:path.join(root,'diagnostics-flow-desktop.png'),fullPage:true});
    await page.setViewportSize({width:390,height:844});
    await page.waitForFunction(()=>getComputedStyle(document.querySelector('.app-main')).marginLeft==='0px');
    await page.screenshot({path:path.join(root,'diagnostics-flow-mobile.png'),animations:'disabled'});
    await failure.scrollIntoViewIfNeeded();
    await page.screenshot({path:path.join(root,'diagnostics-flow-mobile-failure.png'),animations:'disabled'});
    assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'Page fits narrow screen');
    assert.ok(await dialog().evaluate(el=>el.getBoundingClientRect().right<=innerWidth && el.scrollWidth<=el.clientWidth+1),'Dialog content fits narrow screen');
    await close();const closed=reads;await page.waitForTimeout(3200);assert.equal(reads,closed,'Closing stops requests');
    await page.screenshot({path:path.join(root,'overview-diagnostics-mobile.png'),fullPage:true});
    assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'Overview fits narrow screen');
    assert.deepEqual(errors,[]);
    console.log('PASS diagnostics journey: specific overview reasons, skew-safe rates, panel default, source serialization, pause/resume, manual refresh, time failure detail, cancel, desktop/mobile, close cleanup');
  } finally {await browser.close();await server.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
