import Foundation
import CoreBluetooth

let SERVICE_UUID = CBUUID(string: "9C90B790-2CC5-42C0-9F87-C9CC40648F4C")
let LOG_FILE = "/tmp/fips-l2cap.log"
let LOG_MAX_SIZE: UInt64 = 10 * 1024 * 1024  // 10MB

enum LogLevel {
    case info
    case debug
    case error
}

class Logger {
    private static let lock = NSLock()
    private static let formatter = ISO8601DateFormatter()

    static func write(_ level: LogLevel, _ message: String) {
        let timestamp = formatter.string(from: Date())
        let levelStr: String
        switch level {
        case .info: levelStr = "INFO"
        case .debug: levelStr = "DEBUG"
        case .error: levelStr = "ERROR"
        }
        
        let line = "[\(timestamp)] [\(levelStr)] \(message)\n"
        print(line.trimmingCharacters(in: .newlines))

        guard let data = line.data(using: .utf8) else { return }

        lock.lock()
        defer { lock.unlock() }

        rotateIfNeeded()
        ensureLogFileExists()

        guard let handle = FileHandle(forWritingAtPath: LOG_FILE) else {
            print("Failed to open log file for writing: \(LOG_FILE)")
            return
        }
        defer { handle.closeFile() }

        handle.seekToEndOfFile()
        handle.write(data)
    }

    private static func ensureLogFileExists() {
        if !FileManager.default.fileExists(atPath: LOG_FILE) {
            FileManager.default.createFile(atPath: LOG_FILE, contents: nil)
        }
    }

    private static func rotateIfNeeded() {
        guard let attrs = try? FileManager.default.attributesOfItem(atPath: LOG_FILE),
              let fileSize = attrs[.size] as? UInt64,
              fileSize > LOG_MAX_SIZE else {
            return
        }

        try? FileManager.default.removeItem(atPath: LOG_FILE)
        FileManager.default.createFile(atPath: LOG_FILE, contents: nil)
        let marker = "[\(formatter.string(from: Date()))] [INFO] === LOG ROTATED (restarted) ===\n"
        if let markerData = marker.data(using: .utf8),
           let handle = FileHandle(forWritingAtPath: LOG_FILE) {
            defer { handle.closeFile() }
            handle.seekToEndOfFile()
            handle.write(markerData)
        }
    }

    static func close() {
    }
}

func logInfo(_ message: String) { Logger.write(.info, message) }
func logDebug(_ message: String) { Logger.write(.debug, message) }
func logError(_ message: String) { Logger.write(.error, message) }

// FMP Constants
let FMP_PREFIX_SIZE = 4
let FMP_PHASE_ESTABLISHED: UInt8 = 0x0
let FMP_PHASE_MSG1: UInt8 = 0x1
let FMP_PHASE_MSG2: UInt8 = 0x2
let FMP_ESTABLISHED_REMAINING_HEADER = 12
let FMP_AEAD_TAG_SIZE = 16
let FMP_MSG1_PAYLOAD_LEN: UInt16 = 110
let FMP_MSG2_PAYLOAD_LEN: UInt16 = 65

class L2CAPChannelHandler: NSObject, StreamDelegate {
    var inputStream: InputStream?
    var outputStream: OutputStream?
    var tcpInputStream: InputStream?
    var tcpOutputStream: OutputStream?
    var onClosed: (() -> Void)?
    private var closed = false
    private let readQueue = DispatchQueue(label: "com.fips.tcp-reader", qos: .utility)
    
    func open(channel: CBL2CAPChannel) {
        inputStream = channel.inputStream
        outputStream = channel.outputStream

        inputStream?.delegate = self
        inputStream?.schedule(in: .main, forMode: .default)
        inputStream?.open()

        outputStream?.delegate = self
        outputStream?.schedule(in: .main, forMode: .default)
        outputStream?.open()

        logInfo("=== L2CAP CHANNEL OPENED ===")
    }
    
    func connectTCP(host: String, port: Int) {
        logInfo("Connecting to TCP \(host):\(port)")
        
        var input: InputStream?
        var output: OutputStream?
        
        Stream.getStreamsToHost(withName: host, port: port, inputStream: &input, outputStream: &output)
        
        guard let tcpIn = input, let tcpOut = output else {
            logError("TCP: Failed to create streams")
            close()
            return
        }
        
        tcpInputStream = tcpIn
        tcpOutputStream = tcpOut
        
        tcpIn.delegate = self
        tcpIn.schedule(in: .main, forMode: .default)
        tcpIn.open()
        
        tcpOut.delegate = self
        tcpOut.schedule(in: .main, forMode: .default)
        tcpOut.open()
        
        logInfo("TCP connected to \(host):\(port)")
        
        // Start TCP reader thread
        startTCPReader()
    }
    
    private func startTCPReader() {
        readQueue.async { [weak self] in
            self?.readTCPLoop()
        }
    }
    
    private func readTCPLoop() {
        guard let tcpIn = tcpInputStream else { return }
        
        while !closed {
            // Read 4-byte FMP prefix
            var prefix = [UInt8](repeating: 0, count: FMP_PREFIX_SIZE)
            let prefixRead = tcpIn.read(&prefix, maxLength: FMP_PREFIX_SIZE)
            
            if prefixRead <= 0 {
                if !closed {
                    logError("TCP→BLE: Failed to read prefix (read \(prefixRead) bytes)")
                    DispatchQueue.main.async { [weak self] in self?.close() }
                }
                return
            }
            
            if prefixRead < FMP_PREFIX_SIZE {
                logError("TCP→BLE: Incomplete prefix (\(prefixRead)/\(FMP_PREFIX_SIZE) bytes)")
                DispatchQueue.main.async { [weak self] in self?.close() }
                return
            }
            
            // Parse FMP header
            let version = prefix[0] >> 4
            let phase = prefix[0] & 0x0F
            let payloadLen = UInt16(prefix[2]) | (UInt16(prefix[3]) << 8)
            
            if version != 0 {
                logError("TCP→BLE: Unknown FMP version: \(version)")
                DispatchQueue.main.async { [weak self] in self?.close() }
                return
            }
            
            // Calculate remaining bytes based on phase
            let remaining: Int
            switch phase {
            case FMP_PHASE_ESTABLISHED:
                remaining = FMP_ESTABLISHED_REMAINING_HEADER + Int(payloadLen) + FMP_AEAD_TAG_SIZE
            case FMP_PHASE_MSG1:
                remaining = Int(payloadLen)
            case FMP_PHASE_MSG2:
                remaining = Int(payloadLen)
            default:
                logError("TCP→BLE: Unknown phase: \(phase)")
                DispatchQueue.main.async { [weak self] in self?.close() }
                return
            }
            
            // Read remaining bytes
            var remainingBytes = [UInt8](repeating: 0, count: remaining)
            var totalRead = 0
            while totalRead < remaining {
                let read = tcpIn.read(&remainingBytes, maxLength: remaining - totalRead)
                if read <= 0 {
                    if !closed {
                        logError("TCP→BLE: Failed to read remaining bytes")
                        DispatchQueue.main.async { [weak self] in self?.close() }
                    }
                    return
                }
                totalRead += read
            }
            
            // Combine prefix + remaining into complete packet
            var packet = prefix
            packet.append(contentsOf: remainingBytes)
            
            // Forward to BLE
            let totalSize = packet.count
            let packetData = Data(packet)
            let phaseNum = phase
            DispatchQueue.main.async { [weak self] in
                guard let bleOut = self?.outputStream else { return }
                var packetBytes = [UInt8](packetData)
                let written = bleOut.write(&packetBytes, maxLength: totalSize)
                if written != totalSize {
                    logError("TCP→BLE: Write failed (wrote \(written)/\(totalSize) bytes)")
                    self?.close()
                } else {
                    let phaseName = phaseNum == 0 ? "established" : phaseNum == 1 ? "msg1" : "msg2"
                    logInfo("TCP→BLE: \(totalSize) bytes (phase \(phaseName))")
                    logHexDump(data: packetData, prefix: "TCP→BLE")
                }
            }
        }
    }
    
    func stream(_ stream: Stream, handle eventCode: Stream.Event) {
        if stream == inputStream {
            switch eventCode {
            case .hasBytesAvailable:
                readAndForward()
            case .errorOccurred:
                logError("BLE RX ERROR: \(stream.streamError?.localizedDescription ?? "unknown")")
                close()
            case .endEncountered:
                logInfo("BLE RX CLOSED: stream ended")
                close()
            default:
                break
            }
        } else if stream == tcpInputStream {
            // TCP stream events handled in readTCPLoop
            switch eventCode {
            case .errorOccurred:
                logError("TCP RX ERROR: \(stream.streamError?.localizedDescription ?? "unknown")")
                close()
            case .endEncountered:
                logInfo("TCP RX CLOSED: stream ended")
                close()
            default:
                break
            }
        }
    }
    
    private func readAndForward() {
        guard let input = inputStream, let tcpOut = tcpOutputStream else { return }
        var buffer = [UInt8](repeating: 0, count: 4096)

        let bytesRead = input.read(&buffer, maxLength: buffer.count)
        if bytesRead > 0 {
            let data = Data(bytes: buffer, count: bytesRead)
            
            // Forward directly to TCP (raw forwarding)
            let written = tcpOut.write(buffer, maxLength: bytesRead)
            
            if written != bytesRead {
                logError("BLE→TCP: Write failed (wrote \(written)/\(bytesRead) bytes)")
                close()
            } else {
                logInfo("BLE→TCP: \(bytesRead) bytes")
                logHexDump(data: data, prefix: "BLE→TCP")
            }
        }
    }
    
    func close() {
        if closed {
            return
        }
        closed = true

        inputStream?.delegate = nil
        outputStream?.delegate = nil
        inputStream?.remove(from: .main, forMode: .default)
        outputStream?.remove(from: .main, forMode: .default)
        inputStream?.close()
        outputStream?.close()
        
        tcpInputStream?.delegate = nil
        tcpOutputStream?.delegate = nil
        tcpInputStream?.remove(from: .main, forMode: .default)
        tcpOutputStream?.remove(from: .main, forMode: .default)
        tcpInputStream?.close()
        tcpOutputStream?.close()
        
        logInfo("=== L2CAP CHANNEL CLOSED ===")
        onClosed?()
    }
}

func logHexDump(data: Data, prefix: String) {
    let bytes = [UInt8](data.prefix(16))
    let hex = bytes.map { String(format: "%02x", $0) }.joined(separator: " ")
    logDebug("\(prefix) hex: \(hex)")
}

class PeripheralManager: NSObject, CBPeripheralManagerDelegate {
    var peripheralManager: CBPeripheralManager!
    var publishedPSM: UInt16 = 0
    var service: CBMutableService!
    var channelHandler: L2CAPChannelHandler?
    var openChannel: CBL2CAPChannel?
    var daemonHost: String = "127.0.0.1"
    var daemonPort: Int = 4443
    
    override init() {
        super.init()
        peripheralManager = CBPeripheralManager(delegate: self, queue: nil)
        logInfo("=== FIPS L2CAP PERIPHERAL STARTED ===")
    }
    
    func peripheralManagerDidUpdateState(_ peripheral: CBPeripheralManager) {
        logInfo("Bluetooth state: \(peripheral.state.rawValue)")
        switch peripheral.state {
        case .poweredOn:
            logInfo("Bluetooth ON - creating service and publishing L2CAP channel")

            // Create the service
            service = CBMutableService(type: SERVICE_UUID, primary: true)
            peripheral.add(service)

            // Publish L2CAP channel
            peripheral.publishL2CAPChannel(withEncryption: false)

        case .poweredOff:
            logInfo("Bluetooth OFF")
        case .unauthorized:
            logInfo("Bluetooth UNAUTHORIZED")
        case .unsupported:
            logInfo("Bluetooth NOT SUPPORTED")
        default:
            logInfo("Bluetooth state: \(peripheral.state.rawValue)")
        }
    }
    
    func peripheralManager(_ peripheral: CBPeripheralManager, didAdd service: CBService, error: Error?) {
        if let error = error {
            logError("Service ADD FAILED: \(error.localizedDescription)")
        } else {
            logInfo("Service added: \(service.uuid)")
        }
    }
    
    func peripheralManager(_ peripheral: CBPeripheralManager, didPublishL2CAPChannel PSM: UInt16, error: Error?) {
        if let error = error {
            logError("L2CAP PUBLISH FAILED: \(error.localizedDescription)")
            exit(1)
        }
        publishedPSM = PSM
        logInfo("L2CAP channel published with PSM: \(PSM)")

        // Start advertising
        let advertisementData: [String: Any] = [
            CBAdvertisementDataServiceUUIDsKey: [SERVICE_UUID],
            CBAdvertisementDataLocalNameKey: "FIPS-L2CAP"
        ]
        peripheral.startAdvertising(advertisementData)
        logInfo("Started advertising service UUID: \(SERVICE_UUID)")
    }
    
    func peripheralManagerDidStartAdvertising(_ peripheral: CBPeripheralManager, error: Error?) {
        if let error = error {
            logError("Advertising FAILED: \(error.localizedDescription)")
        } else {
            logInfo("Advertising started - waiting for L2CAP connections...")
            logInfo("PSM: \(publishedPSM) | Service UUID: \(SERVICE_UUID)")
        }
    }
    
    func peripheralManager(_ peripheral: CBPeripheralManager, didOpen channel: CBL2CAPChannel?, error: Error?) {
        if let error = error {
            logError("L2CAP OPEN FAILED: \(error.localizedDescription)")
            return
        }

        guard let channel = channel else {
            logError("L2CAP channel is NIL")
            return
        }

        let peerInfo = channel.peer?.description ?? "unknown"
        logInfo("=== L2CAP CONNECTION ESTABLISHED ===")
        logInfo("Peer: \(peerInfo)")

        if let existingHandler = channelHandler {
            logInfo("Closing previous L2CAP channel handler before accepting new connection")
            existingHandler.close()
        }

        openChannel = channel

        let handler = L2CAPChannelHandler()
        handler.onClosed = { [weak self] in
            self?.openChannel = nil
            self?.channelHandler = nil
        }
        channelHandler = handler
        channelHandler?.open(channel: channel)
        
        handler.connectTCP(host: daemonHost, port: daemonPort)
    }
    
    func peripheralManager(_ peripheral: CBPeripheralManager, didReceiveRead request: CBATTRequest) {
        logInfo("GATT READ: \(request.characteristic.uuid)")
        peripheral.respond(to: request, withResult: .success)
    }
}

let manager = PeripheralManager()

if CommandLine.arguments.count > 1 {
    let addr = CommandLine.arguments[1]
    if let colonIdx = addr.range(of: ":") {
        manager.daemonHost = String(addr[..<colonIdx.lowerBound])
        if let port = Int(String(addr[colonIdx.upperBound...])) {
            manager.daemonPort = port
        }
    } else {
        manager.daemonHost = addr
    }
    logInfo("Daemon address: \(manager.daemonHost):\(manager.daemonPort)")
}

RunLoop.main.run()
