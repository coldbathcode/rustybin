use axum::{
    body::Bytes,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

// ── Constants ───────────────────────────────────────────────────────

const SOAP_CONTENT_TYPE: &str = "text/xml; charset=utf-8";

const WSDL_DOC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://schemas.xmlsoap.org/wsdl/"
             xmlns:soap="http://schemas.xmlsoap.org/wsdl/soap/"
             xmlns:tns="http://rustybin.local/wsdl"
             xmlns:xsd="http://www.w3.org/2001/XMLSchema"
             xmlns:usr="http://rustybin.local/users"
             xmlns:ord="http://rustybin.local/orders"
             xmlns:sts="http://rustybin.local/status"
             name="RustybinService"
             targetNamespace="http://rustybin.local/wsdl">

  <!-- Types -->
  <types>
    <xsd:schema targetNamespace="http://rustybin.local/users">
      <xsd:element name="GetUser">
        <xsd:complexType>
          <xsd:sequence>
            <xsd:element name="userId" type="xsd:string"/>
          </xsd:sequence>
        </xsd:complexType>
      </xsd:element>
      <xsd:element name="GetUserResponse">
        <xsd:complexType>
          <xsd:sequence>
            <xsd:element name="user">
              <xsd:complexType>
                <xsd:sequence>
                  <xsd:element name="id" type="xsd:string"/>
                  <xsd:element name="name" type="xsd:string"/>
                  <xsd:element name="email" type="xsd:string"/>
                  <xsd:element name="role" type="xsd:string"/>
                </xsd:sequence>
              </xsd:complexType>
            </xsd:element>
          </xsd:sequence>
        </xsd:complexType>
      </xsd:element>
    </xsd:schema>
    <xsd:schema targetNamespace="http://rustybin.local/orders">
      <xsd:element name="CreateOrder">
        <xsd:complexType>
          <xsd:sequence>
            <xsd:element name="merchantId" type="xsd:string"/>
            <xsd:element name="amount" type="xsd:integer"/>
            <xsd:element name="currency" type="xsd:string"/>
          </xsd:sequence>
        </xsd:complexType>
      </xsd:element>
      <xsd:element name="CreateOrderResponse">
        <xsd:complexType>
          <xsd:sequence>
            <xsd:element name="orderId" type="xsd:string"/>
            <xsd:element name="status" type="xsd:string"/>
            <xsd:element name="timestamp" type="xsd:string"/>
          </xsd:sequence>
        </xsd:complexType>
      </xsd:element>
    </xsd:schema>
    <xsd:schema targetNamespace="http://rustybin.local/status">
      <xsd:element name="GetStatus">
        <xsd:complexType>
          <xsd:sequence>
            <xsd:element name="orderId" type="xsd:string"/>
          </xsd:sequence>
        </xsd:complexType>
      </xsd:element>
      <xsd:element name="GetStatusResponse">
        <xsd:complexType>
          <xsd:sequence>
            <xsd:element name="orderId" type="xsd:string"/>
            <xsd:element name="status" type="xsd:string"/>
            <xsd:element name="createdAt" type="xsd:string"/>
            <xsd:element name="updatedAt" type="xsd:string"/>
            <xsd:element name="tracking" type="xsd:string"/>
          </xsd:sequence>
        </xsd:complexType>
      </xsd:element>
    </xsd:schema>
  </types>

  <!-- Messages -->
  <message name="GetUserRequest"><part name="parameters" element="usr:GetUser"/></message>
  <message name="GetUserResponse"><part name="parameters" element="usr:GetUserResponse"/></message>
  <message name="CreateOrderRequest"><part name="parameters" element="ord:CreateOrder"/></message>
  <message name="CreateOrderResponse"><part name="parameters" element="ord:CreateOrderResponse"/></message>
  <message name="GetStatusRequest"><part name="parameters" element="sts:GetStatus"/></message>
  <message name="GetStatusResponse"><part name="parameters" element="sts:GetStatusResponse"/></message>

  <!-- Port Type -->
  <portType name="RustybinPortType">
    <operation name="GetUser">
      <input message="tns:GetUserRequest"/>
      <output message="tns:GetUserResponse"/>
    </operation>
    <operation name="CreateOrder">
      <input message="tns:CreateOrderRequest"/>
      <output message="tns:CreateOrderResponse"/>
    </operation>
    <operation name="GetStatus">
      <input message="tns:GetStatusRequest"/>
      <output message="tns:GetStatusResponse"/>
    </operation>
  </portType>

  <!-- Binding -->
  <binding name="RustybinBinding" type="tns:RustybinPortType">
    <soap:binding style="document" transport="http://schemas.xmlsoap.org/soap/http"/>
    <operation name="GetUser">
      <soap:operation soapAction="http://rustybin.local/GetUser"/>
      <input><soap:body use="literal"/></input>
      <output><soap:body use="literal"/></output>
    </operation>
    <operation name="CreateOrder">
      <soap:operation soapAction="http://rustybin.local/CreateOrder"/>
      <input><soap:body use="literal"/></input>
      <output><soap:body use="literal"/></output>
    </operation>
    <operation name="GetStatus">
      <soap:operation soapAction="http://rustybin.local/GetStatus"/>
      <input><soap:body use="literal"/></input>
      <output><soap:body use="literal"/></output>
    </operation>
  </binding>

  <!-- Service -->
  <service name="RustybinService">
    <port name="RustybinPort" binding="tns:RustybinBinding">
      <soap:address location="http://localhost/soap"/>
    </port>
  </service>
</definitions>"#;

// ── SOAP helpers ────────────────────────────────────────────────────

fn soap_envelope(body_inner: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <soap:Envelope xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\">\n\
         <soap:Body>\n\
         {body_inner}\n\
         </soap:Body>\n\
         </soap:Envelope>"
    )
}

fn soap_fault(code: &str, message: &str, detail: &str) -> String {
    soap_envelope(&format!(
        "<soap:Fault>\n\
         <faultcode>{code}</faultcode>\n\
         <faultstring>{message}</faultstring>\n\
         <detail>{detail}</detail>\n\
         </soap:Fault>"
    ))
}

fn soap_response(status: StatusCode, body: String, soap_action: Option<&str>) -> Response {
    let mut builder = axum::http::Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, SOAP_CONTENT_TYPE);

    if let Some(action) = soap_action {
        builder = builder.header("SOAPAction", action);
    }

    builder
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ── XML parsing ─────────────────────────────────────────────────────

struct ParsedOperation {
    name: String,
    fields: Vec<(String, String)>,
}

fn parse_soap_body(xml: &[u8]) -> Result<ParsedOperation, String> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut in_body = false;
    let mut operation_name: Option<String> = None;
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut current_field: Option<String> = None;
    let mut depth_in_op = 0;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let local = local_name(e.name().as_ref());
                if local == "Body" {
                    in_body = true;
                } else if in_body && operation_name.is_none() {
                    operation_name = Some(local.to_string());
                    depth_in_op = 1;
                } else if operation_name.is_some() {
                    depth_in_op += 1;
                    if depth_in_op == 2 {
                        current_field = Some(local.to_string());
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let local = local_name(e.name().as_ref());
                if local == "Body" {
                    in_body = false;
                } else if operation_name.is_some() {
                    if depth_in_op == 2 {
                        current_field = None;
                    }
                    depth_in_op -= 1;
                    if depth_in_op == 0 {
                        break;
                    }
                }
            }
            Ok(Event::Text(ref e)) => {
                if let Some(ref field) = current_field {
                    let text = e.unescape().unwrap_or_default().to_string();
                    fields.push((field.clone(), text));
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML parse error: {e}")),
            _ => {}
        }
        buf.clear();
    }

    match operation_name {
        Some(name) => Ok(ParsedOperation { name, fields }),
        None => Err("No operation found in SOAP Body".to_string()),
    }
}

fn local_name(full: &[u8]) -> String {
    let s = String::from_utf8_lossy(full);
    match s.rfind(':') {
        Some(pos) => s[pos + 1..].to_string(),
        None => s.to_string(),
    }
}

fn get_field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

// ── Operation handlers ──────────────────────────────────────────────

fn handle_get_user(op: &ParsedOperation) -> String {
    let user_id = get_field(&op.fields, "userId").unwrap_or("0");

    // Generate deterministic user data based on ID
    let (name, email, role) = match user_id {
        "123" => ("Jane Smith", "jane@rustybin.local", "ADMIN"),
        "456" => ("Bob Wilson", "bob@rustybin.local", "USER"),
        _ => ("Demo User", "demo@rustybin.local", "GUEST"),
    };

    soap_envelope(&format!(
        "<GetUserResponse xmlns=\"http://rustybin.local/users\">\n\
         <user>\n\
         <id>{user_id}</id>\n\
         <name>{name}</name>\n\
         <email>{email}</email>\n\
         <role>{role}</role>\n\
         </user>\n\
         </GetUserResponse>"
    ))
}

fn handle_create_order(op: &ParsedOperation) -> String {
    let merchant_id = get_field(&op.fields, "merchantId").unwrap_or("unknown");
    let amount = get_field(&op.fields, "amount").unwrap_or("0");
    let currency = get_field(&op.fields, "currency").unwrap_or("GBP");
    let order_id = format!("ORD-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let timestamp = chrono::Utc::now().to_rfc3339();

    soap_envelope(&format!(
        "<CreateOrderResponse xmlns=\"http://rustybin.local/orders\">\n\
         <orderId>{order_id}</orderId>\n\
         <merchantId>{merchant_id}</merchantId>\n\
         <amount>{amount}</amount>\n\
         <currency>{currency}</currency>\n\
         <status>ACCEPTED</status>\n\
         <timestamp>{timestamp}</timestamp>\n\
         </CreateOrderResponse>"
    ))
}

fn handle_get_status(op: &ParsedOperation) -> String {
    let order_id = get_field(&op.fields, "orderId").unwrap_or("ORD-000");
    let now = chrono::Utc::now();
    let created = (now - chrono::Duration::hours(2)).to_rfc3339();
    let updated = now.to_rfc3339();

    soap_envelope(&format!(
        "<GetStatusResponse xmlns=\"http://rustybin.local/status\">\n\
         <orderId>{order_id}</orderId>\n\
         <status>PROCESSING</status>\n\
         <createdAt>{created}</createdAt>\n\
         <updatedAt>{updated}</updatedAt>\n\
         <tracking>TRK-{order_id}-001</tracking>\n\
         </GetStatusResponse>"
    ))
}

// ── Handlers ────────────────────────────────────────────────────────

async fn soap_handler(headers: HeaderMap, body: Bytes) -> Response {
    let soap_action = headers
        .get("SOAPAction")
        .or_else(|| headers.get("soapaction"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"'));

    let parsed = match parse_soap_body(&body) {
        Ok(op) => op,
        Err(e) => {
            return soap_response(
                StatusCode::BAD_REQUEST,
                soap_fault("soap:Client", "Malformed XML", &e),
                None,
            );
        }
    };

    let response_xml = match parsed.name.as_str() {
        "GetUser" => handle_get_user(&parsed),
        "CreateOrder" => handle_create_order(&parsed),
        "GetStatus" => handle_get_status(&parsed),
        other => {
            return soap_response(
                StatusCode::BAD_REQUEST,
                soap_fault(
                    "soap:Client",
                    "Unknown operation",
                    &format!(
                        "The requested operation '{}' is not supported. \
                         Available operations: GetUser, CreateOrder, GetStatus",
                        other
                    ),
                ),
                soap_action,
            );
        }
    };

    soap_response(StatusCode::OK, response_xml, soap_action)
}

async fn wsdl_handler() -> Response {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/xml; charset=utf-8"),
        )],
        WSDL_DOC,
    )
        .into_response()
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/soap", post(soap_handler))
        .route("/soap/wsdl", get(wsdl_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new("/soap", &["POST"], category::SOAP, "SOAP 1.1 service (GetUser, ListUsers, CreateUser)")
            .example(Example::post("SOAP GetUser", "/soap")
                .header("SOAPAction", "GetUser")
                .xml(r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body><GetUser xmlns="http://rustybin.local/users"><userId>123</userId></GetUser></soap:Body></soap:Envelope>"#)),
        Endpoint::new("/soap/wsdl", &["GET"], category::SOAP, "WSDL document")
            .example(Example::get("WSDL", "/soap/wsdl")),
    ]
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
    }

    async fn body_string(resp: axum::http::Response<Body>) -> String {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8(body.to_vec()).expect("utf8")
    }

    fn soap_req(operation_xml: &str) -> String {
        format!(
            r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body>{}</soap:Body></soap:Envelope>"#,
            operation_xml
        )
    }

    #[tokio::test]
    async fn get_user_returns_soap_response() {
        let app = test_app();
        let xml = soap_req(
            r#"<GetUser xmlns="http://rustybin.local/users"><userId>123</userId></GetUser>"#,
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/soap")
                    .header("content-type", "text/xml")
                    .body(Body::from(xml))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp
            .headers()
            .get("content-type")
            .expect("ct")
            .to_str()
            .expect("str")
            .contains("text/xml"));

        let body = body_string(resp).await;
        assert!(body.contains("soap:Envelope"));
        assert!(body.contains("GetUserResponse"));
        assert!(body.contains("<id>123</id>"));
        assert!(body.contains("<name>Jane Smith</name>"));
        assert!(body.contains("<role>ADMIN</role>"));
    }

    #[tokio::test]
    async fn get_user_unknown_id() {
        let app = test_app();
        let xml = soap_req(
            r#"<GetUser xmlns="http://rustybin.local/users"><userId>999</userId></GetUser>"#,
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/soap")
                    .header("content-type", "text/xml")
                    .body(Body::from(xml))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("<name>Demo User</name>"));
    }

    #[tokio::test]
    async fn create_order_returns_order_id() {
        let app = test_app();
        let xml = soap_req(
            r#"<CreateOrder xmlns="http://rustybin.local/orders"><merchantId>M001</merchantId><amount>5000</amount><currency>GBP</currency></CreateOrder>"#,
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/soap")
                    .header("content-type", "text/xml")
                    .body(Body::from(xml))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("CreateOrderResponse"));
        assert!(body.contains("<merchantId>M001</merchantId>"));
        assert!(body.contains("<amount>5000</amount>"));
        assert!(body.contains("<status>ACCEPTED</status>"));
        assert!(body.contains("ORD-"));
    }

    #[tokio::test]
    async fn get_status_returns_tracking() {
        let app = test_app();
        let xml = soap_req(
            r#"<GetStatus xmlns="http://rustybin.local/status"><orderId>ORD-001</orderId></GetStatus>"#,
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/soap")
                    .header("content-type", "text/xml")
                    .body(Body::from(xml))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("GetStatusResponse"));
        assert!(body.contains("<orderId>ORD-001</orderId>"));
        assert!(body.contains("<status>PROCESSING</status>"));
        assert!(body.contains("TRK-ORD-001-001"));
    }

    #[tokio::test]
    async fn unknown_operation_returns_fault() {
        let app = test_app();
        let xml = soap_req(r#"<FooBar xmlns="http://rustybin.local/test"><id>1</id></FooBar>"#);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/soap")
                    .header("content-type", "text/xml")
                    .body(Body::from(xml))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_string(resp).await;
        assert!(body.contains("soap:Fault"));
        assert!(body.contains("Unknown operation"));
        assert!(body.contains("FooBar"));
    }

    #[tokio::test]
    async fn malformed_xml_returns_fault() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/soap")
                    .header("content-type", "text/xml")
                    .body(Body::from("this is not xml at all"))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_string(resp).await;
        assert!(body.contains("soap:Fault"));
        assert!(body.contains("soap:Client"));
    }

    #[tokio::test]
    async fn empty_body_returns_fault() {
        let app = test_app();
        let xml = soap_req("");
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/soap")
                    .header("content-type", "text/xml")
                    .body(Body::from(xml))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_string(resp).await;
        assert!(body.contains("soap:Fault"));
    }

    #[tokio::test]
    async fn soap_action_echoed() {
        let app = test_app();
        let xml = soap_req(
            r#"<GetUser xmlns="http://rustybin.local/users"><userId>123</userId></GetUser>"#,
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/soap")
                    .header("content-type", "text/xml")
                    .header("SOAPAction", "\"http://rustybin.local/GetUser\"")
                    .body(Body::from(xml))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let action = resp
            .headers()
            .get("SOAPAction")
            .expect("action")
            .to_str()
            .expect("str");
        assert!(action.contains("GetUser"));
    }

    #[tokio::test]
    async fn wsdl_returns_xml() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/soap/wsdl")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp
            .headers()
            .get("content-type")
            .expect("ct")
            .to_str()
            .expect("str")
            .contains("text/xml"));

        let body = body_string(resp).await;
        assert!(body.contains("definitions"));
        assert!(body.contains("GetUser"));
        assert!(body.contains("CreateOrder"));
        assert!(body.contains("GetStatus"));
        assert!(body.contains("RustybinService"));
    }
}
