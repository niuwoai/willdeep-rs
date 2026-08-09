import { useEffect, useMemo, useState } from "react";
import { Box, Button, Container, Flex, Heading, NativeSelect, Spinner, Text, Textarea, VStack } from "@chakra-ui/react";
import { detectLanguage, languageLabels, languages, messages, type Language } from "./i18n";

type Workspace = { path: string; name: string };
type Session = { id: string; title: string; workspace: string; updated_at: number };
type ChatMessage = { role: "user" | "assistant"; content: string };

async function json<T>(url: string): Promise<T> {
  const response = await fetch(url);
  const value = await response.json();
  if (!response.ok) throw new Error(value.error ?? response.statusText);
  return value as T;
}

export function App() {
  const [language, setLanguage] = useState<Language>(detectLanguage);
  const t = messages[language];
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [workspace, setWorkspace] = useState("");
  const [sessions, setSessions] = useState<Session[]>([]);
  const [sessionId, setSessionId] = useState("");
  const [chat, setChat] = useState<ChatMessage[]>([]);
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  const [activity, setActivity] = useState("");
  const [error, setError] = useState("");

  useEffect(() => { document.title = t.documentTitle; document.documentElement.lang = language; localStorage.setItem("willdeep.language", language); }, [language, t.documentTitle]);
  useEffect(() => { json<Workspace[]>("/api/workspaces").then((values) => { setWorkspaces(values); setWorkspace(values[0]?.path ?? ""); }).catch((reason: Error) => setError(`${t.loadFailed}: ${reason.message}`)); }, [t.loadFailed]);
  useEffect(() => { if (!workspace) return; json<Session[]>("/api/sessions").then(setSessions).catch((reason: Error) => setError(`${t.loadFailed}: ${reason.message}`)); setSessionId(""); setChat([]); }, [workspace, t.loadFailed]);

  const visibleSessions = useMemo(() => sessions.filter((item) => item.workspace === workspace), [sessions, workspace]);
  const changeLanguage = (value: string) => setLanguage(value as Language);

  async function send() {
    const content = prompt.trim();
    if (!content || busy || !workspace) return;
    setPrompt(""); setError(""); setActivity(t.working); setBusy(true);
    setChat((current) => [...current, { role: "user", content }]);
    try {
      const response = await fetch("/api/chat/stream", {
        method: "POST", headers: { "content-type": "application/json", accept: "text/event-stream" },
        body: JSON.stringify({ prompt: content, session_id: sessionId || null, workspace, language }),
      });
      if (!response.ok || !response.body) throw new Error(await response.text());
      const reader = response.body.getReader(); const decoder = new TextDecoder();
      let buffer = ""; let answer = "";
      for (;;) {
        const { done, value } = await reader.read(); if (done) break;
        buffer += decoder.decode(value, { stream: true }); const frames = buffer.split("\n\n"); buffer = frames.pop() ?? "";
        for (const frame of frames) {
          const line = frame.split("\n").find((value) => value.startsWith("data: ")); if (!line) continue;
          const event = JSON.parse(line.slice(6));
          if (event.type === "completed") { answer = event.text || t.emptyReply; setSessionId(event.session_id || ""); }
          else if (event.type === "error") throw new Error(event.message);
          else setActivity(event.label || event.type);
        }
      }
      setChat((current) => [...current, { role: "assistant", content: answer || t.emptyReply }]);
    } catch (reason) { setError(`${t.requestFailed}: ${reason instanceof Error ? reason.message : String(reason)}`); }
    finally { setBusy(false); setActivity(""); }
  }

  return <Flex minH="100vh" bg="#0b0f14" color="#e7edf4">
    <Box as="aside" w={{ base: "0", md: "300px" }} display={{ base: "none", md: "block" }} borderRight="1px solid" borderColor="#25303c" p="5">
      <Heading size="lg">{t.appName}</Heading><Text color="#8290a3" mb="8">{t.webHarness}</Text>
      <Text fontSize="sm" mb="2">{t.language}</Text>
      <NativeSelect.Root mb="5"><NativeSelect.Field aria-label={t.language} value={language} onChange={(event) => changeLanguage(event.target.value)} bg="#111821" borderColor="#334155">{languages.map((code) => <option key={code} value={code}>{languageLabels[code]}</option>)}</NativeSelect.Field><NativeSelect.Indicator /></NativeSelect.Root>
      <Text fontSize="sm" mb="2">{t.workspace}</Text>
      <NativeSelect.Root><NativeSelect.Field aria-label={t.workspace} value={workspace} onChange={(event) => setWorkspace(event.target.value)} bg="#111821" borderColor="#334155">{workspaces.map((item) => <option key={item.path} value={item.path}>{item.name}</option>)}</NativeSelect.Field><NativeSelect.Indicator /></NativeSelect.Root>
      <Flex mt="8" mb="3" justify="space-between"><Text fontSize="sm">{t.session}</Text><Button size="xs" variant="ghost" onClick={() => { setSessionId(""); setChat([]); }}>{t.newSession}</Button></Flex>
      <VStack align="stretch" gap="2">{visibleSessions.slice(0, 20).map((item) => <Button key={item.id} variant={sessionId === item.id ? "solid" : "ghost"} justifyContent="start" overflow="hidden" onClick={() => { setSessionId(item.id); setChat([]); }}>{item.title}</Button>)}{!visibleSessions.length && <Text color="#8290a3" fontSize="sm">{t.noSessions}</Text>}</VStack>
    </Box>
    <Container maxW="900px" py="8" display="flex" flexDir="column" minH="100vh"><Box flex="1">
      {!chat.length && <Box py="24"><Heading size="2xl" mb="4">{t.welcomeTitle}</Heading><Text color="#9aa7b8">{t.welcomeBody}</Text></Box>}
      <VStack align="stretch" gap="4">{chat.map((message, index) => <Box key={`${message.role}-${index}`} alignSelf={message.role === "user" ? "end" : "stretch"} maxW={message.role === "user" ? "80%" : "100%"} bg={message.role === "user" ? "#173151" : "#111821"} border="1px solid #283545" rounded="xl" px="5" py="4" whiteSpace="pre-wrap">{message.content}</Box>)}</VStack>
      {busy && <Flex gap="3" align="center" py="5"><Spinner size="sm" /><Text color="#9aa7b8">{activity || t.working}</Text></Flex>}{error && <Text color="#ff8f8f" py="4">{error}</Text>}
    </Box><Box position="sticky" bottom="0" bg="#0b0f14" pt="4" pb="2"><Textarea value={prompt} onChange={(event) => setPrompt(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void send(); } }} placeholder={t.promptPlaceholder} minH="110px" bg="#111821" borderColor="#334155" /><Flex justify="end" mt="3"><Button colorPalette="blue" loading={busy} onClick={() => void send()}>{t.send}</Button></Flex></Box></Container>
  </Flex>;
}
