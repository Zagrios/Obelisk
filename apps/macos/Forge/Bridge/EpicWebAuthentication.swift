import AppKit
import WebKit

enum EpicWebAuthenticationError: LocalizedError {
    case invalidResponse
    case network
    case unsupportedPage

    var errorDescription: String? {
        switch self {
        case .invalidResponse:
            L10n.string("model.epicWeb.invalidResponse")
        case .network:
            L10n.string("model.epicWeb.network")
        case .unsupportedPage:
            L10n.string("model.epicWeb.unsupportedPage")
        }
    }
}

/// Owns one login attempt and its popup windows. No callback is registered with
/// the OS: only navigations made inside this attempt can complete the login.
@MainActor
final class EpicWebAuthentication: NSObject, WKNavigationDelegate, WKUIDelegate, NSWindowDelegate {
    private var continuation: CheckedContinuation<String, Error>?
    private var windows: [LoginWindow] = []

    static func authorize() async throws -> String {
        let attempt = EpicWebAuthentication()
        return try await attempt.authorize { webView in
            webView.load(URLRequest(url: URL(string: epicLoginUrl())!))
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
        configuration.websiteDataStore = .nonPersistent()
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = true
        let window = makeWindow(configuration: configuration, primary: true)
        load(window.webView)
    }

    private func makeWindow(configuration: WKWebViewConfiguration, primary: Bool) -> LoginWindow {
        let window = LoginWindow(configuration: configuration, primary: primary)
        window.delegate = self
        window.webView.navigationDelegate = self
        window.webView.uiDelegate = self
        // Epic uses the launcher marker to return an authorization code to
        // localhost. Keep it fixed for this protocol, including its popups.
        window.webView.customUserAgent = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) EpicGamesLauncher"
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
                if let code = try EpicAuthorizationCallback.code(from: url) {
                    decisionHandler(.cancel)
                    finish(.success(code))
                    return
                }
            } catch {
                decisionHandler(.cancel)
                finish(.failure(EpicWebAuthenticationError.invalidResponse))
                return
            }
            // Never show the raw JSON endpoint, even briefly, if Epic returns
            // that variant instead of navigating directly to localhost.
            if EpicAuthorizationCallback.isJSONEndpoint(url) {
                window(for: webView)?.showProgress(L10n.string("model.epicWeb.connecting"))
            }
        }
        guard url.scheme == "https" || url.absoluteString == "about:blank" else {
            decisionHandler(.cancel)
            if navigationAction.targetFrame?.isMainFrame == true {
                finish(.failure(EpicWebAuthenticationError.unsupportedPage))
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
            if continuation != nil { finish(.failure(EpicWebAuthenticationError.unsupportedPage)) }
            return
        }
        if navigationResponse.isForMainFrame,
           let url = navigationResponse.response.url,
           EpicAuthorizationCallback.isJSONEndpoint(url) {
            window(for: webView)?.showProgress(L10n.string("model.epicWeb.connecting"))
        }
        decisionHandler(.allow)
    }

    func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) {
        guard continuation != nil, let window = window(for: webView), let url = webView.url else { return }
        window.setHost(url.host)
        if !EpicAuthorizationCallback.isJSONEndpoint(url) { window.showPage() }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        guard continuation != nil, let window = window(for: webView), let url = webView.url else { return }
        guard EpicAuthorizationCallback.isJSONEndpoint(url) else {
            window.showPage()
            return
        }
        // Evaluate only the final, allowlisted authorization response. The
        // script never runs on sign-in forms and never reads passwords/cookies.
        webView.evaluateJavaScript("document.body.innerText") { [weak self, weak webView] body, error in
            guard let self, let webView, self.continuation != nil,
                  self.window(for: webView) != nil, webView.url == url else { return }
            guard error == nil, let json = body as? String else {
                self.finish(.failure(EpicWebAuthenticationError.invalidResponse))
                return
            }
            do {
                let code = try EpicAuthorizationCallback.code(fromJSON: json, at: url)
                self.finish(.success(code))
            } catch {
                self.finish(.failure(EpicWebAuthenticationError.invalidResponse))
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
        finish(.failure(EpicWebAuthenticationError.network))
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        if window(for: webView) != nil { finish(.failure(EpicWebAuthenticationError.network)) }
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
    private let hostLabel = NSTextField(labelWithString: "epicgames.com")
    private let loading = NSProgressIndicator()
    private let statusLabel = NSTextField(labelWithString: L10n.string("model.epicWeb.loading"))
    private let progress = NSStackView()

    init(configuration: WKWebViewConfiguration, primary: Bool) {
        webView = WKWebView(frame: .zero, configuration: configuration)
        isPrimary = primary
        super.init(
            contentRect: NSRect(x: 0, y: 0, width: 540, height: 720),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        title = L10n.string("model.epicWeb.title")
        isReleasedWhenClosed = false
        minSize = NSSize(width: 480, height: 560)
        let content = NSView()
        contentView = content
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
        showProgress(L10n.string("model.epicWeb.loading"))
    }

    func setHost(_ host: String?) { hostLabel.stringValue = host ?? L10n.string("model.epicWeb.epicGames") }

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
