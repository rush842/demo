import Foundation

actor WebSocketClient {
    private var task: URLSessionWebSocketTask?
    private let session = URLSession(configuration: .default)
    private var onMessage: ((String) -> Void)?
    private var isConnected = false

    func connect(url: String, onMessage: @escaping (String) -> Void) {
        self.onMessage = onMessage
        guard let wsURL = URL(string: url) else { return }
        task = session.webSocketTask(with: wsURL)
        task?.resume()
        isConnected = true
        receive()
    }

    func send(_ message: String) async throws {
        try await task?.send(.string(message))
    }

    func disconnect() {
        isConnected = false
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
    }

    private func receive() {
        task?.receive { [weak self] result in
            guard let self = self else { return }
            switch result {
            case .success(let msg):
                Task {
                    let str: String
                    switch msg {
                    case .string(let s): str = s
                    case .data(let d): str = String(data: d, encoding: .utf8) ?? ""
                    @unknown default: str = ""
                    }
                    await self.handleMessage(str)
                    let connected = await self.isConnected
                    if connected { await self.receive() }
                }
            case .failure:
                Task { await self.scheduleReconnect() }
            }
        }
    }

    private func handleMessage(_ str: String) {
        onMessage?(str)
    }

    private func scheduleReconnect() {
        // Reconnect logic handled by caller
    }
}

// MARK: - WebSocket Registration

func registerViaWebSocket(config: ServiceConfig, sysInfo: SystemInfo) async throws {
    let client = WebSocketClient()
    var registered = false
    var error: Error?

    await client.connect(url: config.wsUrl) { msg in }

    let payload: [String: Any] = [
        "type": "register_desktop",
        "payload": [
            "machineId": sysInfo.machineId,
            "macAddress": sysInfo.macAddress,
            "ipAddress": sysInfo.ipAddress,
            "hostname": sysInfo.hostname,
            "operatingSystem": sysInfo.operatingSystem,
            "osVersion": sysInfo.osVersion,
            "cpuModel": sysInfo.cpuModel,
            "cpuCore": sysInfo.cpuCores,
            "totalram": sysInfo.totalRam,
            "screenresolution": sysInfo.screenResolution,
            "userId": config.userId,
            "organizationId": config.organizationId
        ] as [String: Any]
    ]

    guard let data = try? JSONSerialization.data(withJSONObject: payload),
          let json = String(data: data, encoding: .utf8) else { return }

    try await client.send(json)
    try await Task.sleep(nanoseconds: 1_000_000_000)
    await client.disconnect()
}

func unregisterViaWebSocket(config: ServiceConfig, sysInfo: SystemInfo) async {
    let client = WebSocketClient()
    await client.connect(url: config.wsUrl) { _ in }
    let payload: [String: Any] = [
        "type": "unregister_desktop",
        "payload": [
            "machineId": sysInfo.machineId,
            "macAddress": sysInfo.macAddress,
            "userId": config.userId,
            "organizationId": config.organizationId
        ] as [String: Any]
    ]
    if let data = try? JSONSerialization.data(withJSONObject: payload),
       let json = String(data: data, encoding: .utf8) {
        try? await client.send(json)
        try? await Task.sleep(nanoseconds: 500_000_000)
    }
    await client.disconnect()
}
