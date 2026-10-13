import {test} from 'node:test';import assert from 'node:assert/strict';
import {evaluate,pcm16,normalize,helperEnvironment} from '../calibrate-nv16-speaker.mjs';
const vector=i=>Array.from({length:192},(_,n)=>n===i?1:0);
const people=['a','b','c'].map((id,i)=>({id,references:[vector(i),vector(i),vector(i)]}));
test('helper receives no inherited credentials, MOSS config or user profile',()=>{
 const key='NV16_SYNTHETIC_PRIVATE_ENV',old=process.env[key];process.env[key]='SYNTHETIC_NOT_SECRET';
 try{const env=helperEnvironment();assert.equal(env[key],undefined);assert(Object.keys(env).every(k=>['SystemRoot','WINDIR','TEMP','TMP'].includes(k)));assert.equal(env.USERPROFILE,undefined);assert.equal(env.OPENAI_API_KEY,undefined);assert.equal(env.ANTHROPIC_API_KEY,undefined);}finally{if(old===undefined)delete process.env[key];else process.env[key]=old;}
});
test('candidate metrics separate genuine, wrong person, unknown, noise and replay; never enable production',()=>{
 const cases=[...people.map((p,i)=>({kind:'genuine',expected:p.id,vector:vector(i)})),{kind:'genuine',expected:'a',vector:vector(1)},{kind:'impostor',expected:null,vector:vector(8)},{kind:'replay',expected:null,vector:vector(0)},{kind:'noise',expected:null,vector:vector(0)}];
 const r=evaluate(people,cases,'synthetic'),c=r.candidates[0];assert.equal(c.FRR,1/4);assert.equal(c.wrong_person,1);assert.equal(c.FAR,2/3);assert(c.FAR_wilson95_upper>c.FAR);assert.equal(c.negative_categories.replay.false_accepts,1);assert.equal(c.negative_categories.noise.false_accepts,1);assert.equal(r.production_ready,false);assert.equal(r.accepted_threshold,null);assert.equal(r.labels_verified,false);
});
test('no negative data reports unavailable FAR instead of false zero; human labels alone do not authorize',()=>{
 const r=evaluate(people,[],'human_claimed');assert.equal(r.candidates[0].FAR,null);assert.equal(r.candidates[0].FRR,null);assert.equal(r.candidates[0].FAR_wilson95_upper,null);assert.equal(r.production_ready,false);assert.equal(r.coverage.each_person_twenty_genuine,false);
});
test('ambiguous candidates do not choose an identity',()=>{
 const r=evaluate(people,[{kind:'genuine',expected:'a',vector:vector(0).map((v,i)=>i===1?1:v)}],'synthetic');assert(r.candidates.every(c=>c.false_rejects===1&&c.wrong_person===0));
});
test('invalid and inconsistent embeddings and fake labels fail closed',()=>{
 for(const v of [[],Array(192).fill(0),Array(192).fill(NaN)])assert.throws(()=>normalize(v));
 assert.throws(()=>evaluate(people.slice(0,2),[],'synthetic'));assert.throws(()=>evaluate([...people,{id:'a',references:[vector(0),vector(0),vector(0)]}],[],'synthetic'));
 assert.throws(()=>evaluate(people,[{kind:'impostor',expected:'a',vector:vector(0)}],'synthetic'));
 assert.throws(()=>evaluate([{id:'a',references:[vector(0),vector(1),vector(0)]},...people.slice(1)],[],'synthetic'));
});
test('PCM parser accepts bounded 16k mono WAV and rejects unsupported rate, length and malformed RIFF',()=>{
 const b=Buffer.alloc(44+64000);b.write('RIFF');b.writeUInt32LE(b.length-8,4);b.write('WAVEfmt ',8);b.writeUInt32LE(16,16);b.writeUInt16LE(1,20);b.writeUInt16LE(1,22);b.writeUInt32LE(16000,24);b.writeUInt32LE(32000,28);b.writeUInt16LE(2,32);b.writeUInt16LE(16,34);b.write('data',36);b.writeUInt32LE(64000,40);assert.equal(pcm16(b).length,64000);
 const bad=Buffer.from(b);bad.writeUInt32LE(48000,24);assert.throws(()=>pcm16(bad));assert.throws(()=>pcm16(b.subarray(0,44)));assert.throws(()=>pcm16(Buffer.alloc(44)));
});
