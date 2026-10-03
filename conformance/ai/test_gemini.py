"""Drive the mock LLM with the official Google Gen AI SDK (google-genai).

The SDK is pointed at the mock with http_options.base_url; it then calls
{base}/v1beta/models/{model}:generateContent and friends.
"""

from google import genai
from google.genai import errors, types
from pydantic import BaseModel

from _common import BASE, check, finish, run

client = genai.Client(
    api_key="gemini-conformance-4321",
    http_options=types.HttpOptions(base_url=f"{BASE}/ai/gemini", api_version="v1beta"),
)
MODEL = "gemini-2.5-flash"


def generate():
    r = client.models.generate_content(
        model=MODEL,
        contents="hello",
        config=types.GenerateContentConfig(system_instruction="Be brief."),
    )
    check("text", "Rustybin" in r.text)
    check("finish STOP", r.candidates[0].finish_reason == types.FinishReason.STOP)
    check("usage metadata", r.usage_metadata.total_token_count == r.usage_metadata.prompt_token_count + r.usage_metadata.candidates_token_count)
    r = client.models.generate_content(model=MODEL, contents="hello", config=types.GenerateContentConfig(max_output_tokens=3))
    check("MAX_TOKENS", r.candidates[0].finish_reason == types.FinishReason.MAX_TOKENS)
    r = client.models.generate_content(
        model="rustybin-echo",
        contents="what did you get?",
        config=types.GenerateContentConfig(system_instruction="SYS"),
    )
    check("echo mode", r.text == "system: SYS\nuser: what did you get?", r.text)


def stream():
    text = ""
    last = None
    for chunk in client.models.generate_content_stream(model=MODEL, contents="hello"):
        text += chunk.text or ""
        last = chunk
    check("stream text", "Rustybin" in text)
    check("stream usage", last is not None and last.usage_metadata.candidates_token_count > 0)


def get_weather(location: str) -> str:
    """Get the current weather for a location."""
    return "sunny"


def functions():
    decl = types.FunctionDeclaration(
        name="get_weather",
        description="Get the current weather for a location",
        parameters=types.Schema(
            type=types.Type.OBJECT,
            properties={"location": types.Schema(type=types.Type.STRING)},
            required=["location"],
        ),
    )
    cfg = types.GenerateContentConfig(
        tools=[types.Tool(function_declarations=[decl])],
        automatic_function_calling=types.AutomaticFunctionCallingConfig(disable=True),
    )
    r = client.models.generate_content(model=MODEL, contents="What is the weather in Paris?", config=cfg)
    fc = r.function_calls[0]
    check("function call", fc.name == "get_weather" and fc.args == {"location": "Paris"}, fc)
    contents = [
        types.Content(role="user", parts=[types.Part(text="What is the weather in Paris?")]),
        r.candidates[0].content,
        types.Content(role="user", parts=[types.Part.from_function_response(name="get_weather", response={"result": "sunny"})]),
    ]
    r2 = client.models.generate_content(model=MODEL, contents=contents, config=cfg)
    check("function response answer", "sunny" in r2.text)
    # Automatic function calling: the SDK runs the Python function and calls again.
    r3 = client.models.generate_content(
        model=MODEL, contents="What is the weather in Paris?", config=types.GenerateContentConfig(tools=[get_weather])
    )
    check("automatic function calling", "sunny" in (r3.text or ""), r3.text)


class Recipe(BaseModel):
    recipe_name: str
    ingredients: list[str]


def structured():
    r = client.models.generate_content(
        model=MODEL,
        contents="List a cookie recipe",
        config=types.GenerateContentConfig(response_mime_type="application/json", response_schema=Recipe),
    )
    check("response_schema parsed", isinstance(r.parsed, Recipe) and len(r.parsed.ingredients) >= 1, r.text)


def tokens_embeddings_models():
    c = client.models.count_tokens(model=MODEL, contents="hello world")
    check("count_tokens", c.total_tokens > 0)
    e = client.models.embed_content(
        model="text-embedding-004",
        contents=["Hello world", "world hello"],
        config=types.EmbedContentConfig(output_dimensionality=64),
    )
    check("embed_content", len(e.embeddings) == 2 and len(e.embeddings[0].values) == 64)
    names = [m.name for m in client.models.list()]
    check("models list", "models/gemini-2.5-flash" in names, names[:3])
    check("models get", client.models.get(model=MODEL).name == "models/gemini-2.5-flash")


def errs():
    try:
        client.models.generate_content(
            model=MODEL, contents="x", config=types.GenerateContentConfig(http_options=types.HttpOptions(headers={"X-Rustybin-Fail": "429"}))
        )
        check("429 raises", False)
    except errors.ClientError as e:
        check("429 RESOURCE_EXHAUSTED", e.code == 429 and e.status == "RESOURCE_EXHAUSTED", e)


for title, fn in [
    ("generate", generate),
    ("stream", stream),
    ("function calling", functions),
    ("structured output", structured),
    ("tokens, embeddings, models", tokens_embeddings_models),
    ("errors", errs),
]:
    run(title, fn)
finish()
