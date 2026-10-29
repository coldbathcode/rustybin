#!/usr/bin/env python3
"""Browser end-to-end check of the Rustybin web console (/ui).

Clicks through every view in light and dark mode, exercises the main flows
(live traffic + compare, request bin, API explorer, AI streaming for every
provider plus tool calling, MCP list + call with progress and the OAuth
discovery, A2A streaming and approval continuation, chaos load generator,
token lab, the "Connect an AI agent" panel) and fails on browser console
errors or uncaught exceptions.

With RUSTYBIN_JWT_URL it also checks the sign-in flow of an instance with
RUSTYBIN_CONTROL_AUTH=jwt (sign-in screen, #token= fragment, fetch-based live
feed with the bearer token, the agent panel with the masked token, expiry,
sign out).

With RUSTYBIN_SS_URL it checks self-service mocks on a public instance
(create as a visitor, Extend, the email step, another visitor, manage link,
end the visit).

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
    RUSTYBIN_SS_URL           base URL of a public instance with self-service mocks
                              (RUSTYBIN_MOCKS_VISIT_MINUTES=5, _ANON_VISITS_PER_DAY=0:
                              Extend is always open, the email step comes first)
    LEADS_FILE                its RUSTYBIN_LEADS_FILE
    RUSTYBIN_HARNESS_URL      base URL of an instance with RUSTYBIN_HARNESS=on (roles both,
                              targets 127.0.0.1): the Harness view, its schema-generated
                              builder (a control for every non-shorthand schema property,
                              the parity check of docs/plans/harness.md) and a short run
    RUSTYBIN_PASSWORD_URL     base URL of an instance with the default control auth (the
                              password sign-in, rustybin / rustybin): the sign-in screen,
                              the default-password banner, Management, Sign-in (change the
                              password, the sessions, sign out everywhere), remember me
"""

import base64
import glob
import hashlib
import json
import os
import re
import sys
import time
import urllib.parse
import urllib.request

from playwright.sync_api import sync_playwright, expect

BASE = os.environ.get("RUSTYBIN_URL", "http://127.0.0.1:18700").rstrip("/")
OUT = os.environ.get("SCREENSHOT_DIR", "screenshots")
THEMES = [t.strip() for t in os.environ.get("THEMES", "light,dark").split(",") if t.strip()]
TIMEOUT = 20_000
SMALL_SPEC = """openapi: 3.0.3
info: {title: Orders, version: "1"}
paths:
  /orders/{id}:
    get:
      operationId: getOrder
      parameters:
        - {name: id, in: path, required: true, schema: {type: integer}}
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: object
                properties: {id: {type: integer}, total: {type: number}}
"""

failures = []
console_errors = []


def chromium_path():
    if os.environ.get("CHROMIUM_PATH"):
        return os.environ["CHROMIUM_PATH"]
    root = os.environ.get("PLAYWRIGHT_BROWSERS_PATH", "/opt/pw-browsers")
    hits = sorted(glob.glob(os.path.join(root, "chromium-*", "chrome-linux*", "chrome")))
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
    assert_icons_sized(page, view)


def assert_icons_sized(page, where):
    # An icon without a CSS size rule stretches to its container's width.
    big = page.evaluate("""() => [...document.querySelectorAll('svg')].flatMap((s) => {
        const r = s.getBoundingClientRect();
        if (r.width <= 48 && r.height <= 48) return [];
        const host = s.parentElement.closest('[id], [class]');
        return [`${Math.round(r.width)}x${Math.round(r.height)} in <${s.parentElement.tagName.toLowerCase()}> under ${host ? (host.id ? '#' + host.id : '.' + host.className) : '?'}`];
    })""")
    assert not big, f"oversized icons on {where}: {big}"


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
        expect(page.locator(".nav-link")).to_have_count(11)
        # No plan configured (RUSTYBIN_PLAN=none): no "Plan and usage" card.
        expect(page.locator("text=Plan and usage")).to_have_count(0)
        # AI assist with the mock provider: the Ask card offers suggestions.
        page.wait_for_selector("text=Ask Rustybin")
        page.wait_for_selector("#overview-ask")
        shot(page, theme, "01-overview")

    step("overview", overview)

    def assist_bar():
        # The bar in the top bar: suggestions for the view, a streamed answer
        # with apply cards in the drawer, an action applied.
        page.keyboard.press("Control+k")
        expect(page.locator("#assist-input")).to_be_focused()
        page.wait_for_selector("#assist-pop:not([hidden])")
        expect(page.locator("#assist-suggestions button")).to_have_count(3)
        expect(page.locator("#assist-hint")).to_contain_text("mock")
        page.fill("#assist-input", "How do I see what the gateway sent?")
        page.keyboard.press("Enter")
        page.wait_for_selector("#assist-drawer:not([hidden])")
        page.wait_for_selector(".assist-answer .md")
        page.wait_for_selector(".apply-card")
        expect(page.locator(".assist-exchange")).to_have_count(1)
        expect(page.locator(".assist-answer")).to_contain_text("Mock assistant")
        expect(page.locator(".assist-answer")).not_to_contain_text("```action")
        page.wait_for_selector(".assist-foot >> text=mock / rustybin-mock")
        page.wait_for_selector("text=What was sent")
        shot(page, theme, "02-assist-drawer")
        # The second card sends a request to the data plane and shows the status.
        cards = page.locator(".apply-card")
        expect(cards).to_have_count(2)
        cards.nth(1).get_by_role("button", name="Apply").click()
        page.wait_for_selector(".apply-result .badge.ok")
        # The first opens a view.
        cards.nth(0).get_by_role("button", name="Apply").click()
        page.wait_for_function("location.hash === '#/traffic'")
        # History survives a reload of the tab; Clear empties it.
        page.reload()
        page.wait_for_selector("#assist-drawer:not([hidden])")
        expect(page.locator(".assist-exchange")).to_have_count(1)
        page.locator("#assist-clear").click()
        expect(page.locator(".assist-exchange")).to_have_count(0)
        page.get_by_role("button", name="Close the assistant").click()
        page.goto(BASE + "/ui/#/overview")
        page.wait_for_selector("text=Requests captured")

    step("assist bar, drawer, apply cards", assist_bar)

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

    def mocks():
        goto_view(page, "mocks")
        row = page.locator(".mock-row", has_text="petstore")
        row.wait_for()
        row.locator(".mr-link").click()
        page.wait_for_selector(".op-row")
        # Operation editor: the rule-driven getPetById with its documented statuses.
        page.locator(".op-row", has_text="Find a pet by id").click()
        page.wait_for_selector(".st-tab")
        page.locator(".rule").first.wait_for()
        page.locator("text=matches the schema").first.wait_for()
        shot(page, theme, "15-mock-operation")
        # Try it: the prefilled sample answers 200 with the mock headers.
        page.get_by_role("tab", name="Try it").click()
        page.get_by_label("Operation").select_option("listPets")
        expect(page.get_by_label("Path and query (below the mock URL)")).to_have_value("/v1/pets")
        page.get_by_role("button", name="Send", exact=True).click()
        page.locator(".mock-strip", has_text="listPets").wait_for()
        page.locator(".card .badge.ok", has_text="200").first.wait_for()
        shot(page, theme, "16-mock-try")
        # Traffic shows the call live.
        page.get_by_role("tab", name="Traffic").click()
        page.locator(".mt-row", has_text="listPets").first.wait_for()
        # Switch the mock off (no confirmation) and back on.
        sw = page.get_by_role("switch", name="Mock on or off")
        sw.click()
        expect(sw).to_have_attribute("aria-checked", "false")
        page.wait_for_selector("text=This mock is switched off")
        assert http("GET", "/mock/petstore/v1/pets") == 503, "switched off mock still answers"
        sw.click()
        expect(sw).to_have_attribute("aria-checked", "true")
        assert http("GET", "/mock/petstore/v1/pets") == 200, "switched on mock does not answer"
        # Rule preview: the sample rule (petId 0 -> 404) would answer the last call.
        assert http("GET", "/mock/petstore/v1/pets/0") == 404, "sample rule does not answer 404"
        page.get_by_role("tab", name="Operations").click()
        page.locator(".op-row", has_text="Find a pet by id").click()
        page.locator(".rule-result", has_text="would answer").wait_for()
        page.locator(".cond-seen-v", has_text='seen "0"').first.wait_for()
        shot(page, theme, "19-mock-rule-preview")
        # Collection download from the overflow menu.
        page.locator("summary[aria-label='More actions']").click()
        page.get_by_role("menuitem", name="Download collection").click()
        page.locator("dialog .radio-card", has_text="Hurl").click()
        with page.expect_download() as dl:
            page.locator("dialog").get_by_role("button", name="Download").click()
        assert dl.value.suggested_filename == "petstore.hurl", dl.value.suggested_filename
        # Duplicate, switch the copy to stateful mode and round trip through Try it.
        copy = f"pets-{theme}"
        page.locator("summary[aria-label='More actions']").click()
        page.get_by_role("menuitem", name="Duplicate").click()
        page.get_by_label("Name of the copy").fill(copy)
        page.locator("dialog").get_by_role("button", name="Duplicate").click()
        page.wait_for_url(f"**#/mocks/{copy}")
        page.locator("h1", has_text=copy).wait_for()
        page.get_by_role("tab", name="Settings").click()
        page.get_by_label("Response mode").select_option("stateful")
        page.get_by_role("button", name="Save to draft").click()
        page.get_by_role("tab", name="Data").wait_for()
        page.locator(".mock-actions").get_by_role("button", name="Deploy").click()
        page.wait_for_selector("text=Deployed revision")
        page.get_by_role("tab", name="Try it").click()
        page.get_by_label("Operation").select_option("createPet")
        expect(page.get_by_label("Path and query (below the mock URL)")).to_have_value("/v1/pets")
        page.get_by_label("Request body").fill('{"name": "Bella", "tag": "dog"}')
        page.get_by_role("button", name="Send", exact=True).click()
        page.locator(".mock-strip", has_text="state create").wait_for()
        page.get_by_role("tab", name="Data").click()
        page.locator(".data-col", has_text="Bella").wait_for()
        shot(page, theme, "20-mock-data")
        # Wizard: paste a spec, review, create and deploy.
        name = f"orders-{theme}"
        goto_view(page, "mocks")
        page.get_by_role("button", name="New mock").click()
        page.get_by_label("OpenAPI document (JSON or YAML)").fill(SMALL_SPEC)
        page.get_by_role("button", name="Continue").click()
        page.get_by_label("Mock name").fill(name)
        page.wait_for_selector("text=Available.")
        page.get_by_role("button", name="Next").click()
        page.get_by_role("button", name="Create and deploy").click()
        page.wait_for_selector("text=Your mock is live")
        assert http("GET", f"/mock/{name}/orders/7") == 200, "new mock does not answer"
        shot(page, theme, "17-mock-created")
        page.get_by_role("link", name="All mocks").click()
        page.locator(".mock-row", has_text=name).wait_for()
        shot(page, theme, "18-mocks")
        # Bulk: select both new mocks, switch them off, then delete them (confirmed).
        page.get_by_label("Select " + name).check()
        page.get_by_label("Select " + copy).check()
        page.locator(".bulk-bar").get_by_role("button", name="Turn off").click()
        page.wait_for_selector("text=2 mocks switched off")
        expect(page.get_by_role("switch", name=copy + " on or off")).to_have_attribute("aria-checked", "false")
        page.once("dialog", lambda d: d.accept())
        page.locator(".bulk-bar").get_by_role("button", name="Delete").click()
        page.wait_for_selector("text=2 mocks deleted")
        expect(page.locator(".mock-row", has_text=copy)).to_have_count(0)
        assert http("GET", f"/mock/{name}/orders/7") == 404, "deleted mock still answers"

    step("mock apis (detail, try it, on/off, rule preview, collection, duplicate, stateful, wizard, bulk)", mocks)

    def mock_mcp():
        mcp_url = BASE + "/mock/petstore/_rustybin/mcp"
        page.evaluate("location.hash = '#/mocks/petstore/mcp'")
        tab = page.get_by_role("tab", name="MCP server")
        expect(tab).to_have_attribute("aria-selected", "true")
        panel = page.locator(".card", has_text="Connect an MCP client")
        panel.locator("text=Serving 6 tools").wait_for()
        expect(panel.locator("pre.code").first).to_have_text(f"claude mcp add --transport http petstore '{mcp_url}'")
        snippet = json.loads(panel.locator("pre.code").nth(1).inner_text())
        assert snippet == {"mcpServers": {"petstore": {"type": "http", "url": mcp_url}}}, snippet
        assert_icons_sized(page, "the mock MCP tab")
        shot(page, theme, "20-mock-mcp")
        # A tool call through the MCP server shows in Traffic with its badge.
        call = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "getPetById", "arguments": {"petId": 5},
            "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28",
                      "io.modelcontextprotocol/clientCapabilities": {}}}})
        status = http("POST", "/mock/petstore/_rustybin/mcp", {
            "Content-Type": "application/json", "Accept": "application/json, text/event-stream",
            "MCP-Protocol-Version": "2026-07-28", "Mcp-Method": "tools/call", "Mcp-Name": "getPetById"}, call)
        assert status == 200, f"tools/call answered {status}"
        page.get_by_role("tab", name="Traffic").click()
        page.locator(".mt-row", has_text="/v1/pets/5").locator(".badge", has_text="MCP").first.wait_for()
        # Untick an operation: saved to the draft, live after deploy.
        page.get_by_role("tab", name="MCP server").click()
        page.get_by_label("Tool for deletePet").uncheck()
        page.locator(".card", has_text="Tools").get_by_role("button", name="Save to draft").click()
        page.wait_for_selector("text=Saved to the draft")
        page.locator(".mock-head").get_by_role("button", name="Deploy").click()
        panel.locator("text=Serving 5 tools").wait_for()
        # Restore the sample for the next theme.
        http("PATCH", "/_rustybin/mocks/petstore", {"Content-Type": "application/json"},
             json.dumps({"mcp": {"enabled": True, "exclude": []}, "deploy": True}))

    step("mock MCP server tab (connect snippets, traffic badge, exclude)", mock_mcp)

    def agent_panel():
        # Open control plane and no admin token held: the command has no header.
        goto_view(page, "mocks")
        panel = page.locator("#agent-connect details")
        assert panel.get_attribute("open") is None, "agent panel open by default"
        panel.locator("summary", has_text="Connect an AI agent").click()
        url = BASE + "/_rustybin/mcp"
        expect(panel.locator("pre.code").first).to_have_text(f"claude mcp add --transport http rustybin '{url}'")
        snippet = json.loads(panel.locator("pre.code").nth(1).inner_text())
        assert snippet == {"mcpServers": {"rustybin": {"type": "http", "url": url}}}, snippet
        expect(panel).to_contain_text("This instance needs no credential")
        expect(panel.get_by_role("button", name="Show the token")).to_have_count(0)
        expect(panel.locator("code", has_text="rustybin mcp-stdio --server " + url)).to_have_count(1)
        assert_icons_sized(page, "the agent panel")
        shot(page, theme, "19-agent-panel")

    step("connect an AI agent panel (open instance)", agent_panel)

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
        # The gear opens Management, General: this browser's settings live there.
        page.get_by_role("button", name="Console settings", exact=True).click()
        page.wait_for_function("location.hash === '#/manage/general'")
        page.get_by_label("Session (X-Rustybin-Session)").fill("e2e-session")
        page.get_by_role("button", name="Save browser settings").click()
        page.wait_for_selector("#topbar-actions >> text=e2e-session")
        # The admin token stays in this tab, never in localStorage.
        page.get_by_label("Admin token").fill("e2e-admin-secret")
        page.get_by_role("button", name="Save browser settings").click()
        page.wait_for_function("(sessionStorage.getItem('rustybin.console.v1.secrets') || '').includes('e2e-admin-secret')")
        assert "e2e-admin-secret" not in (page.evaluate("localStorage.getItem('rustybin.console.v1')") or ""), "admin token on disk"
        # One saved by an older console moves out of localStorage on load.
        page.evaluate("""(() => {
            const s = JSON.parse(localStorage.getItem('rustybin.console.v1') || '{}');
            s.adminToken = 'e2e-old-secret';
            localStorage.setItem('rustybin.console.v1', JSON.stringify(s));
            sessionStorage.removeItem('rustybin.console.v1.secrets');
        })()""")
        page.reload()
        page.wait_for_selector("#view h1")
        assert "e2e-old-secret" not in (page.evaluate("localStorage.getItem('rustybin.console.v1')") or ""), "old admin token kept on disk"
        assert "e2e-old-secret" in (page.evaluate("sessionStorage.getItem('rustybin.console.v1.secrets')") or ""), "old admin token lost"
        page.get_by_role("button", name="Console settings", exact=True).click()
        page.get_by_label("Admin token").fill("")
        page.get_by_role("button", name="Save browser settings").click()
        page.keyboard.press("Alt+1")
        page.wait_for_function("location.hash === '#/overview'")

    step("management: browser settings + keyboard nav", settings_and_theme)

    def management():
        # Every registered instance setting has a control (its key as aria-label).
        page.goto(BASE + "/ui/#/manage/general")
        page.wait_for_selector("form[aria-label='Settings: console']")
        with urllib.request.urlopen(BASE + "/_rustybin/settings", timeout=10) as r:
            doc = json.load(r)
        assert doc["persisted"] is False and doc["locked"] is False, doc
        page.get_by_role("tab", name="Network").click()
        page.wait_for_selector("form[aria-label='Settings: egress']")
        page.wait_for_selector("form[aria-label='Settings: leads']")
        page.get_by_role("tab", name="General").click()
        page.wait_for_selector("form[aria-label='Settings: console']")
        for s in doc["settings"]:
            if s["group"] == "console":
                expect(page.locator(f"[aria-label='{s['key']}']")).to_have_count(1)
        # Sign-in: the form exists (disabled) on an open control plane too.
        page.get_by_role("tab", name="Sign-in").click()
        page.wait_for_selector("form[aria-label='Change the password']")
        expect(page.locator("text=The password sign-in is off")).to_have_count(1)
        for s in doc["settings"]:
            if s["group"] == "signin":
                expect(page.locator(f"[aria-label='{s['key']}']")).to_have_count(1)
        expect(page.locator("[aria-label='console.user']")).to_be_disabled()
        # AI: a control per setting, the curated models, the test against the mock.
        page.get_by_role("tab", name="AI").click()
        page.wait_for_selector("form[aria-label='Settings: assist']")
        for s in doc["settings"]:
            if s["group"] == "assist":
                expect(page.locator(f"[aria-label='{s['key']}']")).to_have_count(1)
        page.wait_for_selector("#assist-models")
        expect(page.locator("text=AI assist is on")).to_have_count(1)
        page.locator("#assist-test").click()
        page.wait_for_selector("text=The provider answered in")
        shot(page, theme, "15-management-ai")
        page.get_by_role("tab", name="Network").click()
        for s in doc["settings"]:
            if s["group"] in ("egress", "leads"):
                expect(page.locator(f"[aria-label='{s['key']}']")).to_have_count(1)
        # The policy card: the global list, each feature, no decisions yet.
        page.wait_for_selector("text=Allowed by feature")
        page.wait_for_selector("text=A2A push notifications")
        # A secret: set it, see "ends in", never the value; then clear it.
        page.get_by_role("button", name="leads.webhook_token replace").click()
        page.locator("[aria-label='leads.webhook_token']").fill("e2e-hook-secret-k3Qx")
        page.get_by_role("button", name="Save leads webhook settings").click()
        page.wait_for_selector("text=ends in k3Qx")
        assert "e2e-hook-secret" not in page.locator("form[aria-label='Settings: leads']").inner_text()
        with urllib.request.urlopen(BASE + "/_rustybin/settings", timeout=10) as r:
            body = r.read().decode()
        assert "e2e-hook-secret" not in body and '"hint":"k3Qx"' in body.replace(" ", ""), body
        shot(page, theme, "15-management-network")
        page.get_by_role("button", name="leads.webhook_token clear").click()
        page.get_by_role("button", name="Save leads webhook settings").click()
        page.wait_for_selector("form[aria-label='Settings: leads'] >> text=Not set")
        # The outbound allowlist: saved, validated, shown in the policy card.
        page.locator("[aria-label='egress.allow']").fill("not a host!")
        page.get_by_role("button", name="Save outbound settings").click()
        page.wait_for_selector("form[aria-label='Settings: egress'] .notice.err")
        page.locator("[aria-label='egress.allow']").fill("*.example.com\n10.0.0.0/8")
        page.get_by_role("button", name="Save outbound settings").click()
        page.wait_for_selector("text=Saved egress.allow")
        page.wait_for_selector("dd:has-text('*.example.com, 10.0.0.0/8')")
        # The console title: saved on the instance, shown after a reload.
        page.get_by_role("tab", name="General").click()
        page.locator("[aria-label='console.title']").fill("E2E pod")
        page.get_by_role("button", name="Save console settings").click()
        page.wait_for_selector("text=Saved console.title")
        page.wait_for_selector("text=Saved in this instance (overrides RUSTYBIN_CONSOLE_TITLE)")
        page.reload()
        page.wait_for_selector("#topbar-meta .console-title")
        expect(page.locator("#topbar-meta .console-title")).to_have_text("E2E pod")
        shot(page, theme, "15-management")
        page.locator("[aria-label='console.title']").fill("")
        page.get_by_role("button", name="Save console settings").click()
        page.wait_for_selector("text=Saved console.title")
        page.get_by_role("tab", name="Network").click()
        page.locator("[aria-label='egress.allow']").fill("")
        page.get_by_role("button", name="Save outbound settings").click()
        page.wait_for_selector("text=Saved egress.allow")
        assert_icons_sized(page, "the management view")

    step("management: instance settings, a secret, the egress policy", management)

    def via_gateway():
        # A second origin for the same server stands in for a gateway with CORS.
        alt = BASE.replace("127.0.0.1", "localhost") if "127.0.0.1" in BASE else BASE.replace("localhost", "127.0.0.1")
        page.get_by_role("button", name="Console settings", exact=True).click()
        page.get_by_label("Gateway base URL").fill(alt)
        page.get_by_role("button", name="Save browser settings").click()
        goto_view(page, "explorer")
        page.get_by_label("Search endpoints").fill("/anything")
        page.locator(".list-item", has_text="/anything").first.click()
        page.locator(".seg button", has_text="Via gateway").first.click()
        page.get_by_role("button", name="Send", exact=True).click()
        page.locator(".card .badge.ok").first.wait_for()
        # Unreachable gateway: a helpful message instead of a spinner.
        page.get_by_role("button", name="Console settings", exact=True).click()
        page.get_by_label("Gateway base URL").fill("http://127.0.0.1:9")
        page.get_by_role("button", name="Save browser settings").click()
        goto_view(page, "explorer")
        page.get_by_label("Search endpoints").fill("/anything")
        page.locator(".list-item", has_text="/anything").first.click()
        page.locator(".seg button", has_text="Via gateway").first.click()
        page.get_by_role("button", name="Send", exact=True).click()
        page.wait_for_selector("text=Copy as curl")
        page.wait_for_selector("text=CORS")
        shot(page, theme, "14-gateway-error")
        page.get_by_role("button", name="Console settings", exact=True).click()
        page.get_by_label("Gateway base URL").fill("")
        page.get_by_role("button", name="Save browser settings").click()

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
    # Clipboard access for the agent panel's copy button.
    ctx = browser.new_context(viewport={"width": 1280, "height": 860}, permissions=["clipboard-read", "clipboard-write"])
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

    def limited_token_signs_in():
        # An inspector token is valid but may not read the overview (403):
        # the console opens and says so instead of keeping the sign-in screen.
        page.fill("#signin-token", control_jwt("inspector"))
        page.get_by_role("button", name="Sign in").click()
        expect(page.locator("#signin")).to_have_count(0)
        expect(page.locator(".toast")).to_contain_text("limited token")
        page.evaluate("sessionStorage.removeItem('rustybin.console.controlToken')")
        page.reload()
        page.wait_for_selector("#signin")

    step("jwt: a limited-scope token signs in", limited_token_signs_in)

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
        # An empty token= is dropped and the rest of the route still applies.
        page.evaluate("location.hash = '#/overview'")
        page.wait_for_selector("text=Requests captured")
        page.evaluate("location.hash = '#/traffic&token='")
        page.wait_for_function("location.hash === '#/traffic'")
        page.wait_for_selector("text=live feed")
        assert page.evaluate("sessionStorage.getItem('rustybin.console.controlToken')") == token, "token lost"
        expect(page.locator("#signin")).to_have_count(0)

    step("jwt: #token= fragment signs in, live feed with bearer", fragment_token)

    def mocks_read_only():
        goto_view(page, "mocks")
        page.locator(".mock-row", has_text="petstore").wait_for()
        page.wait_for_selector("text=read-only")
        expect(page.get_by_role("button", name="New mock")).to_have_count(0)
        expect(page.get_by_role("switch").first).to_be_disabled()
        expect(page.locator(".bulk-bar")).to_have_count(0)
        page.locator(".mock-row .mr-link", has_text="petstore").click()
        page.locator("summary[aria-label='More actions']").click()
        page.get_by_role("menuitem", name="Download collection").wait_for()
        expect(page.get_by_role("menuitem", name="Duplicate")).to_have_count(0)
        expect(page.get_by_role("menuitem", name="Delete")).to_have_count(0)

    step("jwt: mock apis read-only with a console token", mocks_read_only)

    def agent_panel_with_token():
        token = page.evaluate("sessionStorage.getItem('rustybin.console.controlToken')")
        assert token, "not signed in"
        page.get_by_role("button", name="Console settings", exact=True).click()
        panel = page.locator("#agent-panel")
        panel.locator("summary", has_text="Connect an AI agent").click()
        url = jwt_base + "/_rustybin/mcp"
        command = f"claude mcp add --transport http rustybin '{url}' --header 'Authorization: Bearer {token}'"
        masked = command.replace(token, token[:4] + "..." + token[-2:])
        cmd = panel.locator("pre.code").first
        expect(cmd).to_have_text(masked)
        expect(panel).to_contain_text("control token")
        # The secret is nowhere in the page until the user asks to see it.
        assert token not in page.content(), "control token rendered before Show the token"
        shot(page, "jwt", "23-agent-panel")
        panel.get_by_role("button", name="Show the token").click()
        expect(cmd).to_have_text(command)
        snippet = json.loads(panel.locator("pre.code").nth(1).inner_text())
        assert snippet["mcpServers"]["rustybin"] == {"type": "http", "url": url, "headers": {"Authorization": "Bearer " + token}}, snippet
        panel.get_by_role("button", name="Hide the token").click()
        expect(cmd).to_have_text(masked)
        # Copy gives the real value while the panel shows the masked one.
        panel.locator("button", has_text="Copy command").click()
        page.wait_for_function("navigator.clipboard.readText().then((t) => t.length > 0)")
        copied = page.evaluate("navigator.clipboard.readText()")
        assert copied == command, copied
        page.keyboard.press("Escape")

    step("jwt: agent panel masks the control token", agent_panel_with_token)

    def explorer_masks_the_control_token():
        token = page.evaluate("sessionStorage.getItem('rustybin.console.controlToken')")
        assert token, "not signed in"
        goto_view(page, "explorer")
        page.wait_for_selector("text=Try it")
        page.get_by_label("Search endpoints").fill("/_rustybin/status")
        page.locator(".list-item", has_text="/_rustybin/status").first.click()
        page.get_by_role("button", name="Send", exact=True).click()
        page.wait_for_selector("#view .card .badge.ok")
        masked = "Bearer " + token[:4] + "..." + token[-2:]
        for tab in ["Request", "curl"]:
            page.locator("#view").get_by_role("tab", name=tab).click()
            expect(page.locator("#view")).to_contain_text(masked)
            assert token not in page.content(), f"control token shown in the {tab} tab"
        # Copy as curl still gives a command that works.
        response_card = page.locator("#view .card", has=page.locator(".badge.ok")).last
        response_card.get_by_role("button", name="Copy as curl").click()
        page.wait_for_function("navigator.clipboard.readText().then((t) => t.startsWith('curl'))")
        copied = page.evaluate("navigator.clipboard.readText()")
        assert "Bearer " + token in copied, copied

    step("jwt: api explorer masks the control token", explorer_masks_the_control_token)

    def agent_sign_in_read_only():
        # Fresh context: no console token yet, so the sign-in screen comes first.
        ctx2 = browser.new_context(viewport={"width": 1280, "height": 860})
        p2 = ctx2.new_page()
        p2.set_default_timeout(TIMEOUT)
        p2.on("pageerror", lambda e: console_errors.append(f"[jwt consent] uncaught: {e}"))
        redirect = "http://127.0.0.1:47911/callback"
        verifier = "e2e-" + "v" * 50
        client_id, req_id = oauth_start(jwt_base, redirect, "E2E <b>agent</b>", verifier)
        landed = []

        def callback(route):
            landed.append(route.request.url)
            route.fulfill(status=200, content_type="text/plain", body="callback reached")

        ctx2.route("http://127.0.0.1:47911/**", callback)
        p2.goto(jwt_base + "/ui/#/authorize/" + req_id)
        p2.wait_for_selector("#signin")
        p2.fill("#signin-token", "ui-admin-token")
        p2.get_by_role("button", name="Sign in").click()
        client = p2.locator("#authorize-client")
        # The registered name is shown as text, never parsed as HTML.
        expect(client).to_have_text("E2E <b>agent</b>")
        expect(p2.locator("#view")).to_contain_text("127.0.0.1:47911")
        expect(p2.locator("#view .notice.warn")).to_have_count(0)
        # The admin token may grant everything; full access is the default.
        expect(p2.locator("input[name=authorize-scope][value=admin]")).to_be_checked()
        expect(p2.locator("nav a[href='#/authorize']")).to_have_count(0)
        shot(p2, "jwt", "24-agent-consent")
        p2.locator("label[data-scope=console]").click()
        p2.locator("#authorize-approve").click()
        p2.wait_for_url(re.compile(r"^http://127\.0\.0\.1:47911/callback\?"))
        q = urllib.parse.parse_qs(urllib.parse.urlparse(landed[-1]).query)
        assert "code" in q, landed
        assert q.get("iss") == [jwt_base + "/_rustybin/oauth"], q
        tokens = oauth_token(jwt_base, client_id, q["code"][0], redirect, verifier)
        assert tokens["scope"] == "console", tokens
        names = mcp_tool_names(jwt_base, tokens["access_token"])
        assert "list_mocks" in names and "create_mock" not in names, names
        ctx2.close()

    step("jwt: agent sign-in (sign in first, read only, approve)", agent_sign_in_read_only)

    def agent_sign_in_web_redirect_denied():
        # The main page is signed in with a console-scope token: admin is not grantable.
        redirect = "https://agent.example.com/oauth/cb"
        _, req_id = oauth_start(jwt_base, redirect, "Web agent", "e2e-" + "w" * 50)
        landed = []

        def callback(route):
            landed.append(route.request.url)
            route.fulfill(status=200, content_type="text/plain", body="callback reached")

        page.context.route("https://agent.example.com/**", callback)
        page.goto(jwt_base + "/ui/#/authorize/" + req_id)
        expect(page.locator("#authorize-client")).to_have_text("Web agent")
        warn = page.locator("#view .notice.warn")
        expect(warn).to_contain_text("agent.example.com")
        expect(warn).to_contain_text("website")
        expect(page.locator("input[name=authorize-scope][value=admin]")).to_be_disabled()
        expect(page.locator("input[name=authorize-scope][value=console]")).to_be_checked()
        shot(page, "jwt", "25-agent-consent-web")
        page.locator("#authorize-deny").click()
        page.wait_for_url(re.compile(r"^https://agent\.example\.com/oauth/cb\?"))
        q = urllib.parse.parse_qs(urllib.parse.urlparse(landed[-1]).query)
        assert q.get("error") == ["access_denied"], q
        page.context.unroute("https://agent.example.com/**")
        # Answered once: the page now says to start again.
        page.goto(jwt_base + "/ui/#/authorize/" + req_id)
        expect(page.locator("#view .notice")).to_contain_text("Start the sign-in again")

    step("jwt: agent sign-in to a website warns, console token caps, deny", agent_sign_in_web_redirect_denied)

    def expiry_and_sign_out():
        page.goto(jwt_base + "/ui/#/overview&token=" + control_jwt("console", ttl=3))
        page.wait_for_selector("text=Requests captured")
        page.wait_for_selector("#signin", timeout=15000)
        expect(page.locator("#signin-title")).to_have_text("Session expired")
        shot(page, "jwt", "22-expired")
        page.goto(jwt_base + "/ui/#/overview&token=" + control_jwt("console"))
        page.wait_for_selector("text=Requests captured")
        page.get_by_role("button", name="Console settings", exact=True).click()
        page.locator("#sign-out").click()
        page.wait_for_selector("#signin")
        expect(page.locator("#signin")).to_contain_text("You signed out")
        stored = page.evaluate("sessionStorage.getItem('rustybin.console.controlToken')")
        assert not stored, "token kept after sign out"

    step("jwt: expiry and sign out", expiry_and_sign_out)
    ctx.close()


def run_self_service(browser):
    """Self-service mocks: two visitors on a public instance."""
    ss_base = os.environ["RUSTYBIN_SS_URL"].rstrip("/")
    print(f"self-service: {ss_base}")
    state = {}

    def new_page():
        ctx = browser.new_context(viewport={"width": 1280, "height": 860})
        page = ctx.new_page()
        page.set_default_timeout(TIMEOUT)

        def on_console(msg):
            # 403 (email step) and 404 (someone else's mock) are expected.
            if msg.type == "error" and "Failed to load resource" not in msg.text:
                console_errors.append(f"[self-service] {msg.text}")

        page.on("console", on_console)
        page.on("pageerror", lambda e: console_errors.append(f"[self-service] uncaught: {e}"))
        return ctx, page

    ctx, page = new_page()

    def create_as_visitor():
        page.goto(ss_base + "/ui/#/mocks")
        page.wait_for_selector("text=Try it with your own API")
        page.get_by_role("button", name="New mock").click()
        page.get_by_label("OpenAPI document (JSON or YAML)").fill(SMALL_SPEC)
        page.get_by_role("button", name="Continue").click()
        page.wait_for_selector("text=random suffix")
        page.get_by_role("button", name="Next").click()
        expect(page.get_by_text("Remember changes (stateful CRUD)")).to_have_count(0)
        page.get_by_role("button", name="Create and deploy").click()
        # No anonymous visits on this instance: the email step comes first.
        page.get_by_label("Email address").fill("visitor@example.com")
        page.locator("#visit-consent").check()
        shot(page, "self-service", "31-email-step")
        page.get_by_role("button", name="Continue").click()
        page.wait_for_selector("text=Your mock is live")
        page.wait_for_selector("text=Only this browser can change it")
        name = page.locator("h1.mono").inner_text().strip()
        assert re.fullmatch(r"orders-[a-z2-7]{6}", name), name
        state["name"] = name
        tokens = page.evaluate("JSON.parse(localStorage.getItem('rustybin.console.mockTokens') || '[]')")
        assert len(tokens) == 1 and tokens[0].startswith("rbm_"), tokens
        state["token"] = tokens[0]
        status = urllib.request.urlopen(ss_base + f"/mock/{name}/orders/7", timeout=10).status
        assert status == 200, status
        shot(page, "self-service", "30-visitor-mock")

    step("self-service: create a mock as a visitor", create_as_visitor)

    def extend_and_lead():
        page.get_by_role("link", name="All mocks").click()
        page.wait_for_selector(".visit-bar")
        page.locator(".mock-group", has_text="Your mocks").wait_for()
        page.wait_for_selector("text=hours used today")
        page.locator("#visit-extend").click()
        page.wait_for_selector("text=Your mocks now run until")
        leads = os.environ.get("LEADS_FILE")
        if leads:
            deadline = time.time() + 5
            while time.time() < deadline and not (os.path.exists(leads) and "visitor@example.com" in open(leads).read()):
                time.sleep(0.2)
            text = open(leads).read()
            assert "visitor@example.com" in text and '"marketing_consent":true' in text, text
        shot(page, "self-service", "32-visit-bar")

    step("self-service: email step, lead written, Extend", extend_and_lead)

    def agent_panel_for_visitor():
        # The visit's manage token is the credential, masked like the others.
        token = state["token"]
        panel = page.locator("#agent-connect details")
        panel.locator("summary", has_text="Connect an AI agent").click()
        url = ss_base + "/_rustybin/mcp"
        masked = token[:4] + "..." + token[-2:]
        expect(panel.locator("pre.code").first).to_have_text(
            f"claude mcp add --transport http rustybin '{url}' --header 'X-Rustybin-Mock-Token: {masked}'")
        expect(panel).to_contain_text("visit manage token")
        assert token not in page.content(), "manage token rendered before Show the token"

    step("self-service: agent panel with the manage token", agent_panel_for_visitor)

    ctx2, page2 = new_page()

    def another_visitor():
        name = state["name"]
        page2.goto(ss_base + "/ui/#/mocks")
        page2.wait_for_selector("text=Try it with your own API")
        expect(page2.locator(".mock-row", has_text=name)).to_have_count(0)
        # No credential held on a public instance: the agent panel says what that allows.
        panel = page2.locator("#agent-connect details")
        panel.locator("summary", has_text="Connect an AI agent").click()
        expect(panel).to_contain_text("No credential held")
        expect(panel.locator("pre.code").first).not_to_contain_text("--header")
        page2.goto(ss_base + f"/ui/#/mocks/{name}")
        page2.wait_for_selector("text=This mock does not exist")

    step("self-service: another visitor does not see it", another_visitor)

    def manage_link_and_end():
        name = state["name"]
        page2.goto(ss_base + "/ui/#/mocks&mock_token=" + state["token"])
        page2.locator(".mock-row", has_text=name).wait_for()
        if "mock_token" in page2.url:
            raise AssertionError("the manage token stays in the URL: " + page2.url)
        page2.wait_for_selector(".visit-bar")
        page2.once("dialog", lambda d: d.accept())
        page2.get_by_role("button", name="End visit").click()
        page2.wait_for_selector("text=Visit ended")
        expect(page2.locator(".mock-row", has_text=name)).to_have_count(0)
        status = http_at(ss_base, f"/mock/{name}/orders/7")
        assert status == 404, status

    step("self-service: manage link, end the visit", manage_link_and_end)
    ctx2.close()
    ctx.close()


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):  # noqa: ARG002 - keep the 302
        return None


def oauth_start(base, redirect, name, verifier):
    """Register a client and start an authorization; returns (client_id, request id)."""
    reg = urllib.request.Request(base + "/_rustybin/oauth/register", method="POST",
                                 data=json.dumps({"redirect_uris": [redirect], "client_name": name}).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(reg, timeout=10) as r:
        client_id = json.loads(r.read())["client_id"]
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    query = urllib.parse.urlencode({
        "response_type": "code", "client_id": client_id, "redirect_uri": redirect,
        "code_challenge": challenge, "code_challenge_method": "S256", "state": "e2e",
    })
    opener = urllib.request.build_opener(_NoRedirect)
    try:
        opener.open(base + "/_rustybin/oauth/authorize?" + query, timeout=10)
        raise AssertionError("authorize did not redirect")
    except urllib.error.HTTPError as e:
        assert e.code == 302, e.code
        location = e.headers["Location"]
    prefix = "/ui/#/authorize/"
    assert location.startswith(prefix), location
    return client_id, location[len(prefix):]


def oauth_token(base, client_id, code, redirect, verifier):
    body = urllib.parse.urlencode({
        "grant_type": "authorization_code", "client_id": client_id, "code": code,
        "redirect_uri": redirect, "code_verifier": verifier,
    }).encode()
    req = urllib.request.Request(base + "/_rustybin/oauth/token", method="POST", data=body,
                                 headers={"Content-Type": "application/x-www-form-urlencoded"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())


def mcp_tool_names(base, token):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"_meta": {
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {}}}}).encode()
    req = urllib.request.Request(base + "/_rustybin/mcp", method="POST", data=body, headers={
        "Content-Type": "application/json", "Accept": "application/json, text/event-stream",
        "MCP-Protocol-Version": "2026-07-28", "Mcp-Method": "tools/list", "Authorization": "Bearer " + token})
    with urllib.request.urlopen(req, timeout=10) as r:
        return [t["name"] for t in json.loads(r.read())["result"]["tools"]]


def http_at(base, path):
    try:
        with urllib.request.urlopen(base + path, timeout=10) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


def run_harness(browser):
    """The Harness view against an instance with RUSTYBIN_HARNESS=on."""
    base = os.environ["RUSTYBIN_HARNESS_URL"].rstrip("/")
    print(f"harness: {base}")
    ctx = browser.new_context(viewport={"width": 1440, "height": 900})
    page = ctx.new_page()
    page.set_default_timeout(TIMEOUT)
    page.on("console", lambda msg: console_errors.append(f"[harness] {msg.text}") if msg.type == "error" and "Failed to load resource" not in msg.text else None)
    page.on("pageerror", lambda e: console_errors.append(f"[harness] uncaught: {e}"))
    page.on("dialog", lambda d: d.accept())

    def nav_shows_the_view():
        page.goto(base + "/ui/#/overview")
        page.wait_for_selector("nav a[href='#/harness']")
        page.click("nav a[href='#/harness']")
        page.wait_for_selector("#view .view-head h1")
        expect(page.locator("#view .view-head h1")).to_have_text("Harness")
        page.wait_for_selector("#scenario-form")
        expect(page.locator("#harness-kill")).to_be_visible()
        shot(page, "harness", "40-harness")

    step("harness: the view appears when harness mode is on", nav_shows_the_view)

    def builder_covers_the_schema():
        schema = json.load(urllib.request.urlopen(base + "/_rustybin/harness/schema", timeout=10))
        page.select_option("#harness-builtin", "smoke")
        page.wait_for_function("document.querySelectorAll('#scenario-form [aria-label]').length > 10")

        def resolve(node):
            while node and "$ref" in node:
                cur = schema
                for seg in node["$ref"].lstrip("#/").split("/"):
                    cur = cur[seg]
                node = dict(cur, **{k: v for k, v in node.items() if k != "$ref"})
            return node or {}

        def labels():
            return set(page.evaluate("[...document.querySelectorAll('#scenario-form [aria-label]')].map((e) => e.getAttribute('aria-label'))"))

        def discriminated(node):
            return "oneOf" in node and all("type" in (resolve(b).get("properties") or {}) for b in node["oneOf"])

        def has_control(here):
            ls = labels()
            return here in ls or any(l.startswith(here + " ") and not l.endswith(" add") for l in ls)

        def ensure(here):
            # Optional parts (a `then` stage, an empty list or map) are added
            # with their button, so their fields can be checked too.
            if not has_control(here):
                btn = page.locator(f"#scenario-form [aria-label='{here} add']")
                if btn.count():
                    # Open the collapsed sections around it, as a user would.
                    btn.first.evaluate("b => { for (let d = b.closest('details'); d; d = d.parentElement.closest('details')) d.open = true; }")
                    btn.first.click()

        missing = []

        def check_prop(sub, here):
            sub_r = resolve(sub)
            if sub.get("x-shorthand") or sub_r.get("x-shorthand"):
                return
            if here.count(".then") > 1:
                return
            items = resolve(sub_r.get("items", {})) if sub_r.get("type") == "array" else None
            if items is not None and (items.get("properties") or "oneOf" in items):
                ensure(here)
                item_schema = items if items.get("properties") else next((resolve(b) for b in items["oneOf"] if resolve(b).get("properties")), items)
                walk(item_schema, f"{here}[0]")
                return
            if discriminated(sub_r):
                ensure(here)
                walk(sub_r, here)
                return
            if sub_r.get("properties"):
                walk(sub_r, here)
                return
            if sub_r.get("additionalProperties") and isinstance(sub_r["additionalProperties"], dict):
                ensure(here)
                if not has_control(here):
                    missing.append(here)
                return
            if not has_control(here):
                missing.append(here)

        def walk(node, path):
            node = resolve(node)
            if discriminated(node):
                sel = f"#scenario-form [aria-label='{path}.type']"
                if not page.locator(sel).count():
                    missing.append(path + ".type")
                    return
                for b in node["oneOf"]:
                    b = resolve(b)
                    kind = b["properties"]["type"].get("const")
                    page.select_option(sel, kind)
                    for name, sub in b["properties"].items():
                        if name != "type":
                            check_prop(sub, f"{path}.{name}")
                return
            for name, sub in (node.get("properties") or {}).items():
                check_prop(sub, f"{path}.{name}" if path else name)

        walk(schema, "")
        assert not missing, f"schema properties without a builder control: {sorted(set(missing))}"

    step("harness: the builder renders a control for every schema property", builder_covers_the_schema)

    def validate_and_run():
        page.select_option("#harness-builtin", "smoke")
        page.wait_for_function("document.querySelector('#scenario-form [aria-label=\"targets[0].url\"]')")
        page.fill("#scenario-form [aria-label='targets[0].url']", base)
        page.fill("#scenario-form [aria-label='layers[0].pattern.for']", "2s")
        page.click("#harness-validate")
        page.wait_for_selector("#view .notice.ok")
        page.click("#harness-run")
        page.wait_for_selector("text=Answered", timeout=TIMEOUT)
        page.wait_for_selector("text=exit code", timeout=TIMEOUT)
        expect(page.locator("#view")).to_contain_text("PASS")
        shot(page, "harness", "41-harness-report")

    step("harness: validate, run and report", validate_and_run)
    ctx.close()


def run_password(browser):
    """The password sign-in against an instance with the default control auth."""
    base = os.environ["RUSTYBIN_PASSWORD_URL"].rstrip("/")
    print(f"control auth password: {base}")
    ctx = browser.new_context(viewport={"width": 1280, "height": 860})
    page = ctx.new_page()
    page.set_default_timeout(TIMEOUT)

    def on_console(msg):
        if msg.type == "error" and "Failed to load resource" not in msg.text:
            console_errors.append(f"[password] {msg.text}")

    page.on("console", on_console)
    page.on("pageerror", lambda e: console_errors.append(f"[password] uncaught: {e}"))
    new_password = "e2e-new-secret-k3Qx"

    def sign_in_screen():
        code = urllib.request.urlopen(base + "/_rustybin/ready", timeout=10).status
        assert code == 200, code
        try:
            urllib.request.urlopen(base + "/_rustybin/status", timeout=10)
            raise AssertionError("the control plane is open")
        except urllib.error.HTTPError as e:
            assert e.code == 401, e.code
        page.goto(base + "/ui/#/overview")
        page.wait_for_selector("#signin")
        expect(page.locator("#signin-title")).to_have_text("Sign in")
        expect(page.locator("#signin-user")).to_be_focused()
        shot(page, "password", "30-sign-in")
        page.fill("#signin-user", "rustybin")
        page.fill("#signin-password", "not-the-password")
        page.get_by_role("button", name="Sign in", exact=True).click()
        page.wait_for_selector("#signin .notice.err")
        expect(page.locator("#signin")).to_contain_text("Wrong user name or password")
        # The token form is still there, folded.
        expect(page.locator("#signin-token-details")).to_have_count(1)

    step("password: sign-in screen, wrong password refused", sign_in_screen)

    def default_sign_in_and_banner():
        page.fill("#signin-user", "rustybin")
        page.fill("#signin-password", "rustybin")
        page.get_by_role("button", name="Sign in", exact=True).click()
        page.wait_for_selector("text=Requests captured")
        expect(page.locator("#signin")).to_have_count(0)
        # The banner until the password changes; the chip in the top bar.
        page.wait_for_selector("#banner .notice.warn")
        # No AI provider here: the bar is an invitation to Management, AI.
        page.wait_for_selector("#assist-wrap.off")
        expect(page.locator("#assist-input")).to_have_attribute("placeholder", "Add an AI key to describe what you want")
        expect(page.locator("text=Ask Rustybin")).to_have_count(1)
        expect(page.locator("#overview-ask")).to_have_count(0)
        expect(page.locator("#banner")).to_contain_text("default console password")
        expect(page.locator("#signed-in")).to_contain_text("rustybin")
        expect(page.locator("#signed-in")).to_contain_text("expires in")
        token = page.evaluate("sessionStorage.getItem('rustybin.console.controlToken')")
        assert token and token.count(".") == 2, "no sign-in token"
        assert not page.evaluate("localStorage.getItem('rustybin.console.rememberedToken')"), "remembered without asking"
        shot(page, "password", "31-signed-in-banner")

    step("password: the defaults sign in, the banner shows", default_sign_in_and_banner)

    def change_password():
        page.locator("#banner-change-password").click()
        page.wait_for_function("location.hash === '#/manage/signin'")
        page.wait_for_selector("form[aria-label='Change the password']")
        expect(page.locator("#manage-panel")).to_contain_text("Default password")
        page.wait_for_selector("#sessions-table")
        expect(page.locator("#sessions-table tbody tr")).to_have_count(1)
        expect(page.locator("#sessions-table")).to_contain_text("this session")
        shot(page, "password", "32-management-sign-in")
        # The current password is required.
        page.fill("#signin-current", "wrong")
        page.fill("#signin-next", new_password)
        page.fill("#signin-confirm", new_password)
        page.get_by_role("button", name="Change the password").click()
        page.wait_for_selector("form[aria-label='Change the password'] .notice.err")
        expect(page.locator("form[aria-label='Change the password']")).to_contain_text("current password is wrong")
        page.fill("#signin-current", "rustybin")
        page.fill("#signin-next", new_password)
        page.fill("#signin-confirm", new_password)
        page.get_by_role("button", name="Change the password").click()
        page.wait_for_selector("text=Password changed")
        # The console keeps working with the fresh token; the banner is gone.
        page.wait_for_function("!document.querySelector('#banner .notice')")
        page.wait_for_selector("#manage-panel >> text=changed in the console")
        with urllib.request.urlopen(base + "/_rustybin/ready", timeout=10) as r:
            assert r.status == 200
        page.get_by_role("tab", name="General").click()
        page.wait_for_selector("form[aria-label='Settings: console']")
        page.get_by_role("tab", name="Sign-in").click()
        page.wait_for_selector("#sessions-table")

    step("password: change the password from Management, Sign-in", change_password)

    def sign_out_everywhere():
        page.once("dialog", lambda d: d.accept())
        page.locator("#sign-out-everywhere").click()
        page.wait_for_selector("#signin")
        expect(page.locator("#signin")).to_contain_text("You signed out")
        # The old password is gone, the new one signs in with Remember me.
        page.fill("#signin-user", "rustybin")
        page.fill("#signin-password", "rustybin")
        page.get_by_role("button", name="Sign in", exact=True).click()
        page.wait_for_selector("#signin .notice.err")
        page.fill("#signin-password", new_password)
        page.check("#signin-remember")
        page.get_by_role("button", name="Sign in", exact=True).click()
        # Back on the view that was open: Management, Sign-in.
        page.wait_for_selector("#sessions-table")
        expect(page.locator("#signin")).to_have_count(0)
        remembered = page.evaluate("localStorage.getItem('rustybin.console.rememberedToken')")
        assert remembered and remembered.count(".") == 2, "not remembered"
        expect(page.locator("#banner .notice")).to_have_count(0)
        # A new tab in the same browser is signed in from the remembered token.
        p2 = ctx.new_page()
        p2.goto(base + "/ui/#/overview")
        p2.wait_for_selector("text=Requests captured")
        expect(p2.locator("#signin")).to_have_count(0)
        p2.close()

    step("password: sign out everywhere, sign in again, remember me", sign_out_everywhere)

    def sign_out():
        page.get_by_role("button", name="Console settings", exact=True).click()
        page.locator("#sign-out").click()
        page.wait_for_selector("#signin")
        expect(page.locator("#signin")).to_contain_text("You signed out")
        assert not page.evaluate("sessionStorage.getItem('rustybin.console.controlToken')"), "token kept"
        assert not page.evaluate("localStorage.getItem('rustybin.console.rememberedToken')"), "remembered token kept"
        # The server revoked it: the token form refuses the old token.
        shot(page, "password", "33-signed-out")

    step("password: sign out revokes and forgets", sign_out)

    def token_form_still_works():
        # A pasted token (here: a fresh sign-in token) works through "Use a token instead".
        body = json.dumps({"username": "rustybin", "password": new_password}).encode()
        req = urllib.request.Request(base + "/_rustybin/login", data=body, headers={"Content-Type": "application/json"}, method="POST")
        with urllib.request.urlopen(req, timeout=10) as r:
            token = json.load(r)["token"]
        page.locator("#signin-token-details summary").click()
        page.fill("#signin-token", token)
        page.get_by_role("button", name="Use this token").click()
        # Back on Management, General, where the sign-out happened.
        page.wait_for_selector("form[aria-label='Settings: console']")
        expect(page.locator("#signin")).to_have_count(0)
        expect(page.locator("#signed-in")).to_contain_text("rustybin")
        # Leave the instance with the default password for the next run.
        req = urllib.request.Request(base + "/_rustybin/settings/console.password_hash", headers={"Authorization": "Bearer " + token}, method="DELETE")
        with urllib.request.urlopen(req, timeout=10) as r:
            assert r.status == 200

    step("password: a pasted token still signs in", token_form_still_works)
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
        if os.environ.get("RUSTYBIN_SS_URL"):
            run_self_service(browser)
        if os.environ.get("RUSTYBIN_HARNESS_URL"):
            run_harness(browser)
        if os.environ.get("RUSTYBIN_PASSWORD_URL"):
            run_password(browser)
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
