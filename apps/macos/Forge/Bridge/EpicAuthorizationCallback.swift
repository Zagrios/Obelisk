import Foundation

/// Extracts authorization codes only from the two expected Epic login responses.
enum EpicAuthorizationCallback {
    private static let clientID = "34a02cf8f4414e29b15921876da36f9a"
    private static let maximumJSONBytes = 32_768

    static func code(from url: URL) throws -> String? {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              hasHTTPSOrigin(components, hosts: ["localhost"]),
              components.percentEncodedPath == "/launcher/authorized" else { return nil }
        guard hasNoCredentialsOrFragment(components) else {
            throw EpicWebAuthenticationError.invalidResponse
        }
        let codes = (components.queryItems ?? []).filter { $0.name == "code" }
        guard codes.count == 1, let code = codes.first?.value, !code.isEmpty else {
            throw EpicWebAuthenticationError.invalidResponse
        }
        return try validatedCode(code)
    }

    static func isJSONEndpoint(_ url: URL) -> Bool {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              hasHTTPSOrigin(components, hosts: ["www.epicgames.com", "epicgames.com"]),
              components.percentEncodedPath == "/id/api/redirect",
              hasNoCredentialsOrFragment(components) else { return false }
        let query = components.queryItems ?? []
        return query.count == 2
            && query.filter { $0.name == "clientId" && $0.value == clientID }.count == 1
            && query.filter { $0.name == "responseType" && $0.value == "code" }.count == 1
    }

    static func code(fromJSON json: String, at url: URL) throws -> String {
        guard isJSONEndpoint(url), json.utf8.count <= maximumJSONBytes else {
            throw EpicWebAuthenticationError.invalidResponse
        }
        let response: AuthorizationResponse
        do {
            response = try JSONDecoder().decode(AuthorizationResponse.self, from: Data(json.utf8))
        } catch {
            throw EpicWebAuthenticationError.invalidResponse
        }
        return try validatedCode(response.authorizationCode)
    }

    private static func hasHTTPSOrigin(_ components: URLComponents, hosts: Set<String>) -> Bool {
        components.scheme?.lowercased() == "https"
            && components.host.map { hosts.contains($0.lowercased()) } == true
            && (components.port == nil || components.port == 443)
    }

    private static func hasNoCredentialsOrFragment(_ components: URLComponents) -> Bool {
        components.user == nil && components.password == nil && components.fragment == nil
    }

    private static func validatedCode(_ code: String) throws -> String {
        // The Rust entry point also accepts pasted URLs and JSON. Values
        // extracted from a callback must already be a code themselves.
        guard (try? epicAuthorizationCode(input: code)) == code else {
            throw EpicWebAuthenticationError.invalidResponse
        }
        return code
    }

    private struct AuthorizationResponse: Decodable {
        let authorizationCode: String
    }
}
