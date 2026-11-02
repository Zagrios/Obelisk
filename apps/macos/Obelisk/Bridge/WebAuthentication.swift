import AppKit
import WebKit

/// The storefront whose web sign-in a window shows, and the text it shows.
enum WebSignInStore: Sendable {
    case epic, gog

    fileprivate enum Text {
        case title, host, loading, connecting, invalidResponse, network, unsupportedPage
    }

    fileprivate func string(_ text: Text) -> String {
        switch (self, text) {
        case (.epic, .title): L10n.string("model.epicWeb.title")
        case (.gog, .title): L10n.string("model.gogWeb.title")
        case (.epic, .host): L10n.string("model.epicWeb.host")
        case (.gog, .host): L10n.string("model.gogWeb.host")
        case (.epic, .loading): L10n.string("model.epicWeb.loading")
        case (.gog, .loading): L10n.string("model.gogWeb.loading")
        case (.epic, .connecting): L10n.string("model.epicWeb.connecting")
        case (.gog, .connecting): L10n.string("model.gogWeb.connecting")
        case (.epic, .invalidResponse): L10n.string("model.epicWeb.invalidResponse")
        case (.gog, .invalidResponse): L10n.string("model.gogWeb.invalidResponse")
        case (.epic, .network): L10n.string("model.epicWeb.network")
        case (.gog, .network): L10n.string("model.gogWeb.network")
        case (.epic, .unsupportedPage): L10n.string("model.epicWeb.unsupportedPage")
        case (.gog, .unsupportedPage): L10n.string("model.gogWeb.unsupportedPage")
        }
    }
}

extension WebSignInStore {
    /// Where its sign-in page keeps its cookies. GOG's site cannot be signed
    /// in from the app's token: its sign-in keeps them for its store pages,
    /// until the account signs out. Epic's sign-in keeps nothing.
    @MainActor var dataStore: WKWebsiteDataStore { site ?? .nonPersistent() }

    @MainActor func forgetSite() {
        site?.removeData(ofTypes: WKWebsiteDataStore.allWebsiteDataTypes(), modifiedSince: .distantPast) {}
    }

    @MainActor private var site: WKWebsiteDataStore? {
        switch self {
        case .epic: nil
        case .gog: Self.gogSite
        }
    }

    @MainActor private static let gogSite = WKWebsiteDataStore(
        forIdentifier: UUID(uuidString: "5C6F6B0E-3E0B-4C55-9B57-0D5B9E2A4F61")!)
}

enum WebAuthenticationError: LocalizedError {
    case invalidResponse(WebSignInStore)
    case network(WebSignInStore)
    case unsupportedPage(WebSignInStore)

    var errorDescription: String? {
        switch self {
        case let .invalidResponse(store): store.string(.invalidResponse)
        case let .network(store): store.string(.network)
        case let .unsupportedPage(store): store.string(.unsupportedPage)
        }
    }
}

/// How one storefront's web sign-in returns its authorization code.
struct WebSignIn: Sendable {
    let store: WebSignInStore
    let loginURL: @Sendable () -> URL
    /// Fixed for the protocol, including its popups, when the storefront
    /// recognizes its launcher by it.
    var userAgent: String?
    /// The code a main-frame navigation carries, or nil when it carries
    /// none; it throws when the navigation is the callback but malformed.
    let code: @MainActor @Sendable (URL) throws -> String?
    /// A page whose JSON body carries the code, never shown.
    var isJSONEndpoint: @MainActor @Sendable (URL) -> Bool = { _ in false }
    var codeFromJSON: @MainActor @Sendable (String, URL) throws -> String = { _, _ in
        throw CocoaError(.featureUnsupported)
    }

    static let epic = WebSignIn(
        store: .epic,
        loginURL: { URL(string: epicLoginUrl())! },
        // Epic uses the launcher marker to return an authorization code to
        // localhost.
        userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) EpicGamesLauncher",
        code: EpicAuthorizationCallback.code(from:),
        isJSONEndpoint: EpicAuthorizationCallback.isJSONEndpoint,
        codeFromJSON: EpicAuthorizationCallback.code(fromJSON:at:)
    )

    static let gog = WebSignIn(
        store: .gog,
        loginURL: { URL(string: gogLoginUrl())! },
        code: { url in
            guard gogIsLoginRedirect(url: url.absoluteString) else { return nil }
            return try gogAuthorizationCode(input: url.absoluteString)
        }
    )
}

/// Owns one login attempt and its popup windows. No callback is registered with
/// the OS: only navigations made inside this attempt can complete the login.
@MainActor
final class WebAuthentication: NSObject, WKNavigationDelegate, WKUIDelegate, NSWindowDelegate {
    private let flow: WebSignIn
    private var continuation: CheckedContinuation<String, Error>?
    private var windows: [LoginWindow] = []

    init(_ flow: WebSignIn) {
        self.flow = flow
    }

    static func authorize(_ flow: WebSignIn) async throws -> String {
        let attempt = WebAuthentication(flow)
        return try await attempt.authorize { webView in
            webView.load(URLRequest(url: flow.loginURL()))
        }
    }

    /// The loader allows WebKit integration tests to use local fixture pages.
    func authorize(load: @escaping @MainActor (WKWebView) -> Void) async throws -> String {
        return try await withTaskCancellationHandler {
            try Task.checkCancellation()
            return try await withCheckedThrowingContinuation { continuation in
                self.continuation = continuation
                self.start(load: load)
            }
        } onCancel: {
            Task { @MainActor in self.finish(.failure(CancellationError())) }
        }
    }

    private func start(load: @MainActor (WKWebView) -> Void) {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = flow.store.dataStore
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = true
        let window = makeWindow(configuration: configuration, primary: true)
        load(window.webView)
    }

    private func makeWindow(configuration: WKWebViewConfiguration, primary: Bool) -> LoginWindow {
        let window = LoginWindow(configuration: configuration, store: flow.store, primary: primary)
        window.delegate = self
        window.webView.navigationDelegate = self
        window.webView.uiDelegate = self
        window.webView.customUserAgent = flow.userAgent
        windows.append(window)
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        return window
    }

    private func finish(_ result: Result<String, Error>) {
        guard let continuation else { return }
        self.continuation = nil
        let closing = windows
        windows = []
        for window in closing {
            window.delegate = nil
            window.webView.navigationDelegate = nil
            window.webView.uiDelegate = nil
            window.webView.stopLoading()
            window.close()
        }
        continuation.resume(with: result)
    }

    private func window(for webView: WKWebView) -> LoginWindow? {
        windows.first { $0.webView === webView }
    }

    func windowWillClose(_ notification: Notification) {
        guard let closed = notification.object as? LoginWindow,
              windows.contains(where: { $0 === closed }) else { return }
        if closed.isPrimary {
            finish(.failure(CancellationError()))
        } else {
            closed.webView.stopLoading()
            closed.webView.navigationDelegate = nil
            closed.webView.uiDelegate = nil
            windows.removeAll { $0 === closed }
            windows.first?.makeKeyAndOrderFront(nil)
        }
    }

    func webView(
        _ webView: WKWebView,
        decidePolicyFor navigationAction: WKNavigationAction,
        decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void
    ) {
        guard continuation != nil, window(for: webView) != nil,
              let url = navigationAction.request.url else {
            decisionHandler(.cancel)
            return
        }
        if navigationAction.targetFrame?.isMainFrame == true {
            do {
                if let code = try flow.code(url) {
                    decisionHandler(.cancel)
                    finish(.success(code))
                    return
                }
            } catch {
                decisionHandler(.cancel)
                finish(.failure(WebAuthenticationError.invalidResponse(flow.store)))
                return
            }
            // Never show the raw JSON endpoint, even briefly, if the
            // storefront returns that variant instead of its callback.
            if flow.isJSONEndpoint(url) {
                window(for: webView)?.showProgress(flow.store.string(.connecting))
            }
        }
        // Cloudflare's captcha, which holds GOG's sign-in button disabled
        // until it hands over its token, builds its frames as `about:srcdoc`.
        guard url.scheme == "https" || url.absoluteString == "about:blank"
                || (url.absoluteString == "about:srcdoc" && navigationAction.targetFrame?.isMainFrame == false) else {
            decisionHandler(.cancel)
            if navigationAction.targetFrame?.isMainFrame == true {
                finish(.failure(WebAuthenticationError.unsupportedPage(flow.store)))
            }
            return
        }
        decisionHandler(.allow)
    }

    func webView(
        _ webView: WKWebView,
        decidePolicyFor navigationResponse: WKNavigationResponse,
        decisionHandler: @escaping @MainActor @Sendable (WKNavigationResponsePolicy) -> Void
    ) {
        guard continuation != nil, window(for: webView) != nil,
              navigationResponse.canShowMIMEType else {
            decisionHandler(.cancel)
            if continuation != nil { finish(.failure(WebAuthenticationError.unsupportedPage(flow.store))) }
            return
        }
        if navigationResponse.isForMainFrame,
           let url = navigationResponse.response.url,
           flow.isJSONEndpoint(url) {
            window(for: webView)?.showProgress(flow.store.string(.connecting))
        }
        decisionHandler(.allow)
    }

    func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) {
        guard continuation != nil, let window = window(for: webView), let url = webView.url else { return }
        window.setHost(url.host)
        if !flow.isJSONEndpoint(url) { window.showPage() }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        guard continuation != nil, let window = window(for: webView), let url = webView.url else { return }
        guard flow.isJSONEndpoint(url) else {
            window.showPage()
            return
        }
        // Evaluate only the final, allowlisted authorization response. The
        // script never runs on sign-in forms and never reads passwords/cookies.
        webView.evaluateJavaScript("document.body.innerText") { [weak self, weak webView] body, error in
            guard let self, let webView, self.continuation != nil,
                  self.window(for: webView) != nil, webView.url == url else { return }
            guard error == nil, let json = body as? String else {
                self.finish(.failure(WebAuthenticationError.invalidResponse(self.flow.store)))
                return
            }
            do {
                let code = try self.flow.codeFromJSON(json, url)
                self.finish(.success(code))
            } catch {
                self.finish(.failure(WebAuthenticationError.invalidResponse(self.flow.store)))
            }
        }
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        failNavigation(webView, error: error)
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        failNavigation(webView, error: error)
    }

    private func failNavigation(_ webView: WKWebView, error: Error) {
        guard continuation != nil, window(for: webView) != nil else { return }
        let error = error as NSError
        if error.domain == NSURLErrorDomain && error.code == NSURLErrorCancelled { return }
        finish(.failure(WebAuthenticationError.network(flow.store)))
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        if window(for: webView) != nil { finish(.failure(WebAuthenticationError.network(flow.store))) }
    }

    func webView(
        _ webView: WKWebView,
        createWebViewWith configuration: WKWebViewConfiguration,
        for navigationAction: WKNavigationAction,
        windowFeatures: WKWindowFeatures
    ) -> WKWebView? {
        guard continuation != nil, window(for: webView) != nil,
              navigationAction.targetFrame == nil, windows.count < 4,
              let url = navigationAction.request.url,
              url.scheme == "https" || url.absoluteString == "about:blank" else { return nil }
        // Use WebKit's supplied configuration so popup sign-in retains its
        // opener and the same ephemeral cookie store.
        return makeWindow(configuration: configuration, primary: false).webView
    }

    func webViewDidClose(_ webView: WKWebView) {
        window(for: webView)?.close()
    }
}

@MainActor
private final class LoginWindow: NSWindow {
    let webView: WKWebView
    let isPrimary: Bool
    private let store: WebSignInStore
    private let hostLabel = NSTextField(labelWithString: "")
    private let loading = NSProgressIndicator()
    private let statusLabel = NSTextField(labelWithString: "")
    private let progress = NSStackView()

    init(configuration: WKWebViewConfiguration, store: WebSignInStore, primary: Bool) {
        webView = WKWebView(frame: .zero, configuration: configuration)
        isPrimary = primary
        self.store = store
        super.init(
            contentRect: NSRect(x: 0, y: 0, width: 540, height: 720),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        title = store.string(.title)
        isReleasedWhenClosed = false
        minSize = NSSize(width: 480, height: 560)
        let content = NSView()
        contentView = content
        hostLabel.stringValue = store.string(.host)
        hostLabel.font = .systemFont(ofSize: 12, weight: .medium)
        hostLabel.textColor = .secondaryLabelColor
        hostLabel.alignment = .center
        hostLabel.lineBreakMode = .byTruncatingMiddle
        loading.style = .spinning
        loading.controlSize = .regular
        statusLabel.font = .systemFont(ofSize: 13)
        statusLabel.textColor = .secondaryLabelColor
        progress.orientation = .vertical
        progress.alignment = .centerX
        progress.spacing = 16
        progress.addArrangedSubview(loading)
        progress.addArrangedSubview(statusLabel)
        for view in [hostLabel, webView, progress] {
            view.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(view)
        }
        NSLayoutConstraint.activate([
            hostLabel.topAnchor.constraint(equalTo: content.topAnchor, constant: 10),
            hostLabel.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 16),
            hostLabel.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -16),
            webView.topAnchor.constraint(equalTo: hostLabel.bottomAnchor, constant: 10),
            webView.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            webView.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            webView.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            progress.centerXAnchor.constraint(equalTo: webView.centerXAnchor),
            progress.centerYAnchor.constraint(equalTo: webView.centerYAnchor),
        ])
        showProgress(store.string(.loading))
    }

    func setHost(_ host: String?) { hostLabel.stringValue = host ?? store.string(.host) }

    func showProgress(_ message: String) {
        webView.isHidden = true
        statusLabel.stringValue = message
        progress.isHidden = false
        loading.startAnimation(nil)
    }

    func showPage() {
        webView.isHidden = false
        progress.isHidden = true
        loading.stopAnimation(nil)
    }
}
