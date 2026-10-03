# Redirects and cookies (docs/src/reference/redirects-cookies.md)

# ANCHOR: redirect_chain
GET {{base_url}}/redirect/3?from=docs
HTTP 302
[Asserts]
header "Location" == "/redirect/2?from=docs"

# Follow the whole chain: it ends at /echo with the query string kept.
GET {{base_url}}/redirect/3?from=docs
[Options]
location: true
HTTP 200
[Asserts]
url endsWith "/echo?from=docs"
jsonpath "$.query_params.from" == "docs"
redirects count == 3
# ANCHOR_END: redirect_chain

# ANCHOR: redirect_to
GET {{base_url}}/redirect-to?url=/status/201&status_code=307
HTTP 307
[Asserts]
header "Location" == "/status/201"
# ANCHOR_END: redirect_to

# ANCHOR: redirect_to_refused
# Other hosts are refused: no open redirect.
GET {{base_url}}/redirect-to?url=https://example.com/
HTTP 400
[Asserts]
jsonpath "$.error" == "redirect target refused"
# ANCHOR_END: redirect_to_refused

# ANCHOR: absolute_redirect
GET {{base_url}}/absolute-redirect/2
HTTP 302
[Asserts]
header "Location" == "{{base_url}}/absolute-redirect/1"
# ANCHOR_END: absolute_redirect

# ANCHOR: cookies_set
GET {{base_url}}/cookies/set?session=abc123&theme=dark&_httponly=true&_maxage=3600
HTTP 302
[Asserts]
header "Location" == "/cookies"
header "Set-Cookie" count == 2
header "Set-Cookie" contains "session=abc123; Path=/; Max-Age=3600; HttpOnly"
# ANCHOR_END: cookies_set

# ANCHOR: cookies_samesite
GET {{base_url}}/cookies/set/theme/dark
HTTP 302
[Asserts]
header "Set-Cookie" == "theme=dark; Path=/"

GET {{base_url}}/cookies/set?tracking=1&_samesite=None
HTTP 302
[Asserts]
header "Set-Cookie" contains "SameSite=None"
header "Set-Cookie" contains "Secure"
# ANCHOR_END: cookies_samesite

# ANCHOR: cookies_read
GET {{base_url}}/cookies
Cookie: session=abc123; theme=dark
HTTP 200
[Asserts]
jsonpath "$.cookies.session" == "abc123"
jsonpath "$.cookies.theme" == "dark"
# ANCHOR_END: cookies_read

# ANCHOR: cookies_delete
GET {{base_url}}/cookies/delete?session&theme
HTTP 302
[Asserts]
header "Set-Cookie" count == 2
header "Set-Cookie" contains "session=; Path=/; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT"
# ANCHOR_END: cookies_delete
