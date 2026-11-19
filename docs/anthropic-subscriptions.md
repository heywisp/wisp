# Anthropic subscriptions

Wisp's explicit `anthropic_subscription` authentication mode sends native
Anthropic Messages requests to `https://api.anthropic.com`. It uses the
Manifest-compatible Claude Code / Agent SDK request profile selected for this
integration. Wisp continues to run the conversation, tools and agent tree.
A signed-in subscription also sends `GET /v1/models` with the same profile
headers. Interactive Wisp refreshes the list hourly after success, or at the
first background check at least about 15 minutes after failure. A missing model
can prompt an earlier fetch (`docs/endpoints.md`, "Each account's model list").

This profile is derived from the reference implementation. Offline tests
verify its requests and credential lifecycle. Acceptance by Anthropic, account
entitlements and which billing balance a request draws from require a real
account check and remain unverified. Known models show the API-equivalent estimate marked `~` (`~$0.129`), not a charge. `pricing = "none"` keeps tokens only. Token counts do not establish remaining subscription allowances. `/usage` displays separately fetched allowance windows when `claude_usage` is enabled.

## Connect

Run `wisp login claude` for browser OAuth; the account is named `claude`
unless `--name` says otherwise. Wisp shows Claude's sign-in link, tries the
browser and copies the link, then asks for the code the page shows: paste it
with the page's Copy button. The code's `#state` part is checked against the
pending sign-in before any request,
exactly as strictly as before, and a paste mistake asks again on the same
sign-in instead of ending it ([Endpoints](endpoints.md), "Subscription login and
status"). The echo is masked. The flow expires after ten minutes. Codes,
verifiers and token responses are never printed, logged or sent in an event.

Alternatively, generate a setup token with `claude setup-token` and run
`wisp account add claude-token --provider anthropic --auth anthropic_subscription`.
The terminal prompt hides the token. Add `--key-stdin` to provide it through a
pipe instead. A setup token has no refresh token, so Wisp
keeps using it until it is replaced or refused. Wisp does not infer its expiry
from the time it was pasted.

Wisp reads no Claude Code, Manifest or other application's credential state.
Both paths require the user to supply credentials explicitly. API keys and
ordinary bearer gateway tokens retain their existing behavior; a token prefix
never silently selects subscription authentication.

## Credential lifecycle

Browser OAuth uses the reference's public client ID, PKCE S256 and the manual
callback at `https://console.anthropic.com/oauth/code/callback`. Authorization
opens `https://claude.ai/oauth/authorize`; code exchange and refresh use JSON
requests to `https://api.anthropic.com/v1/oauth/token`. The scope is
`org:create_api_key user:profile user:inference`. Wisp performs no profile or
organization lookups and sends no account IDs, names, emails or machine IDs.
The token answer's account and organization IDs are read only to refuse a second
name for an account already signed in: Wisp keeps a SHA-256 of the pair beside
the profile (`oauth-<id>.account`) and never stores, logs or sends the IDs.
Wisp generates state independently of the PKCE verifier, keeping the verifier
out of the authorization URL. Acceptance of this safer state handling remains
part of the unverified live flow.

Managed credentials are resolved before each request. Refresh is serialized
and persisted before the new access token is used; an omitted replacement
refresh token preserves the previous one. Refresh failure ends the request
with a typed authentication error. It never switches to an API key or another
endpoint. Provider authentication bodies are replaced with safe Wisp wording.
A refusal of the request itself (an invalid request, a request too large or not
found) keeps Anthropic's own text, cut to 2,000 characters, with the sign-in
tokens Wisp manages and anything shaped like a key or an account ID replaced by
`[redacted]`; sign-in, permission, billing, rate-limit and server failures keep
the safe wording, and the request ID stays cleared.
When the token endpoint refuses a refresh with 400 or 401, Wisp writes an empty
owner-only mark beside the profile (`oauth-<id>.refused`), named only by the
profile's random ID; a 429, a 5xx, no network or a bad answer writes none. The
account then reads `sign-in expired` everywhere, and a failed reply offers
"Sign in again, then retry". A successful save of that profile, logout, forget
and a store move remove the mark; failing to write or remove it never changes
the refresh's result.

For browser OAuth, use `wisp status claude` to see the sign-in's state and its
limits without exposing secrets and `wisp logout claude` to delete the endpoint and saved browser
credentials. `wisp logout claude-token` also removes a static-token endpoint and
its locally stored token, without an OAuth-profile operation. Defaults and preset
references are refused without a prompt; change those references first. Removal
writes config before deleting credentials. Failed cleanup is reported separately
and does not restore the removed endpoint.
Anthropic has no revocation
endpoint in the inspected reference; local removal does not claim server-side
revocation.

Subscription inference has a fixed official HTTPS origin. Custom URLs, URL
variables, stored URL overrides and redirects are refused. Token counting and
the free endpoint probe are not offered for this mode. An unsupported counting
endpoint does not make an inference token invalid. Hostname alone establishes
no subscription feature entitlement; Wisp's existing feature trial and refusal
handling remain in use.

## Request contract

The shipped identity is in `[providers.anthropic.claude_code]` in
[`crates/wisp-core/data/models.toml`](../crates/wisp-core/data/models.toml).
It holds the user-agent templates, ordered beta flags and compatibility headers.
The fallback version and release source are in `[providers.anthropic.client]`.
Wisp appends its requested feature flags without duplicates;
these include the message-thread flag on a thread's requests
(`docs/context.md`, "Message threads").

Inference and usage lookup keep the version read when the connection was made.
The model list can use a newer version immediately (`docs/endpoints.md`, "Client
versions"). Usage lookup sends only the betas marked for usage. Architecture
and OS are generic platform names. No user or machine identifier is generated.

Every subscription request starts its system blocks with:

> You are a Claude agent, built on Anthropic's Claude Agent SDK.

This is the requested compatibility profile. Wisp itself remains a Rust
kernel and does not invoke the Agent SDK. The identity is inserted before
serialization on the first request and retained unchanged on later requests;
tokens, expiry and login state never join the prompt. Cache fingerprints and
tail-marker comparisons include the actual rendered identity and beta flags.
Existing one-hour system/tool markers and the session's tail TTL stay intact.
Tools, signed thinking, native opaque blocks and append-only history use the
existing Anthropic serializer and SSE parser.

## Evidence and validation

The reported version follows Claude Code's releases, not the Manifest reference
checkout. The identity values and their provenance live in the shipped data row
linked under "Request contract"; updating them needs no provider-code change.
The research report (`research/anthropic-subscription.md`) records the original
Manifest observations and its inspected revision as dated evidence, and
distinguishes those observations from provider policy and runtime assumptions.

Offline fixtures cover the exact profile headers, stable system bytes across
turns and credential replacement, explicit tail markers, signed thinking and
unknown native blocks, typed sanitized authentication errors, and redirect
refusal. The common OAuth fixtures cover exchange, refresh and persistence.
These checks do not establish successful login, model entitlement, live cache
reuse or subscription billing. No real login or provider request is part of
implementation verification.

## Usage and terms

Anthropic's terms restrict third-party use of Claude subscription credentials.
The user accepts that risk for personal use. A future hosted Wisp must not offer
Claude login.

The separate `claude_usage` setting is on by default: a client displaying usage
looks up allowances. Set `claude_usage = false` in config, or open
`/config claude_usage` and switch the toggle off, to opt out. Disabling it cancels pending lookups and discards late
results; it does not affect login or inference. A chat with it off still shows a
reading another chat saved in the shared usage record, until its own next lookup.
The request reads only the fixed OAuth usage endpoint, with bearer authorization,
JSON Accept, the User-Agent `claude-code/<version>` (the version Wisp's subscription
User-Agent names), `x-app: cli`, and the OAuth beta. There are no profile or inventory
requests. Missing profile scope reports unavailable rather than zero usage.

Claude allowance displays omit the plan tier. Stored Claude credentials have no
tier field, and Wisp neither adds a profile lookup nor changes credential storage
to discover one. An alias such as `claude-max` is not evidence of a Max tier.
