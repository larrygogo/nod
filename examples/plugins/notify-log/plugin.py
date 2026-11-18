"""Nod external plugin example: a notification channel that writes JSON lines to a file.

Protocol: JSON-RPC 2.0, one message per line on stdin/stdout (see docs/plugins.md).
"""
import json
import os
import sys
import time

settings = {}


def reply(msg_id, result=None, error=None):
    msg = {"jsonrpc": "2.0", "id": msg_id}
    if error is not None:
        msg["error"] = {"code": -32000, "message": error}
    else:
        msg["result"] = result
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def append(event):
    path = os.path.expanduser(settings.get("path") or "")
    if not path:
        raise ValueError("set the file path in the plugin's settings")
    event["at"] = int(time.time())
    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps(event, ensure_ascii=False) + "\n")


for line in sys.stdin:
    if not line.strip():
        continue
    msg = json.loads(line)
    method, params, msg_id = msg.get("method"), msg.get("params") or {}, msg.get("id")
    if method == "shutdown":
        break
    try:
        if method == "initialize":
            settings = params.get("settings") or {}
            reply(msg_id, {"name": "notify-log", "version": "1.0.0"})
        elif method == "notify.test":
            append({"kind": "test"})
            reply(msg_id, "written")
        elif method == "notify.send":
            append(dict(params))
            reply(msg_id, {"sent": True})
        else:
            reply(msg_id, error="unknown method " + str(method))
    except Exception as e:  # report, never crash the host
        reply(msg_id, error=str(e))
