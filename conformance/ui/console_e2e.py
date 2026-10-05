#!/usr/bin/env python3
"""Browser end-to-end check of the Rustybin web console (/ui).

Clicks through every view in light and dark mode, exercises the main flows
(live traffic + compare, request bin, API explorer, AI streaming for every
provider plus tool calling, MCP list + call with progress and the OAuth
discovery, A2A streaming and approval continuation, chaos load generator,
token lab) and fails on browser console errors or uncaught exceptions.

With RUSTYBIN_JWT_URL it also checks the sign-in flow of an instance with
RUSTYBIN_CONTROL_AUTH=jwt (sign-in screen, #token= fragment, fetch-based live
feed with the bearer token, expiry, sign out).

Usage (server already running):
    RUSTYBIN_URL=http://127.0.0.1:18700 python conformance/ui/console_e2e.py

Environment:
    RUSTYBIN_URL     base URL of the server (default http://127.0.0.1:18700)
    SCREENSHOT_DIR   where to write PNG screenshots (default ./screenshots)
    CHROMIUM_PATH    Chromium executable (default: Playwright's own, or the
                     first chrome found under $PLAYWRIGHT_BROWSERS_PATH)
    THEMES           comma list of light,dark (default both)
    HEADED=1         show the browser
    RUSTYBIN_JWT_URL          base URL of a RUSTYBIN_CONTROL_AUTH=jwt instance
                              (with RUSTYBIN_CONSOLE_TITLE and _BACKLINK set)
    CONTROL_JWT_PRIVATE_KEY   Ed25519 private key (PEM file) it trusts
    CONTROL_JWT_AUDIENCE      its RUSTYBIN_CONTROL_JWT_AUDIENCE
"""

import glob
import os
import re
import sys
import time
import urllib.request

from playwright.sync_api import sync_playwright, expect

BASE = os.environ.get("RUSTYBIN_URL", "http://127.0.0.1:18700").rstrip("/")
OUT = os.environ.get("SCREENSHOT_DIR", "screenshots")
THEMES = [t.strip() for t in os.environ.get("THEMES", "light,dark").split(",") if t.strip()]
TIMEOUT = 20_000

failures = []
console_errors = []


def chromium_path():
    if os.environ.get("CHROMIUM_PATH"):
        return os.environ["CHROMIUM_PATH"]
    root = os.environ.get("PLAYWRIGHT_BROWSERS_PATH", "/opt/pw-browsers")
    hits = sorted(glob.glob(os.path.join(root, "chromium-*", "chrome-linux", "chrome")))
    return hits[-1] if hits else None


def http(method, path, headers=None, body=None):
    req = urllib.request.Request(BASE + path, method=method, data=body.encode() if body else None, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


def step(name, fn):
    started = time.time()
    try:
        fn()
        print(f"  ok   {name} ({time.time() - started:.1f}s)")
    except Exception as e:  # noqa: BLE001 - report every failure and continue
        print(f"  FAIL {name}: {e}")
        failures.append(f"{name}: {e}")


def shot(page, theme, name):
    os.makedirs(OUT, exist_ok=True)
    page.screenshot(path=os.path.join(OUT, f"{name}-{theme}.png"), full_page=False)


def goto_view(page, view):
    page.evaluate(f"location.hash = '#/{view}'")
    page.wait_for_function(f"document.title.includes('Rustybin') && location.hash === '#/{view}'")
    page.wait_for_selector("#view .view-head h1", timeout=TIMEOUT)


def run_theme(browser, theme):
    print(f"theme: {theme}")
    ctx = browser.new_context(viewport={"width": 1440, "height": 900}, color_scheme=theme, device_scale_factor=1)
    page = ctx.new_page()
    page.set_default_timeout(TIMEOUT)

    def on_console(msg):
        if msg.type == "error":
            text = msg.text
            # Expected HTTP errors of demo calls (401 challenge, injected 5xx, ...)
            # are logged by Chromium as resource load failures: not console bugs.
            if "Failed to load resource" in text:
                return
            console_errors.append(f"[{theme}] {text}")

    page.on("console", on_console)
    page.on("pageerror", lambda e: console_errors.append(f"[{theme}] uncaught: {e}"))

    page.goto(BASE + "/ui", wait_until="domcontentloaded")
    # Fresh console settings for a deterministic run.
    page.evaluate("localStorage.clear()")
    page.goto(BASE + "/ui/#/overview")

    def overview():
        page.wait_for_selector("text=Requests captured")
        page.wait_for_selector("text=What Rustybin sees of this browser")
        page.wait_for_selector(".url-row code")
        expect(page.locator(".nav-link")).to_have_count(9)
        # No plan configured (RUSTYBIN_PLAN=none): no "Plan and usage" card.
        expect(page.locator("text=Plan and usage")).to_have_count(0)
        shot(page, theme, "01-overview")

    step("overview", overview)

    def live_traffic():
        goto_view(page, "traffic")
        page.wait_for_selector("text=live feed")
        tag = f"demo-{theme}"
        http("GET", f"/echo/{tag}?direct=1", {"User-Agent": "curl/8.5.0"})
        http("POST", f"/echo/{tag}?via=gateway", {
            "Content-Type": "application/json",
            "X-Forwarded-For": "203.0.113.7",
            "X-Forwarded-Proto": "https",
            "Via": "1.1 demo-gateway",
            "X-Consumer-Username": "acme-mobile",
            "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "User-Agent": "demo-gateway/3.9",
        }, '{"order": 42, "items": ["a", "b"]}')
        item = page.locator(".list-item", has_text=f"/echo/{tag}?via=gateway").first
        item.wait_for()
        item.click()
        page.wait_for_selector(".tag-gw")
        page.get_by_role("tab", name="Body").click()
        page.wait_for_selector(".code .j-key")
        page.get_by_role("tab", name="Headers").first.click()
        shot(page, theme, "02-traffic")
        # Compare mode keeps the selected request as A; pick the direct call as B.
        page.get_by_role("button", name="Compare", exact=True).click()
        page.locator(".list-item", has_text=f"/echo/{tag}?direct=1").first.click()
        page.wait_for_selector("text=header difference")
        shot(page, theme, "03-traffic-compare")
        page.get_by_role("button", name="Exit compare").click()

    step("live traffic + compare", live_traffic)

    def bins():
        goto_view(page, "bins")
        page.get_by_role("button", name="Create bin").click()
        page.wait_for_selector("text=Copy URL")
        page.get_by_role("button", name="Send test request").click()
        page.locator(".list-item", has_text="/webhook").first.wait_for()
        page.locator(".split .code", has_text="order.created").first.wait_for()
        shot(page, theme, "04-bins")

    step("request bin", bins)

    def explorer():
        goto_view(page, "explorer")
        page.wait_for_selector("text=Try it")
        page.get_by_label("Search endpoints").fill("/anything")
        page.locator(".list-item", has_text="/anything").first.click()
        page.get_by_role("button", name="Send", exact=True).click()
        page.wait_for_selector(".card .badge.ok")
        shot(page, theme, "05-explorer")

    step("api explorer", explorer)

    def ai():
        goto_view(page, "ai")
        for label in ["OpenAI chat", "OpenAI Responses", "Anthropic", "Gemini", "Ollama", "Bedrock"]:
            page.locator(".seg button", has_text=label).first.click()
            page.get_by_role("button", name="New chat").click()
            page.get_by_label("Message").fill(f"Hello from the {label} test")
            page.get_by_label("Message").press("Enter")
            page.locator(".msg.assistant .meta", has_text="tokens").first.wait_for(timeout=TIMEOUT)
            if page.locator(".msg.error").count():
                raise AssertionError(f"{label}: error bubble")
        # Tool calling round trip (OpenAI chat, streaming).
        page.locator(".seg button", has_text="OpenAI chat").first.click()
        page.get_by_role("button", name="New chat").click()
        cb = page.get_by_label("Offer the get_weather tool")
        if not cb.is_checked():
            cb.check()
        page.get_by_label("Message").fill("What is the weather in Paris?")
        page.get_by_label("Message").press("Enter")
        page.get_by_role("button", name="Send tool result").click()
        page.locator(".msg.assistant", has_text="tool results").first.wait_for()
        page.get_by_role("tab", name="Response").click()
        shot(page, theme, "06-ai-playground")

    step("ai playground (6 providers + tools)", ai)

    def mcp():
        goto_view(page, "mcp")
        page.get_by_role("button", name="Connect", exact=True).click()
        page.wait_for_selector("text=connected")
        page.get_by_role("button", name="slow_task").click()
        page.get_by_label("duration_ms").fill("1500")
        page.get_by_label("steps").fill("3")
        page.get_by_role("button", name="Call tool").click()
        page.wait_for_selector("text=step 1/3", timeout=TIMEOUT)
        page.wait_for_selector("text=done in")
        page.locator("text=done in").scroll_into_view_if_needed()
        shot(page, theme, "07-mcp")
        # 2026-07-28 multi round-trip: elicitation answered in the console.
        page.get_by_role("button", name="elicit_confirmation").click()
        page.get_by_role("button", name="Call tool").click()
        page.wait_for_selector("text=The server asks for input")
        page.get_by_label("confirm").select_option("true")
        page.get_by_role("button", name="Accept").click()
        page.wait_for_selector("text=done in")
        page.get_by_role("tab", name="Resources").click()
        page.locator(".list-item", has_text="rustybin://docs/readme").click()
        page.wait_for_selector("text=Rustybin MCP")
        # Session era.
        page.locator(".seg button", has_text="2025-11-25").click()
        page.get_by_role("button", name="Connect", exact=True).click()
        page.wait_for_selector("[title='Mcp-Session-Id']")
        # OAuth discovery for the protected variant.
        page.locator("select[aria-label='MCP endpoint']").select_option("/mcp/protected")
        page.locator(".seg button", has_text="2026-07-28").click()
        page.get_by_role("button", name="Get a token").click()
        page.get_by_role("button", name="Connect now").click()
        page.wait_for_selector("text=Rustybin MCP")
        page.get_by_role("tab", name="Tools").click()
        page.get_by_role("button", name="cancel_order").wait_for()
        shot(page, theme, "08-mcp-oauth")

    step("mcp inspector", mcp)

    def a2a():
        goto_view(page, "a2a")
        page.locator(".list-item", has_text="Travel Planner").first.click()
        page.get_by_label("Message").fill("Plan a 2 day trip to Kyoto")
        page.locator("input[type=number]").first.fill("100")
        page.get_by_role("button", name="Send", exact=True).click()
        page.locator(".badge", has_text="completed").first.wait_for(timeout=30_000)
        page.wait_for_selector("text=itinerary.json")
        shot(page, theme, "09-a2a-travel")
        page.locator(".list-item", has_text="Approval").first.click()
        page.get_by_label("Message").fill("expense 120 EUR team dinner")
        page.get_by_role("button", name="Send", exact=True).click()
        page.locator(".badge", has_text="input-required").first.wait_for()
        page.get_by_label("Message").fill("approve")
        page.get_by_role("button", name="Send", exact=True).click()
        page.locator(".tl-item .badge", has_text="completed").first.wait_for()
        shot(page, theme, "10-a2a-approval")
        # auth-required continuation with a token from the built-in IdP.
        page.locator(".list-item", has_text="Secure Agent").first.click()
        page.get_by_label("Message").fill("who am i")
        page.get_by_role("button", name="Send", exact=True).click()
        page.locator(".badge", has_text="auth-required").first.wait_for()
        page.get_by_role("button", name="Get token").click()
        expect(page.get_by_placeholder("Bearer token (secure agent)")).to_have_value(re.compile(r".{20,}"))
        page.get_by_role("button", name="Send", exact=True).click()
        page.locator(".tl-item .badge", has_text="completed").first.wait_for()

    step("a2a client", a2a)

    def chaos():
        goto_view(page, "chaos")
        page.wait_for_selector("text=GET /health answers 200")
        page.get_by_role("button", name="Send 10").click()
        page.wait_for_selector("text=Server counters for session")
        page.get_by_label("Path", exact=True).last.fill("/flaky/40")
        page.locator("input[type=number]").nth(2).fill("60")
        page.locator("input[type=number]").nth(3).fill("6")
        page.get_by_role("button", name="Run", exact=True).click()
        page.locator(".badge", has_text="finished").wait_for(timeout=60_000)
        page.wait_for_selector("text=Responses by status")
        page.locator("text=Latency per request").scroll_into_view_if_needed()
        shot(page, theme, "11-chaos")

    step("chaos and load generator", chaos)

    def tokens():
        goto_view(page, "tokens")
        page.get_by_role("button", name="Get token").click()
        page.get_by_role("button", name="Decode access token").click()
        page.wait_for_selector(".countdown")
        page.wait_for_selector("text=left")
        page.get_by_role("button", name="Introspect").click()
        page.locator(".j-key", has_text='"active"').first.wait_for()
        shot(page, theme, "12-token-lab")
        # Authorization code + PKCE in a popup, redirected back to /ui/callback.
        page.get_by_role("tab", name="OAuth tokens").click()
        page.locator(".seg button", has_text="Authorization code + PKCE").click()
        with page.expect_popup() as popup_info:
            page.get_by_role("button", name="Sign in").click()
        popup = popup_info.value
        popup.locator("input[name=username]").fill("alice")
        popup.locator("input[name=password]").fill("alice")
        popup.get_by_role("button", name="Authorize").click()
        page.get_by_role("button", name="Decode ID token").wait_for()
        page.get_by_role("tab", name="HMAC signer").click()
        page.get_by_role("button", name="Sign and send").click()
        page.locator(".j-key", has_text='"authenticated"').first.wait_for()
        page.get_by_role("tab", name="Webhook signatures").click()
        page.get_by_role("button", name="Sign", exact=True).click()
        page.get_by_role("button", name="Verify at /webhooks/verify").click()
        page.wait_for_selector("text=Signature valid.")

    step("token lab", tokens)

    def settings_and_theme():
        page.get_by_role("button", name="Console settings").click()
        page.get_by_label("Session (X-Rustybin-Session)").fill("e2e-session")
        page.get_by_role("button", name="Save").click()
        page.wait_for_selector("text=e2e-session")
        page.keyboard.press("Alt+1")
        page.wait_for_function("location.hash === '#/overview'")

    step("settings dialog + keyboard nav", settings_and_theme)

    def via_gateway():
        # A second origin for the same server stands in for a gateway with CORS.
        alt = BASE.replace("127.0.0.1", "localhost") if "127.0.0.1" in BASE else BASE.replace("localhost", "127.0.0.1")
        page.get_by_role("button", name="Console settings").click()
        page.get_by_label("Gateway base URL").fill(alt)
        page.get_by_role("button", name="Save").click()
        goto_view(page, "explorer")
        page.get_by_label("Search endpoints").fill("/anything")
        page.locator(".list-item", has_text="/anything").first.click()
        page.locator(".seg button", has_text="Via gateway").first.click()
        page.get_by_role("button", name="Send", exact=True).click()
        page.locator(".card .badge.ok").first.wait_for()
        # Unreachable gateway: a helpful message instead of a spinner.
        page.get_by_role("button", name="Console settings").click()
        page.get_by_label("Gateway base URL").fill("http://127.0.0.1:9")
        page.get_by_role("button", name="Save").click()
        page.get_by_role("button", name="Send", exact=True).click()
        page.wait_for_selector("text=Copy as curl")
        page.wait_for_selector("text=CORS")
        shot(page, theme, "14-gateway-error")
        page.get_by_role("button", name="Console settings").click()
        page.get_by_label("Gateway base URL").fill("")
        page.get_by_role("button", name="Save").click()

    step("send via gateway (CORS) + unreachable gateway", via_gateway)

    def spa_and_mobile():
        page.set_viewport_size({"width": 390, "height": 844})
        page.goto(BASE + "/ui/#/traffic")
        page.reload()
        page.wait_for_selector("#view h1")
        page.wait_for_timeout(400)
        overflow = page.evaluate("document.documentElement.scrollWidth - window.innerWidth")
        if overflow > 2:
            raise AssertionError(f"horizontal overflow of {overflow}px at phone width")
        shot(page, theme, "13-mobile")
        page.set_viewport_size({"width": 1440, "height": 900})

    step("phone layout", spa_and_mobile)
    ctx.close()


def control_jwt(scope="console", ttl=600):
    import jwt  # PyJWT (with cryptography for EdDSA)

    with open(os.environ["CONTROL_JWT_PRIVATE_KEY"], "rb") as f:
        key = f.read()
    claims = {
        "aud": os.environ.get("CONTROL_JWT_AUDIENCE", "ui-conformance"),
        "sub": "e2e-user",
        "exp": int(time.time()) + ttl,
        "scope": scope,
    }
    return jwt.encode(claims, key, algorithm="EdDSA")


def run_jwt(browser):
    """Sign-in flow against an instance with RUSTYBIN_CONTROL_AUTH=jwt."""
    jwt_base = os.environ["RUSTYBIN_JWT_URL"].rstrip("/")
    print(f"control auth jwt: {jwt_base}")
    ctx = browser.new_context(viewport={"width": 1280, "height": 860})
    page = ctx.new_page()
    page.set_default_timeout(TIMEOUT)

    def on_console(msg):
        # 401s of the control plane are expected here (logged as resource failures).
        if msg.type == "error" and "Failed to load resource" not in msg.text:
            console_errors.append(f"[jwt] {msg.text}")

    page.on("console", on_console)
    page.on("pageerror", lambda e: console_errors.append(f"[jwt] uncaught: {e}"))

    def sign_in_screen():
        status = urllib.request.urlopen(jwt_base + "/_rustybin/ready", timeout=10).status
        assert status == 200, status
        page.goto(jwt_base + "/ui/#/overview")
        page.wait_for_selector("#signin")
        expect(page.locator("#signin-title")).to_have_text("Sign in required")
        expect(page.locator("#signin-backlink")).to_have_attribute("href", "https://portal.example.com/pods/acme")
        expect(page.locator("#signin-backlink")).to_contain_text("Acme demo pod")
        shot(page, "jwt", "20-sign-in")
        page.fill("#signin-token", "not-a-valid-token")
        page.get_by_role("button", name="Sign in").click()
        page.wait_for_selector("#signin .notice.err")
        expect(page.locator("#signin")).to_contain_text("refused")

    step("jwt: sign-in screen, wrong token refused", sign_in_screen)

    def fragment_token():
        token = control_jwt("console")
        page.goto(jwt_base + "/ui/#/overview&token=" + token)
        page.wait_for_selector("text=Requests captured")
        expect(page.locator("#signin")).to_have_count(0)
        if "token" in page.url:
            raise AssertionError("the token stays in the URL: " + page.url)
        stored = page.evaluate("sessionStorage.getItem('rustybin.console.controlToken')")
        assert stored == token, "token not kept for this tab"
        expect(page.locator(".console-title")).to_have_text("Acme demo pod")
        expect(page.locator("#backlink")).to_have_attribute("href", "https://portal.example.com/pods/acme")
        # The live feed (fetch-based SSE with the bearer token) works.
        goto_view(page, "traffic")
        page.wait_for_selector("text=live feed")
        tag = "jwt-" + str(int(time.time()))
        urllib.request.urlopen(jwt_base + "/echo/" + tag, timeout=10).read()
        page.locator(".list-item", has_text="/echo/" + tag).first.wait_for()
        shot(page, "jwt", "21-signed-in")
        # A reload keeps the session (sessionStorage), without the fragment.
        page.reload()
        page.wait_for_selector("#view h1")
        expect(page.locator("#signin")).to_have_count(0)

    step("jwt: #token= fragment signs in, live feed with bearer", fragment_token)

    def expiry_and_sign_out():
        page.goto(jwt_base + "/ui/#/overview&token=" + control_jwt("console", ttl=3))
        page.wait_for_selector("text=Requests captured")
        page.wait_for_selector("#signin", timeout=15000)
        expect(page.locator("#signin-title")).to_have_text("Session expired")
        shot(page, "jwt", "22-expired")
        page.goto(jwt_base + "/ui/#/overview&token=" + control_jwt("console"))
        page.wait_for_selector("text=Requests captured")
        page.get_by_role("button", name="Console settings").click()
        page.locator("#sign-out").click()
        page.wait_for_selector("#signin")
        expect(page.locator("#signin")).to_contain_text("You signed out")
        stored = page.evaluate("sessionStorage.getItem('rustybin.console.controlToken')")
        assert not stored, "token kept after sign out"

    step("jwt: expiry and sign out", expiry_and_sign_out)
    ctx.close()


def main():
    print(f"console e2e against {BASE}")
    with sync_playwright() as p:
        exe = chromium_path()
        browser = p.chromium.launch(executable_path=exe, headless=not os.environ.get("HEADED")) if exe else p.chromium.launch()
        for theme in THEMES:
            run_theme(browser, theme)
        if os.environ.get("RUSTYBIN_JWT_URL"):
            run_jwt(browser)
        browser.close()
    for e in console_errors:
        failures.append("console: " + e)
    if failures:
        print(f"\n{len(failures)} failure(s):")
        for f in failures:
            print("  - " + f)
        sys.exit(1)
    print(f"\nall good, screenshots in {OUT}")


if __name__ == "__main__":
    main()
