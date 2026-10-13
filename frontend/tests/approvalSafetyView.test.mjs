import test from "node:test";
import assert from "node:assert/strict";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";
import { applyJobPoll, jobSafetyNotice } from "../src/approvalJobMonitor.js";

test("actual overlay keeps emergency control when polling fails or a backend terminal state is shown", async () => {
  const server = await createServer({ configFile: false, appType: "custom", server: { middlewareMode: true, hmr: false, watch: null } });
  try {
    const { SendOverlay } = await server.ssrLoadModule("/src/components/SendOverlay.jsx");
    let job = { id:"wf1", kind:"workflow", status:"sending", progress:null };
    for(let i=0;i<7;i++) job=applyJobPoll(job,"wf1","workflow",{ok:false});
    const view = (state) => renderToStaticMarkup(React.createElement(SendOverlay, {
      visible:true, status:state.status, onCancel:()=>{}, onClose:()=>{}, onEmergencyStop:()=>{}, keepEmergencyStop:true, safetyNotice:jobSafetyNotice(state),
    }));
    const unknown = view(job);
    assert.match(unknown,/id="send-estop"/); assert.match(unknown,/状态未知/);
    assert.doesNotMatch(unknown,/发送失败/);
    assert.match(view({...job,status:"failed"}),/id="send-estop"/);
    const recovered=applyJobPoll(job,"wf1","workflow",{ok:true,data:{success:true,result:{status:"sending"}}});
    assert.match(view(recovered),/id="send-estop"/); assert.doesNotMatch(view(recovered),/状态未知/);
  } finally { await server.close(); }
});
