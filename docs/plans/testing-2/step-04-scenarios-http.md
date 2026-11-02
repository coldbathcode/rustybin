# Step 04: Scenarios: CORS and header transformation

## Goal

Two more outcomes: the gateway answers CORS preflights for an allowed origin (and not for another),
and the gateway adds, removes or rewrites headers on the way up and down.

## Model

- `cors`: the check sends an `OPTIONS` preflight from an allowed origin and from a refused one (both
  parameters), then a simple request; expected: the allowed preflight answers the CORS headers
  without reaching the upstream (or reaching it, when the operator says the upstream answers CORS),
  the refused one gets none.
- `header_transformation`: the operator names a request header the gateway adds, one it removes
  and a response header it adds; the check sends the removed one, and reads the upstream capture and
  the answer.

## Surfaces

As step 03: catalogue rows, the docs page.

## Tests

As step 03, with the stand-in gateway answering preflights and transforming headers.
