import Foundation

struct ApiClient {
    let session: URLSession
    let baseURL: String

    init(baseURL: String) {
        self.baseURL = baseURL
        let cfg = URLSessionConfiguration.default
        cfg.timeoutIntervalForRequest = 30
        self.session = URLSession(configuration: cfg)
    }

    // MARK: - Register / Heartbeat

    func register(config: ServiceConfig, sysInfo: SystemInfo) async throws {
        let url = URL(string: "\(baseURL)/dawell360-installations")!
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")

        var body: [String: Any] = [
            "user_id": config.userId,
            "organization_id": config.organizationId,
            "machineid": sysInfo.machineId,
            "macaddress": sysInfo.macAddress,
            "ipaddress": sysInfo.ipAddress,
            "hostname": sysInfo.hostname,
            "operatingsystem": sysInfo.operatingSystem,
            "os_version": sysInfo.osVersion,
            "cpu_model": sysInfo.cpuModel,
            "cpu_core": sysInfo.cpuCores,
            "totalram": sysInfo.totalRam,
            "screenresolution": sysInfo.screenResolution,
            "status": "online"
        ]
        req.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (_, _) = try await session.data(for: req)
    }

    func heartbeat(config: ServiceConfig, sysInfo: SystemInfo) async throws -> String? {
        let url = URL(string: "\(baseURL)/dawell360-installations/heartbeat")!
        var req = URLRequest(url: url)
        req.httpMethod = "PUT"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let body: [String: Any] = [
            "macaddress": sysInfo.macAddress,
            "status": "online",
            "ipaddress": sysInfo.ipAddress
        ]
        req.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (data, _) = try await session.data(for: req)
        if let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let url = json["update_url"] as? String {
            return url
        }
        return nil
    }

    func setOffline(macAddress: String) async {
        guard let url = URL(string: "\(baseURL)/dawell360-installations") else { return }
        var req = URLRequest(url: url)
        req.httpMethod = "PUT"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let body: [String: Any] = ["macaddress": macAddress, "status": "offline"]
        req.httpBody = try? JSONSerialization.data(withJSONObject: body)
        let _ = try? await session.data(for: req)
    }

    // MARK: - Screenshot Upload

    func uploadScreenshot(jpegData: Data, macAddress: String, organizationId: Int) async throws {
        let url = URL(string: "\(baseURL)/screenshots")!
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        let boundary = "Boundary-\(UUID().uuidString)"
        req.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")

        var body = Data()
        let fields: [(String, String)] = [
            ("macaddress", macAddress),
            ("organization_id", "\(organizationId)")
        ]
        for (name, value) in fields {
            body.append("--\(boundary)\r\n".data(using: .utf8)!)
            body.append("Content-Disposition: form-data; name=\"\(name)\"\r\n\r\n".data(using: .utf8)!)
            body.append("\(value)\r\n".data(using: .utf8)!)
        }
        body.append("--\(boundary)\r\n".data(using: .utf8)!)
        body.append("Content-Disposition: form-data; name=\"screenshot\"; filename=\"screenshot.jpg\"\r\n".data(using: .utf8)!)
        body.append("Content-Type: image/jpeg\r\n\r\n".data(using: .utf8)!)
        body.append(jpegData)
        body.append("\r\n--\(boundary)--\r\n".data(using: .utf8)!)
        req.httpBody = body

        let (_, _) = try await session.data(for: req)
    }

    // MARK: - Video Upload

    func uploadVideo(videoData: Data, macAddress: String, organizationId: Int, mimeType: String, filename: String) async throws {
        let url = URL(string: "\(baseURL)/video-recordings")!
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        let boundary = "Boundary-\(UUID().uuidString)"
        req.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
        req.timeoutInterval = 300

        var body = Data()
        let fields: [(String, String)] = [
            ("macaddress", macAddress),
            ("organization_id", "\(organizationId)")
        ]
        for (name, value) in fields {
            body.append("--\(boundary)\r\n".data(using: .utf8)!)
            body.append("Content-Disposition: form-data; name=\"\(name)\"\r\n\r\n".data(using: .utf8)!)
            body.append("\(value)\r\n".data(using: .utf8)!)
        }
        body.append("--\(boundary)\r\n".data(using: .utf8)!)
        body.append("Content-Disposition: form-data; name=\"video\"; filename=\"\(filename)\"\r\n".data(using: .utf8)!)
        body.append("Content-Type: \(mimeType)\r\n\r\n".data(using: .utf8)!)
        body.append(videoData)
        body.append("\r\n--\(boundary)--\r\n".data(using: .utf8)!)
        req.httpBody = body

        let (_, _) = try await session.data(for: req)
    }

    // MARK: - Input Log Upload

    func uploadInputLog(payload: [[String: Any]], macAddress: String, organizationId: Int) async throws {
        let url = URL(string: "\(baseURL)/input-logs")!
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let body: [String: Any] = [
            "macaddress": macAddress,
            "organization_id": organizationId,
            "logs": payload
        ]
        req.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (_, _) = try await session.data(for: req)
    }

    // MARK: - Activity Upload

    func uploadActivity(payload: [String: Any]) async throws {
        let url = URL(string: "\(baseURL)/activity-logs")!
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = try JSONSerialization.data(withJSONObject: payload)
        let (_, _) = try await session.data(for: req)
    }

    // MARK: - Settings

    func fetchCaptureSettings(organizationId: Int) async -> CaptureSettings {
        guard let url = URL(string: "\(baseURL)/capture-settings/\(organizationId)") else {
            return CaptureSettings()
        }
        guard let (data, _) = try? await session.data(from: url),
              let s = try? JSONDecoder().decode(CaptureSettings.self, from: data) else {
            return CaptureSettings()
        }
        return s
    }

    func fetchMonitoringSettings(organizationId: Int) async -> MonitoringSettings {
        guard let url = URL(string: "\(baseURL)/monitoring-settings/\(organizationId)") else {
            return MonitoringSettings()
        }
        guard let (data, _) = try? await session.data(from: url),
              let s = try? JSONDecoder().decode(MonitoringSettings.self, from: data) else {
            return MonitoringSettings()
        }
        return s
    }
}

struct CaptureSettings: Decodable {
    var screenshotEnabled: Bool = true
    var screenshotInterval: Int = 5
    var screenshotQuality: String = "medium"
    var videoEnabled: Bool = true
    var videoDuration: Int = 300
    var keystrokeLogging: Bool = true
    var clipboardMonitoring: Bool = true

    enum CodingKeys: String, CodingKey {
        case screenshotEnabled = "screenshot_enabled"
        case screenshotInterval = "screenshot_interval"
        case screenshotQuality = "screenshot_quality"
        case videoEnabled = "video_enabled"
        case videoDuration = "video_clip_duration"
        case keystrokeLogging = "keystroke_logging"
        case clipboardMonitoring = "clipboard_monitoring"
    }
}

struct MonitoringSettings: Decodable {
    var activityTracking: Bool = true
    var appUsage: Bool = true

    enum CodingKeys: String, CodingKey {
        case activityTracking = "activity_tracking"
        case appUsage = "app_usage"
    }
}
