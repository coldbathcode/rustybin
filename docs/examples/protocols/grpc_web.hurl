# gRPC-Web on the gRPC port (docs/src/reference/grpc.md)

# ANCHOR: grpc_web
# One length-prefixed frame: flag 0x00, length 7, EchoRequest{message: "hello"}.
POST {{grpc_url}}/rustybin.echo.v1.EchoService/Echo
Content-Type: application/grpc-web+proto
X-Grpc-Web: 1
hex,00000000070a0568656c6c6f;
HTTP 200
[Asserts]
header "Content-Type" == "application/grpc-web+proto"
# The reply frame contains "hello"; the trailer frame carries grpc-status:0.
bytes contains hex,68656c6c6f;
bytes contains hex,677270632d7374617475733a30;
# ANCHOR_END: grpc_web

# ANCHOR: grpc_web_cors
OPTIONS {{grpc_url}}/rustybin.echo.v1.EchoService/Echo
Origin: https://app.example.com
Access-Control-Request-Method: POST
Access-Control-Request-Headers: content-type,x-grpc-web
HTTP 200
[Asserts]
header "Access-Control-Allow-Origin" == "https://app.example.com"
# ANCHOR_END: grpc_web_cors
