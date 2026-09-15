#!/usr/bin/env python3
import functools
import json
import http.server
import urllib.error
import urllib.request
from pathlib import Path

MGMT = "http://localhost:9000/lambda-url/mgmt"
REDIRECT = "http://localhost:9000/lambda-url/redirect"
FORWARDED = ("content-type", "x-manage-secret")
RELAYED = ("cache-control", "content-type", "location")


class KeepRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        return None


OPENER = urllib.request.build_opener(KeepRedirects)


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path.startswith("/api/"):
            self.forward()
        elif self.is_static():
            super().do_GET()
        else:
            self.resolve()

    def do_DELETE(self):
        self.forward()

    def do_PATCH(self):
        self.forward()

    def do_POST(self):
        self.forward()

    def is_static(self):
        path = self.path.split("?", 1)[0]

        return path == "/" or "." in path.rsplit("/", 1)[-1]

    def forward(self):
        if not self.path.startswith("/api/"):
            self.send_error(404, "no such route")
            return

        length = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(length) if length else None
        headers = {name: self.headers[name] for name in FORWARDED if self.headers.get(name)}
        request = urllib.request.Request(MGMT + self.path[len("/api"):], data=body, headers=headers, method=self.command)

        self.relay(request, "api unreachable")

    def resolve(self):
        request = urllib.request.Request(REDIRECT + self.path, method="GET")

        self.relay(request, "redirect function unreachable")

    def relay(self, request, unreachable):
        try:
            with OPENER.open(request, timeout=120) as response:
                status, payload, headers = response.status, response.read(), response.headers
        except urllib.error.HTTPError as error:
            status, payload, headers = error.code, error.read(), error.headers
        except urllib.error.URLError as error:
            self.send_json(502, {"error": {"code": "upstream", "message": f"{unreachable}: {error.reason}"}})
            return

        self.send_response(status)
        for name in RELAYED:
            if headers.get(name):
                self.send_header(name, headers[name])
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def send_json(self, status, payload):
        body = json.dumps(payload).encode()

        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    handler = functools.partial(Handler, directory=str(Path(__file__).parent))
    print(f"frontend on http://localhost:3000, /api/* -> {MGMT}/* with the /api prefix stripped, /{{code}} -> {REDIRECT}/{{code}}")
    http.server.ThreadingHTTPServer(("0.0.0.0", 3000), handler).serve_forever()


if __name__ == "__main__":
    main()
