import test from 'node:test';
import assert from 'node:assert/strict';
import {mcpForm,mcpFormError,mcpSavePayload,mcpConfirmation,mcpConnectionText} from '../src/voiceMcpState.js';
import {approvalCheckText,approvalErrorText,kindLabel} from '../src/approvalState.js';

test('NV9 默认服务停用；凭据不会回填，密码只进入独立 secrets 字段',()=>{
 const form=mcpForm(); assert.equal(form.enabled,false);
 Object.assign(form,{name:'notes',url:'https://example.test/mcp',enabled:true,credentials:[{key:'authorization',value:'Bearer test-secret',saved:false}]});
 assert.equal(mcpFormError(form),'');
 const payload=mcpSavePayload(form,'revision');
 assert.equal(payload.config.transport.header_refs.authorization,'voice.mcp.notes.header.authorization');
 assert.equal(payload.secrets['voice.mcp.notes.header.authorization'],'Bearer test-secret');
 assert.equal(JSON.stringify(payload.config).includes('test-secret'),false);
 const ref='voice.mcp.notes.header.authorization.v-'+ 'a'.repeat(32);
 const edited=mcpForm({config:{...payload.config,transport:{...payload.config.transport,header_refs:{authorization:ref}}},secret_set:{[ref]:true}});
 assert.equal(edited.credentials[0].value,'');
 assert.equal(edited.credentials[0].saved,true);
 assert.deepEqual(mcpSavePayload(edited,'revision').secrets,{});
 assert.equal(mcpSavePayload(edited,'revision').config.transport.header_refs.authorization,ref);
});
test('NV9 URL、保留名称、重复凭据、协议字段与参数数组校验',()=>{
 const good={...mcpForm(),name:'notes',url:'https://example.test/mcp'};
 for(const name of ['', 'device','DEVICE','a__b','a.b','a'.repeat(33)]) assert.ok(mcpFormError({...good,name}));
 for(const url of ['file:///tmp/a','https://u:p@example.test/mcp','https://example.test/mcp?token=x','https://example.test/mcp#x']) assert.ok(mcpFormError({...good,url}));
 for(const key of ['Host','host','mcp-session-id','content-type','connection','accept']) assert.ok(mcpFormError({...good,credentials:[{key,value:'x'}]}));
 assert.ok(mcpFormError({...good,url:'http://example.test/mcp',credentials:[{key:'authorization',value:'x'}]}));
 assert.ok(mcpFormError({...good,credentials:[{key:'authorization',value:'x'},{key:'authorization',value:'y'}]}));
 assert.ok(mcpFormError({...good,enabled:true,credentials:[{key:'authorization',value:'',saved:false}]}));
 const local={...good,type:'stdio',command:'node',args:'["server.js", "a b"]'};
 assert.equal(mcpFormError(local),''); assert.deepEqual(mcpSavePayload(local,'r').config.transport.args,['server.js','a b']);
 for(const args of ['not json','{}','[1]','[null]']) assert.ok(mcpFormError({...local,args}));
});
test('NV9 免批准风险文案、重连状态与第三方审批不冒充激光加工',()=>{
 const form={...mcpForm(),name:'local',type:'stdio',command:'node',enabled:true,policies:{read:'allow'}};
 assert.match(mcpConfirmation(form),/文件和网络权限/);assert.match(mcpConfirmation(form),/免批准/);
 assert.equal(mcpConnectionText({state:'reconnecting'}),'等待重连');
 assert.equal(kindLabel('external_tool'),'调用第三方工具');
 assert.match(approvalCheckText('external_tool'),/数据访问/);assert.doesNotMatch(approvalCheckText('external_tool'),/机器旁/);
 assert.match(approvalCheckText('safe_action'),/机器旁/);
 assert.match(approvalErrorText('approval_mismatch','','external_tool'),/第三方配置/);
 assert.match(approvalErrorText('result_unknown','','external_tool'),/不会自动重试/);
});
