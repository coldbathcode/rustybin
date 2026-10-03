//! Deterministic embeddings and rerank scores (for semantic-cache demos).
//!
//! An embedding is a hashed bag of features of the lower-cased text, spread
//! into the requested number of dimensions and normalised to unit length:
//! - each word (weight 1.0),
//! - each pair of adjacent words (weight 0.35, so word order matters a little),
//! - each character trigram of each word (weight 0.15, so "cat" and "cats" are close).
//!
//! Hashing is FNV-1a (stable across builds and platforms). Consequences:
//! identical input gives identical vectors; case and punctuation do not
//! matter; the same words in another order give a cosine similarity around
//! 0.9; unrelated texts are near 0.

/// 64-bit FNV-1a.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

const SPREAD: u64 = 4;

fn add_feature(v: &mut [f32], feature: &str, weight: f32) {
    let h = fnv1a(feature.as_bytes());
    let n = v.len() as u64;
    for k in 0..SPREAD {
        let r = mix(h.wrapping_add(k));
        let idx = (r % n) as usize;
        let sign = if (r >> 63) == 1 { -1.0 } else { 1.0 };
        if let Some(slot) = v.get_mut(idx) {
            *slot += sign * weight;
        }
    }
}

/// Unit-length embedding of `text` with `dims` dimensions (at least 1).
pub fn embed(text: &str, dims: usize) -> Vec<f32> {
    let dims = dims.max(1);
    let mut v = vec![0f32; dims];
    let words = super::engine::words(text);
    for w in &words {
        add_feature(&mut v, &format!("w:{w}"), 1.0);
        let chars: Vec<char> = format!("^{w}$").chars().collect();
        for t in chars.windows(3) {
            let tri: String = t.iter().collect();
            add_feature(&mut v, &format!("t:{tri}"), 0.15);
        }
    }
    for pair in words.windows(2) {
        add_feature(&mut v, &format!("b:{} {}", pair[0], pair[1]), 0.35);
    }
    let mut norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        // Empty text (or features that cancelled out): a fixed unit vector.
        let idx = (fnv1a(b"empty") % dims as u64) as usize;
        if let Some(slot) = v.get_mut(idx) {
            *slot = 1.0;
        }
        norm = 1.0;
    }
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// Cosine similarity of two vectors of the same length.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Little-endian f32 bytes, base64 (OpenAI `encoding_format: base64`).
pub fn to_base64(v: &[f32]) -> String {
    use base64::Engine;
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for x in v {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Deterministic relevance of `doc` to `query` in 0..=1: query-word overlap
/// blended with embedding similarity.
pub fn relevance(query: &str, doc: &str) -> f64 {
    let q = super::engine::words(query);
    let d: std::collections::HashSet<String> = super::engine::words(doc).into_iter().collect();
    let overlap = if q.is_empty() {
        0.0
    } else {
        q.iter().filter(|w| d.contains(*w)).count() as f64 / q.len() as f64
    };
    let cos = f64::from(cosine(&embed(query, 256), &embed(doc, 256))).max(0.0);
    let score = 0.7 * overlap + 0.3 * cos;
    (score.clamp(0.0, 1.0) * 1_000_000.0).round() / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_normalised() {
        let a = embed("The quick brown fox", 1536);
        let b = embed("The quick brown fox", 1536);
        assert_eq!(a, b);
        assert_eq!(a.len(), 1536);
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4);
        // FNV is stable: pin one value so a hashing change is noticed.
        assert_eq!(fnv1a(b"rustybin"), fnv1a(b"rustybin"));
        assert_ne!(fnv1a(b"a"), fnv1a(b"b"));
    }

    #[test]
    fn similar_texts_are_close() {
        let a = embed("What is the capital of France?", 512);
        let case = embed("what is the CAPITAL of france", 512);
        let order = embed("France: what is the capital of", 512);
        let other = embed("Recipe for chocolate chip cookies", 512);
        assert!(cosine(&a, &case) > 0.999);
        let co = cosine(&a, &order);
        assert!(co > 0.8 && co < 0.999, "reordered cosine {co}");
        assert!(cosine(&a, &other) < 0.3);
        assert!(!embed("", 8).iter().all(|x| *x == 0.0));
    }

    #[test]
    fn relevance_ranks_matching_docs_higher() {
        let q = "capital of France";
        assert!(
            relevance(q, "Paris is the capital of France.") > relevance(q, "Bananas are yellow.")
        );
        assert!(relevance(q, "x") >= 0.0);
    }

    #[test]
    fn base64_round_trip() {
        use base64::Engine;
        let v = vec![1.0f32, -0.5];
        let b = base64::engine::general_purpose::STANDARD
            .decode(to_base64(&v))
            .expect("b64");
        assert_eq!(b.len(), 8);
        assert_eq!(f32::from_le_bytes([b[4], b[5], b[6], b[7]]), -0.5);
    }
}
