import Foundation

// Build-time embedded URLs (set via environment at compile time)
let kApiBaseURL = ProcessInfo.processInfo.environment["DAWELLSERVICE_API_BASE_URL"]
    ?? "http://192.168.1.40:4000/api/client"
let kWsURL = ProcessInfo.processInfo.environment["DAWELLSERVICE_WS_URL"]
    ?? "ws://192.168.1.40:4000/ws"

struct ServiceConfig: Codable {
    var userId: Int
    var organizationId: Int
    var apiBaseUrl: String
    var wsUrl: String

    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case organizationId = "organization_id"
        case apiBaseUrl = "api_base_url"
        case wsUrl = "ws_url"
    }
}

struct TokenPayload: Decodable {
    let userId: Int
    let organizationId: Int
    enum CodingKeys: String, CodingKey {
        case userId = "user_id"
        case organizationId = "organization_id"
    }
}

func configDir() -> URL {
    let home = FileManager.default.homeDirectoryForCurrentUser
    return home.appendingPathComponent("Library/Application Support/DawellService")
}

func configFile() -> URL {
    configDir().appendingPathComponent("config.json")
}

func saveConfig(_ config: ServiceConfig) throws {
    try FileManager.default.createDirectory(at: configDir(), withIntermediateDirectories: true)
    let data = try JSONEncoder().encode(config)
    try data.write(to: configFile())
}

func loadConfig() -> ServiceConfig? {
    guard let data = try? Data(contentsOf: configFile()) else { return nil }
    return try? JSONDecoder().decode(ServiceConfig.self, from: data)
}

func deleteConfig() {
    try? FileManager.default.removeItem(at: configDir())
}

func decodeToken(_ token: String) throws -> (userId: Int, organizationId: Int) {
    var base64 = token
        .replacingOccurrences(of: "-", with: "+")
        .replacingOccurrences(of: "_", with: "/")
    let rem = base64.count % 4
    if rem > 0 { base64 += String(repeating: "=", count: 4 - rem) }
    guard let data = Data(base64Encoded: base64),
          let payload = try? JSONDecoder().decode(TokenPayload.self, from: data) else {
        throw NSError(domain: "DawellService", code: 1, userInfo: [NSLocalizedDescriptionKey: "Invalid token"])
    }
    return (payload.userId, payload.organizationId)
}
