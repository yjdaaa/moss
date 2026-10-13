import test from "node:test";
import assert from "node:assert/strict";
import { applyJobPoll, markStopRequested, jobSafetyNotice, startJobPolling } from "../src/approvalJobMonitor.js";
const job = () => ({ id:"wf1", kind:"workflow", status:"sending", progress:{sent:1,total:10,target:""} });
test("poll failures preserve job and progress; a later successful reply recovers", () => {
  let state=job();
  for(let i=0;i<7;i++) state=applyJobPoll(state,"wf1","workflow",{ok:false});
  assert.equal(state.status,"sending"); assert.equal(state.pollFailures,7);
  assert.deepEqual(state.progress,job().progress); assert.match(jobSafetyNotice(state),/状态未知/);
  state=applyJobPoll(state,"wf1","workflow",{ok:true,data:{success:true,result:{status:"sending",send_result:{lines_sent:4,total_lines:10}}}});
  assert.equal(state.connectionLost,false); assert.equal(state.pollFailures,0); assert.equal(state.progress.sent,4);
});
test("successful HTTP without a real status remains unknown", () => {
  const state=applyJobPoll(job(),"wf1","workflow",{ok:true,data:{success:true,result:{}}});
  assert.equal(state.status,"sending"); assert.equal(state.connectionLost,true);
});
test("stop acknowledgement is not a terminal state and stale replies cannot replace a new job", () => {
  const state=markStopRequested(job(),"wf1");
  assert.equal(state.status,"sending"); assert.match(jobSafetyNotice(state),/尚未确认/);
  assert.equal(applyJobPoll(state,"old","workflow",{ok:false}),state);
  assert.equal(markStopRequested(state,"old"),state);
  const done=applyJobPoll(state,"wf1","workflow",{ok:true,data:{success:true,result:{status:"completed"}}});
  assert.equal(done.status,"completed");
});
test("poller bounds timeout, does not overlap requests, and ignores replies after cleanup", async () => {
  let resolve, calls=0, delivered=0, signal;
  const stop=startJobPolling((s)=>{calls++; signal=s; return new Promise(r=>{resolve=r;});},()=>delivered++,{intervalMs:5,timeoutMs:200});
  await new Promise(r=>setTimeout(r,35)); assert.equal(calls,1);
  stop(); assert.equal(signal.aborted,true); resolve({ok:true});
  await new Promise(r=>setTimeout(r,15)); assert.equal(delivered,0); assert.equal(calls,1);
  let timeoutDelivered; const got=new Promise(r=>timeoutDelivered=r);
  const cancel=startJobPolling(()=>new Promise(()=>{}),r=>timeoutDelivered(r),{intervalMs:100,timeoutMs:15});
  assert.equal((await got).ok,false); cancel();
});
