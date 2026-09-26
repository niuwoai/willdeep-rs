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
import { PluginPageHeader } from "./PluginPageHeader";
import { pluginTheme } from "./pluginTheme";
import type { ColorScheme } from "./theme";
import {
  callPluginTool,
  executePluginCommand,
  filePickerRejection,
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
  /** 宿主此刻生效的配色；插件页面的 colorScheme 上下文与主题变量都跟着它。 */
  colorScheme: ColorScheme;
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

/** MCP Apps 的显示模式。插件页面占满中央区域，没有 inline / pip 可切。 */
const MCP_DISPLAY_MODE = "fullscreen";

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
 * 文件上传到本插件隔离的目录，再把落地的服务端路径交给宿主。
 * `accept` 由宿主按命令给出（参照图是图片，背景音乐是音频）。
 */
function chooseLocalFile(accept: string): Promise<File | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
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

export function PluginPage({
  plugin,
  destination,
  messages,
  locale,
  colorScheme,
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
  // 重新载入时给一条细进度条：iframe 换新的那一下旧内容已经没了，
  // 不给反馈的话用户分不清是点了没反应还是正在加载。
  // 用「已加载完的是哪一帧」而不是一个布尔：换目的地、手动刷新都会换
  // iframe 的 key，key 对不上就是还在加载，不需要在各处记得把它置回 true。
  const [loadedFrameKey, setLoadedFrameKey] = useState<string | null>(null);
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
      colorScheme,
    };
  }, [plugin.permissions, destination.qualified_id, selectedItemId, workspace, sessionId, locale, colorScheme]);

  const post = useCallback((payload: unknown) => {
    frameRef.current?.contentWindow?.postMessage(payload, "*");
  }, []);

  // 与 macOS 宿主的 pushContext 同一份载荷：window.__WILLDEEP_CONTEXT__ +
  // willdeep:context-changed 由页面里的桥落地；MCP App 另外收一条
  // host-context-changed，顶层带 theme / locale，willdeep 下是完整上下文。
  //
  // 主题变量在推送时现取：这个回调跑在 effect 里，根元素的 data-theme
  // 已经换好，读到的是新配色。
  const pushContext = useCallback(() => {
    post({ __willdeep: 1, type: "context", context });
    post({ __willdeep: 1, type: "theme", theme: pluginTheme(colorScheme) });
    if (initialized.current) {
      post({
        __willdeep: 1,
        type: "mcpMessage",
        message: {
          jsonrpc: "2.0",
          method: "ui/notifications/host-context-changed",
          params: { theme: context.colorScheme, locale: context.locale, willdeep: context },
        },
      });
    }
  }, [context, colorScheme, post]);

  const reload = useCallback(() => {
    setReloadKey((current) => current + 1);
  }, []);

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
          reload();
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
    [onNavigate, onOpenPluginCenter, reload]
  );

  const runCommand = useCallback(
    async (commandId: string, args: unknown) => {
      // 「选文件」类命令在这里改道：先让用户在浏览器里挑，再把上传后的
      // 服务端路径作为 `path` 交给宿主（宿主合成结果或转给原工具）。不改道
      // 的话请求会进 MCP 服务，那边去弹一个没人看得见的原生框，然后超时。
      let payload = args;
      const picker = plugin.file_pickers.find((entry) => entry.command === commandId);
      if (picker) {
        const file = await chooseLocalFile(picker.accept);
        if (!file) throw new Error("selection_cancelled");
        const rejection = filePickerRejection(file, picker);
        if (rejection) throw new Error(rejection);
        const uploaded = await uploadPluginFile(plugin.id, commandId, file);
        payload = { ...(args as Record<string, unknown> | null), path: uploaded.path };
      }
      const response = await executePluginCommand(plugin.id, commandId, payload);
      applyHostAction(response.action, response.destination);
      return response.kind === "tool" ? response.result : { kind: response.kind };
    },
    [plugin.id, plugin.file_pickers, applyHostAction]
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
          reload();
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
          // 重新握手就是重新开始：initialized 之前的 tools/call 一律按未就绪拒。
          initialized.current = false;
          // 能力名与 hostContext 的形状对齐 macOS 宿主（也就是 MCP Apps 规范
          // 里的 serverTools / serverResources），同一个 MCP App 在两端读到
          // 同样的字段。
          replyMcp(data.id, {
            protocolVersion: "2026-01-26",
            hostCapabilities: { serverTools: {}, serverResources: {} },
            hostInfo: { name: "willdeep-web", version: "1" },
            hostContext: {
              theme: context.colorScheme,
              locale: context.locale,
              displayMode: MCP_DISPLAY_MODE,
              availableDisplayModes: [MCP_DISPLAY_MODE],
              willdeep: context,
            },
          });
          break;
        case "ui/notifications/initialized":
          initialized.current = true;
          // 与 macOS 一致：握手一完成就补推一次上下文，页面不必等下一次变化。
          pushContext();
          break;
        case "ping":
          replyMcp(data.id, {});
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
    pushContext,
    reload,
    runCommand,
    onSelectItem,
    onChatText,
    onOpenSession,
    messages.pluginRunCommandConfirm,
  ]);

  const frameKey = `${destination.qualified_id}-${reloadKey}`;
  const frameLoading = url !== null && loadedFrameKey !== frameKey;

  return (
    <Flex direction="column" flex="1" minW="0" h="100vh" bg="var(--bg-page)">
      <PluginPageHeader
        pluginId={plugin.id}
        icon={destination.icon}
        title={destination.title}
        commands={destination.toolbar_commands}
        busyCommand={busyCommand}
        messages={messages}
        onRunCommand={async (commandId) => {
          setBusyCommand(commandId);
          setError(null);
          try {
            await runCommand(commandId, {});
          } catch (reason) {
            setError(reason instanceof Error ? reason.message : String(reason));
          } finally {
            setBusyCommand(null);
          }
        }}
        onRefresh={reload}
        onOpenSettings={onOpenPluginCenter}
      />
      {error && (
        <Text className="plugin-error" role="alert">
          {messages.pluginCommandFailed}: {error}
        </Text>
      )}
      {url ? (
        <Box flex="1" minH="0" position="relative">
          {frameLoading && <Box className="plugin-reload-bar" role="progressbar" aria-label={messages.pluginSidebarLoading} />}
          <iframe
            key={frameKey}
            ref={frameRef}
            src={url}
            title={destination.title}
            className="plugin-frame"
            // 只给 allow-scripts。不给 allow-same-origin，页面就是 opaque
            // origin，拿不到父窗口的 DOM、cookie 与 localStorage；给了等于
            // 把整个宿主界面交到插件手里。也不给 popups：一个能逃出沙箱的
            // 新窗口，等于这道围栏没设。
            sandbox="allow-scripts"
            onLoad={() => {
              setLoadedFrameKey(frameKey);
              pushContext();
            }}
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
