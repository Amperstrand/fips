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

class L2CAPChannelHandler: NSObject, StreamDelegate {
    var inputStream: InputStream?
    var outputStream: OutputStream?
    var onClosed: (() -> Void)?
    private var closed = false
    
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
    
    func stream(_ stream: Stream, handle eventCode: Stream.Event) {
        if stream == inputStream {
            switch eventCode {
            case .hasBytesAvailable:
                readAndEcho()
            case .errorOccurred:
                logError("RX ERROR: \(stream.streamError?.localizedDescription ?? "unknown")")
                close()
            case .endEncountered:
                logInfo("RX CLOSED: stream ended")
                close()
            default:
                break
            }
        }
    }
    
    private func readAndEcho() {
        guard let input = inputStream else { return }
        var buffer = [UInt8](repeating: 0, count: 1024)

        let bytesRead = input.read(&buffer, maxLength: buffer.count)
        if bytesRead > 0 {
            let data = Data(bytes: buffer, count: bytesRead)
            if let message = String(data: data, encoding: .utf8) {
                let msg = message.trimmingCharacters(in: .newlines)
                logDebug("RX: \(msg) (\(bytesRead) bytes)")
            } else {
                logDebug("RX: binary data (\(bytesRead) bytes)")
            }

            // Echo back with "PONG" if we received "PING"
            if let output = outputStream {
                let response: Data
                if let str = String(data: data, encoding: .utf8), str.trimmingCharacters(in: .newlines).uppercased() == "PING" {
                    response = "PONG\n".data(using: .utf8)!
                    logInfo("TX: PONG")
                } else {
                    response = data
                    logDebug("TX: echo (\(bytesRead) bytes)")
                }
                _ = response.withUnsafeBytes { ptr in
                    output.write(ptr.baseAddress!.assumingMemoryBound(to: UInt8.self), maxLength: response.count)
                }
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
        logInfo("=== L2CAP CHANNEL CLOSED ===")
        onClosed?()
    }
}

class PeripheralManager: NSObject, CBPeripheralManagerDelegate {
    var peripheralManager: CBPeripheralManager!
    var publishedPSM: UInt16 = 0
    var service: CBMutableService!
    var channelHandler: L2CAPChannelHandler?
    var openChannel: CBL2CAPChannel?
    
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
    }
    
    func peripheralManager(_ peripheral: CBPeripheralManager, didReceiveRead request: CBATTRequest) {
        logInfo("GATT READ: \(request.characteristic.uuid)")
        peripheral.respond(to: request, withResult: .success)
    }
}

let manager = PeripheralManager()
RunLoop.main.run()
