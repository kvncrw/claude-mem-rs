//! Local deterministic text embedding used by optional vector stores.

pub const DEFAULT_EMBEDDING_DIM: usize = 64;

pub fn embed_text(text: &str) -> Vec<f32> {
    embed_text_with_dim(text, DEFAULT_EMBEDDING_DIM)
}

pub fn embed_text_with_dim(text: &str, dim: usize) -> Vec<f32> {
    let dim = dim.max(1);
    let mut vector = vec![0.0_f32; dim];
    for token in tokens(text) {
        let hash = fnv1a(token.as_bytes());
        let index = (hash as usize) % dim;
        let sign = if hash & 1 == 0 { 1.0 } else { -1.0 };
        vector[index] += sign;
    }

    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 2)
        .map(str::to_ascii_lowercase)
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_is_deterministic_and_normalized() {
        let a = embed_text("Dynatron cooler power cap");
        let b = embed_text("Dynatron cooler power cap");
        assert_eq!(a, b);
        assert_eq!(a.len(), DEFAULT_EMBEDDING_DIM);
        let norm = a.iter().map(|value| value * value).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.0001);
    }

    #[test]
    fn empty_text_returns_zero_vector() {
        assert_eq!(embed_text_with_dim("", 4), vec![0.0, 0.0, 0.0, 0.0]);
    }
}
