# Roadmap

What is being built next. Nothing on this page is available yet; the pages for the
features that exist today describe current behaviour.

## Next: self-service mocks on the hosted demo

Today a [public instance](concepts/sessions.md#public-mode) is read-only for
[OpenAPI mocks](mocks/index.md): creating or changing one needs the admin token. The
next release lets visitors of the hosted demo at rustybin.dev create their own mocks
without an account.

- **Your own mocks.** Drop in an OpenAPI document and get a mock with its own address,
  for example `https://petstore-k3f9q2.rustybin-mock.dev`. The random suffix keeps
  names private; up to 3 mocks per visitor.
- **Only you can manage them.** The console keeps a manage token in your browser.
  Other visitors cannot list, change, delete or read the traffic of your mocks
  (anyone can still call a mock's URL, which is the point). A manage link moves your
  mocks to another browser or a teammate.
- **Time.** Your mocks run for 1 hour. Press Extend for more time, up to 3 hours. To
  keep going after that, give an email address: you then get up to 6 hours of use per
  day (UTC). Product news by email only if you tick the box for it.
- **A separate domain.** Visitor mocks are served only from `rustybin-mock.dev`, never
  from rustybin.dev, and their responses are restricted: no cookies, no redirects to
  other sites, no HTML or scripts (served as plain text).
- **When the demo is busy** or your day is used up, the console says when to come back
  and how to run your own Rustybin ([Quick start](quick-start.md)), which has none of
  these limits.
- **Not included** for visitor mocks: stateful mode (mocks that store what callers send
  and return it later).

Self-hosted instances are unchanged: self-service is off by default, also in public
mode, and only an operator can turn it on.
