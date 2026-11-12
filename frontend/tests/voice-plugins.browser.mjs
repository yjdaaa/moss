// Actual React with a fake native bridge; no providers, accounts or devices.
import assert from 'node:assert/strict';
import {createServer} from 'vite';
import {chromium} from 'playwright';
import os from 'node:os';
import path from 'node:path';
import {randomUUID} from 'node:crypto';
const server=await createServer({cacheDir:path.join(os.tmpdir(),'moss-nv11-render-'+randomUUID()),optimizeDeps:{entries:[],include:['react','react-dom/client']},server:{host:'127.0.0.1',port:0},logLevel:'error',plugins:[{name:'nv11-fixture',configureServer(vite){vite.middlewares.use(async(req,res,next)=>{
  if(req.url!=='/__nv11_fixture.html'){next();return;}
  res.setHeader('Content-Type','text/html');
  res.end(await vite.transformIndexHtml('/__nv11_fixture.html','<html><meta charset="utf-8"><div id="root"></div><script type="module">import React from "react";import {createRoot} from "react-dom/client";import Card from "/src/components/VoicePluginsCard.jsx";import "/src/styles.css";createRoot(document.getElementById("root")).render(React.createElement(Card));</script></html>'));
});}}]});
await server.listen();
const browser=await chromium.launch({headless:true,channel:process.env.PLAYWRIGHT_CHANNEL||(process.platform==='win32'?'msedge':undefined)});
try {
  const page=await browser.newPage({viewport:{width:1100,height:900}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.addInitScript(()=>{
    window.__calls=[];window.__failSave=true;
    // Older settings intentionally lack Metaso fields; status contains bools,
    // never existing credential text.
    let settings={search_host:'https://api.tavily.com',search_enabled:false};
    let keys={weather:false,search:true,metaso:false,rag:false};
    window.__TAURI__={core:{invoke:async(command,{input}={})=>{
      if(command==='voice_plugins_status')return {settings,keys};
      if(command!=='voice_plugins_save')throw Error('Unexpected native command');
      window.__calls.push(input);
      if(window.__failSave)throw {message:'合成保存失败，原配置保留'};
      settings=input.settings;
      for(const [kind,change] of Object.entries(input.keys)){
        if(change.action==='set')keys[kind]=true;
        if(change.action==='clear')keys[kind]=false;
      }
      return {settings,keys};
    }}};
  });
  await page.goto(server.resolvedUrls.local[0]+'__nv11_fixture.html');
  await page.getByRole('heading',{name:'信息服务'}).waitFor();
  assert.equal(await page.getByLabel('搜索服务').count(),0,'未选择时不显示搜索表单');
  await page.getByLabel('要设置的服务').selectOption('search');
  await page.getByLabel('搜索服务').waitFor();
  assert.equal(await page.getByLabel('搜索服务').inputValue(),'tavily');
  for(const checkbox of await page.getByRole('checkbox').all())assert.equal(await checkbox.isChecked(),false);
  assert.equal(await page.getByLabel('秘塔 API 地址',{exact:true}).count(),0,'未选择的服务商不显示输入框');
  await page.getByLabel('搜索服务').selectOption('metaso');
  assert.equal(await page.getByLabel('秘塔 API 地址',{exact:true}).inputValue(),'https://metaso.cn');
  assert.equal(await page.getByLabel('Tavily API 地址').count(),0);
  await page.getByLabel('秘塔 API 密钥 密钥操作').selectOption('set');
  const draft=page.getByLabel('秘塔 API 密钥 新密钥');
  await draft.fill('NV11_FAKE_METASO_ONLY');assert.equal(await draft.getAttribute('type'),'password');
  await page.getByLabel('启用联网搜索',{exact:true}).check();
  const save=page.getByRole('button',{name:'保存信息服务',exact:true});await save.click();
  await page.getByRole('alert').filter({hasText:'合成保存失败'}).waitFor();
  assert.equal(await page.getByLabel('搜索服务').inputValue(),'metaso');assert.equal(await draft.inputValue(),'NV11_FAKE_METASO_ONLY');
  let payload=await page.evaluate(()=>window.__calls.at(-1));
  assert.equal(payload.settings.search_provider,'metaso');
  assert.deepEqual(payload.keys.metaso,{action:'set',value:'NV11_FAKE_METASO_ONLY'});
  assert.deepEqual(payload.keys.search,{action:'keep',value:''});
  assert(!JSON.stringify(payload.settings).includes('NV11_FAKE_METASO_ONLY'));
  await page.evaluate(()=>{window.__failSave=false;});await save.click();
  await page.getByRole('status').filter({hasText:'插件设置已保存'}).waitFor();
  assert.equal(await page.getByLabel('秘塔 API 密钥 密钥操作').inputValue(),'keep');assert.equal(await draft.count(),0);
  assert.equal(await page.getByRole('alert').count(),0);
  assert(!(await page.locator('body').innerText()).includes('NV11_FAKE_METASO_ONLY'));
  await page.getByLabel('启用联网搜索',{exact:true}).uncheck();
  await page.getByLabel('秘塔 API 密钥 密钥操作').selectOption('clear');await save.click();
  await page.waitForFunction(()=>window.__calls.at(-1)?.keys.metaso.action==='clear');
  await page.getByRole('status').filter({hasText:'插件设置已保存'}).waitFor();
  payload=await page.evaluate(()=>window.__calls.at(-1));assert.equal(payload.settings.search_enabled,false);assert.equal(payload.keys.search.action,'keep');
  await page.setViewportSize({width:390,height:900});
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'NV11 narrow viewport overflow');
  if(process.argv[2])await page.screenshot({path:process.argv[2],fullPage:true});
  assert.deepEqual(errors,[]);console.log('NV11 actual React: old settings/defaults/provider selection/independent Keep Set Clear/failed draft/success reset/narrow layout PASS');
}finally{await browser.close();await server.close();}
