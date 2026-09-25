#!/usr/bin/env python3
"""Dev server for the new UI in mock mode: serves the repo root with caching disabled.

    python3 web/tests/tools/serve.py [port]      # default 4173
    open http://127.0.0.1:4173/web/next.html?mock=1
"""
import http.server
import os
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), '..', '..', '..'))


class NoCache(http.server.SimpleHTTPRequestHandler):
    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map, '.js': 'text/javascript', '.mjs': 'text/javascript', '.svg': 'image/svg+xml'}

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=ROOT, **kwargs)

    def end_headers(self):
        self.send_header('Cache-Control', 'no-store')
        super().end_headers()

    def log_message(self, fmt, *args):  # quiet
        pass


class Server(http.server.ThreadingHTTPServer):
    # The default backlog (5) resets connections when the browser loads ~40 modules at once.
    request_queue_size = 256
    daemon_threads = True


if __name__ == '__main__':
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 4173
    Server(('127.0.0.1', port), NoCache).serve_forever()
