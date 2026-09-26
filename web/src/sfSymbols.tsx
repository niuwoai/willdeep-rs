// 图标组件。字形表与降级规则在 `sfSymbolPaths.ts`，这里只负责画。

import { useState } from "react";
import { COMMAND_FALLBACK_SYMBOL, FALLBACK_SYMBOL, pluginIconSource, resolveSymbol } from "./sfSymbolPaths";

type SfIconProps = {
  name?: string | null;
  size?: number;
  /** 认不出 `name` 时画哪个符号；缺省是拼图块。 */
  fallback?: string;
};

export function SfIcon({ name, size = 18, fallback = FALLBACK_SYMBOL }: SfIconProps) {
  const glyph = resolveSymbol(name, fallback);
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
      data-symbol={glyph.symbol}
      data-fallback={glyph.fallback ? "true" : undefined}
      style={{ flex: "none" }}
    >
      {glyph.stroke && <path d={glyph.stroke} />}
      {glyph.fill && <path d={glyph.fill} fill="currentColor" stroke="none" />}
    </svg>
  );
}

type PluginIconProps = {
  pluginId: string;
  icon?: string | null;
  size?: number;
  /** 命令图标缺省画闪电（macOS 工具栏同款），目的地缺省画拼图块。 */
  kind?: "destination" | "command";
};

/**
 * 插件清单里的图标：`sf:` 符号或包内图片。图片读不出来（被删、格式坏、插件
 * 刚停用）时退回符号兜底，入口不会变成一块空白。
 */
export function PluginIcon({ pluginId, icon, size = 18, kind = "destination" }: PluginIconProps) {
  const source = pluginIconSource(pluginId, icon);
  const [brokenUrl, setBrokenUrl] = useState<string | null>(null);
  const fallback = kind === "command" ? COMMAND_FALLBACK_SYMBOL : FALLBACK_SYMBOL;
  if (source.kind === "image" && brokenUrl !== source.url) {
    return (
      <img
        src={source.url}
        width={size}
        height={size}
        alt=""
        aria-hidden="true"
        draggable={false}
        style={{ flex: "none", objectFit: "contain" }}
        onError={() => setBrokenUrl(source.url)}
      />
    );
  }
  return <SfIcon name={source.kind === "symbol" ? source.name : null} size={size} fallback={fallback} />;
}
