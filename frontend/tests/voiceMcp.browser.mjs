// NV9 real React UI, fake Tauri bridge, isolated Vite server and headless browser. No application/service configuration is changed.
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
import os from 'node:os';
import fs from 'node:fs/promises';
import {createServer} from 'vite';
import react from '@vitejs/plugin-react';
const require=createRequire(import.meta.url);
const {chromium}=require('playwright');
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const temp=await fs.mkdtemp(path.join(os.tmpdir(),'moss-nv9-ui-'));
const output=path.resolve(root,'../target/nv9-ui');
await fs.mkdir(output,{recursive:true});
const vite=await createServer({root,configFile:false,plugins:[react()],cacheDir:path.join(temp,'vite'),server:{host:'127.0.0.1',port:0},logLevel:'error'});
let browser;
try {
  await vite.listen();
  const address=vite.httpServer.address();
  const base=`http://127.0.0.1:${address.port}`;
  const html=await vite.transformIndexHtml('/nv9-test',`<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="stylesheet" href="/src/styles.css"></head><body><div id="root"></div><script type="module">
    import React from 'react';
    import {createRoot} from 'react-dom/client';
    import {VoiceMcpCard} from '/src/components/VoiceMcpCard.jsx';
    window.__TAURI__={core:{invoke:(command,args)=>window.__fakeInvoke(command,args)}};
    createRoot(document.getElementById('root')).render(React.createElement('main',{className:'ob-page voice-page',style:{padding:20,maxWidth:1200,margin:'auto'}},React.createElement(VoiceMcpCard)));
  </script></body></html>`);
  browser=await chromium.launch({headless:true,channel:process.env.PLAYWRIGHT_CHANNEL || (process.platform==='win32'?'msedge':undefined)});
  for (const width of [1280,960]) {
    const page=await browser.newPage({viewport:{width,height:950}});
    const errors=[],calls=[],dialogs=[];
    let serial=1;
    let status={revision:'r1',servers:[],connections:[],audit:[],max_servers:8,max_tools:64,last_error:null};
    const keys=new Map();
    page.on('pageerror',error=>errors.push(error.message));
    page.on('dialog',async dialog=>{dialogs.push(dialog.message());await dialog.accept();});
    await page.exposeFunction('__fakeInvoke',async (command,args={})=>{
      calls.push({command,args});
      if(command==='voice_mcp_status')return structuredClone(status);
      if(command==='voice_mcp_save'){
        const input=args.input;assert.equal(input.expected_revision,status.revision);assert.equal(input.confirmed,true);
        assert.ok(!JSON.stringify(input.config).includes('private-ui-key'));
        const config=structuredClone(input.config);
        if(Object.values(config.policies).includes('allow'))assert.equal(input.allow_without_approval,true);
        const old=status.servers.find(r=>r.config.name===config.name);
        const refs=config.transport.header_refs || config.transport.env_refs;
        for(const [key,ref] of Object.entries(refs)){
          const value=input.secrets[ref] || keys.get(ref);
          assert.ok(value,'fixture requires credentials');
          const versioned=`voice.mcp.${config.name}.${config.transport.type==='stdio'?'env':'header'}.${key}.v-${String(++serial).padStart(32,'0')}`;
          keys.set(versioned,value);refs[key]=versioned;
        }
        status.servers=status.servers.filter(r=>r.config.name!==config.name);
        status.servers.push({config,secret_set:Object.fromEntries(Object.values(refs).map(ref=>[ref,true]))});
        status.connections=status.connections.filter(r=>r.name!==config.name);
        status.connections.push({name:config.name,state:config.enabled?'connected':'disabled',connections:1,last_error:null,retry_seconds:null,tools:[{name:`${config.name}__read`,original:'read',description:'测试用只读工具，不访问文件或网络',permission:config.policies.read || 'ask',stats:{calls:2,failures:0,last_call_at:1791620000},blocked_reason:null}]});
        status.revision=`r${++serial}`;
        status.audit=[{at:1791620000,action:'external_mcp_config_changed',target:config.name,result:'ok',detail:{reason:old?'配置已更新':'配置已保存'}}];
        return structuredClone(status);
      }
      if(command==='voice_mcp_reconnect'){assert.equal(args.confirmed,true);return structuredClone(status);}
      if(command==='voice_mcp_delete'){assert.equal(args.confirmed,true);assert.equal(args.revision,status.revision);status={...status,revision:`r${++serial}`,servers:[],connections:[]};keys.clear();return structuredClone(status);}
      throw Error(`Unexpected command ${command}`);
    });
    await page.route('**/*',route=>new URL(route.request().url()).origin===base?route.continue():route.abort());
    await page.route('**/nv9-test',route=>route.fulfill({contentType:'text/html',body:html}));
    await page.goto(`${base}/nv9-test`);
    await page.locator('#voice-mcp-add').click();
    assert.equal(await page.locator('#voice-mcp-enabled').isChecked(),false);
    await page.locator('#voice-mcp-name').fill('notes');
    await page.locator('#voice-mcp-url').fill('https://example.test/mcp');
    await page.locator('#voice-mcp-enabled').check();
    await page.locator('#voice-mcp-add-secret').click();
    await page.getByLabel('凭据名称 1',{exact:true}).fill('authorization');
    const password=page.getByLabel('凭据值 1',{exact:true});
    assert.equal(await password.getAttribute('type'),'password');
    await password.fill('Bearer private-ui-key');
    await page.locator('#voice-mcp-save').click();
    await page.locator('#voice-mcp-server-notes').waitFor();
    assert.equal(calls.filter(c=>c.command==='voice_mcp_save').length,1);
    const first=calls.find(c=>c.command==='voice_mcp_save').args.input;
    assert.equal(first.config.enabled,true);assert.deepEqual(first.config.policies,{});
    assert.equal(first.secrets['voice.mcp.notes.header.authorization'],'Bearer private-ui-key');
    await page.getByRole('button',{name:'编辑设置',exact:true}).click();
    assert.equal(await page.getByLabel('凭据值 1',{exact:true}).inputValue(),'');
    await page.locator('#voice-mcp-url').fill('http://example.test/mcp');
    assert.equal(await page.locator('#voice-mcp-save').isDisabled(),true);
    await page.locator('#voice-mcp-url').fill('https://example.test/mcp');
    await page.locator('#voice-mcp-save').click();
    await page.waitForFunction(()=>!document.querySelector('#voice-mcp-form'));
    assert.deepEqual(calls.filter(c=>c.command==='voice_mcp_save')[1].args.input.secrets,{});
    await page.getByLabel('notes__read 权限',{exact:true}).selectOption('allow');
    await page.waitForFunction(()=>document.querySelector('select[aria-label="notes__read 权限"]')?.value==='allow');
    assert.ok(dialogs.some(text=>text.includes('免批准')&&text.includes('只读')));
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),true,'desktop width should not overflow');
    await page.getByText('最近审批与审计（最多 50 条）',{exact:true}).click();
    await page.screenshot({path:path.join(output,`nv9-${width}.png`),fullPage:true});
    await page.getByRole('button',{name:'重新连接',exact:true}).click();
    await page.getByRole('button',{name:'移除',exact:true}).click();
    await page.waitForFunction(()=>!document.querySelector('#voice-mcp-server-notes'));
    assert.equal(calls.filter(c=>c.command==='voice_mcp_reconnect').length,1);
    assert.equal(calls.filter(c=>c.command==='voice_mcp_delete').length,1);
    assert.deepEqual(errors,[]);
    console.log(`PASS NV9 ${width}px: add, separate password save, blank preserve, HTTPS check, policy warning, reconnect, remove, stats/audit, no overflow`);
    await page.close();
  }
} finally {
  if(browser)await browser.close();
  await vite.close();
  await fs.rm(temp,{recursive:true,force:true});
}
