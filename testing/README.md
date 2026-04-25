# FIPS Testing

Integration and simulation test harnesses for FIPS, using Docker
containers running the full protocol stack.

## Test Harnesses

### [static/](static/) -- Static Docker Network

Fixed topologies with manual scripts for building, config generation,
connectivity tests (ping, iperf), and network impairment (netem).
Useful for deterministic debugging and validating specific topology
configurations.

| Topology    | Nodes | Transport | Description                      |
| ----------- | ----- | --------- | -------------------------------- |
| mesh        | 5     | UDP       | Sparse mesh, 6 links, multi-hop  |
| chain       | 5     | UDP       | Linear chain, max 4-hop paths    |
| mesh-public | 5+1   | UDP       | Mesh with external public node   |
| tcp-chain   | 3     | TCP       | Linear chain over TCP (port 8443) |
| rekey       | 5     | UDP       | Rekey integration test topology  |

### [tor/](tor/) -- Tor Transport Integration

End-to-end Tor transport testing with Docker containers running real
Tor daemons. Requires internet access for Tor bootstrapping.

| Scenario       | Description                                              |
| -------------- | -------------------------------------------------------- |
| socks5-outbound | Outbound SOCKS5 connections through Tor to clearnet peer |
| directory-mode  | Inbound via HiddenServiceDir onion service (co-located)  |

### [chaos/](chaos/) -- Stochastic Simulation

Automated network testing with configurable node counts, topology
algorithms (random geometric, Erdos-Renyi, chain, explicit), and fault
injection (netem mutation, link flaps, traffic generation, node
churn). 20 scenarios covering general stress testing, cost-based parent
selection, mixed link technologies (fiber/Bluetooth/WiFi),
transport-specific validation (UDP, TCP, Ethernet), and ECN/congestion
testing. Scenarios are
defined in YAML and executed via a Python harness that manages the full
lifecycle: topology generation, Docker orchestration, fault scheduling,
log collection, and analysis.

### [ble/](ble/) -- BLE Transport Hardware Validation

Hardware-in-the-loop BLE testing using real Bluetooth Low Energy adapters.
Requires physical BLE hardware (Linux with BlueZ or macOS with
CoreBluetooth). Includes a standalone spike test (`ble_spike.rs`) for
verifying BLE hardware/stack setup, and integrates with the chaos
simulation harness via native (non-Docker) process runner.

| Scenario | Nodes | Description |
| -------- | ----- | ----------- |
| `ble-smoke` | 2 | Basic BLE connectivity validation (30s) |
| `ble-only` | 4 | BLE-only ring topology with traffic |
| `ble-cost` | 4 | Cost-based parent selection over heterogeneous links |
| `ble-mesh` | 6 | Mixed UDP + BLE mesh |

BLE transport logic is fully tested via `MockBleIo` unit tests in CI
(no hardware needed). The scenarios above validate against real BLE radios.
