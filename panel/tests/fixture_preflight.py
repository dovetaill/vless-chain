"""Test-only transport seam for loopback REALITY data-plane fixtures.

Public-target admission is covered separately, including rejection here before the
seam is enabled. Production binaries and API contracts receive no bypass flags.
"""
import http.server
import json
import urllib.error
import urllib.request


def server(harness, target, server_name, enabled):
    host, port = target.rsplit(':', 1)
    assert host == '127.0.0.1' and int(port) in harness.ports
    endpoint = harness.base
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            assert self.path.startswith('/api/agent/')
            body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
            headers = {'Content-Type': 'application/json', 'Authorization': self.headers.get('Authorization', '')}

            def forward(payload):
                request = urllib.request.Request(endpoint + self.path, data=payload, headers=headers)
                try:
                    response = opener.open(request, timeout=20)
                except urllib.error.HTTPError as error:
                    response = error
                except urllib.error.URLError:
                    return 503, b'{"error":"fixture upstream unavailable"}'
                return response.status, response.read()

            status, body = forward(body)
            if status == 200 and self.path.endswith('/diagnostics'):
                task = json.loads(body)
                if task and task.get('request', {}).get('operation') == 'reality-preflight' and self.server.enabled:
                    request = task['request']['request']
                    assert request == {'target': target, 'server_name': server_name}, request
                    if self.server.on_preflight:
                        self.server.on_preflight(headers['Authorization'])
                    status, body = forward(json.dumps({'id': task['id'], 'result': {
                        'ok': True, 'cloudflare_checked': True, 'fallback': {'status': 'verified'}}}).encode())
                    self.server.intercepted += 1
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass  # The interrupted-publication fixture deliberately disconnects its Agent.

        def log_message(self, *_):
            pass

    proxy = http.server.ThreadingHTTPServer(('127.0.0.1', harness.port()), Handler)
    proxy.enabled, proxy.intercepted = enabled, 0
    proxy.on_preflight = None
    harness.server(proxy)
    return proxy
