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
  // 插件能问模型，但拿不到任何一把 key：provider 与模型都由宿主校验，
  // 页面递上来的 baseURL 一律不认。需要 ai.chat / providers.read 权限。
  window.willdeep.ai = {
    providers: function () {
      return request('aiProviders', {});
    },
    complete: function (payload) {
      return request('aiComplete', { request: payload || {} });
    }
  };

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
        window.dispatchEvent(
          new CustomEvent('willdeep:context-changed', { detail: window.__WILLDEEP_CONTEXT__ })
        );
        break;
      case 'mcpMessage':
        window.__willdeepDeliverMCPMessage(data.message);
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
