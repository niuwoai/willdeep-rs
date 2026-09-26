import { describe, expect, it } from "vitest";
import {
  COMMAND_FALLBACK_SYMBOL,
  FALLBACK_SYMBOL,
  KNOWN_SYMBOLS,
  pluginIconSource,
  resolveSymbol,
} from "./sfSymbolPaths";

// 已知插件包（Xedit/PluginExamples 与各 willdeep-* 插件仓库）的清单、声明式
// 侧栏里出现过的全部符号名。新插件带来新名字时往这里加，并在字形表里补上。
const KNOWN_PLUGIN_SYMBOLS = [
  "sf:arrow.clockwise",
  "sf:arrow.clockwise.circle",
  "sf:arrow.counterclockwise",
  "sf:arrow.down.circle",
  "sf:arrow.triangle.2.circlepath",
  "sf:arrow.uturn.backward",
  "sf:bookmark",
  "sf:bookmark.fill",
  "sf:chart.bar",
  "sf:checklist",
  "sf:checkmark.circle",
  "sf:checkmark.shield",
  "sf:clock.arrow.circlepath",
  "sf:clock.arrow.trianglehead.counterclockwise.rotate.90",
  "sf:doc.richtext",
  "sf:doc.text",
  "sf:externaldrive",
  "sf:eye",
  "sf:film.stack",
  "sf:folder",
  "sf:gamecontroller",
  "sf:gamecontroller.fill",
  "sf:gearshape",
  "sf:hexagon.fill",
  "sf:list.bullet",
  "sf:list.bullet.rectangle",
  "sf:magnifyingglass",
  "sf:paperclip",
  "sf:paperclip.badge.ellipsis",
  "sf:pencil",
  "sf:photo",
  "sf:play.rectangle",
  "sf:play.rectangle.on.rectangle",
  "sf:plus.circle",
  "sf:square.and.arrow.up",
  "sf:square.and.pencil",
  "sf:square.grid.2x2",
  "sf:square.grid.3x3.fill",
  "sf:text.badge.plus",
  "sf:text.quote",
  "sf:trash",
  "sf:trash.slash",
  "sf:waveform.path.ecg",
  // 声明式侧栏的 systemImage 不带 `sf:` 前缀。
  "circle",
  "circle.lefthalf.filled",
  "folder",
  "folder.fill",
  "hare",
  "square.grid.3x3",
  "tortoise",
];

// 宿主自己画的那几个：入口栏、标题栏、「…」菜单与两种兜底。
const HOST_SYMBOLS = [
  "sf:text.badge.plus",
  "sf:gearshape",
  "sf:ellipsis",
  "sf:ellipsis.circle",
  "sf:plus.circle",
  "sf:x.mark",
  `sf:${FALLBACK_SYMBOL}`,
  `sf:${COMMAND_FALLBACK_SYMBOL}`,
];

describe("resolveSymbol", () => {
  it.each(KNOWN_PLUGIN_SYMBOLS)("已知插件用到的 %s 有专属字形，不走兜底", (name) => {
    const glyph = resolveSymbol(name);
    expect(glyph.fallback).toBe(false);
    expect(glyph.symbol).toBe(name.replace(/^sf:/, ""));
  });

  it.each(HOST_SYMBOLS)("宿主自用的 %s 有专属字形", (name) => {
    expect(resolveSymbol(name).fallback).toBe(false);
  });

  it("每个字形都至少画了一层", () => {
    for (const symbol of KNOWN_SYMBOLS) {
      const glyph = resolveSymbol(symbol);
      expect(glyph.stroke || glyph.fill, symbol).toBeTruthy();
    }
  });

  it("sf: 前缀与裸名字画同一个字形", () => {
    expect(resolveSymbol("sf:folder")).toEqual(resolveSymbol("folder"));
  });

  it("没画过的 .fill 变体降级到线性版本", () => {
    const glyph = resolveSymbol("sf:photo.fill");
    expect(glyph).toMatchObject({ symbol: "photo", fallback: false });
  });

  it("带修饰的名字逐段降级到最近的已知符号", () => {
    expect(resolveSymbol("sf:doc.text.magnifyingglass").symbol).toBe("doc.text");
    expect(resolveSymbol("sf:folder.badge.plus").symbol).toBe("folder");
    expect(resolveSymbol("sf:trash.circle.fill").symbol).toBe("trash");
  });

  it("完全认不出的名字画拼图块，而不是圆点或空白", () => {
    const glyph = resolveSymbol("sf:totally.unknown.symbol");
    expect(glyph).toMatchObject({ symbol: FALLBACK_SYMBOL, fallback: true });
    expect(glyph.stroke.length).toBeGreaterThan(0);
  });

  it("空名字与 null 也走兜底", () => {
    expect(resolveSymbol(null)).toMatchObject({ symbol: FALLBACK_SYMBOL, fallback: true });
    expect(resolveSymbol("sf:")).toMatchObject({ symbol: FALLBACK_SYMBOL, fallback: true });
  });

  it("命令没图标时按调用方给的兜底画闪电", () => {
    expect(resolveSymbol(null, COMMAND_FALLBACK_SYMBOL)).toMatchObject({ symbol: "bolt", fallback: true });
  });

  it("兜底名本身认不出时仍落到拼图块", () => {
    expect(resolveSymbol(null, "nope.nope").symbol).toBe(FALLBACK_SYMBOL);
  });

  it("半实心与实心点的字形带实心层", () => {
    expect(resolveSymbol("circle.lefthalf.filled").fill).toBeTruthy();
    expect(resolveSymbol("sf:ellipsis").fill).toBeTruthy();
    expect(resolveSymbol("sf:trash").fill).toBeNull();
  });
});

describe("pluginIconSource", () => {
  it("sf: 图标按符号画", () => {
    expect(pluginIconSource("demo", "sf:film.stack")).toEqual({ kind: "symbol", name: "sf:film.stack" });
  });

  it("没有图标时交给符号兜底", () => {
    expect(pluginIconSource("demo", null)).toEqual({ kind: "symbol", name: null });
    expect(pluginIconSource("demo", "  ")).toEqual({ kind: "symbol", name: null });
  });

  it("包内相对路径的图片经插件资源路由取", () => {
    expect(pluginIconSource("video studio", "assets/icon 1.png")).toEqual({
      kind: "image",
      url: "/plugin-host/video%20studio/assets/icon%201.png",
    });
    expect(pluginIconSource("demo", "icon.svg")).toEqual({ kind: "image", url: "/plugin-host/demo/icon.svg" });
  });

  it.each(["../escape.png", "/etc/icon.png", "https://example.com/icon.png", "assets//icon.png", "data:image/png;base64,AAAA.png"])(
    "不是包内相对路径的 %s 不当图片取",
    (icon) => {
      expect(pluginIconSource("demo", icon).kind).toBe("symbol");
    }
  );

  it("没有图片扩展名的裸名字当符号名", () => {
    expect(pluginIconSource("demo", "folder")).toEqual({ kind: "symbol", name: "folder" });
  });
});
