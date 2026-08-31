// 插件中心：装了什么、要什么权限、批没批准、开没开。
//
// 界面上最重要的一件事是**让权限在点头之前看得见**。所以未批准的插件不是一个
// 灰掉的开关，而是一段说清"它要什么、来自哪里、内容指纹是多少"的文字，
// 加一个明确的批准动作。批准与启用分成两步也是有意的：批准是对内容的判断，
// 启用是对此刻要不要跑的判断。

import { useState } from "react";
import { Box, Button, Flex, Heading, Text } from "@chakra-ui/react";
import type { Messages } from "./i18n";
import {
  approvePlugin,
  setPluginEnabled,
  setPluginSetting,
  uninstallPlugin,
  type PluginFailureView,
  type PluginView,
} from "./plugins";

type Props = {
  plugins: PluginView[];
  failures: PluginFailureView[];
  messages: Messages;
  onChanged: () => void;
};

function permissionLabel(messages: Messages, permission: string): string {
  const table: Record<string, string> = {
    "conversation.read": messages.pluginPermConversationRead,
    "workspace.read": messages.pluginPermWorkspaceRead,
    "workspace.write": messages.pluginPermWorkspaceWrite,
    "process.execute": messages.pluginPermProcessExecute,
    "network.access": messages.pluginPermNetworkAccess,
    "credentials.use": messages.pluginPermCredentialsUse,
    "ai.chat": messages.pluginPermAiChat,
    "providers.read": messages.pluginPermProvidersRead,
    "clipboard.write": messages.pluginPermClipboardWrite,
    notifications: messages.pluginPermNotifications,
  };
  return table[permission] ?? permission;
}

function gapLabel(messages: Messages, reason: string): string {
  const table: Record<string, string> = {
    "never-approved": messages.pluginGapNeverApproved,
    "version-changed": messages.pluginGapVersionChanged,
    "digest-changed": messages.pluginGapDigestChanged,
    "source-changed": messages.pluginGapSourceChanged,
    "new-permissions": messages.pluginGapNewPermissions,
  };
  return table[reason] ?? reason;
}

export function PluginCenter({ plugins, failures, messages, onChanged }: Props) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null);

  const act = async (pluginId: string, work: () => Promise<unknown>) => {
    setBusy(pluginId);
    setError(null);
    try {
      await work();
      onChanged();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(null);
    }
  };

  return (
    <Box className="plugin-center">
      <Heading size="lg" mb="1">
        {messages.pluginCenter}
      </Heading>
      <Text className="plugin-center-hint">{messages.pluginCenterHint}</Text>
      {error && (
        <Text className="plugin-error" role="alert">
          {error}
        </Text>
      )}

      {plugins.length === 0 && (
        <Box className="plugin-card">
          <Text color="#8b99aa">{messages.pluginNoneInstalled}</Text>
          <code className="plugin-code">willdeep plugin import</code>
        </Box>
      )}

      {plugins.map((plugin) => {
        const permissions = plugin.permissions.length ? plugin.permissions : plugin.inferred_permissions;
        const isBusy = busy === plugin.id;
        return (
          <Box key={plugin.id} className="plugin-card">
            <Flex justify="space-between" align="flex-start" gap="3" wrap="wrap">
              <Box flex="1" minW="240px">
                <Flex align="baseline" gap="2">
                  <Text className="plugin-card-name">{plugin.name}</Text>
                  <Text className="plugin-card-version">{plugin.version}</Text>
                  <Text className="plugin-card-source">{plugin.source}</Text>
                </Flex>
                {plugin.description && <Text className="plugin-card-about">{plugin.description}</Text>}
                <Flex className="plugin-chip-row" wrap="wrap">
                  {permissions.length === 0 && <span className="plugin-chip quiet">{messages.pluginPermNone}</span>}
                  {permissions.map((permission) => (
                    <span key={permission} className="plugin-chip">
                      {permissionLabel(messages, permission)}
                    </span>
                  ))}
                  {plugin.mcp_servers.map((server) => (
                    <span key={server} className="plugin-chip mcp">
                      MCP · {server}
                    </span>
                  ))}
                </Flex>
                {plugin.destinations.length > 0 && (
                  <Text className="plugin-card-meta">
                    {messages.pluginContributes}: {plugin.destinations.map((item) => item.title).join(" · ")}
                  </Text>
                )}
                {plugin.digest && (
                  <Text className="plugin-card-digest" title={plugin.digest}>
                    {plugin.digest}
                  </Text>
                )}
              </Box>

              <Flex direction="column" gap="2" align="stretch" minW="180px">
                {plugin.approval_gap ? (
                  <>
                    <Text className="plugin-gap">
                      {gapLabel(messages, plugin.approval_gap.reason)}
                      {plugin.approval_gap.detail ? ` · ${plugin.approval_gap.detail}` : ""}
                    </Text>
                    <Button
                      size="sm"
                      className="plugin-primary-button"
                      disabled={isBusy}
                      onClick={() => void act(plugin.id, () => approvePlugin(plugin.id))}
                    >
                      {messages.pluginApprove}
                    </Button>
                  </>
                ) : (
                  <Button
                    size="sm"
                    variant="outline"
                    borderColor="#3a4859"
                    color="#d8e2ec"
                    disabled={isBusy}
                    onClick={() => void act(plugin.id, () => setPluginEnabled(plugin.id, !plugin.enabled))}
                  >
                    {plugin.enabled ? messages.pluginDisable : messages.pluginEnable}
                  </Button>
                )}
                {plugin.source === "shared" &&
                  (confirmRemove === plugin.id ? (
                    <Flex gap="1">
                      <Button size="xs" variant="ghost" onClick={() => setConfirmRemove(null)}>
                        {messages.cancel}
                      </Button>
                      <Button
                        size="xs"
                        colorPalette="red"
                        disabled={isBusy}
                        onClick={() =>
                          void act(plugin.id, async () => {
                            await uninstallPlugin(plugin.id);
                            setConfirmRemove(null);
                          })
                        }
                      >
                        {messages.pluginRemoveConfirm}
                      </Button>
                    </Flex>
                  ) : (
                    <Button size="xs" variant="ghost" color="#8b99aa" onClick={() => setConfirmRemove(plugin.id)}>
                      {messages.pluginRemove}
                    </Button>
                  ))}
              </Flex>
            </Flex>

            {plugin.settings.length > 0 && (
              <Box className="plugin-settings">
                {plugin.settings.map((setting) => (
                  <Flex key={setting.id} className="plugin-setting-row" gap="2" align="center">
                    <Box flex="1" minW="0">
                      <Text className="plugin-setting-title">{setting.title}</Text>
                      {setting.description && <Text className="plugin-setting-about">{setting.description}</Text>}
                    </Box>
                    {setting.type === "boolean" ? (
                      <input
                        type="checkbox"
                        aria-label={setting.title}
                        checked={(setting.value ?? setting.default_value) === "true"}
                        onChange={(event) =>
                          void act(plugin.id, () =>
                            setPluginSetting(plugin.id, setting.id, String(event.target.checked))
                          )
                        }
                      />
                    ) : (
                      <input
                        className="plugin-setting-input"
                        aria-label={setting.title}
                        // secret 只显示"设过没有"，不回显值：一个能读回来的
                        // 密钥等于没存过。
                        type={setting.type === "secret" ? "password" : "text"}
                        placeholder={
                          setting.type === "secret" && setting.configured
                            ? messages.pluginSecretStored
                            : setting.default_value ?? ""
                        }
                        defaultValue={setting.type === "secret" ? "" : setting.value ?? ""}
                        onBlur={(event) => {
                          const value = event.target.value;
                          if (setting.type === "secret" && value === "") return;
                          void act(plugin.id, () => setPluginSetting(plugin.id, setting.id, value || null));
                        }}
                      />
                    )}
                  </Flex>
                ))}
              </Box>
            )}
          </Box>
        );
      })}

      {failures.length > 0 && (
        <Box className="plugin-card">
          <Text className="plugin-card-name">{messages.pluginLoadFailures}</Text>
          {failures.map((failure) => (
            <Text key={`${failure.path}-${failure.reason}`} className="plugin-card-about">
              {failure.path} — {failure.reason}
            </Text>
          ))}
        </Box>
      )}
    </Box>
  );
}
