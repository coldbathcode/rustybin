"""Drive the mock LLM with the official Anthropic Python SDK.

Covers messages (system blocks with cache_control and cache usage),
streaming (text_stream, event types, final message), tool use including
streamed input_json_delta and the tool_result round trip, forced tool
output, stop sequences, max_tokens, count_tokens, models and native errors.
"""

import uuid

import anthropic

from _common import AUTH_BASE, BASE, check, finish, run

client = anthropic.Anthropic(base_url=f"{BASE}/ai/anthropic", api_key="sk-ant-conformance-9999", max_retries=0)

WEATHER = {
    "name": "get_weather",
    "description": "Get the current weather for a location",
    "input_schema": {
        "type": "object",
        "properties": {"location": {"type": "string"}, "unit": {"type": "string", "enum": ["celsius", "fahrenheit"]}},
        "required": ["location"],
    },
}


def messages():
    raw = client.messages.with_raw_response.create(
        model="rustybin-claude", max_tokens=256, messages=[{"role": "user", "content": "hello"}]
    )
    m = raw.parse()
    check("message type", m.type == "message" and m.role == "assistant")
    check("text block", m.content[0].type == "text" and "Rustybin" in m.content[0].text)
    check("stop_reason end_turn", m.stop_reason == "end_turn")
    check("usage", m.usage.input_tokens > 0 and m.usage.output_tokens > 0)
    check("credential header", raw.headers.get("x-rustybin-credential") == "x-api-key ****9999")
    check("anthropic ratelimit headers", raw.headers.get("anthropic-ratelimit-tokens-remaining") is not None)
    # Unique per run: the mock remembers cached prefixes for 5 minutes.
    system = [{"type": "text", "text": f"Run {uuid.uuid4()}. " + "You are a helpful assistant. " * 20, "cache_control": {"type": "ephemeral"}}]
    a = client.messages.create(model="claude-cache", max_tokens=64, system=system, messages=[{"role": "user", "content": "hi"}])
    b = client.messages.create(model="claude-cache", max_tokens=64, system=system, messages=[{"role": "user", "content": "hi"}])
    check("cache creation then read",
          a.usage.cache_creation_input_tokens > 0 and b.usage.cache_read_input_tokens == a.usage.cache_creation_input_tokens,
          (a.usage, b.usage))
    m = client.messages.create(model="claude", max_tokens=3, messages=[{"role": "user", "content": "hello"}])
    check("max_tokens stop", m.stop_reason == "max_tokens" and m.usage.output_tokens == 3)
    m = client.messages.create(model="claude", max_tokens=200, stop_sequences=["mock"], messages=[{"role": "user", "content": "hello"}])
    check("stop_sequence", m.stop_reason == "stop_sequence" and m.stop_sequence == "mock")
    m = client.messages.create(
        model="claude",
        max_tokens=200,
        messages=[{"role": "user", "content": [
            {"type": "text", "text": "describe"},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}},
        ]}],
    )
    check("image block accepted", m.usage.input_tokens > 85)


def streaming():
    events = []
    with client.messages.stream(
        model="rustybin-claude", max_tokens=256, messages=[{"role": "user", "content": "hello"}]
    ) as s:
        text = "".join(s.text_stream)
        final = s.get_final_message()
    check("text_stream", "Rustybin" in text)
    check("final message", final.content[0].text == text and final.stop_reason == "end_turn")
    raw = client.messages.create(
        model="rustybin-claude", max_tokens=256, stream=True, messages=[{"role": "user", "content": "hello"}]
    )
    for ev in raw:
        events.append(ev.type)
    check("event order", events[0] == "message_start" and events[-1] == "message_stop" and "message_delta" in events, events[:4])


def tools():
    msgs = [{"role": "user", "content": "What is the weather in Paris?"}]
    m = client.messages.create(model="rustybin-claude", max_tokens=512, tools=[WEATHER], messages=msgs)
    tu = [b for b in m.content if b.type == "tool_use"][0]
    check("tool_use", m.stop_reason == "tool_use" and tu.name == "get_weather" and tu.input["location"] == "Paris", tu)
    msgs += [
        {"role": "assistant", "content": [b.model_dump() for b in m.content]},
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tu.id, "content": "18C and cloudy"}]},
    ]
    m2 = client.messages.create(model="rustybin-claude", max_tokens=512, tools=[WEATHER], messages=msgs)
    check("tool_result answer", "18C and cloudy" in m2.content[0].text)
    with client.messages.stream(
        model="rustybin-claude", max_tokens=512, tools=[WEATHER],
        messages=[{"role": "user", "content": "What is the weather in Paris?"}],
    ) as s:
        final = s.get_final_message()
    tu = [b for b in final.content if b.type == "tool_use"][0]
    check("streamed input_json_delta assembled", tu.input == {"location": "Paris", "unit": "celsius"}, tu.input)
    m = client.messages.create(
        model="rustybin-claude", max_tokens=512, tools=[WEATHER],
        tool_choice={"type": "tool", "name": "get_weather"},
        messages=[{"role": "user", "content": "anything"}],
    )
    check("forced tool output", m.content[0].type == "tool_use")


def misc():
    n = client.messages.count_tokens(model="rustybin-claude", messages=[{"role": "user", "content": "hello"}])
    check("count_tokens", n.input_tokens > 0)
    ids = [m.id for m in client.models.list()]
    check("models list", "rustybin-claude" in ids)
    check("models retrieve", client.models.retrieve("rustybin-claude").id == "rustybin-claude")


def errors():
    try:
        client.messages.create(model="claude", max_tokens=10, messages=[{"role": "user", "content": "x"}],
                               extra_headers={"X-Rustybin-Fail": "529"})
        check("529 raises", False)
    except anthropic.OverloadedError as e:
        check("529 OverloadedError", e.body["error"]["type"] == "overloaded_error")
    try:
        client.messages.create(model="claude", max_tokens=10, messages=[{"role": "user", "content": "x"}],
                               extra_headers={"X-Rustybin-Fail": "rate_limit"})
        check("429 raises", False)
    except anthropic.RateLimitError as e:
        check("429 RateLimitError", e.response.headers.get("anthropic-ratelimit-requests-remaining") == "0")
    try:
        client.messages.create(model="claude", max_tokens=10, messages=[{"role": "user", "content": "x"}],
                               extra_headers={"X-Rustybin-Require-Auth": "true", "X-Api-Key": anthropic.omit})
        check("missing key raises", False)
    except anthropic.AuthenticationError as e:
        check("missing key AuthenticationError", e.body["error"]["type"] == "authentication_error")
    if AUTH_BASE:
        bad = anthropic.Anthropic(base_url=f"{AUTH_BASE}/ai/anthropic", api_key="wrong", max_retries=0)
        try:
            bad.messages.create(model="claude", max_tokens=10, messages=[{"role": "user", "content": "x"}])
            check("wrong key raises", False)
        except anthropic.AuthenticationError:
            check("wrong key AuthenticationError", True)
        good = anthropic.Anthropic(base_url=f"{AUTH_BASE}/ai/anthropic", api_key="conformance-secret", max_retries=0)
        check("expected key accepted", good.messages.create(model="c", max_tokens=10, messages=[{"role": "user", "content": "x"}]).type == "message")


for title, fn in [("messages", messages), ("streaming", streaming), ("tools", tools), ("count tokens, models", misc), ("errors", errors)]:
    run(title, fn)
finish()
