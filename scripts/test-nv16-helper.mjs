// NV16 real helper, public pinned model, SYNTHETIC PCM only. No microphone/device.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import assert from 'node:assert/strict';
const [exe, root] = process.argv.slice(2);
if (!exe || !root) throw new Error('Usage: node scripts/test-nv16-helper.mjs <helper.exe> <task-owned-model-root>');
const file=path.join(root,'3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx');
const bytes=fs.readFileSync(file);
assert.equal(bytes.length,28281138);
assert.equal(crypto.createHash('sha256').update(bytes).digest('hex'),'f682b514c05d947ee3fa91cd6ec6c5c7543479a128373fa29b1faedccd21fd11');
const child=spawn(exe,[],{stdio:['pipe','pipe','ignore'],windowsHide:true});
let buffer=Buffer.alloc(0), next=0;
const pending=new Map();
child.stdout.on('data',chunk=>{
  buffer=Buffer.concat([buffer,chunk]);
  while(buffer.length>=8){
    const h=buffer.readUInt32LE(0),p=buffer.readUInt32LE(4);
    assert(h<=65536 && p<=5760000);
    if(buffer.length<8+h+p)break;
    const reply=JSON.parse(buffer.subarray(8,8+h).toString('utf8'));
    const payload=Buffer.from(buffer.subarray(8+h,8+h+p));buffer=buffer.subarray(8+h+p);
    const waiter=pending.get(reply.id);if(waiter){pending.delete(reply.id);clearTimeout(waiter.timer);waiter.resolve({reply,payload});}
  }
});
child.on('error',error=>{for(const waiter of pending.values()){clearTimeout(waiter.timer);waiter.reject(error);}pending.clear();});
child.on('exit',code=>{for(const waiter of pending.values()){clearTimeout(waiter.timer);waiter.reject(new Error(`Helper exited: ${code}`));}pending.clear();});
function call(op,payload=Buffer.alloc(0)){
  const id=++next,header=Buffer.from(JSON.stringify({id,...op}));
  const sizes=Buffer.alloc(8);sizes.writeUInt32LE(header.length);sizes.writeUInt32LE(payload.length,4);
  return new Promise((resolve,reject)=>{
    const timer=setTimeout(()=>{pending.delete(id);reject(new Error('Helper request timeout'));child.kill();},10000);
    pending.set(id,{resolve,reject,timer});child.stdin.write(Buffer.concat([sizes,header,payload]));
  });
}
const model_id='campplus_sv_zh_cn_16k_common';
try{
  assert.equal((await call({op:'hello'})).reply.protocol,4);
  assert.equal((await call({op:'load_speaker',model_id:'untrusted',models_root:root})).reply.kind,'error');
  const loaded=await call({op:'load_speaker',model_id,models_root:root});
  assert.equal(loaded.reply.kind,'speaker_loaded');assert.equal(loaded.reply.dimension,192);assert.equal(loaded.reply.sample_rate,16000);
  if(process.platform==='win32') {
    assert.throws(()=>fs.openSync(file,'r+'),/EPERM|EACCES|EBUSY/, 'loaded model must deny write');
    assert.throws(()=>fs.renameSync(file,file+'.replaced'),/EPERM|EACCES|EBUSY/, 'loaded model must deny replacement');
  }
  const pcm=Buffer.alloc(48000*2);
  for(let i=0;i<48000;i++)pcm.writeInt16LE(Math.round(6000*Math.sin(i*2*Math.PI*220/16000)+2000*Math.sin(i*2*Math.PI*530/16000)),i*2);
  const start=performance.now();
  const result=await call({op:'speaker_embed',model_id,sample_rate:16000,utterance_generation:17},pcm);
  assert.equal(result.reply.kind,'embedding');assert.equal(result.reply.model_id,model_id);
  assert.equal(result.reply.utterance_generation,17);assert.equal(result.reply.dimension,192);assert.equal(result.reply.format,'f32le');
  assert.equal(result.payload.length,768);
  let norm=0;for(let i=0;i<192;i++){const v=result.payload.readFloatLE(i*4);assert(Number.isFinite(v));norm+=v*v;}assert(norm>1e-16);
  assert.equal((await call({op:'speaker_embed',model_id,sample_rate:24000,utterance_generation:18},pcm)).reply.kind,'error');
  assert.equal((await call({op:'speaker_embed',model_id,sample_rate:16000,utterance_generation:19},Buffer.alloc(100))).reply.kind,'error');
  console.log(JSON.stringify({protocol:4,model_id,sha256:'f682b514c05d947ee3fa91cd6ec6c5c7543479a128373fa29b1faedccd21fd11',dimension:192,payload_bytes:768,synthetic_inference_ms:Math.round(performance.now()-start),valid_embedding:true,negative_requests_rejected:true,real_speaker_accuracy:'unverified'}));
}finally{child.stdin.end();if(child.exitCode===null)child.kill();}
