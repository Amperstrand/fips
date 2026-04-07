#!/usr/bin/env python3
"""
FIPS Control Socket to HTTP Proxy

This proxy allows browser-based visualization of FIPS network data
by exposing the Unix socket control API over HTTP.

Usage:
    python3 fips-proxy.py [--socket PATH] [--port PORT]
    
    --socket: Path to FIPS control socket (default: /tmp/fips-control.sock)
    --port: HTTP port to listen on (default: 8080)

Example:
    python3 fips-proxy.py --socket /tmp/fips-control.sock --port 8080
    
Then open: file:///tmp/fips-visualizer.html
"""

import argparse
import json
import socket
import sys
from http.server import HTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse, parse_qs
import threading

class FIPSProxyHandler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        # Suppress default logging
        pass
    
    def do_OPTIONS(self):
        """Handle CORS preflight requests"""
        self.send_response(200)
        self.send_header('Access-Control-Allow-Origin', '*')
        self.send_header('Access-Control-Allow-Methods', 'GET, POST, OPTIONS')
        self.send_header('Access-Control-Allow-Headers', 'Content-Type')
        self.end_headers()
    
    def do_GET(self):
        """Handle GET requests"""
        parsed = urlparse(self.path)
        
        if parsed.path == '/api':
            self.handle_api_request(parsed.query)
        elif parsed.path == '/health':
            self.handle_health()
        else:
            self.send_error(404, 'Not Found')
    
    def do_POST(self):
        """Handle POST requests with JSON body"""
        if self.path == '/api':
            content_length = int(self.headers.get('Content-Length', 0))
            body = self.rfile.read(content_length).decode('utf-8')
            self.handle_api_request(body)
        else:
            self.send_error(404, 'Not Found')
    
    def handle_health(self):
        """Health check endpoint"""
        self.send_json_response({'status': 'ok', 'socket': self.server.socket_path})
    
    def handle_api_request(self, query_string):
        """Proxy request to FIPS control socket"""
        try:
            # Parse command from query string or body
            params = parse_qs(query_string)
            command = params.get('command', ['show_status'])[0]
            
            # Try to parse as JSON body if query didn't work
            if query_string and not params:
                try:
                    data = json.loads(query_string)
                    command = data.get('command', 'show_status')
                except:
                    pass
            
            # Query FIPS daemon
            response = self.query_fips(command)
            
            self.send_json_response(response)
            
        except Exception as e:
            self.send_error_response(500, str(e))
    
    def query_fips(self, command):
        """Send command to FIPS control socket and return response"""
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        
        try:
            sock.settimeout(5.0)
            sock.connect(self.server.socket_path)
            
            # Send request
            request = json.dumps({"command": command}) + '\n'
            sock.sendall(request.encode('utf-8'))
            
            # Read response
            response_data = b''
            while True:
                chunk = sock.recv(4096)
                if not chunk:
                    break
                response_data += chunk
                # Check if we have complete JSON
                try:
                    json.loads(response_data.decode('utf-8'))
                    break
                except:
                    continue
            
            return json.loads(response_data.decode('utf-8'))
            
        finally:
            sock.close()
    
    def send_json_response(self, data):
        """Send JSON response with CORS headers"""
        response = json.dumps(data, indent=2).encode('utf-8')
        
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', len(response))
        self.send_header('Access-Control-Allow-Origin', '*')
        self.end_headers()
        self.wfile.write(response)
    
    def send_error_response(self, code, message):
        """Send error JSON response"""
        response = json.dumps({'error': message}).encode('utf-8')
        
        self.send_response(code)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', len(response))
        self.send_header('Access-Control-Allow-Origin', '*')
        self.end_headers()
        self.wfile.write(response)


class ThreadedHTTPServer(HTTPServer):
    """Handle requests in separate threads"""
    def __init__(self, *args, socket_path=None, **kwargs):
        super().__init__(*args, **kwargs)
        self.socket_path = socket_path
    
    def process_request(self, request, client_address):
        """Start a new thread to process the request"""
        thread = threading.Thread(target=self.process_request_thread,
                                  args=(request, client_address))
        thread.daemon = True
        thread.start()
    
    def process_request_thread(self, request, client_address):
        """Process the request in a thread"""
        try:
            self.finish_request(request, client_address)
        except Exception:
            self.handle_error(request, client_address)
        finally:
            self.shutdown_request(request)


def main():
    parser = argparse.ArgumentParser(description='FIPS Control Socket HTTP Proxy')
    parser.add_argument('--socket', default='/tmp/fips-control.sock',
                       help='Path to FIPS control socket')
    parser.add_argument('--port', type=int, default=8080,
                       help='HTTP port to listen on')
    parser.add_argument('--host', default='127.0.0.1',
                       help='Host to bind to')
    
    args = parser.parse_args()
    
    server_address = (args.host, args.port)
    httpd = ThreadedHTTPServer(server_address, FIPSProxyHandler, socket_path=args.socket)
    
    print(f"FIPS Control Socket Proxy")
    print(f"  Socket: {args.socket}")
    print(f"  HTTP:   http://{args.host}:{args.port}")
    print(f"  API:    http://{args.host}:{args.port}/api?command=show_status")
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
