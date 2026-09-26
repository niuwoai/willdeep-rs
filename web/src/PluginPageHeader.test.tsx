import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { messages } from "./i18n";
import { PluginPageHeader } from "./PluginPageHeader";
import type { PluginCommandView } from "./plugins";

const noop = () => undefined;

function render(props: Partial<Parameters<typeof PluginPageHeader>[0]> = {}) {
  return renderToStaticMarkup(
    <PluginPageHeader
      pluginId="com.willdeep.video-studio"
      icon="sf:film.stack"
      title="短剧工坊"
      commands={[]}
      busyCommand={null}
      messages={messages["zh-CN"]}
      onRunCommand={noop}
      onRefresh={noop}
      onOpenSettings={noop}
      {...props}
    />
  );
}

const command = (overrides: Partial<PluginCommandView>): PluginCommandView => ({
  id: "refresh",
  title: "刷新任务",
  icon: "sf:arrow.clockwise",
  handler: "mcpTool",
  ...overrides,
});

describe("PluginPageHeader", () => {
  it("标题前画目的地图标，与 macOS 标题栏一致", () => {
    const html = render();
    const iconIndex = html.indexOf('data-symbol="film.stack"');
    const titleIndex = html.indexOf("短剧工坊");
    expect(iconIndex).toBeGreaterThan(-1);
    expect(titleIndex).toBeGreaterThan(iconIndex);
    expect(html).toContain('<h1 class="plugin-title" title="短剧工坊">短剧工坊</h1>');
  });

  it("认不出的目的地图标画拼图块兜底", () => {
    const html = render({ icon: "sf:not.a.real.symbol" });
    expect(html).toContain('data-symbol="puzzlepiece.extension"');
    expect(html).toContain('data-fallback="true"');
  });

  it("包内图片图标走插件资源路由", () => {
    const html = render({ icon: "assets/icon.png" });
    expect(html).toContain('src="/plugin-host/com.willdeep.video-studio/assets/icon.png"');
  });

  it("工具栏命令画图标加文字，没声明图标的命令画闪电", () => {
    const html = render({
      commands: [command({}), command({ id: "export", title: "导出", icon: null })],
    });
    expect(html).toContain('aria-label="刷新任务"');
    expect(html).toContain('<span class="plugin-toolbar-command-label">刷新任务</span>');
    expect(html).toContain('<span class="plugin-toolbar-command-label">导出</span>');
    expect(html).toContain('data-symbol="bolt"');
  });

  it("正在执行的命令按钮禁用", () => {
    const html = render({ commands: [command({})], busyCommand: "refresh" });
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*aria-label="刷新任务"|<button[^>]*aria-label="刷新任务"[^>]*disabled=""/);
  });

  it("右侧固定有刷新与「…」菜单按钮，文案走 i18n", () => {
    const zh = render();
    expect(zh).toContain(`aria-label="${messages["zh-CN"].pluginRefresh}"`);
    expect(zh).toContain(`aria-label="${messages["zh-CN"].pluginMoreActions}"`);
    expect(zh).toContain('aria-haspopup="menu"');
    const en = render({ messages: messages.en });
    expect(en).toContain('aria-label="Refresh"');
    expect(en).toContain('aria-label="More actions"');
  });

  it("菜单默认收起", () => {
    expect(render()).not.toContain('role="menu"');
  });
});
