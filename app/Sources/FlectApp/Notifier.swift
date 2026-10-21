import AppKit
import UserNotifications

/// "Run finished" notifications. Permission is asked for lazily, the first time there is something to report,
/// and every path degrades to doing nothing: denied permission, a build run outside an .app bundle
/// (UNUserNotificationCenter raises an Objective-C exception there), or a failed request.
final class Notifier: NSObject, UNUserNotificationCenterDelegate, @unchecked Sendable {
    static let shared = Notifier()

    /// Called on the main thread when the user clicks a notification (the run's id, if it created files).
    var onOpen: ((UUID?) -> Void)?

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
        guard Notifier.isAvailable else { return }
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        center.getNotificationSettings { settings in
            switch settings.authorizationStatus {
            case .authorized, .provisional:
                Notifier.add(center, title: title, body: body, recordID: recordID)
            case .notDetermined:
                center.requestAuthorization(options: [.alert]) { granted, _ in
                    if granted { Notifier.add(center, title: title, body: body, recordID: recordID) }
                }
            default:
                break // denied: stay quiet
            }
        }
    }

    private static func add(_ center: UNUserNotificationCenter, title: String, body: String, recordID: UUID?) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        if let id = recordID { content.userInfo = ["record": id.uuidString] }
        center.add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)) { _ in }
    }

    // Show the banner even while Flect is the active app (its window may be behind others or about to close).
    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                                withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner])
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                                withCompletionHandler completionHandler: @escaping () -> Void) {
        let id = (response.notification.request.content.userInfo["record"] as? String).flatMap(UUID.init)
        DispatchQueue.main.async { self.onOpen?(id) }
        completionHandler()
    }
}
