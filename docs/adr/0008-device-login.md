# 0008: Device login with state and PKCE instead of a token in a deep link

Status: accepted (2026-11-06)

## Context

A deep link that carries a sign-in token, with no state or nonce tying it to a login the app started, can be opened by any website or app, so a victim can be signed into an attacker's account (login CSRF), after which their transcripts flow to the attacker.

## Decision

- The app creates a random `state` and a PKCE verifier, keeps them in memory with a 5-minute expiry, and opens the system browser at the backend login page with `state` and the PKCE challenge.
- After login the backend redirects to `sotto://auth/callback?code=…&state=…`. The code is single use and expires in 60 seconds.
- The app ignores any callback whose `state` does not match a pending request created by the running process, then exchanges the code plus verifier at the backend for tokens.
- Callbacks are handled by the Rust core only; the webview never sees codes or tokens.

## Consequences

- An unsolicited link does nothing. A leaked code is useless without the verifier.
- Login requires a browser round trip; there is no "paste a token" path.

## Alternatives

- Loopback redirect (`http://127.0.0.1:<port>`): also valid and avoids the custom scheme, but needs a listener; kept as an option for the Google Calendar flow.
