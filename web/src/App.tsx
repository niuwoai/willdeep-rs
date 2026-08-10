import { useEffect, useMemo, useRef, useState } from "react";
import { Box, Button, Container, Flex, Heading, Input, NativeSelect, Text, Textarea, VStack } from "@chakra-ui/react";
import { detectLanguage, languageLabels, languages, messages, type Language } from "./i18n";
import { RuntimeSidebar, type RuntimeActivity } from "./RuntimeSidebar";

type Workspace = { id: string; path: string; name: string; active: boolean; access: "read_only" | "smart" | "workspace_write" };
type Session = { id: string; title: string; workspace: string; updated_at: number; archived: boolean; active: boolean };
type SessionDetail = { id: string; messages: Array<{ role: "user" | "assistant"; content: string; attachment_count: number }> };
type RunStep = { id: string; label: string; status: "active" | "done" | "failed" };
type ChatMessage = { id: string; role: "user" | "assistant" | "activity"; content: string; steps?: RunStep[] };
type Attachment = { kind: "text"; name: string; content: string } | { kind: "image"; name: string; media_type: string; data: string; width: number; height: number };
type ComposerSkill = { identifier: string; name: string; description: string };
type ComposerData = { commands: string[]; skills: ComposerSkill[] };
const defaultCommands = ["/help", "/goal", "/compress", "/skills", "/clear"];

async function json<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, init); const value = await response.json();
  if (!response.ok) throw new Error(value.error ?? response.statusText); return value as T;
}

async function mutate(url: string, init: RequestInit) {
  const response = await fetch(url, init);
  if (!response.ok) throw new Error((await response.text()) || response.statusText);
}

function nextId(prefix: string) { return `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2)}`; }

export function App() {
  const [language, setLanguage] = useState<Language>(detectLanguage); const t = messages[language];
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]); const [workspace, setWorkspace] = useState("");
  const [sessions, setSessions] = useState<Session[]>([]); const [sessionId, setSessionId] = useState("");
  const [sessionSearch, setSessionSearch] = useState("");
  const [chat, setChat] = useState<ChatMessage[]>([]); const [prompt, setPrompt] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [goal, setGoal] = useState("");
  const [composer, setComposer] = useState<ComposerData>({ commands: defaultCommands, skills: [] });
  const [runtimeActivity, setRuntimeActivity] = useState<RuntimeActivity>({ tools: [], artifacts: [], agents: [], gates: [], attention_count: 0 });
  const [busy, setBusy] = useState(false); const [activity, setActivity] = useState(""); const [error, setError] = useState("");
  const abortRef = useRef<AbortController | null>(null); const endRef = useRef<HTMLDivElement | null>(null);
  const activeRunRef = useRef<string | null>(null);
  const activeTurnRef = useRef<string | null>(null);
  const pendingStopRef = useRef(false);
  const chatViewportRef = useRef<HTMLDivElement | null>(null); const followBottomRef = useRef(true);

  useEffect(() => { document.title = t.documentTitle; document.documentElement.lang = language; localStorage.setItem("willdeep.language", language); }, [language, t.documentTitle]);
  useEffect(() => { json<Workspace[]>("/api/workspaces").then((values) => { setWorkspaces(values); setWorkspace(values[0]?.path ?? ""); }).catch((reason: Error) => setError(`${t.loadFailed}: ${reason.message}`)); }, [t.loadFailed]);
  useEffect(() => { if (!workspace) return; json<Session[]>("/api/sessions").then(setSessions).catch((reason: Error) => setError(`${t.loadFailed}: ${reason.message}`)); json<ComposerData>(`/api/composer?workspace=${encodeURIComponent(workspace)}`).then(setComposer).catch((reason: Error) => setError(`${t.loadFailed}: ${reason.message}`)); setSessionId(""); setChat([]); }, [workspace, t.loadFailed]);
  useEffect(() => {
    if (!workspace) { setRuntimeActivity({ tools: [], artifacts: [], agents: [], gates: [], attention_count: 0 }); return; }
    let active = true;
    const refresh = () => json<RuntimeActivity>(`/api/runtime/activity?workspace=${encodeURIComponent(workspace)}`).then((value) => { if (active) setRuntimeActivity(value); }).catch(() => undefined);
    void refresh(); const timer = window.setInterval(refresh, 2000);
    return () => { active = false; window.clearInterval(timer); };
  }, [workspace]);
  useEffect(() => { if (followBottomRef.current) endRef.current?.scrollIntoView({ behavior: "smooth", block: "end" }); }, [chat, activity]);

  const visibleSessions = useMemo(() => { const query = sessionSearch.trim().toLowerCase(); return sessions.filter((item) => item.workspace === workspace && (!query || item.title.toLowerCase().includes(query))); }, [sessions, workspace, sessionSearch]);
  const selectedSession = useMemo(() => sessions.find((item) => item.id === sessionId), [sessions, sessionId]);
  const commandMatches = useMemo(() => prompt.startsWith("/") ? composer.commands.filter((item) => item.startsWith(prompt.split(/\s/)[0])).slice(0, 6) : [], [prompt, composer.commands]);
  const skillQuery = prompt.match(/(?:^|\s)\$([\w-]*)$/)?.[1]?.toLowerCase();
  const skillMatches = useMemo(() => skillQuery === undefined ? [] : composer.skills.filter((skill) => `${skill.identifier} ${skill.name} ${skill.description}`.toLowerCase().includes(skillQuery)).slice(0, 8), [skillQuery, composer.skills]);

  function updateRun(runId: string, updater: (steps: RunStep[]) => RunStep[]) {
    setChat((current) => current.map((message) => message.id === runId ? { ...message, steps: updater(message.steps ?? []) } : message));
  }

  async function stopRemoteTurn(turnId: string) {
    const response = await fetch(`/api/turns/${encodeURIComponent(turnId)}/stop`, { method: "POST" });
    if (!response.ok) throw new Error(await response.text());
    abortRef.current?.abort(); setActivity(t.stopped); setBusy(false); abortRef.current = null; activeTurnRef.current = null;
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

  async function loadSession(id: string) {
    if (busy) return;
    setError("");
    try {
      const detail = await json<SessionDetail>(`/api/sessions/${encodeURIComponent(id)}`);
      setSessionId(detail.id);
      setChat(detail.messages.map((message, index) => ({
        id: `${detail.id}-${index}`,
        role: message.role,
        content: `${message.content}${message.attachment_count ? `\n[${message.attachment_count} ${t.attachmentCount}]` : ""}`,
      })));
      followBottomRef.current = true;
    } catch (reason) {
      setError(`${t.loadFailed}: ${reason instanceof Error ? reason.message : String(reason)}`);
    }
  }

  async function refreshSessions() {
    setSessions(await json<Session[]>("/api/sessions"));
  }

  async function renameSelectedSession() {
    if (!selectedSession) return;
    const title = window.prompt(t.renamePrompt, selectedSession.title)?.trim(); if (!title) return;
    try { await mutate(`/api/sessions/${encodeURIComponent(selectedSession.id)}/rename`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ title }) }); await refreshSessions(); }
    catch (reason) { setError(`${t.sessionActionFailed}: ${reason instanceof Error ? reason.message : String(reason)}`); }
  }

  async function forkSelectedSession() {
    if (!selectedSession) return;
    const title = window.prompt(t.forkPrompt, `${selectedSession.title}`)?.trim(); if (!title) return;
    try { const fork = await json<{ id: string }>(`/api/sessions/${encodeURIComponent(selectedSession.id)}/fork`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ title }) }); await refreshSessions(); await loadSession(fork.id); }
    catch (reason) { setError(`${t.sessionActionFailed}: ${reason instanceof Error ? reason.message : String(reason)}`); }
  }

  async function toggleArchiveSelectedSession() {
    if (!selectedSession) return;
    const action = selectedSession.archived ? "unarchive" : "archive";
    try { await mutate(`/api/sessions/${encodeURIComponent(selectedSession.id)}/${action}`, { method: "POST" }); await refreshSessions(); }
    catch (reason) { setError(`${t.sessionActionFailed}: ${reason instanceof Error ? reason.message : String(reason)}`); }
  }

  async function deleteSelectedSession() {
    if (!selectedSession || !window.confirm(t.deleteConfirm)) return;
    try { await mutate(`/api/sessions/${encodeURIComponent(selectedSession.id)}`, { method: "DELETE", headers: { "content-type": "application/json" }, body: JSON.stringify({ confirmation: selectedSession.id }) }); setSessionId(""); setChat([]); await refreshSessions(); }
    catch (reason) { setError(`${t.sessionActionFailed}: ${reason instanceof Error ? reason.message : String(reason)}`); }
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
    const imageItem = Array.from(event.clipboardData.items).find((item) => item.kind === "file" && item.type.startsWith("image/"));
    if (imageItem) { const file = imageItem.getAsFile(); if (file) { event.preventDefault(); attachImage(file); } return; }
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
    try {
      const response = await fetch("/api/chat/stream", { method: "POST", signal: controller.signal, headers: { "content-type": "application/json", accept: "text/event-stream" }, body: JSON.stringify({ prompt: harnessPrompt, session_id: sessionId || null, workspace, language, attachments: outgoingAttachments }) });
      if (!response.ok || !response.body) throw new Error(await response.text());
      const reader = response.body.getReader(); const decoder = new TextDecoder(); let buffer = ""; let answer = "";
      for (;;) {
        const { done, value } = await reader.read(); if (done) break; buffer += decoder.decode(value, { stream: true });
        const frames = buffer.split("\n\n"); buffer = frames.pop() ?? "";
        for (const frame of frames) {
          const line = frame.split("\n").find((value) => value.startsWith("data: ")); if (!line) continue;
          const event = JSON.parse(line.slice(6));
          if (event.type === "submitted") {
            setSessionId(event.session_id || ""); activeTurnRef.current = event.turn_id || null;
            if (pendingStopRef.current && activeTurnRef.current) await stopRemoteTurn(activeTurnRef.current);
          }
          else if (event.type === "completed") { answer = event.text || t.emptyReply; setSessionId(event.session_id || ""); activeTurnRef.current = null; }
          else if (event.type === "error") throw new Error(event.message);
          else if (event.type === "thought") setActivity(event.text || t.thinking);
          else if (event.type === "turn_started") { setActivity(event.label); updateRun(runId, (steps) => [...steps.map((step) => step.status === "active" ? { ...step, status: "done" as const } : step), { id: nextId("turn"), label: event.label, status: "active" }]); }
          else if (event.type === "tool_requested") { setActivity(event.label); updateRun(runId, (steps) => [...steps, { id: event.id || nextId("tool"), label: event.label, status: "active" }]); }
          else if (event.type === "tool_completed") { setActivity(event.label); updateRun(runId, (steps) => steps.map((step) => step.id === event.id ? { ...step, label: event.label, status: event.is_error ? "failed" : "done" } : step)); }
          else setActivity(event.label || event.type);
        }
      }
      updateRun(runId, (steps) => steps.map((step) => step.status === "active" ? { ...step, status: "done" } : step));
      setChat((current) => [...current, { id: nextId("assistant"), role: "assistant", content: answer || t.emptyReply }]);
      refreshSessions().catch(() => undefined);
    } catch (reason) {
      if (!(reason instanceof DOMException && reason.name === "AbortError")) setError(`${t.requestFailed}: ${reason instanceof Error ? reason.message : String(reason)}`);
    } finally { if (abortRef.current === controller) abortRef.current = null; if (activeRunRef.current === runId) activeRunRef.current = null; activeTurnRef.current = null; pendingStopRef.current = false; setBusy(false); setActivity(""); }
  }

  return <Flex minH="100vh" bg="#080d12" color="#e7edf4">
    <Box as="aside" w={{ base: "0", md: "282px" }} display={{ base: "none", md: "block" }} borderRight="1px solid" borderColor="#202a35" p="5" bg="#0b1118">
      <Heading size="lg">{t.appName}</Heading><Text color="#718096" mb="8">{t.webHarness}</Text>
      <Text fontSize="xs" color="#8290a3" mb="2">{t.language}</Text><NativeSelect.Root mb="5"><NativeSelect.Field aria-label={t.language} value={language} onChange={(event) => setLanguage(event.target.value as Language)} bg="#101820" borderColor="#2b3948">{languages.map((code) => <option key={code} value={code}>{languageLabels[code]}</option>)}</NativeSelect.Field><NativeSelect.Indicator /></NativeSelect.Root>
      <Text fontSize="xs" color="#8290a3" mb="2">{t.workspace}</Text><NativeSelect.Root><NativeSelect.Field aria-label={t.workspace} value={workspace} onChange={(event) => setWorkspace(event.target.value)} bg="#101820" borderColor="#2b3948">{workspaces.map((item) => <option key={item.id} value={item.path}>{item.name} · {item.access === "read_only" ? t.readOnly : item.access === "smart" ? t.smartApproval : t.workspaceWrite}{item.active ? ` · ${t.activeWorkspace}` : ""}</option>)}</NativeSelect.Field><NativeSelect.Indicator /></NativeSelect.Root>
      <RuntimeSidebar activity={runtimeActivity} messages={t} />
      <Flex mt="8" mb="3" justify="space-between"><Text fontSize="xs" color="#8290a3">{t.session}</Text><Button size="xs" variant="ghost" disabled={busy} onClick={() => { setSessionId(""); setChat([]); }}>{t.newSession}</Button></Flex>
      <Input size="sm" mb="2" value={sessionSearch} onChange={(event) => setSessionSearch(event.target.value)} placeholder={t.searchSessions} aria-label={t.searchSessions} />
      <VStack align="stretch" gap="1">{visibleSessions.slice(0, 20).map((item) => <Button key={item.id} size="sm" opacity={item.archived ? 0.58 : 1} variant={sessionId === item.id ? "subtle" : "ghost"} justifyContent="start" overflow="hidden" disabled={busy} onClick={() => void loadSession(item.id)}>{item.title}{item.archived ? ` · ${t.archived}` : ""}</Button>)}{!visibleSessions.length && <Text color="#657386" fontSize="sm">{t.noSessions}</Text>}</VStack>
      {selectedSession && <Flex mt="2" gap="1" wrap="wrap"><Button size="xs" variant="ghost" title={t.renameSession} aria-label={t.renameSession} disabled={busy || selectedSession.active} onClick={() => void renameSelectedSession()}>R</Button><Button size="xs" variant="ghost" title={t.forkSession} aria-label={t.forkSession} disabled={busy || selectedSession.active} onClick={() => void forkSelectedSession()}>⑂</Button><Button size="xs" variant="ghost" title={selectedSession.archived ? t.unarchiveSession : t.archiveSession} aria-label={selectedSession.archived ? t.unarchiveSession : t.archiveSession} disabled={busy || selectedSession.active} onClick={() => void toggleArchiveSelectedSession()}>{selectedSession.archived ? "↥" : "▣"}</Button><Button size="xs" variant="ghost" title={t.exportSession} aria-label={t.exportSession} disabled={busy} onClick={() => void exportSelectedSession()}>⇩</Button><Button size="xs" variant="ghost" colorPalette="red" title={t.deleteSession} aria-label={t.deleteSession} disabled={busy || selectedSession.active} onClick={() => void deleteSelectedSession()}>×</Button></Flex>}
    </Box>
    <Container maxW="920px" px={{ base: "4", md: "8" }} py="6" display="flex" flexDir="column" h="100vh">
      <Box ref={chatViewportRef} className="chat-viewport" flex="1" minH="0" overflowY="auto" pb="6" onScroll={() => { const node = chatViewportRef.current; if (node) followBottomRef.current = node.scrollHeight - node.scrollTop - node.clientHeight < 80; }}>{!chat.length && <Box py="24"><Heading size="2xl" mb="4">{t.welcomeTitle}</Heading><Text color="#8b99aa">{t.welcomeBody}</Text></Box>}
        <VStack align="stretch" gap="4">{chat.map((message) => message.role === "activity" ? <Box key={message.id} className="run-card">{message.steps?.map((step) => <Flex key={step.id} className={`run-step ${step.status}`}><Box className="step-dot" /><Text>{step.label}</Text></Flex>)}</Box> : <Box key={message.id} className={`message ${message.role}`}>{message.content}</Box>)}</VStack>
        {error && <Text color="#ff8f8f" py="4">{error}</Text>}<div ref={endRef} />
      </Box>
      <Box className="composer-shell">
        {busy && <Flex className="thinking-strip"><Box className="thinking-pulse" /><Text title={activity}>{activity || t.thinking}</Text></Flex>}
        {commandMatches.length > 0 && <Box className="suggestions"><Text className="suggestion-title">{t.commands}</Text>{commandMatches.map((command) => <button key={command} type="button" onMouseDown={(event) => { event.preventDefault(); setPrompt(command); }}>{command}</button>)}</Box>}
        {skillQuery !== undefined && <Box className="suggestions"><Text className="suggestion-title">{t.skills}</Text>{skillMatches.length ? skillMatches.map((skill) => <button key={skill.identifier} type="button" onMouseDown={(event) => { event.preventDefault(); setPrompt((current) => current.replace(/\$[\w-]*$/, `$${skill.identifier} `)); }}><strong>${skill.identifier}</strong><small>{skill.name} · {skill.description}</small></button>) : <Text className="suggestion-empty">{t.noSkills}</Text>}</Box>}
        {attachments.length > 0 && <Flex className="attachment-row">{attachments.map((attachment, index) => <Box key={`${attachment.name}-${index}`} className="attachment-chip">{attachment.kind === "image" ? <img src={`data:${attachment.media_type};base64,${attachment.data}`} alt={attachment.name} /> : <Box className="text-attachment">TXT</Box>}<Text title={attachment.name}>{attachment.name}</Text><button type="button" aria-label={t.removeAttachment} title={t.removeAttachment} onClick={() => setAttachments((current) => current.filter((_, itemIndex) => itemIndex !== index))}>×</button></Box>)}</Flex>}
        <Textarea value={prompt} onPaste={handlePaste} onChange={(event) => setPrompt(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void send(); } }} placeholder={t.promptPlaceholder} minH="104px" maxH="240px" resize="vertical" border="0" _focus={{ boxShadow: "none" }} px="4" pt={attachments.length ? "2" : "4"} pb="12" />
        <Text className="send-hint">{t.sendHint}</Text>
        <Button aria-label={busy ? t.stop : t.send} title={busy ? t.stop : t.send} className={`send-button ${busy ? "stop" : ""}`} onClick={busy ? () => void stop() : () => void send()} disabled={!busy && ((!prompt.trim() && !attachments.length) || selectedSession?.archived)}>{busy ? <Box className="stop-icon" /> : <Text className="send-icon">↑</Text>}</Button>
      </Box>
    </Container>
  </Flex>;
}
