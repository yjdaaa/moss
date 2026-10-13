// Local diagnostic only. Never enables recognition or changes MOSS settings.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {spawn} from 'node:child_process';
import {fileURLToPath} from 'node:url';
export const MODEL='campplus_sv_zh_cn_16k_common';
export const MODEL_SHA='f682b514c05d947ee3fa91cd6ec6c5c7543479a128373fa29b1faedccd21fd11';
export function helperEnvironment(){const env={};for(const key of ['SystemRoot','WINDIR','TEMP','TMP'])if(process.env[key])env[key]=process.env[key];return env;}
const fail=()=>{throw Error('nv16_input_invalid');};
export function normalize(v){
  if(!Array.isArray(v)||v.length!==192||v.some(x=>!Number.isFinite(x)))fail();
  const n=Math.hypot(...v);if(n<1e-8||!Number.isFinite(n))fail();return v.map(x=>x/n);
}
const cosine=(a,b)=>Math.max(-1,Math.min(1,a.reduce((n,x,i)=>n+x*b[i],0)));
export function pcm16(wav){
  if(!Buffer.isBuffer(wav)||wav.length<44||wav.length>400000||wav.toString('ascii',0,4)!=='RIFF'||wav.toString('ascii',8,12)!=='WAVE'||wav.readUInt32LE(4)!==wav.length-8)fail();
  let fmt=null,data=null;
  for(let p=12;p+8<=wav.length;){const type=wav.toString('ascii',p,p+4),size=wav.readUInt32LE(p+4),end=p+8+size;if(end>wav.length)fail();
    if(type==='fmt '){if(fmt||size<16)fail();fmt=wav.subarray(p+8,end);}
    if(type==='data'){if(data)fail();data=wav.subarray(p+8,end);}p=end+(size&1);
  }
  if(!fmt||!data||fmt.readUInt16LE(0)!==1||fmt.readUInt16LE(2)!==1||fmt.readUInt32LE(4)!==16000||fmt.readUInt32LE(8)!==32000||fmt.readUInt16LE(12)!==2||fmt.readUInt16LE(14)!==16||data.length<64000||data.length>320000||data.length%2)fail();
  return data;
}
function wilsonUpper(errors,n){if(!n)return null;const z=1.96,p=errors/n;return (p+z*z/(2*n)+z*Math.sqrt(p*(1-p)/n+z*z/(4*n*n)))/(1+z*z/n);}
export function evaluate(people,cases,sourceClass){
  if(!['synthetic','human_claimed'].includes(sourceClass)||!Array.isArray(people)||people.length<3||people.length>64||!Array.isArray(cases)||cases.length>5000)fail();
  const ids=new Set(),templates=people.map(p=>{if(!/^[a-zA-Z0-9_-]{1,64}$/.test(p.id)||ids.has(p.id)||p.references?.length!==3)fail();ids.add(p.id);
    const v=p.references.map(normalize);for(let i=0;i<3;i++)for(let j=i+1;j<3;j++)if(cosine(v[i],v[j])<0.7)fail();
    return {id:p.id,vector:normalize(v[0].map((_,i)=>(v[0][i]+v[1][i]+v[2][i])/3))};
  });
  const scored=cases.map(c=>{if(!['genuine','impostor','noise','replay'].includes(c.kind)||(c.kind==='genuine'?!ids.has(c.expected):c.expected!==null))fail();
    const vector=normalize(c.vector),scores=templates.map(t=>({id:t.id,score:cosine(vector,t.vector)})).sort((a,b)=>b.score-a.score);
    return {kind:c.kind,expected:c.expected,best:scores[0],gap:scores[0].score-scores[1].score};
  });
  const genuine=scored.filter(c=>c.kind==='genuine'),negative=scored.filter(c=>c.kind!=='genuine');
  const coverage={people:ids.size,genuine:genuine.length,impostor:scored.filter(c=>c.kind==='impostor').length,noise:scored.filter(c=>c.kind==='noise').length,replay:scored.filter(c=>c.kind==='replay').length,each_person_twenty_genuine:[...ids].every(id=>genuine.filter(c=>c.expected===id).length>=20)};
  const candidates=[];
  for(const threshold of [0.6,0.7,0.8,0.9,0.95])for(const margin of [0.02,0.05,0.1]){
    let falseAccept=0,falseReject=0,wrongPerson=0;const byKind={};
    for(const c of scored){const match=c.best.score>=threshold&&c.gap>=margin?c.best.id:null;
      if(c.kind==='genuine'){if(match!==c.expected)falseReject++;if(match&&match!==c.expected)wrongPerson++;}
      else{const row=byKind[c.kind]??={samples:0,false_accepts:0};row.samples++;if(match){falseAccept++;row.false_accepts++;}}
    }
    candidates.push({threshold,margin,genuine_trials:genuine.length,negative_trials:negative.length,false_accepts:falseAccept,false_rejects:falseReject,wrong_person:wrongPerson,FAR:negative.length?falseAccept/negative.length:null,FRR:genuine.length?falseReject/genuine.length:null,FAR_wilson95_upper:wilsonUpper(falseAccept,negative.length),negative_categories:byKind});
  }
  return {schema:1,model_id:MODEL,model_sha256:MODEL_SHA,protocol:4,dimension:192,source_class:sourceClass,labels_verified:false,coverage,candidates,accepted_threshold:null,production_ready:false,real_speaker_accuracy:'unverified'};
}
function readSelected(root,name){
  if(typeof name!=='string'||!/^[-a-zA-Z0-9_.]{1,180}\.wav$/.test(name)||name.includes('..'))fail();
  const file=path.join(root,name),stat=fs.lstatSync(file);if(!stat.isFile()||stat.isSymbolicLink()||stat.size>400000)fail();
  if(path.dirname(fs.realpathSync(file))!==root)fail();return pcm16(fs.readFileSync(file));
}
function noLinks(file){let cursor=path.parse(file).root;for(const part of file.slice(cursor.length).split(path.sep).filter(Boolean)){cursor=path.join(cursor,part);if(fs.lstatSync(cursor).isSymbolicLink())fail();}}
async function main(){
  const args=process.argv.slice(2),opts={};for(let i=0;i<args.length;i++){const key=args[i];if(key==='--consent'){opts.consent=true;continue;}if(!['--helper','--model-root','--dataset','--report'].includes(key)||opts[key]||!args[i+1])fail();opts[key]=args[++i];}
  if(!opts.consent||['--helper','--model-root','--dataset','--report'].some(k=>!opts[k]))fail();
  if(fs.existsSync(opts['--report']))fail();
  const manifestFile=path.resolve(opts['--dataset']),manifestStat=fs.lstatSync(manifestFile);if(!manifestStat.isFile()||manifestStat.isSymbolicLink()||manifestStat.size>512000)fail();
  noLinks(manifestFile);
  const root=fs.realpathSync(path.dirname(manifestFile)),dataset=JSON.parse(fs.readFileSync(manifestFile,'utf8'));
  if(dataset.schema!==1||!['synthetic','human_claimed'].includes(dataset.source_class)||!Array.isArray(dataset.people)||dataset.people.length<3||dataset.people.length>64||!Array.isArray(dataset.cases)||dataset.cases.length>5000)fail();
  const model=fs.readFileSync(path.join(opts['--model-root'],'3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx'));
  if(model.length!==28281138||crypto.createHash('sha256').update(model).digest('hex')!==MODEL_SHA)fail();
  const child=spawn(path.resolve(opts['--helper']),[],{cwd:path.resolve(opts['--model-root']),env:helperEnvironment(),stdio:['pipe','pipe','ignore'],windowsHide:true});let buffer=Buffer.alloc(0),seq=0,pending=null;
  child.stdout.on('data',chunk=>{buffer=Buffer.concat([buffer,chunk]);if(buffer.length>70000){pending?.reject(Error('helper_invalid'));child.kill();return;}
    while(buffer.length>=8){const h=buffer.readUInt32LE(0),n=buffer.readUInt32LE(4);if(h>65536||n>768){pending?.reject(Error('helper_invalid'));child.kill();return;}if(buffer.length<8+h+n)return;
      try{const reply=JSON.parse(buffer.subarray(8,8+h)),payload=Buffer.from(buffer.subarray(8+h,8+h+n));buffer=buffer.subarray(8+h+n);if(!pending||reply.id!==pending.id)throw Error('helper_invalid');clearTimeout(pending.timer);pending.resolve({reply,payload});pending=null;}catch{pending?.reject(Error('helper_invalid'));child.kill();return;}
    }
  });
  for(const event of ['error','exit'])child.on(event,()=>{if(pending){clearTimeout(pending.timer);pending.reject(Error('helper_unavailable'));pending=null;}});
  child.stdin.on('error',()=>{if(pending){clearTimeout(pending.timer);pending.reject(Error('helper_unavailable'));pending=null;}});
  const call=(op,payload=Buffer.alloc(0))=>new Promise((resolve,reject)=>{if(pending)fail();const id=++seq,header=Buffer.from(JSON.stringify({id,...op})),sizes=Buffer.alloc(8);sizes.writeUInt32LE(header.length);sizes.writeUInt32LE(payload.length,4);const timer=setTimeout(()=>{pending=null;child.kill();reject(Error('helper_timeout'));},1500);pending={id,resolve,reject,timer};child.stdin.write(Buffer.concat([sizes,header,payload]));});
  const hashes=new Set();
  async function embedding(file,replay=false){const pcm=readSelected(root,file),hash=crypto.createHash('sha256').update(pcm).digest('hex');if(hashes.has(hash)&&!replay)fail();hashes.add(hash);const generation=seq+1;
    const {reply,payload}=await call({op:'speaker_embed',model_id:MODEL,sample_rate:16000,utterance_generation:generation},pcm);
    if(reply.kind!=='embedding'||reply.model_id!==MODEL||reply.dimension!==192||reply.sample_rate!==16000||reply.utterance_generation!==generation||reply.format!=='f32le'||payload.length!==768)fail();return normalize(Array.from({length:192},(_,i)=>payload.readFloatLE(i*4)));
  }
  try{
    if((await call({op:'hello'})).reply.protocol!==4)fail();const loaded=(await call({op:'load_speaker',model_id:MODEL,models_root:path.resolve(opts['--model-root'])})).reply;if(loaded.kind!=='speaker_loaded'||loaded.dimension!==192||loaded.sample_rate!==16000)fail();
    const people=[];for(const p of dataset.people){if(p.references?.length!==3)fail();const references=[];for(const file of p.references)references.push(await embedding(file));people.push({id:p.id,references});}
    const cases=[];for(const c of dataset.cases)cases.push({...c,vector:await embedding(c.file,c.kind==='replay')});
    const report=evaluate(people,cases,dataset.source_class);fs.writeFileSync(path.resolve(opts['--report']),JSON.stringify(report,null,2)+'\n',{flag:'wx'});console.log(JSON.stringify({status:'diagnostic_report_written',samples:cases.length,production_ready:false}));
  }finally{child.stdin.end();if(child.exitCode===null)child.kill();}
}
if(process.argv[1]&&path.resolve(process.argv[1])===fileURLToPath(import.meta.url))main().catch(()=>{console.error('NV16_DIAGNOSTIC_FAILED: check consent, local WAV format, model/helper version and dataset; no settings changed.');process.exitCode=1;});
