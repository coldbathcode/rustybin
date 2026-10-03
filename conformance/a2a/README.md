# A2A conformance (official a2a-sdk client)

`a2a_conformance.py` drives rustybin's mock A2A agents (`/a2a/{agent}`) with the
official Python SDK client (`a2a-sdk` 1.2.x, `pip install a2a-sdk httpx`).

Run (builds the binary, starts it on ports 18500-18502, runs the checks):

```bash
PYTHON=/path/to/venv/bin/python conformance/a2a/run.sh
```

Or against a running server: `python conformance/a2a/a2a_conformance.py --base http://127.0.0.1:18500`.

What is checked, for every agent (echo, weather, travel-planner, approval, flaky, secure, reject):

- v1.0 over JSON-RPC and HTTP+JSON (card from `/a2a/{agent}/.well-known/agent-card.json`)
- v0.3 over JSON-RPC and HTTP+JSON (SDK compat transports, card from `/a2a/{agent}/.well-known/agent.json`)
- card resolution, blocking send (expected final state per agent), get task (historyLength),
  streaming send (first event is the Task, terminal/interrupted state ends the stream),
  cancel (running task -> CANCELED, terminal task -> TaskNotCancelableError), unknown task -> TaskNotFoundError
- travel-planner: artifact chunks (append / lastChunk), text/data/url/raw parts, return immediately, subscribe
- approval: input-required continuation with the same taskId
- secure: auth-required without a token, completed with a client_credentials token from `/oauth/token`, extended card
- echo: direct message reply
- push config create/get/list/delete, delivery to the built-in sink `/a2a/webhook-sink/{id}`
  (Authorization and X-A2A-Notification-Token headers), SSRF URL rejection, ListTasks pagination

Exit code 0 when every check passes.
