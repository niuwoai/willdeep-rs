// 插件页面宿主：一个 sandbox 过的 iframe，加一条通往 Rust 宿主的代理。
//
// 插件页面自己够不着任何网络端点（CSP `connect-src 'none'`），它能做的只有
// postMessage 给这个父窗口；父窗口再按清单声明的边界去调宿主 API。所以这里
// 是唯一的闸门，每条消息都要先认身份（event.source 必须是这个 iframe），
// 再认类型。
//
// 两套协议共用这条通道：
//   - `{__willdeep: 1, …}`  window.willdeep.* 的桥（bootstrap 注入）
//   - `{jsonrpc: "2.0", …}` MCP Apps 页面的标准握手（页面直接 postMessage 给 parent）

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Box, Flex, Text } from "@chakra-ui/react";
import type { Messages } from "./i18n";
import { SfIcon } from "./sfSymbols";
import {
  callPluginTool,
  executePluginCommand,
  pageUrl,
  pluginCancel,
  pluginComplete,
  pluginFetch,
  pluginFs,
  pluginGenerateImage,
  pluginHostAction,
  pluginProviders,
  pluginRunProcess,
  pluginSkills,
  readPluginResource,
  readPluginStorage,
  uploadPluginFile,
  writePluginStorage,
  writePluginStore,
  type DestinationContext,
  type PluginDestinationView,
  type PluginView,
} from "./plugins";

type Props = {
  plugin: PluginView;
  destination: PluginDestinationView;
  messages: Messages;
  locale: string;
  workspace: string | null;
  sessionId: string | null;
  selectedItemId: string | null;
  onSelectItem: (itemId: string | null) => void;
  onNavigate: (qualifiedDestination: string) => void;
  onOpenPluginCenter: () => void;
  /** 主 Agent 正在跑。宿主事件 turn.started / turn.finished 由它推出。 */
  busy: boolean;
  /** `window.willdeep.chat.*`：insert 只填输入框，send 才真的起回合。 */
  onChatText: (text: string, send: boolean) => void;
  /** `window.willdeep.openConversation`：跳到那条会话。 */
  onOpenSession: (sessionId: string) => void;
};

type JsonRpc = { jsonrpc: "2.0"; id?: number | string; method?: string; params?: Record<string, unknown> };

type BridgeMessage = {
  __willdeep: 1;
  type: string;
  requestID?: string;
  commandID?: string;
  arguments?: unknown;
  itemID?: string;
  request?: unknown;
  key?: string;
  value?: unknown;
  // fs.* / process.run / net.fetch / chat.* / events / 会话跳转的载荷。
  path?: string;
  text?: string;
  query?: string;
  regex?: boolean;
  limit?: number;
  oldString?: string;
  newString?: string;
  replaceAll?: boolean;
  command?: string;
  url?: string;
  method?: string;
  headers?: Record<string, string>;
  body?: string | null;
  title?: string;
  name?: string;
  streamID?: string;
  sessionID?: string;
  messageID?: string;
};

/**
 * 浏览器侧的「选文件」。
 *
 * macOS 宿主上这一步是插件的 MCP 服务弹原生框；在这里服务可能跑在另一台
 * 机器上，那个框会弹在没人看的屏幕上。所以改成：宿主页面弹浏览器文件框，
 * 文件上传到本插件隔离的目录，再把落地的服务端路径当作选择结果。
 */
function chooseLocalFile(): Promise<File | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "image/png,image/jpeg,image/webp";
    input.style.display = "none";
    document.body.appendChild(input);
    // 取消不会触发 change，所以窗口一拿回焦点就当没选——否则这个 Promise
    // 永远不落地，插件页面就一直转圈等一个不会来的结果。
    const finish = (file: File | null) => {
      window.removeEventListener("focus", onFocus);
      input.remove();
      resolve(file);
    };
    const onFocus = () => window.setTimeout(() => {
      if (!input.files?.length) finish(null);
    }, 500);
    input.addEventListener("change", () => finish(input.files?.[0] ?? null));
    window.addEventListener("focus", onFocus, { once: true });
    input.click();
  });
}

function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error("readFailed"));
    reader.onload = () => {
      const result = String(reader.result ?? "");
      const comma = result.indexOf(",");
      resolve(comma >= 0 ? result.slice(comma + 1) : result);
    };
    reader.readAsDataURL(file);
  });
}

export function PluginPage({
  plugin,
  destination,
  messages,
  locale,
  workspace,
  sessionId,
  selectedItemId,
  onSelectItem,
  onNavigate,
  onOpenPluginCenter,
  busy,
  onChatText,
  onOpenSession,
}: Props) {
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  // 页面订阅过的宿主事件。没人订的事件一条都不推——插件不关心的东西不该
  // 每回合都穿过这条桥。
  const subscribedEvents = useRef<Set<string>>(new Set());
  const [reloadKey, setReloadKey] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [busyCommand, setBusyCommand] = useState<string | null>(null);
  // MCP Apps 的握手是有序的：宿主在 initialized 之前不受理 tools/call
  // 与 resources/read。乱序的页面应该拿到明确的 -32002，而不是一个能用的结果。
  const initialized = useRef(false);

  const url = useMemo(() => pageUrl(plugin, destination), [plugin, destination]);

  // 权限决定上下文里有什么。这里只组装，实际的字段裁剪在 Rust 侧按清单做过，
  // 这一层再按 permissions 挡一次，免得前端把不该给的引用塞进去。
  const context: DestinationContext = useMemo(() => {
    const permissions = new Set(plugin.permissions);
    const canReadWorkspace = permissions.has("workspace.read") || permissions.has("workspace.write");
    return {
      destinationID: destination.qualified_id,
      selectedItemID: selectedItemId,
      workspaceReference: canReadWorkspace ? workspace : null,
      sessionReference: permissions.has("conversation.read") ? sessionId : null,
      locale,
      colorScheme: "dark",
    };
  }, [plugin.permissions, destination.qualified_id, selectedItemId, workspace, sessionId, locale]);

  const post = useCallback((payload: unknown) => {
    frameRef.current?.contentWindow?.postMessage(payload, "*");
  }, []);

  const pushContext = useCallback(() => {
    post({ __willdeep: 1, type: "context", context });
    if (initialized.current) {
      post({
        __willdeep: 1,
        type: "mcpMessage",
        message: {
          jsonrpc: "2.0",
          method: "ui/notifications/host-context-changed",
          params: { willdeep: context },
        },
      });
    }
  }, [context, post]);

  useEffect(() => {
    pushContext();
  }, [pushContext]);

  // 目的地或页面换了就是一次全新的加载，握手状态必须跟着清零。
  // 订阅也要一起清：新页面还没开口订阅，不该继承上一页的订阅。
  useEffect(() => {
    initialized.current = false;
    subscribedEvents.current = new Set();
  }, [destination.qualified_id, reloadKey]);

  // 宿主事件。只推页面订阅过的那些；订阅时的权限已在 Rust 侧核过。
  const pushHostEvent = useCallback(
    (name: string, payload: Record<string, unknown>) => {
      if (!subscribedEvents.current.has(name)) return;
      post({ __willdeep: 1, type: "hostEvent", name, payload });
    },
    [post]
  );
  const lastBusy = useRef(busy);
  useEffect(() => {
    if (lastBusy.current !== busy) {
      lastBusy.current = busy;
      pushHostEvent(busy ? "turn.started" : "turn.finished", { sessionID: sessionId });
    }
  }, [busy, sessionId, pushHostEvent]);
  useEffect(() => {
    pushHostEvent("session.changed", { sessionID: sessionId });
  }, [sessionId, pushHostEvent]);
  useEffect(() => {
    pushHostEvent("workspace.changed", { workspace });
  }, [workspace, pushHostEvent]);

  const applyHostAction = useCallback(
    (action: string | undefined, navigateTo: string | undefined) => {
      switch (action) {
        case "plugin.refresh":
          setReloadKey((current) => current + 1);
          break;
        case "plugins.open-center":
        case "settings.mcp":
          onOpenPluginCenter();
          break;
        case "destination.select":
          break;
        default:
          break;
      }
      if (navigateTo) onNavigate(navigateTo);
    },
    [onNavigate, onOpenPluginCenter]
  );

  const runCommand = useCallback(
    async (commandId: string, args: unknown) => {
      // 「选文件」类命令在这里改道：先让用户在浏览器里挑，再把上传后的
      // 服务端路径交给宿主去合成结果。不改道的话请求会进 MCP 服务，
      // 那边去弹一个没人看得见的原生框，然后超时。
      let payload = args;
      if (plugin.file_picker_commands.includes(commandId)) {
        const file = await chooseLocalFile();
        if (!file) throw new Error("selection_cancelled");
        const uploaded = await uploadPluginFile(plugin.id, file.name, await fileToBase64(file));
        payload = { ...(args as Record<string, unknown> | null), path: uploaded.path };
      }
      const response = await executePluginCommand(plugin.id, commandId, payload);
      applyHostAction(response.action, response.destination);
      return response.kind === "tool" ? response.result : { kind: response.kind };
    },
    [plugin.id, plugin.file_picker_commands, applyHostAction]
  );

  useEffect(() => {
    const handler = async (event: MessageEvent) => {
      const frame = frameRef.current;
      // 身份靠 source 认，不靠 origin 字符串：sandbox 出来的文档是 opaque
      // origin，event.origin 恒为 "null"，拿它做判断等于没判断。
      if (!frame || !event.source || event.source !== frame.contentWindow) return;
      const data = event.data as (Partial<BridgeMessage> & Partial<JsonRpc>) | null;
      if (!data || typeof data !== "object") return;

      if (data.__willdeep === 1 && typeof data.type === "string") {
        await handleBridge(data as BridgeMessage);
        return;
      }
      if (data.jsonrpc === "2.0" && typeof data.method === "string") {
        await handleMcp(data as JsonRpc);
      }
    };

    const replyBridge = (requestID: string, result: unknown, failure?: string) => {
      post({ __willdeep: 1, type: "bridgeResult", requestID, result, error: failure });
    };

    /**
     * 桥请求的实际派发。
     *
     * 权限**一律**在 Rust 侧核：这里看起来像是「前端在调 API」，但每个
     * 端点第一句都是清单权限校验，页面自报的东西一个都不作数。这一层只做
     * 三件事：转发、把只能在浏览器里发生的效果（剪贴板、通知、把文本递进
     * 输入框）落地、以及替 process.run 弹那个确认框。
     */
    const runBridgeRequest = async (data: BridgeMessage): Promise<unknown> => {
      switch (data.type) {
        case "aiProviders":
          return pluginProviders(plugin.id);
        case "aiComplete":
          return pluginComplete(plugin.id, data.request ?? {});
        case "aiCancel":
          return pluginCancel(plugin.id, data.streamID ?? "");
        case "aiGenerateImage":
          return pluginGenerateImage(plugin.id, data.request ?? {});
        case "skillsList":
          return pluginSkills(plugin.id);
        case "fsList":
          return pluginFs(plugin.id, "list", { path: data.path ?? "" });
        case "fsRead":
          return pluginFs(plugin.id, "read", { path: data.path ?? "" });
        case "fsSearch":
          return pluginFs(plugin.id, "search", {
            query: data.query ?? "",
            path: data.path ?? "",
            regex: !!data.regex,
            limit: data.limit ?? 0,
          });
        case "fsWrite":
          return pluginFs(plugin.id, "write", { path: data.path ?? "", text: data.text ?? "" });
        case "fsPatch":
          return pluginFs(plugin.id, "patch", {
            path: data.path ?? "",
            oldString: data.oldString ?? "",
            newString: data.newString ?? "",
            replaceAll: !!data.replaceAll,
          });
        case "processRun": {
          const command = (data.command ?? "").trim();
          if (!command) throw new Error("invalidCommand");
          // 先按「不确认」问一次：只读命令直接就跑完了，用户不该为
          // `git status` 看一个确认框。宿主说要确认，才弹。
          try {
            return await pluginRunProcess(plugin.id, command, false);
          } catch (reason) {
            const message = reason instanceof Error ? reason.message : String(reason);
            if (message !== "confirmationRequired") throw reason;
            // 确认框弹在**宿主页面**上，不在沙箱 iframe 里：iframe 够不着
            // 这个接口，所以「确认过了」这一位只可能由这里带上，与 macOS
            // 宿主的那个 NSAlert 是同一道门。
            const approved = window.confirm(
              `${plugin.name}\n\n${messages.pluginRunCommandConfirm}\n\n${command}`
            );
            if (!approved) throw new Error("commandDeclined");
            return pluginRunProcess(plugin.id, command, true);
          }
        }
        case "netFetch":
          return pluginFetch(plugin.id, {
            url: data.url ?? "",
            method: data.method ?? "GET",
            headers: data.headers ?? {},
            body: data.body ?? null,
          });
        case "clipboardWrite": {
          const allowed = await pluginHostAction(plugin.id, "clipboardWrite", { text: data.text ?? "" });
          await navigator.clipboard.writeText(String(allowed.text ?? ""));
          return { ok: true };
        }
        case "notify": {
          const allowed = await pluginHostAction(plugin.id, "notify", {
            title: data.title ?? "",
            body: data.body ?? "",
          });
          if ("Notification" in window && Notification.permission === "granted") {
            new Notification(String(allowed.title ?? ""), { body: String(allowed.body ?? "") });
          }
          return { ok: true };
        }
        case "chatInsert":
        case "chatSend": {
          const allowed = await pluginHostAction(plugin.id, data.type, { text: data.text ?? "" });
          onChatText(String(allowed.text ?? ""), data.type === "chatSend");
          return { ok: true };
        }
        case "eventsSubscribe": {
          const result = await pluginHostAction(plugin.id, "eventsSubscribe", { name: data.name ?? "" });
          subscribedEvents.current.add(String(data.name ?? ""));
          return result;
        }
        case "openConversation": {
          const allowed = await pluginHostAction(plugin.id, "openConversation", {
            sessionID: data.sessionID ?? "",
          });
          onOpenSession(String(allowed.sessionID ?? ""));
          return { ok: true };
        }
        case "storageGet":
          return readPluginStorage(plugin.id, String(data.key ?? ""));
        case "storageKeys":
          return readPluginStorage(plugin.id);
        case "storageSet2":
          await writePluginStore(plugin.id, String(data.key ?? ""), data.value ?? null);
          return { ok: true };
        case "storageRemove2":
          await writePluginStore(plugin.id, String(data.key ?? ""), null);
          return { ok: true };
        default:
          throw new Error("unknownBridgeRequest");
      }
    };

    const handleBridge = async (data: BridgeMessage) => {
      switch (data.type) {
        case "selectItem":
          onSelectItem(data.itemID ?? null);
          break;
        case "refresh":
          setReloadKey((current) => current + 1);
          break;
        case "executeCommand": {
          if (!data.requestID || !data.commandID) return;
          try {
            const result = await runCommand(data.commandID, data.arguments);
            // 结果必须是 **JSON 字符串**，不是对象：macOS 宿主那边
            // `sendCommandResult(result: String?)` 送的就是字符串，共享的
            // 插件包因此一律 `JSON.parse(raw)`。这里直接把对象递过去，
            // 插件收到的是 "[object Object]"，每条命令都在第一步炸掉。
            post({
              __willdeep: 1,
              type: "commandResult",
              requestID: data.requestID,
              result: typeof result === "string" ? result : JSON.stringify(result),
            });
          } catch (reason) {
            post({
              __willdeep: 1,
              type: "commandResult",
              requestID: data.requestID,
              error: reason instanceof Error ? reason.message : String(reason),
            });
          }
          break;
        }
        case "aiProviders":
        case "aiComplete":
        case "aiCancel":
        case "aiGenerateImage":
        case "skillsList":
        case "fsList":
        case "fsRead":
        case "fsSearch":
        case "fsWrite":
        case "fsPatch":
        case "processRun":
        case "netFetch":
        case "clipboardWrite":
        case "notify":
        case "chatInsert":
        case "chatSend":
        case "eventsSubscribe":
        case "openConversation":
        case "storageGet":
        case "storageKeys":
        case "storageSet2":
        case "storageRemove2": {
          if (!data.requestID) return;
          try {
            replyBridge(data.requestID, await runBridgeRequest(data));
          } catch (reason) {
            // 拒绝的理由原样回到页面：待办插件据此决定是换模型还是回落到
            // 自己的本地规则——那条待办不该因为模型不可用就丢掉。
            replyBridge(data.requestID, null, reason instanceof Error ? reason.message : String(reason));
          }
          break;
        }
        case "storageSet":
        case "storageRemove":
          if (data.key) {
            void writePluginStorage(
              plugin.id,
              data.key,
              data.type === "storageSet" ? String(data.value ?? "") : null
            ).catch(() => undefined);
          }
          break;
        default:
          break;
      }
    };

    const replyMcp = (id: number | string | undefined, result: unknown, failure?: { code: number; message: string }) => {
      if (id === undefined || id === null) return;
      post({
        __willdeep: 1,
        type: "mcpMessage",
        message: failure
          ? { jsonrpc: "2.0", id, error: failure }
          : { jsonrpc: "2.0", id, result },
      });
    };

    const handleMcp = async (data: JsonRpc) => {
      const server = destination.page_server;
      switch (data.method) {
        case "ui/initialize":
          replyMcp(data.id, {
            protocolVersion: "2026-01-26",
            hostCapabilities: { tools: {}, resources: {} },
            hostInfo: { name: "willdeep-web", version: "1" },
            hostContext: { willdeep: context },
          });
          break;
        case "ui/notifications/initialized":
          initialized.current = true;
          break;
        case "tools/call":
        case "resources/read": {
          if (!initialized.current) {
            replyMcp(data.id, null, { code: -32002, message: "MCP App is not initialized" });
            return;
          }
          if (!server) {
            replyMcp(data.id, null, { code: -32601, message: "page declares no MCP server" });
            return;
          }
          try {
            const params = data.params ?? {};
            const result =
              data.method === "tools/call"
                ? await callPluginTool(plugin.id, server, String(params.name ?? ""), params.arguments ?? {})
                : await readPluginResource(plugin.id, server, String(params.uri ?? ""));
            replyMcp(data.id, result);
          } catch (reason) {
            replyMcp(data.id, null, {
              code: -32000,
              message: reason instanceof Error ? reason.message : String(reason),
            });
          }
          break;
        }
        default:
          // 通知没有 id，无需回复；带 id 的未知方法按标准回 -32601。
          replyMcp(data.id, null, { code: -32601, message: `unsupported method: ${data.method}` });
          break;
      }
    };

    window.addEventListener("message", handler);
    return () => window.removeEventListener("message", handler);
  }, [
    plugin.id,
    plugin.name,
    destination.page_server,
    context,
    post,
    runCommand,
    onSelectItem,
    onChatText,
    onOpenSession,
    messages.pluginRunCommandConfirm,
  ]);

  const toolbar = destination.toolbar_commands;

  return (
    <Flex direction="column" flex="1" minW="0" h="100vh" bg="var(--bg-page)">
      <Flex className="plugin-toolbar">
        <Text className="plugin-title">{destination.title}</Text>
        <Flex gap="1">
          {toolbar.map((command) => (
            <button
              key={command.id}
              type="button"
              className="plugin-toolbar-button"
              title={command.title}
              aria-label={command.title}
              disabled={busyCommand === command.id}
              onClick={async () => {
                setBusyCommand(command.id);
                setError(null);
                try {
                  await runCommand(command.id, {});
                } catch (reason) {
                  setError(reason instanceof Error ? reason.message : String(reason));
                } finally {
                  setBusyCommand(null);
                }
              }}
            >
              <SfIcon name={command.icon} size={16} />
            </button>
          ))}
        </Flex>
      </Flex>
      {error && (
        <Text className="plugin-error" role="alert">
          {messages.pluginCommandFailed}: {error}
        </Text>
      )}
      {url ? (
        <Box flex="1" minH="0">
          <iframe
            key={`${destination.qualified_id}-${reloadKey}`}
            ref={frameRef}
            src={url}
            title={destination.title}
            className="plugin-frame"
            // 只给 allow-scripts。不给 allow-same-origin，页面就是 opaque
            // origin，拿不到父窗口的 DOM、cookie 与 localStorage；给了等于
            // 把整个宿主界面交到插件手里。也不给 popups：一个能逃出沙箱的
            // 新窗口，等于这道围栏没设。
            sandbox="allow-scripts"
            onLoad={pushContext}
          />
        </Box>
      ) : (
        <Flex flex="1" align="center" justify="center">
          <Text color="var(--text-faint)">{messages.pluginPageUnavailable}</Text>
        </Flex>
      )}
    </Flex>
  );
}
