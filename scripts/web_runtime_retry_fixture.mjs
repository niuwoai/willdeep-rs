// Serve the production React app against an isolated, deterministic Runtime fixture.
// No provider credentials, daemon, real workspace, or outbound API proxy is used.
import { createServer } from '../web/node_modules/vite/dist/node/index.js';
import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';

const workspace = '/qa/runtime-retry';
const version = JSON.parse(readFileSync(new URL('../web/package.json', import.meta.url))).version;
let phase = 'waiting';
const requests = [];
let chatMode = 'waiting';
let chat = null;
let chatCounter = 0;
// 下一句预测的假回包；null 表示服务端判定不给。
let suggestion = '提交并合回 develop';
const submitChat = () => {
  if (!chat || chat.submitted || chat.closed) return;
  chat.submitted = true;
  const events = chatMode === 'complete'
    ? [
      { type: 'submitted', session_id: chat.session, turn_id: chat.turn, cursor: 1 },
      { type: 'completed', session_id: chat.session, turn_id: chat.turn, text: '测试已全部通过。要不要我提交？', cursor: 2 },
    ]
    : [
      { type: 'submitted', session_id: chat.session, turn_id: chat.turn, cursor: 1 },
      { type: 'provider_retry_wait', label: '等待重试 · 60s', cursor: 2 },
    ];
  for (const event of events) chat.response.write(`data: ${JSON.stringify(event)}\n\n`);
  if (chatMode === 'complete') { chat.closed = true; chat.response.end(); }
};
const agent = () => ({
  id: 'qa-child', parent_id: 'qa-root', label: null, background: true,
  profile: 'reader', model: null, status: phase === 'stopped' ? 'cancelled' : 'working',
  current_turn: 1, current_tool: null,
  retry_wait: phase === 'waiting' ? { attempt: 2, delay_ms: 60000 } : null,
  total_tokens: 8, elapsed_seconds: 12, finished_seconds_ago: phase === 'stopped' ? 0 : null,
  worktree_branch: null, dedicated_worktree: false,
});
const server = await createServer({
  configFile: false,
  root: fileURLToPath(new URL('../web', import.meta.url)),
  server: { host: '127.0.0.1', port: 19849, strictPort: true },
  plugins: [{ name: 'runtime-retry-fixture', configureServer(vite) {
    vite.middlewares.use(async (request, response, next) => {
      const url = new URL(request.url, 'http://127.0.0.1');
      const reply = (value, status = 200) => {
        response.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' });
        response.end(JSON.stringify(value));
      };
      if (url.pathname === '/__qa/state') return reply({ phase, requests, chat: chat && { turn: chat.turn, session: chat.session, submitted: chat.submitted, stopped: chat.stopped, closed: chat.closed } });
      if (url.pathname === '/__qa/suggestion' && request.method === 'POST') {
        const value = url.searchParams.get('value');
        suggestion = value === 'null' ? null : value;
        return reply({ suggestion });
      }
      if (url.pathname === '/__qa/chat-mode' && request.method === 'POST') {
        const value = url.searchParams.get('value');
        if (!['waiting', 'pending', 'complete'].includes(value)) return reply({}, 400);
        chatMode = value;
        return reply({ chatMode });
      }
      if (url.pathname === '/__qa/submit-chat' && request.method === 'POST') {
        submitChat();
        return reply({ ok: true });
      }
      if (url.pathname === '/__qa/phase' && request.method === 'POST') {
        const value = url.searchParams.get('value');
        if (!['waiting', 'running', 'stopped'].includes(value)) return reply({}, 400);
        phase = value;
        return reply({ phase });
      }
      if (url.pathname === '/health') return reply({ status: 'ok', version });
      if (!url.pathname.startsWith('/api/')) return next();
      requests.push({ method: request.method, path: url.pathname });
      if (url.pathname === '/api/chat/stream' && request.method === 'POST') {
        let body = '';
        for await (const chunk of request) body += chunk;
        if (JSON.parse(body).workspace !== workspace) return reply({ error: 'wrong fixture workspace' }, 400);
        const id = ++chatCounter;
        const current = { turn: `qa-turn-${id}`, session: `qa-session-${id}`, submitted: false, stopped: false, closed: false, response };
        chat = current;
        response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-store' });
        response.flushHeaders();
        response.on('close', () => { current.closed = true; });
        if (chatMode !== 'pending') submitChat();
        return;
      }
      if (chat && url.pathname === `/api/turns/${chat.turn}/stop` && request.method === 'POST') {
        if (!chat.submitted) return reply({ error: 'turn not submitted' }, 409);
        chat.stopped = true;
        return reply({ ok: true });
      }
      const suggestionPath = url.pathname.match(/^\/api\/sessions\/([^/]+)\/input-suggestion$/);
      if (suggestionPath && request.method === 'POST') {
        let body = '';
        for await (const chunk of request) body += chunk;
        const { turn_id } = JSON.parse(body);
        requests[requests.length - 1].turn_id = turn_id;
        return reply({ suggestion, turn_id });
      }
      const detailPath = url.pathname.match(/^\/api\/sessions\/(qa-session-\d+)$/);
      if (detailPath && request.method === 'GET') {
        return reply({ id: detailPath[1], messages: [
          { role: 'user', content: '把测试修好', attachment_count: 0 },
          { role: 'assistant', content: '测试已全部通过。要不要我提交？', attachment_count: 0 },
        ] });
      }
      if (url.pathname === '/api/workspaces') return reply([{ id: 'qa', path: workspace, name: 'QA', active: true, access: 'read_only' }]);
      if (url.pathname === '/api/sessions') return reply([]);
      if (url.pathname === '/api/composer') return reply({ commands: [], skills: [] });
      if (url.pathname === '/api/runtime/events') return reply([]);
      if (url.pathname === '/api/runtime/activity') return reply({ agents: [agent()], tools: [], artifacts: [], tasks: [], gates: [], attention_count: 0 });
      if (url.pathname === '/api/plugins') return reply({ plugins: [], failures: [] });
      if (url.pathname === '/api/runtime/agents/qa-child/stop' && request.method === 'POST') {
        let body = '';
        for await (const chunk of request) body += chunk;
        const args = JSON.parse(body);
        if (args.workspace !== workspace) return reply({ error: 'wrong fixture workspace' }, 400);
        phase = 'stopped';
        return reply({ ok: true });
      }
      return reply({ error: 'unsupported fixture operation' }, 404);
    });
  } }],
});
await server.listen();
server.printUrls();
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, async () => { await server.close(); process.exit(0); });
