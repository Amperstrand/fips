#!/usr/bin/env python3
"""
FIPS Control Socket to HTTP Proxy + Visualizer

This proxy allows browser-based visualization of FIPS network data
by exposing the Unix socket control API over HTTP and serving the
visualizer HTML page.

Usage:
    python3 fips-proxy.py [--socket PATH] [--port PORT] [--static-dir DIR]
    
    --socket:     Path to FIPS control socket (default: /tmp/fips-control.sock)
    --port:       HTTP port to listen on (default: 8080)
    --static-dir: Directory containing static files to serve (default: same as this script)

Example:
    python3 fips-proxy.py --socket /tmp/fips-control.sock --port 8080
    
Then open: http://localhost:8080/
"""

import argparse
import json
import mimetypes
import os
import socket
import sys
from http.server import HTTPServer, BaseHTTPRequestHandler
from pathlib import Path
from urllib.parse import urlparse, parse_qs
import threading


class FIPSProxyHandler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass
    
    def do_OPTIONS(self):
        self.send_response(200)
        self.send_header('Access-Control-Allow-Origin', '*')
        self.send_header('Access-Control-Allow-Methods', 'GET, POST, OPTIONS')
        self.send_header('Access-Control-Allow-Headers', 'Content-Type')
        self.end_headers()
    
    def guess_type(self, path):
        ext = path.rsplit('.')[-1].lower()
        if ext in ('html', 'htm'):
            return 'text/html'
        elif ext in ('css',):
            return 'text/css'
        elif ext in ('js', 'javascript', 'mjs'):
            return 'application/javascript'
        elif ext in ('json',):
            return 'application/json'
        elif ext in ('png', 'jpg', 'jpeg', 'gif', 'ico', 'svg'):
            return 'image/' + ext
        elif ext in ('woff', 'woff2'):
            return 'font/woff2'
        elif ext in ('ttf', 'otf'):
            return 'font/ttf'
        else:
            return 'application/octet-stream'
    
    def do_GET(self):
        parsed = urlparse(self.path)
        
        if parsed.path == '/api':
            self.handle_api_request(parsed.query)
        elif parsed.path == '/health':
            self.handle_health()
        elif parsed.path.startswith('/static/'):
            self.serve_static_file(parsed.path[8:])
        elif parsed.path == '/' or parsed.path == '':
            self.serve_visualizer()
        else:
            self.send_error(404, 'Not found')
    
    def serve_static_file(self, rel_path):
        full_path = Path(self.server.static_dir) / rel_path.lstrip('/')
        if not full_path.exists():
            self.send_error(404, f'File not found: {rel_path}')
            return
        
        try:
            with open(full_path, 'rb') as f:
                content = f.read()
            
            content_type = self.guess_type(str(full_path))
            
            self.send_response(200)
            self.send_header('Content-Type', content_type)
            self.send_header('Content-Length', len(content))
            self.send_header('Access-Control-Allow-Origin', '*')
            self.end_headers()
            self.wfile.write(content)
        except Exception as e:
            self.send_error(500, f'Error reading file: {e}')
    
    def serve_visualizer(self):
        visualizer_path = Path(self.server.static_dir) / 'fips-visualizer.html'
        if not visualizer_path.exists():
            self.send_error(404, f'Visualizer not found: {visualizer_path}')
            return
        
        try:
            with open(visualizer_path, 'rb') as f:
                content = f.read().decode('utf-8')
            
            content = content.replace(
                "const CONFIG = {\n            proxyUrl: 'http://localhost:8080/api',",
                f"const CONFIG = {{\n            proxyUrl: 'http://{self.server.host}:{self.server.port}/api',"
            )
            
            response = content.encode('utf-8')
            self.send_response(200)
            self.send_header('Content-Type', 'text/html')
            self.send_header('Content-Length', len(response))
            self.send_header('Access-Control-Allow-Origin', '*')
            self.end_headers()
            self.wfile.write(response)
        except Exception as e:
            self.send_error(500, f'Error serving visualizer: {e}')
    
    def do_POST(self):
        if self.path == '/api':
            content_length = int(self.headers.get('Content-Length', 0))
            body = self.rfile.read(content_length).decode('utf-8')
            self.handle_api_request(body)
        else:
            self.send_error(404, 'Not found')
    
    def handle_health(self):
        self.send_json_response({'status': 'ok', 'socket': self.server.socket_path})
    
    def handle_api_request(self, query_string):
        try:
            params = parse_qs(query_string)
            command = params.get('command', ['show_status'])[0]
            
            if query_string and not params:
                try:
                    data = json.loads(query_string)
                    command = data.get('command', 'show_status')
                except:
                    pass
            
            response = self.query_fips(command)
            self.send_json_response(response)
        except Exception as e:
            self.send_error_response(500, str(e))
    
    def query_fips(self, command):
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        
        try:
            sock.settimeout(5.0)
            sock.connect(self.server.socket_path)
            
            request = json.dumps({"command": command}) + '\n'
            sock.sendall(request.encode('utf-8'))
            
            response_data = b''
            while True:
                chunk = sock.recv(4096)
                if not chunk:
                    break
                response_data += chunk
                try:
                    json.loads(response_data.decode('utf-8'))
                    break
                except:
                    continue
            
            return json.loads(response_data.decode('utf-8'))
        finally:
            sock.close()
    
    def send_json_response(self, data):
        response = json.dumps(data, indent=2).encode('utf-8')
        
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', len(response))
        self.send_header('Access-Control-Allow-Origin', '*')
        self.end_headers()
        self.wfile.write(response)
    
    def send_error_response(self, code, message):
        response = json.dumps({'error': message}).encode('utf-8')
        
        self.send_response(code)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', len(response))
        self.send_header('Access-Control-Allow-Origin', '*')
        self.end_headers()
        self.wfile.write(response)


class ThreadedHTTPServer(HTTPServer):
    def __init__(self, server_address, handler_class, socket_path=None, static_dir=None, host=None, port=None):
        super().__init__(server_address, handler_class)
        self.socket_path = socket_path
        self.static_dir = static_dir
        self.host = host or server_address[0]
        self.port = port or server_address[1]


def main():
    parser = argparse.ArgumentParser(description='FIPS Control Socket HTTP Proxy + Visualizer')
    parser.add_argument('--socket', default='/tmp/fips-control.sock',
                        help='Path to FIPS control socket')
    parser.add_argument('--port', type=int, default=8080,
                        help='HTTP port to listen on')
    parser.add_argument('--host', default='127.0.0.1',
                        help='Host to bind to')
    parser.add_argument('--static-dir', default=None,
                        help='Directory containing static files (default: same as script)')
    
    args = parser.parse_args()
    
    static_dir = Path(args.static_dir) if args.static_dir else Path(__file__).parent
    
    server_address = (args.host, args.port)
    httpd = ThreadedHTTPServer(
        server_address,
        FIPSProxyHandler,
        socket_path=args.socket,
        static_dir=static_dir,
        host=args.host,
        port=args.port
    )
    
    print(f"FIPS Control Socket Proxy + Visualizer")
    print(f"  Socket:     {args.socket}")
    print(f"  Static:     {static_dir}")
    print(f"  HTTP:      http://{args.host}:{args.port}")
    print(f"  API:       http://{args.host}:{args.port}/api?command=show_status")
    print(f"  Visualizer: http://{args.host}:{args.port}/")
    print()
    print("Press Ctrl+C to stop")
    print()
    
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\nShutting down...")
        httpd.shutdown()


if __name__ == '__main__':
    main()
