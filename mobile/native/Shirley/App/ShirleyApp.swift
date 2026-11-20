import SwiftUI
import UIKit
import UserNotifications

@main
struct ShirleyApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        WindowGroup {
            RootView()
        }
    }
}

/// Notification plumbing: banners while the app is open, and a tap opens that bot's conversation.
final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    /// Set by the root view once it can navigate; until then a tapped notification waits here.
    @MainActor static var openBot: ((String) -> Void)?
    @MainActor static var pendingBot: String?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        return true
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .list, .sound])
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let bot = response.notification.request.content.userInfo["bot"] as? String
        Task { @MainActor in
            if let bot {
                if let open = AppDelegate.openBot { open(bot) } else { AppDelegate.pendingBot = bot }
            }
            completionHandler()
        }
    }
}

/// The screens are drawn by the app (the web UI's own headers), so the system bar is hidden;
/// this keeps the edge swipe that steps back out of a screen.
extension UINavigationController: @retroactive UIGestureRecognizerDelegate {
    override open func viewDidLoad() {
        super.viewDidLoad()
        interactivePopGestureRecognizer?.delegate = self
    }

    public func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
        viewControllers.count > 1
    }
}
