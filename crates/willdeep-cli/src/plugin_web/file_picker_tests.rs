//! 远程选文件：表、上传落盘、合成与转交两种交付。
//!
//! 端到端那几条真的绑回环端口、真的拉起一个假插件进程（python3），挂上与
//! `web.rs` 同一道 1 MiB 请求体上限。家目录一律在临时目录里，不碰真实的
//! `~/.willdeep`。

use super::*;
use axum::extract::DefaultBodyLimit;
use willdeep_core::plugin::test_support::{python_available, scratch_home};

const PLUGIN: &str = "willdeep-video-studio";

#[test]
fn the_table_gives_reference_images_and_music_their_own_rules() {
    let reference =
        file_picker(PLUGIN, "video-studio", "video.pick_reference").expect("reference picker");
    assert_eq!(reference.delivery, PickerDelivery::Synthesize);
    assert_eq!(reference.max_bytes, 20 * MIB);
    assert_eq!(reference.extensions, ["png", "jpg", "jpeg", "webp"]);

    // 与插件 `EpisodeComposer::MUSIC_EXTENSIONS` / `MAX_MUSIC_BYTES` 同值。
    let music = file_picker(PLUGIN, "video-studio", "episode.import_music").expect("music picker");
    assert_eq!(music.delivery, PickerDelivery::Forward);
    assert_eq!(music.max_bytes, 200 * MIB);
    assert_eq!(
        music.extensions,
        ["mp3", "wav", "m4a", "aac", "flac", "aiff", "aif"]
    );

    // 三元组要全对上：同名工具换个插件、换个服务都不接管。
    assert!(file_picker("other-plugin", "video-studio", "episode.import_music").is_none());
    assert!(file_picker(PLUGIN, "other-server", "episode.import_music").is_none());
    assert!(file_picker(PLUGIN, "video-studio", "episode.generate_music").is_none());

    let view = FilePickerView::new("episode.importMusic", music);
    assert_eq!(view.accept, ".mp3,.wav,.m4a,.aac,.flac,.aiff,.aif");
    assert_eq!(view.max_bytes, 200 * MIB);
}

#[test]
fn upload_names_only_contribute_a_whitelisted_extension() {
    let music = file_picker(PLUGIN, "video-studio", "episode.import_music").unwrap();
    assert_eq!(
        upload_extension("主题曲.MP3", music).as_deref(),
        Some("mp3")
    );
    assert_eq!(
        upload_extension("../../etc/passwd.flac", music).as_deref(),
        Some("flac"),
        "只取扩展名，路径段活不到落盘"
    );
    assert_eq!(upload_extension("cover.png", music), None);
    assert_eq!(upload_extension("noextension", music), None);
    assert_eq!(upload_extension("x.mp3.exe", music), None);
}

#[test]
fn only_fresh_uploads_can_be_forwarded_because_the_host_deletes_them() {
    let music = file_picker(PLUGIN, "video-studio", "episode.import_music").unwrap();
    assert!(is_forwardable_upload(
        FsPath::new("/m/upload-abc.mp3"),
        music
    ));
    // 插件自己导入过的曲目：转交完会被删，所以不收。
    assert!(!is_forwardable_upload(
        FsPath::new("/m/music-abc.mp3"),
        music
    ));
    assert!(!is_forwardable_upload(
        FsPath::new("/m/upload-abc.png"),
        music
    ));
    assert!(!is_forwardable_upload(
        FsPath::new("/m/upload-abc.mp3.part"),
        music
    ));
}

#[tokio::test]
async fn uploads_stream_to_disk_and_never_leave_a_partial_file() {
    let home = scratch_home("upload-stream");
    let target = home.join("upload-ok.mp3");
    let size = receive_upload(Body::from(vec![7u8; 4096]), &target, 4096)
        .await
        .unwrap_or_else(|_| panic!("exactly at the limit is fine"));
    assert_eq!(size, 4096);
    assert_eq!(std::fs::read(&target).unwrap(), vec![7u8; 4096]);
    assert!(!home.join("upload-ok.mp3.part").exists());

    let too_big = home.join("upload-big.mp3");
    let error = receive_upload(Body::from(vec![0u8; 4097]), &too_big, 4096)
        .await
        .expect_err("one byte over");
    assert!(matches!(error, PluginWebError::BadRequest(ref code) if code == "invalidSize"));
    assert!(!too_big.exists() && !home.join("upload-big.mp3.part").exists());

    let empty = home.join("upload-empty.mp3");
    let error = receive_upload(Body::empty(), &empty, 4096)
        .await
        .expect_err("empty");
    assert!(matches!(error, PluginWebError::BadRequest(ref code) if code == "invalidSize"));
    assert!(!empty.exists() && !home.join("upload-empty.mp3.part").exists());
    let _ = std::fs::remove_dir_all(&home);
}

/// 假的短剧插件：`episode.import_music` 照真插件的做法读 `path`、拷成
/// `music-<n>.<ext>` 落进同一个媒体目录；其余工具一律记一笔「不该来」。
/// 每次调用把看到的东西写进 `FAKE_LOG`，测试据此断言插件那一侧的视角。
const FAKE_VIDEO_STUDIO: &str = r#"
import json, os, shutil, sys
LOG = os.environ["FAKE_LOG"]
def log(entry):
    with open(LOG, "a") as handle:
        handle.write(json.dumps(entry) + "\n")
def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    message = json.loads(line)
    method, mid, params = message.get("method"), message.get("id"), message.get("params") or {}
    if mid is None:
        continue
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {"protocolVersion": "2025-06-18",
              "capabilities": {"tools": {}}, "serverInfo": {"name": "fake", "version": "1"}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": mid, "result": {"tools": [
              {"name": "episode.import_music", "inputSchema": {"type": "object"}},
              {"name": "video.pick_reference", "inputSchema": {"type": "object"}}]}})
    elif method == "tools/call":
        name, args = params.get("name"), params.get("arguments") or {}
        path = args.get("path", "")
        entry = {"tool": name, "arguments": args, "exists": os.path.isfile(path)}
        payload = {"ok": False}
        if name == "episode.import_music" and entry["exists"]:
            entry["size"] = os.path.getsize(path)
            copy = os.path.join(os.path.dirname(path), "music-%d%s" % (entry["size"], os.path.splitext(path)[1]))
            shutil.copyfile(path, copy)
            payload = {"ok": True, "settings": {"bgm": {"source": "file", "fileName": os.path.basename(copy)}}}
        log(entry)
        send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": json.dumps(payload)}]}})
    else:
        send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": method}})
"#;

struct Fixture {
    home: PathBuf,
    base: String,
    log: PathBuf,
    client: reqwest::Client,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

impl Fixture {
    async fn new(label: &str) -> Option<Self> {
        if !python_available() {
            eprintln!("python3 not found; skipping file picker test");
            return None;
        }
        let home = scratch_home(label);
        let root = home.join("plugins").join(PLUGIN).join("1.0.0");
        let write = |path: PathBuf, contents: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        };
        write(
            root.join(".codex-plugin/plugin.json"),
            &json!({"name": PLUGIN, "version": "1.0.0", "interface": {"displayName": "Video Studio"}})
                .to_string(),
        );
        let command = |id: &str, tool: &str| json!({"id": id, "titleKey": id, "handler": {"type": "mcpTool", "server": "video-studio", "tool": tool}});
        write(
            root.join(".willdeep-plugin/plugin.json"),
            &json!({
                "schemaVersion": 1,
                "permissions": ["process.execute"],
                "dependencies": {"mcpServers": ["video-studio"]},
                "contributes": {"destinations": [], "pages": [], "commands": [
                    command("episode.importMusic", "episode.import_music"),
                    command("video.pickReference", "video.pick_reference"),
                    command("episode.generateMusic", "episode.generate_music"),
                ]}
            })
            .to_string(),
        );
        write(
            root.join(".willdeep-plugin/locales/en.json"),
            &json!({
                "episode.importMusic": "Import music",
                "video.pickReference": "Pick reference",
                "episode.generateMusic": "Generate music",
            })
            .to_string(),
        );
        write(root.join("server.py"), FAKE_VIDEO_STUDIO);
        let log = home.join("fake-video-studio.jsonl");
        write(
            root.join("mcp.json"),
            &json!({"mcpServers": {"video-studio": {
                "command": "python3",
                "args": ["${pluginRoot}/server.py"],
                "env": {"FAKE_LOG": log.to_string_lossy()},
                "startup_timeout_sec": 5
            }}})
            .to_string(),
        );

        let host = Arc::new(PluginHost::discover(&home).expect("host"));
        host.approve(PLUGIN, 1)
            .await
            .unwrap_or_else(|error| panic!("approve: {error:?} failures: {:?}", host.failures()));
        host.set_enabled(PLUGIN, true)
            .await
            .expect("write")
            .expect("enabled");
        let state = Arc::new(PluginWebState::new(
            host,
            home.join("config.toml"),
            home.clone(),
            Arc::new(std::sync::RwLock::new(Vec::new())),
        ));
        // 与 web.rs 同一道全站上限：上传要能越过它，别的 JSON 接口仍受它管。
        let app = router(state).layer(DefaultBodyLimit::max(1024 * 1024));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Some(Self {
            home,
            base,
            log,
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            server,
        })
    }

    fn media(&self) -> PathBuf {
        crate::plugin_capabilities::plugin_media_directory(&self.home, PLUGIN)
            .unwrap_or_else(|_| panic!("media directory"))
    }

    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    async fn upload(&self, command: &str, name: &str, body: Vec<u8>) -> (u16, Value) {
        let response = self
            .client
            .post(format!("{}/api/plugins/{PLUGIN}/files", self.base))
            .query(&[("command", command), ("name", name)])
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(body)
            .send()
            .await
            .expect("upload");
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn command(&self, command: &str, arguments: Value) -> (u16, Value) {
        let response = self
            .client
            .post(format!(
                "{}/api/plugins/{PLUGIN}/commands/{command}",
                self.base
            ))
            .json(&json!({"arguments": arguments}))
            .send()
            .await
            .expect("command");
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }
}

/// 工具结果外层是 MCP 的 `content[0].text`，里面才是插件的业务对象。
fn tool_payload(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("text block");
    serde_json::from_str(text).expect("payload json")
}

#[tokio::test]
async fn snapshot_tells_the_page_which_commands_pick_files_and_what_they_accept() {
    let Some(fixture) = Fixture::new("picker-snapshot").await else {
        return;
    };
    let snapshot: Value = fixture
        .client
        .get(format!("{}/api/plugins", fixture.base))
        .send()
        .await
        .expect("snapshot")
        .json()
        .await
        .expect("json");
    let plugin = snapshot["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == PLUGIN)
        .expect("plugin listed");
    assert_eq!(
        plugin["file_pickers"],
        json!([
            {"command": "episode.importMusic", "accept": ".mp3,.wav,.m4a,.aac,.flac,.aiff,.aif", "max_bytes": 200 * MIB},
            {"command": "video.pickReference", "accept": ".png,.jpg,.jpeg,.webp", "max_bytes": 20 * MIB},
        ])
    );
}

#[tokio::test]
async fn imported_music_is_uploaded_forwarded_with_its_path_and_then_cleaned_up() {
    let Some(fixture) = Fixture::new("picker-music").await else {
        return;
    };
    // 3 MiB：比全站 1 MiB 的请求体上限大，证明上传不再被那道上限卡住。
    let audio = vec![0x49u8; 3 * 1024 * 1024];
    let (status, uploaded) = fixture
        .upload("episode.importMusic", "晚风.MP3", audio.clone())
        .await;
    assert_eq!(status, 200, "{uploaded}");
    assert_eq!(uploaded["byteSize"], audio.len());
    let path = PathBuf::from(uploaded["path"].as_str().unwrap());
    let name = path.file_name().unwrap().to_str().unwrap().to_owned();
    assert!(
        name.starts_with("upload-") && name.ends_with(".mp3"),
        "{name}"
    );
    assert_eq!(
        path.parent().unwrap().canonicalize().unwrap(),
        fixture.media().canonicalize().unwrap()
    );
    assert_eq!(std::fs::read(&path).unwrap(), audio);
    let canonical = path.canonicalize().unwrap();

    let (status, response) = fixture
        .command(
            "episode.importMusic",
            json!({"dramaID": "d1", "episodeID": "e1", "path": path.display().to_string()}),
        )
        .await;
    assert_eq!(status, 200, "{response}");
    assert_eq!(response["kind"], "tool");
    // 结果是插件原样回的，不是宿主合成的。
    let payload = tool_payload(&response);
    assert_eq!(payload["ok"], true);
    let imported = payload["settings"]["bgm"]["fileName"].as_str().unwrap();

    let calls = fixture.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0]["tool"], "episode.import_music");
    assert_eq!(calls[0]["arguments"]["dramaID"], "d1");
    assert_eq!(calls[0]["arguments"]["episodeID"], "e1");
    assert_eq!(
        calls[0]["arguments"]["path"],
        canonical.display().to_string(),
        "插件拿到的是规范化后的服务端路径"
    );
    assert_eq!(calls[0]["exists"], true, "插件被调用时上传件还在、读得到");
    assert_eq!(calls[0]["size"], audio.len());

    assert!(!path.exists(), "转交完上传件就删掉");
    assert!(
        fixture.media().join(imported).is_file(),
        "插件自己拷的那一份留着"
    );
}

#[tokio::test]
async fn forwarding_refuses_anything_but_a_fresh_upload_and_never_calls_the_plugin() {
    let Some(fixture) = Fixture::new("picker-refuse").await else {
        return;
    };
    let (status, body) = fixture
        .command(
            "episode.importMusic",
            json!({"dramaID": "d1", "episodeID": "e1"}),
        )
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("filePickerRequired"))
    );

    // 插件已经导入过的曲目：路径合法，但宿主转交完会删它，所以不收。
    let existing = fixture.media().join("music-old.mp3");
    std::fs::write(&existing, b"old track").unwrap();
    let (status, body) = fixture
        .command(
            "episode.importMusic",
            json!({"dramaID": "d1", "episodeID": "e1", "path": existing.display().to_string()}),
        )
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("invalidSelection"))
    );
    assert!(existing.is_file(), "旧曲目不能被当成上传件删掉");

    let outside = fixture.home.join("outside.mp3");
    std::fs::write(&outside, b"secret").unwrap();
    let (status, body) = fixture
        .command(
            "episode.importMusic",
            json!({"dramaID": "d1", "episodeID": "e1", "path": outside.display().to_string()}),
        )
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("invalidSelection"))
    );
    assert!(outside.is_file());

    assert!(fixture.calls().is_empty(), "拒掉的请求一条都不该进插件");
}

#[tokio::test]
async fn uploads_follow_the_rules_of_the_command_they_are_for() {
    let Some(fixture) = Fixture::new("picker-rules").await else {
        return;
    };
    let (status, body) = fixture
        .upload("episode.importMusic", "cover.png", vec![1, 2, 3])
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("unsupportedFileType"))
    );

    // 参照图的上限是 20 MiB：声明长度一超就拒，不必收完。
    let (status, body) = fixture
        .upload(
            "video.pickReference",
            "big.png",
            vec![0u8; (20 * MIB + 1) as usize],
        )
        .await;
    assert_eq!((status, body["error"].as_str()), (400, Some("invalidSize")));

    // 不是选文件的命令，上传口不接。
    let (status, body) = fixture
        .upload("episode.generateMusic", "x.mp3", vec![1, 2, 3])
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("notFilePicker"))
    );

    // 全站 1 MiB 上限对 JSON 接口照样生效——旧的 base64-JSON 上传就是
    // 卡在这里，参照图过了约 750 KiB 就传不上去。
    let (status, _) = fixture
        .command(
            "episode.importMusic",
            json!({"padding": "x".repeat(2 * 1024 * 1024)}),
        )
        .await;
    assert_eq!(status, 413);

    let leftovers: Vec<_> = std::fs::read_dir(fixture.media())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    assert!(leftovers.is_empty(), "被拒的上传不留文件：{leftovers:?}");
}

#[tokio::test]
async fn reference_images_are_still_synthesized_and_kept() {
    let Some(fixture) = Fixture::new("picker-reference").await else {
        return;
    };
    let (status, uploaded) = fixture
        .upload("video.pickReference", "ref.JPG", vec![0xFF, 0xD8, 0xFF])
        .await;
    assert_eq!(status, 200, "{uploaded}");
    let path = PathBuf::from(uploaded["path"].as_str().unwrap());
    assert!(path.to_string_lossy().ends_with(".jpg"));

    let (status, response) = fixture
        .command(
            "video.pickReference",
            json!({"path": path.display().to_string()}),
        )
        .await;
    assert_eq!(status, 200, "{response}");
    assert_eq!(
        tool_payload(&response),
        json!({"ok": true, "path": path.canonicalize().unwrap().display().to_string()})
    );
    assert!(fixture.calls().is_empty(), "参照图不进 MCP 服务");
    assert!(path.is_file(), "参照图就是选择结果，后续工具还要读，不删");
}
