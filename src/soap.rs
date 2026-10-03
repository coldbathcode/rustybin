//! SOAP 1.1 and 1.2 service (GetUser, CreateOrder, GetStatus) plus WSDL.
//!
//! - Version: from the Envelope namespace (authoritative), else the
//!   Content-Type (`text/xml` = 1.1, `application/soap+xml` = 1.2). Responses
//!   use the same version and Content-Type. An unknown envelope namespace is
//!   a VersionMismatch fault.
//! - Dispatch: `SOAPAction` (1.1) or the Content-Type `action` parameter
//!   (1.2); an empty or unknown action falls back to the first Body element.
//! - Faults: SOAP 1.1 faults are HTTP 500 (faultcode/faultstring/detail);
//!   SOAP 1.2 faults use Code/Reason/Detail with HTTP 400 for env:Sender and
//!   500 otherwise (SOAP 1.2 HTTP binding).
//! - Every user value is XML-escaped; self-closing elements are handled.
//! - `/soap/wsdl` (and `/soap?wsdl`) describes both bindings; the
//!   soap:address is derived from the request (honours trusted
//!   `X-Forwarded-*`).

use axum::{
    body::Bytes,
    extract::{RawQuery, State},
    http::{header, request::Parts, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::NsReader;
use std::sync::Arc;

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::content_negotiation::xml_escape;
use crate::state::AppState;

pub const SOAP11_ENV_NS: &str = "http://schemas.xmlsoap.org/soap/envelope/";
pub const SOAP12_ENV_NS: &str = "http://www.w3.org/2003/05/soap-envelope";
const SOAP11_CONTENT_TYPE: &str = "text/xml; charset=utf-8";
const SOAP12_CONTENT_TYPE: &str = "application/soap+xml; charset=utf-8";
const ACTION_BASE: &str = "http://rustybin.local/";
const OPERATIONS: [&str; 3] = ["GetUser", "CreateOrder", "GetStatus"];
/// Maximum number of fields read from the operation element.
const MAX_FIELDS: usize = 64;
/// Maximum element nesting accepted.
const MAX_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoapVersion {
    V11,
    V12,
}

impl SoapVersion {
    fn ns(self) -> &'static str {
        match self {
            SoapVersion::V11 => SOAP11_ENV_NS,
            SoapVersion::V12 => SOAP12_ENV_NS,
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            SoapVersion::V11 => "soap",
            SoapVersion::V12 => "env",
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            SoapVersion::V11 => SOAP11_CONTENT_TYPE,
            SoapVersion::V12 => SOAP12_CONTENT_TYPE,
        }
    }
}

/// Fault classes (1.1 name / 1.2 name).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultCode {
    /// 1.1 Client, 1.2 Sender
    Sender,
    /// 1.1 Server, 1.2 Receiver
    Receiver,
    VersionMismatch,
    MustUnderstand,
}

struct Fault {
    code: FaultCode,
    reason: String,
    detail: String,
}

impl Fault {
    fn sender(reason: &str, detail: impl Into<String>) -> Self {
        Fault {
            code: FaultCode::Sender,
            reason: reason.to_string(),
            detail: detail.into(),
        }
    }
}

// ── Envelope helpers ────────────────────────────────────────────────

fn envelope(version: SoapVersion, body_inner: &str) -> String {
    let p = version.prefix();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <{p}:Envelope xmlns:{p}=\"{ns}\">\n\
         <{p}:Body>\n{body_inner}\n</{p}:Body>\n\
         </{p}:Envelope>",
        ns = version.ns()
    )
}

fn fault_xml(version: SoapVersion, fault: &Fault) -> String {
    let p = version.prefix();
    let reason = xml_escape(&fault.reason);
    let detail = format!(
        "<rb:error xmlns:rb=\"http://rustybin.local/faults\">{}</rb:error>",
        xml_escape(&fault.detail)
    );
    let inner = match version {
        SoapVersion::V11 => {
            let code = match fault.code {
                FaultCode::Sender => "Client",
                FaultCode::Receiver => "Server",
                FaultCode::VersionMismatch => "VersionMismatch",
                FaultCode::MustUnderstand => "MustUnderstand",
            };
            format!(
                "<{p}:Fault>\n<faultcode>{p}:{code}</faultcode>\n\
                 <faultstring>{reason}</faultstring>\n<detail>{detail}</detail>\n</{p}:Fault>"
            )
        }
        SoapVersion::V12 => {
            let code = match fault.code {
                FaultCode::Sender => "Sender",
                FaultCode::Receiver => "Receiver",
                FaultCode::VersionMismatch => "VersionMismatch",
                FaultCode::MustUnderstand => "MustUnderstand",
            };
            format!(
                "<{p}:Fault>\n<{p}:Code><{p}:Value>{p}:{code}</{p}:Value></{p}:Code>\n\
                 <{p}:Reason><{p}:Text xml:lang=\"en\">{reason}</{p}:Text></{p}:Reason>\n\
                 <{p}:Detail>{detail}</{p}:Detail>\n</{p}:Fault>"
            )
        }
    };
    envelope(version, &inner)
}

fn xml_response(status: StatusCode, content_type: &'static str, body: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, HeaderValue::from_static(content_type))],
        body,
    )
        .into_response()
}

fn fault_response(version: SoapVersion, fault: Fault) -> Response {
    let status = match (version, fault.code) {
        // SOAP 1.2 HTTP binding: sender faults are client errors.
        (SoapVersion::V12, FaultCode::Sender) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    xml_response(status, version.content_type(), fault_xml(version, &fault))
}

// ── Request parsing ─────────────────────────────────────────────────

#[derive(Debug)]
struct ParsedOperation {
    version: SoapVersion,
    name: String,
    fields: Vec<(String, String)>,
}

/// Version implied by the Content-Type, if any.
fn version_from_content_type(headers: &HeaderMap) -> Option<SoapVersion> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())?
        .to_ascii_lowercase();
    if ct.starts_with("application/soap+xml") {
        Some(SoapVersion::V12)
    } else if ct.starts_with("text/xml") {
        Some(SoapVersion::V11)
    } else {
        None
    }
}

/// The requested action: `SOAPAction` (1.1) or the `action` media type
/// parameter (1.2). Returned as the operation name (last URI segment).
fn requested_action(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get("soapaction")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| {
            let ct = headers.get(header::CONTENT_TYPE)?.to_str().ok()?;
            ct.split(';').skip(1).find_map(|p| {
                let (k, v) = p.split_once('=')?;
                k.trim()
                    .eq_ignore_ascii_case("action")
                    .then(|| v.trim().to_string())
            })
        })?;
    let action = raw.trim().trim_matches('"').trim();
    let name = action.rsplit(['/', '#', ':']).next().unwrap_or(action);
    (!name.is_empty()).then(|| name.to_string())
}

fn local(name: &[u8]) -> String {
    String::from_utf8_lossy(name).into_owned()
}

fn parse_envelope(
    xml: &[u8],
    ct_version: Option<SoapVersion>,
) -> Result<ParsedOperation, (SoapVersion, Fault)> {
    let fallback = ct_version.unwrap_or(SoapVersion::V11);
    let mut reader = NsReader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut version: Option<SoapVersion> = None;
    let mut depth = 0usize;
    let mut in_header = false;
    let mut in_body = false;
    let mut op: Option<String> = None;
    let mut op_done = false;
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;

    loop {
        let event = reader.read_resolved_event_into(&mut buf);
        let v = version.unwrap_or(fallback);
        let (ns, event) = match event {
            Ok(x) => x,
            Err(e) => {
                return Err((
                    v,
                    Fault::sender("Malformed XML", format!("XML parse error: {e}")),
                ));
            }
        };
        let ns_str = match ns {
            ResolveResult::Bound(n) => Some(String::from_utf8_lossy(n.as_ref()).into_owned()),
            _ => None,
        };
        let (is_start, is_empty) = match &event {
            Event::Start(_) => (true, false),
            Event::Empty(_) => (true, true),
            _ => (false, false),
        };
        match &event {
            Event::Start(e) | Event::Empty(e) if is_start => {
                let name = local(e.local_name().as_ref());
                let level = depth + 1;
                if level > MAX_DEPTH {
                    return Err((
                        v,
                        Fault::sender("Malformed XML", "document nested too deeply"),
                    ));
                }
                match level {
                    1 => {
                        if name != "Envelope" {
                            return Err((
                                v,
                                Fault::sender(
                                    "Not a SOAP message",
                                    "the root element must be Envelope",
                                ),
                            ));
                        }
                        version = match ns_str.as_deref() {
                            Some(SOAP11_ENV_NS) => Some(SoapVersion::V11),
                            Some(SOAP12_ENV_NS) => Some(SoapVersion::V12),
                            other => {
                                return Err((
                                    fallback,
                                    Fault {
                                        code: FaultCode::VersionMismatch,
                                        reason: "Unsupported SOAP envelope namespace".to_string(),
                                        detail: format!(
                                            "expected {SOAP11_ENV_NS} (SOAP 1.1) or {SOAP12_ENV_NS} (SOAP 1.2), got {}",
                                            other.unwrap_or("no namespace")
                                        ),
                                    },
                                ));
                            }
                        };
                    }
                    2 => {
                        in_header = name == "Header";
                        in_body = name == "Body";
                    }
                    3 if in_header => {
                        let must = e.attributes().flatten().any(|a| {
                            a.key.local_name().as_ref() == b"mustUnderstand"
                                && matches!(a.value.as_ref(), b"1" | b"true")
                        });
                        if must {
                            return Err((
                                v,
                                Fault {
                                    code: FaultCode::MustUnderstand,
                                    reason: "Header not understood".to_string(),
                                    detail: format!("header block {name} is marked mustUnderstand"),
                                },
                            ));
                        }
                    }
                    3 if in_body && op.is_none() && !op_done => {
                        op = Some(name);
                        if is_empty {
                            op_done = true;
                        }
                    }
                    4 if in_body && op.is_some() && !op_done => {
                        if fields.len() >= MAX_FIELDS {
                            return Err((
                                v,
                                Fault::sender(
                                    "Too many fields",
                                    format!("at most {MAX_FIELDS} fields"),
                                ),
                            ));
                        }
                        if is_empty {
                            fields.push((name, String::new()));
                        } else {
                            current = Some((name, String::new()));
                        }
                    }
                    _ => {}
                }
                if !is_empty {
                    depth = level;
                }
            }
            Event::End(_) => {
                match depth {
                    4 => {
                        if let Some(f) = current.take() {
                            fields.push(f);
                        }
                    }
                    3 if in_body && op.is_some() => op_done = true,
                    2 => {
                        in_header = false;
                        in_body = false;
                    }
                    _ => {}
                }
                depth = depth.saturating_sub(1);
            }
            Event::Text(t) if depth == 4 => {
                if let Some((_, value)) = current.as_mut() {
                    let text = t.unescape().map_err(|e| {
                        (v, Fault::sender("Malformed XML", format!("bad text: {e}")))
                    })?;
                    value.push_str(&text);
                }
            }
            Event::CData(t) if depth == 4 => {
                if let Some((_, value)) = current.as_mut() {
                    value.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
            }
            Event::DocType(_) => {
                return Err((
                    v,
                    Fault::sender("Malformed XML", "DTDs are not allowed in SOAP messages"),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    let Some(version) = version else {
        return Err((
            fallback,
            Fault::sender("Not a SOAP message", "no Envelope element found"),
        ));
    };
    match op {
        Some(name) => Ok(ParsedOperation {
            version,
            name,
            fields,
        }),
        None => Err((
            version,
            Fault::sender("Empty Body", "no operation element in the SOAP Body"),
        )),
    }
}

fn get_field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty())
}

// ── Operations ──────────────────────────────────────────────────────

fn handle_get_user(op: &ParsedOperation) -> Result<String, Fault> {
    let user_id = get_field(&op.fields, "userId")
        .ok_or_else(|| Fault::sender("Missing field", "GetUser requires userId"))?;
    let (name, email, role) = match user_id {
        "123" => ("Jane Smith", "jane@rustybin.local", "ADMIN"),
        "456" => ("Bob Wilson", "bob@rustybin.local", "USER"),
        _ => ("Demo User", "demo@rustybin.local", "GUEST"),
    };
    Ok(format!(
        "<GetUserResponse xmlns=\"http://rustybin.local/users\">\n\
         <user>\n<id>{}</id>\n<name>{name}</name>\n<email>{email}</email>\n<role>{role}</role>\n</user>\n\
         </GetUserResponse>",
        xml_escape(user_id)
    ))
}

fn handle_create_order(op: &ParsedOperation) -> Result<String, Fault> {
    let merchant_id = get_field(&op.fields, "merchantId")
        .ok_or_else(|| Fault::sender("Missing field", "CreateOrder requires merchantId"))?;
    let amount = get_field(&op.fields, "amount").unwrap_or("0");
    if !amount
        .parse::<f64>()
        .is_ok_and(|a| a.is_finite() && a >= 0.0)
    {
        return Err(Fault::sender(
            "Invalid field",
            format!("amount must be a non-negative decimal, got {amount:?}"),
        ));
    }
    let currency = get_field(&op.fields, "currency").unwrap_or("GBP");
    let order_id = format!("ORD-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    Ok(format!(
        "<CreateOrderResponse xmlns=\"http://rustybin.local/orders\">\n\
         <orderId>{order_id}</orderId>\n<merchantId>{}</merchantId>\n<amount>{}</amount>\n\
         <currency>{}</currency>\n<status>ACCEPTED</status>\n<timestamp>{}</timestamp>\n\
         </CreateOrderResponse>",
        xml_escape(merchant_id),
        xml_escape(amount),
        xml_escape(currency),
        chrono::Utc::now().to_rfc3339()
    ))
}

fn handle_get_status(op: &ParsedOperation) -> Result<String, Fault> {
    let order_id = get_field(&op.fields, "orderId").unwrap_or("ORD-000");
    let now = chrono::Utc::now();
    let order_id = xml_escape(order_id);
    Ok(format!(
        "<GetStatusResponse xmlns=\"http://rustybin.local/status\">\n\
         <orderId>{order_id}</orderId>\n<status>PROCESSING</status>\n\
         <createdAt>{}</createdAt>\n<updatedAt>{}</updatedAt>\n\
         <tracking>TRK-{order_id}-001</tracking>\n</GetStatusResponse>",
        (now - chrono::Duration::hours(2)).to_rfc3339(),
        now.to_rfc3339()
    ))
}

// ── Handlers ────────────────────────────────────────────────────────

async fn soap_handler(headers: HeaderMap, body: Bytes) -> Response {
    let ct_version = version_from_content_type(&headers);
    let parsed = match parse_envelope(&body, ct_version) {
        Ok(op) => op,
        Err((version, fault)) => return fault_response(version, fault),
    };
    let version = parsed.version;

    // The action wins when it names a known operation; otherwise dispatch on
    // the Body element.
    let operation = requested_action(&headers)
        .filter(|a| OPERATIONS.contains(&a.as_str()))
        .unwrap_or_else(|| parsed.name.clone());

    let result = match operation.as_str() {
        "GetUser" => handle_get_user(&parsed),
        "CreateOrder" => handle_create_order(&parsed),
        "GetStatus" => handle_get_status(&parsed),
        other => Err(Fault::sender(
            "Unknown operation",
            format!(
                "The requested operation '{other}' is not supported. Available operations: {}",
                OPERATIONS.join(", ")
            ),
        )),
    };
    match result {
        Ok(inner) => xml_response(
            StatusCode::OK,
            version.content_type(),
            envelope(version, &inner),
        ),
        Err(fault) => fault_response(version, fault),
    }
}

async fn wsdl_handler(State(config): State<Arc<Config>>, parts: Parts) -> Response {
    let origin =
        crate::session::request_origin(&parts.headers, &parts.extensions, &parts.uri, &config);
    let address = format!("{}/soap", origin.base_url());
    xml_response(StatusCode::OK, SOAP11_CONTENT_TYPE, wsdl(&address))
}

/// `GET /soap?wsdl` is the conventional WSDL location.
async fn soap_get_handler(
    State(config): State<Arc<Config>>,
    RawQuery(query): RawQuery,
    parts: Parts,
) -> Response {
    if query.is_some_and(|q| q.eq_ignore_ascii_case("wsdl")) {
        return wsdl_handler(State(config), parts).await;
    }
    let fault = Fault::sender(
        "Use POST",
        "POST a SOAP envelope to /soap; GET /soap?wsdl or /soap/wsdl returns the WSDL",
    );
    let mut resp = xml_response(
        StatusCode::METHOD_NOT_ALLOWED,
        SOAP11_CONTENT_TYPE,
        fault_xml(SoapVersion::V11, &fault),
    );
    resp.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("GET, POST"));
    resp
}

fn wsdl(address: &str) -> String {
    let address = xml_escape(address);
    let ops = |binding_ns: &str| {
        OPERATIONS
            .iter()
            .map(|op| {
                format!(
                    "    <operation name=\"{op}\">\n      <{binding_ns}:operation soapAction=\"{ACTION_BASE}{op}\"/>\n      \
                     <input><{binding_ns}:body use=\"literal\"/></input>\n      \
                     <output><{binding_ns}:body use=\"literal\"/></output>\n      \
                     <fault name=\"ServiceFault\"><{binding_ns}:fault name=\"ServiceFault\" use=\"literal\"/></fault>\n    </operation>\n"
                )
            })
            .collect::<String>()
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://schemas.xmlsoap.org/wsdl/"
             xmlns:soap="http://schemas.xmlsoap.org/wsdl/soap/"
             xmlns:soap12="http://schemas.xmlsoap.org/wsdl/soap12/"
             xmlns:tns="http://rustybin.local/wsdl"
             xmlns:xsd="http://www.w3.org/2001/XMLSchema"
             xmlns:usr="http://rustybin.local/users"
             xmlns:ord="http://rustybin.local/orders"
             xmlns:sts="http://rustybin.local/status"
             xmlns:flt="http://rustybin.local/faults"
             name="RustybinService"
             targetNamespace="http://rustybin.local/wsdl">

  <types>
    <xsd:schema targetNamespace="http://rustybin.local/users" elementFormDefault="qualified">
      <xsd:element name="GetUser">
        <xsd:complexType><xsd:sequence>
          <xsd:element name="userId" type="xsd:string"/>
        </xsd:sequence></xsd:complexType>
      </xsd:element>
      <xsd:element name="GetUserResponse">
        <xsd:complexType><xsd:sequence>
          <xsd:element name="user">
            <xsd:complexType><xsd:sequence>
              <xsd:element name="id" type="xsd:string"/>
              <xsd:element name="name" type="xsd:string"/>
              <xsd:element name="email" type="xsd:string"/>
              <xsd:element name="role" type="xsd:string"/>
            </xsd:sequence></xsd:complexType>
          </xsd:element>
        </xsd:sequence></xsd:complexType>
      </xsd:element>
    </xsd:schema>
    <xsd:schema targetNamespace="http://rustybin.local/orders" elementFormDefault="qualified">
      <xsd:element name="CreateOrder">
        <xsd:complexType><xsd:sequence>
          <xsd:element name="merchantId" type="xsd:string"/>
          <xsd:element name="amount" type="xsd:decimal" minOccurs="0"/>
          <xsd:element name="currency" type="xsd:string" minOccurs="0"/>
        </xsd:sequence></xsd:complexType>
      </xsd:element>
      <xsd:element name="CreateOrderResponse">
        <xsd:complexType><xsd:sequence>
          <xsd:element name="orderId" type="xsd:string"/>
          <xsd:element name="merchantId" type="xsd:string"/>
          <xsd:element name="amount" type="xsd:decimal"/>
          <xsd:element name="currency" type="xsd:string"/>
          <xsd:element name="status" type="xsd:string"/>
          <xsd:element name="timestamp" type="xsd:dateTime"/>
        </xsd:sequence></xsd:complexType>
      </xsd:element>
    </xsd:schema>
    <xsd:schema targetNamespace="http://rustybin.local/status" elementFormDefault="qualified">
      <xsd:element name="GetStatus">
        <xsd:complexType><xsd:sequence>
          <xsd:element name="orderId" type="xsd:string" minOccurs="0"/>
        </xsd:sequence></xsd:complexType>
      </xsd:element>
      <xsd:element name="GetStatusResponse">
        <xsd:complexType><xsd:sequence>
          <xsd:element name="orderId" type="xsd:string"/>
          <xsd:element name="status" type="xsd:string"/>
          <xsd:element name="createdAt" type="xsd:dateTime"/>
          <xsd:element name="updatedAt" type="xsd:dateTime"/>
          <xsd:element name="tracking" type="xsd:string"/>
        </xsd:sequence></xsd:complexType>
      </xsd:element>
    </xsd:schema>
    <xsd:schema targetNamespace="http://rustybin.local/faults" elementFormDefault="qualified">
      <xsd:element name="error" type="xsd:string"/>
    </xsd:schema>
  </types>

  <message name="GetUserRequest"><part name="parameters" element="usr:GetUser"/></message>
  <message name="GetUserResponse"><part name="parameters" element="usr:GetUserResponse"/></message>
  <message name="CreateOrderRequest"><part name="parameters" element="ord:CreateOrder"/></message>
  <message name="CreateOrderResponse"><part name="parameters" element="ord:CreateOrderResponse"/></message>
  <message name="GetStatusRequest"><part name="parameters" element="sts:GetStatus"/></message>
  <message name="GetStatusResponse"><part name="parameters" element="sts:GetStatusResponse"/></message>
  <message name="ServiceFault"><part name="fault" element="flt:error"/></message>

  <portType name="RustybinPortType">
    <operation name="GetUser">
      <input message="tns:GetUserRequest"/>
      <output message="tns:GetUserResponse"/>
      <fault name="ServiceFault" message="tns:ServiceFault"/>
    </operation>
    <operation name="CreateOrder">
      <input message="tns:CreateOrderRequest"/>
      <output message="tns:CreateOrderResponse"/>
      <fault name="ServiceFault" message="tns:ServiceFault"/>
    </operation>
    <operation name="GetStatus">
      <input message="tns:GetStatusRequest"/>
      <output message="tns:GetStatusResponse"/>
      <fault name="ServiceFault" message="tns:ServiceFault"/>
    </operation>
  </portType>

  <binding name="RustybinBinding" type="tns:RustybinPortType">
    <soap:binding style="document" transport="http://schemas.xmlsoap.org/soap/http"/>
{ops11}  </binding>

  <binding name="RustybinBinding12" type="tns:RustybinPortType">
    <soap12:binding style="document" transport="http://schemas.xmlsoap.org/soap/http"/>
{ops12}  </binding>

  <service name="RustybinService">
    <port name="RustybinPort" binding="tns:RustybinBinding">
      <soap:address location="{address}"/>
    </port>
    <port name="RustybinPort12" binding="tns:RustybinBinding12">
      <soap12:address location="{address}"/>
    </port>
  </service>
</definitions>
"#,
        ops11 = ops("soap"),
        ops12 = ops("soap12"),
    )
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/soap", get(soap_get_handler).post(soap_handler))
        .route("/soap/wsdl", get(wsdl_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    const ENV11: &str = r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body><GetUser xmlns="http://rustybin.local/users"><userId>123</userId></GetUser></soap:Body></soap:Envelope>"#;
    const ENV12: &str = r#"<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope"><env:Body><CreateOrder xmlns="http://rustybin.local/orders"><merchantId>M001</merchantId><amount>42.50</amount><currency>EUR</currency></CreateOrder></env:Body></env:Envelope>"#;
    vec![
        Endpoint::new(
            "/soap",
            &["GET", "POST"],
            category::SOAP,
            "SOAP 1.1 / 1.2 service (GetUser, CreateOrder, GetStatus); GET /soap?wsdl returns the WSDL",
        )
        .description(
            "Version from the envelope namespace (or Content-Type text/xml vs application/soap+xml), \
             dispatch by SOAPAction / action parameter, falling back to the Body element. \
             Faults: 1.1 HTTP 500; 1.2 Code/Reason, HTTP 400 (Sender) or 500.",
        )
        .example(
            Example::post("SOAP 1.1 GetUser", "/soap")
                .header("SOAPAction", "\"http://rustybin.local/GetUser\"")
                .xml(ENV11),
        )
        .example(
            Example::post("SOAP 1.2 CreateOrder", "/soap")
                .header(
                    "Content-Type",
                    "application/soap+xml; charset=utf-8; action=\"http://rustybin.local/CreateOrder\"",
                )
                .xml(ENV12),
        )
        .example(Example::get("WSDL (query form)", "/soap?wsdl")),
        Endpoint::new(
            "/soap/wsdl",
            &["GET"],
            category::SOAP,
            "WSDL 1.1 document (SOAP 1.1 and 1.2 bindings)",
        )
        .example(Example::get("WSDL", "/soap/wsdl")),
    ]
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_string, get_request};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn env11(op: &str) -> String {
        format!(
            r#"<soap:Envelope xmlns:soap="{SOAP11_ENV_NS}"><soap:Body>{op}</soap:Body></soap:Envelope>"#
        )
    }

    fn env12(op: &str) -> String {
        format!(
            r#"<env:Envelope xmlns:env="{SOAP12_ENV_NS}"><env:Body>{op}</env:Body></env:Envelope>"#
        )
    }

    async fn call(ct: &str, extra: &[(&str, &str)], body: String) -> (StatusCode, String, String) {
        let mut b = Request::builder()
            .method("POST")
            .uri("/soap")
            .header("content-type", ct);
        for (k, v) in extra {
            b = b.header(*k, *v);
        }
        let resp = crate::test_support::module_app(router)
            .oneshot(b.body(Body::from(body)).expect("request"))
            .await
            .expect("response");
        let status = resp.status();
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        (status, ct, body_string(resp).await)
    }

    fn assert_well_formed(xml: &str) {
        let mut reader = quick_xml::Reader::from_str(xml);
        loop {
            match reader.read_event() {
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(e) => panic!("invalid XML: {e}\n{xml}"),
            }
        }
    }

    #[tokio::test]
    async fn soap11_get_user() {
        let (status, ct, body) = call(
            "text/xml",
            &[],
            env11(r#"<GetUser xmlns="http://rustybin.local/users"><userId>123</userId></GetUser>"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct, SOAP11_CONTENT_TYPE);
        assert!(body.contains("soap:Envelope"));
        assert!(body.contains(SOAP11_ENV_NS));
        assert!(body.contains("<id>123</id>"));
        assert!(body.contains("<role>ADMIN</role>"));
        assert_well_formed(&body);
    }

    #[tokio::test]
    async fn soap12_responds_in_kind() {
        let (status, ct, body) = call(
            "application/soap+xml; charset=utf-8",
            &[],
            env12(r#"<u:GetUser xmlns:u="http://rustybin.local/users"><u:userId>456</u:userId></u:GetUser>"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct, SOAP12_CONTENT_TYPE);
        assert!(body.contains(SOAP12_ENV_NS));
        assert!(body.contains("<name>Bob Wilson</name>"));
        assert_well_formed(&body);
    }

    #[tokio::test]
    async fn user_values_are_escaped() {
        let (status, _, body) = call(
            "text/xml",
            &[],
            env11(
                r#"<CreateOrder><merchantId>A&amp;B &lt;script&gt;</merchantId><amount>12.5</amount><currency><![CDATA[<EUR>]]></currency></CreateOrder>"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<merchantId>A&amp;B &lt;script&gt;</merchantId>"));
        assert!(body.contains("<currency>&lt;EUR&gt;</currency>"));
        assert!(body.contains("<amount>12.5</amount>"));
        assert_well_formed(&body);
    }

    #[tokio::test]
    async fn self_closing_elements() {
        // Self-closing operation: GetStatus with defaults.
        let (status, _, body) = call("text/xml", &[], env11("<GetStatus/>")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<orderId>ORD-000</orderId>"));
        // Self-closing required field: a fault, not a panic.
        let (status, _, body) = call("text/xml", &[], env11("<GetUser><userId/></GetUser>")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("userId"));
        // Self-closing optional field followed by a real one.
        let (status, _, body) = call(
            "text/xml",
            &[],
            env11("<CreateOrder><currency/><merchantId>M1</merchantId></CreateOrder>"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<merchantId>M1</merchantId>"));
        assert!(body.contains("<currency>GBP</currency>"));
    }

    #[tokio::test]
    async fn dispatch_by_action() {
        // SOAPAction wins over the body element when it names an operation.
        let (status, _, body) = call(
            "text/xml",
            &[("SOAPAction", "\"http://rustybin.local/GetStatus\"")],
            env11("<Whatever><orderId>ORD-9</orderId></Whatever>"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("GetStatusResponse"));
        // Empty SOAPAction falls back to the body.
        let (_, _, body) = call(
            "text/xml",
            &[("SOAPAction", "\"\"")],
            env11("<GetUser><userId>1</userId></GetUser>"),
        )
        .await;
        assert!(body.contains("GetUserResponse"));
        // SOAP 1.2 action parameter.
        let (status, _, body) = call(
            "application/soap+xml; action=\"http://rustybin.local/CreateOrder\"",
            &[],
            env12("<Op><merchantId>M2</merchantId></Op>"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("CreateOrderResponse"));
    }

    #[tokio::test]
    async fn soap11_faults_are_500() {
        let (status, ct, body) = call("text/xml", &[], env11("<FooBar/>")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(ct, SOAP11_CONTENT_TYPE);
        assert!(body.contains("<faultcode>soap:Client</faultcode>"));
        assert!(body.contains("FooBar"));
        assert_well_formed(&body);

        let (status, _, body) = call("text/xml", &[], "<not xml".to_string()).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("soap:Client"));
        let (status, _, _) = call("text/xml", &[], String::new()).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let (status, _, body) = call("text/xml", &[], env11("")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("Empty Body"));
    }

    #[tokio::test]
    async fn soap12_faults_use_code_and_reason() {
        let (status, ct, body) = call("application/soap+xml", &[], env12("<Nope/>")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(ct, SOAP12_CONTENT_TYPE);
        assert!(body.contains("<env:Code><env:Value>env:Sender</env:Value></env:Code>"));
        assert!(body.contains("<env:Reason><env:Text xml:lang=\"en\">Unknown operation</env:Text>"));
        assert!(!body.contains("faultcode"));
        assert_well_formed(&body);
    }

    #[tokio::test]
    async fn version_mismatch_and_must_understand() {
        let (status, _, body) = call(
            "text/xml",
            &[],
            r#"<e:Envelope xmlns:e="urn:other"><e:Body><GetUser/></e:Body></e:Envelope>"#
                .to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("soap:VersionMismatch"));

        let msg = format!(
            r#"<env:Envelope xmlns:env="{SOAP12_ENV_NS}"><env:Header><x:Auth xmlns:x="urn:x" env:mustUnderstand="true"/></env:Header><env:Body><GetStatus/></env:Body></env:Envelope>"#
        );
        let (status, _, body) = call("application/soap+xml", &[], msg).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("env:MustUnderstand"));
    }

    #[tokio::test]
    async fn invalid_amount_is_a_fault() {
        let (status, _, body) = call(
            "text/xml",
            &[],
            env11("<CreateOrder><merchantId>M</merchantId><amount>lots</amount></CreateOrder>"),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("amount must be"));
    }

    #[tokio::test]
    async fn wsdl_address_follows_the_request() {
        let resp = crate::test_support::module_app(router)
            .oneshot(
                Request::builder()
                    .uri("/soap/wsdl")
                    .header("host", "soap.example.com:8080")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("<soap:address location=\"http://soap.example.com:8080/soap\"/>"));
        assert!(body.contains("<soap12:address location=\"http://soap.example.com:8080/soap\"/>"));
        assert!(body.contains("RustybinBinding12"));
        assert!(body.contains("<xsd:element name=\"merchantId\" type=\"xsd:string\"/>"));
        assert_well_formed(&body);

        // Trusted forwarded headers change the address.
        let config = Config {
            trust_forward: true,
            ..crate::test_support::test_config()
        };
        let resp = crate::test_support::module_app_with_config(config, router)
            .oneshot(
                Request::builder()
                    .uri("/soap?wsdl")
                    .header("host", "internal:8080")
                    .header("x-forwarded-proto", "https")
                    .header("x-forwarded-host", "api.example.com")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let body = body_string(resp).await;
        assert!(body.contains("location=\"https://api.example.com/soap\""));
        let resp = crate::test_support::module_app(router)
            .oneshot(get_request("/soap"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn action_parsing() {
        let mut h = HeaderMap::new();
        h.insert(
            "soapaction",
            HeaderValue::from_static("\"urn:rustybin#GetUser\""),
        );
        assert_eq!(requested_action(&h).as_deref(), Some("GetUser"));
        let mut h = HeaderMap::new();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(
                "application/soap+xml;charset=utf-8;action=\"http://x/GetStatus\"",
            ),
        );
        assert_eq!(requested_action(&h).as_deref(), Some("GetStatus"));
        assert_eq!(version_from_content_type(&h), Some(SoapVersion::V12));
    }
}
