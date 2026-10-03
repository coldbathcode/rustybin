# Embeddings and rerank

## Embeddings

Every provider's embedding endpoint returns deterministic, similarity-preserving
vectors, which is what a semantic cache plugin needs to be demonstrated: similar
prompts hit the cache, different prompts do not.

An embedding is a hashed bag of features of the lower-cased text, spread over the
requested number of dimensions and normalised to unit length:

- each word (weight 1.0),
- each pair of adjacent words (0.35, so word order matters a little),
- each character trigram of each word (0.15, so "cat" and "cats" are close).

Hashing is FNV-1a, stable across builds and platforms. In practice: identical input
gives identical vectors; case and punctuation are ignored; the same words in another
order have a cosine similarity around 0.9 or more; unrelated texts are near 0.

| Endpoint | Default dimensions | Options |
|---|---|---|
| OpenAI / Azure `embeddings` | 1536 (3072 for `text-embedding-3-large`) | `dimensions`, `encoding_format: base64` |
| Gemini `:embedContent`, `:batchEmbedContents` | 768 | `outputDimensionality` |
| Ollama `api/embed`, `api/embeddings` | 768 | |
| Cohere `v2/embed` | 1024 | `output_dimension`, `embedding_types` (`float`, `int8`, `uint8`, `base64`) |
| Bedrock Titan embeddings (`invoke`) | 1024 | `dimensions` |

Dimensions are capped at 4096 (1536 in public mode) and inputs per request at 2048
(256).

```hurl
{{#include ../../examples/ai/embeddings_guardrails.hurl:embeddings}}
```

```hurl
{{#include ../../examples/ai/embeddings_guardrails.hurl:embeddings_base64}}
```

```hurl
{{#include ../../examples/ai/embeddings_guardrails.hurl:cohere_embed}}
```

## Rerank

`POST /ai/cohere/v2/rerank` and the generic alias `POST /ai/v1/rerank` (also the
shape Jina and Voyage style clients use) score `documents` against `query`:
query-word overlap blended with the cosine similarity of the mock embeddings.
Results are sorted by `relevance_score`; `top_n` limits them and
`return_documents: true` includes the text.

```hurl
{{#include ../../examples/ai/embeddings_guardrails.hurl:rerank}}
```
