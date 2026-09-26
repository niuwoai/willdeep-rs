"""A tiny stdio MCP server that behaves like a plugin (for tests only).

Line-delimited JSON-RPC on stdin/stdout. Every interesting event is appended as
one JSON line to the file named by FAKE_MCP_LOG so tests can assert on what the
host sent.

Tools:
  echo   -> returns its arguments as text
  draw   -> sends a reverse request to the host (arguments.method / .params),
            waits for the answer and returns it as text
  later  -> answers immediately, then sends a reverse request while the host
            has nothing in flight; the host's answer is logged
  sleep  -> sleeps arguments.seconds before answering
  exit   -> answers, then exits the process
"""

import json
import os
import sys
import threading
import time

LOG = os.environ.get("FAKE_MCP_LOG")
WRITE_LOCK = threading.Lock()
LOG_LOCK = threading.Lock()
QUEUED = []
COUNTER = [0]


def log(event, **fields):
    if not LOG:
        return
    with LOG_LOCK:
        with open(LOG, "a") as handle:
            handle.write(json.dumps(dict(event=event, **fields)) + "\n")


def send(message):
    with WRITE_LOCK:
        sys.stdout.write(json.dumps(message) + "\n")
        sys.stdout.flush()


def next_request_id():
    with WRITE_LOCK:
        COUNTER[0] += 1
        return "fake-%d" % COUNTER[0]


def read_message():
    if QUEUED:
        return QUEUED.pop(0)
    line = sys.stdin.readline()
    if not line:
        return None
    return json.loads(line)


def host_request(method, params):
    request_id = next_request_id()
    send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
    while True:
        line = sys.stdin.readline()
        if not line:
            return None
        message = json.loads(line)
        if message.get("id") == request_id and "method" not in message:
            return message
        QUEUED.append(message)


def later(method, params, delay):
    time.sleep(delay)
    request_id = next_request_id()
    log("later_sent", id=request_id)
    send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})


TOOLS = [
    {"name": "echo", "description": "Echo the arguments", "inputSchema": {"type": "object"}},
    {"name": "draw", "description": "Ask the host for something"},
    {"name": "later", "description": "Ask the host later"},
    {"name": "sleep", "description": "Sleep", "annotations": {"readOnlyHint": True}},
    {"name": "exit", "description": "Exit"},
]

log("spawned", pid=os.getpid(), home=os.environ.get("WILLDEEP_HOME"))

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    message_id = message.get("id")
    if method is None:
        log("host_response", message=message)
        continue
    if message_id is None:
        log("notification", method=method)
        continue
    params = message.get("params") or {}
    if method == "initialize":
        log("initialize", params=params)
        send({"jsonrpc": "2.0", "id": message_id, "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake", "version": "1"},
        }})
    elif method == "tools/list":
        log("tools_list")
        send({"jsonrpc": "2.0", "id": message_id, "result": {"tools": TOOLS}})
    elif method == "tools/call":
        name = params.get("name")
        arguments = params.get("arguments") or {}
        log("tools_call", name=name)
        if name == "echo":
            text = json.dumps(arguments)
        elif name == "draw":
            answer = host_request(arguments.get("method", "willdeep/images/generate"),
                                  arguments.get("params", {}))
            text = json.dumps(answer)
        elif name == "later":
            threading.Thread(
                target=later,
                args=(arguments.get("method", "willdeep/images/generate"),
                      arguments.get("params", {}), float(arguments.get("delay", 0.3))),
                daemon=True,
            ).start()
            text = "scheduled"
        elif name == "sleep":
            time.sleep(float(arguments.get("seconds", 1)))
            text = "slept"
        elif name == "exit":
            send({"jsonrpc": "2.0", "id": message_id,
                  "result": {"content": [{"type": "text", "text": "bye"}]}})
            sys.exit(0)
        else:
            send({"jsonrpc": "2.0", "id": message_id,
                  "error": {"code": -32602, "message": "unknown tool %s" % name}})
            continue
        send({"jsonrpc": "2.0", "id": message_id,
              "result": {"content": [{"type": "text", "text": text}]}})
    elif method == "ping":
        send({"jsonrpc": "2.0", "id": message_id, "result": {}})
    else:
        send({"jsonrpc": "2.0", "id": message_id,
              "error": {"code": -32601, "message": "Method not found: %s" % method}})
