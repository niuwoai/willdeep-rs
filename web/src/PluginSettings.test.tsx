import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { PluginSettings } from "./PluginSettings";
import { messages } from "./i18n";
import type { PluginView } from "./plugins";

describe("PluginSettings", () => {
  it.each(["zh-CN", "en", "ja"] as const)("%s 显示保存按钮和编辑提示，已有密钥不回显", (language) => {
    const plugin = {
      id: "test",
      settings: [{ id: "key", title: "API Key", type: "secret", value: "never-render-this", configured: true, default_value: null, description: null, options: [] }],
    } as unknown as PluginView;
    const html = renderToStaticMarkup(<PluginSettings plugin={plugin} messages={messages[language]} onChanged={() => undefined} />);
    expect(html).toContain(messages[language].pluginSettingsSave);
    expect(html).toContain(messages[language].pluginSettingsHint);
    expect(html).toContain(messages[language].pluginSecretStored);
    expect(html).toContain('type="password"');
    expect(html).not.toContain("never-render-this");
    expect(html).toMatch(/<button[^>]*disabled=""/);
  });
});
