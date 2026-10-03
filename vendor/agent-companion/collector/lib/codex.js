// Codex mapping migrated from hy4's adapter, preserving explicit lifecycle events.
export function timestamp(value, fallback = Date.now()) { const n = typeof value === 'number' ? value : Date.parse(value); return Number.isFinite(n) && n > 0 ? n : fallback; }
function contentText(content) { return typeof content === 'string' ? content : Array.isArray(content) ? content.map(c => c.text || '').join('\n') : ''; }
export function questionDetails(input) {
  try {
    const p=typeof input==='string'?JSON.parse(input):input;
    return (Array.isArray(p?.questions)?p.questions:[]).map(q=>({
      text:String(q.question||q.title||''), header:String(q.header||''),
      options:(Array.isArray(q.options)?q.options:[]).map(o=>typeof o==='string'?{label:o,description:''}:{label:String(o.label||''),description:String(o.description||'')})
    }));
  } catch { return []; }
}
function question(input) { return questionDetails(input).map(q=>q.text).filter(Boolean).join('\n')||'等待用户输入'; }
// Codex answers arrive as `<send_user_message_question_reply>` plus a JSON array
// whose `questionItemId` embeds `[tool, callId, index]`. A missing envelope or
// invalid JSON yields no answer instead of an error.
export function questionReplyIds(prompt) {
  const body = typeof prompt === 'string' ? prompt : '';
  if (!body.includes('<send_user_message_question_reply>')) return [];
  const start = body.indexOf('['), end = body.lastIndexOf(']');
  if (start < 0 || end < start) return [];
  try {
    const items = JSON.parse(body.slice(start, end + 1));
    return (Array.isArray(items) ? items : []).flatMap(item => {
      if (typeof item?.questionItemId !== 'string') return [];
      try {
        const ids = JSON.parse(item.questionItemId);
        return Array.isArray(ids) && typeof ids[1] === 'string' && ids[1] ? [ids[1]] : [];
      } catch { return []; }
    });
  } catch { return []; }
}
export function codexRecord(rec, ctx, emit) {
  const p = rec.payload || {}, type = p.type, ts = timestamp(rec.timestamp);
  if (rec.type === 'session_meta') { ctx.sessionId = p.id || p.session_id || ctx.sessionId; ctx.cwd = p.cwd || ctx.cwd; }
  if (rec.type === 'turn_context') { ctx.cwd = p.cwd || ctx.cwd; ctx.roundId = p.turn_id || ctx.roundId; }
  const send = ev => emit({ source: 'codex', sessionId: ctx.sessionId, cwd: ctx.cwd, ts,
    ...(['wait','resolve'].includes(ev.type) && ctx.roundId ? {roundId:ctx.roundId} : {}), ...ev });
  if (rec.type === 'session_meta' || rec.type === 'turn_context') { send({ type: 'meta', roundId: ctx.roundId }); return; }
  if (type === 'task_started') { ctx.asyncQuestions = new Set(); ctx.roundId = p.turn_id || `turn:${ts}`; send({ type: 'start', roundId: ctx.roundId }); }
  else if (['task_complete','task_failed','turn_aborted'].includes(type)) send({ type: 'end', roundId: p.turn_id || ctx.roundId, status: type === 'task_complete' ? 'done' : type === 'turn_aborted' ? 'aborted' : 'error' });
  else if (type === 'user_message' || type === 'message' && p.role === 'user') {
    // User messages carry titles; task_started is the sole explicit round boundary.
    const text = p.message || contentText(p.content), trimmed = text.trim();
    if (trimmed.startsWith('<send_user_message_question_reply>')) {
      // Answers clear their exact call and never become a session title.
      for (const callId of questionReplyIds(text)) send({ type: 'resolve', callId });
    } else if (trimmed && !trimmed.startsWith('<') && !text.startsWith('The following is the Codex agent history')) {
      // A real user message supersedes every unanswered async question.
      for (const callId of ctx.asyncQuestions || []) send({ type: 'resolve', callId });
      ctx.asyncQuestions?.clear();
      send({ type: 'meta', title: text });
    }
  } else if (['function_call','custom_tool_call'].includes(type)) {
    const name = p.name || 'tool', callId = p.call_id || p.id || `${ts}:${rec.ordinal ?? name}`;
    // Async calls stay pending until answered; remember them only so their
    // outputs do not emit a resolve on their behalf.
    const async = /(?:^|__|\.)request_user_input_async$/.test(name);
    if (async) (ctx.asyncQuestions ||= new Set()).add(callId);
    if (async || /(?:^|__|\.)(request_user_input|AskUserQuestion|ask_user_question|RequestUserInput)$/.test(name)) {
      send({ type: 'wait', callId, tool: name, text: question(p.arguments || p.input), questions: questionDetails(p.arguments || p.input), ...(async ? {optional:true} : {}) });
    }
    else send({ type: 'step', eventId: callId, label: name });
  } else if (['function_call_output','custom_tool_call_output'].includes(type)) {const callId=p.call_id||p.id;if(!ctx.asyncQuestions?.has(callId))send({type:'resolve',callId});}
  else if (type === 'token_count') send({ type: 'tokens', tokens: p.info?.total_token_usage?.total_tokens });
  else if (type === 'reasoning' || type === 'message') send({ type: 'activity' });
}
export { question, contentText };
