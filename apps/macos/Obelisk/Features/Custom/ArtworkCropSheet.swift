import AppKit
import SwiftUI

/// Mode specifying whether we are cropping a vertical cover (or icon) or a landscape banner.
enum ArtworkCropTarget: Identifiable, Sendable {
    case cover(path: String)
    case banner(path: String)

    var id: String {
        switch self {
        case .cover(let path): "cover-\(path)"
        case .banner(let path): "banner-\(path)"
        }
    }

    var path: String {
        switch self {
        case .cover(let path), .banner(let path): path
        }
    }

    var isCover: Bool {
        switch self {
        case .cover: true
        case .banner: false
        }
    }
}

/// Modal sheet for cropping custom game cover and banner artwork.
struct ArtworkCropSheet: View {
    let target: ArtworkCropTarget
    let onApply: (String) -> Void

    @State private var aspectRatio: CGFloat
    @State private var scale: CGFloat = 1.0
    @State private var baseScale: CGFloat = 1.0
    @State private var rotation: Double = 0.0
    @State private var offset: CGSize = .zero
    @State private var dragOffset: CGSize = .zero
    @State private var image: NSImage?
    @State private var isHoveringCanvas: Bool = false
    @State private var currentCanvasSize: CGSize = CGSize(width: 532, height: 330)
    @State private var scrollMonitor: Any?

    @Environment(\.dismiss) private var dismiss

    init(target: ArtworkCropTarget, onApply: @escaping (String) -> Void) {
        self.target = target
        self.onApply = onApply
        _aspectRatio = State(initialValue: target.isCover ? (2.0 / 3.0) : (16.0 / 9.0))
    }

    private var cropBoxSize: CGSize {
        if target.isCover {
            if abs(aspectRatio - (2.0 / 3.0)) < 0.01 {
                return CGSize(width: 200, height: 300)
            } else {
                return CGSize(width: 270, height: 270)
            }
        } else {
            if abs(aspectRatio - (16.0 / 9.0)) < 0.01 {
                return CGSize(width: 460, height: 258.75)
            } else {
                return CGSize(width: 480, height: 205.71)
            }
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SheetHeader(
                title: L10n.string(target.isCover ? "ui.custom.crop_cover" : "ui.custom.crop_banner"),
                subtitle: target.path.components(separatedBy: "/").last,
                symbol: "crop",
                colors: TileColors.violet,
                close: { dismiss() },
                closeIdentifier: "artwork-crop-close"
            )
            .padding(.horizontal, 24)
            .padding(.top, 24)
            .padding(.bottom, 16)

            VStack(spacing: 16) {
                cropCanvas
                    .frame(height: 330)
                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                    .overlay {
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .strokeBorder(Color.primary.opacity(0.12), lineWidth: 1)
                    }

                controlsBar
            }
            .padding(.horizontal, 24)
            .padding(.bottom, 20)

            Divider()

            bottomBar
                .padding(.horizontal, 24)
                .padding(.vertical, 16)
        }
        .frame(width: 580, height: 570)
        .tint(ObeliskStyle.accent)
        .onAppear {
            loadImage()
            installScrollMonitor()
        }
        .onDisappear {
            removeScrollMonitor()
        }
    }

    // MARK: - Crop Canvas

    private var cropCanvas: some View {
        GeometryReader { geometry in
            let canvasSize = geometry.size
            let box = cropBoxSize

            ZStack {
                Color.black.opacity(0.92)

                // Transformed Image
                if let image {
                    Image(nsImage: image)
                        .resizable()
                        .scaledToFit()
                        .frame(width: canvasSize.width, height: canvasSize.height)
                        .scaleEffect(scale)
                        .rotationEffect(.degrees(rotation))
                        .offset(x: offset.width + dragOffset.width, y: offset.height + dragOffset.height)
                        .gesture(
                            SimultaneousGesture(
                                DragGesture()
                                    .onChanged { value in
                                        dragOffset = value.translation
                                    }
                                    .onEnded { value in
                                        offset.width += value.translation.width
                                        offset.height += value.translation.height
                                        dragOffset = .zero
                                    },
                                MagnificationGesture()
                                    .onChanged { val in
                                        let newScale = baseScale * val
                                        scale = min(max(newScale, 0.15), 8.0)
                                    }
                                    .onEnded { _ in
                                        baseScale = scale
                                    }
                            )
                        )
                        .onTapGesture(count: 2) {
                            withAnimation(.spring(response: 0.25, dampingFraction: 0.75)) {
                                if scale > 1.2 {
                                    scale = 1.0
                                    baseScale = 1.0
                                    offset = .zero
                                    dragOffset = .zero
                                } else {
                                    scale = 2.0
                                    baseScale = 2.0
                                }
                            }
                        }
                } else {
                    ProgressView()
                }

                // Dimmed mask with transparent cutout
                cutoutOverlay(canvasSize: canvasSize, boxSize: box)
                    .allowsHitTesting(false)

                // Crop box border & rule of thirds guidelines
                cropBoxGuide(size: box)
                    .allowsHitTesting(false)
            }
            .clipped()
            .onHover { hovering in
                isHoveringCanvas = hovering
            }
            .onAppear {
                currentCanvasSize = canvasSize
            }
            .onChange(of: canvasSize) { _, newSize in
                currentCanvasSize = newSize
            }
        }
    }

    private func cutoutOverlay(canvasSize: CGSize, boxSize: CGSize) -> some View {
        Canvas { context, size in
            let fullRect = CGRect(origin: .zero, size: size)
            let boxRect = CGRect(
                x: (size.width - boxSize.width) / 2,
                y: (size.height - boxSize.height) / 2,
                width: boxSize.width,
                height: boxSize.height
            )

            var path = Path()
            path.addRect(fullRect)
            path.addRoundedRect(in: boxRect, cornerSize: CGSize(width: 4, height: 4))

            context.fill(path, with: .color(.black.opacity(0.55)), style: FillStyle(eoFill: true))
        }
    }

    private func cropBoxGuide(size: CGSize) -> some View {
        ZStack {
            RoundedRectangle(cornerRadius: 4, style: .continuous)
                .strokeBorder(Color.white, lineWidth: 1.5)

            // Rule of thirds vertical lines
            HStack {
                Spacer()
                Rectangle().fill(Color.white.opacity(0.2)).frame(width: 1)
                Spacer()
                Rectangle().fill(Color.white.opacity(0.2)).frame(width: 1)
                Spacer()
            }

            // Rule of thirds horizontal lines
            VStack {
                Spacer()
                Rectangle().fill(Color.white.opacity(0.2)).frame(height: 1)
                Spacer()
                Rectangle().fill(Color.white.opacity(0.2)).frame(height: 1)
                Spacer()
            }
        }
        .frame(width: size.width, height: size.height)
    }

    // MARK: - Controls Bar

    private var controlsBar: some View {
        ObeliskCard {
            VStack(spacing: 12) {
                // Aspect Ratio Selector
                HStack(spacing: 12) {
                    Text(L10n.string("ui.custom.crop_title"))
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(.secondary)

                    Spacer(minLength: 0)

                    if target.isCover {
                        aspectButton(title: L10n.string("ui.custom.crop_aspect_2_3"), ratio: 2.0 / 3.0)
                        aspectButton(title: L10n.string("ui.custom.crop_aspect_1_1"), ratio: 1.0)
                    } else {
                        aspectButton(title: L10n.string("ui.custom.crop_aspect_16_9"), ratio: 16.0 / 9.0)
                        aspectButton(title: L10n.string("ui.custom.crop_aspect_21_9"), ratio: 21.0 / 9.0)
                    }
                }

                ObeliskCardDivider()

                // Zoom Controls & Rotate / Reset buttons
                HStack(spacing: 12) {
                    Button {
                        withAnimation(.easeInOut(duration: 0.15)) {
                            scale = max(scale - 0.25, 0.15)
                            baseScale = scale
                        }
                    } label: {
                        Image(systemName: "minus.magnifyingglass")
                            .font(.system(size: 13))
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                    .help(L10n.string("ui.custom.crop_zoom_out"))

                    Slider(value: $scale, in: 0.2...5.0, step: 0.02)
                        .labelsHidden()
                        .onChange(of: scale) { _, newScale in
                            baseScale = newScale
                        }

                    Button {
                        withAnimation(.easeInOut(duration: 0.15)) {
                            scale = min(scale + 0.25, 8.0)
                            baseScale = scale
                        }
                    } label: {
                        Image(systemName: "plus.magnifyingglass")
                            .font(.system(size: 13))
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                    .help(L10n.string("ui.custom.crop_zoom_in"))

                    Text(String(format: "%.0f%%", scale * 100))
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .frame(width: 42, alignment: .trailing)

                    Divider()
                        .frame(height: 16)

                    Button {
                        withAnimation(.easeInOut(duration: 0.2)) {
                            rotation = (rotation + 90).truncatingRemainder(dividingBy: 360)
                        }
                    } label: {
                        Image(systemName: "rotate.right")
                            .font(.system(size: 13))
                    }
                    .buttonStyle(.plain)
                    .help(L10n.string("ui.custom.crop_rotate"))

                    Button {
                        withAnimation(.easeInOut(duration: 0.2)) {
                            scale = 1.0
                            baseScale = 1.0
                            offset = .zero
                            dragOffset = .zero
                            rotation = 0.0
                        }
                    } label: {
                        Image(systemName: "arrow.counterclockwise")
                            .font(.system(size: 13))
                    }
                    .buttonStyle(.plain)
                    .help(L10n.string("ui.custom.crop_reset"))
                }
            }
            .padding(14)
        }
    }

    private func aspectButton(title: String, ratio: CGFloat) -> some View {
        let isSelected = abs(aspectRatio - ratio) < 0.01
        return Button {
            withAnimation(.easeInOut(duration: 0.2)) {
                aspectRatio = ratio
                offset = .zero
                dragOffset = .zero
            }
        } label: {
            Text(title)
                .font(.system(size: 11, weight: isSelected ? .bold : .medium))
                .padding(.horizontal, 10)
                .padding(.vertical, 4)
                .background(isSelected ? ObeliskStyle.accent.opacity(0.2) : Color.primary.opacity(0.06))
                .foregroundStyle(isSelected ? ObeliskStyle.accent : .primary)
                .clipShape(Capsule())
        }
        .buttonStyle(.plain)
    }

    // MARK: - Bottom Bar

    private var bottomBar: some View {
        HStack(spacing: 12) {
            Spacer(minLength: 0)

            GlassCapsuleButton(
                title: L10n.string("ui.common.cancel"),
                size: .large,
                identifier: "artwork-crop-cancel",
                action: { dismiss() }
            )
            .keyboardShortcut(.cancelAction)

            AccentCapsuleButton(
                title: L10n.string("ui.custom.crop_apply"),
                systemImage: "crop",
                size: .large,
                identifier: "artwork-crop-apply",
                action: applyCrop
            )
            .keyboardShortcut(.defaultAction)
        }
    }

    // MARK: - Event Monitoring

    private func installScrollMonitor() {
        guard scrollMonitor == nil else { return }
        scrollMonitor = NSEvent.addLocalMonitorForEvents(matching: [.scrollWheel, .magnify]) { event in
            guard isHoveringCanvas else { return event }
            if event.type == .scrollWheel {
                let delta = event.scrollingDeltaY != 0 ? event.scrollingDeltaY : (event.deltaY * 8)
                let sensitivity: CGFloat = 0.003
                let newScale = scale + (delta * sensitivity * scale)
                let clamped = min(max(newScale, 0.15), 8.0)
                scale = clamped
                baseScale = clamped
                return nil
            } else if event.type == .magnify {
                let newScale = scale * (1.0 + event.magnification)
                let clamped = min(max(newScale, 0.15), 8.0)
                scale = clamped
                baseScale = clamped
                return nil
            }
            return event
        }
    }

    private func removeScrollMonitor() {
        if let monitor = scrollMonitor {
            NSEvent.removeMonitor(monitor)
            scrollMonitor = nil
        }
    }

    // MARK: - Actions

    private func loadImage() {
        if let loaded = NSImage(contentsOfFile: target.path) {
            image = loaded
            let aspect = loaded.size.width / max(loaded.size.height, 1)
            if target.isCover {
                if abs(aspect - 1.0) < 0.15 {
                    aspectRatio = 1.0
                } else {
                    aspectRatio = 2.0 / 3.0
                }
            } else {
                if aspect > 2.05 {
                    aspectRatio = 21.0 / 9.0
                } else {
                    aspectRatio = 16.0 / 9.0
                }
            }
        }
    }

    private func applyCrop() {
        guard let sourceImage = image else { return }

        // Render destination cropped image
        let isSquare = abs(aspectRatio - 1.0) < 0.01
        let targetWidth: CGFloat
        if target.isCover {
            targetWidth = isSquare ? 1024 : 900
        } else {
            targetWidth = 1920
        }
        let targetHeight: CGFloat = targetWidth / aspectRatio
        let targetSize = CGSize(width: targetWidth, height: targetHeight)

        let targetImage = NSImage(size: targetSize)
        targetImage.lockFocus()

        guard let context = NSGraphicsContext.current?.cgContext else {
            targetImage.unlockFocus()
            return
        }

        let cSize = currentCanvasSize
        let box = cropBoxSize
        let multiplier = targetWidth / box.width

        // Translate to target image center
        context.translateBy(x: targetSize.width / 2, y: targetSize.height / 2)

        // Apply UI offset (Y is inverted in Cocoa drawing)
        let totalOffset = CGSize(
            width: (offset.width + dragOffset.width) * multiplier,
            height: -(offset.height + dragOffset.height) * multiplier
        )
        context.translateBy(x: totalOffset.width, y: totalOffset.height)

        // Apply rotation
        context.rotate(by: CGFloat(-rotation * .pi / 180.0))

        // Apply scale
        context.scaleBy(x: scale * multiplier, y: scale * multiplier)

        // Compute source image aspect fit size within the preview canvas
        let imgAspect = sourceImage.size.width / max(sourceImage.size.height, 1)
        let canvasAspect = cSize.width / max(cSize.height, 1)
        let fitSize: CGSize
        if imgAspect > canvasAspect {
            fitSize = CGSize(width: cSize.width, height: cSize.width / imgAspect)
        } else {
            fitSize = CGSize(width: cSize.height * imgAspect, height: cSize.height)
        }

        let drawRect = CGRect(
            x: -fitSize.width / 2,
            y: -fitSize.height / 2,
            width: fitSize.width,
            height: fitSize.height
        )

        sourceImage.draw(in: drawRect, from: .zero, operation: .sourceOver, fraction: 1.0)
        targetImage.unlockFocus()

        // Write cropped PNG file
        guard let tiffRepresentation = targetImage.tiffRepresentation,
              let bitmap = NSBitmapImageRep(data: tiffRepresentation),
              let pngData = bitmap.representation(using: .png, properties: [:]) else {
            return
        }

        let url = URL(fileURLWithPath: target.path)
        let dir = url.deletingLastPathComponent()
        let filename = url.deletingPathExtension().lastPathComponent
        let cleanName = filename.replacingOccurrences(of: "_cropped.*", with: "", options: .regularExpression)
        let timestamp = Int(Date().timeIntervalSince1970)
        let destinationURL = dir.appendingPathComponent("\(cleanName)_cropped_\(timestamp).png")

        // Clean up previous temporary cropped files for this asset
        if let files = try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil) {
            for file in files {
                let name = file.lastPathComponent
                if (name.hasPrefix("\(cleanName)_cropped_") || name == "\(cleanName)_cropped.png") && name.hasSuffix(".png") {
                    try? FileManager.default.removeItem(at: file)
                }
            }
        }

        do {
            try pngData.write(to: destinationURL, options: .atomic)
            onApply(destinationURL.path)
            dismiss()
        } catch {
            dismiss()
        }
    }
}
