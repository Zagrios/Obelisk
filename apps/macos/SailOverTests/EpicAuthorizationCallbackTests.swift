import Foundation
import XCTest
@testable import SailOver

final class EpicAuthorizationCallbackTests: XCTestCase {
    private let code = "0123456789abcdef0123456789abcdef"
    private let clientID = "34a02cf8f4414e29b15921876da36f9a"

    func testLauncherCallbackAcceptsDefaultHTTPSPortAndDecodesCode() throws {
        for (address, expected) in [
            ("https://localhost/launcher/authorized?code=\(code)", code),
            ("https://localhost:443/launcher/authorized?code=\(code)&state=epic", code),
            ("https://localhost/launcher/authorized?code=abc%5F123-XYZ", "abc_123-XYZ"),
        ] {
            XCTAssertEqual(try EpicAuthorizationCallback.code(from: url(address)), expected)
        }
    }

    func testOrdinaryNavigationDoesNotExtractAuthorizationCodes() throws {
        for address in [
            "https://www.epicgames.com/id/login?code=\(code)",
            "https://localhost/launcher/authorized/extra?code=\(code)",
            "https://localhost/launcher/authorized/?code=\(code)",
            "https://localhost/launcher/%61uthorized?code=\(code)",
            "http://localhost/launcher/authorized?code=\(code)",
            "https://localhost:444/launcher/authorized?code=\(code)",
        ] {
            XCTAssertNil(try EpicAuthorizationCallback.code(from: url(address)))
        }
    }

    func testForeignOriginsAndHostLookalikesNeverSupplyCallbackCodes() throws {
        for host in ["localhost.example.com", "notlocalhost", "127.0.0.1", "example.com", "localhost."] {
            XCTAssertNil(try EpicAuthorizationCallback.code(from: url("https://\(host)/launcher/authorized?code=\(code)")))
        }
        XCTAssertNil(try EpicAuthorizationCallback.code(from: url("https://localhost@example.com/launcher/authorized?code=\(code)")))
    }

    func testRecognizedCallbackRejectsFragmentsAndUserInfo() {
        for address in [
            "https://localhost/launcher/authorized?code=\(code)#fragment",
            "https://localhost/launcher/authorized?code=\(code)#",
            "https://user@localhost/launcher/authorized?code=\(code)",
            "https://user:password@localhost/launcher/authorized?code=\(code)",
            "https://@localhost/launcher/authorized?code=\(code)",
        ] {
            assertInvalidResponse { _ = try EpicAuthorizationCallback.code(from: url(address)) }
        }
    }

    func testRecognizedCallbackRequiresOneValidNonemptyCode() {
        for query in [
            "", "code", "code=", "error=access_denied", "code=%20",
            "code=first&code=second", "code=first&co%64e=second",
            "code=first%0Asecond", "code=invalid%20code",
            "code=%20abc%20", "code=%7B%22authorizationCode%22%3A%22abc%22%7D",
        ] {
            assertInvalidResponse { _ = try EpicAuthorizationCallback.code(from: url("https://localhost/launcher/authorized?\(query)")) }
        }
    }

    func testJSONEndpointRequiresExpectedOriginPathAndUniqueClientQuery() {
        for address in [
            "https://www.epicgames.com/id/api/redirect?clientId=\(clientID)&responseType=code",
            "https://epicgames.com:443/id/api/redirect?responseType=code&clientId=\(clientID)",
        ] {
            XCTAssertTrue(EpicAuthorizationCallback.isJSONEndpoint(url(address)))
        }
        for address in [
            "http://www.epicgames.com/id/api/redirect?clientId=\(clientID)&responseType=code",
            "https://www.epicgames.com:444/id/api/redirect?clientId=\(clientID)&responseType=code",
            "https://www.epicgames.com.example.com/id/api/redirect?clientId=\(clientID)&responseType=code",
            "https://www.epicgames.com/id/api/redirect/extra?clientId=\(clientID)&responseType=code",
            "https://www.epicgames.com/id/api/%72edirect?clientId=\(clientID)&responseType=code",
            "https://user@www.epicgames.com/id/api/redirect?clientId=\(clientID)&responseType=code",
            "https://www.epicgames.com/id/api/redirect?clientId=\(clientID)&responseType=code#",
            "https://www.epicgames.com/id/api/redirect?clientId=other&responseType=code",
            "https://www.epicgames.com/id/api/redirect?clientId=\(clientID)",
            "https://www.epicgames.com/id/api/redirect?clientId=\(clientID)&responseType=token",
            "https://www.epicgames.com/id/api/redirect?clientId=\(clientID)&clientId=\(clientID)&responseType=code",
            "https://www.epicgames.com/id/api/redirect?clientId=\(clientID)&responseType=code&responseType=code",
            "https://www.epicgames.com/id/api/redirect?clientId=\(clientID)&responseType=code&extra=value",
        ] {
            XCTAssertFalse(EpicAuthorizationCallback.isJSONEndpoint(url(address)))
        }
    }

    func testJSONResponseExtractsCodeOnlyFromTheTrustedEndpoint() throws {
        let json = "{\"authorizationCode\":\"\(code)\",\"sid\":null}"
        XCTAssertEqual(try EpicAuthorizationCallback.code(fromJSON: json, at: jsonEndpoint), code)
        for address in [
            "https://example.com/id/api/redirect?clientId=\(clientID)&responseType=code",
            "https://www.epicgames.com/id/login",
            "https://localhost/launcher/authorized?code=\(code)",
        ] {
            assertInvalidResponse { _ = try EpicAuthorizationCallback.code(fromJSON: json, at: url(address)) }
        }
    }

    func testJSONResponseRejectsRawPageTextAndInvalidCodeTypes() {
        for json in [
            code, "\"\(code)\"", "<html>\(code)</html>", "{broken}", "[]", "{}",
            "{\"authorizationCode\":null}", "{\"authorizationCode\":123}",
            "{\"authorizationCode\":\"\"}", "{\"authorizationCode\":\"invalid code\"}",
            "{\"authorizationCode\":\"https://www.epicgames.com/id/api/redirect?code=abc\"}",
        ] {
            assertInvalidResponse { _ = try EpicAuthorizationCallback.code(fromJSON: json, at: jsonEndpoint) }
        }
    }

    func testJSONResponseLimitCountsUTF8BytesAndAcceptsBoundary() throws {
        let json = "{\"authorizationCode\":\"\(code)\"}"
        let boundary = json + String(repeating: " ", count: 32_768 - json.utf8.count)
        XCTAssertEqual(try EpicAuthorizationCallback.code(fromJSON: boundary, at: jsonEndpoint), code)
        assertInvalidResponse { _ = try EpicAuthorizationCallback.code(fromJSON: boundary + " ", at: jsonEndpoint) }

        let multibyte = "{\"authorizationCode\":\"\(code)\",\"padding\":\"\(String(repeating: "é", count: 17_000))\"}"
        XCTAssertLessThan(multibyte.count, 32_768)
        XCTAssertGreaterThan(multibyte.utf8.count, 32_768)
        assertInvalidResponse { _ = try EpicAuthorizationCallback.code(fromJSON: multibyte, at: jsonEndpoint) }
    }

    private var jsonEndpoint: URL {
        url("https://www.epicgames.com/id/api/redirect?clientId=\(clientID)&responseType=code")
    }

    private func url(_ value: String) -> URL { URL(string: value)! }

    private func assertInvalidResponse(
        file: StaticString = #filePath,
        line: UInt = #line,
        _ operation: () throws -> Void
    ) {
        XCTAssertThrowsError(try operation(), file: file, line: line) { error in
            guard case EpicWebAuthenticationError.invalidResponse = error else {
                return XCTFail("Expected a sanitized invalid response error", file: file, line: line)
            }
        }
    }
}
