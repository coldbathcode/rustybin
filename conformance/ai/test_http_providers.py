"""Providers without an SDK in the conformance venv, driven with httpx.

- Bedrock: Converse, ConverseStream and InvokeModelWithResponseStream are
  decoded with an independent AWS event stream parser (prelude and message
  CRC32 checked with zlib.crc32). boto3 is not installed, so the request is
  signed with a hand-built SigV4 header (the mock checks the structure only).
- Ollama: NDJSON streaming chat and generate, tags, embed.
- Cohere: rerank and embed.
- Cross-cutting: X-Rustybin-Request-Id + /ai/requests/{id}, latency and TTFT headers.
"""

import base64
import datetime
import hashlib
import hmac
import json
import struct
import time
import zlib

import httpx

from _common import BASE, check, finish, run

http = httpx.Client(base_url=BASE, timeout=30)


def decode_eventstream(buf: bytes):
    """Independent AWS event stream decoder (string headers only)."""
    out = []
    while buf:
        total, hlen, pcrc = struct.unpack(">III", buf[:12])
        assert zlib.crc32(buf[:8]) == pcrc, "prelude crc"
        (mcrc,) = struct.unpack(">I", buf[total - 4 : total])
        assert zlib.crc32(buf[: total - 4]) == mcrc, "message crc"
        headers, i, hb = {}, 0, buf[12 : 12 + hlen]
        while i < len(hb):
            n = hb[i]
            name = hb[i + 1 : i + 1 + n].decode()
            i += 1 + n
            assert hb[i] == 7, "string header"
            (vl,) = struct.unpack(">H", hb[i + 1 : i + 3])
            headers[name] = hb[i + 3 : i + 3 + vl].decode()
            i += 3 + vl
        out.append((headers, buf[12 + hlen : total - 4]))
        buf = buf[total:]
    return out


def sigv4_headers(body: bytes, path: str):
    """A real SigV4 signature (fake credentials) for the structural check."""
    akid, secret, region, service = "AKIDCONFORMANCE1", "secret", "us-east-1", "bedrock"
    now = datetime.datetime.now(datetime.timezone.utc)
    amz_date, day = now.strftime("%Y%m%dT%H%M%SZ"), now.strftime("%Y%m%d")
    host = BASE.split("://", 1)[1]
    canonical = "\n".join(["POST", path, "", f"host:{host}", f"x-amz-date:{amz_date}", "", "host;x-amz-date", hashlib.sha256(body).hexdigest()])
    scope = f"{day}/{region}/{service}/aws4_request"
    to_sign = "\n".join(["AWS4-HMAC-SHA256", amz_date, scope, hashlib.sha256(canonical.encode()).hexdigest()])
    k = ("AWS4" + secret).encode()
    for part in (day, region, service, "aws4_request"):
        k = hmac.new(k, part.encode(), hashlib.sha256).digest()
    sig = hmac.new(k, to_sign.encode(), hashlib.sha256).hexdigest()
    return {
        "Authorization": f"AWS4-HMAC-SHA256 Credential={akid}/{scope}, SignedHeaders=host;x-amz-date, Signature={sig}",
        "X-Amz-Date": amz_date,
        "Content-Type": "application/json",
    }


MODEL = "anthropic.claude-3-5-sonnet-20240620-v1:0"
TOOLS = {"tools": [{"toolSpec": {"name": "get_weather", "description": "Get the current weather for a location",
                                 "inputSchema": {"json": {"type": "object", "properties": {"location": {"type": "string"}}, "required": ["location"]}}}}]}


def bedrock():
    path = f"/ai/bedrock/model/{MODEL}/converse"
    body = json.dumps({"messages": [{"role": "user", "content": [{"text": "hello"}]}], "inferenceConfig": {"maxTokens": 100}}).encode()
    r = http.post(path, content=body, headers={**sigv4_headers(body, path), "X-Rustybin-Require-Auth": "true"})
    check("converse signed", r.status_code == 200 and r.json()["stopReason"] == "end_turn", r.text[:200])
    check("sigv4 credential reported", r.headers.get("x-rustybin-credential") == "sigv4 ****NCE1")
    r = http.post(path, content=body, headers={"Content-Type": "application/json", "X-Rustybin-Require-Auth": "true"})
    check("unsigned 403", r.status_code == 403 and r.headers.get("x-amzn-errortype") == "MissingAuthenticationTokenException")

    r = http.post(f"/ai/bedrock/model/{MODEL}/converse-stream",
                  json={"messages": [{"role": "user", "content": [{"text": "What is the weather in Paris?"}]}], "toolConfig": TOOLS})
    check("converse-stream content type", r.headers["content-type"] == "application/vnd.amazon.eventstream")
    msgs = decode_eventstream(r.content)
    types = [h[":event-type"] for h, _ in msgs]
    check("event sequence", types[0] == "messageStart" and types[-2:] == ["messageStop", "metadata"], types)
    args = "".join(json.loads(p)["delta"]["toolUse"]["input"] for h, p in msgs if h[":event-type"] == "contentBlockDelta")
    check("toolUse input assembled", json.loads(args) == {"location": "Paris"})

    r = http.post(f"/ai/bedrock/model/{MODEL}/invoke-with-response-stream",
                  json={"anthropic_version": "bedrock-2023-05-31", "max_tokens": 100, "messages": [{"role": "user", "content": "hello"}]})
    events = [json.loads(base64.b64decode(json.loads(p)["bytes"])) for h, p in decode_eventstream(r.content)]
    text = "".join(e["delta"].get("text", "") for e in events if e["type"] == "content_block_delta")
    check("invoke stream anthropic events", events[0]["type"] == "message_start" and "Rustybin" in text)
    r = http.post(f"/ai/bedrock/model/{MODEL}/converse", json={"messages": []}, headers={"X-Rustybin-Fail": "throttling"})
    check("throttling", r.status_code == 429 and r.headers.get("x-amzn-errortype") == "ThrottlingException")


def ollama():
    with http.stream("POST", "/ai/ollama/api/chat", json={"model": "llama3.2", "messages": [{"role": "user", "content": "hello"}]}) as r:
        lines = [json.loads(l) for l in r.iter_lines() if l]
    check("ndjson default stream", len(lines) > 2 and lines[-1]["done"] is True)
    check("ndjson text", "Rustybin" in "".join(l["message"]["content"] for l in lines))
    r = http.post("/ai/ollama/api/generate", json={"model": "llama3.2", "prompt": "hello", "stream": False})
    check("generate non-stream", r.json()["done"] and "Rustybin" in r.json()["response"])
    check("tags", len(http.get("/ai/ollama/api/tags").json()["models"]) > 0)
    e = http.post("/ai/ollama/api/embed", json={"model": "nomic-embed-text", "input": ["a b", "b a"]}).json()
    check("embed", len(e["embeddings"]) == 2 and len(e["embeddings"][0]) == 768)


def cohere():
    r = http.post("/ai/cohere/v2/rerank", json={"model": "rerank-v3.5", "query": "capital of France",
                                                 "documents": ["Bananas are yellow.", "Paris is the capital of France."], "top_n": 1})
    res = r.json()["results"]
    check("rerank top_n and order", len(res) == 1 and res[0]["index"] == 1, res)
    e = http.post("/ai/cohere/v2/embed", json={"texts": ["hi"], "embedding_types": ["float"]}).json()
    check("cohere embed", len(e["embeddings"]["float"][0]) == 1024)


def inspection_and_timing():
    r = http.post("/ai/openai/v1/chat/completions", json={"model": "gpt-4o", "messages": [{"role": "user", "content": "hello"}]},
                  headers={"Authorization": "Bearer injected-by-gateway-abcd", "X-Rustybin-Session": "conformance"})
    rid = r.headers["x-rustybin-request-id"]
    rec = http.get(f"/ai/requests/{rid}").json()
    check("inspection record", rec["provider"] == "openai" and rec["normalized_prompt"] == "user: hello", rec)
    check("inspection redacts credential", rec["headers"]["authorization"] == "Bearer ****abcd")
    t = time.time()
    http.post("/ai/openai/v1/chat/completions", json={"messages": [{"role": "user", "content": "hi"}]}, headers={"X-Rustybin-Latency-Ms": "300"})
    check("latency header", time.time() - t >= 0.3)
    t = time.time()
    with http.stream("POST", "/ai/openai/v1/chat/completions", json={"stream": True, "messages": [{"role": "user", "content": "hi"}]},
                     headers={"X-Rustybin-TTFT-Ms": "400", "X-Rustybin-Tokens-Per-Second": "0"}) as r:
        headers_at = time.time() - t
        first = None
        for line in r.iter_lines():
            if line.startswith("data: ") and '"content":"' in line and '"content":""' not in line:
                first = time.time() - t
                break
    check("ttft after headers", headers_at < 0.3 and first is not None and first >= 0.4, (headers_at, first))


for title, fn in [("bedrock", bedrock), ("ollama", ollama), ("cohere", cohere), ("inspection and timing", inspection_and_timing)]:
    run(title, fn)
finish()
