# SOAP 1.1 / 1.2 (docs/src/reference/soap.md)

# ANCHOR: soap11
POST {{base_url}}/soap
Content-Type: text/xml; charset=utf-8
SOAPAction: "http://rustybin.local/GetUser"
```
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <GetUser xmlns="http://rustybin.local/users"><userId>123</userId></GetUser>
  </soap:Body>
</soap:Envelope>
```
HTTP 200
[Asserts]
header "Content-Type" == "text/xml; charset=utf-8"
xpath "string(//*[local-name()='GetUserResponse']/*[local-name()='user']/*[local-name()='id'])" == "123"
# ANCHOR_END: soap11

# ANCHOR: soap12
POST {{base_url}}/soap
Content-Type: application/soap+xml; charset=utf-8; action="http://rustybin.local/CreateOrder"
```
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope">
  <env:Body>
    <CreateOrder xmlns="http://rustybin.local/orders">
      <merchantId>M001</merchantId><amount>42.50</amount><currency>EUR</currency>
    </CreateOrder>
  </env:Body>
</env:Envelope>
```
HTTP 200
[Asserts]
header "Content-Type" == "application/soap+xml; charset=utf-8"
xpath "string(//*[local-name()='CreateOrderResponse']/*[local-name()='status'])" == "ACCEPTED"
xpath "string(//*[local-name()='CreateOrderResponse']/*[local-name()='amount'])" == "42.50"
# ANCHOR_END: soap12

# ANCHOR: fault11
POST {{base_url}}/soap
Content-Type: text/xml
```
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body><DeleteEverything/></soap:Body>
</soap:Envelope>
```
HTTP 500
[Asserts]
xpath "string(//faultcode)" == "soap:Client"
xpath "string(//faultstring)" == "Unknown operation"
# ANCHOR_END: fault11

# ANCHOR: fault12
POST {{base_url}}/soap
Content-Type: application/soap+xml
```
<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope">
  <env:Body><DeleteEverything/></env:Body>
</env:Envelope>
```
HTTP 400
[Asserts]
xpath "string(//*[local-name()='Code']/*[local-name()='Value'])" == "env:Sender"
# ANCHOR_END: fault12

# ANCHOR: wsdl
GET {{base_url}}/soap?wsdl
HTTP 200
[Asserts]
header "Content-Type" contains "xml"
xpath "count(//*[local-name()='operation' and @name='GetStatus'])" >= 1
body contains "{{base_url}}/soap"
# ANCHOR_END: wsdl
