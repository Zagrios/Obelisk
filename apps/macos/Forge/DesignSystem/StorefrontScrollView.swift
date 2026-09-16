import AppKit
import SwiftUI

/// Keeps the same persistent scrollbar and content gutter for every storefront.
struct StorefrontScrollView<Content: View>: View {
    private let content: Content
    private let topOverlayHeight: CGFloat

    init(topOverlayHeight: CGFloat = 0, @ViewBuilder content: () -> Content) {
        self.topOverlayHeight = topOverlayHeight
        self.content = content()
    }

    var body: some View {
        GeometryReader { geometry in
            ScrollView {
                content
                    .padding(.top, topOverlayHeight)
                    // SwiftUI gives short documents the full scroll-view width,
                    // even with a persistent AppKit scroller. Reserve its gutter
                    // in both states, keeping the content aligned to the left.
                    .frame(width: max(0, geometry.size.width - StorefrontPersistentScroller.width), alignment: .leading)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background { StorefrontPersistentScroller(topInset: topOverlayHeight).frame(width: 0, height: 0) }
            }
        }
    }
}

private struct StorefrontPersistentScroller: NSViewRepresentable {
    let topInset: CGFloat
    static var width: CGFloat {
        PersistentScroller.scrollerWidth(for: .regular, scrollerStyle: .legacy)
    }

    func makeNSView(context: Context) -> ScrollerConfigurationView {
        let view = ScrollerConfigurationView()
        view.topInset = topInset
        return view
    }

    func updateNSView(_ nsView: ScrollerConfigurationView, context: Context) {
        nsView.topInset = topInset
        nsView.configureScroller()
    }

    final class ScrollerConfigurationView: NSView {
        var topInset: CGFloat = 0
        override func viewDidMoveToSuperview() {
            super.viewDidMoveToSuperview()
            configureScroller()
        }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            configureScroller()
        }

        override func layout() {
            super.layout()
            configureScroller()
        }

        func configureScroller() {
            guard let scrollView = enclosingScrollView else { return }
            // Overlay scrollers fade out even with autohiding disabled. Reserve
            // the scrollbar's space while the detail's news are still loading.
            if scrollView.scrollerStyle != .legacy { scrollView.scrollerStyle = .legacy }
            if scrollView.autohidesScrollers { scrollView.autohidesScrollers = false }
            if !scrollView.hasVerticalScroller { scrollView.hasVerticalScroller = true }
            if scrollView.scrollerInsets.top != topInset {
                scrollView.scrollerInsets.top = topInset
            }
            if !(scrollView.verticalScroller is PersistentScroller) {
                let scroller = PersistentScroller(frame: scrollView.verticalScroller?.frame ?? .zero)
                scroller.controlSize = scrollView.verticalScroller?.controlSize ?? .regular
                scrollView.verticalScroller = scroller
            }
        }
    }

    final class PersistentScroller: NSScroller {
        // Use the modern scroller metrics while the enclosing view keeps the
        // legacy style so its thumb cannot fade away between pages.
        override class var isCompatibleWithOverlayScrollers: Bool { true }

        override func drawKnobSlot(in slotRect: NSRect, highlight flag: Bool) {
            super.drawKnobSlot(in: slotRect, highlight: flag)
            guard !isEnabled else { return }

            // AppKit stops drawing the thumb when the whole document fits.
            // Keep a full-height thumb visible without enabling fake scrolling.
            let thumb = bounds.insetBy(dx: 3, dy: 3)
            NSColor.labelColor.withAlphaComponent(0.5).setFill()
            NSBezierPath(roundedRect: thumb, xRadius: thumb.width / 2, yRadius: thumb.width / 2).fill()
        }
    }
}
