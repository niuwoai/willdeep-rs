/*
 * 插件页面宿主桥。注入进每个插件页面的 <head>。
 *
 * 这份 window.willdeep 契约与 macOS 版（Xedit `AgentPluginPageHost.swift` 的
 * bootstrap）逐个方法对齐，插件包因此**不需要为 Web 改一行**。变的只有传输层：
 * 那边是 WKWebView 的 messageHandlers，这边是 postMessage 到父窗口。
 *
 * 页面跑在 sandbox="allow-scripts" 的 iframe 里，是 opaque origin：拿不到父页面
 * 的 DOM、cookie 或 localStorage，CSP 的 connect-src 'none' 再挡掉 fetch / XHR /
 * WebSocket。所以页面能让宿主做的事，只有清单里声明过的那些。
 */
(function () {
  'use strict';

  var PENDING = Object.create(null);
  var HOST = window.parent;

  function newRequestID() {
    if (window.crypto && typeof crypto.randomUUID === 'function') return crypto.randomUUID();
    return String(Date.now()) + '-' + String(Math.random()).slice(2);
  }

  function post(message) {
    // 目标 origin 只能是 '*'：opaque origin 的 iframe 无法用具体 origin 发送。
    // 反方向由父页面用 event.source 身份核对，不靠 origin 字符串。
    if (HOST && HOST !== window) HOST.postMessage(message, '*');
  }

  function request(type, payload) {
    var requestID = newRequestID();
    var message = { __willdeep: 1, type: type, requestID: requestID };
    for (var key in payload) {
      if (Object.prototype.hasOwnProperty.call(payload, key)) message[key] = payload[key];
    }
    return new Promise(function (resolve, reject) {
      PENDING[requestID] = { resolve: resolve, reject: reject };
      post(message);
    });
  }

  function settle(detail, eventName) {
    if (!detail || !detail.requestID) return;
    var pending = PENDING[detail.requestID];
    delete PENDING[detail.requestID];
    // 事件照发：插件页面可以自己监听 willdeep:command-result，
    // macOS 版就是这么把结果送进页面的。
    window.dispatchEvent(new CustomEvent(eventName, { detail: detail }));
    if (!pending) return;
    if (detail.error) pending.reject(new Error(detail.error));
    else pending.resolve(detail.result);
  }

  window.willdeep = window.willdeep || {};
  // 同一个插件包跑在两个宿主上，能力集不一样：页面必须能问清楚再用，
  // 而不是调到一半吃 `undefined is not a function`。写法与 macOS 版同一份：
  //
  //     if ((window.willdeep.capabilities || []).includes('fs.write')) { … }
  //
  // 判据是 capabilities，不是 version：version 只说这套桥自己的迭代，
  // 两个宿主的号段互不比较大小。
  window.willdeep.version = '1.0.0';
  window.willdeep.capabilities = ['context', 'commands', 'ai.complete', 'ai.providers'];
  window.willdeep.getContext = function () {
    return window.__WILLDEEP_CONTEXT__ || {};
  };
  window.willdeep.selectItem = function (itemID) {
    post({ __willdeep: 1, type: 'selectItem', itemID: itemID });
  };
  window.willdeep.refresh = function () {
    post({ __willdeep: 1, type: 'refresh' });
  };
  window.willdeep.executeCommand = function (commandID, args) {
    return request('executeCommand', { commandID: commandID, arguments: args || {} });
  };
  // 打开指定 Agent 会话。需要 conversation.read。
  window.willdeep.openConversation = function (sessionID, messageID) {
    return request('openConversation', { sessionID: sessionID || '', messageID: messageID || '' });
  };
  // 插件能问模型，但拿不到任何一把 key：provider 与模型都由宿主校验，
  // 页面递上来的 baseURL 一律不认。需要 ai.chat / providers.read 权限。
  window.willdeep.ai = {
    providers: function () {
      return request('aiProviders', {});
    },
    complete: function (payload) {
      return request('aiComplete', { request: payload || {} });
    },
    // 停止一条正在跑的对话。按 complete() 里传的 streamID 找：没传
    // streamID 的请求停不了，因为页面手上没有别的把手。
    cancel: function (streamID) {
      return request('aiCancel', { streamID: streamID || '' });
    },
    // 请宿主生成图片。密钥不出宿主，模型与画幅都过宿主白名单；
    // referenceImagePaths 是宿主侧的绝对路径，宿主负责上传换 URL。
    generateImage: function (payload) {
      return request('aiGenerateImage', { request: payload || {} });
    }
  };
  // 已启用技能的只读清单。页面拿到的是 identifier / 名称 / 描述，
  // SKILL.md 正文与磁盘路径都不出宿主：把 identifier 放进 ai.complete 的
  // skills 数组，宿主会替你把正文读出来注入。需要 skills.read。
  window.willdeep.skills = {
    list: function () {
      return request('skillsList', {});
    }
  };
  // 工作区文件。路径可以是相对工作区根的，也可以是 fs.list 回给你的绝对
  // 路径；越界的一律拒。读要 workspace.read，写要 workspace.write。
  window.willdeep.fs = {
    list: function (path) {
      return request('fsList', { path: path || '' });
    },
    read: function (path) {
      return request('fsRead', { path: path || '' });
    },
    search: function (payload) {
      var req = payload || {};
      return request('fsSearch', {
        query: req.query || '',
        path: req.path || '',
        regex: !!req.regex,
        limit: req.limit || 0
      });
    },
    write: function (path, text) {
      return request('fsWrite', { path: path || '', text: text || '' });
    },
    // 改一段而不是整文件覆盖。找不到、不唯一、改了个寂寞，三种失败分开报。
    patch: function (path, oldString, newString, options) {
      var opts = options || {};
      return request('fsPatch', {
        path: path || '',
        oldString: oldString || '',
        newString: typeof newString === 'string' ? newString : '',
        replaceAll: !!opts.replaceAll
      });
    }
  };
  // 插件自己的小仓库。与下面的 localStorage 垫片是两套：这套存任意 JSON，
  // 异步，跨刷新；垫片只存字符串，给那些本来就在用 localStorage 的插件。
  window.willdeep.storage = {
    get: function (key) {
      return request('storageGet', { key: String(key) });
    },
    set: function (key, value) {
      return request('storageSet2', { key: String(key), value: value === undefined ? null : value });
    },
    remove: function (key) {
      return request('storageRemove2', { key: String(key) });
    },
    keys: function () {
      return request('storageKeys', {});
    }
  };
  // 跑命令（需要 process.execute）。只读命令直接跑，其余由**宿主页面**
  // 弹确认框，破坏性形状连问都不问。
  window.willdeep.process = {
    run: function (command) {
      return request('processRun', { command: command || '' });
    }
  };
  // 宿主代发的 HTTP（需要 network.access + 清单里的 networkDomains）。
  window.willdeep.net = {
    fetch: function (url, init) {
      var options = init || {};
      return request('netFetch', {
        url: url || '',
        method: options.method || 'GET',
        headers: options.headers || {},
        body: typeof options.body === 'string' ? options.body : null
      });
    }
  };
  window.willdeep.clipboard = {
    write: function (text) {
      return request('clipboardWrite', { text: text || '' });
    }
  };
  window.willdeep.notify = function (notification) {
    var payload = notification || {};
    return request('notify', { title: payload.title || '', body: payload.body || '' });
  };
  // 把活交给主 Agent。insert 只填输入框，send 才真的起回合。
  // 需要 conversation.write。
  window.willdeep.chat = {
    insert: function (text) {
      return request('chatInsert', { text: text || '' });
    },
    send: function (text) {
      return request('chatSend', { text: text || '' });
    }
  };
  // 宿主事件。on('turn.finished', cb) 之后宿主每次推事件都会调回调，
  // 不用再拿定时器扫。
  var eventListeners = {};
  window.addEventListener('willdeep:host-event', function (event) {
    var detail = event.detail || {};
    var listeners = eventListeners[detail.name] || [];
    for (var index = 0; index < listeners.length; index += 1) {
      // 插件的回调炸了不该带塌桥。
      try {
        listeners[index](detail.payload || {});
      } catch (error) {
        /* ignored */
      }
    }
  });
  window.willdeep.events = {
    on: function (name, callback) {
      if (typeof callback !== 'function') {
        return Promise.reject(new Error('callback must be a function'));
      }
      eventListeners[name] = (eventListeners[name] || []).concat([callback]);
      return request('eventsSubscribe', { name: name });
    },
    off: function (name, callback) {
      var listeners = eventListeners[name] || [];
      eventListeners[name] = callback
        ? listeners.filter(function (entry) {
            return entry !== callback;
          })
        : [];
    }
  };
  // 桥的版本与能力清单。插件据此降级，而不是在旧宿主上白屏：
  //   if ((window.willdeep.capabilities || []).indexOf('fs.search') >= 0) { … }
  // 与 macOS 宿主 AgentPluginPageBridgeVersion 同名同序——同一份插件包
  // 在两端做同样的特性判断，判出来必须是同一个答案。
  window.willdeep.version = '2.6.0';
  window.willdeep.capabilities = [
    'context',
    'commands',
    'conversation.open',
    'ai.complete',
    // 2.6.0：user 消息可带 imagePaths（插件媒体目录内的本地路径）。
    // 'ai.videos' 不在这里：本宿主没有视频解码器抽帧，带 videoPaths 会被拒。
    'ai.images',
    'ai.tools',
    'ai.cancel',
    // 'ai.reasoning' 不在这里：那一条是流式思考增量，本宿主的 ai.complete
    // 是一次性返回的。声明一个自己不发的事件，插件会白等一个永远不来的
    // willdeep:ai-reasoning——宁可少报一项，让它按没有来降级。
    'ai.image',
    'ai.providers',
    'skills.list',
    'fs.list',
    'fs.read',
    'fs.search',
    'storage',
    'events',
    'chat.insert',
    'chat.send',
    'fs.write',
    'fs.patch',
    'process.run',
    'net.fetch',
    'clipboard.write',
    'notify'
  ];

  window.__willdeepDeliverMCPMessage = function (message) {
    window.dispatchEvent(new MessageEvent('message', { data: message, source: null }));
  };

  window.addEventListener('message', function (event) {
    var data = event.data;
    if (!data || typeof data !== 'object') return;

    // 页面自己发给自己的 JSON-RPC：MCP Apps 的标准握手走这条路。
    if (event.source === window && data.jsonrpc === '2.0' && typeof data.method === 'string') {
      post({ __willdeep: 1, type: 'mcpMessage', message: data });
      return;
    }
    if (event.source !== HOST || data.__willdeep !== 1) return;

    switch (data.type) {
      case 'commandResult':
        settle(data, 'willdeep:command-result');
        break;
      case 'bridgeResult':
        settle(data, 'willdeep:bridge-result');
        break;
      case 'context':
        window.__WILLDEEP_CONTEXT__ = data.context || {};
        // 宿主注入的两套 --willdeep-* 变量由这个属性选一套。切主题只改属性，
        // 不重载页面——插件里填了一半的表单不该因为换了个配色就丢掉。
        var scheme = window.__WILLDEEP_CONTEXT__.colorScheme;
        if (scheme === 'light' || scheme === 'dark') {
          document.documentElement.setAttribute('data-willdeep-color-scheme', scheme);
        }
        window.dispatchEvent(
          new CustomEvent('willdeep:context-changed', { detail: window.__WILLDEEP_CONTEXT__ })
        );
        break;
      case 'mcpMessage':
        window.__willdeepDeliverMCPMessage(data.message);
        break;
      case 'hostEvent':
        window.dispatchEvent(
          new CustomEvent('willdeep:host-event', {
            detail: { name: data.name, payload: data.payload || {} }
          })
        );
        break;
      default:
        break;
    }
  });

  /*
   * localStorage 垫片。
   *
   * opaque origin 里 window.localStorage 直接抛 SecurityError，而插件包（经典
   * 游戏厅的最高分就是一例）本来在原生宿主里是有存储可用的。垫片让它们照常
   * 工作：读走注入的快照，写回宿主落盘，per-plugin 隔离。
   *
   * 这不是"给插件加了新能力"——原生宿主本来就给非持久化的 WebView 存储。
   */
  var storageWorks = true;
  try {
    window.localStorage.getItem('__willdeep_probe__');
  } catch (error) {
    storageWorks = false;
  }
  if (!storageWorks) {
    var data = window.__WILLDEEP_STORAGE__ || {};
    delete window.__WILLDEEP_STORAGE__;
    var shim = {
      getItem: function (key) {
        var name = String(key);
        return Object.prototype.hasOwnProperty.call(data, name) ? data[name] : null;
      },
      setItem: function (key, value) {
        data[String(key)] = String(value);
        post({ __willdeep: 1, type: 'storageSet', key: String(key), value: String(value) });
      },
      removeItem: function (key) {
        delete data[String(key)];
        post({ __willdeep: 1, type: 'storageRemove', key: String(key) });
      },
      clear: function () {
        data = {};
        post({ __willdeep: 1, type: 'storageClear' });
      },
      key: function (index) {
        var keys = Object.keys(data);
        return index >= 0 && index < keys.length ? keys[index] : null;
      }
    };
    Object.defineProperty(shim, 'length', {
      get: function () {
        return Object.keys(data).length;
      }
    });
    try {
      Object.defineProperty(window, 'localStorage', { value: shim, configurable: true });
      Object.defineProperty(window, 'sessionStorage', { value: shim, configurable: true });
    } catch (error) {
      // 定义不上就算了：插件包该有自己的容错，宿主不该为此崩掉页面。
    }
  }
})();
