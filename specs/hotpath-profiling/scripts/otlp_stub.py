#!/usr/bin/env python3
"""Remote-only synthetic OTLP/HTTP sink: consumes protobuf and acknowledges."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

class Sink(BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length', '0')))
        self.send_response(200)
        self.send_header('Content-Type', 'application/x-protobuf')
        self.send_header('Content-Length', '0')
        self.end_headers()
    def log_message(self, *args):
        pass

ThreadingHTTPServer(('127.0.0.1', 4318), Sink).serve_forever()
