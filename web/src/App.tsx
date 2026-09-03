import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Box, Button, Container, Dialog, Flex, Heading, Input, NativeSelect, Portal, Text, Textarea, VStack } from "@chakra-ui/react";
import { detectLanguage, messages, type Language, type Messages } from "./i18n";
import { RuntimeSidebar, type AgentSpawnProfile, type RuntimeActivity, type RuntimeEvent } from "./RuntimeSidebar";
import { Markdown } from "./Markdown";
import { SidebarSettings } from "./SidebarSettings";
import { QuickSettings } from "./QuickSettings";
import { PluginCenter } from "./PluginCenter";
import { PluginPage } from "./PluginPage";
import { PluginRail, type RailSelection } from "./PluginRail";
import { PluginSidebar } from "./PluginSidebar";
import { fetchPlugins, orderedDestinations, type PluginFailureView, type PluginView } from "./plugins";
import { PluginCommandPalette, PluginMenuPopup } from "./PluginMenus";
import { menuEntries, useChatSelection, usePluginCommandRunner, type MenuEntry } from "./pluginMenuModel";
import { SfIcon } from "./sfSymbols";

type Workspace = { id: string; path: string; name: string; active: boolean; access: "read_only" | "smart" | "workspace_write" };
type Session = { id: string; title: string; preview?: string; workspace: string; updated_at: number; pinned_at: number | null; archived: boolean; active: boolean; active_turn_id: string | null };
type SessionDetail = { id: string; messages: Array<{ role: "user" | "assistant"; content: string; attachment_count: number }> };
type RunStep = { id: string; label: string; detail?: string; status: "active" | "done" | "failed" };
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
  /// 这一步具体在干什么：命令的前几个词，或 Worker 的职责与标签。服务端已经
  /// 收敛并打码过，前端只负责淡色显示。
  detail?: string;
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

// 一级入口写进 URL hash，刷新和分享链接都能回到同一个目的地。
function railFromHash(): RailSelection {
  const hash = window.location.hash.replace(/^#/, "");
  if (hash === "plugins") return { kind: "center" };
  const plugin = hash.startsWith("plugin/") ? hash.slice("plugin/".length) : "";
  return plugin ? { kind: "plugin", qualifiedId: decodeURIComponent(plugin) } : { kind: "conversation" };
}

function hashForRail(selection: RailSelection): string {
  if (selection.kind === "center") return "#plugins";
  if (selection.kind === "plugin") return `#plugin/${encodeURIComponent(selection.qualifiedId)}`;
  return "";
}

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

/// 一轮里最多显示几条进度。
///
/// 一次长任务动辄几十步，全列出来会把整个聊天区顶满，而人真正关心的只有
/// 「现在在干什么、刚才几步是什么」。折起来的部分只报个数，不留一长串。
const VISIBLE_RUN_STEPS = 5;

function RunCard({ steps, messages: t }: { steps: RunStep[]; messages: Messages }) {
  const hidden = Math.max(0, steps.length - VISIBLE_RUN_STEPS);
  const visible = steps.slice(hidden);
  return <Box className="run-card">
    {hidden > 0 && <Text className="run-collapsed">{t.earlierSteps.replace("{count}", String(hidden))}</Text>}
    {visible.map((step) => <Flex key={step.id} className={`run-step ${step.status}`}><Box className="step-dot" /><Text>{step.label}</Text>{step.detail && <Text className="run-step-detail" title={step.detail}>{step.detail}</Text>}</Flex>)}
  </Box>;
}

export function App() {
  const [language, setLanguage] = useState<Language>(detectLanguage); const t = messages[language];
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]); const [workspace, setWorkspace] = useState("");
  const [sessions, setSessions] = useState<Session[]>([]); const [sessionId, setSessionId] = useState("");
  const [runtimeEvents, setRuntimeEvents] = useState<RuntimeEvent[]>([]);
  // 轮询闭包里读得到当前会话，而不必把 sessionId 放进依赖数组重开定时器。
  const sessionIdRef = useRef(sessionId);
  useEffect(() => { sessionIdRef.current = sessionId; }, [sessionId]);
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
  const [addingWorkspace, setAddingWorkspace] = useState(false);
  const [newWorkspacePath, setNewWorkspacePath] = useState("");
  const [workspaceError, setWorkspaceError] = useState("");
  const [plugins, setPlugins] = useState<PluginView[]>([]);
  const [pluginFailures, setPluginFailures] = useState<PluginFailureView[]>([]);
  const [rail, setRail] = useState<RailSelection>(railFromHash);
  const [pluginSelectedItem, setPluginSelectedItem] = useState<string | null>(null);
  const [pluginReloadToken, setPluginReloadToken] = useState(0);

  const refreshPlugins = useCallback(() => {
    fetchPlugins(language)
      .then((value) => { setPlugins(value.plugins); setPluginFailures(value.failures); })
      // 插件宿主没起来不该让聊天界面报错——那是两件独立的事。
      .catch(() => { setPlugins([]); setPluginFailures([]); });
  }, [language]);
  useEffect(() => { refreshPlugins(); }, [refreshPlugins, pluginReloadToken]);

  const pluginEntries = useMemo(() => orderedDestinations(plugins), [plugins]);
  const activePlugin = useMemo(
    () => (rail.kind === "plugin" ? pluginEntries.find((entry) => entry.destination.qualified_id === rail.qualifiedId) : undefined),
    [rail, pluginEntries]
  );
  // 选中的插件被停用或卸载时回到对话，而不是停在一个已经不存在的目的地上。
  // 插件清单还没加载完时先别动——否则从一个 #plugin/... 链接进来会被立刻踢走。
  useEffect(() => {
    if (rail.kind === "plugin" && plugins.length > 0 && !activePlugin) setRail({ kind: "conversation" });
  }, [rail, activePlugin, plugins.length]);

  useEffect(() => {
    const target = hashForRail(rail);
    if (window.location.hash !== target) {
      window.history.replaceState(null, "", `${window.location.pathname}${window.location.search}${target}`);
    }
  }, [rail]);
  useEffect(() => {
    const onHashChange = () => setRail(railFromHash());
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  const navigateToDestination = useCallback((qualifiedId: string) => {
    setPluginSelectedItem(null);
    setRail({ kind: "plugin", qualifiedId });
  }, []);

  // 插件的五个菜单贡献点。目的地是主入口，这些是顺手入口——收藏夹与待办
  // 真正的用法是「聊天里选中一句就记下」，而不是先切目的地再手打一遍。
  const [showPalette, setShowPalette] = useState(false);
  const [popup, setPopup] = useState<{ entries: MenuEntry[]; x: number; y: number; args: Record<string, string> } | null>(null);
  const [pluginMenuError, setPluginMenuError] = useState("");
  const runPluginCommand = usePluginCommandRunner(navigateToDestination);
  const paletteEntries = useMemo(() => menuEntries(plugins, "commandPalette"), [plugins]);
  const chatSelectionEntries = useMemo(() => menuEntries(plugins, "chat.selection"), [plugins]);
  const sessionContextEntries = useMemo(() => menuEntries(plugins, "session.context"), [plugins]);
  const composerEntries = useMemo(() => menuEntries(plugins, "composer.more"), [plugins]);
  const [chatSelection, setChatSelection] = useChatSelection(chatViewportRef, chatSelectionEntries.length > 0);

  const runMenuEntry = useCallback(async (entry: MenuEntry, args: Record<string, string> = {}) => {
    setPopup(null);
    setShowPalette(false);
    setChatSelection(null);
    setPluginMenuError("");
    try {
      await runPluginCommand(entry, args);
      // 命令可能改了插件自己的数据，让当前目的地的侧栏与页面重取一次。
      setPluginReloadToken((current) => current + 1);
    } catch (reason) {
      setPluginMenuError(reason instanceof Error ? reason.message : String(reason));
    }
  }, [runPluginCommand, setChatSelection]);

  useEffect(() => {
    if (!paletteEntries.length) return;
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setShowPalette((current) => !current);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [paletteEntries.length]);

  const pluginOverlays = <>
    {showPalette && <PluginCommandPalette entries={paletteEntries} messages={t} onRun={(entry) => void runMenuEntry(entry)} onClose={() => setShowPalette(false)} />}
    {popup && <PluginMenuPopup entries={popup.entries} x={popup.x} y={popup.y} onRun={(entry) => void runMenuEntry(entry, popup.args)} onClose={() => setPopup(null)} />}
    {pluginMenuError && <Text className="plugin-error" position="fixed" bottom="3" right="3" zIndex="50" borderRadius="8px" onClick={() => setPluginMenuError("")}>{t.pluginCommandFailed}: {pluginMenuError}</Text>}
  </>;

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
      // 事件按会话取，且只在选中会话时取：浏览器端没有应用层鉴权，跨会话
      // 列表没有归属可校验。
      const eventsRequest = sessionIdRef.current
        ? json<RuntimeEvent[]>(`/api/runtime/events?session=${encodeURIComponent(sessionIdRef.current)}`).catch(() => [] as RuntimeEvent[])
        : Promise.resolve([] as RuntimeEvent[]);
      return Promise.all([json<RuntimeActivity>(`/api/runtime/activity?workspace=${encodeURIComponent(workspace)}`), json<Session[]>("/api/sessions"), eventsRequest])
        .then(([runtime, currentSessions, events]) => { if (active) { setRuntimeActivity(runtime); setSessions(currentSessions); setRuntimeEvents(events); } })
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
      updateRun(runId, (steps) => steps.some((step) => step.id === stepId) ? steps : [...steps, { id: stepId, label: event.label || t.toolRunning, detail: event.detail, status: "active" }]);
    }
    else if (event.type === "tool_completed") {
      setActivity(event.label || (event.is_error ? t.toolFailed : t.toolDone));
      const stepId = event.id || `tool-${event.cursor ?? nextId("tool")}`;
      updateRun(runId, (steps) => steps.some((step) => step.id === stepId)
        ? steps.map((step) => step.id === stepId ? { ...step, label: event.label || step.label, detail: event.detail ?? step.detail, status: event.is_error ? "failed" : "done" } : step)
        : [...steps, { id: stepId, label: event.label || (event.is_error ? t.toolFailed : t.toolDone), detail: event.detail, status: event.is_error ? "failed" : "done" }]);
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

  async function ignoreRuntimeEvent(id: string) {
    if (!sessionId) return;
    await fetch(`/api/runtime/events/${encodeURIComponent(id)}/ignore`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ session: sessionId }),
    });
    setRuntimeEvents((current) => current.filter((event) => event.id !== id));
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

  // 加进来的工作区只影响这个浏览器视图能看到什么，不改 Runtime 的全局默认。
  // 非回环监听时后端会拒绝并说明理由——那条边界留在启动命令里。
  async function addWorkspace() {
    const path = newWorkspacePath.trim();
    if (!path) return;
    setWorkspaceError("");
    try {
      const added = await json<Workspace>("/api/workspaces", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ path }),
      });
      const values = await json<Workspace[]>("/api/workspaces");
      setWorkspaces(values);
      setWorkspace(added.path);
      setNewWorkspacePath("");
      setAddingWorkspace(false);
    } catch (reason) {
      setWorkspaceError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  function sessionRow(item: Session) {
    const selected = sessionId === item.id;
    return <Flex key={item.id} className={`session-row${selected ? " selected" : ""}`} opacity={item.archived ? 0.68 : 1}
      onContextMenu={(event) => {
        // 没有插件贡献这个位置时不劫持右键——浏览器自带的菜单仍然该能用。
        if (!sessionContextEntries.length) return;
        event.preventDefault();
        setPopup({ entries: sessionContextEntries, x: event.clientX, y: event.clientY, args: { session: item.id } });
      }}>
      <button type="button" className="session-title" disabled={busy} onClick={() => void loadSession(item.id, item.active_turn_id)}>
        {item.pinned_at != null && <span className="session-pin" title={t.pinned} aria-label={t.pinned}>📌</span>}
        {/* 标题没生成时后端会带回首条用户消息。显示它而不是一排
            一模一样的 New session——那些会话是有内容的。 */}
        {item.preview ? <span className="session-untitled">{item.preview}</span> : item.title}
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

  // 一级入口栏之后，右侧要么是对话（原有侧栏 + 聊天），要么是一个插件目的地
  // （它的配套侧栏 + 页面），要么是插件中心。入口、侧栏与中央页永远来自
  // 同一个 descriptor，不会出现入口属于 A、侧栏属于 B 的半切换状态。
  const railNav = <PluginRail entries={pluginEntries} selection={rail} messages={t} onSelect={(next) => { setPluginSelectedItem(null); setRail(next); }} />;

  if (rail.kind === "center") {
    return <Flex minH="100vh" bg="#080d12" color="#e7edf4">
      {railNav}
      <PluginCenter plugins={plugins} failures={pluginFailures} messages={t} onChanged={() => setPluginReloadToken((current) => current + 1)} />
      {pluginOverlays}
    </Flex>;
  }

  if (rail.kind === "plugin" && activePlugin) {
    const { plugin, destination } = activePlugin;
    return <Flex minH="100vh" bg="#080d12" color="#e7edf4">
      {railNav}
      {destination.sidebar?.mode === "declarative" && <PluginSidebar plugin={plugin} destination={destination} locale={language} messages={t} reloadToken={pluginReloadToken} onNavigate={navigateToDestination} onSelectItem={setPluginSelectedItem} selectedItemId={pluginSelectedItem} onRowContextMenu={(event, componentId, commands) => {
        const entries = menuEntries(plugins, "plugin.sidebar.row.context").filter((entry) => entry.pluginId === plugin.id && (!commands.length || commands.includes(entry.commandId)));
        if (!entries.length) return;
        event.preventDefault();
        setPopup({ entries, x: event.clientX, y: event.clientY, args: { item: componentId } });
      }} />}
      <PluginPage plugin={plugin} destination={destination} messages={t} locale={language} workspace={workspace || null} sessionId={sessionId || null} selectedItemId={pluginSelectedItem} onSelectItem={setPluginSelectedItem} onNavigate={navigateToDestination} onOpenPluginCenter={() => setRail({ kind: "center" })} />
      {pluginOverlays}
    </Flex>;
  }

  return <Flex minH="100vh" bg="#080d12" color="#e7edf4">
    {railNav}
    {/* 侧栏本身不滚动：只有会话列表滚。这样列表能吃掉全部剩余高度，
        而不是被上面几块设置挤到下半屏。 */}
    <Box as="aside" w={{ base: "0", md: "282px" }} display={{ base: "none", md: "flex" }} flexDir="column" borderRight="1px solid" borderColor="#202a35" p="5" bg="#0b1118" overflow="hidden">
      <Flex justify="space-between" align="flex-start" mb="5">
        <Box minW="0">
          <Heading size="md" lineHeight="1.2">{t.appName}</Heading>
          <Text color="#718096" fontSize="xs">{t.webHarness}</Text>
        </Box>
        <SidebarSettings messages={t} language={language} onLanguageChange={setLanguage} />
      </Flex>
      {/* 工作区收成一行：选择器占满，加号贴右。标签文字省掉——下拉里
          显示的就是工作区名和访问模式，再顶一行「工作区」是废话。 */}
      <Flex gap="1" align="center">
        <NativeSelect.Root size="sm" flex="1" minW="0"><NativeSelect.Field aria-label={t.workspace} title={workspace} value={workspace} onChange={(event) => setWorkspace(event.target.value)} bg="#101820" borderColor="#2b3948" color="#d8e2ec">{workspaces.map((item) => <option key={item.id} value={item.path}>{item.name} · {item.access === "read_only" ? t.accessReadOnly : item.access === "smart" ? t.accessSmart : t.accessWrite}{item.active ? ` · ${t.activeWorkspace}` : ""}</option>)}</NativeSelect.Field><NativeSelect.Indicator /></NativeSelect.Root>
        <button type="button" className={`workspace-add${addingWorkspace ? " active" : ""}`} title={addingWorkspace ? t.cancel : t.addWorkspace} aria-label={addingWorkspace ? t.cancel : t.addWorkspace} aria-expanded={addingWorkspace} onClick={() => { setAddingWorkspace((current) => !current); setWorkspaceError(""); }}>
          <SfIcon name={addingWorkspace ? "sf:x.mark" : "sf:plus.circle"} size={15} />
        </button>
      </Flex>
      {addingWorkspace && <Box mt="2">
        <Input size="sm" autoFocus value={newWorkspacePath} placeholder={t.addWorkspacePlaceholder} aria-label={t.addWorkspace} color="#d8e2ec" _placeholder={{ color: "#667587" }} bg="#0f1720" borderColor="#465568"
          onChange={(event) => setNewWorkspacePath(event.target.value)}
          onKeyDown={(event) => { if (event.key === "Enter") void addWorkspace(); if (event.key === "Escape") { setAddingWorkspace(false); setWorkspaceError(""); } }} />
        <Text fontSize="2xs" color="#6d7c90" mt="1">{t.addWorkspaceHint}</Text>
        {workspaceError && <Text fontSize="xs" color="#ff9d9d" mt="1">{workspaceError}</Text>}
      </Box>}
      <RuntimeSidebar activity={runtimeActivity} events={runtimeEvents} onIgnoreEvent={ignoreRuntimeEvent} messages={t} onResolveApproval={resolveRuntimeApproval} onAnswerQuestion={answerRuntimeQuestion} onAgentAction={runAgentAction} canSpawnAgent={Boolean(sessionId) && (selectedSession?.active === true || activeRuntimeSessionId === sessionId)} onSpawnAgent={spawnRuntimeAgent} />
      {/* 会话区吃掉剩余高度。minH=0 是必须的：没有它，flex 子项的最小高度是
          内容高度，列表撑破容器而不是内部滚动。 */}
      <Flex direction="column" flex="1" minH="0" mt="5">
        <Flex mb="2" justify="space-between" align="baseline"><Text fontSize="xs" color="#8290a3">{t.session}</Text><Button size="xs" variant="ghost" color="#9dabbd" _hover={{ bg: "#16212c", color: "#f2f6fa" }} disabled={busy} onClick={() => { localStorage.removeItem(`${lastSessionPrefix}${workspace}`); setSessionId(""); setChat([]); }}>{t.newSession}</Button></Flex>
        <Input size="sm" mb="2" flex="0 0 auto" value={sessionSearch} onChange={(event) => setSessionSearch(event.target.value)} placeholder={t.searchSessions} aria-label={t.searchSessions} color="#d8e2ec" _placeholder={{ color: "#667587" }} bg="#0f1720" borderColor="#465568" />
        <Box className="session-list" flex="1" minH="0">
          <VStack align="stretch" gap="1">{liveSessions.map(sessionRow)}{!liveSessions.length && <Text color="#77879a" fontSize="sm">{t.noSessions}</Text>}</VStack>
          {archivedSessions.length > 0 && <Box mt="2">
            <button type="button" className="archived-toggle" aria-expanded={showArchived} onClick={() => setShowArchived((current) => !current)}>
              <span className={`archived-caret${showArchived ? " open" : ""}`}>▸</span>{t.archived} ({archivedSessions.length})
            </button>
            {showArchived && <VStack align="stretch" gap="1" mt="1">{archivedSessions.map(sessionRow)}</VStack>}
          </Box>}
        </Box>
        {selectedSession && <Flex mt="2" gap="1" wrap="wrap" flex="0 0 auto"><Button size="xs" variant="ghost" title={t.forkSession} aria-label={t.forkSession} disabled={busy || selectedSession.active} onClick={() => void forkSelectedSession()}>⑂</Button><Button size="xs" variant="ghost" title={t.exportSession} aria-label={t.exportSession} disabled={busy} onClick={() => void exportSelectedSession()}>⇩</Button></Flex>}
      </Flex>
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
      <Box ref={chatViewportRef} className="chat-viewport" flex="1" minH="0" overflowY="auto" pb="10" onScroll={() => { const node = chatViewportRef.current; if (node) followBottomRef.current = node.scrollHeight - node.scrollTop - node.clientHeight < 80; }}>{!chat.length && <Box py="24"><Heading size="2xl" mb="4">{t.welcomeTitle}</Heading><Text color="#8b99aa">{t.welcomeBody}</Text></Box>}
        <VStack align="stretch" gap="3">{chat.map((message) => message.role === "activity" ? <RunCard key={message.id} steps={message.steps ?? []} messages={t} /> : <Box key={message.id} className={`message ${message.role}`}>{message.role === "assistant" ? <Markdown content={message.content} /> : message.content}</Box>)}</VStack>
        {error && <Text color="#ff8f8f" py="4">{error}</Text>}<div ref={endRef} />
      </Box>
      <QuickSettings messages={t} language={language} onLanguageChange={setLanguage} />
      {/* 聊天正文选中气泡。插件拿到的 `text` 是用户真正看到的那段字，
          `source` 固定 "chat.selection"，与 macOS 宿主传的两个参数一致。 */}
      {chatSelection && chatSelectionEntries.length > 0 && (
        <PluginMenuPopup
          entries={chatSelectionEntries}
          x={chatSelection.x}
          y={chatSelection.y}
          onRun={(entry) => void runMenuEntry(entry, { text: chatSelection.text, source: "chat.selection" })}
          onClose={() => setChatSelection(null)}
        />
      )}
      <Box className="composer-shell">
        {busy && <Flex className="thinking-strip"><Box className="thinking-pulse" /><Text title={activity}>{activity || t.thinking}</Text></Flex>}
        {commandMatches.length > 0 && <Box className="suggestions"><Text className="suggestion-title">{t.commands}</Text>{commandMatches.map((command) => <button key={command} type="button" onMouseDown={(event) => { event.preventDefault(); setPrompt(command); }}>{command}</button>)}</Box>}
        {skillQuery !== undefined && <Box className="suggestions"><Text className="suggestion-title">{t.skills}</Text><Input className="skill-search" size="sm" value={skillSearch} onChange={(event) => setSkillSearch(event.target.value)} placeholder={t.searchSkills} aria-label={t.searchSkills} />{skillMatches.length ? skillMatches.map((skill) => <button key={skill.identifier} type="button" onMouseDown={(event) => { event.preventDefault(); setSkillSearch(""); setPrompt((current) => current.replace(/\$[\w-]*$/, `$${skill.identifier} `)); }}><strong>${skill.identifier}</strong><small>{skill.name} · {skill.description}</small></button>) : <Text className="suggestion-empty">{t.noSkills}</Text>}</Box>}
        {attachments.length > 0 && <Flex className="attachment-row">{attachments.map((attachment, index) => <Box key={`${attachment.name}-${index}`} className="attachment-chip">{attachment.kind === "image" ? <img src={`data:${attachment.media_type};base64,${attachment.data}`} alt={attachment.name} /> : <Box className="text-attachment">TXT</Box>}<Text title={attachment.name}>{attachment.name}</Text><button type="button" aria-label={t.removeAttachment} title={t.removeAttachment} onClick={() => setAttachments((current) => current.filter((_, itemIndex) => itemIndex !== index))}>×</button></Box>)}</Flex>}
        <Textarea value={prompt} onPaste={handlePaste} onChange={(event) => setPrompt(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void send(); } }} placeholder={t.promptPlaceholder} minH="104px" maxH="240px" resize="vertical" border="0" outline="none" lineHeight="1.5" _focus={{ boxShadow: "none", outline: "none" }} _focusVisible={{ boxShadow: "none", outline: "none" }} px="4" pt={attachments.length ? "2" : "4"} pb="12" />
        {composerEntries.length > 0 && (
          <button
            type="button"
            className="composer-more"
            title={t.pluginComposerMore}
            aria-label={t.pluginComposerMore}
            onClick={(event) => {
              const rect = event.currentTarget.getBoundingClientRect();
              setPopup({ entries: composerEntries, x: rect.left, y: rect.top - 8, args: {} });
            }}
          >
            <SfIcon name="sf:plus.circle" size={16} />
          </button>
        )}
        <Text className="send-hint">{t.sendHint}</Text>
        <Button aria-label={busy ? t.stop : t.send} title={busy ? t.stop : t.send} className={`send-button ${busy ? "stop" : ""}`} onClick={busy ? () => void stop() : () => void send()} disabled={!busy && ((!prompt.trim() && !attachments.length) || selectedSession?.archived)}>{busy ? <Box className="stop-icon" /> : <Text className="send-icon">↑</Text>}</Button>
      </Box>
    </Container>
    {pluginOverlays}
  </Flex>;
}
