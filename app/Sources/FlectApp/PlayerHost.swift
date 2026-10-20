import AppKit
import AVKit

// AVKit is imported here and nowhere that imports SwiftUI: when a file imports both, Swift auto-loads the
// `_AVKit_SwiftUI` overlay, whose generic metadata aborted the app at launch of the trim dialog (macOS 26).
// SwiftUI's own VideoPlayer is therefore avoided; the dialog hosts the AppKit AVPlayerView instead.
enum PlayerHost {
    static func makeView(player: AVPlayer) -> NSView {
        let v = AVPlayerView()
        v.player = player
        v.controlsStyle = .inline
        v.showsFullScreenToggleButton = false
        return v
    }
}
