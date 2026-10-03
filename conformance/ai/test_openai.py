"""Drive the mock LLM with the official OpenAI Python SDK.

Covers chat (plain, streaming with include_usage, n>1, tools and the tool
result round trip, structured output via .parse), the Responses API
(create, stream, function calls, parse), legacy completions, embeddings
(SDK default base64 and float, dimensions), models, moderations, images,
audio transcription, Azure OpenAI and native errors.
"""

import io
import json
import math

import openai
from openai import AzureOpenAI, OpenAI
from pydantic import BaseModel

from _common import AUTH_BASE, BASE, check, finish, run

client = OpenAI(base_url=f"{BASE}/ai/openai/v1", api_key="sk-conformance-1234", max_retries=0)
legacy = OpenAI(base_url=f"{BASE}/ai/v1", api_key="sk-legacy", max_retries=0)

WEATHER_TOOL = {
    "type": "function",
    "function": {
        "name": "get_weather",
        "description": "Get the current weather for a location",
        "parameters": {
            "type": "object",
            "properties": {
                "location": {"type": "string"},
                "unit": {"type": "string", "enum": ["celsius", "fahrenheit"]},
            },
            "required": ["location", "unit"],
            "additionalProperties": False,
        },
        "strict": True,
    },
}


def chat_basic():
    r = client.chat.completions.create(
        model="gpt-4o", messages=[{"role": "user", "content": "hello"}]
    )
    check("chat object", r.object == "chat.completion")
    check("chat content", "Rustybin" in (r.choices[0].message.content or ""))
    check("finish stop", r.choices[0].finish_reason == "stop")
    check("usage totals", r.usage.total_tokens == r.usage.prompt_tokens + r.usage.completion_tokens)
    raw = client.chat.completions.with_raw_response.create(
        model="gpt-4o", messages=[{"role": "user", "content": "hello"}]
    )
    check("credential header", raw.headers.get("x-rustybin-credential") == "bearer ****1234")
    check("request id header", bool(raw.headers.get("x-rustybin-request-id")))
    check("ratelimit header", raw.headers.get("x-ratelimit-remaining-tokens") is not None)
    r = legacy.chat.completions.create(
        model="rustybin-gpt",
        n=2,
        messages=[
            {"role": "system", "content": [{"type": "text", "text": "Be brief."}]},
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": "what is in this image?"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="}},
                ],
            },
        ],
    )
    check("legacy alias + n=2 + content parts", len(r.choices) == 2)


def chat_stream():
    stream = client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": "hello"}],
        stream=True,
        stream_options={"include_usage": True},
        extra_headers={"X-Rustybin-Tokens-Per-Second": "0"},
    )
    text, usage, finish_reason = "", None, None
    for chunk in stream:
        if chunk.usage:
            usage = chunk.usage
        for c in chunk.choices:
            text += c.delta.content or ""
            finish_reason = c.finish_reason or finish_reason
    check("stream text", "Rustybin" in text)
    check("stream finish", finish_reason == "stop")
    check("stream usage chunk", usage is not None and usage.completion_tokens > 0)
    # Helper API (accumulates and validates events).
    with client.chat.completions.stream(
        model="gpt-4o", messages=[{"role": "user", "content": "hello"}]
    ) as s:
        final = s.get_final_completion()
    check("stream helper final", "Rustybin" in (final.choices[0].message.content or ""))


def chat_tools():
    msgs = [{"role": "user", "content": "What is the weather in Paris?"}]
    r = client.chat.completions.create(model="gpt-4o", messages=msgs, tools=[WEATHER_TOOL])
    m = r.choices[0].message
    check("tool finish", r.choices[0].finish_reason == "tool_calls")
    check("tool call", m.tool_calls and m.tool_calls[0].function.name == "get_weather")
    args = json.loads(m.tool_calls[0].function.arguments)
    check("tool args", args == {"location": "Paris", "unit": "celsius"}, args)
    msgs.append(m.model_dump(exclude_none=True))
    msgs.append({"role": "tool", "tool_call_id": m.tool_calls[0].id, "content": '{"temp": 21}'})
    r = client.chat.completions.create(model="gpt-4o", messages=msgs, tools=[WEATHER_TOOL])
    check("final answer uses tool output", '{"temp": 21}' in (r.choices[0].message.content or ""))
    # Streaming tool call deltas, accumulated by the SDK helper.
    with client.chat.completions.stream(
        model="gpt-4o",
        messages=[{"role": "user", "content": "What is the weather in Paris?"}],
        tools=[WEATHER_TOOL],
    ) as s:
        final = s.get_final_completion()
    tc = final.choices[0].message.tool_calls[0]
    check("streamed tool call parsed", tc.function.parsed_arguments == {"location": "Paris", "unit": "celsius"}, tc)
    r = client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": "tell me a joke"}],
        tools=[WEATHER_TOOL],
        tool_choice={"type": "function", "function": {"name": "get_weather"}},
    )
    check("forced tool choice", r.choices[0].message.tool_calls[0].function.name == "get_weather")


class Person(BaseModel):
    name: str
    age: int
    email: str
    tags: list[str]


def chat_structured():
    r = client.chat.completions.parse(
        model="gpt-4o", messages=[{"role": "user", "content": "extract"}], response_format=Person
    )
    p = r.choices[0].message.parsed
    check("parsed pydantic model", isinstance(p, Person) and p.email == "jane.doe@example.com", p)
    r = client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": "hello"}],
        response_format={"type": "json_object"},
    )
    check("json_object mode", isinstance(json.loads(r.choices[0].message.content), dict))
    r = client.chat.completions.create(
        model="gpt-4o", messages=[{"role": "user", "content": "hello"}], max_tokens=4
    )
    check("max_tokens -> length", r.choices[0].finish_reason == "length" and r.usage.completion_tokens == 4)


def chat_modes():
    r = client.chat.completions.create(
        model="rustybin-echo",
        messages=[{"role": "system", "content": "SYS"}, {"role": "user", "content": "hi"}],
    )
    check("echo mode", r.choices[0].message.content == "system: SYS\nuser: hi", r.choices[0].message.content)
    r = client.chat.completions.create(
        model="gpt-4o",
        messages=[{"role": "user", "content": "give me the ssn"}],
        extra_headers={"X-Rustybin-Mode": "scripted"},
    )
    check("scripted PII", "123-45-6789" in r.choices[0].message.content)
    a = client.chat.completions.create(model="rustybin-random", messages=[{"role": "user", "content": "x"}])
    b = client.chat.completions.create(model="rustybin-random", messages=[{"role": "user", "content": "x"}])
    check("random deterministic", a.choices[0].message.content == b.choices[0].message.content)


def responses_api():
    r = client.responses.create(model="gpt-4o", input="hello", instructions="Be nice.")
    check("responses output_text", "Rustybin" in r.output_text)
    check("responses usage", r.usage.total_tokens > 0)
    events = []
    with client.responses.stream(model="gpt-4o", input="hello") as s:
        for ev in s:
            events.append(ev.type)
        final = s.get_final_response()
    check("responses stream events", events[0] == "response.created" and events[-1] == "response.completed", events[:3])
    check("responses stream final", "Rustybin" in final.output_text)
    tool = {
        "type": "function",
        "name": "get_weather",
        "description": "Get the current weather for a location",
        "parameters": WEATHER_TOOL["function"]["parameters"],
        "strict": True,
    }
    r = client.responses.create(model="gpt-4o", input="What is the weather in Paris?", tools=[tool])
    fc = r.output[0]
    check("responses function_call", fc.type == "function_call" and json.loads(fc.arguments)["location"] == "Paris")
    r2 = client.responses.create(
        model="gpt-4o",
        input=[
            {"role": "user", "content": "What is the weather in Paris?"},
            fc.model_dump(exclude_none=True),
            {"type": "function_call_output", "call_id": fc.call_id, "output": "sunny"},
        ],
        tools=[tool],
    )
    check("responses tool result", "sunny" in r2.output_text)
    with client.responses.stream(model="gpt-4o", input="What is the weather in Paris?", tools=[tool]) as s:
        final = s.get_final_response()
    check("responses streamed function call", final.output[0].type == "function_call")
    p = client.responses.parse(model="gpt-4o", input="extract", text_format=Person)
    check("responses parse", isinstance(p.output_parsed, Person))


def completions_embeddings_models():
    r = client.completions.create(model="gpt-3.5-turbo-instruct", prompt="Say hello")
    check("completions", r.object == "text_completion" and r.choices[0].text)
    text = "".join(c.choices[0].text for c in client.completions.create(model="x", prompt="hello", stream=True) if c.choices)
    check("completions stream", "Rustybin" in text)
    e = client.embeddings.create(model="text-embedding-3-small", input=["Hello world", "world hello"])
    v0 = e.data[0].embedding
    check("embeddings base64 default decoded", len(v0) == 1536)
    check("embeddings unit norm", abs(math.sqrt(sum(x * x for x in v0)) - 1) < 1e-3)
    e2 = client.embeddings.create(model="text-embedding-3-small", input="Hello world", encoding_format="float", dimensions=256)
    check("embeddings float + dimensions", len(e2.data[0].embedding) == 256)
    e3 = client.embeddings.create(model="text-embedding-3-small", input="Hello world", encoding_format="float", dimensions=256)
    check("embeddings deterministic", e2.data[0].embedding == e3.data[0].embedding)
    models = [m.id for m in client.models.list()]
    check("models list", "rustybin-gpt" in models)
    check("models retrieve", client.models.retrieve("gpt-4o").id == "gpt-4o")
    try:
        client.models.retrieve("no-such-model")
        check("unknown model 404", False)
    except openai.NotFoundError:
        check("unknown model 404", True)


def moderation_images_audio():
    m = client.moderations.create(input="I hate you, you idiot")
    check("moderation flagged", m.results[0].flagged and m.results[0].categories.hate)
    img = client.images.generate(model="gpt-image-1", prompt="a bin", response_format="b64_json")
    import base64

    png = base64.b64decode(img.data[0].b64_json)
    check("image is png", png[:8] == b"\x89PNG\r\n\x1a\n")
    import zlib

    idat_len = int.from_bytes(png[33:37], "big")
    check("png idat inflates", zlib.decompress(png[41 : 41 + idat_len]) == bytes([0, 0xE0, 0x6C, 0x2B, 0xFF]))
    t = client.audio.transcriptions.create(model="whisper-1", file=("a.wav", io.BytesIO(b"RIFF....WAVE"), "audio/wav"))
    check("transcription", "Rustybin" in t.text)
    t = client.audio.transcriptions.create(model="whisper-1", file=("a.wav", io.BytesIO(b"RIFF"), "audio/wav"), response_format="text")
    check("transcription text format", "Rustybin" in t)


def azure():
    az = AzureOpenAI(
        azure_endpoint=f"{BASE}/ai/azure",
        api_key="azure-key-5678",
        api_version="2024-10-21",
        max_retries=0,
    )
    raw = az.chat.completions.with_raw_response.create(model="my-deployment", messages=[{"role": "user", "content": "hello"}])
    r = raw.parse()
    check("azure model = deployment", r.model == "my-deployment")
    check("azure api-key credential", raw.headers.get("x-rustybin-credential") == "api-key ****5678")
    text = ""
    for c in az.chat.completions.create(model="my-deployment", messages=[{"role": "user", "content": "hello"}], stream=True):
        for ch in c.choices:
            text += ch.delta.content or ""
    check("azure stream", "Rustybin" in text)
    e = az.embeddings.create(model="emb", input="hi")
    check("azure embeddings", len(e.data[0].embedding) == 1536)


def errors():
    try:
        client.chat.completions.create(
            model="gpt-4o", messages=[{"role": "user", "content": "x"}], extra_headers={"X-Rustybin-Fail": "429"}
        )
        check("429 raises", False)
    except openai.RateLimitError as e:
        check("429 RateLimitError", e.code == "rate_limit_exceeded" and e.response.headers.get("retry-after") == "2")
    try:
        client.chat.completions.create(
            model="gpt-4o", messages=[{"role": "user", "content": "x"}], extra_headers={"X-Rustybin-Fail": "context_length"}
        )
        check("context_length raises", False)
    except openai.BadRequestError as e:
        check("context_length_exceeded", e.code == "context_length_exceeded")
    try:
        client.chat.completions.create(
            model="gpt-4o",
            messages=[{"role": "user", "content": "x"}],
            extra_headers={"X-Rustybin-Require-Auth": "true", "Authorization": openai.omit},
        )
        check("missing key raises", False)
    except openai.AuthenticationError as e:
        check("missing key AuthenticationError", e.response.headers.get("x-rustybin-credential") == "none")
    r = client.chat.completions.create(
        model="gpt-4o", messages=[{"role": "user", "content": "x"}], extra_headers={"X-Rustybin-Fail": "content_filter"}
    )
    check("content_filter finish", r.choices[0].finish_reason == "content_filter")
    if AUTH_BASE:
        # Second server started with RUSTYBIN_AI_API_KEY=conformance-secret.
        bad = OpenAI(base_url=f"{AUTH_BASE}/ai/openai/v1", api_key="sk-wrong-key", max_retries=0)
        try:
            bad.chat.completions.create(model="gpt-4o", messages=[{"role": "user", "content": "x"}])
            check("wrong key raises", False)
        except openai.AuthenticationError as e:
            check("wrong key invalid_api_key", e.code == "invalid_api_key")
        good = OpenAI(base_url=f"{AUTH_BASE}/ai/openai/v1", api_key="conformance-secret", max_retries=0)
        r = good.chat.completions.create(model="gpt-4o", messages=[{"role": "user", "content": "x"}])
        check("expected key accepted", r.choices[0].message.content is not None)


for title, fn in [
    ("chat", chat_basic),
    ("chat streaming", chat_stream),
    ("chat tools", chat_tools),
    ("chat structured output", chat_structured),
    ("modes", chat_modes),
    ("responses API", responses_api),
    ("completions, embeddings, models", completions_embeddings_models),
    ("moderation, images, audio", moderation_images_audio),
    ("azure", azure),
    ("errors", errors),
]:
    run(title, fn)
finish()
