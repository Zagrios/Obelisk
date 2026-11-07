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
    @State private var rotation: Double = 0.0
    @State private var offset: CGSize = .zero
    @State private var dragOffset: CGSize = .zero
    @State private var image: NSImage?

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
                return CGSize(width: 260, height: 260)
            }
        } else {
            if abs(aspectRatio - (16.0 / 9.0)) < 0.01 {
                return CGSize(width: 440, height: 247.5)
            } else {
                return CGSize(width: 460, height: 197.1)
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
                            DragGesture()
                                .onChanged { value in
                                    dragOffset = value.translation
                                }
                                .onEnded { value in
                                    offset.width += value.translation.width
                                    offset.height += value.translation.height
                                    dragOffset = .zero
                                }
                        )
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

                // Zoom Slider & Rotate / Reset buttons
                HStack(spacing: 14) {
                    Image(systemName: "magnifyingglass")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)

                    Slider(value: $scale, in: 1.0...3.5, step: 0.05)
                        .labelsHidden()

                    Text(String(format: "%.1fx", scale))
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .frame(width: 34, alignment: .trailing)

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

    // MARK: - Actions

    private func loadImage() {
        if let loaded = NSImage(contentsOfFile: target.path) {
            image = loaded
        }
    }

    private func applyCrop() {
        guard let sourceImage = image else { return }

        // Render destination cropped image
        let targetWidth: CGFloat = target.isCover ? 900 : 1920
        let targetHeight: CGFloat = targetWidth / aspectRatio
        let targetSize = CGSize(width: targetWidth, height: targetHeight)

        let targetImage = NSImage(size: targetSize)
        targetImage.lockFocus()

        guard let context = NSGraphicsContext.current?.cgContext else {
            targetImage.unlockFocus()
            return
        }

        let canvasWidth: CGFloat = 532
        let canvasHeight: CGFloat = 330
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
        let canvasAspect = canvasWidth / canvasHeight
        let fitSize: CGSize
        if imgAspect > canvasAspect {
            fitSize = CGSize(width: canvasWidth, height: canvasWidth / imgAspect)
        } else {
            fitSize = CGSize(width: canvasHeight * imgAspect, height: canvasHeight)
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
        let destinationURL = dir.appendingPathComponent("\(filename)_cropped.png")

        do {
            try pngData.write(to: destinationURL, options: .atomic)
            onApply(destinationURL.path)
            dismiss()
        } catch {
            dismiss()
        }
    }
}
