# Steam sign-in in Rust

Status verified on September 15, 2026.

## Protocol references

The SteamRE/SteamKit repository was reviewed at commit
`9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4`:

- [SteamAuthentication.cs](https://github.com/SteamRE/SteamKit/blob/9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4/SteamKit2/SteamKit2/Steam/Authentication/SteamAuthentication.cs) covers starting QR authentication, the RSA key, PKCS#1 v1.5 encryption, and starting authentication with credentials.
- [AuthSession.cs](https://github.com/SteamRE/SteamKit/blob/9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4/SteamKit2/SteamKit2/Steam/Authentication/AuthSession.cs) covers the polling interval, available confirmations, and client ID changes.
- [CredentialsAuthSession.cs](https://github.com/SteamRE/SteamKit/blob/9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4/SteamKit2/SteamKit2/Steam/Authentication/CredentialsAuthSession.cs) covers submitting a Steam Guard code and the `DuplicateRequest` result after mobile approval.
- [QrAuthSession.cs](https://github.com/SteamRE/SteamKit/blob/9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4/SteamKit2/SteamKit2/Steam/Authentication/QrAuthSession.cs) covers QR challenge replacement during polling.
- [SteamMsgAuth.cs](https://github.com/SteamRE/SteamKit/blob/9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4/SteamKit2/SteamKit2/Base/Generated/SteamMsgAuth.cs) defines protobuf field numbers and types, along with enum values. Steam IDs do not all use the same encoding: `fixed64` for the Guard code and `uint64` for the credentials response.
- [WebAPI.cs](https://github.com/SteamRE/SteamKit/blob/9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4/SteamKit2/SteamKit2/Steam/WebAPI/WebAPI.cs) covers HTTPS transport, the `input_protobuf_encoded` form, the `protobuf_raw` format, and the result in the `x-eresult` header.

SteamKit is distributed under [LGPL-2.1](https://github.com/SteamRE/SteamKit/blob/9c7f3c3fab6a8cf4ddd6cc7fe11bee70154e82d4/LICENSE).
Forge bundles neither its assembly, executable, nor protocol generator.
The Rust module was written for Forge from protocol observations. Its protobuf
declarations contain only the fields used. The references above are retained to
track protocol changes.

## Flow

The client uses `https://api.steampowered.com/IAuthenticationService` and the
`SteamClient` profile to obtain a refresh token accepted by the Connection
Manager. No application secret or personal Web API key is required.

1. **QR**: `BeginAuthSessionViaQR` returns the `https://s.team/q/…` challenge,
   the private request identifier, and the polling interval. SwiftUI draws the
   QR code, and the user approves the request in Steam mobile.
2. **Credentials**: `GetPasswordRSAPublicKey` obtains the public key and its
   timestamp. The UTF-8 password is encrypted with RSA PKCS#1 v1.5 before
   `BeginAuthSessionViaCredentials`. The session retains only tracking data,
   never the password.
3. **Steam Guard**: the server supplies the allowed options: a mobile
   notification, a mobile code, or an emailed code. `submit_guard` rejects an
   option not offered for this session. An incorrect code can be resubmitted.
   Mobile approval received in the meantime is handled on the next poll.
4. **Polling**: the caller respects `polling_interval()`. The module replaces
   the client ID and QR code if Steam renews them. It returns tokens only after
   the server approves the request.
5. **Restoration**: the refresh token can be saved in the macOS Keychain.
   `AuthTokens::restore` extracts only the routing identifier from the JWT.
   This read does not validate its signature. The Connection Manager performs
   the actual token verification when connecting.

Public sessions and tokens do not implement `Debug`. Network errors include no
URLs, forms, response bodies, passwords, codes, or tokens. HTTP responses are
limited to 1 MiB. The supplied HTTP client must validate TLS certificates and
disable redirects.

Account agreements requiring acceptance are reported to the user for resolution
in Steam. Legacy machine confirmations are not implemented. This module does
not replace multiplayer or Steamworks game APIs.

## Authentication dependencies

| Crate | Purpose |
| --- | --- |
| `reqwest` with `rustls-tls`, without default features | HTTPS, forms, and bounded response reading |
| `prost` | Encoding and decoding the protobuf subset |
| `base64` | Protobuf transport and token envelopes |
| `rsa`, without unused PEM/signature features | Password encryption required by Steam |
| `rand_core` with `getrandom` | System randomness for RSA padding |
| `serde_json` | Reading the routing token's `sub` field |
| `thiserror` | Typed errors shared across the project |

## Verification

`cargo test -p steam-kit auth::`: **8 tests passed**, with 1 network test ignored
by default. They check Steam Guard wire encoding, token-bound identity, QR
rotation, rejected URLs, retryable errors, invalid JWTs, HTTP limits, and the
full credentials flow against a local server. That server decrypts the password
with a test private key to verify encryption, then simulates a rejected code,
concurrent mobile approval, and token issuance.

`cargo test -p steam-kit auth::tests::live_qr_handshake_and_pending_poll -- --ignored`
**passed against Steam**. Steam issued a real QR request and returned a pending
response after the required interval. No QR code or request identifier was
written to the logs. This test uses no account.

Completing sign-in with a real account, its Steam Guard methods, and reuse of its
token requires the user to approve the flow in the interface. The account-free
network test does not prove these steps work.
