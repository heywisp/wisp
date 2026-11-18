import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def event(kind, **fields):
    return {"type": kind, **fields}


events = [
    event("message_start", message={
        "id": "msg_smoke", "type": "message", "role": "assistant",
        "model": "claude-opus-5-5", "content": [], "stop_reason": None,
        "stop_sequence": None, "usage": {"input_tokens": 10, "output_tokens": 1},
    }),
    event("content_block_start", index=0, content_block={"type": "text", "text": ""}),
    event("content_block_delta", index=0, delta={"type": "text_delta", "text": "Smoke reply."}),
    event("content_block_stop", index=0),
    event("message_delta", delta={"stop_reason": "end_turn", "stop_sequence": None},
          usage={"output_tokens": 5}),
    event("message_stop"),
]
body = "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def __getattr__(self, name):
        if name.startswith("do_"):
            return lambda: self.send_error(404)
        raise AttributeError(name)

    def log_message(self, *args):
        pass


with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
    print(f"http://127.0.0.1:{server.server_port}", flush=True)
    server.serve_forever()
