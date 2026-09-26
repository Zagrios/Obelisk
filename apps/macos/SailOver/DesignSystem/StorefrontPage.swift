import SwiftUI

/// The document scrolls behind a fixed, translucent storefront title bar.
struct StorefrontPage<Header: View, Content: View>: View {
    private let header: Header
    private let content: Content

    init(@ViewBuilder header: () -> Header, @ViewBuilder content: () -> Content) {
        self.header = header()
        self.content = content()
    }

    var body: some View {
        StorefrontScrollView(topOverlayHeight: SailOverStyle.titleBarHeight) { content }
            .overlay(alignment: .top) {
                header.frame(height: SailOverStyle.titleBarHeight)
            }
            .clipped()
            .ignoresSafeArea(.container, edges: .top)
    }
}
