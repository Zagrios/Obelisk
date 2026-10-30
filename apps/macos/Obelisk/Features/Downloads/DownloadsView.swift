import Charts
import SwiftUI

/// What the download queue shows of a game: its title, and its cover and
/// wide artwork once its storefront's library knows it, built when shown.
struct DownloadGame {
    let title: String
    let cover: (() -> AnyView)?
    let artwork: (() -> AnyView)?
}

extension LibraryStorefront {
    /// The queued Transfer's game, as the account's library knows it.
    func downloadGame<Cover: View, Artwork: View>(
        _ item: DownloadItem, cover: @escaping (Game) -> Cover, artwork: @escaping (Game) -> Artwork
    ) -> DownloadGame {
        guard let game = Store.game(item.game).flatMap({ library.game($0) }) else {
            return DownloadGame(title: item.name, cover: nil, artwork: nil)
        }
        return DownloadGame(
            title: game.title,
            cover: { AnyView(cover(game)) },
            artwork: { AnyView(artwork(game)) }
        )
    }
}

/// A count, as figures alone.
private func verbatimCount(_ prefix: String, _ count: Int) -> String {
    prefix + count.formatted(.number.locale(L10n.locale))
}

extension SessionStorefront {
    var section: LibrarySection {
        switch self {
        case .steam: .steam
        case .epic: .epic
        case .gog: .gog
        }
    }
}

enum DownloadFormatting {
    /// A speed in the file sizes Finder shows; nothing reads as "0 kB/s".
    static func speed(_ bytesPerSecond: Double) -> String {
        if UserDefaults.standard.bool(forKey: "downloadSpeedInMbps") {
            return L10n.string("ui.settings.speed_mbps_value",
                (max(0, bytesPerSecond) * 8 / 1_000_000)
                    .formatted(.number.precision(.fractionLength(0...1)).locale(L10n.locale)))
        }
        return L10n.string("ui.downloads.speed", size(UInt64(max(0, bytesPerSecond))))
    }

    /// A size in the units Finder shows; nothing reads as "Zero kB".
    static func size(_ bytes: UInt64) -> String {
        guard bytes == 0 else { return GameFormatting.bytes(bytes) }
        return Int64(0).formatted(.byteCount(style: .file, allowedUnits: .kb, spellsOutZero: false).locale(L10n.locale))
    }

    static func percent(_ fraction: Double) -> String {
        fraction.formatted(.percent.precision(.fractionLength(0)).locale(L10n.locale))
    }

    static func kind(_ kind: DownloadKind) -> String {
        switch kind {
        case .install: L10n.string("ui.downloads.kind_install")
        case .update: L10n.string("ui.downloads.kind_update")
        case .repair: L10n.string("ui.downloads.kind_repair")
        case .move: L10n.string("ui.downloads.kind_move")
        }
    }

    static func storefront(_ storefront: SessionStorefront) -> String {
        storefront.section.title
    }
}

// MARK: - Sidebar

/// The queue at the bottom of the sidebar, over its game's artwork: the
/// running Transfer's game and progress, or what waits. It opens the queue in
/// a popover.
struct DownloadsSidebarButton: View {
    let downloads: DownloadsModel
    let game: (DownloadItem) -> DownloadGame
    let open: (DownloadItem) -> Void
    @Environment(\.colorScheme) private var colorScheme
    @State private var isHovered = false

    /// The ink: white over darkened artwork in dark mode, black over a white
    /// veil in light mode.
    private var ink: Color { colorScheme == .dark ? .white : .black }
    private var shape: RoundedRectangle { RoundedRectangle(cornerRadius: 14, style: .continuous) }

    var body: some View {
        // The running game, else the next one: resolved once per update.
        let shown = (downloads.running ?? downloads.waiting.first).map(game)
        let title = (downloads.running == nil ? nil : shown?.title) ?? L10n.string("ui.downloads.title")
        Button { downloads.isPresented = true } label: {
            HStack(spacing: 10) {
                DownloadRing(fraction: fraction, active: downloads.running != nil, ink: ink)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(.system(size: 12.5, weight: .semibold))
                        .lineLimit(1)
                    Text(subtitle)
                        .font(.system(size: 11))
                        .monospacedDigit()
                        .foregroundStyle(ink.opacity(0.72))
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
                if downloads.waiting.count > 0 && downloads.running != nil {
                    Text(verbatimCount("+", downloads.waiting.count))
                        .font(.system(size: 11, weight: .bold))
                        .monospacedDigit()
                        .padding(.horizontal, 7)
                        .padding(.vertical, 3)
                        .background(ink.opacity(0.14), in: Capsule())
                }
            }
            .foregroundStyle(ink)
            .padding(.leading, 9)
            .padding(.trailing, 11)
            .frame(height: 50)
            .background { background(shown?.artwork) }
            .overlay { shape.strokeBorder(.primary.opacity(0.1), lineWidth: 1) }
            .shadow(color: .black.opacity(colorScheme == .dark ? 0.35 : 0.18), radius: 10, y: 4)
            .contentShape(shape)
        }
        .obeliskButtonStyle(.plain)
        .onHover { isHovered = $0 }
        .animation(.easeOut(duration: 0.15), value: isHovered)
        .popover(isPresented: Bindable(downloads).isPresented, arrowEdge: .trailing) {
            DownloadsPanel(downloads: downloads, game: game, open: open)
        }
        .help(L10n.string("ui.downloads.open"))
        .accessibilityLabel(L10n.string("ui.downloads.title"))
        .accessibilityValue([title, subtitle].joined(separator: ", "))
        .accessibilityIdentifier("downloads-button")
    }

    private var transfer: GameTransfer? { downloads.currentTransfer }
    private var fraction: Double? { transfer?.progressFraction }

    /// The running game's artwork, else the next one's, under a veil the
    /// text reads over: darkest behind it, lighter on hover.
    private func background(_ artwork: (() -> AnyView)?) -> some View {
        let dark = colorScheme == .dark
        let veil: Color = dark ? .black : .white
        let opacity = (dark ? 0.82 : 0.84) - (isHovered ? 0.08 : 0)
        return Rectangle()
            .fill(dark ? Color(white: 0.14) : Color(white: 0.95))
            .overlay {
                if let artwork { artwork() }
            }
            .overlay {
                LinearGradient(colors: [veil.opacity(opacity), veil.opacity(opacity - 0.24)],
                               startPoint: .leading, endPoint: .trailing)
            }
            .clipShape(shape)
    }

    private var subtitle: String {
        if let running = downloads.running {
            guard let transfer else { return L10n.string("ui.transfer.preparing_short") }
            let phase = running.launching ? L10n.string("ui.downloads.before_launch") : transfer.activityTitle
            // A move's pace reads as a download's.
            let paced = transfer.state == .downloading || transfer.state == .moving
            let lead = (paced ? downloads.currentSpeed.map(DownloadFormatting.speed) : nil) ?? phase
            return [lead, fraction.map(DownloadFormatting.percent)].compactMap { $0 }.joined(separator: " · ")
        }
        if downloads.waitsForGame { return L10n.string("ui.downloads.waiting_for_game") }
        if downloads.waiting.allSatisfy(\.paused) { return L10n.string("ui.downloads.paused") }
        return L10n.string("ui.downloads.waiting_count", Int64(downloads.waiting.count))
    }
}

/// The running Transfer's progress around a download arrow.
private struct DownloadRing: View {
    let fraction: Double?
    let active: Bool
    let ink: Color

    var body: some View {
        ZStack {
            Circle().stroke(ink.opacity(0.2), lineWidth: 2.5)
            if let fraction {
                Circle()
                    .trim(from: 0, to: max(0.02, fraction))
                    .stroke(ink, style: StrokeStyle(lineWidth: 2.5, lineCap: .round))
                    .rotationEffect(.degrees(-90))
                    .animation(.easeOut(duration: 0.4), value: fraction)
            }
            Image(systemName: active ? "arrow.down" : "pause.fill")
                .font(.system(size: 11, weight: .bold))
        }
        .frame(width: 30, height: 30)
        .accessibilityHidden(true)
    }
}

// MARK: - Modal

/// The download queue, in its popover: the running Transfer's speed over
/// time, the Transfer itself, then what waits.
struct DownloadsPanel: View {
    let downloads: DownloadsModel
    let game: (DownloadItem) -> DownloadGame
    let open: (DownloadItem) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    if downloads.isVisible {
                        if let running = downloads.running {
                            DownloadSpeedCard(downloads: downloads)
                            DownloadCurrentCard(item: running, game: game(running), downloads: downloads,
                                                open: { open(running) })
                        }
                        if !downloads.waiting.isEmpty { queue }
                    } else {
                        empty
                    }
                }
                .padding(.horizontal, 18)
                .padding(.bottom, 18)
            }
            .scrollBounceBehavior(.basedOnSize)
            // As tall as the queue, up to a scrolling limit.
            .frame(maxHeight: 520)
            .fixedSize(horizontal: false, vertical: true)
        }
        .frame(width: 440)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(L10n.string("ui.downloads.title"))
        .accessibilityIdentifier("downloads-panel")
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 3) {
                Text(L10n.string("ui.downloads.title"))
                    .font(.system(size: 22, weight: .bold))
                    .accessibilityAddTraits(.isHeader)
                Text(downloads.count == 0 ? L10n.string("ui.downloads.empty_short")
                     : L10n.string("ui.downloads.count", Int64(downloads.count)))
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            }
            Spacer()
        }
        .padding(.horizontal, 18)
        .padding(.top, 16)
        .padding(.bottom, 14)
    }

    private var queue: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text(L10n.string("ui.downloads.up_next"))
                    .font(.system(size: 13, weight: .semibold))
                Text(verbatimCount("", downloads.waiting.count))
                    .font(.system(size: 11, weight: .bold))
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(.primary.opacity(0.07), in: Capsule())
            }
            if downloads.held && downloads.waiting.contains(where: \.automatic) {
                Label(L10n.string("ui.downloads.held"), systemImage: "gamecontroller")
                    .font(.system(size: 11.5))
                    .foregroundStyle(.secondary)
            }
            VStack(spacing: 2) {
                ForEach(downloads.waiting, id: \.id) { item in
                    DownloadQueueRow(item: item, game: game(item), downloads: downloads, open: { open(item) })
                }
            }
        }
    }

    private var empty: some View {
        VStack(spacing: 10) {
            Image(systemName: "arrow.down.circle")
                .font(.system(size: 34, weight: .light))
                .foregroundStyle(.tertiary)
            Text(L10n.string("ui.downloads.empty"))
                .font(.system(size: 14, weight: .semibold))
            Text(L10n.string("ui.downloads.empty_message"))
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 56)
    }
}

// MARK: - Speed

/// The running Transfer's download speed over the last two minutes, under its
/// current value. Hovering reads any moment.
private struct DownloadSpeedCard: View {
    @AppStorage("downloadSpeedInMbps") private var speedInMbps = false
    let downloads: DownloadsModel
    @State private var selected: Date?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .lastTextBaseline, spacing: 24) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(L10n.string("ui.downloads.speed_title"))
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(.secondary)
                    // A fast speed beside a large Downloaded size would wrap its unit
                    // under its figure: it shrinks instead.
                    Text(headline)
                        .font(.system(size: 34, weight: .bold, design: .rounded))
                        .monospacedDigit()
                        .lineLimit(1)
                        .minimumScaleFactor(0.6)
                }
                .layoutPriority(1)
                Spacer()
                DownloadStat(title: L10n.string("ui.downloads.peak"),
                             value: downloads.samples.count > 1 ? DownloadFormatting.speed(downloads.peakSpeed) : "—")
                DownloadedStat(downloads: downloads)
            }
            chart
                .frame(height: 104)
        }
        .modifier(GamePageCard())
        .id(speedInMbps)
    }

    private var headline: String { DownloadFormatting.speed(downloads.currentSpeed ?? 0) }

    @ViewBuilder private var chart: some View {
        let samples = downloads.samples
        if samples.count < 2 {
            VStack {
                Spacer()
                Text(L10n.string("ui.downloads.speed_waiting"))
                    .font(.system(size: 11.5))
                    .foregroundStyle(.secondary)
                Spacer()
                Rectangle().fill(.primary.opacity(0.08)).frame(height: 1)
            }
        } else {
            Chart {
                ForEach(samples) { sample in
                    AreaMark(x: .value("time", sample.time), y: .value("speed", sample.bytesPerSecond))
                        .interpolationMethod(.monotone)
                        .foregroundStyle(LinearGradient(colors: [ObeliskStyle.accent.opacity(0.22),
                                                                 ObeliskStyle.accent.opacity(0.02)],
                                                        startPoint: .top, endPoint: .bottom))
                    LineMark(x: .value("time", sample.time), y: .value("speed", sample.bytesPerSecond))
                        .interpolationMethod(.monotone)
                        .foregroundStyle(ObeliskStyle.accent)
                        .lineStyle(StrokeStyle(lineWidth: 2, lineCap: .round, lineJoin: .round))
                }
                if let hovered {
                    RuleMark(x: .value("time", hovered.time))
                        .foregroundStyle(.primary.opacity(0.25))
                        .lineStyle(StrokeStyle(lineWidth: 1))
                        .annotation(position: .top, spacing: 4,
                                    overflowResolution: .init(x: .fit(to: .chart), y: .disabled)) {
                            Text(DownloadFormatting.speed(hovered.bytesPerSecond))
                                .font(.system(size: 11, weight: .semibold))
                                .monospacedDigit()
                                .padding(.horizontal, 8)
                                .padding(.vertical, 4)
                                .background(.regularMaterial, in: Capsule())
                        }
                    PointMark(x: .value("time", hovered.time), y: .value("speed", hovered.bytesPerSecond))
                        .symbolSize(48)
                        .foregroundStyle(ObeliskStyle.accent)
                }
            }
            .chartXAxis(.hidden)
            .chartYAxis {
                AxisMarks(position: .trailing, values: .automatic(desiredCount: 3)) { value in
                    AxisGridLine(stroke: StrokeStyle(lineWidth: 1)).foregroundStyle(.primary.opacity(0.07))
                    AxisValueLabel {
                        if let speed = value.as(Double.self) {
                            Text(DownloadFormatting.speed(speed))
                                .font(.system(size: 10))
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            }
            // A still Transfer's axis reads up to 1 MB/s rather than in bytes.
            .chartYScale(domain: 0...max(1_000_000, downloads.peakSpeed * 1.15))
            .chartXSelection(value: $selected)
            .accessibilityLabel(L10n.string("ui.downloads.speed_title"))
            .accessibilityValue(headline)
        }
    }

    /// The sample nearest the pointer.
    private var hovered: DownloadSpeedSample? {
        guard let selected else { return nil }
        return downloads.samples.min {
            abs($0.time.timeIntervalSince(selected)) < abs($1.time.timeIntervalSince(selected))
        }
    }
}

/// One figure beside the speed.
private struct DownloadStat: View {
    let title: String
    let value: String

    var body: some View {
        VStack(alignment: .trailing, spacing: 2) {
            Text(title)
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(.secondary)
            Text(value)
                .font(.system(size: 13, weight: .semibold))
                .monospacedDigit()
        }
        .fixedSize()
    }
}

/// The running Transfer's downloaded size, in its own body: it changes with
/// every progress snapshot, the chart only with each speed sample.
private struct DownloadedStat: View {
    let downloads: DownloadsModel

    var body: some View {
        DownloadStat(title: L10n.string("ui.downloads.downloaded"),
                     value: downloads.currentTransfer.map { DownloadFormatting.size($0.contentBytes) } ?? "—")
    }
}

// MARK: - Transfers

/// A queued game's cover, or a quiet stand-in until its library knows it.
private struct DownloadCover: View {
    let game: DownloadGame
    let width: CGFloat

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: width * 0.14, style: .continuous)
        Group {
            if let cover = game.cover { cover() } else {
                Image(systemName: "gamecontroller")
                    .font(.system(size: width * 0.32))
                    .foregroundStyle(.tertiary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(.primary.opacity(0.06))
            }
        }
        .frame(width: width, height: width / ObeliskStyle.coverAspectRatio)
        .clipShape(shape)
        .overlay { shape.strokeBorder(.primary.opacity(0.1), lineWidth: 1) }
        .accessibilityHidden(true)
    }
}

/// The Transfer under way: its game, its progress and what it does.
private struct DownloadCurrentCard: View {
    let item: DownloadItem
    let game: DownloadGame
    let downloads: DownloadsModel
    let open: () -> Void

    /// Read here, so progress updates this card rather than the whole panel.
    private var transfer: GameTransfer? { downloads.currentTransfer }

    var body: some View {
        HStack(alignment: .center, spacing: 16) {
            Button(action: open) { DownloadCover(game: game, width: 60) }
                .obeliskButtonStyle(.plain)
                .help(L10n.string("ui.downloads.show_game"))
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    Text(game.title)
                        .font(.system(size: 15, weight: .semibold))
                        .lineLimit(1)
                    DownloadBadge(text: item.launching ? L10n.string("ui.downloads.before_launch")
                                  : DownloadFormatting.kind(item.kind), prominent: true)
                }
                HStack {
                    Text(phase)
                    Spacer()
                    if let fraction = transfer?.progressFraction {
                        Text(DownloadFormatting.percent(fraction)).fontWeight(.semibold)
                    }
                }
                .font(.system(size: 11.5))
                .monospacedDigit()
                .foregroundStyle(.secondary)
                DownloadProgressBar(fraction: transfer?.progressFraction)
                Text(detail)
                    .font(.system(size: 11))
                    .monospacedDigit()
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
            }
            HStack(spacing: 8) {
                if !item.launching {
                    StorefrontGlassButton(systemName: "pause.fill", size: 12, label: L10n.string("ui.downloads.pause"),
                                          identifier: "downloads-pause") { downloads.pause(item) }
                }
                StorefrontGlassButton(systemName: "xmark", size: 12, label: L10n.string("ui.downloads.remove"),
                                      identifier: "downloads-cancel") { downloads.remove(item) }
            }
        }
        .modifier(GamePageCard())
        .accessibilityElement(children: .contain)
        .accessibilityLabel(game.title)
    }

    private var phase: String {
        guard let transfer else { return L10n.string("ui.transfer.preparing_short") }
        return transfer.activityTitle
    }

    private var detail: String {
        guard let transfer else { return DownloadFormatting.storefront(item.storefront) }
        return [
            DownloadFormatting.storefront(item.storefront),
            transfer.countedFilesText,
            transfer.downloadedGameDataText,
        ]
        .compactMap { $0 }
        .joined(separator: " · ")
    }
}

/// One waiting Transfer, its controls showing on hover.
private struct DownloadQueueRow: View {
    let item: DownloadItem
    let game: DownloadGame
    let downloads: DownloadsModel
    let open: () -> Void
    @State private var isHovered = false

    var body: some View {
        HStack(spacing: 12) {
            DownloadCover(game: game, width: 30)
            VStack(alignment: .leading, spacing: 3) {
                Text(game.title)
                    .font(.system(size: 13, weight: .medium))
                    .lineLimit(1)
                Text(subtitle)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            if item.paused { DownloadBadge(text: L10n.string("ui.downloads.paused"), prominent: false) }
            HStack(spacing: 4) {
                if downloads.waiting.first?.id != item.id && !item.launching {
                    rowButton("arrow.up.to.line", L10n.string("ui.downloads.move_to_top")) { downloads.moveToTop(item) }
                }
                if !item.launching {
                    if item.paused {
                        rowButton("play.fill", L10n.string("ui.downloads.resume")) { downloads.resume(item) }
                    } else {
                        rowButton("pause.fill", L10n.string("ui.downloads.pause")) { downloads.pause(item) }
                    }
                }
                rowButton("xmark", L10n.string("ui.downloads.remove")) { downloads.remove(item) }
            }
            .opacity(isHovered ? 1 : 0.35)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(.primary.opacity(isHovered ? 0.05 : 0), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .contentShape(.rect)
        .onHover { isHovered = $0 }
        .onTapGesture(count: 2, perform: open)
        .animation(.easeOut(duration: 0.12), value: isHovered)
        .accessibilityElement(children: .contain)
        .accessibilityLabel([game.title, subtitle].joined(separator: ", "))
    }

    private var subtitle: String {
        [
            DownloadFormatting.kind(item.kind),
            DownloadFormatting.storefront(item.storefront),
            item.automatic ? L10n.string("ui.downloads.automatic") : nil,
            downloads.held && item.automatic ? L10n.string("ui.downloads.waiting_for_game") : nil,
        ]
        .compactMap { $0 }
        .joined(separator: " · ")
    }

    private func rowButton(_ symbol: String, _ label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 10.5, weight: .bold))
                .frame(width: 26, height: 26)
                .background(.primary.opacity(0.07), in: Circle())
                .contentShape(.circle)
        }
        .obeliskButtonStyle(.plain)
        .foregroundStyle(.secondary)
        .help(label)
        .accessibilityLabel(label)
    }
}

private struct DownloadBadge: View {
    let text: String
    let prominent: Bool

    var body: some View {
        Text(text)
            .font(.system(size: 10, weight: .semibold))
            .foregroundStyle(prominent ? ObeliskStyle.accent : .secondary)
            .padding(.horizontal, 7)
            .padding(.vertical, 2.5)
            .background((prominent ? ObeliskStyle.accent : Color.primary).opacity(prominent ? 0.13 : 0.07), in: Capsule())
            .lineLimit(1)
            .fixedSize()
    }
}

/// A thin track the progress fills; indeterminate while unknown.
private struct DownloadProgressBar: View {
    let fraction: Double?

    var body: some View {
        GeometryReader { proxy in
            ZStack(alignment: .leading) {
                Capsule().fill(.primary.opacity(0.08))
                if let fraction {
                    Capsule()
                        .fill(LinearGradient(colors: [ObeliskStyle.accent.opacity(0.85), ObeliskStyle.accent],
                                             startPoint: .leading, endPoint: .trailing))
                        .frame(width: max(6, proxy.size.width * fraction))
                        .animation(.easeOut(duration: 0.4), value: fraction)
                }
            }
        }
        .frame(height: 6)
        .accessibilityHidden(true)
    }
}
