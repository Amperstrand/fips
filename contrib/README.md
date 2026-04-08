# FIPS Contrib Tools

Standalone utilities for working with FIPS mesh networks.

## Contents

- **fips-proxy.py** - HTTP proxy for FIPS control socket
- **fips-visualizer.html** - Browser-based mesh network visualization

## Quick Start

### 1. Enable Control Socket

Add to your FIPS config (`/etc/fips/fips.yaml` on Linux):

```yaml
control:
  enabled: true
  socket_path: "/tmp/fips-control.sock"
```

Restart FIPS to create the socket:

```bash
sudo systemctl restart fips
```

### 2. Run the Proxy

On the machine running FIPS:

```bash
python3 contrib/fips-proxy.py --socket /tmp/fips-control.sock --port 8080
```

Or with SSH tunneling from a remote machine:

```bash
# On macOS (remote):
ssh -L /tmp/fips-control.sock:/tmp/fips-control.sock user@linux-host

# Then run proxy locally:
python3 contrib/fips-proxy.py --socket /tmp/fips-control.sock --port 8080
```

### 3. Open Visualizer

Serve the HTML file to avoid CORS issues:

```bash
cd contrib && python3 -m http.server 8000
```

Then open: http://localhost:8000/fips-visualizer.html

## fips-proxy.py

HTTP proxy that exposes the FIPS Unix control socket over HTTP with CORS support.

### Options

```
--socket PATH   Path to FIPS control socket (default: /tmp/fips-control.sock)
--port PORT     HTTP port to listen on (default: 8080)
--host HOST     Host to bind to (default: 127.0.0.1)
```

### Endpoints

- `GET /api?command=show_status` - Send command to FIPS
- `POST /api` - Send JSON command `{"command": "show_status"}`
- `GET /health` - Health check

### Example

```bash
curl "http://localhost:8080/api?command=show_peers"
```

## fips-visualizer.html

Browser-based visualization using Cytoscape.js. Displays:

- Mesh topology with node relationships
- Latency (SRTT) on edges
- Node types (self, root, peer)
- Interactive node details panel

### Configuration

Edit the `CONFIG` object in the HTML file:

```javascript
const CONFIG = {
    proxyUrl: 'http://localhost:8080/api',
    staticData: null,  // Set to JSON string for offline mode
    refreshInterval: 5000  // ms
};
```

### Layouts

- **Circle** - Nodes arranged in a circle
- **Tree** - Hierarchical layout
- **Force** - Force-directed layout

## Requirements

- Python 3.x (for proxy)
- Modern browser with JavaScript (for visualizer)
- FIPS with control socket enabled

## Troubleshooting

### Socket not found

```
Error: No such file or directory
```

Ensure FIPS is running with `control.enabled: true` in config.

### Connection refused

```
Error: Connection refused
```

Check that the proxy is running and the socket path is correct.

### CORS errors in browser

Serve the HTML file via HTTP server instead of opening directly:

```bash
python3 -m http.server 8000
```

Then access via `http://localhost:8000/fips-visualizer.html`.
