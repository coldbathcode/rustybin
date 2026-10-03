# SOAP

A SOAP service with three operations, for XML routing, SOAP-to-REST and WSDL
demos.

| Route | Purpose |
|---|---|
| `POST /soap` | The SOAP endpoint (1.1 and 1.2) |
| `GET /soap?wsdl`, `GET /soap/wsdl` | WSDL 1.1 with SOAP 1.1 and SOAP 1.2 bindings |

| Operation | Namespace of the request element | Input | Output |
|---|---|---|---|
| `GetUser` | `http://rustybin.local/users` | `userId` | `user` with `id`, `name`, `email`, `role` |
| `CreateOrder` | `http://rustybin.local/orders` | `merchantId`, `amount`, `currency` | `orderId`, the input, `status` ACCEPTED, `timestamp` |
| `GetStatus` | `http://rustybin.local/status` | `orderId` | `status`, `createdAt`, `updatedAt`, `tracking` |

**Version**: taken from the envelope namespace (authoritative), else from the
`Content-Type` (`text/xml` is 1.1, `application/soap+xml` is 1.2). The response uses
the same version and content type. An unknown envelope namespace is a
`VersionMismatch` fault.

**Dispatch**: by `SOAPAction` (1.1) or the `action` parameter of the
`Content-Type` (1.2), falling back to the first element of the `Body` when the
action is empty or unknown.

**Faults**: SOAP 1.1 faults (`faultcode`, `faultstring`, `detail`) use HTTP 500;
SOAP 1.2 faults (`Code`, `Reason`, `Detail`) use 400 for `env:Sender` and 500
otherwise, as the SOAP 1.2 HTTP binding specifies. Every value taken from the
request is XML-escaped.

```hurl
{{#include ../../examples/protocols/soap.hurl:soap11}}
```

```hurl
{{#include ../../examples/protocols/soap.hurl:soap12}}
```

```hurl
{{#include ../../examples/protocols/soap.hurl:fault11}}
```

```hurl
{{#include ../../examples/protocols/soap.hurl:fault12}}
```

The WSDL's `soap:address` is derived from the request (`X-Forwarded-*` honoured
with `RUSTYBIN_TRUST_FORWARD=true`), so a WSDL fetched through a gateway points
back at the gateway:

```hurl
{{#include ../../examples/protocols/soap.hurl:wsdl}}
```
