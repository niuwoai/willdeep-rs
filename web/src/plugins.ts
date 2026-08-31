// 插件宿主的前端契约。
//
// 后端 `crates/willdeep-cli/src/plugin_web.rs` 是这些形状的唯一来源；这里的
// 类型是它的镜像，字段名保持 snake_case，不做一层没必要的改名映射。

export type PluginSidebarView = { id: string; mode: "sessionList" | "declarative" | "none" };

export type PluginCommandView = {
  id: string;
  title: string;
  icon: string | null;
  handler: "host" | "mcpTool" | "navigate";
};

export type PluginDestinationView = {
  id: string;
  qualified_id: string;
  title: string;
  icon: string | null;
  main_page: string;
  page_runtime: "localWeb" | "mcpApp" | "declarative" | "unknown";
  page_url: string | null;
  /** mcpApp 页面所属的 MCP 服务；页面自报的服务名一律不认，只用这一个。 */
  page_server: string | null;
  sidebar: PluginSidebarView | null;
  toolbar_commands: PluginCommandView[];
  default_pinned: boolean;
  pinned_order: number | null;
};

export type PluginSettingView = {
  id: string;
  type: "string" | "number" | "boolean" | "enum" | "secret";
  title: string;
  description: string | null;
  default_value: string | null;
  options: string[];
  value: string | null;
  configured: boolean;
};

export type ApprovalGapView = { reason: string; detail: string | null };

export type PluginView = {
  id: string;
  name: string;
  version: string;
  description: string | null;
  source: string;
  enabled: boolean;
  approval_gap: ApprovalGapView | null;
  permissions: string[];
  inferred_permissions: string[];
  mcp_servers: string[];
  destinations: PluginDestinationView[];
  commands: PluginCommandView[];
  menus: Record<string, string[]>;
  settings: PluginSettingView[];
  /** 从没批准过的包没有这一项——算它要读遍包内容，那是「点批准」时才做的事。 */
  digest?: string;
};

export type PluginFailureView = { path: string; reason: string };
export type PluginsResponse = { plugins: PluginView[]; failures: PluginFailureView[] };

export type DeclarativeComponent = {
  id: string;
  kind: string;
  titleKey?: string;
  subtitleKey?: string;
  systemImage?: string;
  value?: string;
  progress?: number;
  command?: string;
  contextCommands?: string[];
  arguments?: Record<string, string>;
  children?: DeclarativeComponent[];
};

export type SidebarDocument = {
  document: { schemaVersion: number; components: DeclarativeComponent[] };
  degraded: string | null;
  strings: Record<string, string>;
};

export type CommandResponse = {
  kind: "host" | "navigate" | "tool";
  action?: string;
  destination?: string;
  result?: unknown;
};

/** 页面上下文，字段名与 macOS 宿主的 AgentPluginDestinationContext 一致。 */
export type DestinationContext = {
  destinationID: string;
  selectedItemID: string | null;
  workspaceReference: string | null;
  sessionReference: string | null;
  locale: string;
  colorScheme: string;
};

async function request<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, init);
  const text = await response.text();
  const value = text ? JSON.parse(text) : {};
  if (!response.ok) throw new Error(value.error ?? response.statusText);
  return value as T;
}

function jsonBody(body: unknown): RequestInit {
  return { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) };
}

export function fetchPlugins(locale: string) {
  return request<PluginsResponse>(`/api/plugins?locale=${encodeURIComponent(locale)}`);
}

export function approvePlugin(pluginId: string) {
  return request(`/api/plugins/${encodeURIComponent(pluginId)}/approve`, { method: "POST" });
}

export function setPluginEnabled(pluginId: string, enabled: boolean) {
  return request<{ enabled: boolean; approval_gap: ApprovalGapView | null }>(
    `/api/plugins/${encodeURIComponent(pluginId)}/enabled`,
    jsonBody({ enabled })
  );
}

export function setPluginSetting(pluginId: string, key: string, value: string | null) {
  return request(
    `/api/plugins/${encodeURIComponent(pluginId)}/settings/${encodeURIComponent(key)}`,
    jsonBody({ value })
  );
}

export function uninstallPlugin(pluginId: string) {
  return request(`/api/plugins/${encodeURIComponent(pluginId)}`, { method: "DELETE" });
}

export function fetchSidebar(pluginId: string, sidebarId: string, locale: string) {
  return request<SidebarDocument>(
    `/api/plugins/${encodeURIComponent(pluginId)}/sidebars/${encodeURIComponent(sidebarId)}?locale=${encodeURIComponent(locale)}`
  );
}

export function executePluginCommand(pluginId: string, commandId: string, args: unknown) {
  return request<CommandResponse>(
    `/api/plugins/${encodeURIComponent(pluginId)}/commands/${encodeURIComponent(commandId)}`,
    jsonBody({ arguments: args ?? {} })
  );
}

export function callPluginTool(pluginId: string, server: string, tool: string, args: unknown) {
  return request<unknown>(
    `/api/plugins/${encodeURIComponent(pluginId)}/mcp/call`,
    jsonBody({ server, tool, arguments: args ?? {} })
  );
}

export function readPluginResource(pluginId: string, server: string, uri: string) {
  return request<unknown>(
    `/api/plugins/${encodeURIComponent(pluginId)}/mcp/resource`,
    jsonBody({ server, uri })
  );
}

export function pluginProviders(pluginId: string) {
  return request<{ providers: unknown[] }>(`/api/plugins/${encodeURIComponent(pluginId)}/ai/providers`);
}

export function pluginComplete(pluginId: string, payload: unknown) {
  return request<{ text: string; model: string; providerID: string }>(
    `/api/plugins/${encodeURIComponent(pluginId)}/ai/complete`,
    jsonBody(payload)
  );
}

export function writePluginStorage(pluginId: string, key: string, value: string | null) {
  return request(`/api/plugins/${encodeURIComponent(pluginId)}/storage`, jsonBody({ key, value }));
}

export function clearPluginStorage(pluginId: string) {
  return request(`/api/plugins/${encodeURIComponent(pluginId)}/storage`, { method: "DELETE" });
}

/** localWeb 页面的 iframe 地址；mcpApp / declarative 页面由宿主渲染文档。 */
export function pageUrl(plugin: PluginView, destination: PluginDestinationView): string | null {
  if (destination.page_runtime === "localWeb") return destination.page_url;
  if (destination.page_runtime === "mcpApp") {
    return `/plugin-page/${encodeURIComponent(plugin.id)}/${encodeURIComponent(destination.main_page)}`;
  }
  return null;
}

/**
 * 目的地排序：声明了 defaultPinned 的排在前面，其余按用户钉的顺序，
 * 最后按标题。与 macOS 宿主一样最多固定 5 个，超出的进「更多插件」。
 */
export const MAX_PINNED_DESTINATIONS = 5;

export function orderedDestinations(plugins: PluginView[]): Array<{ plugin: PluginView; destination: PluginDestinationView }> {
  const items = plugins
    .filter((plugin) => plugin.enabled)
    .flatMap((plugin) => plugin.destinations.map((destination) => ({ plugin, destination })));
  return items.sort((left, right) => {
    if (left.destination.default_pinned !== right.destination.default_pinned) {
      return left.destination.default_pinned ? -1 : 1;
    }
    const leftOrder = left.destination.pinned_order ?? Number.MAX_SAFE_INTEGER;
    const rightOrder = right.destination.pinned_order ?? Number.MAX_SAFE_INTEGER;
    if (leftOrder !== rightOrder) return leftOrder - rightOrder;
    return left.destination.title.localeCompare(right.destination.title);
  });
}
