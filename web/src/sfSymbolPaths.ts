// SF Symbols 在浏览器里不存在，插件清单里的 `sf:` 图标名需要一份等价的
// 线性图标。这里用 Heroicons 风格的 24×24 stroke 路径手绘对应项——形状对得上
// 语义即可，不追求像素级复刻 Apple 的字形。
//
// 覆盖面以「已知插件包实际用到的名字」为准（PluginExamples 与各 willdeep-*
// 插件仓库的清单、声明式侧栏、以及宿主自己用到的几个）。新插件用了这里没有的
// 名字时按下面的顺序降级，而不是画一个没有语义的圆点：
//
//   1. 精确命中；
//   2. 去掉 `.fill` 修饰再找（`folder.fill` → `folder`）；
//   3. 从右往左逐段去修饰（`arrow.clockwise.circle` → `arrow.clockwise`，
//      `doc.text.magnifyingglass` → `doc.text` → `doc`）；
//   4. 都不中就用拼图块——与 macOS 宿主 `AgentPluginIcon` 的兜底是同一个符号，
//      意思是「这里是个插件入口，只是图标没认出来」。

/** 认不出来时的兜底符号，与 macOS 宿主一致。 */
export const FALLBACK_SYMBOL = "puzzlepiece.extension";

/** 命令没有声明图标时的兜底，与 macOS 宿主工具栏一致。 */
export const COMMAND_FALLBACK_SYMBOL = "bolt";

const CIRCLE = "M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z";
const HISTORY = "M3.5 12a8.5 8.5 0 1 0 2.5-6M3.5 4.5V9H8M12 8v4l3 2";
const DOC = "M6.5 3h7.5l4.5 4.5v12A1.5 1.5 0 0 1 17 21H6.5A1.5 1.5 0 0 1 5 19.5v-15A1.5 1.5 0 0 1 6.5 3ZM14 3v4.5h4.5";
const FOLDER =
  "M3.5 7a1.5 1.5 0 0 1 1.5-1.5h4.2l2 2.2H19a1.5 1.5 0 0 1 1.5 1.5v8.3a1.5 1.5 0 0 1-1.5 1.5H5a1.5 1.5 0 0 1-1.5-1.5V7Z";
const GAMECONTROLLER =
  "M7.5 10.5v3M6 12h3M15.5 11.5h.01M17.5 13.5h.01M6.8 7.5h10.4a4.8 4.8 0 0 1 4.6 6.1l-.7 2.6a2.6 2.6 0 0 1-4.6.9L15 15H9l-1.5 2.1a2.6 2.6 0 0 1-4.6-.9l-.7-2.6a4.8 4.8 0 0 1 4.6-6.1Z";
const HEXAGON = "M12 3l7.8 4.5v9L12 21l-7.8-4.5v-9L12 3Z";
const SQUARE_GRID_3X3_FILL =
  "M3.5 3.5h5v5h-5v-5Zm6 0h5v5h-5v-5Zm6 0h5v5h-5v-5Zm-12 6h5v5h-5v-5Zm6 0h5v5h-5v-5Zm6 0h5v5h-5v-5Zm-12 6h5v5h-5v-5Zm6 0h5v5h-5v-5Zm6 0h5v5h-5v-5Z";
const THREE_DOTS =
  "M6 13.4a1.4 1.4 0 1 0 0-2.8 1.4 1.4 0 0 0 0 2.8ZM12 13.4a1.4 1.4 0 1 0 0-2.8 1.4 1.4 0 0 0 0 2.8ZM18 13.4a1.4 1.4 0 1 0 0-2.8 1.4 1.4 0 0 0 0 2.8Z";

/** 线性主体：stroke 画出来的那一层。 */
const STROKES: Record<string, string> = {
  // ------------------------------------------------------------ 箭头 / 刷新
  "arrow.clockwise": "M4.5 12a7.5 7.5 0 1 1 2.2 5.3M4.5 12V7.5M4.5 12H9",
  "arrow.clockwise.circle": `${CIRCLE}M8 12a4 4 0 1 0 1.2-2.8M9.2 6.8v2.4h2.4`,
  "arrow.counterclockwise": "M19.5 12a7.5 7.5 0 1 0-2.2 5.3M19.5 12V7.5M19.5 12H15",
  "arrow.down.circle": `${CIRCLE}M12 7.5v9M8.5 13l3.5 3.5 3.5-3.5`,
  "arrow.triangle.2.circlepath":
    "M4.5 10.5a7.5 7.5 0 0 1 13.4-3.3M19.5 13.5a7.5 7.5 0 0 1-13.4 3.3M18 3.5v4h-4M6 20.5v-4h4",
  "arrow.uturn.backward": "M9 14 4 9l5-5M4 9h9a6 6 0 0 1 0 12h-3",
  // 时钟加逆时针箭头：「历史」。两个名字画同一个形状，macOS 上它们也几乎一样。
  "clock.arrow.circlepath": HISTORY,
  "clock.arrow.trianglehead.counterclockwise.rotate.90": HISTORY,
  clock: `${CIRCLE}M12 7v5l3 2`,

  // ------------------------------------------------------------ 书签 / 文档
  bookmark: "M6 4.5h12v15l-6-4-6 4v-15Z",
  "bookmark.fill": "M6 4.5h12v15l-6-4-6 4v-15Z",
  doc: DOC,
  "doc.text": `${DOC}M8.5 12h7M8.5 15.5h7`,
  "doc.richtext": `${DOC}M8 17.5l2.5-3 2 2 1.5-1.5 2 2.5M9.5 11h.01`,
  externaldrive:
    "M3.5 13.5l2.3-7.3A1.5 1.5 0 0 1 7.2 5h9.6a1.5 1.5 0 0 1 1.4 1.2l2.3 7.3M3.5 13.5h17v4a1.5 1.5 0 0 1-1.5 1.5H5a1.5 1.5 0 0 1-1.5-1.5v-4ZM16.5 16.2h.01",
  folder: FOLDER,
  "folder.fill": FOLDER,
  paperclip:
    "M20 11.5l-7.8 7.8a5 5 0 0 1-7.1-7.1l8.1-8.1a3.3 3.3 0 0 1 4.7 4.7l-8.1 8.1a1.7 1.7 0 0 1-2.4-2.4l7.5-7.5",
  "paperclip.badge.ellipsis":
    "M16 9.5l-6.2 6.2a3.6 3.6 0 0 1-5.1-5.1l6.6-6.6a2.4 2.4 0 0 1 3.4 3.4l-6.6 6.6a1.2 1.2 0 0 1-1.7-1.7l5.9-5.9",
  photo:
    "M5.5 4.5h13A1.5 1.5 0 0 1 20 6v12a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 18V6a1.5 1.5 0 0 1 1.5-1.5ZM4 15l4.5-4.5L13 15l2.5-2.5L20 17M15.5 8.5h.01",
  "text.quote": "M9 6.5h11.5M9 12h11.5M9 17.5h7M4.5 5v14",

  // ------------------------------------------------------------ 列表 / 网格
  "chart.bar": "M4 20V10M10 20V4M16 20v-7M22 20H2",
  checklist: "M9 6h11M9 12h11M9 18h11M3.5 6l1.2 1.2L7 5M3.5 12l1.2 1.2L7 11M3.5 18l1.2 1.2L7 17",
  "list.bullet": "M8.5 6h12M8.5 12h12M8.5 18h12M3.8 6h.01M3.8 12h.01M3.8 18h.01",
  "list.bullet.rectangle":
    "M5 4.5h14a1.5 1.5 0 0 1 1.5 1.5v12a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 18V6A1.5 1.5 0 0 1 5 4.5ZM10 9h6.5M10 12h6.5M10 15h6.5M7.2 9h.01M7.2 12h.01M7.2 15h.01",
  "square.grid.2x2": "M4 4h7v7H4V4Zm9 0h7v7h-7V4ZM4 13h7v7H4v-7Zm9 0h7v7h-7v-7Z",
  "square.grid.2x2.fill": "M4 4h7v7H4V4Zm9 0h7v7h-7V4ZM4 13h7v7H4v-7Zm9 0h7v7h-7v-7Z",
  "square.grid.3x3": "M4 4h16v16H4V4ZM9.3 4v16M14.7 4v16M4 9.3h16M4 14.7h16",
  "square.grid.3x3.fill": SQUARE_GRID_3X3_FILL,
  "text.badge.plus": "M3.5 6h11M3.5 12h8M3.5 18h8M18 8.5v7M14.5 12h7",

  // ------------------------------------------------------------ 媒体
  "film.stack":
    "M4.5 8h15a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1h-15a1 1 0 0 1-1-1V9a1 1 0 0 1 1-1ZM5.5 5h13M7.5 2.5h9M8 8v12M16 8v12",
  "play.rectangle":
    "M5 5h14a1.5 1.5 0 0 1 1.5 1.5v11A1.5 1.5 0 0 1 19 19H5a1.5 1.5 0 0 1-1.5-1.5v-11A1.5 1.5 0 0 1 5 5ZM10 9v6l5-3-5-3Z",
  "play.rectangle.on.rectangle":
    "M7.5 4.5h12A1.5 1.5 0 0 1 21 6v9M4.5 8h11A1.5 1.5 0 0 1 17 9.5v9a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 3 18.5v-9A1.5 1.5 0 0 1 4.5 8ZM8.5 11.5v5l4-2.5-4-2.5Z",
  "waveform.path.ecg": "M2.5 12h4L9 6l3.5 12L15.5 12h6",

  // ------------------------------------------------------------ 状态 / 操作
  bolt: "M13 2.5 5 13.5h6l-1 8 8-11h-6l1-8Z",
  "checkmark.circle": `M9 12.5l2.2 2.2L15.5 10${CIRCLE}`,
  "checkmark.shield": "M12 3l7.5 3v5.5c0 4.5-3.2 8.2-7.5 9.5-4.3-1.3-7.5-5-7.5-9.5V6L12 3ZM8.8 12l2.2 2.2 4.2-4.4",
  circle: CIRCLE,
  "circle.fill": CIRCLE,
  "circle.lefthalf.filled": CIRCLE,
  ellipsis: "",
  "ellipsis.circle": CIRCLE,
  eye: "M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12ZM12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z",
  gearshape:
    "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Zm7.4-3a7.4 7.4 0 0 0-.1-1.2l2-1.5-2-3.4-2.3 1a7.5 7.5 0 0 0-2-1.2l-.4-2.4h-4l-.4 2.4a7.5 7.5 0 0 0-2 1.2l-2.3-1-2 3.4 2 1.5a7.4 7.4 0 0 0 0 2.4l-2 1.5 2 3.4 2.3-1c.6.5 1.3.9 2 1.2l.4 2.4h4l.4-2.4c.7-.3 1.4-.7 2-1.2l2.3 1 2-3.4-2-1.5c.1-.4.1-.8.1-1.2Z",
  magnifyingglass: "M10.5 17a6.5 6.5 0 1 0 0-13 6.5 6.5 0 0 0 0 13ZM15.3 15.3l5.2 5.2",
  pencil: "M16.9 3.8l3.3 3.3L8.4 18.9l-4.2.9.9-4.2L16.9 3.8Z",
  "plus.circle": `M12 8.5v7M8.5 12h7${CIRCLE}`,
  "plus.square.dashed": "M4 7V4h3M10 4h4M17 4h3v3M20 10v4M20 17v3h-3M14 20h-4M7 20H4v-3M4 14v-4M12 8.5v7M8.5 12h7",
  "puzzlepiece.extension":
    "M10 4.5a2 2 0 1 1 4 0V6h4a1 1 0 0 1 1 1v4h-1.5a2 2 0 1 0 0 4H19v4a1 1 0 0 1-1 1h-4v-1.5a2 2 0 1 0-4 0V20H6a1 1 0 0 1-1-1v-4h1.5a2 2 0 1 0 0-4H5V7a1 1 0 0 1 1-1h4V4.5Z",
  "square.and.arrow.up": "M12 15V3.5M8.5 7 12 3.5 15.5 7M5 13v6.5a1.5 1.5 0 0 0 1.5 1.5h11a1.5 1.5 0 0 0 1.5-1.5V13",
  "square.and.pencil":
    "M17 3.8l3.2 3.2L13 14.2l-4 .8.8-4L17 3.8ZM19 14v5.5a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 5 19.5v-11A1.5 1.5 0 0 1 6.5 7H12",
  trash: "M4.5 6.5h15M9.5 6.5V4h5v2.5M6.5 6.5l1 13h9l1-13M10 10v6M14 10v6",
  "trash.slash": "M4.5 6.5h15M6.5 6.5l1 13h9l.6-8M3 3l18 18",
  "x.mark": "M6 6l12 12M18 6L6 18",
  xmark: "M6 6l12 12M18 6L6 18",

  // ------------------------------------------------------------ 游戏 / 动物
  gamecontroller: GAMECONTROLLER,
  "gamecontroller.fill": GAMECONTROLLER,
  hexagon: HEXAGON,
  "hexagon.fill": HEXAGON,
  hare: "M5 18.5a5 4.5 0 0 1 5-4.5h3.5a4.5 4.5 0 0 1 4.5 4.5v.5H5.5a.5.5 0 0 1-.5-.5ZM12 14 10 5.3a1.3 1.3 0 0 1 2.5-.6L15.5 14.5M16.2 14.8l2.3-6.3a1.3 1.3 0 0 1 2.4.9L19 15.8M15 17h.01",
  tortoise: "M3.5 15.5a7 6.5 0 0 1 14 0H3.5ZM17.5 15.5h2a1.5 1.5 0 0 0 1.5-1.5 2 2 0 0 0-3.2-1.6M6 15.5V18M15 15.5V18M7 12l3.5-3 3.5 3",
};

/**
 * 叠在线性主体上的实心部分。整枚实心的图标放整条路径；半实心的
 * （`circle.lefthalf.filled`）或本身就该是实心点的（`ellipsis`）只放那一块。
 */
const FILLS: Record<string, string> = {
  "bookmark.fill": STROKES["bookmark.fill"],
  "circle.fill": CIRCLE,
  "circle.lefthalf.filled": "M12 3a9 9 0 0 0 0 18V3Z",
  ellipsis: THREE_DOTS,
  "ellipsis.circle": "M7.5 13.2a1.2 1.2 0 1 0 0-2.4 1.2 1.2 0 0 0 0 2.4ZM12 13.2a1.2 1.2 0 1 0 0-2.4 1.2 1.2 0 0 0 0 2.4ZM16.5 13.2a1.2 1.2 0 1 0 0-2.4 1.2 1.2 0 0 0 0 2.4Z",
  "folder.fill": FOLDER,
  "gamecontroller.fill": GAMECONTROLLER,
  "hexagon.fill": HEXAGON,
  "paperclip.badge.ellipsis":
    "M14.5 20.5a1.2 1.2 0 1 0 0-2.4 1.2 1.2 0 0 0 0 2.4ZM18 20.5a1.2 1.2 0 1 0 0-2.4 1.2 1.2 0 0 0 0 2.4ZM21.5 20.5a1.2 1.2 0 1 0 0-2.4 1.2 1.2 0 0 0 0 2.4Z",
  "square.grid.2x2.fill": STROKES["square.grid.2x2.fill"],
  "square.grid.3x3.fill": SQUARE_GRID_3X3_FILL,
};

export type SymbolGlyph = {
  /** 实际画出来的那个符号名。降级时它和请求的名字不一样。 */
  symbol: string;
  stroke: string;
  fill: string | null;
  /** 走到了拼图块兜底。测试和调试时用得上：一眼看出哪些名字还没画。 */
  fallback: boolean;
};

/** 已画好的全部符号名。 */
export const KNOWN_SYMBOLS: readonly string[] = Object.keys(STROKES);

function glyph(symbol: string, fallback: boolean): SymbolGlyph {
  return { symbol, stroke: STROKES[symbol], fill: FILLS[symbol] ?? null, fallback };
}

function lookup(candidate: string): string | null {
  if (Object.prototype.hasOwnProperty.call(STROKES, candidate)) return candidate;
  const withoutFill = candidate
    .split(".")
    .filter((segment) => segment !== "fill" && segment !== "filled")
    .join(".");
  if (withoutFill && Object.prototype.hasOwnProperty.call(STROKES, withoutFill)) return withoutFill;
  return null;
}

/**
 * 符号名 → 要画的字形。接受 `sf:` 前缀，也接受声明式侧栏里那种裸名字。
 * `fallbackSymbol` 必须是已画好的名字；传错了也回落到拼图块，不会画空。
 */
export function resolveSymbol(name?: string | null, fallbackSymbol: string = FALLBACK_SYMBOL): SymbolGlyph {
  const raw = (name ?? "").trim();
  const symbol = raw.startsWith("sf:") ? raw.slice(3) : raw;
  if (symbol) {
    const segments = symbol.split(".");
    for (let length = segments.length; length > 0; length -= 1) {
      const hit = lookup(segments.slice(0, length).join("."));
      if (hit) return glyph(hit, false);
    }
  }
  const fallback = lookup(fallbackSymbol) ?? FALLBACK_SYMBOL;
  return glyph(fallback, true);
}

/** 插件包里图标可以是 `sf:` 符号，也可以是包内相对路径的图片（与 macOS 宿主同规则）。 */
export type PluginIconSource = { kind: "symbol"; name: string | null } | { kind: "image"; url: string };

const IMAGE_EXTENSION = /\.(png|jpe?g|gif|webp|svg|avif|ico)$/i;

/**
 * 清单图标 → 渲染来源。
 *
 * 包内图片经 `/plugin-host/{plugin}/{path}` 取——就是插件页面自己的资源走的
 * 那条路，越界与停用都由宿主判。这里只挡掉明显不是包内相对路径的写法（带
 * scheme、绝对路径、`..`），它们一律按符号名处理，最后落到拼图块。
 */
export function pluginIconSource(pluginId: string, icon?: string | null): PluginIconSource {
  const value = (icon ?? "").trim();
  if (!value || value.startsWith("sf:")) return { kind: "symbol", name: value || null };
  const segments = value.split("/");
  const isPackagePath =
    IMAGE_EXTENSION.test(value) &&
    !value.includes(":") &&
    !value.startsWith("/") &&
    !segments.some((segment) => segment === ".." || segment === "");
  if (!isPackagePath) return { kind: "symbol", name: value };
  const path = segments.map(encodeURIComponent).join("/");
  return { kind: "image", url: `/plugin-host/${encodeURIComponent(pluginId)}/${path}` };
}
