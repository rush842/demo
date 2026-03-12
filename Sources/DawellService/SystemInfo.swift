import Foundation
import IOKit
import SystemConfiguration
#if canImport(AppKit)
import AppKit
#endif

struct SystemInfo: Codable {
    var machineId: String
    var macAddress: String
    var ipAddress: String
    var hostname: String
    var operatingSystem: String
    var osVersion: String
    var cpuModel: String
    var cpuCores: Int
    var totalRam: String
    var screenResolution: String

    enum CodingKeys: String, CodingKey {
        case machineId = "machineid"
        case macAddress = "macaddress"
        case ipAddress = "ipaddress"
        case hostname
        case operatingSystem = "operatingsystem"
        case osVersion = "os_version"
        case cpuModel = "cpu_model"
        case cpuCores = "cpu_core"
        case totalRam = "totalram"
        case screenResolution = "screenresolution"
    }

    static func collect() -> SystemInfo {
        SystemInfo(
            machineId: getMachineId(),
            macAddress: getMacAddress(),
            ipAddress: getIPAddress(),
            hostname: Host.current().localizedName ?? ProcessInfo.processInfo.hostName,
            operatingSystem: "Darwin",
            osVersion: ProcessInfo.processInfo.operatingSystemVersionString,
            cpuModel: getCpuModel(),
            cpuCores: ProcessInfo.processInfo.processorCount,
            totalRam: getTotalRam(),
            screenResolution: getScreenResolution()
        )
    }
}

private func getMachineId() -> String {
    let service = IOServiceGetMatchingService(kIOMainPortDefault,
        IOServiceMatching("IOPlatformExpertDevice"))
    defer { IOObjectRelease(service) }
    let uuid = IORegistryEntryCreateCFProperty(service,
        "IOPlatformUUID" as CFString, kCFAllocatorDefault, 0)
    return (uuid?.takeRetainedValue() as? String) ?? UUID().uuidString
}

private func getMacAddress() -> String {
    var ifaddrs: UnsafeMutablePointer<ifaddrs>?
    guard getifaddrs(&ifaddrs) == 0 else { return "00:00:00:00:00:00" }
    defer { freeifaddrs(ifaddrs) }
    var ptr = ifaddrs
    while let p = ptr {
        let name = String(cString: p.pointee.ifa_name)
        if name == "en0", p.pointee.ifa_addr.pointee.sa_family == UInt8(AF_LINK) {
            var mac = [UInt8](repeating: 0, count: 6)
            let sdl = p.pointee.ifa_addr.withMemoryRebound(to: sockaddr_dl.self, capacity: 1) { $0 }
            withUnsafePointer(to: sdl.pointee.sdl_data) { ptr in
                let offset = Int(sdl.pointee.sdl_nlen)
                for i in 0..<6 { mac[i] = ptr.withMemoryRebound(to: UInt8.self, capacity: 6 + offset) { $0[offset + i] } }
            }
            return mac.map { String(format: "%02X", $0) }.joined(separator: ":")
        }
        ptr = p.pointee.ifa_next
    }
    return "00:00:00:00:00:00"
}

private func getIPAddress() -> String {
    var ifaddrs: UnsafeMutablePointer<ifaddrs>?
    guard getifaddrs(&ifaddrs) == 0 else { return "0.0.0.0" }
    defer { freeifaddrs(ifaddrs) }
    var ptr = ifaddrs
    while let p = ptr {
        let name = String(cString: p.pointee.ifa_name)
        if name == "en0", p.pointee.ifa_addr.pointee.sa_family == UInt8(AF_INET) {
            var addr = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
            p.pointee.ifa_addr.withMemoryRebound(to: sockaddr_in.self, capacity: 1) {
                inet_ntop(AF_INET, &$0.pointee.sin_addr, &addr, socklen_t(INET_ADDRSTRLEN))
            }
            return String(cString: addr)
        }
        ptr = p.pointee.ifa_next
    }
    return "0.0.0.0"
}

private func getCpuModel() -> String {
    var size = 0
    sysctlbyname("machdep.cpu.brand_string", nil, &size, nil, 0)
    var brand = [CChar](repeating: 0, count: size)
    sysctlbyname("machdep.cpu.brand_string", &brand, &size, nil, 0)
    let result = String(cString: brand)
    if result.isEmpty {
        // Apple Silicon
        var hwSize = 0
        sysctlbyname("hw.model", nil, &hwSize, nil, 0)
        var hw = [CChar](repeating: 0, count: hwSize)
        sysctlbyname("hw.model", &hw, &hwSize, nil, 0)
        return String(cString: hw)
    }
    return result
}

private func getTotalRam() -> String {
    var size: UInt64 = 0
    var len = MemoryLayout<UInt64>.size
    sysctlbyname("hw.memsize", &size, &len, nil, 0)
    let gb = Double(size) / 1_073_741_824.0
    return String(format: "%.2f GB", gb)
}

private func getScreenResolution() -> String {
    #if canImport(AppKit)
    if let screen = NSScreen.main {
        let w = Int(screen.frame.width * screen.backingScaleFactor)
        let h = Int(screen.frame.height * screen.backingScaleFactor)
        return "\(w)x\(h)"
    }
    #endif
    return "0x0"
}
