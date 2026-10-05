// Ed25519 key pairs used ONLY by tests and the documentation examples to sign
// control-plane JWTs (RUSTYBIN_CONTROL_AUTH=jwt). They protect nothing.
// Included by src/test_support.rs and tests/integration.rs (the docs runner
// and the console check generate fresh keys with openssl instead).

/// Private key (PKCS#8 PEM) of the test control-plane signer.
pub const CONTROL_JWT_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEINEGRAYsK/Hr/j6lWAOZUY7ARmMSVkyC1xNF2JzqYoe9
-----END PRIVATE KEY-----
";

/// Public key (SPKI PEM) matching [`CONTROL_JWT_PRIVATE_PEM`].
pub const CONTROL_JWT_PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAzD7+5ZDAX/ZxKGaO1Bm32ZSI1/eWEnNKkeiV8s2lyu0=
-----END PUBLIC KEY-----
";

/// The raw public key as base64 (the other accepted format).
pub const CONTROL_JWT_PUBLIC_RAW_B64: &str = "zD7+5ZDAX/ZxKGaO1Bm32ZSI1/eWEnNKkeiV8s2lyu0=";

/// A second, unrelated private key: its signatures must be refused.
pub const CONTROL_JWT_OTHER_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIJJcnsOwi/Hc7eQgmWbMKrYSfKLSihF8XJ1NoV91Qf8v
-----END PRIVATE KEY-----
";
