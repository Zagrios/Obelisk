import AppKit
import WebKit
import XCTest
@testable import Forge

@MainActor
final class EpicWebAuthenticationTests: XCTestCase {
    func testWebKitNavigationReturnsCodeAndClosesWindowBeforeLocalhostLoads() async throws {
        let attempt = EpicWebAuthentication()
        var loginWindow: NSWindow?
        let completed = expectation(description: "Epic callback intercepted")
        let task = Task {
            defer { completed.fulfill() }
            return try await attempt.authorize { webView in
                loginWindow = webView.window
                webView.loadHTMLString("""
                    <html><body><script>
                    window.location.href = 'https://localhost/launcher/authorized?code=fixture-code';
                    </script></body></html>
                    """, baseURL: nil)
            }
        }
        await fulfillment(of: [completed], timeout: 10)
        // Also prevents a regression from leaving the test suspended forever.
        task.cancel()
        let code = try await task.value
        XCTAssertEqual(code, "fixture-code")
        XCTAssertNotNil(loginWindow)
        XCTAssertEqual(loginWindow?.isVisible, false)
    }

    func testEpicJSONResponseIsCapturedWithoutShowingTheTechnicalPage() async throws {
        let completed = expectation(description: "Epic JSON consumed")
        var loginWebView: WKWebView?
        let task = Task {
            defer { completed.fulfill() }
            return try await EpicWebAuthentication().authorize { webView in
                loginWebView = webView
                let url = URL(string: "https://www.epicgames.com/id/api/redirect?clientId=34a02cf8f4414e29b15921876da36f9a&responseType=code")!
                webView.loadHTMLString("<html><body><pre>{\"authorizationCode\":\"fixture-json\"}</pre></body></html>", baseURL: url)
            }
        }
        await fulfillment(of: [completed], timeout: 10)
        task.cancel()
        let code = try await task.value
        XCTAssertEqual(code, "fixture-json")
        XCTAssertEqual(loginWebView?.isHidden, true)
    }

    func testPopupCanCompleteTheSameAuthorizationAttempt() async throws {
        let completed = expectation(description: "Popup callback intercepted")
        let task = Task {
            defer { completed.fulfill() }
            return try await EpicWebAuthentication().authorize { webView in
                webView.loadHTMLString("""
                    <html><body><script>
                    const popup = window.open('about:blank');
                    popup.location.href = 'https://localhost/launcher/authorized?code=fixture-popup';
                    </script></body></html>
                    """, baseURL: nil)
            }
        }
        await fulfillment(of: [completed], timeout: 10)
        task.cancel()
        let code = try await task.value
        XCTAssertEqual(code, "fixture-popup")
    }

    func testClosingWindowCancelsAuthorization() async throws {
        let opened = expectation(description: "Epic window opened")
        var loginWindow: NSWindow?
        let task = Task {
            try await EpicWebAuthentication().authorize { webView in
                loginWindow = webView.window
                opened.fulfill()
            }
        }
        await fulfillment(of: [opened], timeout: 5)
        XCTAssertEqual(loginWindow?.isVisible, true)
        loginWindow?.performClose(nil)
        do {
            _ = try await task.value
            XCTFail("Closing the login window must cancel its authorization")
        } catch is CancellationError {
        }
        XCTAssertEqual(loginWindow?.isVisible, false)
    }

    func testTaskCancellationClosesWindowAndDiscardsItsPage() async throws {
        let opened = expectation(description: "Epic window opened")
        var loginWindow: NSWindow?
        let task = Task {
            try await EpicWebAuthentication().authorize { webView in
                loginWindow = webView.window
                XCTAssertFalse(webView.configuration.websiteDataStore.isPersistent)
                opened.fulfill()
            }
        }
        await fulfillment(of: [opened], timeout: 5)
        task.cancel()
        do {
            _ = try await task.value
            XCTFail("Cancelling the task must cancel the browser login")
        } catch is CancellationError {
        }
        XCTAssertEqual(loginWindow?.isVisible, false)
    }
}
