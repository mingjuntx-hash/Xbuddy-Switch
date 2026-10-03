import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {Hub} from '../collector/lib/hub.js';
import {CodexLivePoller} from '../collector/lib/codex-live.js';

test('pure Codex hook lifecycle handles answers, approvals, concurrency, ordering, and interruption without file reads',async t=>{
 const fixture=JSON.parse(await fs.readFile(new URL('./fixtures/codex-hooks.json',import.meta.url),'utf8'));
 const home=await fs.mkdtemp(path.join(os.tmpdir(),'codex-hook-only-'));t.after(()=>fs.rm(home,{recursive:true,force:true}));
 const hub=new Hub(),poller=new CodexLivePoller(hub,{home});
 const methods=['readFile','open','stat','access','readdir'];const originals=Object.fromEntries(methods.map(k=>[k,fs[k]]));
 try {
  for(const k of methods)fs[k]=()=>{throw Error(`Unexpected filesystem access: ${k}`)};
  for(const [i,c] of fixture.entries()) {
   poller.ingestHook({session_id:'x',turn_id:'r',cwd:'/project',timestamp:Date.now()+i,transcript_path:'/must-not-read.jsonl',...c.hook});
   await poller.poll();
   assert.equal(hub.sessions.get('codex:x').status,c.status,`case ${i}`);
   assert.equal(hub.sessions.get('codex:x').pending.length,c.pending,`case ${i}`);
  }
  const restarted=new CodexLivePoller(new Hub(),{home});await restarted.poll();assert.equal(restarted.hub.sessions.size,0);
 } finally {for(const k of methods)fs[k]=originals[k];}
});

test('permission checks are neutral, silent, and do not hide a real question', async()=>{
 const {sessionPresentation}=await import('../src/monitor/presentation.js');
 const {createNotificationTracker}=await import('../src/monitor/model.js');
 const hub=new Hub();hub.ready=true;
 const tracker=createNotificationTracker();tracker.ingest(hub.snapshot());
 const poller=new CodexLivePoller(hub,{home:'/unused'});
 let ts=Date.now();const hook=(event,extra={})=>poller.ingestHook({session_id:'x',turn_id:'r',hook_event_name:event,timestamp:++ts,...extra});
 hook('UserPromptSubmit');
 hook('PermissionRequest',{tool_name:'Bash',tool_use_id:'a'});
 let session=hub.sessions.get('codex:x');
 assert.equal(session.status,'running');assert.equal(sessionPresentation(session).statusLabel,'权限检查中');
 assert.deepEqual(tracker.ingest(hub.snapshot()),[]);assert.equal(session.pending.length,0);
 hook('PreToolUse',{tool_name:'request_user_input',tool_use_id:'q'});
 assert.equal(sessionPresentation(session).statusLabel,'待确认');
 assert.equal(tracker.ingest(hub.snapshot()).filter(e=>e.kind==='wait').length,1);
 hook('PostToolUse',{tool_name:'Bash',tool_use_id:'a'});
 assert.equal(session.status,'wait');assert.equal(session.permissionChecks.length,0);
 hook('PostToolUse',{tool_name:'request_user_input',tool_use_id:'q'});
 assert.equal(sessionPresentation(session).statusLabel,'运行中');
 hook('PermissionRequest',{tool_name:'Bash',tool_use_id:'b'});
 hook('Stop');assert.equal(session.permissionChecks.length,0);
});

test('async questions notify once, answer by call id, and never title the session', async()=>{
 const {createNotificationTracker}=await import('../src/monitor/model.js');
 const {questionReplyIds}=await import('../collector/lib/codex.js');
 const hub=new Hub();hub.ready=true;
 const tracker=createNotificationTracker();tracker.ingest(hub.snapshot());
 const poller=new CodexLivePoller(hub,{home:'/unused'});
 let ts=Date.now();const hook=(event,extra={})=>poller.ingestHook({session_id:'x',turn_id:'r',hook_event_name:event,timestamp:++ts,...extra});
 const session=()=>hub.sessions.get('codex:x');
 const reply=tool=>`<send_user_message_question_reply>\n${JSON.stringify([{questionItemId:JSON.stringify([tool,tool,0]),answer:'A'}])}\n</send_user_message_question_reply>`;
 hook('UserPromptSubmit',{prompt:'Start'});
 for(const name of ['request_user_input_async','functions.request_user_input_async','mcp__codex__request_user_input_async']) {
  hook('PreToolUse',{tool_name:name,tool_use_id:name,tool_input:{questions:[{title:'偏好',options:[{label:'A',description:'甲'}]}]}});
  assert.equal(session().status,'wait');
  assert.equal(session().pending.length,1);
  assert.equal(session().pending[0].optional,true);
  assert.equal(session().pending[0].text,'偏好');
  assert.equal(session().pending[0].questions[0].options[0].description,'甲');
  assert.equal(tracker.ingest(hub.snapshot()).filter(e=>e.kind==='wait').length,1);
  hook('PostToolUse',{tool_name:name,tool_use_id:name,tool_response:{accepted:true}});
  assert.equal(session().pending.length,1);
  assert.deepEqual(tracker.ingest(hub.snapshot()),[]);
  hook('UserPromptSubmit',{prompt:reply(name)});
  assert.deepEqual(session().pending,[]);
  assert.equal(session().status,'running');
  assert.equal(session().title,'Start');
 }
 assert.equal(questionReplyIds(reply('missing')).length,1);
 assert.deepEqual(questionReplyIds('<send_user_message_question_reply>not json</send_user_message_question_reply>'),[]);
 assert.deepEqual(questionReplyIds('hello'),[]);
 assert.deepEqual(questionReplyIds('<send_user_message_question_reply>{}</send_user_message_question_reply>'),[]);
 assert.deepEqual(questionReplyIds('<send_user_message_question_reply>[{"questionItemId":["t","x",0]}]</send_user_message_question_reply>'),[]);
 assert.deepEqual(questionReplyIds(`<send_user_message_question_reply>\n${JSON.stringify([
  {questionItemId:JSON.stringify(['t','a',0]),answer:'A'},
  {questionItemId:JSON.stringify(['t','b',1]),answer:'B'},
 ])}\n</send_user_message_question_reply>`),['a','b']);
 hook('PreToolUse',{tool_name:'functions.request_user_input_async',tool_use_id:'bare'});
 assert.equal(session().pending[0].text,'等待用户输入');
 hook('UserPromptSubmit',{prompt:'   '});
 assert.equal(session().pending.length,1);
 assert.equal(session().title,'Start');
 hook('UserPromptSubmit',{prompt:reply('bare')});
 assert.deepEqual(session().pending,[]);
 // A synchronous question is not optional and survives an async completion.
 hook('PreToolUse',{tool_name:'functions.request_user_input',tool_use_id:'sync'});
 hook('PreToolUse',{tool_name:'functions.request_user_input_async',tool_use_id:'waiting'});
 hook('PostToolUse',{tool_name:'functions.request_user_input_async',tool_use_id:'waiting'});
 assert.deepEqual(session().pending.map(p=>[p.id,p.optional??false]),[['sync',false],['waiting',true]]);
 // Another internal envelope neither titles the session nor answers a question.
 hook('UserPromptSubmit',{prompt:'<in-app-browser-context url="x">ctx</in-app-browser-context>'});
 assert.equal(session().pending.length,2);
 assert.equal(session().title,'Start');
 // A real message clears every unanswered async item and titles the session.
 hook('UserPromptSubmit',{prompt:'Continue'});
 assert.deepEqual(session().pending.map(p=>p.id),['sync']);
 assert.equal(session().title,'Continue');
 hook('Stop');
 assert.equal(session().status,'done');
 assert.deepEqual(session().pending,[]);
});

test('a real prompt clears optional waits even after call bookkeeping is gone', () => {
 const hub=new Hub();hub.ready=true;
 const poller=new CodexLivePoller(hub,{home:'/unused'});
 poller.ingestHook({session_id:'x',turn_id:'r',hook_event_name:'UserPromptSubmit',prompt:'Start',timestamp:1});
 poller.ingestHook({session_id:'x',turn_id:'r',hook_event_name:'PreToolUse',tool_name:'functions.request_user_input_async',tool_use_id:'gone',timestamp:2});
 poller.live.get('x').calls.clear();
 poller.ingestHook({session_id:'x',turn_id:'r',hook_event_name:'UserPromptSubmit',prompt:'Follow up',timestamp:3});
 assert.deepEqual(hub.sessions.get('codex:x').pending,[]);
 assert.equal(hub.sessions.get('codex:x').title,'Follow up');
});
