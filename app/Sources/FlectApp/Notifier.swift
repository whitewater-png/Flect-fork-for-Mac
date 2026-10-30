import AppKit
import UserNotifications

/// "Run finished" notifications. Permission is asked for lazily, the first time there is something to report,
/// and every path degrades to doing nothing: denied permission, a build run outside an .app bundle
/// (UNUserNotificationCenter raises an Objective-C exception there), or a failed request.
final class Notifier: NSObject, UNUserNotificationCenterDelegate, @unchecked Sendable {
    static let shared = Notifier()

    /// Called on the main thread when the user clicks a notification (the run's id, if it created files).
    var onOpen: ((UUID?) -> Void)?
    /// Called on the main thread when the user clicks a clipboard-watch notification (the address found).
    var onOpenDownload: ((String) -> Void)?

    /// Only a real, bundled app has a notification center.
    static var isAvailable: Bool {
        Bundle.main.bundleIdentifier != nil && Bundle.main.bundleURL.pathExtension == "app"
    }

    /// Makes clicks on a notification reach the app even when it was launched by the click. Does not ask for permission.
    func attach() {
        guard Notifier.isAvailable else { return }
        UNUserNotificationCenter.current().delegate = self
    }

    func post(title: String, body: String, recordID: UUID?) {
        var info: [String: Any] = [:]
        if let id = recordID { info["record"] = id.uuidString }
        post(title: title, body: body, userInfo: info)
    }

    /// "A web address is on the clipboard" — clicking opens the download dialog with it typed in.
    func postDownload(url: String, title: String, body: String) {
        post(title: title, body: body, userInfo: ["download": url])
    }

    private func post(title: String, body: String, userInfo: [String: Any]) {
        guard Notifier.isAvailable else { return }
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        center.getNotificationSettings { settings in
            switch settings.authorizationStatus {
            case .authorized, .provisional:
                Notifier.add(center, title: title, body: body, userInfo: userInfo)
            case .notDetermined:
                center.requestAuthorization(options: [.alert]) { granted, _ in
                    if granted { Notifier.add(center, title: title, body: body, userInfo: userInfo) }
                }
            default:
                break // denied: stay quiet
            }
        }
    }

    private static func add(_ center: UNUserNotificationCenter, title: String, body: String, userInfo: [String: Any]) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        if !userInfo.isEmpty { content.userInfo = userInfo }
        center.add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)) { _ in }
    }

    // Show the banner even while Flect is the active app (its window may be behind others or about to close).
    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                                withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner])
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                                withCompletionHandler completionHandler: @escaping () -> Void) {
        let info = response.notification.request.content.userInfo
        if let url = info["download"] as? String {
            DispatchQueue.main.async { self.onOpenDownload?(url) }
        } else {
            let id = (info["record"] as? String).flatMap(UUID.init)
            DispatchQueue.main.async { self.onOpen?(id) }
        }
        completionHandler()
    }
}
