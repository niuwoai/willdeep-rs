import { useEffect, useMemo, useRef, useState } from "react";
import { Box, Button, Container, Dialog, Flex, Heading, Input, NativeSelect, Portal, Text, Textarea, VStack } from "@chakra-ui/react";
import { detectLanguage, languageLabels, languages, messages, type Language } from "./i18n";
import { RuntimeSidebar, type AgentSpawnProfile, type RuntimeActivity } from "./RuntimeSidebar";
import { Markdown } from "./Markdown";
import { ModelRoutingSettings } from "./ModelRoutingSettings";

type Workspace = { id: string; path: string; name: string; active: boolean; access: "read_only" | "smart" | "workspace_write" };
type Session = { id: string; title: string; workspace: string; updated_at: number; pinned_at: number | null; archived: boolean; active: boolean; active_turn_id: string | null };
type SessionDetail = { id: string; messages: Array<{ role: "user" | "assistant"; content: string; attachment_count: number }> };
type RunStep = { id: string; label: string; status: "active" | "done" | "failed" };
type ChatMessage = { id: string; role: "user" | "assistant" | "activity"; content: string; steps?: RunStep[] };
type Attachment = { kind: "text"; name: string; content: string } | { kind: "image"; name: string; media_type: string; data: string; width: number; height: number };
type ComposerSkill = { identifier: string; name: string; description: string };
type ComposerData = { commands: string[]; skills: ComposerSkill[] };
type Health = { status: string; version: string };
type RuntimeStreamEvent = {
  type: string;
  cursor?: number;
  session_id?: string;
  turn_id?: string;
  text?: string;
  label?: string;
  id?: string;
  name?: string;
  is_error?: boolean;
  message?: string;
};
const defaultCommands = ["/help", "/goal", "/compress", "/skills", "/clear"];
const lastSessionPrefix = "willdeep.web.last-session.";
const runtimeCursorPrefix = "willdeep.web.runtime-cursor.";

async function json<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, init); const value = await response.json();
  if (!response.ok) throw new Error(value.error ?? response.statusText); return value as T;
}

async function mutate(url: string, init: RequestInit) {
  const response = await fetch(url, init);
  if (!response.ok) throw new Error((await response.text()) || response.statusText);
}

function nextId(prefix: string) { return `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2)}`; }

function clipboardImageFile(clipboard: DataTransfer): File | null {
  const file = Array.from(clipboard.files).find((candidate) => candidate.type.startsWith("image/"));
  if (file) return file;
  for (const item of Array.from(clipboard.items)) {
    if (item.kind !== "file" || !item.type.startsWith("image/")) continue;
    const candidate = item.getAsFile();
    if (candidate) return candidate;
  }
  return null;
}

function runtimeCursorKey(sessionId: string, turnId: string) {
  return `${runtimeCursorPrefix}${sessionId}.${turnId}`;
}

function savedRuntimeCursor(sessionId: string, turnId: string) {
  const value = Number(localStorage.getItem(runtimeCursorKey(sessionId, turnId)) ?? "0");
  return Number.isSafeInteger(value) && value >= 0 ? value : 0;
}

function saveRuntimeCursor(sessionId: string, turnId: string, cursor: number | undefined) {
  if (cursor === undefined || !Number.isSafeInteger(cursor) || cursor < 0) return;
  localStorage.setItem(runtimeCursorKey(sessionId, turnId), String(cursor));
}

function nextSseFrame(buffer: string): { frame: string; rest: string } | null {
  const lf = buffer.indexOf("\n\n");
  const crlf = buffer.indexOf("\r\n\r\n");
  if (lf < 0 && crlf < 0) return null;
  const useCrlf = crlf >= 0 && (lf < 0 || crlf < lf);
  const index = useCrlf ? crlf : lf;
  const width = useCrlf ? 4 : 2;
  return { frame: buffer.slice(0, index), rest: buffer.slice(index + width) };
}

async function readSse(response: Response, onEvent: (event: RuntimeStreamEvent) => Promise<void>) {
  if (!response.ok || !response.body) throw new Error(await response.text());
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    for (;;) {
      const next = nextSseFrame(buffer);
      if (!next) break;
      buffer = next.rest;
      const data = next.frame
        .split(/\r?\n/)
        .filter((line) => line.startsWith("data:"))
        .map((line) => line.slice(5).trimStart())
        .join("\n");
      if (data) await onEvent(JSON.parse(data) as RuntimeStreamEvent);
    }
  }
}

function waitForReconnect(delay: number, signal: AbortSignal) {
  return new Promise<void>((resolve) => {
    if (signal.aborted) { resolve(); return; }
    const timer = window.setTimeout(resolve, delay);
    signal.addEventListener("abort", () => { window.clearTimeout(timer); resolve(); }, { once: true });
  });
}

export function App() {
  const [language, setLanguage] = useState<Language>(detectLanguage); const t = messages[language];
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]); const [workspace, setWorkspace] = useState("");
  const [sessions, setSessions] = useState<Session[]>([]); const [sessionId, setSessionId] = useState("");
  const [sessionSearch, setSessionSearch] = useState("");
  const [skillSearch, setSkillSearch] = useState("");
  const [version, setVersion] = useState("");
  const [chat, setChat] = useState<ChatMessage[]>([]); const [prompt, setPrompt] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [goal, setGoal] = useState("");
  const [composer, setComposer] = useState<ComposerData>({ commands: defaultCommands, skills: [] });
  const [runtimeActivity, setRuntimeActivity] = useState<RuntimeActivity>({ tools: [], artifacts: [], agents: [], tasks: [], gates: [], attention_count: 0 });
  const [busy, setBusy] = useState(false); const [activity, setActivity] = useState(""); const [error, setError] = useState("");
  const abortRef = useRef<AbortController | null>(null); const endRef = useRef<HTMLDivElement | null>(null);
  const activeRunRef = useRef<string | null>(null);
  const activeTurnRef = useRef<string | null>(null);
  const activeSessionRef = useRef<string | null>(null);
  const [activeRuntimeSessionId, setActiveRuntimeSessionId] = useState("");
  const pendingStopRef = useRef(false);
  const loadSessionRef = useRef<(id: string, activeTurnId?: string | null) => Promise<void>>(async () => undefined);
  const chatViewportRef = useRef<HTMLDivElement | null>(null); const followBottomRef = useRef(true);

  useEffect(() => { document.title = t.documentTitle; document.documentElement.lang = language; localStorage.setItem("willdeep.language", language); }, [language, t.documentTitle]);
  useEffect(() => { json<Health>("/health").then((value) => setVersion(value.version)).catch(() => setVersion("")); }, []);
  useEffect(() => { json<Workspace[]>("/api/workspaces").then((values) => { setWorkspaces(values); setWorkspace(values[0]?.path ?? ""); }).catch((reason: Error) => setError(`${t.loadFailed}: ${reason.message}`)); }, [t.loadFailed]);
  useEffect(() => {
    if (!workspace) return;
    abortRef.current?.abort();
    setSessionId("");
    setChat([]);
    setShowArchived(false);
    json<Session[]>("/api/sessions").then((values) => {
      setSessions(values);
      const available = values.filter((item) => item.workspace === workspace && !item.archived);
      const remembered = localStorage.getItem(`${lastSessionPrefix}${workspace}`);
      const candidate = available.find((item) => item.id === remembered) ?? available.find((item) => item.active);
      if (candidate) void loadSessionRef.current(candidate.id, candidate.active_turn_id);
    }).catch((reason: Error) => setError(`${t.loadFailed}: ${reason.message}`));
    json<ComposerData>(`/api/composer?workspace=${encodeURIComponent(workspace)}`).then(setComposer).catch((reason: Error) => setError(`${t.loadFailed}: ${reason.message}`));
  }, [workspace, t.loadFailed]);
  useEffect(() => {
    if (!workspace) { setRuntimeActivity({ tools: [], artifacts: [], agents: [], tasks: [], gates: [], attention_count: 0 }); return; }
    let active = true;
    // 上一轮没回来就跳过这一轮，避免慢响应时请求堆积、把服务端 CPU 顶满。
    let inFlight = false;
    const refresh = () => {
      if (inFlight) return Promise.resolve();
      inFlight = true;
      return Promise.all([json<RuntimeActivity>(`/api/runtime/activity?workspace=${encodeURIComponent(workspace)}`), json<Session[]>("/api/sessions")])
        .then(([runtime, currentSessions]) => { if (active) { setRuntimeActivity(runtime); setSessions(currentSessions); } })
        .catch(() => undefined)
        .finally(() => { inFlight = false; });
    };
    void refresh(); const timer = window.setInterval(refresh, 2000);
    return () => { active = false; window.clearInterval(timer); };
  }, [workspace]);
  useEffect(() => { if (followBottomRef.current) endRef.current?.scrollIntoView({ behavior: "smooth", block: "end" }); }, [chat, activity]);

  const [showArchived, setShowArchived] = useState(false);
  const { liveSessions, archivedSessions } = useMemo(() => {
    const query = sessionSearch.trim().toLowerCase();
    const filtered = sessions
      .filter((item) => item.workspace === workspace && (!query || item.title.toLowerCase().includes(query)))
      .sort((a, b) => (b.pinned_at ?? 0) - (a.pinned_at ?? 0) || b.updated_at - a.updated_at);
    return {
      liveSessions: filtered.filter((item) => !item.archived),
      archivedSessions: filtered.filter((item) => item.archived),
    };
  }, [sessions, workspace, sessionSearch]);
  const selectedSession = useMemo(() => sessions.find((item) => item.id === sessionId), [sessions, sessionId]);
  const commandMatches = useMemo(() => prompt.startsWith("/") ? composer.commands.filter((item) => item.startsWith(prompt.split(/\s/)[0])).slice(0, 6) : [], [prompt, composer.commands]);
  const skillQuery = prompt.match(/(?:^|\s)\$([\w-]*)$/)?.[1]?.toLowerCase();
  const skillMatches = useMemo(() => {
    if (skillQuery === undefined) return [];
    const query = skillSearch.trim().toLowerCase() || skillQuery;
    return composer.skills.filter((skill) => `${skill.identifier} ${skill.name} ${skill.description}`.toLowerCase().includes(query)).slice(0, 8);
  }, [skillQuery, skillSearch, composer.skills]);

  function updateRun(runId: string, updater: (steps: RunStep[]) => RunStep[]) {
    setChat((current) => current.map((message) => message.id === runId ? { ...message, steps: updater(message.steps ?? []) } : message));
  }

  function applySessionDetail(detail: SessionDetail) {
    setSessionId(detail.id);
    localStorage.setItem(`${lastSessionPrefix}${workspace}`, detail.id);
    setChat(detail.messages.map((message, index) => ({
      id: `${detail.id}-${index}`,
      role: message.role,
      content: `${message.content}${message.attachment_count ? `\n[${message.attachment_count} ${t.attachmentCount}]` : ""}`,
    })));
    followBottomRef.current = true;
  }

  async function applyStreamEvent(event: RuntimeStreamEvent, runId: string) {
    if (event.type === "submitted" || event.type === "resumed") {
      const nextSessionId = event.session_id || activeSessionRef.current || "";
      const nextTurnId = event.turn_id || activeTurnRef.current;
      setSessionId(nextSessionId);
      setActiveRuntimeSessionId(nextSessionId);
      activeSessionRef.current = nextSessionId || null;
      activeTurnRef.current = nextTurnId || null;
      if (nextSessionId) localStorage.setItem(`${lastSessionPrefix}${workspace}`, nextSessionId);
      if (nextSessionId && nextTurnId) saveRuntimeCursor(nextSessionId, nextTurnId, event.cursor);
      if (event.type === "submitted" && pendingStopRef.current && activeTurnRef.current) await stopRemoteTurn(activeTurnRef.current);
      return { terminal: false as const };
    }
    const currentSessionId = event.session_id || activeSessionRef.current;
    const currentTurnId = event.turn_id || activeTurnRef.current;
    if (currentSessionId && currentTurnId) saveRuntimeCursor(currentSessionId, currentTurnId, event.cursor);
    if (event.type === "completed") {
      setActiveRuntimeSessionId("");
      activeSessionRef.current = null;
      activeTurnRef.current = null;
      return { terminal: true as const, text: event.text || t.emptyReply };
    }
    if (event.type === "error") return { terminal: true as const, error: event.message || t.requestFailed };
    if (event.type === "thought") setActivity(event.text || t.thinking);
    else if (event.type === "turn_started") {
      setActivity(event.label || t.thinking);
      const stepId = event.id || `turn-${event.cursor ?? nextId("turn")}`;
      updateRun(runId, (steps) => {
        const updated = steps.map((step) => step.status === "active" ? { ...step, status: "done" as const } : step);
        return updated.some((step) => step.id === stepId) ? updated : [...updated, { id: stepId, label: event.label || t.thinking, status: "active" }];
      });
    }
    else if (event.type === "tool_requested") {
      setActivity(event.label || t.toolRunning);
      const stepId = event.id || `tool-${event.cursor ?? nextId("tool")}`;
      updateRun(runId, (steps) => steps.some((step) => step.id === stepId) ? steps : [...steps, { id: stepId, label: event.label || t.toolRunning, status: "active" }]);
    }
    else if (event.type === "tool_completed") {
      setActivity(event.label || (event.is_error ? t.toolFailed : t.toolDone));
      const stepId = event.id || `tool-${event.cursor ?? nextId("tool")}`;
      updateRun(runId, (steps) => steps.some((step) => step.id === stepId)
        ? steps.map((step) => step.id === stepId ? { ...step, label: event.label || step.label, status: event.is_error ? "failed" : "done" } : step)
        : [...steps, { id: stepId, label: event.label || (event.is_error ? t.toolFailed : t.toolDone), status: event.is_error ? "failed" : "done" }]);
    }
    else setActivity(event.label || event.type);
    return { terminal: false as const };
  }

  async function stopRemoteTurn(turnId: string) {
    const response = await fetch(`/api/turns/${encodeURIComponent(turnId)}/stop`, { method: "POST" });
    if (!response.ok) throw new Error(await response.text());
    abortRef.current?.abort(); setActivity(t.stopped); setBusy(false); abortRef.current = null; activeTurnRef.current = null; activeSessionRef.current = null;
    setActiveRuntimeSessionId("");
    if (activeRunRef.current) updateRun(activeRunRef.current, (steps) => steps.map((step) => step.status === "active" ? { ...step, label: t.stopped, status: "failed" } : step));
  }

  async function stop() {
    pendingStopRef.current = true;
    setActivity(t.stopping);
    const turnId = activeTurnRef.current;
    if (!turnId) return;
    try {
      await stopRemoteTurn(turnId);
    } catch (reason) {
      pendingStopRef.current = false;
      setError(`${t.requestFailed}: ${reason instanceof Error ? reason.message : String(reason)}`);
    }
  }

  async function resumeTurn(id: string, turnId: string) {
    if (busy || abortRef.current) return;
    const controller = new AbortController();
    const runId = nextId("resume");
    let cursor = savedRuntimeCursor(id, turnId);
    let terminal = false;
    let terminalError = "";
    let retryDelay = 500;
    abortRef.current = controller;
    activeRunRef.current = runId;
    activeSessionRef.current = id;
    activeTurnRef.current = turnId;
    setActiveRuntimeSessionId(id);
    setBusy(true);
    setActivity(t.reconnecting);
    setChat((current) => [...current, { id: runId, role: "activity", content: "", steps: [] }]);
    try {
      while (!terminal && !controller.signal.aborted) {
        try {
          const response = await fetch(`/api/sessions/${encodeURIComponent(id)}/stream?after=${cursor}&language=${encodeURIComponent(language)}`, { signal: controller.signal, headers: { accept: "text/event-stream" } });
          if (response.status === 409) { terminal = true; break; }
          if (!response.ok && response.status >= 400 && response.status < 500) {
            terminalError = (await response.text()) || t.requestFailed;
            terminal = true;
            break;
          }
          await readSse(response, async (event) => {
            if (event.cursor !== undefined) cursor = Math.max(cursor, event.cursor);
            const result = await applyStreamEvent(event, runId);
            if (result.terminal && result.error && event.turn_id) {
              terminal = true;
              terminalError = result.error;
            } else if (result.terminal && result.error) {
              throw new Error(result.error);
            } else if (result.terminal) {
              terminal = true;
            }
          });
          retryDelay = 500;
        } catch {
          if (controller.signal.aborted) break;
          setActivity(t.reconnecting);
          await waitForReconnect(retryDelay, controller.signal);
          retryDelay = Math.min(retryDelay * 2, 5000);
          continue;
        }
        if (!terminal && !controller.signal.aborted) {
          setActivity(t.reconnecting);
          await waitForReconnect(retryDelay, controller.signal);
          retryDelay = Math.min(retryDelay * 2, 5000);
        }
      }
      updateRun(runId, (steps) => steps.map((step) => step.status === "active" ? { ...step, status: terminalError ? "failed" : "done" } : step));
      if (!controller.signal.aborted) {
        const detail = await json<SessionDetail>(`/api/sessions/${encodeURIComponent(id)}`);
        applySessionDetail(detail);
        if (terminalError) setError(`${t.requestFailed}: ${terminalError}`);
      }
      await refreshSessions();
    } finally {
      if (abortRef.current === controller) abortRef.current = null;
      if (activeRunRef.current === runId) activeRunRef.current = null;
      if (activeTurnRef.current === turnId) activeTurnRef.current = null;
      if (activeSessionRef.current === id) activeSessionRef.current = null;
      setActiveRuntimeSessionId("");
      pendingStopRef.current = false;
      setBusy(false);
      setActivity("");
    }
  }

  async function loadSession(id: string, activeTurnId: string | null = null) {
    if (busy) return;
    setError("");
    try {
      const detail = await json<SessionDetail>(`/api/sessions/${encodeURIComponent(id)}`);
      applySessionDetail(detail);
      if (activeTurnId) await resumeTurn(detail.id, activeTurnId);
    } catch (reason) {
      setError(`${t.loadFailed}: ${reason instanceof Error ? reason.message : String(reason)}`);
    }
  }
  loadSessionRef.current = loadSession;

  async function refreshSessions() {
    setSessions(await json<Session[]>("/api/sessions"));
  }

  async function refreshRuntimeActivity() {
    if (!workspace) return;
    setRuntimeActivity(await json<RuntimeActivity>(`/api/runtime/activity?workspace=${encodeURIComponent(workspace)}`));
  }

  async function resolveRuntimeApproval(id: string, decision: "allow_once" | "deny" | "always_allow") {
    try {
      await mutate(`/api/runtime/approvals/${encodeURIComponent(id)}/resolve`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ workspace, decision }) });
      await refreshRuntimeActivity();
    } catch (reason) { setError(`${t.runtimeActionFailed}: ${reason instanceof Error ? reason.message : String(reason)}`); }
  }

  async function answerRuntimeQuestion(id: string, answer: string | null) {
    try {
      await mutate(`/api/runtime/questions/${encodeURIComponent(id)}/answer`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ workspace, answer }) });
      await refreshRuntimeActivity();
    } catch (reason) { setError(`${t.runtimeActionFailed}: ${reason instanceof Error ? reason.message : String(reason)}`); }
  }

  async function runAgentAction(id: string, action: "stop" | "retry" | "retry_model" | "prompt", currentModel?: string | null) {
    const message = action === "prompt" ? window.prompt(t.agentPrompt)?.trim() : undefined;
    if (action === "prompt" && !message) return;
    const model = action === "retry_model" ? window.prompt(t.agentModelPrompt, currentModel ?? "")?.trim() : undefined;
    if (action === "retry_model" && !model) return;
    const endpointAction = action === "retry_model" ? "retry" : action;
    try {
      await mutate(`/api/runtime/agents/${encodeURIComponent(id)}/${endpointAction}`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ workspace, ...(message ? { message } : {}), ...(model ? { model } : {}) }) });
      await refreshRuntimeActivity();
    } catch (reason) { setError(`${t.runtimeActionFailed}: ${reason instanceof Error ? reason.message : String(reason)}`); }
  }

  async function spawnRuntimeAgent(profile: AgentSpawnProfile, task: string): Promise<boolean> {
    const targetSessionId = sessionId;
    const isActive = Boolean(targetSessionId) && (selectedSession?.active === true || activeRuntimeSessionId === targetSessionId);
    if (!isActive) { setError(`${t.runtimeActionFailed}: ${t.activeSessionRequired}`); return false; }
    try {
      setError("");
      await mutate("/api/runtime/agents/spawn", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ workspace, session_id: targetSessionId, profile, prompt: task }) });
      await refreshRuntimeActivity();
      return true;
    } catch (reason) {
      setError(`${t.runtimeActionFailed}: ${reason instanceof Error ? reason.message : String(reason)}`);
      return false;
    }
  }

  const [deleteTarget, setDeleteTarget] = useState<Session | null>(null);

  function sessionRow(item: Session) {
    const selected = sessionId === item.id;
    return <Flex key={item.id} className={`session-row${selected ? " selected" : ""}`} opacity={item.archived ? 0.68 : 1}>
      <button type="button" className="session-title" disabled={busy} onClick={() => void loadSession(item.id, item.active_turn_id)}>
        {item.pinned_at != null && <span className="session-pin" title={t.pinned} aria-label={t.pinned}>📌</span>}
        {item.title}
      </button>
      <Flex className="session-actions">
        <button type="button" title={t.renameSession} aria-label={t.renameSession} disabled={busy || item.active} onClick={() => void renameSession(item)}>✎</button>
        <button type="button" title={item.pinned_at != null ? t.unpinSession : t.pinSession} aria-label={item.pinned_at != null ? t.unpinSession : t.pinSession} disabled={busy} onClick={() => void togglePinSession(item)}>{item.pinned_at != null ? "↧" : "📌"}</button>
        <button type="button" title={item.archived ? t.unarchiveSession : t.archiveSession} aria-label={item.archived ? t.unarchiveSession : t.archiveSession} disabled={busy || item.active} onClick={() => void toggleArchiveSession(item)}>{item.archived ? "↥" : "▣"}</button>
        <button type="button" className="danger" title={t.deleteSession} aria-label={t.deleteSession} disabled={busy || item.active} onClick={() => setDeleteTarget(item)}>×</button>
      </Flex>
    </Flex>;
  }

  async function renameSession(target: Session) {
    const title = window.prompt(t.renamePrompt, target.title)?.trim(); if (!title) return;
    try { await mutate(`/api/sessions/${encodeURIComponent(target.id)}/rename`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ title }) }); await refreshSessions(); }
    catch (reason) { setError(`${t.sessionActionFailed}: ${reason instanceof Error ? reason.message : String(reason)}`); }
  }

  async function togglePinSession(target: Session) {
    const action = target.pinned_at ? "unpin" : "pin";
    try { await mutate(`/api/sessions/${encodeURIComponent(target.id)}/${action}`, { method: "POST" }); await refreshSessions(); }
    catch (reason) { setError(`${t.sessionActionFailed}: ${reason instanceof Error ? reason.message : String(reason)}`); }
  }

  async function forkSelectedSession() {
    if (!selectedSession) return;
    const title = window.prompt(t.forkPrompt, `${selectedSession.title}`)?.trim(); if (!title) return;
    try { const fork = await json<{ id: string }>(`/api/sessions/${encodeURIComponent(selectedSession.id)}/fork`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ title }) }); await refreshSessions(); await loadSession(fork.id); }
    catch (reason) { setError(`${t.sessionActionFailed}: ${reason instanceof Error ? reason.message : String(reason)}`); }
  }

  async function toggleArchiveSession(target: Session) {
    const action = target.archived ? "unarchive" : "archive";
    try { await mutate(`/api/sessions/${encodeURIComponent(target.id)}/${action}`, { method: "POST" }); await refreshSessions(); }
    catch (reason) { setError(`${t.sessionActionFailed}: ${reason instanceof Error ? reason.message : String(reason)}`); }
  }

  async function confirmDeleteSession() {
    const target = deleteTarget;
    if (!target) return;
    try {
      await mutate(`/api/sessions/${encodeURIComponent(target.id)}`, { method: "DELETE", headers: { "content-type": "application/json" }, body: JSON.stringify({ confirmation: target.id }) });
      if (sessionId === target.id) { setSessionId(""); setChat([]); }
      await refreshSessions();
    } catch (reason) { setError(`${t.sessionActionFailed}: ${reason instanceof Error ? reason.message : String(reason)}`); }
    finally { setDeleteTarget(null); }
  }

  async function exportSelectedSession() {
    if (!selectedSession) return;
    try { const value = await json<unknown>(`/api/sessions/${encodeURIComponent(selectedSession.id)}/export`); const url = URL.createObjectURL(new Blob([JSON.stringify(value, null, 2)], { type: "application/json" })); const anchor = document.createElement("a"); anchor.href = url; anchor.download = `willdeep-session-${selectedSession.id}.json`; anchor.click(); URL.revokeObjectURL(url); }
    catch (reason) { setError(`${t.sessionActionFailed}: ${reason instanceof Error ? reason.message : String(reason)}`); }
  }

  function attachImage(file: File) {
    const reader = new FileReader(); reader.onload = () => {
      const url = String(reader.result); const image = new Image(); image.onload = () => {
        const data = url.slice(url.indexOf(",") + 1);
        setAttachments((current) => [...current, { kind: "image", name: file.name || `${t.pastedImage}.png`, media_type: file.type, data, width: image.naturalWidth, height: image.naturalHeight }]);
      }; image.src = url;
    }; reader.readAsDataURL(file);
  }

  function handlePaste(event: React.ClipboardEvent<HTMLTextAreaElement>) {
    const imageFile = clipboardImageFile(event.clipboardData);
    if (imageFile) { event.preventDefault(); attachImage(imageFile); return; }
    const text = event.clipboardData.getData("text/plain");
    if (text.includes("\n") || text.length > 200) { event.preventDefault(); setAttachments((current) => [...current, { kind: "text", name: `${t.pastedText}-${current.length + 1}.txt`, content: text }]); }
  }

  async function send() {
    const typed = prompt.trim();
    if (selectedSession?.archived) { setError(`${t.requestFailed}: ${t.archived}`); return; }
    if (typed === "/clear") { setChat([]); setPrompt(""); return; }
    if (typed === "/help") { setChat((current) => [...current, { id: nextId("assistant"), role: "assistant", content: t.helpText }]); setPrompt(""); return; }
    if (typed === "/skills") { setChat((current) => [...current, { id: nextId("assistant"), role: "assistant", content: composer.skills.length ? composer.skills.map((skill) => `$${skill.identifier} · ${skill.name}\n${skill.description}`).join("\n\n") : t.noSkills }]); setPrompt(""); return; }
    if (typed === "/goal off") { setGoal(""); setChat((current) => [...current, { id: nextId("assistant"), role: "assistant", content: t.goalOff }]); setPrompt(""); return; }
    if (typed.startsWith("/goal ")) { setGoal(typed.slice(6).trim()); setChat((current) => [...current, { id: nextId("assistant"), role: "assistant", content: `${t.goalSet}: ${typed.slice(6).trim()}` }]); setPrompt(""); return; }
    const content = typed || (attachments.length ? t.attachmentPrompt : ""); if (!content || busy || !workspace) return;
    const harnessPrompt = goal && content !== "/compress" ? `<goal>\n${goal}\n</goal>\nContinue until this goal is genuinely complete.\n\n${content}` : content;
    const outgoingAttachments = attachments;
    const runId = nextId("run"); const controller = new AbortController(); abortRef.current = controller;
    activeRunRef.current = runId;
    followBottomRef.current = true;
    setPrompt(""); setAttachments([]); setError(""); setActivity(t.thinking); setBusy(true);
    setChat((current) => [...current, { id: nextId("user"), role: "user", content: `${content}${outgoingAttachments.length ? `\n[${outgoingAttachments.length} ${t.attachmentCount}]` : ""}` }, { id: runId, role: "activity", content: "", steps: [] }]);
    let terminalFailure = false;
    try {
      const response = await fetch("/api/chat/stream", { method: "POST", signal: controller.signal, headers: { "content-type": "application/json", accept: "text/event-stream" }, body: JSON.stringify({ prompt: harnessPrompt, session_id: sessionId || null, workspace, language, attachments: outgoingAttachments }) });
      let answer = "";
      let terminal = false;
      await readSse(response, async (event) => {
        const result = await applyStreamEvent(event, runId);
        if (result.terminal && result.error) {
          terminalFailure = Boolean(event.turn_id);
          throw new Error(result.error);
        }
        if (result.terminal) {
          terminal = true;
          answer = result.text || t.emptyReply;
        }
      });
      if (!terminal) throw new Error(t.streamDisconnected);
      updateRun(runId, (steps) => steps.map((step) => step.status === "active" ? { ...step, status: "done" } : step));
      setChat((current) => [...current, { id: nextId("assistant"), role: "assistant", content: answer || t.emptyReply }]);
      refreshSessions().catch(() => undefined);
    } catch (reason) {
      const recoverSessionId = activeSessionRef.current;
      const recoverTurnId = activeTurnRef.current;
      if (!(reason instanceof DOMException && reason.name === "AbortError") && recoverSessionId && recoverTurnId && !terminalFailure) {
        if (abortRef.current === controller) abortRef.current = null;
        if (activeRunRef.current === runId) activeRunRef.current = null;
        setBusy(false);
        setActivity(t.reconnecting);
        await resumeTurn(recoverSessionId, recoverTurnId);
      } else if (!(reason instanceof DOMException && reason.name === "AbortError")) {
        setError(`${t.requestFailed}: ${reason instanceof Error ? reason.message : String(reason)}`);
      }
    } finally { if (abortRef.current === controller) abortRef.current = null; if (activeRunRef.current === runId) activeRunRef.current = null; activeTurnRef.current = null; activeSessionRef.current = null; setActiveRuntimeSessionId(""); pendingStopRef.current = false; setBusy(false); setActivity(""); }
  }

  return <Flex minH="100vh" bg="#080d12" color="#e7edf4">
    <Box as="aside" w={{ base: "0", md: "282px" }} display={{ base: "none", md: "flex" }} flexDir="column" borderRight="1px solid" borderColor="#202a35" p="5" bg="#0b1118" overflowY="auto">
      <Box flex="1">
      <Heading size="lg">{t.appName}</Heading><Text color="#718096" mb="8">{t.webHarness}</Text>
      <Text fontSize="xs" color="#8290a3" mb="2">{t.language}</Text><NativeSelect.Root mb="5"><NativeSelect.Field aria-label={t.language} value={language} onChange={(event) => setLanguage(event.target.value as Language)} bg="#101820" borderColor="#2b3948">{languages.map((code) => <option key={code} value={code}>{languageLabels[code]}</option>)}</NativeSelect.Field><NativeSelect.Indicator /></NativeSelect.Root>
      <ModelRoutingSettings messages={t} />
      <Text fontSize="xs" color="#8290a3" mb="2">{t.workspace}</Text><NativeSelect.Root><NativeSelect.Field aria-label={t.workspace} value={workspace} onChange={(event) => setWorkspace(event.target.value)} bg="#101820" borderColor="#2b3948">{workspaces.map((item) => <option key={item.id} value={item.path}>{item.name} · {item.access === "read_only" ? t.readOnly : item.access === "smart" ? t.smartApproval : t.workspaceWrite}{item.active ? ` · ${t.activeWorkspace}` : ""}</option>)}</NativeSelect.Field><NativeSelect.Indicator /></NativeSelect.Root>
      <RuntimeSidebar activity={runtimeActivity} messages={t} onResolveApproval={resolveRuntimeApproval} onAnswerQuestion={answerRuntimeQuestion} onAgentAction={runAgentAction} canSpawnAgent={Boolean(sessionId) && (selectedSession?.active === true || activeRuntimeSessionId === sessionId)} onSpawnAgent={spawnRuntimeAgent} />
      <Flex mt="8" mb="3" justify="space-between"><Text fontSize="xs" color="#8290a3">{t.session}</Text><Button size="xs" variant="ghost" disabled={busy} onClick={() => { localStorage.removeItem(`${lastSessionPrefix}${workspace}`); setSessionId(""); setChat([]); }}>{t.newSession}</Button></Flex>
      <Input size="sm" mb="2" value={sessionSearch} onChange={(event) => setSessionSearch(event.target.value)} placeholder={t.searchSessions} aria-label={t.searchSessions} color="#d8e2ec" _placeholder={{ color: "#667587" }} bg="#0f1720" borderColor="#465568" />
      <Box className="session-list">
        <VStack align="stretch" gap="1">{liveSessions.map(sessionRow)}{!liveSessions.length && <Text color="#77879a" fontSize="sm">{t.noSessions}</Text>}</VStack>
        {archivedSessions.length > 0 && <Box mt="2">
          <button type="button" className="archived-toggle" aria-expanded={showArchived} onClick={() => setShowArchived((current) => !current)}>
            <span className={`archived-caret${showArchived ? " open" : ""}`}>▸</span>{t.archived} ({archivedSessions.length})
          </button>
          {showArchived && <VStack align="stretch" gap="1" mt="1">{archivedSessions.map(sessionRow)}</VStack>}
        </Box>}
      </Box>
      {selectedSession && <Flex mt="2" gap="1" wrap="wrap"><Button size="xs" variant="ghost" title={t.forkSession} aria-label={t.forkSession} disabled={busy || selectedSession.active} onClick={() => void forkSelectedSession()}>⑂</Button><Button size="xs" variant="ghost" title={t.exportSession} aria-label={t.exportSession} disabled={busy} onClick={() => void exportSelectedSession()}>⇩</Button></Flex>}
      </Box>
      <Text as="footer" mt="5" pt="3" borderTop="1px solid" borderColor="#202a35" color="#627184" fontSize="2xs" textAlign="right">{t.version}: {version ? `v${version}` : "—"}</Text>
      {/* closeOnInteractOutside 必须显式写：zag 对 role="alertdialog" 默认关掉外部点击，
          于是这个框只认 Esc，点旁边的输入框什么也不发生。删除仍要点「删除」才发生，
          点外面等同取消，没有误删风险。 */}
      <Dialog.Root role="alertdialog" closeOnInteractOutside open={deleteTarget !== null} onOpenChange={(details) => { if (!details.open) setDeleteTarget(null); }}>
        <Portal>
          <Dialog.Backdrop bg="#000a" />
          <Dialog.Positioner>
            <Dialog.Content bg="#101820" color="#e7edf4" border="1px solid" borderColor="#2b3948" borderRadius="12px" maxW="360px">
              <Dialog.Header><Dialog.Title fontSize="md">{t.deleteDialogTitle}</Dialog.Title></Dialog.Header>
              <Dialog.Body>
                <Text fontWeight="600" mb="2" overflow="hidden" textOverflow="ellipsis" whiteSpace="nowrap">{deleteTarget?.title}</Text>
                <Text fontSize="sm" color="#8b99aa">{t.deleteConfirm}</Text>
              </Dialog.Body>
              <Dialog.Footer gap="2">
                <Button size="sm" variant="outline" borderColor="#3a4859" color="#d8e2ec" onClick={() => setDeleteTarget(null)}>{t.cancel}</Button>
                <Button size="sm" colorPalette="red" onClick={() => void confirmDeleteSession()}>{t.confirmDelete}</Button>
              </Dialog.Footer>
            </Dialog.Content>
          </Dialog.Positioner>
        </Portal>
      </Dialog.Root>
    </Box>
    <Container maxW="920px" px={{ base: "4", md: "8" }} py="6" display="flex" flexDir="column" h="100vh">
      <Box ref={chatViewportRef} className="chat-viewport" flex="1" minH="0" overflowY="auto" pb="6" onScroll={() => { const node = chatViewportRef.current; if (node) followBottomRef.current = node.scrollHeight - node.scrollTop - node.clientHeight < 80; }}>{!chat.length && <Box py="24"><Heading size="2xl" mb="4">{t.welcomeTitle}</Heading><Text color="#8b99aa">{t.welcomeBody}</Text></Box>}
        <VStack align="stretch" gap="3">{chat.map((message) => message.role === "activity" ? <Box key={message.id} className="run-card">{message.steps?.map((step) => <Flex key={step.id} className={`run-step ${step.status}`}><Box className="step-dot" /><Text>{step.label}</Text></Flex>)}</Box> : <Box key={message.id} className={`message ${message.role}`}>{message.role === "assistant" ? <Markdown content={message.content} /> : message.content}</Box>)}</VStack>
        {error && <Text color="#ff8f8f" py="4">{error}</Text>}<div ref={endRef} />
      </Box>
      <Box className="composer-shell">
        {busy && <Flex className="thinking-strip"><Box className="thinking-pulse" /><Text title={activity}>{activity || t.thinking}</Text></Flex>}
        {commandMatches.length > 0 && <Box className="suggestions"><Text className="suggestion-title">{t.commands}</Text>{commandMatches.map((command) => <button key={command} type="button" onMouseDown={(event) => { event.preventDefault(); setPrompt(command); }}>{command}</button>)}</Box>}
        {skillQuery !== undefined && <Box className="suggestions"><Text className="suggestion-title">{t.skills}</Text><Input className="skill-search" size="sm" value={skillSearch} onChange={(event) => setSkillSearch(event.target.value)} placeholder={t.searchSkills} aria-label={t.searchSkills} />{skillMatches.length ? skillMatches.map((skill) => <button key={skill.identifier} type="button" onMouseDown={(event) => { event.preventDefault(); setSkillSearch(""); setPrompt((current) => current.replace(/\$[\w-]*$/, `$${skill.identifier} `)); }}><strong>${skill.identifier}</strong><small>{skill.name} · {skill.description}</small></button>) : <Text className="suggestion-empty">{t.noSkills}</Text>}</Box>}
        {attachments.length > 0 && <Flex className="attachment-row">{attachments.map((attachment, index) => <Box key={`${attachment.name}-${index}`} className="attachment-chip">{attachment.kind === "image" ? <img src={`data:${attachment.media_type};base64,${attachment.data}`} alt={attachment.name} /> : <Box className="text-attachment">TXT</Box>}<Text title={attachment.name}>{attachment.name}</Text><button type="button" aria-label={t.removeAttachment} title={t.removeAttachment} onClick={() => setAttachments((current) => current.filter((_, itemIndex) => itemIndex !== index))}>×</button></Box>)}</Flex>}
        <Textarea value={prompt} onPaste={handlePaste} onChange={(event) => setPrompt(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void send(); } }} placeholder={t.promptPlaceholder} minH="104px" maxH="240px" resize="vertical" border="0" outline="none" lineHeight="1.5" _focus={{ boxShadow: "none", outline: "none" }} _focusVisible={{ boxShadow: "none", outline: "none" }} px="4" pt={attachments.length ? "2" : "4"} pb="12" />
        <Text className="send-hint">{t.sendHint}</Text>
        <Button aria-label={busy ? t.stop : t.send} title={busy ? t.stop : t.send} className={`send-button ${busy ? "stop" : ""}`} onClick={busy ? () => void stop() : () => void send()} disabled={!busy && ((!prompt.trim() && !attachments.length) || selectedSession?.archived)}>{busy ? <Box className="stop-icon" /> : <Text className="send-icon">↑</Text>}</Button>
      </Box>
    </Container>
  </Flex>;
}
