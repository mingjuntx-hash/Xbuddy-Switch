import test from 'node:test';
import assert from 'node:assert/strict';
import templates from '../src/monitor/codex-internal-prompts.json' with { type: 'json' };
import { isInternalCodexPrompt, visibleSession } from '../src/monitor/session-visibility.js';
import { CodexLivePoller } from '../collector/lib/codex-live.js';
import { Hub } from '../collector/lib/hub.js';
import { createRailModel } from '../src/desktop/rail-model.js';
import { createNotificationTracker } from '../src/monitor/model.js';

test('known internal templates are suppressed throughout the hook lifecycle', () => {
  const hub = new Hub(), poller = new CodexLivePoller(hub, {home:'/tmp/unused'});
  templates.forEach((prompt,i) => {
    const session_id=String(i);
    poller.ingestHook({session_id,hook_event_name:'SessionStart'});
    assert.equal(poller.ingestHook({session_id,hook_event_name:'UserPromptSubmit',prompt:prompt.replaceAll(' ','\n')}),false);
    for (const hook_event_name of ['PreToolUse','PostToolUse','Stop','SessionStart']) assert.equal(poller.ingestHook({session_id,hook_event_name}),false);
    assert.equal(hub.sessions.has(`codex:${session_id}`),false);
  });
});
test('normal user tasks, quoted templates, empty titles, and other sources remain visible',()=>{
  for(const prompt of ['', '帮我做个性化建议', 'Memory Writing Agent', 'Explain this: '+templates[0]]) assert.equal(isInternalCodexPrompt(prompt),false);
  assert.equal(visibleSession({source:'codeg',title:templates[0]}),true);
});
test('rail removes an already visible internal session from an older runtime snapshot',()=>{
  const model=createRailModel();model.connect('connected');
  const state=title=>({sessions:[{id:'codex:x',source:'codex',status:'running',title}],sources:{codex:{state:'ok'}}});
  model.accept(state(''));assert.equal(model.items.length,1);
  model.accept(state(templates[0]));assert.equal(model.items.length,0);
  model.accept(state('普通用户会话'));assert.equal(model.items.length,1);
});

test('Codeg title generation hooks do not create a second completion card',()=>{
  const hub=new Hub(),poller=new CodexLivePoller(hub,{home:'/tmp/unused'});
  hub.ready=true;
  const tracker=createNotificationTracker();
  tracker.ingest(hub.snapshot());
  const prompt=`${templates.at(-1)} Capture the main topic concisely; include the user's message only as context.`;
  poller.ingestHook({session_id:'title-generator',hook_event_name:'SessionStart'});
  assert.equal(poller.ingestHook({session_id:'title-generator',hook_event_name:'UserPromptSubmit',prompt}),false);
  assert.equal(poller.ingestHook({session_id:'title-generator',hook_event_name:'Stop'}),false);
  hub.ingest({source:'codeg',sessionId:'290',type:'start',roundId:'codeg-round',ts:Date.now(),title:'真实 Codeg 会话',agentType:'codex'});
  hub.ingest({source:'codeg',sessionId:'290',type:'end',roundId:'codeg-round',ts:Date.now()+1,status:'done'});
  const snapshot=hub.snapshot();
  assert.deepEqual(snapshot.sessions.map(s=>s.id),['codeg:290']);
  assert.deepEqual(tracker.ingest(snapshot).map(e=>e.sessionId),['codeg:290']);
});
