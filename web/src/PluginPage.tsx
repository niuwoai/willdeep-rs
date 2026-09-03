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
  pluginComplete,
  pluginProviders,
  readPluginResource,
  writePluginStorage,
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
  value?: string;
};

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
}: Props) {
  const frameRef = useRef<HTMLIFrameElement | null>(null);
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
  useEffect(() => {
    initialized.current = false;
  }, [destination.qualified_id, reloadKey]);

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
      const response = await executePluginCommand(plugin.id, commandId, args);
      applyHostAction(response.action, response.destination);
      return response.kind === "tool" ? response.result : { kind: response.kind };
    },
    [plugin.id, applyHostAction]
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
            post({ __willdeep: 1, type: "commandResult", requestID: data.requestID, result });
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
        case "aiComplete": {
          if (!data.requestID) return;
          try {
            const result =
              data.type === "aiProviders"
                ? await pluginProviders(plugin.id)
                : await pluginComplete(plugin.id, data.request ?? {});
            replyBridge(data.requestID, result);
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
            void writePluginStorage(plugin.id, data.key, data.type === "storageSet" ? data.value ?? "" : null).catch(
              () => undefined
            );
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
  }, [plugin.id, destination.page_server, context, post, runCommand, onSelectItem]);

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
