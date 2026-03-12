import Foundation

actor ScreenshotModule {
    let api: ApiClient
    let macAddress: String
    let organizationId: Int

    init(api: ApiClient, macAddress: String, organizationId: Int) {
        self.api = api
        self.macAddress = macAddress
        self.organizationId = organizationId
    }

    func run(config: ServiceConfig) async {
        // Wait before first screenshot
        try? await Task.sleep(nanoseconds: 5_000_000_000)

        while !Task.isCancelled {
            let settings = await api.fetchCaptureSettings(organizationId: organizationId)
            guard settings.screenshotEnabled else {
                try? await Task.sleep(nanoseconds: 30_000_000_000)
                continue
            }

            do {
                let jpeg = try await captureScreenshot(quality: settings.screenshotQuality)
                try await api.uploadScreenshot(jpegData: jpeg, macAddress: macAddress, organizationId: organizationId)
                NSLog("[DawellService] Screenshot uploaded (\(jpeg.count) bytes)")
            } catch {
                NSLog("[DawellService] Screenshot failed: \(error)")
            }

            let waitSecs = UInt64(max(1, settings.screenshotInterval) * 60)
            try? await Task.sleep(nanoseconds: waitSecs * 1_000_000_000)
        }
    }
}
