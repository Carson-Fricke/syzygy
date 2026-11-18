"""Capture the supported wire subset from a pinned Codex App Server on loopback."""

import argparse
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


VERSION = "codex-cli 0.169.0-alpha.11.1"
MODEL = "gpt-5.4"
PROMPT = "Read the result for trial-7."
CALL = {
    "type": "function_call",
    "id": "fc_fixture",
    "call_id": "call_fixture",
    "namespace": "research",
    "name": "read_trial",
    "arguments": '{"trial":"trial-7"}',
}
REASONING = {
    "type": "reasoning",
    "id": "rs_fixture",
    "summary": [{"type": "summary_text", "text": "Looking up the trial."}],
    "encrypted_content": "c3ludGhldGljLWZpeHR1cmU",
}
MESSAGE = {
    "type": "message",
    "id": "msg_fixture",
    "role": "assistant",
    "content": [{"type": "output_text", "text": "Trial seven passed.", "annotations": []}],
}
RESULT = '{"passed":true}'
SCHEMA = {
    "type": "object",
    "properties": {"trial": {"type": "string"}},
    "required": ["trial"],
    "additionalProperties": False,
}
TOOL = {
    "type": "namespace",
    "name": "research",
    "description": "Read retained research results",
    "tools": [{
        "type": "function",
        "name": "read_trial",
        "description": "Read a trial result",
        "inputSchema": SCHEMA,
    }],
}
USAGE = {
    "input_tokens": 42,
    "output_tokens": 11,
    "total_tokens": 53,
    "input_tokens_details": {"cached_tokens": 12},
    "output_tokens_details": {"reasoning_tokens": 4},
}


def event(kind, **fields):
    return {"type": f"response.{kind}", **fields}


def steps():
    return [
        [
            event("created", response={"id": "resp_one"}),
            event("output_item.added", output_index=0, item={**REASONING, "summary": []}),
            event("reasoning_summary_part.added", item_id=REASONING["id"], output_index=0,
                  part={"type": "summary_text", "text": ""}, summary_index=0),
            event("reasoning_summary_text.delta", item_id=REASONING["id"], output_index=0,
                  summary_index=0, delta="Looking up the trial."),
            event("output_item.done", output_index=0, item=REASONING),
            event("output_item.added", output_index=1, item={**CALL, "arguments": ""}),
            event("function_call_arguments.delta", output_index=1, item_id=CALL["id"], delta='{"trial":'),
            event("function_call_arguments.delta", output_index=1, item_id=CALL["id"], delta='"trial-7"}'),
            event("function_call_arguments.done", output_index=1, item_id=CALL["id"], arguments=CALL["arguments"]),
            event("output_item.done", output_index=1, item=CALL),
            event("completed", response={"id": "resp_one", "output": [REASONING, CALL], "usage": USAGE}),
        ],
        [
            event("created", response={"id": "resp_two"}),
            event("output_item.added", output_index=0, item={**MESSAGE, "content": []}),
            event("output_text.delta", output_index=0, content_index=0, item_id=MESSAGE["id"], delta="Trial seven "),
            event("output_text.delta", output_index=0, content_index=0, item_id=MESSAGE["id"], delta="passed."),
            event("output_item.done", output_index=0, item=MESSAGE),
            event("completed", response={"id": "resp_two", "output": [MESSAGE], "usage": USAGE}),
        ],
    ]


def fixture(codex):
    version = subprocess.check_output([codex, "--version"], text=True).strip()
    if version != VERSION:
        raise RuntimeError(f"expected {VERSION}, found {version}")
    streams = steps()
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            self.send_error(404)

        def do_POST(self):
            index = len(requests)
            if self.path != "/v1/responses" or index >= len(streams):
                self.send_error(400)
                return
            if self.headers.get("Authorization") != "Bearer fixture-only":
                self.send_error(401)
                return
            body = self.rfile.read(int(self.headers["Content-Length"]))
            requests.append(json.loads(body))
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            for item in streams[index]:
                self.wfile.write(f"event: {item['type']}\ndata: {json.dumps(item)}\n\n".encode())
                self.wfile.flush()

    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            deltas = run_codex(codex, f"http://127.0.0.1:{server.server_port}/v1")
        finally:
            server.shutdown()
            worker.join()
    if len(requests) != 2 or "".join(deltas) != "Trial seven passed.":
        raise RuntimeError("Codex did not finish the two request tool exchange")

    def supported(body):
        fields = ("model", "stream", "store", "tool_choice", "parallel_tool_calls", "reasoning")
        subset = {key: body[key] for key in fields if key in body}
        tools = [tool for tool in body.get("tools", []) if tool.get("name") == TOOL["name"]]
        inputs = []
        for item in body["input"]:
            if item.get("type") == "message" and item.get("role") == "user":
                if any(content.get("text") == PROMPT for content in item.get("content", [])):
                    inputs.append({field: item[field] for field in ("type", "role", "content")})
            elif item.get("call_id") == CALL["call_id"] or item.get("id") == REASONING["id"]:
                item = item.copy()
                # The wire-assigned result ID is nondeterministic. Correlation
                # uses call_id. An omitted optional reasoning body is equivalent.
                if item["type"] == "function_call_output":
                    item.pop("id", None)
                if item["type"] == "reasoning" and item.get("content") is None:
                    item.pop("content", None)
                inputs.append(item)
        return {**subset, "tools": tools, "input": inputs}

    return {"codex_version": version, "requests": list(map(supported, requests)), "streams": streams, "text_deltas": deltas}


def run_codex(codex, url):
    settings = {
        "model_provider": "syzygy_fixture",
        "model_providers.syzygy_fixture.name": "Syzygy loopback fixture",
        "model_providers.syzygy_fixture.base_url": url,
        "model_providers.syzygy_fixture.env_key": "SYZYGY_WIRE_FIXTURE_TOKEN",
        "model_providers.syzygy_fixture.wire_api": "responses",
        "model_providers.syzygy_fixture.requires_openai_auth": False,
        "model_providers.syzygy_fixture.supports_websockets": False,
        "analytics.enabled": False,
        "web_search": "disabled",
        "model_reasoning_effort": "medium",
    }
    command = [codex, "app-server", "--listen", "stdio://"]
    for key, value in settings.items():
        command += ["-c", f"{key}={json.dumps(value)}"]
    # Give the child only the environment needed to launch and its synthetic key.
    names = ("PATH", "SYSTEMROOT", "WINDIR", "USERPROFILE", "LOCALAPPDATA", "APPDATA",
             "TEMP", "TMP", "HOME", "LANG", "TERM")
    env = {key: os.environ[key] for key in names if key in os.environ}
    env["SYZYGY_WIRE_FIXTURE_TOKEN"] = "fixture-only"
    with tempfile.TemporaryDirectory(prefix="syzygy-responses-fixture-") as cwd:
        with tempfile.TemporaryFile(mode="w+t", encoding="utf-8") as stderr:
            with subprocess.Popen(
                command, cwd=cwd, env=env, text=True, encoding="utf-8",
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr,
            ) as process:
                try:
                    return exchange(process, cwd)
                except (RuntimeError, queue.Empty) as error:
                    # Avoid forwarding Codex's environment, workspace or account diagnostics.
                    raise RuntimeError(f"Codex fixture failed: {error}") from None
                finally:
                    if process.poll() is None:
                        process.kill()


def exchange(process, cwd):
    messages = queue.Queue()

    def read():
        for line in process.stdout:
            messages.put(json.loads(line))
        messages.put(None)

    threading.Thread(target=read, daemon=True).start()

    def send(message):
        process.stdin.write(json.dumps(message) + "\n")
        process.stdin.flush()

    def receive():
        message = messages.get(timeout=20)
        if message is None:
            raise RuntimeError("App Server closed its stream")
        return message

    def rpc(request_id, method, params):
        send({"id": request_id, "method": method, "params": params})
        while True:
            message = receive()
            if message.get("id") == request_id and "method" not in message:
                if "error" in message:
                    raise RuntimeError(f"{method}: {message['error'].get('message')}")
                return message.get("result")

    rpc(1000, "initialize", {
        "clientInfo": {"name": "Syzygy", "title": "Syzygy fixture", "version": "0.1.0"},
        "capabilities": {"experimentalApi": True},
    })
    send({"method": "initialized"})
    thread = rpc(1001, "thread/start", {
        "cwd": cwd, "model": MODEL, "ephemeral": True, "sandbox": "read-only",
        "approvalPolicy": "never", "dynamicTools": [TOOL],
    })["thread"]["id"]
    send({"id": 1002, "method": "turn/start", "params": {
        "threadId": thread, "input": [{"type": "text", "text": PROMPT, "textElements": []}],
    }})
    deltas = []
    while True:
        message = receive()
        kind = message.get("method")
        if "error" in message and message.get("id") == 1002:
            raise RuntimeError(f"turn/start: {message['error'].get('message')}")
        if kind == "item/tool/call":
            params = message["params"]
            if params.get("tool") != CALL["name"] or params.get("arguments") != json.loads(CALL["arguments"]):
                raise RuntimeError("unexpected tool call")
            send({"id": message["id"], "result": {
                "success": True, "contentItems": [{"type": "inputText", "text": RESULT}],
            }})
        elif kind == "item/agentMessage/delta":
            deltas.append(message["params"]["delta"])
        elif kind == "turn/completed":
            status = message["params"]["turn"]["status"]
            if status != "completed":
                raise RuntimeError(f"unexpected terminal turn status {status}")
            return deltas


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex", default="codex")
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.write_text(json.dumps(fixture(args.codex), indent=2) + "\n", encoding="utf-8")
