#!/usr/bin/env python3
"""Loopback HTTPS OAuth and resource peers for the outbound call comparison."""

import argparse
from collections import Counter
from contextlib import ExitStack
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import ipaddress
import json
import signal
import socket
from socketserver import BaseRequestHandler, ThreadingTCPServer
import ssl
from threading import Event, Lock, Thread
import time
from urllib.parse import parse_qs, urlsplit

from h2.config import H2Configuration
from h2.connection import H2Connection
from h2.events import ConnectionTerminated, DataReceived, RequestReceived, StreamEnded, StreamReset
from h2.exceptions import ProtocolError, StreamClosedError


EXCHANGE_GRANT = "urn:ietf:params:oauth:grant-type:token-exchange"
ACCESS_TOKEN_TYPE = "urn:ietf:params:oauth:token-type:access_token"
FIRST_SERVICE_TOKEN = "fixture-service-1"
HTTP_BODIES = {"/one": b"a" * 1024, "/tiny": b"a", "/large": b"a" * 65536, "/empty": b""}
HTTP_CHUNKS = (b"a" * 512, b"b" * 512)
GRPC_HEADERS = ((":status", "200"), ("content-type", "application/grpc"))
GRPC_MESSAGE = b"\x00\x00\x00\x00\x02\x08\x07"
TRANSPORT_CLOSED = (BrokenPipeError, ConnectionResetError, ssl.SSLEOFError, TimeoutError)
COUNTERS = (
    "service_grants",
    "service_responses",
    "service_pending",
    "exchange_grants",
    "exchange_responses",
    "invalid_grants",
    "resources",
    "old_token_requests",
    "replacement_token_requests",
    "exchange_token_requests",
    "unauthorized",
    "anonymous_requests",
    "http_connections",
    "http_forced_idle_closes",
    "http_request_bytes",
    "http_response_bytes",
    "http_response_aborts",
    "http_late_errors",
    "grpc_connections",
    "grpc_requests",
    "grpc_messages",
    "grpc_traceparent",
    "grpc_request_bytes",
    "grpc_old_token_requests",
    "grpc_replacement_token_requests",
    "grpc_exchange_token_requests",
    "grpc_unauthorized",
    "grpc_anonymous_requests",
)


class Fixture:
    def __init__(self, ttl: int):
        self.ttl = ttl
        self.lock = Lock()
        self.gate = Event()
        self.gate.set()
        self.count = Counter(dict.fromkeys(COUNTERS, 0))
        self.tokens = {}
        self.idle_http = set()
        self.http_wire = Counter()
        self.http_status = Counter()
        self.grpc_methods = Counter()
        self.grpc_status = Counter()
        self.grpc_last_traceparent = None

    def start_grant(self, kind: str) -> tuple[str, bool]:
        with self.lock:
            self.count[f"{kind}_grants"] += 1
            sequence = self.count[f"{kind}_grants"]
            held = kind == "service" and sequence > 1 and not self.gate.is_set()
            if held:
                self.count["service_pending"] += 1
            return f"fixture-{kind}-{sequence}", held

    def issue(self, token: str, kind: str) -> None:
        with self.lock:
            issued_ns = time.monotonic_ns()
            identity = f"{kind}_token_requests"
            if token == FIRST_SERVICE_TOKEN:
                identity = "old_token_requests"
            elif kind == "service":
                identity = "replacement_token_requests"
            self.tokens[token] = (identity, issued_ns, issued_ns + self.ttl * 1_000_000_000)

    def authorize_locked(self, header: str | None, prefix: str = "") -> bool:
        if not header:
            self.count[f"{prefix}anonymous_requests"] += 1
            return True
        _, _, token = header.partition(" ")
        issued = self.tokens.get(token) if header.startswith("Bearer ") else None
        if issued and time.monotonic_ns() < issued[2]:
            self.count[f"{prefix}{issued[0]}"] += 1
            return True
        self.count[f"{prefix}unauthorized"] += 1
        return False

    def force_idle_close(self) -> dict:
        with self.lock:
            closed = 0
            for connection in self.idle_http:
                try:
                    connection.shutdown(socket.SHUT_RDWR)
                except OSError:
                    continue
                closed += 1
            self.idle_http.clear()
            self.count["http_forced_idle_closes"] += closed
            return {"closed": closed}

    def snapshot(self) -> dict:
        with self.lock:
            _, issued_ns, expires_ns = self.tokens.get(FIRST_SERVICE_TOKEN, (None, None, None))
            return {
                **self.count,
                "gate_held": not self.gate.is_set(),
                "token_ttl": self.ttl,
                "monotonic_ns": time.monotonic_ns(),
                "first_service_issued_ns": issued_ns,
                "first_service_expires_ns": expires_ns,
                "http_idle_connections": len(self.idle_http),
                "http_status": dict(self.http_status),
                "http_wire": [
                    {
                        "method": method,
                        "target": target,
                        "host": host,
                        "content_length": lengths,
                        "transfer_encoding": encodings,
                        "body_bytes": body_bytes,
                        "requests": count,
                    }
                    for (method, target, host, lengths, encodings, body_bytes), count in self.http_wire.items()
                ],
                "grpc_methods": dict(self.grpc_methods),
                "grpc_status": dict(self.grpc_status),
                "grpc_last_traceparent": self.grpc_last_traceparent,
            }


class HttpHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    wbufsize = 131072

    def log_message(self, _format, *_args):
        pass

    def read_body(self) -> bytes | None:
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            length = -1
        if not 0 <= length <= 1024 * 1024 or self.headers.get("Transfer-Encoding"):
            self.close_connection = True
            self.send_bytes(400)
            return None
        payload = self.rfile.read(length)
        if len(payload) != length:
            self.close_connection = True
            return None
        return payload

    def send_bytes(self, status: int, body: bytes = b"", headers=()) -> int:
        self.send_response(status)
        if status != 204:
            self.send_header("Content-Length", str(len(body)))
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        if self.command == "HEAD":
            body = b""
        self.wfile.write(body)
        self.wfile.flush()
        return len(body)

    def send_json(self, value: dict, status: int = 200) -> None:
        body = json.dumps(value).encode()
        self.send_bytes(status, body, (("Content-Type", "application/json"),))


class ResourceHandler(HttpHandler):
    def setup(self):
        super().setup()
        self.fixture = self.server.fixture
        with self.fixture.lock:
            self.fixture.count["http_connections"] += 1

    def finish(self):
        with self.fixture.lock:
            self.fixture.idle_http.discard(self.connection)
        super().finish()

    def dispatch(self):
        with self.fixture.lock:
            self.fixture.idle_http.discard(self.connection)
        payload = self.read_body()
        if payload is None:
            return
        path = urlsplit(self.path).path
        if self.command == "POST" and path == "/token":
            self.token(payload)
        else:
            self.resource(path, len(payload))

    do_GET = do_HEAD = do_POST = do_PUT = do_PATCH = do_DELETE = dispatch

    def token(self, payload: bytes) -> None:
        try:
            form = parse_qs(payload.decode("ascii"))
        except UnicodeDecodeError:
            form = {}
        kind = {
            "client_credentials": "service",
            EXCHANGE_GRANT: "exchange",
        }.get(form.get("grant_type", [""])[0])
        if kind is None:
            with self.fixture.lock:
                self.fixture.count["invalid_grants"] += 1
            self.send_json({"error": "unsupported_grant_type"}, 400)
            return
        token, held = self.fixture.start_grant(kind)
        try:
            if held:
                self.fixture.gate.wait()
            self.fixture.issue(token, kind)
            result = {"access_token": token, "token_type": "Bearer", "expires_in": self.fixture.ttl}
            if kind == "exchange":
                result["issued_token_type"] = ACCESS_TOKEN_TYPE
            self.send_json(result)
            with self.fixture.lock:
                self.fixture.count[f"{kind}_responses"] += 1
        finally:
            if held:
                with self.fixture.lock:
                    self.fixture.count["service_pending"] -= 1

    def resource(self, path: str, request_bytes: int) -> None:
        wire = (
            self.command, self.path, self.headers.get("Host"),
            tuple(self.headers.get_all("Content-Length", ())),
            tuple(self.headers.get_all("Transfer-Encoding", ())),
            request_bytes,
        )
        with self.fixture.lock:
            self.fixture.count["resources"] += 1
            self.fixture.count["http_request_bytes"] += request_bytes
            self.fixture.http_wire[wire] += 1
            authorized = self.fixture.authorize_locked(self.headers.get("Authorization"))
        status = 200
        try:
            if not authorized:
                status = 401
                sent = self.send_bytes(status)
            elif path in ("/multi", "/late-error"):
                sent = self.send_chunks(late_error=path == "/late-error")
            elif path in HTTP_BODIES:
                status = 204 if path == "/empty" else 200
                padding = (("X-Fixture-Padding", "a" * 4096),) if path == "/tiny" else ()
                sent = self.send_bytes(status, HTTP_BODIES[path], padding)
            else:
                status = 404
                sent = self.send_bytes(status)
        except TRANSPORT_CLOSED:
            with self.fixture.lock:
                self.fixture.count["http_response_aborts"] += 1
            raise
        with self.fixture.lock:
            self.fixture.http_status[status] += 1
            self.fixture.count["http_response_bytes"] += sent
            if not self.close_connection:
                self.fixture.idle_http.add(self.connection)

    def send_chunks(self, late_error: bool) -> int:
        self.send_response(200)
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        if self.command == "HEAD":
            self.wfile.flush()
            return 0
        chunks = HTTP_CHUNKS[:1] if late_error else HTTP_CHUNKS
        for chunk in chunks:
            self.wfile.write(f"{len(chunk):x}\r\n".encode() + chunk + b"\r\n")
            self.wfile.flush()
        if late_error:
            with self.fixture.lock:
                self.fixture.count["http_late_errors"] += 1
            self.close_connection = True
            time.sleep(0.1)
            self.connection.shutdown(socket.SHUT_WR)
        else:
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
        return sum(map(len, chunks))


class ControlHandler(HttpHandler):
    def do_GET(self):
        if self.path == "/stats":
            self.send_json(self.server.fixture.snapshot())
        else:
            self.send_bytes(404)

    def do_POST(self):
        if self.read_body() is None:
            return
        fixture = self.server.fixture
        if self.path == "/gate/hold":
            with fixture.lock:
                fixture.gate.clear()
            self.send_json(fixture.snapshot())
        elif self.path == "/gate/release":
            with fixture.lock:
                fixture.gate.set()
            self.send_json(fixture.snapshot())
        elif self.path == "/retry-arm":
            self.send_json(fixture.force_idle_close())
        else:
            self.send_bytes(404)


class GrpcHandler(BaseRequestHandler):
    def handle(self):
        if self.request.selected_alpn_protocol() != "h2":
            return
        self.fixture = self.server.fixture
        with self.fixture.lock:
            self.fixture.count["grpc_connections"] += 1
        protocol = H2Connection(config=H2Configuration(client_side=False, header_encoding="utf-8"))
        protocol.initiate_connection()
        self.flush(protocol)
        headers = {}
        while data := self.request.recv(64 * 1024):
            for event in protocol.receive_data(data):
                stream = getattr(event, "stream_id", None)
                if isinstance(event, RequestReceived):
                    headers[stream] = dict(event.headers)
                elif isinstance(event, DataReceived):
                    protocol.acknowledge_received_data(event.flow_controlled_length, stream)
                    with self.fixture.lock:
                        self.fixture.count["grpc_request_bytes"] += len(event.data)
                elif isinstance(event, StreamEnded):
                    try:
                        self.reply(protocol, stream, headers.pop(stream))
                    except StreamClosedError:
                        continue
                elif isinstance(event, StreamReset):
                    headers.pop(stream, None)
                elif isinstance(event, ConnectionTerminated):
                    return
            self.flush(protocol)

    def flush(self, protocol: H2Connection) -> None:
        if output := protocol.data_to_send():
            self.request.sendall(output)

    def reply(self, protocol: H2Connection, stream: int, headers: dict) -> None:
        path = headers[":path"]
        method = path.rsplit("/", 1)[-1]
        trace = headers.get("traceparent")
        with self.fixture.lock:
            self.fixture.count["grpc_requests"] += 1
            self.fixture.grpc_methods[path] += 1
            if trace:
                self.fixture.count["grpc_traceparent"] += 1
                self.fixture.grpc_last_traceparent = trace
            authorized = self.fixture.authorize_locked(headers.get("authorization"), "grpc_")
        status = "0" if authorized else "16"
        trailers = (("grpc-status", status),)
        if method == "Immediate" or not authorized:
            protocol.send_headers(stream, GRPC_HEADERS + trailers, end_stream=True)
            messages = 0
        else:
            messages = 3 if method == "Stream" else 1
            protocol.send_headers(stream, GRPC_HEADERS)
            if method == "Stream":
                self.flush(protocol)
            for _ in range(messages):
                protocol.send_data(stream, GRPC_MESSAGE)
            protocol.send_headers(stream, trailers, end_stream=True)
        with self.fixture.lock:
            self.fixture.grpc_status[status] += 1
            self.fixture.count["grpc_messages"] += messages


class PeerServer:
    request_queue_size = 256
    allow_reuse_address = True
    daemon_threads = True

    def __init__(self, host, port, handler, fixture, tls=None):
        self.address_family = socket.AF_INET6 if host.version == 6 else socket.AF_INET
        self.fixture = fixture
        self.tls = tls
        super().__init__((str(host), port), handler)

    def finish_request(self, request, client_address):
        try:
            request.settimeout(120)
            request.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            if self.tls is None:
                super().finish_request(request, client_address)
            else:
                with self.tls.wrap_socket(request, server_side=True) as secured:
                    super().finish_request(secured, client_address)
        except (*TRANSPORT_CLOSED, ssl.SSLError, ProtocolError):
            return


class HttpServer(PeerServer, ThreadingHTTPServer):
    pass


class GrpcServer(PeerServer, ThreadingTCPServer):
    pass


def tls_context(args, alpn: str) -> ssl.SSLContext:
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(args.cert, args.key)
    context.set_alpn_protocols([alpn])
    return context


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bind", type=ipaddress.ip_address, default="127.0.0.1")
    parser.add_argument("--cert", required=True)
    parser.add_argument("--key", required=True)
    parser.add_argument("--http-port", type=int, default=8443)
    parser.add_argument("--grpc-port", type=int, default=8444)
    parser.add_argument("--control-port", type=int, default=8445)
    parser.add_argument("--token-ttl", type=int, default=30)
    args = parser.parse_args()
    if not args.bind.is_loopback:
        parser.error("--bind must be a loopback IP address")
    if args.token_ttl <= 0:
        parser.error("--token-ttl must be positive")
    stop = Event()
    signal.signal(signal.SIGINT, lambda *_args: stop.set())
    signal.signal(signal.SIGTERM, lambda *_args: stop.set())
    fixture = Fixture(args.token_ttl)
    with ExitStack() as cleanup:
        resource = cleanup.enter_context(HttpServer(
            args.bind, args.http_port, ResourceHandler, fixture, tls_context(args, "http/1.1")
        ))
        grpc = cleanup.enter_context(GrpcServer(
            args.bind, args.grpc_port, GrpcHandler, fixture, tls_context(args, "h2")
        ))
        control = cleanup.enter_context(HttpServer(args.bind, args.control_port, ControlHandler, fixture))
        if args.bind.version == 6:
            authority = f"[{args.bind}]"
        else:
            authority = str(args.bind)
        ready = {}
        for name, server in (("http", resource), ("grpc", grpc), ("control", control)):
            port = server.server_address[1]
            scheme = "http" if name == "control" else "https"
            ready[f"{name}_port"] = port
            ready[f"{name}_origin"] = f"{scheme}://{authority}:{port}"
            thread = Thread(target=server.serve_forever, kwargs={"poll_interval": 0.1}, daemon=True)
            thread.start()
            cleanup.callback(thread.join)
            cleanup.callback(server.shutdown)
        cleanup.callback(fixture.gate.set)
        print(json.dumps(ready), flush=True)
        stop.wait()


if __name__ == "__main__":
    main()
