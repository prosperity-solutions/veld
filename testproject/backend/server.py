#!/usr/bin/env python3
"""Simple HTTP backend server for the Veld test project.

Usage:
    python3 server.py <port>

Endpoints:
    GET /health  -> 200 "ok"
    GET /        -> 200 HTML page
"""

import sys
from http.server import HTTPServer, BaseHTTPRequestHandler


class BackendHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            self._respond(200, "text/plain", "ok")
        elif self.path == "/":
            body = (
                "<!DOCTYPE html>"
                "<html><head><title>Backend</title></head>"
                "<body><h1>Backend Service</h1>"
                "<p>Powered by Veld</p></body></html>"
            )
            self._respond(200, "text/html", body)
        else:
            self._respond(404, "text/plain", "not found")

    def _respond(self, status, content_type, body):
        payload = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, format, *args):
        # Write logs to stderr so they show up in veld logs.
        sys.stderr.write(f"[backend] {format % args}\n")


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <port>", file=sys.stderr)
        sys.exit(1)

    port = int(sys.argv[1])
    server = HTTPServer(("127.0.0.1", port), BackendHandler)
    print(f"[backend] listening on 127.0.0.1:{port}", file=sys.stderr)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
