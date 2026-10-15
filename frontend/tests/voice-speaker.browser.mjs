// Isolated actual React component with fake native commands. No microphone.
import assert from "node:assert/strict";
import { createServer } from "vite";
import { chromium } from "playwright";
import os from 'node:os';
import path from 'node:path';
import {randomUUID} from 'node:crypto';
const server=await createServer({cacheDir:path.join(os.tmpdir(),'moss-nv16-render-'+randomUUID()),optimizeDeps:{entries:[],include:['react','react-dom/client']},server:{host:"127.0.0.1",port:0},logLevel:"error",plugins:[{name:'nv16-fixture',configureServer(vite){vite.middlewares.use(async(req,res,next)=>{
  if(req.url!=='/__nv16_fixture.html'){next();return;}
  res.setHeader('Content-Type','text/html');
  res.end(await vite.transformIndexHtml('/__nv16_fixture.html','<html><meta charset="utf-8"><div id="root"></div><script type="module">import React from "react";import {createRoot} from "react-dom/client";import Card from "/src/components/VoiceSpeakerCard.jsx";import "/src/styles.css";createRoot(document.getElementById("root")).render(React.createElement(Card));</script></html>'));
});}}]});
await server.listen();
const browser=await chromium.launch({headless:true,executablePath:"C:/Program Files/Google/Chrome/Application/chrome.exe"});
try{
  const page=await browser.newPage({viewport:{width:1100,height:850}});
  const errors=[];page.on('pageerror',e=>{errors.push(e.message);console.error('Fixture page error:',e.message);});page.on('dialog',dialog=>dialog.accept());
  page.on('response',r=>{if(r.status()>=400)console.error('Fixture HTTP:',r.status(),r.url());});
  await page.addInitScript(()=>{
    window.__calls=[];let active=false;let memory={text:'合成旧摘要',revision:0};
    window.__externalUpdate=()=>{memory={text:'其他操作更新的摘要',revision:memory.revision+1};};
    window.__TAURI__={core:{invoke:async(command,{input})=>{
      if(command!=='voice_speaker_manage')throw Error('Unexpected command');
      window.__calls.push(input);if(input.action==='begin')active=true;if(input.action==='cancel')active=false;
      if(input.action==='save_memory'){
        if(input.revision!==memory.revision)throw {message:'个人摘要版本冲突',code:'voice_conflict'};
        memory={text:input.summary,revision:memory.revision+1};
      }
      return {sessions:[{device_id:'synthetic-a',session_id:'same'},{device_id:'synthetic-b',session_id:'same'}],speakers:[{id:'a',revision:1,name:'合成人甲',dimension:192,memory_summary:memory.text,memory_revision:memory.revision}],recognition_enabled:false,personal_memory_enabled:false,enrollment:{active,collected:0,processing:false,message:active?'请说三段完整句子':'未登记'}};
    }}};
  });
  await page.goto(server.resolvedUrls.local[0]+'__nv16_fixture.html');
  await page.getByRole('heading',{name:'声纹登记',exact:true}).waitFor();
  const button=page.getByRole('button',{name:'开始三段登记'});
  assert(await button.isDisabled());
  await page.getByLabel('姓名').fill('合成用户');
  await page.getByLabel('采集设备').selectOption(JSON.stringify(['synthetic-b','same']));
  await button.click();
  await page.getByRole('button',{name:'取消并丢弃片段'}).waitFor();
  const begin=await page.evaluate(()=>window.__calls.find(c=>c.action==='begin'));
  assert.deepEqual(begin,{action:'begin',name:'合成用户',device_id:'synthetic-b',session_id:'same',confirmed:true});
  assert(await button.isDisabled());
  if(process.argv[2])await page.screenshot({path:process.argv[2],fullPage:true});
  await page.getByRole('button',{name:'取消并丢弃片段'}).click();
  await page.getByRole('status').filter({hasText:'未登记'}).waitFor();
  assert(await page.getByRole('checkbox').isDisabled());
  const summary=page.getByLabel('个人摘要');
  await summary.fill('我的草稿');
  await page.evaluate(()=>window.__externalUpdate());
  await page.getByRole('status').filter({hasText:'个人摘要已被其他操作更新'}).waitFor();
  assert.equal(await summary.inputValue(),'我的草稿');
  await page.getByRole('button',{name:'保存个人摘要'}).click();
  await page.getByRole('alert').filter({hasText:'个人摘要版本冲突'}).waitFor();
  const conflict=await page.evaluate(()=>window.__calls.filter(c=>c.action==='save_memory').at(-1));
  assert.deepEqual(conflict,{action:'save_memory',id:'a',revision:0,summary:'我的草稿'});
  const polls=await page.evaluate(()=>window.__calls.filter(c=>c.action==='status').length);
  await page.waitForFunction(n=>window.__calls.filter(c=>c.action==='status').length>n,polls);
  assert(await page.getByRole('alert').filter({hasText:'个人摘要版本冲突'}).isVisible());
  await page.getByRole('button',{name:'丢弃草稿并加载最新摘要'}).click();
  assert.equal(await summary.inputValue(),'其他操作更新的摘要');
  await summary.fill('新的本机摘要');
  await page.getByRole('button',{name:'保存个人摘要'}).click();
  await page.getByRole('button',{name:'丢弃草稿并加载最新摘要'}).waitFor({state:'hidden'});
  assert.equal(await summary.inputValue(),'新的本机摘要');
  assert.equal(await page.getByRole('alert').count(),0);
  const area=await summary.boundingBox();
  assert(area.width>800,'Personal summary uses the available desktop width');
  if(process.argv[2])await page.screenshot({path:process.argv[2],fullPage:true});
  await page.setViewportSize({width:390,height:850});
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'No horizontal overflow on narrow viewport');
  // 简易标定（2026-10-14）：两人各录三段测试音 → 计算阈值 → 识别开启后才能勾选按人记忆
  const calib=await browser.newPage({viewport:{width:1100,height:900}});
  calib.on('pageerror',e=>errors.push(e.message));calib.on('dialog',dialog=>dialog.accept());
  await calib.addInitScript(()=>{
    window.__calls=[];const clips={a:0,b:0};let calibration={state:'none'};
    window.__TAURI__={core:{invoke:async(command,{input})=>{
      if(command!=='voice_speaker_manage')throw Error('Unexpected command');
      window.__calls.push(input);
      if(input.action==='begin_test')clips[input.id]=3;
      if(input.action==='calibrate')calibration={state:'valid',report:{threshold:0.62,margin:0.05,people:2,genuine_trials:6,genuine_accepted:6,impostor_trials:6,false_accepts:0,max_impostor_score:0.31}};
      const valid=calibration.state==='valid';
      return {sessions:[{device_id:'synthetic-a',session_id:'same'}],speakers:[{id:'a',revision:1,name:'甲',dimension:192,memory_summary:'',memory_revision:0},{id:'b',revision:1,name:'乙',dimension:192,memory_summary:'',memory_revision:0}],
        recognition_enabled:valid,personal_memory_enabled:false,calibration,test_clips:{...clips},enrollment:{active:false,collected:0,processing:false,message:'未登记'}};
    }}};
  });
  await calib.goto(server.resolvedUrls.local[0]+'__nv16_fixture.html');
  await calib.getByText('未标定：身份识别与按人记忆保持关闭').waitFor();
  const compute=calib.getByRole('button',{name:'计算阈值并自检'});
  assert(await compute.isDisabled(),'没录测试音不能计算');
  assert(await calib.getByRole('checkbox').isDisabled(),'未标定不能开启按人记忆');
  await calib.getByLabel('采集设备').selectOption(JSON.stringify(['synthetic-a','same']));
  const recordButtons=calib.getByRole('button',{name:'录测试音'});
  await recordButtons.nth(0).click();
  await calib.getByText('标定测试音 3/3').first().waitFor();
  assert(await compute.isDisabled(),'乙还没录');
  await recordButtons.nth(1).click();
  await calib.waitForFunction(()=>window.__calls.filter(c=>c.action==='begin_test').length===2);
  await calib.waitForFunction(()=>!document.querySelector('#voice-speaker-calibration button').disabled);
  await compute.click();
  await calib.getByText(/已标定（2 人）：阈值 0\.62 · 本人认对 6\/6 · 认成别人 0\/6/).waitFor();
  assert.deepEqual(await calib.evaluate(()=>window.__calls.filter(c=>c.action==='begin_test')),[
    {action:'begin_test',id:'a',device_id:'synthetic-a',session_id:'same',confirmed:true},
    {action:'begin_test',id:'b',device_id:'synthetic-a',session_id:'same',confirmed:true}]);
  assert(!(await calib.getByRole('checkbox').isDisabled()),'标定通过后可以开启按人记忆');
  await calib.close();
  assert.equal(errors.length,0,errors.join('\n'));
  console.log('NV16 actual React: confirmation, device/session binding, cancel, disabled identity, summary CAS conflict preserves draft/error across polls, reload/save, easy calibration gate PASS');
}finally{await browser.close();await server.close();}
