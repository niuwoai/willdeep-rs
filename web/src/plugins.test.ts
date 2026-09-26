import { afterEach, describe, expect, it, vi } from "vitest";
import { filePickerRejection, uploadPluginFile, type PluginFilePickerView } from "./plugins";

const MIB = 1024 * 1024;
// 与后端 FILE_PICKER_TOOLS 里 episode.import_music 那一行同值。
const music: PluginFilePickerView = {
  command: "episode.importMusic",
  accept: ".mp3,.wav,.m4a,.aac,.flac,.aiff,.aif",
  max_bytes: 200 * MIB,
};

describe("filePickerRejection", () => {
  it("白名单内、大小合规的文件放行，扩展名不分大小写", () => {
    expect(filePickerRejection({ name: "主题曲.MP3", size: 3 * MIB }, music)).toBeNull();
    expect(filePickerRejection({ name: "loop.aiff", size: 1 }, music)).toBeNull();
  });

  it("绕过文件框 accept 选来的类型在上传前就拦下", () => {
    expect(filePickerRejection({ name: "cover.png", size: 10 }, music)).toBe("unsupportedFileType");
    expect(filePickerRejection({ name: "noextension", size: 10 }, music)).toBe("unsupportedFileType");
    expect(filePickerRejection({ name: ".mp3", size: 10 }, music)).toBe("unsupportedFileType");
  });

  it("空文件与超过上限的文件不上传，上限本身可以", () => {
    expect(filePickerRejection({ name: "a.mp3", size: 0 }, music)).toBe("invalidSize");
    expect(filePickerRejection({ name: "a.mp3", size: 200 * MIB + 1 }, music)).toBe("invalidSize");
    expect(filePickerRejection({ name: "a.mp3", size: 200 * MIB }, music)).toBeNull();
  });
});

describe("uploadPluginFile", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("请求体就是文件本身，命令与原文件名走查询串", async () => {
    const fetchMock = vi.fn(async () => new Response(JSON.stringify({ path: "/srv/upload-x.mp3" })));
    vi.stubGlobal("fetch", fetchMock);
    const file = new File([new Uint8Array([1, 2, 3])], "晚风 & 雨.mp3");

    const uploaded = await uploadPluginFile("willdeep-video-studio", "episode.importMusic", file);

    expect(uploaded.path).toBe("/srv/upload-x.mp3");
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    const parsed = new URL(url, "http://host");
    expect(parsed.pathname).toBe("/api/plugins/willdeep-video-studio/files");
    expect(parsed.searchParams.get("command")).toBe("episode.importMusic");
    expect(parsed.searchParams.get("name")).toBe("晚风 & 雨.mp3");
    expect(init.method).toBe("POST");
    expect(init.body).toBe(file);
    expect((init.headers as Record<string, string>)["content-type"]).toBe("application/octet-stream");
  });
});
